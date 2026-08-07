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

#[test]
fn render_writes_png_and_pdf_files_by_output_extension() {
    let page = write_sample_page("render-page-raster.json");

    let png_path = temp_file("render-page.png");
    let _png_guard = TempPath(png_path.clone());
    let output = Command::new(bin_path())
        .args(["render", page.0.to_str().unwrap(), "--output", png_path.to_str().unwrap()])
        .output()
        .expect("failed to run kite-lite render (png)");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let png_bytes = std::fs::read(&png_path).expect("png output missing");
    assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");

    let pdf_path = temp_file("render-page.pdf");
    let _pdf_guard = TempPath(pdf_path.clone());
    let output = Command::new(bin_path())
        .args(["render", page.0.to_str().unwrap(), "--output", pdf_path.to_str().unwrap()])
        .output()
        .expect("failed to run kite-lite render (pdf)");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let pdf_bytes = std::fs::read(&pdf_path).expect("pdf output missing");
    assert_eq!(&pdf_bytes[..5], b"%PDF-");
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

/// Like `http_request`, but reads the response as raw bytes instead of
/// UTF-8 text, for endpoints that can return binary payloads (PNG/PDF).
fn http_request_bytes(port: u16, method: &str, path: &str, body: &str) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("failed to connect");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("failed to send request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("failed to read response");
    let separator = b"\r\n\r\n";
    let split_at = response
        .windows(separator.len())
        .position(|window| window == separator)
        .expect("malformed HTTP response");
    let headers = String::from_utf8_lossy(&response[..split_at]).into_owned();
    let status = headers.lines().next().unwrap_or_default().to_string();
    let payload = response[split_at + separator.len()..].to_vec();
    (status, payload)
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

    let (status, body) = http_request_bytes(port, "POST", "/v1/render?format=png", sample_page);
    assert!(status.contains("200"), "status: {status}");
    assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n");

    let (status, body) = http_request_bytes(port, "POST", "/v1/render?format=pdf", sample_page);
    assert!(status.contains("200"), "status: {status}");
    assert_eq!(&body[..5], b"%PDF-");

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

fn send_cdp(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({"id": id, "method": method, "params": params})
                .to_string()
                .into(),
        ))
        .unwrap_or_else(|_| panic!("failed to send {method}"));
    read_json_message(socket)
}

/// A minimal local HTTP server with a link and a form, used to verify that
/// clicking navigates and that submitting a form sends its field values —
/// without depending on the real internet.
fn spawn_interaction_server(port: u16) {
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
            let (route, query) = path.split_once('?').unwrap_or((path.as_str(), ""));

            let body = match route {
                "/" => {
                    r#"<title>Home</title><a href="/target">Go</a><form action="/search"><input name="q"><button>Search</button></form>"#
                        .to_string()
                }
                "/target" => "<title>Landed</title>".to_string(),
                "/search" => {
                    let q = url::form_urlencoded::parse(query.as_bytes())
                        .find(|(key, _)| key == "q")
                        .map(|(_, value)| value.into_owned())
                        .unwrap_or_default();
                    format!("<title>Results: {q}</title>")
                }
                _ => String::new(),
            };
            let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
}

