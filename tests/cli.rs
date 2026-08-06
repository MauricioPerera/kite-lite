//! Black-box integration tests that spawn the real `kite-lite` binary.
//!
//! These exist because `env::current_exe()` inside a unit test points at the
//! test harness, not at `kite-lite` — so the child-process JS isolation used
//! by `eval`/`serve`/CDP's `Runtime.evaluate` can only be exercised here,
//! via `CARGO_BIN_EXE_kite-lite`, which Cargo only sets for `tests/`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kite-lite"))
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_file(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("kite-lite-test-{}-{id}-{name}", std::process::id()))
}

const SAMPLE_PAGE: &str = r#"{
    "title": "Test Page",
    "text": "Hello",
    "links": [],
    "root": {
        "tag": "document",
        "text": "Hello",
        "href": null,
        "children": [
            { "tag": "h1", "text": "Hello", "href": null, "children": [] }
        ]
    }
}"#;

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write_sample_page(name: &str) -> TempPath {
    let path = temp_file(name);
    std::fs::write(&path, SAMPLE_PAGE).expect("failed to write sample page");
    TempPath(path)
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server on port {port} did not start listening in time");
}

#[test]
fn eval_runs_isolated_js_and_returns_document_title() {
    let page = write_sample_page("eval-page.json");

    let output = Command::new(bin_path())
        .args(["eval", page.0.to_str().unwrap(), "--js", "document.title"])
        .output()
        .expect("failed to run kite-lite eval");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Test Page");
}

#[test]
fn render_writes_svg_file() {
    let page = write_sample_page("render-page.json");
    let svg_path = temp_file("render-page.svg");
    let _svg_guard = TempPath(svg_path.clone());

    let output = Command::new(bin_path())
        .args([
            "render",
            page.0.to_str().unwrap(),
            "--output",
            svg_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run kite-lite render");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let svg = std::fs::read_to_string(&svg_path).expect("svg output missing");
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("Hello"));
}

fn find_element_by_tag<'a>(element: &'a serde_json::Value, tag: &str) -> Option<&'a serde_json::Value> {
    if element["tag"] == tag {
        return Some(element);
    }
    element["children"]
        .as_array()?
        .iter()
        .find_map(|child| find_element_by_tag(child, tag))
}

fn http_request(port: u16, method: &str, path: &str, body: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("failed to connect");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("failed to send request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("failed to read response");
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .expect("malformed HTTP response");
    let status = headers.lines().next().unwrap_or_default().to_string();
    (status, body.to_string())
}

#[test]
fn serve_exposes_health_parse_render_and_eval() {
    let port = 18787;
    let child = Command::new(bin_path())
        .args(["serve", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start kite-lite serve");
    let _guard = ChildGuard(child);
    wait_for_port(port);

    let (status, body) = http_request(port, "GET", "/health", "");
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"ok\":true"));

    let (status, body) = http_request(
        port,
        "POST",
        "/v1/parse",
        "<title>Hi</title><body><h1>Hola</h1></body>",
    );
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"title\":\"Hi\""));
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("invalid /v1/parse JSON");
    let h1 = find_element_by_tag(&parsed["root"], "h1").expect("h1 missing from parsed tree");
    assert!(
        h1["layout"]["height"].as_f64().unwrap() > 0.0,
        "expected a computed layout on h1, got {h1:?}"
    );

    let sample_page = r#"{"title":"Hi","text":"Hola","links":[],"root":{"tag":"document","text":"Hola","href":null,"children":[{"tag":"h1","text":"Hola","href":null,"children":[]}]}}"#;

    let (status, body) = http_request(port, "POST", "/v1/render", sample_page);
    assert!(status.contains("200"), "status: {status}");
    assert!(body.starts_with("<svg"));

    let eval_body = format!(r#"{{"page":{sample_page},"script":"document.title"}}"#);
    let (status, body) = http_request(port, "POST", "/v1/eval", &eval_body);
    assert!(status.contains("200"), "status: {status}");
    assert!(body.contains("\"value\":\"Hi\""));

    let (status, _) = http_request(port, "GET", "/nope", "");
    assert!(status.contains("404"), "status: {status}");
}

fn read_json_message(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
) -> serde_json::Value {
    loop {
        let message = socket.read().expect("failed to read CDP message");
        if let tungstenite::Message::Text(text) = message {
            return serde_json::from_str(&text).expect("invalid JSON from CDP server");
        }
    }
}

