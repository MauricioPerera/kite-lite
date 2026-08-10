use anyhow::{Context, Result};
use kite_lite_core::{
    compute_layout, lint_a11y, lint_seo, lint_social, lint_webmcp, parse_html, render_pdf,
    render_png, render_svg, resolve_links, A11ySeverity, EvalRequest, EvalResponse, SeoSeverity,
    SocialSeverity, WebMcpLintSeverity,
};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tungstenite::{accept, Message};

const JS_TIMEOUT: Duration = Duration::from_millis(1500);
/// Applied to every reqwest client so a page/URL controlled by whoever we're
/// fetching can't hang a request (and, for the CDP/MCP clients, the shared
/// session) indefinitely.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Caps how much a single request to `serve`'s control API may total
/// (headers + body), so a client that never finishes sending (or claims an
/// enormous `Content-Length`) can't grow `read_http_request`'s buffer
/// without bound.
const MAX_HTTP_REQUEST_BYTES: usize = 2 * 1024 * 1024;
/// Viewport width used to compute each page's layout when no explicit
/// render width is requested yet (e.g. right after `fetch`/navigate, before
/// anyone calls `render`). `render_svg` recomputes layout at whatever width
/// it's actually asked to render, so this default only affects the layout
/// data exposed directly in a page's JSON.
const DEFAULT_VIEWPORT_WIDTH: f64 = 1024.0;
/// Fixed id for the CDP session (attached to *the* one target this project
/// ever tracks) and for that target itself. Real Chrome mints these
/// per-connection/per-target; kite-lite only ever has one page, so a
/// constant is enough for a client to treat every attach as talking to the
/// same target — no actual multi-target routing exists.
const CDP_SESSION_ID: &str = "kite-lite-session";
const CDP_TARGET_ID: &str = "kite-lite-target";

/// Presented on every outbound fetch. reqwest's own default UA
/// ("reqwest/x.y.z") is the cheapest possible signal for a WAF to filter
/// on, so every real request looks like a normal browser instead. This
/// only clears header-based filtering -- it does nothing against
/// TLS/JA3-level fingerprinting or JS-gated challenge pages.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn browser_like_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
            .parse()
            .unwrap(),
    );
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        "en-US,en;q=0.9".parse().unwrap(),
    );
    headers
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.get(1).map(String::as_str) == Some("fetch") {
        return fetch_command(&args).await;
    }

    if args.get(1).map(String::as_str) == Some("eval") {
        return eval_command(&args);
    }

    if args.get(1).map(String::as_str) == Some("render") {
        return render_command(&args);
    }

    if args.get(1).map(String::as_str) == Some("webmcp-lint") {
        return webmcp_lint_command(&args).await;
    }

    if args.get(1).map(String::as_str) == Some("a11y-lint") {
        return a11y_lint_command(&args).await;
    }

    if args.get(1).map(String::as_str) == Some("social-lint") {
        return social_lint_command(&args).await;
    }

    if args.get(1).map(String::as_str) == Some("seo-lint") {
        return seo_lint_command(&args).await;
    }

    if args.get(1).map(String::as_str) == Some("serve") {
        return serve_command(&args);
    }

    if args.get(1).map(String::as_str) == Some("cdp") {
        return cdp_command(&args);
    }

    if args.get(1).map(String::as_str) == Some("mcp") {
        // mcp_command makes blocking HTTP requests (fetch_page,
        // browser_navigate, ...). Those panic if attempted directly on a
        // tokio worker thread — same issue as reqwest::blocking::Client
        // construction elsewhere in this file, but here it bites on every
        // *use* of the client, not just building it, since the whole
        // request-handling loop lives on the async main task. Running it
        // on a plain OS thread (like handle_cdp already does for its own
        // per-connection blocking calls) sidesteps the tokio runtime
        // entirely instead of needing block_in_place around every call.
        return std::thread::spawn(mcp_command)
            .join()
            .map_err(|_| anyhow::anyhow!("mcp thread panicked"))?;
    }

    let url = args
        .get(1)
        .context("usage: kite-lite <url> [--svg|--png|--pdf output] [--js code]")?;
    let client = http_client()?;
    let page = fetch_page(&client, url).await?;

    if let Some(index) = args.iter().position(|arg| arg == "--svg") {
        let output = args
            .get(index + 1)
            .context("--svg requires an output path")?;
        fs::write(output, render_svg(&page, 1024))?;
        eprintln!("SVG written to {output}");
    }

    if let Some(index) = args.iter().position(|arg| arg == "--png") {
        let output = args
            .get(index + 1)
            .context("--png requires an output path")?;
        fs::write(output, render_png(&page, 1024)?)?;
        eprintln!("PNG written to {output}");
    }

    if let Some(index) = args.iter().position(|arg| arg == "--pdf") {
        let output = args
            .get(index + 1)
            .context("--pdf requires an output path")?;
        fs::write(output, render_pdf(&page, 1024)?)?;
        eprintln!("PDF written to {output}");
    }

    if let Some(index) = args.iter().position(|arg| arg == "--js") {
        let script = args
            .get(index + 1)
            .context("--js requires JavaScript source")?;
        let result = evaluate_js_in_child(&page, script, JS_TIMEOUT)?;
        eprintln!("JavaScript result: {result}");
    }

    println!("{}", serde_json::to_string_pretty(&page)?);
    Ok(())
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .cookie_store(true)
        .user_agent(BROWSER_USER_AGENT)
        .default_headers(browser_like_headers())
        .timeout(HTTP_TIMEOUT)
        .build()?)
}

/// Parses every `Set-Cookie` header on a response — reqwest's own
/// `cookie_store` tracks these internally for outgoing requests but
/// never exposes them, so this is the only way to actually see what a
/// page tried to set.
fn extract_cookies(headers: &reqwest::header::HeaderMap) -> Vec<kite_lite_core::CookieInfo> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(kite_lite_core::parse_set_cookie)
        .collect()
}

async fn fetch_page(client: &reqwest::Client, url: &str) -> Result<kite_lite_core::Page> {
    let response = client.get(url).send().await?.error_for_status()?;
    let final_url = response.url().to_string();
    let cookies = extract_cookies(response.headers());
    let html = response.text().await?;
    let mut page = parse_html(&html);
    page.url = Some(final_url.clone());
    page.cookies = cookies;
    resolve_links(&mut page, &final_url);
    compute_layout(&mut page, DEFAULT_VIEWPORT_WIDTH);
    Ok(page)
}

async fn fetch_command(args: &[String]) -> Result<()> {
    let url = args
        .get(2)
        .context("usage: kite-lite fetch <url> [--output page.json]")?;
    let client = http_client()?;
    let page = fetch_page(&client, url).await?;
    let serialized = serde_json::to_string_pretty(&page)?;
    if let Some(index) = args.iter().position(|arg| arg == "--output") {
        let output = args
            .get(index + 1)
            .context("--output requires a JSON path")?;
        fs::write(output, serialized)?;
        eprintln!("Page snapshot written to {output}");
    } else {
        println!("{serialized}");
    }
    Ok(())
}

fn eval_command(args: &[String]) -> Result<()> {
    let input = args
        .get(2)
        .context("usage: kite-lite eval <page.json> --js <code>")?;
    let index = args
        .iter()
        .position(|arg| arg == "--js")
        .context("eval requires --js <code>")?;
    let script = args
        .get(index + 1)
        .context("--js requires JavaScript source")?;
    let page: kite_lite_core::Page = serde_json::from_str(&fs::read_to_string(input)?)?;
    let result = evaluate_js_in_child(&page, script, JS_TIMEOUT)?;
    println!("{result}");
    Ok(())
}

fn render_command(args: &[String]) -> Result<()> {
    let input = args
        .get(2)
        .context("usage: kite-lite render <page.json> --output page.{svg,png,pdf}")?;
    let index = args
        .iter()
        .position(|arg| arg == "--output")
        .context("render requires --output <path>")?;
    let output = args
        .get(index + 1)
        .context("--output requires a path ending in .svg, .png, or .pdf")?;
    let page: kite_lite_core::Page = serde_json::from_str(&fs::read_to_string(input)?)?;
    let extension = std::path::Path::new(output)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("svg")
        .to_ascii_lowercase();
    let (bytes, format): (Vec<u8>, &str) = match extension.as_str() {
        "png" => (render_png(&page, 1024)?, "PNG"),
        "pdf" => (render_pdf(&page, 1024)?, "PDF"),
        _ => (render_svg(&page, 1024).into_bytes(), "SVG"),
    };
    fs::write(output, bytes)?;
    eprintln!("{format} written to {output}");
    Ok(())
}

/// Resolves a `<url|page.json|file.html>` CLI argument (the shared input
/// convention for `webmcp-lint` and `a11y-lint`) into a `Page`.
async fn load_page_target(target: &str) -> Result<kite_lite_core::Page> {
    if target.starts_with("http://") || target.starts_with("https://") {
        let client = http_client()?;
        fetch_page(&client, target).await
    } else if target.ends_with(".json") {
        Ok(serde_json::from_str(&fs::read_to_string(target)?)?)
    } else {
        Ok(parse_html(&fs::read_to_string(target)?))
    }
}

