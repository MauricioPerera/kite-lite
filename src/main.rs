use anyhow::{Context, Result};
use kite_lite_core::{parse_html, render_svg, resolve_links, EvalRequest, EvalResponse};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::{accept, Message};

const JS_TIMEOUT: Duration = Duration::from_millis(1500);

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

    if args.get(1).map(String::as_str) == Some("serve") {
        return serve_command(&args);
    }

    if args.get(1).map(String::as_str) == Some("cdp") {
        return cdp_command(&args);
    }

    let url = args
        .get(1)
        .context("usage: kite-lite <url> [--svg output.svg] [--js code]")?;
    let client = http_client()?;
    let page = fetch_page(&client, url).await?;

    if let Some(index) = args.iter().position(|arg| arg == "--svg") {
        let output = args
            .get(index + 1)
            .context("--svg requires an output path")?;
        fs::write(output, render_svg(&page, 1024))?;
        eprintln!("SVG written to {output}");
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
    Ok(reqwest::Client::builder().cookie_store(true).build()?)
}

async fn fetch_page(client: &reqwest::Client, url: &str) -> Result<kite_lite_core::Page> {
    let response = client.get(url).send().await?.error_for_status()?;
    let final_url = response.url().to_string();
    let html = response.text().await?;
    let mut page = parse_html(&html);
    page.url = Some(final_url.clone());
    resolve_links(&mut page, &final_url);
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
        .context("usage: kite-lite render <page.json> --output page.svg")?;
    let index = args
        .iter()
        .position(|arg| arg == "--output")
        .context("render requires --output <path>")?;
    let output = args
        .get(index + 1)
        .context("--output requires an SVG path")?;
    let page: kite_lite_core::Page = serde_json::from_str(&fs::read_to_string(input)?)?;
    fs::write(output, render_svg(&page, 1024))?;
    eprintln!("SVG written to {output}");
    Ok(())
}