#[test]
fn cdp_runtime_evaluate_uses_isolated_child_process() {
    let page = write_sample_page("cdp-page.json");
    let port = 18923;

    let child = Command::new(bin_path())
        .args([
            "cdp",
            page.0.to_str().unwrap(),
            &format!("127.0.0.1:{port}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start kite-lite cdp");
    let _guard = ChildGuard(child);
    wait_for_port(port);

    let (mut socket, _) =
        tungstenite::connect(format!("ws://127.0.0.1:{port}/")).expect("failed to connect CDP websocket");

    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({
                "id": 1,
                "method": "Runtime.evaluate",
                "params": {"expression": "document.title"}
            })
            .to_string()
            .into(),
        ))
        .expect("failed to send Runtime.evaluate");
    let response = read_json_message(&mut socket);
    assert_eq!(response["id"], serde_json::json!(1));
    assert_eq!(response["result"]["result"]["value"], "Test Page");

    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({
                "id": 2,
                "method": "DOM.querySelector",
                "params": {"selector": "h1"}
            })
            .to_string()
            .into(),
        ))
        .expect("failed to send DOM.querySelector");
    let response = read_json_message(&mut socket);
    let node_id = response["result"]["nodeId"].as_u64().unwrap();
    assert!(node_id > 0);

    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({
                "id": 3,
                "method": "DOM.getOuterHTML",
                "params": {"nodeId": node_id}
            })
            .to_string()
            .into(),
        ))
        .expect("failed to send DOM.getOuterHTML");
    let response = read_json_message(&mut socket);
    assert_eq!(response["result"]["outerHTML"], "<h1>Hello</h1>");
}

/// A minimal local HTTP server used to verify URL/cookie/redirect handling
/// without depending on the real internet. `/start` sets a cookie and
/// redirects to `/final`; both `/final` and `/whoami` report whether that
/// cookie came back on the request, without ever setting it themselves —
/// so a passing check on `/whoami` proves the cookie survived from an
/// earlier, separate request.
fn spawn_cookie_redirect_server(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("failed to bind mock server");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0_u8; 4096];
            let size = match stream.read(&mut buffer) {
                Ok(size) => size,
                Err(_) => continue,
            };
            let request = String::from_utf8_lossy(&buffer[..size]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let has_cookie = request.lines().any(|line| {
                line.to_ascii_lowercase().starts_with("cookie:") && line.contains("session=abc123")
            });

            let response = match path.as_str() {
                "/start" => format!(
                    "HTTP/1.1 302 Found\r\nSet-Cookie: session=abc123\r\nLocation: http://127.0.0.1:{port}/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                "/final" | "/whoami" => {
                    let title = if has_cookie { "Cookie OK" } else { "No Cookie" };
                    let body = format!("<title>{title}</title>");
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
}

fn drain_events(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    count: usize,
) {
    for _ in 0..count {
        socket.read().expect("failed to drain CDP event");
    }
}

#[test]
fn fetch_follows_redirect_and_resolves_final_url() {
    let port = 18999;
    spawn_cookie_redirect_server(port);
    wait_for_port(port);

    let output_path = temp_file("fetch-redirect.json");
    let _guard = TempPath(output_path.clone());

    let output = Command::new(bin_path())
        .args([
            "fetch",
            &format!("http://127.0.0.1:{port}/start"),
            "--output",
            output_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run kite-lite fetch");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let page: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&output_path).expect("missing fetch output"),
    )
    .expect("invalid page JSON");

    // The cookie set on /start's redirect must have been carried onto the
    // /final request within this single fetch's redirect chain.
    assert_eq!(page["title"], "Cookie OK");
    // page.url must reflect the URL actually rendered (post-redirect), not
    // the one originally requested.
    assert_eq!(page["url"], format!("http://127.0.0.1:{port}/final"));
}

#[test]
fn cdp_session_persists_cookies_across_navigations() {
    let mock_port = 19010;
    spawn_cookie_redirect_server(mock_port);
    wait_for_port(mock_port);

    let cdp_port = 18924;
    let child = Command::new(bin_path())
        .args(["cdp", &format!("127.0.0.1:{cdp_port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start kite-lite cdp");
    let _guard = ChildGuard(child);
    wait_for_port(cdp_port);

    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{cdp_port}/"))
        .expect("failed to connect CDP websocket");

    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({
                "id": 1,
                "method": "Page.navigate",
                "params": {"url": format!("http://127.0.0.1:{mock_port}/start")}
            })
            .to_string()
            .into(),
        ))
        .expect("failed to send first Page.navigate");
    let nav1 = read_json_message(&mut socket);
    assert!(
        nav1["result"].get("errorText").is_none(),
        "first navigate failed: {nav1:?}"
    );
    drain_events(&mut socket, 3);

    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({"id": 2, "method": "Runtime.evaluate", "params": {"expression": "document.title"}})
                .to_string()
                .into(),
        ))
        .expect("failed to send first Runtime.evaluate");
    let eval1 = read_json_message(&mut socket);
    assert_eq!(eval1["result"]["result"]["value"], "Cookie OK");

    // Second navigation, to a path that never sets the cookie itself — it
    // only comes back "Cookie OK" if the session's cookie jar carried the
    // cookie from the FIRST navigation over to this one.
    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({
                "id": 3,
                "method": "Page.navigate",
                "params": {"url": format!("http://127.0.0.1:{mock_port}/whoami")}
            })
            .to_string()
            .into(),
        ))
        .expect("failed to send second Page.navigate");
    let nav2 = read_json_message(&mut socket);
    assert!(
        nav2["result"].get("errorText").is_none(),
        "second navigate failed: {nav2:?}"
    );
    drain_events(&mut socket, 3);

    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({"id": 4, "method": "Runtime.evaluate", "params": {"expression": "document.title"}})
                .to_string()
                .into(),
        ))
        .expect("failed to send second Runtime.evaluate");
    let eval2 = read_json_message(&mut socket);
    assert_eq!(eval2["result"]["result"]["value"], "Cookie OK");
}