/// Checks a page's declarative WebMCP forms (`toolname`/`tooldescription`/...)
/// against a handful of practical rules — see `kite_lite_core::webmcp::lint`
/// for what's checked. `target` can be a live URL, an already-fetched
/// `page.json` snapshot, or a raw local `.html` file. Exits non-zero when any
/// `Error`-severity finding turns up, so it's usable as a CI gate.
async fn webmcp_lint_command(args: &[String]) -> Result<()> {
    let target = args
        .get(2)
        .context("usage: kite-lite webmcp-lint <url|page.json|file.html> [--json]")?;
    let json_output = args.iter().any(|arg| arg == "--json");

    let page = load_page_target(target).await?;

    let findings = lint_webmcp(&page);

    if json_output {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    } else if findings.is_empty() {
        println!(
            "Sin hallazgos: no hay problemas en las tools de WebMCP declarativo (o la pagina no declara ninguna via 'toolname')."
        );
    } else {
        for finding in &findings {
            let label = match finding.severity {
                WebMcpLintSeverity::Error => "ERROR",
                WebMcpLintSeverity::Warning => "WARN ",
                WebMcpLintSeverity::Info => "INFO ",
            };
            println!("[{label}] {}: {}", finding.tool, finding.message);
        }
    }

    let error_count = findings
        .iter()
        .filter(|f| f.severity == WebMcpLintSeverity::Error)
        .count();
    if error_count > 0 {
        anyhow::bail!("{error_count} error(es) de WebMCP declarativo encontrados");
    }
    Ok(())
}

/// Checks a page against a handful of practical accessibility rules — see
/// `kite_lite_core::a11y::lint` for what's checked. `target` uses the same
/// `<url|page.json|file.html>` convention as `webmcp-lint`. Exits non-zero
/// when any `Error`-severity finding turns up, so it's usable as a CI gate
/// (today every rule is `Warning`, so it always exits 0 — kept for parity
/// with `webmcp-lint` and future rules that do warrant a hard failure).
async fn a11y_lint_command(args: &[String]) -> Result<()> {
    let target = args
        .get(2)
        .context("usage: kite-lite a11y-lint <url|page.json|file.html> [--json]")?;
    let json_output = args.iter().any(|arg| arg == "--json");

    let page = load_page_target(target).await?;
    let findings = lint_a11y(&page);

    if json_output {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    } else if findings.is_empty() {
        println!("Sin hallazgos: no se detectaron problemas de accesibilidad.");
    } else {
        for finding in &findings {
            let label = match finding.severity {
                A11ySeverity::Error => "ERROR",
                A11ySeverity::Warning => "WARN ",
                A11ySeverity::Info => "INFO ",
            };
            println!("[{label}] {}", finding.message);
        }
    }

    let error_count = findings
        .iter()
        .filter(|f| f.severity == A11ySeverity::Error)
        .count();
    if error_count > 0 {
        anyhow::bail!("{error_count} error(es) de accesibilidad encontrados");
    }
    Ok(())
}

/// Simulates a link-unfurling bot (Twitter/Slack/Facebook/...): resolves
/// title/description/image through the Open Graph -> Twitter Card ->
/// plain-meta/`<title>` fallback chain real crawlers use, and flags the
/// common ways that preview ends up degraded — see
/// `kite_lite_core::social::lint`. Same `<url|page.json|file.html>`
/// convention as the other two linters. Exits non-zero when any
/// `Error`-severity finding turns up (today: no title resolvable at all).
async fn social_lint_command(args: &[String]) -> Result<()> {
    let target = args
        .get(2)
        .context("usage: kite-lite social-lint <url|page.json|file.html> [--json]")?;
    let json_output = args.iter().any(|arg| arg == "--json");

    let page = load_page_target(target).await?;
    let (preview, findings) = lint_social(&page);

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"preview": preview, "findings": findings})
            )?
        );
    } else {
        println!(
            "Titulo:      {}",
            preview.title.as_deref().unwrap_or("(ninguno)")
        );
        println!(
            "Descripcion: {}",
            preview.description.as_deref().unwrap_or("(ninguna)")
        );
        println!(
            "Imagen:      {}",
            preview.image.as_deref().unwrap_or("(ninguna)")
        );
        if findings.is_empty() {
            println!(
                "\nSin hallazgos: el preview deberia verse bien en la mayoria de las plataformas."
            );
        } else {
            println!();
            for finding in &findings {
                let label = match finding.severity {
                    SocialSeverity::Error => "ERROR",
                    SocialSeverity::Warning => "WARN ",
                    SocialSeverity::Info => "INFO ",
                };
                println!("[{label}] {}", finding.message);
            }
        }
    }

    let error_count = findings
        .iter()
        .filter(|f| f.severity == SocialSeverity::Error)
        .count();
    if error_count > 0 {
        anyhow::bail!("{error_count} error(es) de preview social encontrados");
    }
    Ok(())
}

/// Checks a page against a handful of practical on-page SEO rules — see
/// `kite_lite_core::seo::lint`. Deliberately doesn't repeat a11y-lint's
/// heading checks. Same `<url|page.json|file.html>` convention as the
/// other linters. Exits non-zero on any `Error`-severity finding (today:
/// missing title, or an explicit noindex).
async fn seo_lint_command(args: &[String]) -> Result<()> {
    let target = args
        .get(2)
        .context("usage: kite-lite seo-lint <url|page.json|file.html> [--json]")?;
    let json_output = args.iter().any(|arg| arg == "--json");

    let page = load_page_target(target).await?;
    let findings = lint_seo(&page);

    if json_output {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    } else if findings.is_empty() {
        println!("Sin hallazgos: no se detectaron problemas basicos de SEO.");
    } else {
        for finding in &findings {
            let label = match finding.severity {
                SeoSeverity::Error => "ERROR",
                SeoSeverity::Warning => "WARN ",
                SeoSeverity::Info => "INFO ",
            };
            println!("[{label}] {}", finding.message);
        }
    }

    let error_count = findings
        .iter()
        .filter(|f| f.severity == SeoSeverity::Error)
        .count();
    if error_count > 0 {
        anyhow::bail!("{error_count} error(es) de SEO encontrados");
    }
    Ok(())
}

fn serve_command(args: &[String]) -> Result<()> {
    let address = args.get(2).map(String::as_str).unwrap_or("127.0.0.1:8787");
    let listener = TcpListener::bind(address)?;
    eprintln!("kite-lite control API listening on {address}");
    // One thread per connection, same as `cdp_command` — otherwise a single
    // slow/stalled client (deliberately or not) would starve every other
    // request against this server, since `handle_http` blocks on I/O.
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    if let Err(error) = handle_http(stream) {
                        eprintln!("request error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("connection error: {error}"),
        }
    }
    Ok(())
}

/// Bundles a page with a persistent HTTP client so cookies set by one
/// navigation (and any redirect hops along the way) are carried over to
/// the next `Page.navigate`/`Page.reload` within the same `cdp` session —
/// "session" here meaning the lifetime of this `cdp` server process, since
/// it only ever tracks a single page/target.
struct CdpSession {
    page: kite_lite_core::Page,
    client: reqwest::blocking::Client,
    /// The node id (in the traversal-order numbering `cdp_node`/`find_node`
    /// use) of the `<input>`/`<textarea>` last clicked via
    /// `Input.dispatchMouseEvent`, so `Input.dispatchKeyEvent` knows where
    /// to type. Reset to `None` whenever the page is replaced (navigate,
    /// reload, or a click that submits a form) since those ids are only
    /// valid against the exact tree they were computed from.
    focused_node_id: Option<u32>,
}