fn serve_command(args: &[String]) -> Result<()> {
    let address = args.get(2).map(String::as_str).unwrap_or("127.0.0.1:8787");
    let listener = TcpListener::bind(address)?;
    eprintln!("kite-lite control API listening on {address}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = handle_http(stream) {
                    eprintln!("request error: {error}");
                }
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
        reqwest::blocking::Client::builder().cookie_store(true).build()
    })?;
    let session = CdpSession { page, client };
    let session = Arc::new(Mutex::new(session));
    let listener = TcpListener::bind(address)?;
    eprintln!("kite-lite CDP listening on {address}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let session = session.clone();
                std::thread::spawn(move || {
                    if let Err(error) = handle_cdp(stream, session) {
                        eprintln!("CDP connection error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("CDP connection error: {error}"),
        }
    }
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
        let response = cdp_response(id, method, request.get("params"), &session);
        socket.send(Message::Text(serde_json::to_string(&response)?.into()))?;
        if (method == "Page.navigate" || method == "Page.reload")
            && response
                .get("result")
                .and_then(|result| result.get("errorText"))
                .is_none()
        {
            for event in [
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
                socket.send(Message::Text(serde_json::to_string(&event)?.into()))?;
            }
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
            find_selectors(&session_guard.page.root, selector, &mut next_id, &mut node_ids);
            serde_json::json!({"nodeIds": node_ids})
        }
        "DOM.getAttributes" => {
            let node_id = params
                .and_then(|value| value.get("nodeId"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;
            let mut next_id = 1;
            let attributes = find_node(&session_guard.page.root, node_id, &mut next_id)
                .map(|element| {
                    element
                        .href
                        .as_ref()
                        .map(|href| vec!["href".to_string(), href.clone()])
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            serde_json::json!({"attributes": attributes})
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
            navigate_page(&mut session_guard, url)
        }
        "Page.reload" => {
            let url = session_guard.page.url.clone().unwrap_or_default();
            navigate_page(&mut session_guard, &url)
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

fn navigate_page(session: &mut CdpSession, url: &str) -> serde_json::Value {
    if url.is_empty() {
        return serde_json::json!({"errorText": "no URL available for navigation"});
    }
    match session
        .client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
    {
        Ok(response) => {
            let final_url = response.url().to_string();
            match response.text() {
                Ok(html) => {
                    let mut page = parse_html(&html);
                    page.url = Some(final_url.clone());
                    resolve_links(&mut page, &final_url);
                    session.page = page;
                    serde_json::json!({"frameId": "kite-lite-frame", "loaderId": "kite-lite-loader"})
                }
                Err(error) => serde_json::json!({"errorText": error.to_string()}),
            }
        }
        Err(error) => serde_json::json!({"errorText": error.to_string()}),
    }
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
    let mut attributes = Vec::new();
    if let Some(href) = &element.href {
        attributes.push(serde_json::json!("href"));
        attributes.push(serde_json::json!(href));
    }
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

fn element_html(element: &kite_lite_core::Element) -> String {
    if element.tag == "document" {
        return element.children.iter().map(element_html).collect();
    }
    let attributes = element
        .href
        .as_ref()
        .map(|href| format!(" href=\"{}\"", html_escape(href)))
        .unwrap_or_default();
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

fn handle_http(mut stream: TcpStream) -> Result<()> {
    let mut buffer = vec![0_u8; 2 * 1024 * 1024];
    let size = stream.read(&mut buffer)?;
    let request = std::str::from_utf8(&buffer[..size])?;
    let (headers, body) = request
        .split_once("\r\n\r\n")
        .context("malformed HTTP request")?;
    let mut request_line = headers
        .lines()
        .next()
        .context("missing request line")?
        .split_whitespace();
    let method = request_line.next().context("missing HTTP method")?;
    let path = request_line.next().context("missing HTTP path")?;

    let (status, content_type, payload) = match (method, path) {
        ("GET", "/health") => (
            "200 OK",
            "application/json",
            r#"{"ok":true,"service":"kite-lite"}"#.to_string(),
        ),
        ("POST", "/v1/parse") => {
            let page = parse_html(body);
            ("200 OK", "application/json", serde_json::to_string(&page)?)
        }
        ("POST", "/v1/render") => {
            let page: kite_lite_core::Page = serde_json::from_str(body)?;
            ("200 OK", "image/svg+xml", render_svg(&page, 1024))
        }
        ("POST", "/v1/eval") => {
            let request: EvalRequest = serde_json::from_str(body)?;
            let value = evaluate_js_in_child(&request.page, &request.script, JS_TIMEOUT)?;
            (
                "200 OK",
                "application/json",
                serde_json::to_string(&serde_json::json!({"value": value}))?,
            )
        }
        _ => (
            "404 Not Found",
            "application/json",
            r#"{"error":"not found"}"#.to_string(),
        ),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
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
    let js_binary = env::current_exe()?
        .with_file_name(format!("kite-lite-js{}", env::consts::EXE_SUFFIX));
    let mut child = Command::new(js_binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn the kite-lite-js evaluator process")?;

    child
        .stdin
        .take()
        .context("failed to open JavaScript child stdin")?
        .write_all(request.as_bytes())?;

    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let mut output = String::new();
            child
                .stdout
                .take()
                .context("failed to open JavaScript child stdout")?
                .read_to_string(&mut output)?;
            let response: EvalResponse = serde_json::from_str(&output)
                .context("JavaScript child returned invalid output")?;
            return match (response.value, response.error) {
                (Some(value), _) => Ok(value),
                (_, Some(error)) => Err(anyhow::anyhow!(error)),
                _ => Err(anyhow::anyhow!("JavaScript child returned no result")),
            };
        }
        if started.elapsed() >= timeout {
            child.kill()?;
            child.wait()?;
            return Err(anyhow::anyhow!(
                "JavaScript execution exceeded {} ms",
                timeout.as_millis()
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_escapes_reserved_characters() {
        assert_eq!(
            html_escape("<a> & \"b\""),
            "&lt;a&gt; &amp; &quot;b&quot;"
        );
    }

    #[test]
    fn element_html_includes_escaped_href_and_text() {
        let element = kite_lite_core::Element {
            tag: "a".to_string(),
            text: "Link & more".to_string(),
            href: Some("/x?a=1&b=2".to_string()),
            children: Vec::new(),
        };
        assert_eq!(
            element_html(&element),
            "<a href=\"/x?a=1&amp;b=2\">Link &amp; more</a>"
        );
    }

    #[test]
    fn element_html_flattens_document_children() {
        let root = kite_lite_core::Element {
            tag: "document".to_string(),
            text: String::new(),
            href: None,
            children: vec![
                kite_lite_core::Element {
                    tag: "p".to_string(),
                    text: "One".to_string(),
                    href: None,
                    children: Vec::new(),
                },
                kite_lite_core::Element {
                    tag: "p".to_string(),
                    text: "Two".to_string(),
                    href: None,
                    children: Vec::new(),
                },
            ],
        };
        assert_eq!(element_html(&root), "<p>One</p><p>Two</p>");
    }

    #[test]
    fn cdp_node_builds_expected_json_shape() {
        let root = kite_lite_core::Element {
            tag: "document".to_string(),
            text: String::new(),
            href: None,
            children: vec![kite_lite_core::Element {
                tag: "a".to_string(),
                text: "Docs".to_string(),
                href: Some("/docs".to_string()),
                children: Vec::new(),
            }],
        };
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
        }))
    }

    #[test]
    fn navigate_page_rejects_empty_url() {
        let mut session = CdpSession {
            page: parse_html("<title>T</title>"),
            client: reqwest::blocking::Client::new(),
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
}