#[test]
fn click_navigates_and_form_submit_sends_field_values() {
    let mock_port = 19021;
    spawn_interaction_server(mock_port);
    wait_for_port(mock_port);

    let cdp_port = 18925;
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

    let nav = send_cdp(
        &mut socket,
        1,
        "Page.navigate",
        serde_json::json!({"url": format!("http://127.0.0.1:{mock_port}/")}),
    );
    assert!(nav["result"].get("frameId").is_some(), "navigate failed: {nav:?}");
    drain_events(&mut socket, 3);

    // Click the <a href="/target"> by first asking where it is (DOM.getBoxModel,
    // the same way a real CDP client like Playwright would), then dispatching
    // a mouse event at a point inside its box.
    let select_link = send_cdp(&mut socket, 2, "DOM.querySelector", serde_json::json!({"selector": "a"}));
    let link_id = select_link["result"]["nodeId"].as_u64().unwrap();
    let box_model = send_cdp(&mut socket, 3, "DOM.getBoxModel", serde_json::json!({"nodeId": link_id}));
    let content = box_model["result"]["model"]["content"].as_array().unwrap();
    let link_y = content[1].as_f64().unwrap() + 0.5;

    let click = send_cdp(
        &mut socket,
        4,
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": 0, "y": link_y}),
    );
    assert!(
        click["result"].get("frameId").is_some(),
        "click on link should have navigated: {click:?}"
    );
    drain_events(&mut socket, 3);

    let title_after_click = send_cdp(
        &mut socket,
        5,
        "Runtime.evaluate",
        serde_json::json!({"expression": "document.title"}),
    );
    assert_eq!(title_after_click["result"]["result"]["value"], "Landed");

    // Navigate back to the form page, type into the input, then click the
    // submit button and confirm the field's value made it into the query
    // string the server received.
    let nav_back = send_cdp(
        &mut socket,
        6,
        "Page.navigate",
        serde_json::json!({"url": format!("http://127.0.0.1:{mock_port}/")}),
    );
    assert!(nav_back["result"].get("frameId").is_some(), "nav back failed: {nav_back:?}");
    drain_events(&mut socket, 3);

    let select_input = send_cdp(&mut socket, 7, "DOM.querySelector", serde_json::json!({"selector": "input"}));
    let input_id = select_input["result"]["nodeId"].as_u64().unwrap();
    let input_box = send_cdp(&mut socket, 8, "DOM.getBoxModel", serde_json::json!({"nodeId": input_id}));
    let input_content = input_box["result"]["model"]["content"].as_array().unwrap();
    let input_y = input_content[1].as_f64().unwrap() + 0.5;

    send_cdp(
        &mut socket,
        9,
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": 0, "y": input_y}),
    );
    send_cdp(
        &mut socket,
        10,
        "Input.dispatchKeyEvent",
        serde_json::json!({"type": "char", "text": "rust"}),
    );

    let select_button = send_cdp(&mut socket, 11, "DOM.querySelector", serde_json::json!({"selector": "button"}));
    let button_id = select_button["result"]["nodeId"].as_u64().unwrap();
    let button_box = send_cdp(&mut socket, 12, "DOM.getBoxModel", serde_json::json!({"nodeId": button_id}));
    let button_content = button_box["result"]["model"]["content"].as_array().unwrap();
    let button_y = button_content[1].as_f64().unwrap() + 0.5;

    let submit = send_cdp(
        &mut socket,
        13,
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": 0, "y": button_y}),
    );
    assert!(
        submit["result"].get("frameId").is_some(),
        "form submit should have navigated: {submit:?}"
    );
    drain_events(&mut socket, 3);

    let title_after_submit = send_cdp(
        &mut socket,
        14,
        "Runtime.evaluate",
        serde_json::json!({"expression": "document.title"}),
    );
    assert_eq!(title_after_submit["result"]["result"]["value"], "Results: rust");
}

#[test]
fn cdp_serves_json_discovery_endpoints() {
    let port = 18926;
    let child = Command::new(bin_path())
        .args(["cdp", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start kite-lite cdp");
    let _guard = ChildGuard(child);
    wait_for_port(port);

    let (status, body) = http_request(port, "GET", "/json/version", "");
    assert!(status.contains("200"), "status: {status}");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("invalid /json/version JSON");
    let ws_url = parsed["webSocketDebuggerUrl"]
        .as_str()
        .expect("missing webSocketDebuggerUrl");
    assert!(ws_url.starts_with("ws://"), "unexpected ws url: {ws_url}");

    let (status, body) = http_request(port, "GET", "/json/list", "");
    assert!(status.contains("200"), "status: {status}");
    let targets: serde_json::Value = serde_json::from_str(&body).expect("invalid /json/list JSON");
    assert_eq!(targets.as_array().unwrap().len(), 1);
}

#[test]
fn cdp_auto_attach_emits_session_and_sessions_get_echoed() {
    let port = 18927;
    let child = Command::new(bin_path())
        .args(["cdp", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start kite-lite cdp");
    let _guard = ChildGuard(child);
    wait_for_port(port);

    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/"))
        .expect("failed to connect CDP websocket");

    let ack = send_cdp(
        &mut socket,
        1,
        "Target.setAutoAttach",
        serde_json::json!({"autoAttach": true, "waitForDebuggerOnStart": false}),
    );
    assert_eq!(ack["id"], 1);

    let attached = read_json_message(&mut socket);
    assert_eq!(attached["method"], "Target.attachedToTarget");
    let session_id = attached["params"]["sessionId"]
        .as_str()
        .expect("missing sessionId")
        .to_string();
    assert!(!session_id.is_empty());

    // A command sent with that sessionId attached should get it echoed
    // back on the response, the way a real CDP client expects to route
    // page-level responses to the right session.
    socket
        .send(tungstenite::Message::Text(
            serde_json::json!({
                "id": 2,
                "sessionId": session_id,
                "method": "Runtime.evaluate",
                "params": {"expression": "1 + 1"}
            })
            .to_string()
            .into(),
        ))
        .expect("failed to send session-scoped Runtime.evaluate");
    let response = read_json_message(&mut socket);
    assert_eq!(response["sessionId"], session_id);
    assert_eq!(response["result"]["result"]["value"], "2");
}