fn cdp_command(args: &[String]) -> Result<()> {
    let (snapshot, address) = match args.get(2).map(String::as_str) {
        Some(value) if value.parse::<std::net::SocketAddr>().is_ok() => (None, value),
        snapshot => (
            snapshot,
            args.get(3).map(String::as_str).unwrap_or("127.0.0.1:9222"),
        ),
    };
    let page = snapshot
        .map(|path| -> Result<kite_lite_core::Page> {
            Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
        })
        .transpose()?
        .unwrap_or_else(|| parse_html("<title>New Page</title><body></body>"));
    // `cdp_command` runs synchronously inside the #[tokio::main] runtime
    // (main() calls it directly, before any std::thread::spawn). Building a
    // reqwest::blocking::Client spins up its own inner runtime, which panics
    // if attempted directly on a tokio worker thread — block_in_place hands
    // this thread off so that's safe.
    let client = tokio::task::block_in_place(|| {
        reqwest::blocking::Client::builder()
            .cookie_store(true)
            .user_agent(BROWSER_USER_AGENT)
            .default_headers(browser_like_headers())
            .timeout(HTTP_TIMEOUT)
            .build()
    })?;
    let session = CdpSession {
        page,
        client,
        focused_node_id: None,
    };
    let session = Arc::new(Mutex::new(session));
    let listener = TcpListener::bind(address)?;
    eprintln!("kite-lite CDP listening on {address}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let session = session.clone();
                std::thread::spawn(move || {
                    if let Err(error) = handle_cdp_connection(stream, session) {
                        eprintln!("CDP connection error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("CDP connection error: {error}"),
        }
    }
    Ok(())
}

/// Routes an incoming TCP connection on the `cdp` port to either the plain
/// HTTP discovery endpoints (`/json*`, the same ones real Chrome exposes on
/// its remote-debugging port so tools can find the WebSocket URL without
/// already knowing it) or the CDP WebSocket handler, by peeking at the
/// request without consuming it — `handle_cdp`'s WebSocket handshake still
/// needs to read those same bytes fresh if this turns out not to be a
/// `/json*` request.
fn handle_cdp_connection(stream: TcpStream, session: Arc<Mutex<CdpSession>>) -> Result<()> {
    // A connection that never sends anything (or trickles bytes in slowly)
    // would otherwise block `peek`/the discovery handler's `read` forever —
    // this is scoped to just that one thread (each connection already runs
    // on its own, see `cdp_command`), but still worth bounding. Cleared
    // below once we know it's a WebSocket, since a live CDP session is
    // supposed to block waiting for the next message.
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    let mut peek_buf = [0_u8; 1024];
    let peeked = stream.peek(&mut peek_buf)?;
    let first_line = String::from_utf8_lossy(&peek_buf[..peeked])
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();

    if first_line.starts_with("GET /json") {
        return handle_cdp_http_discovery(stream, &session);
    }
    stream.set_read_timeout(None)?;
    handle_cdp(stream, session)
}

/// Serves the `/json/version`, `/json`, and `/json/list` endpoints that
/// tools use to discover the WebSocket debugger URL from a plain HTTP
/// request — the same discovery flow real Chrome's remote-debugging port
/// supports, and what most CDP client libraries (`connectOverCDP`-style
/// APIs, `chrome-remote-interface`, etc.) try first.
fn handle_cdp_http_discovery(
    mut stream: TcpStream,
    session: &Arc<Mutex<CdpSession>>,
) -> Result<()> {
    let mut buffer = vec![0_u8; 8192];
    let size = stream.read(&mut buffer)?;
    let request = std::str::from_utf8(&buffer[..size])?;
    let mut lines = request.lines();
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let host = lines
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("host")
                    .then(|| value.trim().to_string())
            })
        })
        .unwrap_or_else(|| "127.0.0.1:9222".to_string());
    let ws_url = format!("ws://{host}/");

    let (title, url) = match session.lock() {
        Ok(guard) => (
            guard.page.title.clone().unwrap_or_default(),
            guard
                .page
                .url
                .clone()
                .unwrap_or_else(|| "about:blank".to_string()),
        ),
        Err(_) => (String::new(), "about:blank".to_string()),
    };

    let body = match path {
        "/json/version" => serde_json::json!({
            "Browser": "kite-lite/0.1.0",
            "Protocol-Version": "1.3",
            "User-Agent": "kite-lite",
            "V8-Version": "0.0.0",
            "WebKit-Version": "0.0.0",
            "webSocketDebuggerUrl": ws_url
        }),
        "/json" | "/json/list" => serde_json::json!([{
            "id": CDP_TARGET_ID,
            "type": "page",
            "title": title,
            "url": url,
            "webSocketDebuggerUrl": ws_url
        }]),
        _ => serde_json::json!({"error": "not found"}),
    };
    let payload = serde_json::to_string(&body)?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

fn handle_cdp(stream: TcpStream, session: Arc<Mutex<CdpSession>>) -> Result<()> {
    let mut socket = accept(stream)?;
    loop {
        let message = socket.read()?;
        let Message::Text(text) = message else {
            continue;
        };
        let request: serde_json::Value = serde_json::from_str(text.as_ref())?;
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        // Real CDP scopes every page-level command/response/event to the
        // sessionId a client got back from Target.attachToTarget. kite-lite
        // only has one target, so there's no real routing to do — we just
        // echo whatever sessionId the client attached the message with, so
        // clients that (correctly) expect one don't discard our responses.
        let incoming_session_id = request
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        let mut response = cdp_response(id, method, request.get("params"), &session);
        if let Some(session_id) = &incoming_session_id {
            response["sessionId"] = serde_json::json!(session_id);
        }
        socket.send(Message::Text(serde_json::to_string(&response)?.into()))?;

        // A navigation may come from Page.navigate/Page.reload directly, or
        // indirectly from clicking a link/submitting a form via
        // Input.dispatchMouseEvent — detect it by the response shape
        // (navigate_page's success case always includes "frameId") rather
        // than by which method was called, so both paths fire the same
        // load events a real CDP client waits on.
        if response
            .get("result")
            .and_then(|result| result.get("frameId"))
            .is_some()
        {
            for mut event in [
                serde_json::json!({
                    "method": "Page.frameStartedLoading",
                    "params": {"frameId": "kite-lite-frame"}
                }),
                serde_json::json!({
                    "method": "Page.loadEventFired",
                    "params": {"timestamp": 0.0}
                }),
                serde_json::json!({
                    "method": "Page.frameStoppedLoading",
                    "params": {"frameId": "kite-lite-frame"}
                }),
            ] {
                if let Some(session_id) = &incoming_session_id {
                    event["sessionId"] = serde_json::json!(session_id);
                }
                socket.send(Message::Text(serde_json::to_string(&event)?.into()))?;
            }
        }

        let auto_attach = method == "Target.setAutoAttach"
            && request
                .get("params")
                .and_then(|params| params.get("autoAttach"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        if method == "Target.attachToTarget" || auto_attach {
            let target = match session.lock() {
                Ok(guard) => target_info(&guard.page),
                Err(_) => continue,
            };
            let attached_event = serde_json::json!({
                "method": "Target.attachedToTarget",
                "params": {
                    "sessionId": CDP_SESSION_ID,
                    "targetInfo": target,
                    "waitingForDebugger": false
                }
            });
            socket.send(Message::Text(
                serde_json::to_string(&attached_event)?.into(),
            ))?;
        }
    }
}

fn cdp_response(
    id: serde_json::Value,
    method: &str,
    params: Option<&serde_json::Value>,
    session: &Arc<Mutex<CdpSession>>,
) -> serde_json::Value {
    let mut session_guard = match session.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return serde_json::json!({
                "id": id,
                "error": {"message": "page state unavailable"}
            });
        }
    };
    let result = match method {
        "Browser.getVersion" => serde_json::json!({
            "protocolVersion": "1.3",
            "product": "KiteLite/0.1.0",
            "revision": "local",
            "userAgent": "kite-lite",
            "jsVersion": "Boa"
        }),
        "Runtime.enable" | "Page.enable" | "Network.enable" => serde_json::json!({}),
        "DOM.enable" => serde_json::json!({}),
        "Target.setDiscoverTargets" | "Target.setAutoAttach" | "Target.disposeBrowserContext" => {
            serde_json::json!({})
        }
        "Target.getTargets" => {
            serde_json::json!({ "targetInfos": [target_info(&session_guard.page)] })
        }
        "Target.getTargetInfo" => {
            serde_json::json!({ "targetInfo": target_info(&session_guard.page) })
        }
        "Target.attachToTarget" | "Target.attachToBrowserTarget" => {
            serde_json::json!({ "sessionId": CDP_SESSION_ID })
        }
        "Target.closeTarget" => serde_json::json!({ "success": true }),
        "DOM.getDocument" => {
            let mut next_id = 1;
            serde_json::json!({
                "root": cdp_node(&session_guard.page.root, &mut next_id),
                "backendNodeId": 1
            })
        }
        "DOM.querySelector" => {
            let selector = params
                .and_then(|value| value.get("selector"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let mut next_id = 1;
            let node_id =
                find_selector(&session_guard.page.root, selector, &mut next_id).unwrap_or(0);
            serde_json::json!({"nodeId": node_id})
        }
        "DOM.querySelectorAll" => {
            let selector = params
                .and_then(|value| value.get("selector"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let mut next_id = 1;
            let mut node_ids = Vec::new();
            find_selectors(
                &session_guard.page.root,
                selector,
                &mut next_id,
                &mut node_ids,
            );
            serde_json::json!({"nodeIds": node_ids})
        }
        "DOM.getAttributes" => {
            let node_id = params
                .and_then(|value| value.get("nodeId"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            let mut next_id = 1;
            let attributes = find_node(&session_guard.page.root, node_id, &mut next_id)
                .map(element_attribute_pairs)
                .unwrap_or_default();
            serde_json::json!({"attributes": attributes})
        }
        "DOM.getBoxModel" => {
            let node_id = params
                .and_then(|value| value.get("nodeId"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            let mut next_id = 1;
            match find_node(&session_guard.page.root, node_id, &mut next_id)
                .and_then(|element| element.layout)
            {
                Some(layout) => {
                    let (x1, y1, x2, y2) = (
                        layout.x,
                        layout.y,
                        layout.x + layout.width,
                        layout.y + layout.height,
                    );
                    serde_json::json!({
                        "model": {
                            "content": [x1, y1, x2, y1, x2, y2, x1, y2],
                            "width": layout.width,
                            "height": layout.height
                        }
                    })
                }
                None => serde_json::json!({}),
            }
        }
        "DOM.getOuterHTML" => {
            let node_id = params
                .and_then(|value| value.get("nodeId"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            let mut next_id = 1;
            let html = find_node(&session_guard.page.root, node_id, &mut next_id)
                .map(element_html)
                .unwrap_or_default();
            serde_json::json!({"outerHTML": html})
        }
        "Page.getNavigationHistory" => serde_json::json!({
            "currentIndex": 0,
            "entries": [{"id": 1, "url": session_guard.page.url.clone().unwrap_or_else(|| "about:blank".to_string()), "userTypedURL": session_guard.page.url.clone().unwrap_or_else(|| "about:blank".to_string()), "title": session_guard.page.title.clone().unwrap_or_default(), "transitionType": "typed"}]
        }),
        "Page.getResourceTree" => serde_json::json!({
            "frameTree": {
                "frame": {
                    "id": "kite-lite-frame",
                    "loaderId": "kite-lite-loader",
                    "url": session_guard.page.url.clone().unwrap_or_else(|| "about:blank".to_string()),
                    "mimeType": "text/html",
                    "securityOrigin": ""
                },
                "resources": []
            }
        }),
        "Page.captureSnapshot" => serde_json::json!({
            "data": session_guard.page.source.clone().unwrap_or_default()
        }),
        "Page.navigate" => {
            let url = params
                .and_then(|value| value.get("url"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            navigate_locked(session, session_guard, url)
        }
        "Page.reload" => {
            let url = session_guard.page.url.clone().unwrap_or_default();
            navigate_locked(session, session_guard, &url)
        }
        "Input.dispatchMouseEvent" => {
            let event_type = params
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let y = params
                .and_then(|value| value.get("y"))
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            if event_type == "mousePressed" {
                dispatch_click_locked(session, session_guard, y)
            } else {
                serde_json::json!({})
            }
        }
        "Input.dispatchKeyEvent" => {
            let event_type = params
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let text = params
                .and_then(|value| value.get("text"))
                .and_then(serde_json::Value::as_str);
            let key = params
                .and_then(|value| value.get("key"))
                .and_then(serde_json::Value::as_str);
            dispatch_key(&mut session_guard, event_type, text, key);
            serde_json::json!({})
        }
        "Runtime.evaluate" => {
            let expression = params
                .and_then(|value| value.get("expression"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let page_snapshot = session_guard.page.clone();
            drop(session_guard);
            match evaluate_js_in_child(&page_snapshot, expression, JS_TIMEOUT) {
                Ok(value) => serde_json::json!({"result": {"type": "string", "value": value}}),
                Err(error) => serde_json::json!({"exceptionDetails": {"text": error.to_string()}}),
            }
        }
        _ => serde_json::json!({}),
    };
    serde_json::json!({"id": id, "result": result})
}

/// The `Target.TargetInfo` object for kite-lite's one and only page/target.
fn target_info(page: &kite_lite_core::Page) -> serde_json::Value {
    serde_json::json!({
        "targetId": CDP_TARGET_ID,
        "type": "page",
        "title": page.title.clone().unwrap_or_default(),
        "url": page.url.clone().unwrap_or_else(|| "about:blank".to_string()),
        "attached": true,
        "canAccessOpener": false,
        "browserContextId": "kite-lite-context"
    })
}

/// Runs the blocking GET plus DOM/layout rebuild for a navigation, with no
/// dependency on `CdpSession` or its lock — callers that share a session
/// behind an `Arc<Mutex<_>>` should release that lock before calling this
/// (see `navigate_locked`) so a slow/unresponsive server doesn't block every
/// other CDP command sharing the session.
fn fetch_navigation(
    client: &reqwest::blocking::Client,
    url: &str,
) -> std::result::Result<kite_lite_core::Page, serde_json::Value> {
    match client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
    {
        Ok(response) => {
            let final_url = response.url().to_string();
            let cookies = extract_cookies(response.headers());
            match response.text() {
                Ok(html) => {
                    let mut page = parse_html(&html);
                    page.url = Some(final_url.clone());
                    page.cookies = cookies;
                    resolve_links(&mut page, &final_url);
                    compute_layout(&mut page, DEFAULT_VIEWPORT_WIDTH);
                    Ok(page)
                }
                Err(error) => Err(serde_json::json!({"errorText": error.to_string()})),
            }
        }
        Err(error) => Err(serde_json::json!({"errorText": error.to_string()})),
    }
}

/// Commits a fetched page to the session. Cheap (no I/O) — safe to run
/// while holding the session lock.
fn apply_navigation(session: &mut CdpSession, page: kite_lite_core::Page) -> serde_json::Value {
    session.page = page;
    session.focused_node_id = None;
    serde_json::json!({"frameId": "kite-lite-frame", "loaderId": "kite-lite-loader"})
}

fn navigate_page(session: &mut CdpSession, url: &str) -> serde_json::Value {
    if url.is_empty() {
        return serde_json::json!({"errorText": "no URL available for navigation"});
    }
    match fetch_navigation(&session.client, url) {
        Ok(page) => apply_navigation(session, page),
        Err(error_json) => error_json,
    }
}

/// Same as `navigate_page`, but for callers holding a `MutexGuard` on a
/// session shared across CDP connections (`cdp_response`'s `Page.navigate`/
/// `Page.reload`). Clones the (cheaply, `Arc`-backed) client and drops the
/// guard before the blocking network request, then reacquires the lock only
/// to commit the result — so this navigation no longer blocks unrelated CDP
/// commands (e.g. another connection's `Runtime.evaluate`, or a concurrent
/// `DOM.querySelector`) for as long as the remote server takes to respond.
fn navigate_locked(
    session: &Arc<Mutex<CdpSession>>,
    guard: MutexGuard<CdpSession>,
    url: &str,
) -> serde_json::Value {
    if url.is_empty() {
        return serde_json::json!({"errorText": "no URL available for navigation"});
    }
    let client = guard.client.clone();
    drop(guard);
    match fetch_navigation(&client, url) {
        Ok(page) => match session.lock() {
            Ok(mut guard) => apply_navigation(&mut guard, page),
            Err(_) => serde_json::json!({"errorText": "page state unavailable"}),
        },
        Err(error_json) => error_json,
    }
}

/// Flattens the small set of attributes this project tracks into CDP's
/// `["key", "value", "key2", "value2", ...]` attribute array shape.
fn element_attribute_pairs(element: &kite_lite_core::Element) -> Vec<String> {
    let href_key = if element.tag == "form" {
        "action"
    } else {
        "href"
    };
    [
        (href_key, &element.href),
        ("value", &element.value),
        ("name", &element.name),
        ("type", &element.kind),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.as_ref().map(|value| (key.to_string(), value.clone())))
    .flat_map(|(key, value)| [key, value])
    .collect()
}

fn cdp_node(element: &kite_lite_core::Element, next_id: &mut u32) -> serde_json::Value {
    let node_id = *next_id;
    *next_id += 1;
    let is_document = element.tag == "document";
    let children = element
        .children
        .iter()
        .map(|child| cdp_node(child, next_id))
        .collect::<Vec<_>>();
    let attributes = element_attribute_pairs(element);
    let node_name = if is_document {
        "#document".to_string()
    } else {
        element.tag.to_uppercase()
    };
    let local_name = if is_document {
        String::new()
    } else {
        element.tag.clone()
    };
    serde_json::json!({
        "nodeId": node_id,
        "nodeType": if is_document {9} else {1},
        "nodeName": node_name,
        "localName": local_name,
        "nodeValue": "",
        "childNodeCount": children.len(),
        "children": children,
        "attributes": attributes
    })
}

fn find_selector(
    element: &kite_lite_core::Element,
    selector: &str,
    next_id: &mut u32,
) -> Option<u32> {
    let current_id = *next_id;
    *next_id += 1;
    let matches = selector == "*" || element.tag.eq_ignore_ascii_case(selector);
    if matches && element.tag != "document" {
        return Some(current_id);
    }
    for child in &element.children {
        if let Some(node_id) = find_selector(child, selector, next_id) {
            return Some(node_id);
        }
    }
    None
}

fn find_selectors(
    element: &kite_lite_core::Element,
    selector: &str,
    next_id: &mut u32,
    matches: &mut Vec<u32>,
) {
    let current_id = *next_id;
    *next_id += 1;
    if (selector == "*" || element.tag.eq_ignore_ascii_case(selector)) && element.tag != "document"
    {
        matches.push(current_id);
    }
    for child in &element.children {
        find_selectors(child, selector, next_id, matches);
    }
}

fn find_node<'a>(
    element: &'a kite_lite_core::Element,
    target_id: u32,
    next_id: &mut u32,
) -> Option<&'a kite_lite_core::Element> {
    let current_id = *next_id;
    *next_id += 1;
    if current_id == target_id {
        return Some(element);
    }
    for child in &element.children {
        if let Some(found) = find_node(child, target_id, next_id) {
            return Some(found);
        }
    }
    None
}

fn find_node_mut<'a>(
    element: &'a mut kite_lite_core::Element,
    target_id: u32,
    next_id: &mut u32,
) -> Option<&'a mut kite_lite_core::Element> {
    let current_id = *next_id;
    *next_id += 1;
    if current_id == target_id {
        return Some(element);
    }
    for child in &mut element.children {
        if let Some(found) = find_node_mut(child, target_id, next_id) {
            return Some(found);
        }
    }
    None
}

/// Finds the id (in the same traversal-order numbering as `find_node`) of
/// the deepest leaf element whose layout box spans vertical position `y`.
/// Layout in this project only stacks elements vertically (see
/// `kite_lite_core::compute_layout`), so `x` plays no part in hit-testing —
/// every row is treated as spanning the full viewport width.
fn hit_test(element: &kite_lite_core::Element, y: f64, next_id: &mut u32) -> Option<u32> {
    let current_id = *next_id;
    *next_id += 1;
    if element.children.is_empty() {
        // head/script/style are non-rendering leaves that sit at whatever
        // degenerate zero-size box the layout left them at (often the same
        // coordinates as real content next to them) — never let them win a
        // hit test.
        if matches!(element.tag.as_str(), "head" | "script" | "style") {
            return None;
        }
        let layout = element.layout?;
        return (y >= layout.y && y < layout.y + layout.height.max(1.0)).then_some(current_id);
    }
    for child in &element.children {
        if let Some(id) = hit_test(child, y, next_id) {
            return Some(id);
        }
    }
    None
}

/// Returns the ids of every element on the path from the root down to
/// `target_id`, inclusive, in root-to-leaf order.
fn ancestor_chain(
    element: &kite_lite_core::Element,
    target_id: u32,
    next_id: &mut u32,
    path: &mut Vec<u32>,
) -> bool {
    let current_id = *next_id;
    *next_id += 1;
    path.push(current_id);
    if current_id == target_id {
        return true;
    }
    for child in &element.children {
        if ancestor_chain(child, target_id, next_id, path) {
            return true;
        }
    }
    path.pop();
    false
}

/// What clicking a given element should do — computed read-only from the
/// page so the caller can apply the mutation (navigate, focus, or submit)
/// afterward without fighting the borrow checker over `session.page`.
enum ClickAction {
    None,
    Navigate(String),
    Focus(u32),
    Submit { action_url: String, query: String },
}

fn plan_click(page: &kite_lite_core::Page, y: f64) -> ClickAction {
    let mut next_id = 1;
    match hit_test(&page.root, y, &mut next_id) {
        Some(hit_id) => plan_action_for_node(page, hit_id),
        None => ClickAction::None,
    }
}

/// The part of `plan_click` that decides what to do once a target node id
/// is already known — shared with selector-based clicking (`browser_click`
/// in MCP mode), which finds its target via `find_selector` instead of a
/// `y` coordinate.
fn plan_action_for_node(page: &kite_lite_core::Page, hit_id: u32) -> ClickAction {
    let mut next_id = 1;
    let Some(hit) = find_node(&page.root, hit_id, &mut next_id) else {
        return ClickAction::None;
    };

    match hit.tag.as_str() {
        "a" => hit
            .href
            .clone()
            .map(ClickAction::Navigate)
            .unwrap_or(ClickAction::None),
        "input" if hit.kind.as_deref() == Some("submit") => {
            find_enclosing_form_submit(page, hit_id).unwrap_or(ClickAction::None)
        }
        "input" | "textarea" => ClickAction::Focus(hit_id),
        "button" => find_enclosing_form_submit(page, hit_id).unwrap_or(ClickAction::None),
        _ => ClickAction::None,
    }
}

/// Walks upward from `hit_id` looking for the closest `<form>` ancestor,
/// and if one exists, builds the query string it would submit from its
/// descendant `<input>`/`<textarea>` fields' current `name`/`value`. Only
/// GET-style submission is supported — there's no request body, so a
/// form's `method="post"` is not honored.
fn find_enclosing_form_submit(page: &kite_lite_core::Page, hit_id: u32) -> Option<ClickAction> {
    let mut next_id = 1;
    let mut path = Vec::new();
    ancestor_chain(&page.root, hit_id, &mut next_id, &mut path);

    path.into_iter().rev().find_map(|ancestor_id| {
        let mut next_id = 1;
        let form = find_node(&page.root, ancestor_id, &mut next_id)?;
        (form.tag == "form").then(|| {
            let action_url = form
                .href
                .clone()
                .or_else(|| page.url.clone())
                .unwrap_or_default();
            let mut fields = Vec::new();
            collect_form_fields(form, &mut fields);
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (name, value) in &fields {
                serializer.append_pair(name, value);
            }
            ClickAction::Submit {
                action_url,
                query: serializer.finish(),
            }
        })
    })
}

fn collect_form_fields(element: &kite_lite_core::Element, fields: &mut Vec<(String, String)>) {
    if let (Some(name), true) = (
        &element.name,
        matches!(element.tag.as_str(), "input" | "textarea"),
    ) {
        fields.push((name.clone(), element.value.clone().unwrap_or_default()));
    }
    for child in &element.children {
        collect_form_fields(child, fields);
    }
}

/// Locates the click, then dispatches to `navigate_locked`/local state via
/// `apply_click_action_locked` for callers holding a `MutexGuard` on a
/// session shared across CDP connections (`cdp_response`'s
/// `Input.dispatchMouseEvent`). Only the `Navigate`/`Submit` actions involve
/// network I/O; those hand off to `navigate_locked` to release the lock
/// during the request, same as `Page.navigate`/`Page.reload` — otherwise a
/// click that submits a form to a slow server would block every other CDP
/// command sharing this session for as long as that request takes.
fn dispatch_click_locked(
    session: &Arc<Mutex<CdpSession>>,
    guard: MutexGuard<CdpSession>,
    y: f64,
) -> serde_json::Value {
    let action = plan_click(&guard.page, y);
    apply_click_action_locked(session, guard, action)
}

fn apply_click_action_locked(
    session: &Arc<Mutex<CdpSession>>,
    mut guard: MutexGuard<CdpSession>,
    action: ClickAction,
) -> serde_json::Value {
    match action {
        ClickAction::Navigate(url) => navigate_locked(session, guard, &url),
        ClickAction::Focus(node_id) => {
            guard.focused_node_id = Some(node_id);
            serde_json::json!({})
        }
        ClickAction::Submit { action_url, query } => {
            let separator = if action_url.contains('?') { "&" } else { "?" };
            let target = if query.is_empty() {
                action_url
            } else {
                format!("{action_url}{separator}{query}")
            };
            navigate_locked(session, guard, &target)
        }
        ClickAction::None => serde_json::json!({}),
    }
}

fn apply_click_action(session: &mut CdpSession, action: ClickAction) -> serde_json::Value {
    match action {
        ClickAction::Navigate(url) => navigate_page(session, &url),
        ClickAction::Focus(node_id) => {
            session.focused_node_id = Some(node_id);
            serde_json::json!({})
        }
        ClickAction::Submit { action_url, query } => {
            let separator = if action_url.contains('?') { "&" } else { "?" };
            let target = if query.is_empty() {
                action_url
            } else {
                format!("{action_url}{separator}{query}")
            };
            navigate_page(session, &target)
        }
        ClickAction::None => serde_json::json!({}),
    }
}

fn dispatch_key(session: &mut CdpSession, event_type: &str, text: Option<&str>, key: Option<&str>) {
    let Some(node_id) = session.focused_node_id else {
        return;
    };
    let mut next_id = 1;
    let Some(element) = find_node_mut(&mut session.page.root, node_id, &mut next_id) else {
        return;
    };
    let mut value = element.value.clone().unwrap_or_default();
    match (event_type, text, key) {
        ("char", Some(text), _) => value.push_str(text),
        ("keyDown", _, Some("Backspace")) => {
            value.pop();
        }
        _ => return,
    }
    element.value = Some(value);
}

fn element_html(element: &kite_lite_core::Element) -> String {
    if element.tag == "document" {
        return element.children.iter().map(element_html).collect();
    }
    let attributes: String = element_attribute_pairs(element)
        .chunks(2)
        .map(|pair| format!(" {}=\"{}\"", pair[0], html_escape(&pair[1])))
        .collect();
    let children = element
        .children
        .iter()
        .map(element_html)
        .collect::<String>();
    let text = html_escape(&element.text);
    format!(
        "<{tag}{attributes}>{text}{children}</{tag}>",
        tag = element.tag
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Reads a full HTTP/1.1 request (headers, then a `Content-Length` body if
/// one is declared) off `stream`. A single `read()` isn't guaranteed to
/// return a whole request in one shot — a body split across TCP segments
/// (common for anything past a few KB, e.g. `/v1/parse`/`/v1/render` with a
/// real page) would otherwise get silently truncated at whatever happened
/// to arrive first. Loops until the `\r\n\r\n` header terminator is seen and
/// then until `Content-Length` bytes of body have arrived, bounded by
/// `MAX_HTTP_REQUEST_BYTES` so a request that never completes (or lies
/// about its length) can't grow this buffer without limit.
fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            anyhow::bail!("connection closed before request headers completed");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_HTTP_REQUEST_BYTES {
            anyhow::bail!("request exceeds {MAX_HTTP_REQUEST_BYTES}-byte limit");
        }
        if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let content_length = std::str::from_utf8(&buffer[..header_end])
        .ok()
        .and_then(|headers| {
            headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);

    let total_len = header_end
        .checked_add(content_length)
        .filter(|&total| total <= MAX_HTTP_REQUEST_BYTES)
        .context("request exceeds MAX_HTTP_REQUEST_BYTES-byte limit")?;
    while buffer.len() < total_len {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            anyhow::bail!("connection closed before request body completed");
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    buffer.truncate(total_len);
    Ok(buffer)
}

fn handle_http(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    let buffer = read_http_request(&mut stream)?;
    let request = std::str::from_utf8(&buffer)?;
    let (headers, body) = request
        .split_once("\r\n\r\n")
        .context("malformed HTTP request")?;
    let mut request_line = headers
        .lines()
        .next()
        .context("missing request line")?
        .split_whitespace();
    let method = request_line.next().context("missing HTTP method")?;
    let full_path = request_line.next().context("missing HTTP path")?;
    let (path, query) = full_path.split_once('?').unwrap_or((full_path, ""));

    let (status, content_type, payload): (&str, &str, Vec<u8>) = match (method, path) {
        ("GET", "/health") => (
            "200 OK",
            "application/json",
            br#"{"ok":true,"service":"kite-lite"}"#.to_vec(),
        ),
        ("POST", "/v1/parse") => {
            let mut page = parse_html(body);
            compute_layout(&mut page, DEFAULT_VIEWPORT_WIDTH);
            ("200 OK", "application/json", serde_json::to_vec(&page)?)
        }
        ("POST", "/v1/render") => {
            let page: kite_lite_core::Page = serde_json::from_str(body)?;
            let format = url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == "format")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_else(|| "svg".to_string());
            match format.as_str() {
                "png" => ("200 OK", "image/png", render_png(&page, 1024)?),
                "pdf" => ("200 OK", "application/pdf", render_pdf(&page, 1024)?),
                _ => (
                    "200 OK",
                    "image/svg+xml",
                    render_svg(&page, 1024).into_bytes(),
                ),
            }
        }
        ("POST", "/v1/eval") => {
            let request: EvalRequest = serde_json::from_str(body)?;
            let value = evaluate_js_in_child(&request.page, &request.script, JS_TIMEOUT)?;
            (
                "200 OK",
                "application/json",
                serde_json::to_vec(&serde_json::json!({"value": value}))?,
            )
        }
        _ => (
            "404 Not Found",
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
        ),
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&payload)?;
    Ok(())
}

fn evaluate_js_in_child(
    page: &kite_lite_core::Page,
    script: &str,
    timeout: Duration,
) -> Result<String> {
    let request = serde_json::to_string(&EvalRequest {
        page: page.clone(),
        script: script.to_owned(),
    })?;
    let js_binary =
        env::current_exe()?.with_file_name(format!("kite-lite-js{}", env::consts::EXE_SUFFIX));
    let mut child = Command::new(js_binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn the kite-lite-js evaluator process")?;

    let mut stdin = child
        .stdin
        .take()
        .context("failed to open JavaScript child stdin")?;
    let mut stdout = child
        .stdout
        .take()
        .context("failed to open JavaScript child stdout")?;

    // Both directions run on their own thread rather than sequentially on
    // this one: the child reads all of stdin before it writes anything, so
    // a request bigger than the OS pipe buffer would block this thread's
    // write() until the child drains it — fine on its own — but a *result*
    // bigger than the pipe buffer (an eval script can return an arbitrarily
    // long string) would then block the child's write to stdout, and this
    // thread was only polling `try_wait` without ever reading stdout until
    // after the child exited. Neither side could make progress: a real
    // deadlock, previously masked by `timeout` eventually killing the
    // child and reporting it as if the script itself had been slow.
    // `read_to_string` on the reader thread naturally returns once the
    // child closes stdout, so it doubles as the completion signal — no
    // need to also poll `try_wait`.
    std::thread::spawn(move || {
        let _ = stdin.write_all(request.as_bytes());
    });
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let result = stdout.read_to_string(&mut output).map(|_| output);
        let _ = sender.send(result);
    });

    match receiver.recv_timeout(timeout) {
        Ok(Ok(output)) => {
            let _ = child.wait();
            let response: EvalResponse = serde_json::from_str(&output)
                .context("JavaScript child returned invalid output")?;
            match (response.value, response.error) {
                (Some(value), _) => Ok(value),
                (_, Some(error)) => Err(anyhow::anyhow!(error)),
                _ => Err(anyhow::anyhow!("JavaScript child returned no result")),
            }
        }
        Ok(Err(error)) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(anyhow::Error::new(error).context("failed to read JavaScript child stdout"))
        }
        Err(_timed_out) => {
            child.kill()?;
            child.wait()?;
            Err(anyhow::anyhow!(
                "JavaScript execution exceeded {} ms",
                timeout.as_millis()
            ))
        }
    }
}

/// Runs kite-lite as an MCP (Model Context Protocol) server over stdio:
/// newline-delimited JSON-RPC 2.0 on stdin/stdout, per the MCP spec's stdio
/// transport. Hand-rolled with `serde_json` rather than an MCP SDK crate,
/// matching how this project already hand-rolls the CDP protocol — the
/// surface needed (`initialize`, `tools/list`, `tools/call`) is small.
///
/// stdio framing means stdout must carry *only* protocol messages, one JSON
/// object per line — any diagnostic output must go to stderr instead, or a
/// client reading stdout as JSON-RPC would choke on it.
///
/// This mode is inherently single-connection and sequential (one request
/// in, one response out, over one pair of pipes), so unlike `cdp`/`serve`
/// there's no concurrency to isolate: `mcp_command` runs as one plain
/// function call, on a dedicated OS thread spawned from `main()` (see the
/// call site) specifically so its blocking HTTP calls never touch the
/// tokio runtime — and per-request state (the persistent `browser_*`
/// session) is just a local variable.
fn mcp_command() -> Result<()> {
    use std::io::BufRead;

    let fetch_client = reqwest::blocking::Client::builder()
        .cookie_store(true)
        .user_agent(BROWSER_USER_AGENT)
        .default_headers(browser_like_headers())
        .timeout(HTTP_TIMEOUT)
        .build()?;
    let browser_client = reqwest::blocking::Client::builder()
        .cookie_store(true)
        .user_agent(BROWSER_USER_AGENT)
        .default_headers(browser_like_headers())
        .timeout(HTTP_TIMEOUT)
        .build()?;
    let mut session = CdpSession {
        page: parse_html("<title>New Page</title><body></body>"),
        client: browser_client,
        focused_node_id: None,
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    eprintln!("kite-lite MCP server ready on stdio");

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("mcp: failed to parse request: {error}");
                continue;
            }
        };
        // A message with no "id" is a notification (e.g.
        // notifications/initialized) — no response is expected or sent.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        let envelope =
            match mcp_dispatch(method, request.get("params"), &fetch_client, &mut session) {
                Ok(result) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": code, "message": message}
                }),
            };
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        stdout.flush()?;
    }
    Ok(())
}

fn mcp_dispatch(
    method: &str,
    params: Option<&serde_json::Value>,
    fetch_client: &reqwest::blocking::Client,
    session: &mut CdpSession,
) -> std::result::Result<serde_json::Value, (i64, String)> {
    match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "kite-lite", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(serde_json::json!({"tools": mcp_tool_definitions()})),
        "tools/call" => mcp_tool_call(params, fetch_client, session),
        "ping" => Ok(serde_json::json!({})),
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn mcp_tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "fetch_page",
            "description": "Fetch a URL and return a lightweight summary (final URL, title, extracted text, links). Does not execute the page's own JavaScript and does not affect the persistent browser_* session.",
            "inputSchema": {
                "type": "object",
                "properties": {"url": {"type": "string", "description": "The URL to fetch"}},
                "required": ["url"]
            }
        }),
        serde_json::json!({
            "name": "render_screenshot",
            "description": "Fetch a URL and render it to a PNG image or SVG markup. This is a simplified, text-driven rendering (no CSS, no JavaScript-rendered content), not pixel-accurate to a real browser.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "format": {"type": "string", "enum": ["png", "svg"], "description": "Defaults to png"}
                },
                "required": ["url"]
            }
        }),
        serde_json::json!({
            "name": "eval_js",
            "description": "Fetch a URL and evaluate a JavaScript expression against a read-only snapshot of its document (document.title, document.body.innerText, and a limited document.querySelector for the first h1/h2/h3/p/a/button). Runs in an isolated process with no network, filesystem, or real-DOM access, and cannot affect the page.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "script": {"type": "string"}
                },
                "required": ["url", "script"]
            }
        }),
        serde_json::json!({
            "name": "browser_navigate",
            "description": "Navigate the persistent browsing session to a URL, following redirects and carrying cookies from prior navigations in this same session. Returns a page summary.",
            "inputSchema": {
                "type": "object",
                "properties": {"url": {"type": "string"}},
                "required": ["url"]
            }
        }),
        serde_json::json!({
            "name": "browser_click",
            "description": "Click the first element in the current session page matching a CSS-like selector (tag name). Clicking <a href> navigates; clicking <input>/<textarea> focuses it for browser_type; clicking a submit button or input[type=submit] submits its enclosing form (GET only, no request body) and navigates. This does not run onclick handlers or any page JavaScript.",
            "inputSchema": {
                "type": "object",
                "properties": {"selector": {"type": "string"}},
                "required": ["selector"]
            }
        }),
        serde_json::json!({
            "name": "browser_type",
            "description": "Type text into the currently focused input/textarea in the session (focus one first with browser_click, or pass 'selector' here to focus it in the same call).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "selector": {"type": "string", "description": "Optional: focus this element before typing"}
                },
                "required": ["text"]
            }
        }),
        serde_json::json!({
            "name": "browser_get_dom",
            "description": "Return the outer HTML of the current session page, or of the first element matching a selector.",
            "inputSchema": {
                "type": "object",
                "properties": {"selector": {"type": "string"}}
            }
        }),
        serde_json::json!({
            "name": "browser_screenshot",
            "description": "Render the current session page (as last navigated by browser_navigate) to a PNG image or SVG markup.",
            "inputSchema": {
                "type": "object",
                "properties": {"format": {"type": "string", "enum": ["png", "svg"]}}
            }
        }),
        serde_json::json!({
            "name": "browser_call_tool",
            "description": "Call a WebMCP tool declared on the current session page via the toolname/tooldescription/toolparamdescription HTML attributes on a <form> (see https://github.com/webmachinelearning/webmcp/blob/main/declarative-api-explainer.md). Fills the form's fields from 'arguments' (falling back to each field's current value when omitted) and submits it: GET only, no request body — the same limitation as browser_click's form submit. Only the declarative HTML-attribute API is supported, not the imperative navigator.modelContext JavaScript API (no page JavaScript runs). Discoverable tools are listed under 'tools' in any page summary returned by fetch_page, browser_navigate, or browser_click.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The tool's toolname"},
                    "arguments": {"type": "object", "description": "Values keyed by each field's name attribute"}
                },
                "required": ["name"]
            }
        }),
    ]
}

fn mcp_tool_call(
    params: Option<&serde_json::Value>,
    fetch_client: &reqwest::blocking::Client,
    session: &mut CdpSession,
) -> std::result::Result<serde_json::Value, (i64, String)> {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(serde_json::Value::as_str)
        .ok_or((-32602, "missing tool name".to_string()))?;
    let empty = serde_json::json!({});
    let arguments = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);

    let result = match name {
        "fetch_page" => mcp_fetch_page(fetch_client, arguments),
        "render_screenshot" => mcp_render_screenshot(fetch_client, arguments),
        "eval_js" => mcp_eval_js(fetch_client, arguments),
        "browser_navigate" => mcp_browser_navigate(session, arguments),
        "browser_click" => mcp_browser_click(session, arguments),
        "browser_type" => mcp_browser_type(session, arguments),
        "browser_get_dom" => mcp_browser_get_dom(session, arguments),
        "browser_screenshot" => mcp_browser_screenshot(session, arguments),
        "browser_call_tool" => mcp_browser_call_tool(session, arguments),
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };

    match result {
        Ok(content) => Ok(serde_json::json!({"content": [content], "isError": false})),
        Err(message) => Ok(serde_json::json!({
            "content": [{"type": "text", "text": message}],
            "isError": true
        })),
    }
}

fn require_str<'a>(
    arguments: &'a serde_json::Value,
    key: &str,
) -> std::result::Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("missing '{key}' argument"))
}

fn page_summary(page: &kite_lite_core::Page) -> serde_json::Value {
    let mut summary = serde_json::json!({
        "url": page.url,
        "title": page.title,
        "text": page.text,
        "links": page.links,
    });
    let tools = kite_lite_core::discover_webmcp_tools(page);
    if !tools.is_empty() {
        summary["tools"] = serde_json::Value::Array(tools.iter().map(webmcp_tool_json).collect());
    }
    if !page.cookies.is_empty() {
        summary["cookies"] = serde_json::to_value(&page.cookies).unwrap_or_default();
    }
    summary
}

fn webmcp_tool_json(tool: &kite_lite_core::WebMcpTool) -> serde_json::Value {
    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "autosubmit": tool.autosubmit,
        "inputSchema": tool.input_schema,
    })
}

fn text_content(text: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"type": "text", "text": text.into()})
}

/// Blocking-client equivalent of `fetch_page` (the async fn used by the CLI
/// path), so MCP's tool handlers can stay plain synchronous functions.
fn fetch_page_blocking(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<kite_lite_core::Page> {
    let response = client.get(url).send()?.error_for_status()?;
    let final_url = response.url().to_string();
    let cookies = extract_cookies(response.headers());
    let html = response.text()?;
    let mut page = parse_html(&html);
    page.url = Some(final_url.clone());
    page.cookies = cookies;
    resolve_links(&mut page, &final_url);
    compute_layout(&mut page, DEFAULT_VIEWPORT_WIDTH);
    Ok(page)
}

fn render_page_as_content(
    page: &kite_lite_core::Page,
    format: &str,
) -> std::result::Result<serde_json::Value, String> {
    if format == "svg" {
        return Ok(text_content(render_svg(page, 1024)));
    }
    let png = render_png(page, 1024).map_err(|error| error.to_string())?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
    Ok(serde_json::json!({"type": "image", "data": encoded, "mimeType": "image/png"}))
}

fn mcp_fetch_page(
    client: &reqwest::blocking::Client,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let url = require_str(arguments, "url")?;
    let page = fetch_page_blocking(client, url).map_err(|error| error.to_string())?;
    Ok(text_content(page_summary(&page).to_string()))
}

fn mcp_render_screenshot(
    client: &reqwest::blocking::Client,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let url = require_str(arguments, "url")?;
    let format = arguments
        .get("format")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("png");
    let page = fetch_page_blocking(client, url).map_err(|error| error.to_string())?;
    render_page_as_content(&page, format)
}

fn mcp_eval_js(
    client: &reqwest::blocking::Client,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let url = require_str(arguments, "url")?;
    let script = require_str(arguments, "script")?;
    let page = fetch_page_blocking(client, url).map_err(|error| error.to_string())?;
    let value =
        evaluate_js_in_child(&page, script, JS_TIMEOUT).map_err(|error| error.to_string())?;
    Ok(text_content(value))
}

fn mcp_browser_navigate(
    session: &mut CdpSession,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let url = require_str(arguments, "url")?;
    let outcome = navigate_page(session, url);
    if let Some(error_text) = outcome.get("errorText").and_then(serde_json::Value::as_str) {
        return Err(error_text.to_string());
    }
    Ok(text_content(page_summary(&session.page).to_string()))
}

fn mcp_browser_click(
    session: &mut CdpSession,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let selector = require_str(arguments, "selector")?;
    let mut next_id = 1;
    let Some(node_id) = find_selector(&session.page.root, selector, &mut next_id) else {
        return Err(format!("no element matches selector '{selector}'"));
    };
    let action = plan_action_for_node(&session.page, node_id);
    let navigated = matches!(
        action,
        ClickAction::Navigate(_) | ClickAction::Submit { .. }
    );
    let outcome = apply_click_action(session, action);
    if let Some(error_text) = outcome.get("errorText").and_then(serde_json::Value::as_str) {
        return Err(error_text.to_string());
    }
    let summary = if navigated {
        serde_json::json!({"action": "navigated", "page": page_summary(&session.page)})
    } else if session.focused_node_id == Some(node_id) {
        serde_json::json!({"action": "focused"})
    } else {
        serde_json::json!({"action": "none"})
    };
    Ok(text_content(summary.to_string()))
}

fn mcp_browser_call_tool(
    session: &mut CdpSession,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let tool_name = require_str(arguments, "name")?;
    let empty = serde_json::json!({});
    let tool_arguments = arguments.get("arguments").unwrap_or(&empty);
    let (action_url, query) =
        kite_lite_core::build_webmcp_submission(&session.page, tool_name, tool_arguments)
            .ok_or_else(|| format!("no WebMCP tool named '{tool_name}' on the current page"))?;
    let separator = if action_url.contains('?') { "&" } else { "?" };
    let target = if query.is_empty() {
        action_url
    } else {
        format!("{action_url}{separator}{query}")
    };
    let outcome = navigate_page(session, &target);
    if let Some(error_text) = outcome.get("errorText").and_then(serde_json::Value::as_str) {
        return Err(error_text.to_string());
    }
    Ok(text_content(page_summary(&session.page).to_string()))
}

fn mcp_browser_type(
    session: &mut CdpSession,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let text = require_str(arguments, "text")?;
    if let Some(selector) = arguments
        .get("selector")
        .and_then(serde_json::Value::as_str)
    {
        let mut next_id = 1;
        let Some(node_id) = find_selector(&session.page.root, selector, &mut next_id) else {
            return Err(format!("no element matches selector '{selector}'"));
        };
        session.focused_node_id = Some(node_id);
    }
    if session.focused_node_id.is_none() {
        return Err("no focused input — click one first, or pass 'selector'".to_string());
    }
    dispatch_key(session, "char", Some(text), None);
    Ok(text_content("ok"))
}

fn mcp_browser_get_dom(
    session: &mut CdpSession,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    match arguments
        .get("selector")
        .and_then(serde_json::Value::as_str)
    {
        Some(selector) => {
            let mut next_id = 1;
            let Some(node_id) = find_selector(&session.page.root, selector, &mut next_id) else {
                return Err(format!("no element matches selector '{selector}'"));
            };
            let mut next_id = 1;
            let html = find_node(&session.page.root, node_id, &mut next_id)
                .map(element_html)
                .unwrap_or_default();
            Ok(text_content(html))
        }
        None => Ok(text_content(element_html(&session.page.root))),
    }
}

fn mcp_browser_screenshot(
    session: &mut CdpSession,
    arguments: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let format = arguments
        .get("format")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("png");
    render_page_as_content(&session.page, format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_escapes_reserved_characters() {
        assert_eq!(html_escape("<a> & \"b\""), "&lt;a&gt; &amp; &quot;b&quot;");
    }

    fn el(
        tag: &str,
        text: &str,
        href: Option<&str>,
        children: Vec<kite_lite_core::Element>,
    ) -> kite_lite_core::Element {
        kite_lite_core::Element {
            tag: tag.to_string(),
            text: text.to_string(),
            href: href.map(str::to_string),
            children,
            ..Default::default()
        }
    }

    #[test]
    fn element_html_includes_escaped_href_and_text() {
        let element = el("a", "Link & more", Some("/x?a=1&b=2"), Vec::new());
        assert_eq!(
            element_html(&element),
            "<a href=\"/x?a=1&amp;b=2\">Link &amp; more</a>"
        );
    }

    #[test]
    fn element_html_flattens_document_children() {
        let root = el(
            "document",
            "",
            None,
            vec![
                el("p", "One", None, Vec::new()),
                el("p", "Two", None, Vec::new()),
            ],
        );
        assert_eq!(element_html(&root), "<p>One</p><p>Two</p>");
    }

    #[test]
    fn cdp_node_builds_expected_json_shape() {
        let root = el(
            "document",
            "",
            None,
            vec![el("a", "Docs", Some("/docs"), Vec::new())],
        );
        let mut next_id = 1;
        let node = cdp_node(&root, &mut next_id);
        assert_eq!(node["nodeType"], 9);
        assert_eq!(node["nodeName"], "#document");
        assert_eq!(node["childNodeCount"], 1);
        let child = &node["children"][0];
        assert_eq!(child["nodeName"], "A");
        assert_eq!(child["localName"], "a");
        assert_eq!(child["attributes"], serde_json::json!(["href", "/docs"]));
    }

    fn test_session(page: kite_lite_core::Page) -> Arc<Mutex<CdpSession>> {
        Arc::new(Mutex::new(CdpSession {
            page,
            client: reqwest::blocking::Client::new(),
            focused_node_id: None,
        }))
    }

    #[test]
    fn navigate_page_rejects_empty_url() {
        let mut session = CdpSession {
            page: parse_html("<title>T</title>"),
            client: reqwest::blocking::Client::new(),
            focused_node_id: None,
        };
        let result = navigate_page(&mut session, "");
        assert_eq!(
            result,
            serde_json::json!({"errorText": "no URL available for navigation"})
        );
    }

    #[test]
    fn cdp_response_reports_browser_version() {
        let session = test_session(parse_html("<title>T</title>"));
        let response = cdp_response(serde_json::json!(1), "Browser.getVersion", None, &session);
        assert_eq!(response["id"], serde_json::json!(1));
        assert_eq!(response["result"]["product"], "KiteLite/0.1.0");
    }

    #[test]
    fn cdp_response_reports_navigation_history() {
        let mut page = parse_html("<title>Example</title>");
        page.url = Some("https://example.com".to_string());
        let session = test_session(page);
        let response = cdp_response(
            serde_json::json!(2),
            "Page.getNavigationHistory",
            None,
            &session,
        );
        let entry = &response["result"]["entries"][0];
        assert_eq!(entry["url"], "https://example.com");
        assert_eq!(entry["title"], "Example");
    }

    #[test]
    fn cdp_response_captures_snapshot_source() {
        let session = test_session(parse_html("<title>Example</title>"));
        let response = cdp_response(serde_json::json!(3), "Page.captureSnapshot", None, &session);
        assert_eq!(response["result"]["data"], "<title>Example</title>");
    }

    #[test]
    fn cdp_response_defaults_to_empty_result_for_unknown_method() {
        let session = test_session(parse_html("<title>Example</title>"));
        let response = cdp_response(serde_json::json!(4), "Nonexistent.method", None, &session);
        assert_eq!(response["result"], serde_json::json!({}));
    }

    #[test]
    fn cdp_finds_selector_and_returns_matching_outer_html() {
        let session = test_session(parse_html("<title>T</title><body><h1>Hello</h1></body>"));
        let select = cdp_response(
            serde_json::json!(5),
            "DOM.querySelector",
            Some(&serde_json::json!({"selector": "h1"})),
            &session,
        );
        let node_id = select["result"]["nodeId"].as_u64().unwrap();
        assert!(node_id > 0);

        let outer = cdp_response(
            serde_json::json!(6),
            "DOM.getOuterHTML",
            Some(&serde_json::json!({"nodeId": node_id})),
            &session,
        );
        assert_eq!(outer["result"]["outerHTML"], "<h1>Hello</h1>");
    }

    #[test]
    fn cdp_finds_all_matching_selectors() {
        let session = test_session(parse_html(
            "<title>T</title><body><p>One</p><p>Two</p></body>",
        ));
        let select_all = cdp_response(
            serde_json::json!(7),
            "DOM.querySelectorAll",
            Some(&serde_json::json!({"selector": "p"})),
            &session,
        );
        let node_ids = select_all["result"]["nodeIds"].as_array().unwrap();
        assert_eq!(node_ids.len(), 2);
    }

    #[test]
    fn target_get_targets_reports_the_one_page() {
        let mut page = parse_html("<title>Hi</title>");
        page.url = Some("https://example.com/".to_string());
        let session = test_session(page);
        let response = cdp_response(serde_json::json!(1), "Target.getTargets", None, &session);
        let targets = response["result"]["targetInfos"].as_array().unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0]["type"], "page");
        assert_eq!(targets[0]["title"], "Hi");
        assert_eq!(targets[0]["url"], "https://example.com/");
    }

    #[test]
    fn target_attach_to_target_returns_a_session_id() {
        let session = test_session(parse_html("<title>Hi</title>"));
        let response = cdp_response(
            serde_json::json!(2),
            "Target.attachToTarget",
            Some(&serde_json::json!({"targetId": CDP_TARGET_ID})),
            &session,
        );
        assert_eq!(response["result"]["sessionId"], CDP_SESSION_ID);
    }

    #[test]
    fn click_on_a_link_navigates() {
        let mut page = parse_html(r#"<h1>Title</h1><a href="https://example.com/x">Go</a>"#);
        compute_layout(&mut page, 800.0);
        let mut next_id = 1;
        let link_id = find_selector(&page.root, "a", &mut next_id).unwrap();
        let mut next_id = 1;
        let link = find_node(&page.root, link_id, &mut next_id).unwrap();
        let click_y = link.layout.unwrap().y + 1.0;

        match plan_click(&page, click_y) {
            ClickAction::Navigate(url) => assert_eq!(url, "https://example.com/x"),
            _ => panic!("expected Navigate, got a different action"),
        }
    }

    #[test]
    fn click_on_an_input_focuses_it() {
        let mut page = parse_html(r#"<input name="q" value="hi">"#);
        compute_layout(&mut page, 800.0);
        let mut next_id = 1;
        let input_id = find_selector(&page.root, "input", &mut next_id).unwrap();
        let mut next_id = 1;
        let input = find_node(&page.root, input_id, &mut next_id).unwrap();
        let click_y = input.layout.unwrap().y + 0.5;

        match plan_click(&page, click_y) {
            ClickAction::Focus(focused_id) => assert_eq!(focused_id, input_id),
            _ => panic!("expected Focus"),
        }
    }

    #[test]
    fn click_on_submit_button_collects_form_fields_and_targets_action() {
        let mut page = parse_html(
            r#"<form action="/search"><input name="q" value="rust"><button>Go</button></form>"#,
        );
        compute_layout(&mut page, 800.0);
        let mut next_id = 1;
        let button_id = find_selector(&page.root, "button", &mut next_id).unwrap();
        let mut next_id = 1;
        let button = find_node(&page.root, button_id, &mut next_id).unwrap();
        let click_y = button.layout.unwrap().y + 1.0;

        match plan_click(&page, click_y) {
            ClickAction::Submit { action_url, query } => {
                assert_eq!(action_url, "/search");
                assert_eq!(query, "q=rust");
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn dispatch_key_types_into_and_backspaces_the_focused_input() {
        let mut page = parse_html(r#"<input name="q">"#);
        compute_layout(&mut page, 800.0);
        let mut next_id = 1;
        let input_id = find_selector(&page.root, "input", &mut next_id).unwrap();

        let mut session = CdpSession {
            page,
            client: reqwest::blocking::Client::new(),
            focused_node_id: Some(input_id),
        };

        dispatch_key(&mut session, "char", Some("h"), None);
        dispatch_key(&mut session, "char", Some("i"), None);
        let mut next_id = 1;
        let input = find_node(&session.page.root, input_id, &mut next_id).unwrap();
        assert_eq!(input.value.as_deref(), Some("hi"));

        dispatch_key(&mut session, "keyDown", None, Some("Backspace"));
        let mut next_id = 1;
        let input = find_node(&session.page.root, input_id, &mut next_id).unwrap();
        assert_eq!(input.value.as_deref(), Some("h"));
    }

    #[test]
    fn dispatch_key_without_a_focused_node_is_a_no_op() {
        let mut page = parse_html(r#"<input name="q">"#);
        compute_layout(&mut page, 800.0);
        let mut session = CdpSession {
            page,
            client: reqwest::blocking::Client::new(),
            focused_node_id: None,
        };
        dispatch_key(&mut session, "char", Some("x"), None);
    }
}
