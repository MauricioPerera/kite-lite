//! Black-box integration tests that spawn the real `kite-lite` binary.
//!
//! These exist because `env::current_exe()` inside a unit test points at the
//! test harness, not at `kite-lite` — so the child-process JS isolation used
//! by `eval`/`serve`/CDP's `Runtime.evaluate` can only be exercised here,
//! via `CARGO_BIN_EXE_kite-lite`, which Cargo only sets for `tests/`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
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

fn write_html(name: &str, content: &str) -> TempPath {
    let path = temp_file(name);
    std::fs::write(&path, content).expect("failed to write html file");
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
                "/webmcp" => r#"<title>WebMCP Home</title>
                    <form toolname="search-cars" tooldescription="Search for a car" action="/webmcp-search">
                        <input type="text" name="make" required toolparamdescription="The vehicle's make">
                        <input type="text" name="model">
                        <button type="submit">Search</button>
                    </form>"#
                    .to_string(),
                "/webmcp-search" => {
                    let params = url::form_urlencoded::parse(query.as_bytes())
                        .into_owned()
                        .collect::<std::collections::HashMap<_, _>>();
                    format!(
                        "<title>Results: {} {}</title>",
                        params.get("make").cloned().unwrap_or_default(),
                        params.get("model").cloned().unwrap_or_default()
                    )
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

/// Drives `kite-lite mcp` as a real subprocess over its stdio JSON-RPC
/// transport — the same way an MCP client (Claude Desktop, Claude Code)
/// would, one newline-delimited JSON object per message.
struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpClient {
    fn start() -> Self {
        let mut child = Command::new(bin_path())
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start kite-lite mcp");
        let stdin = child.stdin.take().expect("missing mcp stdin");
        let stdout = BufReader::new(child.stdout.take().expect("missing mcp stdout"));
        let mut client = Self { child, stdin, stdout };
        let init = client.send(
            0,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "kite-lite-test", "version": "0"}
            }),
        );
        assert_eq!(
            init["result"]["serverInfo"]["name"], "kite-lite",
            "unexpected initialize response: {init:?}"
        );
        client
    }

    fn send(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        let request = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{request}").expect("failed to write mcp request");
        self.stdin.flush().expect("failed to flush mcp stdin");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("failed to read mcp response");
        assert!(!line.is_empty(), "mcp server closed stdout unexpectedly");
        serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("invalid mcp response JSON: {error}\nline: {line}")
        })
    }

    fn call_tool(&mut self, id: u64, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.send(id, "tools/call", serde_json::json!({"name": name, "arguments": arguments}))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_initialize_and_list_tools() {
    let mut mcp = McpClient::start();
    let list = mcp.send(1, "tools/list", serde_json::json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools/list missing tools array");
    let names: Vec<&str> = tools.iter().filter_map(|tool| tool["name"].as_str()).collect();
    for expected in ["fetch_page", "render_screenshot", "eval_js", "browser_navigate", "browser_click", "browser_type", "browser_get_dom", "browser_screenshot", "browser_call_tool"] {
        assert!(names.contains(&expected), "missing tool '{expected}' in {names:?}");
    }
}

#[test]
fn mcp_fetch_page_returns_a_lightweight_summary() {
    let port = 19031;
    spawn_interaction_server(port);
    wait_for_port(port);

    let mut mcp = McpClient::start();
    let response = mcp.call_tool(2, "fetch_page", serde_json::json!({"url": format!("http://127.0.0.1:{port}/")}));
    assert_eq!(response["result"]["isError"], false, "{response:?}");
    let text = response["result"]["content"][0]["text"].as_str().expect("missing text content");
    let summary: serde_json::Value = serde_json::from_str(text).expect("fetch_page text is not JSON");
    assert_eq!(summary["title"], "Home");
    assert!(summary.get("root").is_none(), "summary should not include the full DOM tree");
}

#[test]
fn mcp_render_screenshot_returns_base64_png_image_content() {
    let port = 19033;
    spawn_interaction_server(port);
    wait_for_port(port);

    let mut mcp = McpClient::start();
    let response = mcp.call_tool(2, "render_screenshot", serde_json::json!({"url": format!("http://127.0.0.1:{port}/")}));
    let content = &response["result"]["content"][0];
    assert_eq!(content["type"], "image");
    assert_eq!(content["mimeType"], "image/png");
    let data = content["data"].as_str().expect("missing base64 image data");
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .expect("image data is not valid base64");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn mcp_eval_js_runs_against_an_isolated_snapshot() {
    let port = 19034;
    spawn_interaction_server(port);
    wait_for_port(port);

    let mut mcp = McpClient::start();
    let response = mcp.call_tool(
        2,
        "eval_js",
        serde_json::json!({"url": format!("http://127.0.0.1:{port}/"), "script": "document.title"}),
    );
    assert_eq!(response["result"]["isError"], false, "{response:?}");
    assert_eq!(response["result"]["content"][0]["text"], "Home");
}

#[test]
fn mcp_browser_session_navigates_clicks_and_submits_a_form() {
    let port = 19035;
    spawn_interaction_server(port);
    wait_for_port(port);

    let mut mcp = McpClient::start();

    let nav = mcp.call_tool(2, "browser_navigate", serde_json::json!({"url": format!("http://127.0.0.1:{port}/")}));
    assert_eq!(nav["result"]["isError"], false, "{nav:?}");

    let click = mcp.call_tool(3, "browser_click", serde_json::json!({"selector": "a"}));
    assert_eq!(click["result"]["isError"], false, "{click:?}");
    let click_summary: serde_json::Value =
        serde_json::from_str(click["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(click_summary["action"], "navigated");
    assert_eq!(click_summary["page"]["title"], "Landed");

    // Back to the form page: focus the input by selector while typing, then
    // click submit and confirm the field's value made it into the query
    // string the mock server received.
    mcp.call_tool(4, "browser_navigate", serde_json::json!({"url": format!("http://127.0.0.1:{port}/")}));
    let typed = mcp.call_tool(5, "browser_type", serde_json::json!({"selector": "input", "text": "rust"}));
    assert_eq!(typed["result"]["isError"], false, "{typed:?}");

    let submit = mcp.call_tool(6, "browser_click", serde_json::json!({"selector": "button"}));
    assert_eq!(submit["result"]["isError"], false, "{submit:?}");
    let submit_summary: serde_json::Value =
        serde_json::from_str(submit["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(submit_summary["page"]["title"], "Results: rust");
}

#[test]
fn mcp_browser_get_dom_and_unknown_selector_error() {
    let port = 19036;
    spawn_interaction_server(port);
    wait_for_port(port);

    let mut mcp = McpClient::start();
    mcp.call_tool(2, "browser_navigate", serde_json::json!({"url": format!("http://127.0.0.1:{port}/")}));

    let dom = mcp.call_tool(3, "browser_get_dom", serde_json::json!({"selector": "a"}));
    assert_eq!(dom["result"]["isError"], false, "{dom:?}");
    assert_eq!(
        dom["result"]["content"][0]["text"],
        format!("<a href=\"http://127.0.0.1:{port}/target\">Go</a>")
    );

    let missing = mcp.call_tool(4, "browser_click", serde_json::json!({"selector": "video"}));
    assert_eq!(missing["result"]["isError"], true, "{missing:?}");
}

#[test]
fn mcp_discovers_and_calls_a_declarative_webmcp_tool() {
    let port = 19037;
    spawn_interaction_server(port);
    wait_for_port(port);

    let mut mcp = McpClient::start();

    let nav = mcp.call_tool(2, "browser_navigate", serde_json::json!({"url": format!("http://127.0.0.1:{port}/webmcp")}));
    assert_eq!(nav["result"]["isError"], false, "{nav:?}");
    let nav_summary: serde_json::Value =
        serde_json::from_str(nav["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let tools = nav_summary["tools"].as_array().expect("missing discovered tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "search-cars");
    assert_eq!(tools[0]["description"], "Search for a car");
    assert_eq!(tools[0]["autosubmit"], false);
    assert_eq!(tools[0]["inputSchema"]["required"], serde_json::json!(["make"]));
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["make"]["description"],
        "The vehicle's make"
    );

    let call = mcp.call_tool(
        3,
        "browser_call_tool",
        serde_json::json!({"name": "search-cars", "arguments": {"make": "BMW", "model": "330i"}}),
    );
    assert_eq!(call["result"]["isError"], false, "{call:?}");
    let call_summary: serde_json::Value =
        serde_json::from_str(call["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(call_summary["title"], "Results: BMW 330i");

    let missing = mcp.call_tool(4, "browser_call_tool", serde_json::json!({"name": "no-such-tool"}));
    assert_eq!(missing["result"]["isError"], true, "{missing:?}");
}

#[test]
fn webmcp_lint_reports_errors_and_exits_nonzero_for_a_broken_local_file() {
    let html = write_html(
        "webmcp-broken.html",
        r#"<form toolname="go now">
             <input type="text">
             <select name="team"></select>
           </form>"#,
    );

    let output = Command::new(bin_path())
        .args(["webmcp-lint", html.0.to_str().unwrap()])
        .output()
        .expect("failed to run kite-lite webmcp-lint");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("falta tooldescription"), "{stdout}");
    assert!(stdout.contains("caracteres fuera de"), "{stdout}");
    assert!(stdout.contains("no tiene 'name'"), "{stdout}");
    assert!(stdout.contains("enum vacio"), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error(es)"), "{stderr}");
}

#[test]
fn webmcp_lint_exits_zero_for_a_well_formed_local_file() {
    let html = write_html(
        "webmcp-clean.html",
        r#"<form toolname="search-cars" tooldescription="Search for a car" action="/search">
             <input type="text" name="make" toolparamdescription="The make">
           </form>"#,
    );

    let output = Command::new(bin_path())
        .args(["webmcp-lint", html.0.to_str().unwrap()])
        .output()
        .expect("failed to run kite-lite webmcp-lint");

    assert!(output.status.success(), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Sin hallazgos"));
}

#[test]
fn webmcp_lint_json_output_is_parseable() {
    let html = write_html(
        "webmcp-json.html",
        r#"<form toolname="go" tooldescription="Go" action="/x"><input name="q"></form>"#,
    );

    let output = Command::new(bin_path())
        .args(["webmcp-lint", html.0.to_str().unwrap(), "--json"])
        .output()
        .expect("failed to run kite-lite webmcp-lint");

    assert!(output.status.success());
    let findings: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("--json output is not valid JSON");
    let findings = findings.as_array().expect("expected a JSON array");
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["severity"], "info");
    assert_eq!(findings[0]["tool"], "go");
}

#[test]
fn webmcp_lint_works_against_a_live_url() {
    let port = 19038;
    spawn_interaction_server(port);
    wait_for_port(port);

    let output = Command::new(bin_path())
        .args(["webmcp-lint", &format!("http://127.0.0.1:{port}/webmcp")])
        .output()
        .expect("failed to run kite-lite webmcp-lint");

    assert!(output.status.success(), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("model"), "{stdout}");
    assert!(stdout.contains("toolparamdescription"), "{stdout}");
}

#[test]
fn a11y_lint_reports_warnings_for_a_broken_local_file() {
    let html = write_html(
        "a11y-broken.html",
        r#"<html><body>
             <h1>Titulo</h1><h1>Otro</h1><h3>Salta un nivel</h3>
             <img src="foto.jpg">
             <a href="/x"></a>
           </body></html>"#,
    );

    let output = Command::new(bin_path())
        .args(["a11y-lint", html.0.to_str().unwrap()])
        .output()
        .expect("failed to run kite-lite a11y-lint");

    assert!(output.status.success(), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sin atributo 'alt'"), "{stdout}");
    assert!(stdout.contains("sin atributo 'lang'"), "{stdout}");
    assert!(stdout.contains("sin texto ni imagen"), "{stdout}");
    assert!(stdout.contains("2 elementos <h1>"), "{stdout}");
    assert!(stdout.contains("salto de nivel"), "{stdout}");
}

#[test]
fn a11y_lint_exits_zero_for_a_well_formed_local_file() {
    let html = write_html(
        "a11y-clean.html",
        r#"<html lang="es"><body>
             <h1>Titulo</h1><h2>Subtitulo</h2>
             <img src="foto.jpg" alt="una foto">
             <a href="/x">Ir a x</a>
           </body></html>"#,
    );

    let output = Command::new(bin_path())
        .args(["a11y-lint", html.0.to_str().unwrap()])
        .output()
        .expect("failed to run kite-lite a11y-lint");

    assert!(output.status.success(), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Sin hallazgos"));
}

#[test]
fn a11y_lint_json_output_is_parseable() {
    let html = write_html("a11y-json.html", r#"<html><body><img src="x.png"></body></html>"#);

    let output = Command::new(bin_path())
        .args(["a11y-lint", html.0.to_str().unwrap(), "--json"])
        .output()
        .expect("failed to run kite-lite a11y-lint");

    assert!(output.status.success());
    let findings: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("--json output is not valid JSON");
    let findings = findings.as_array().expect("expected a JSON array");
    assert_eq!(findings.len(), 2, "{findings:?}"); // missing alt + missing lang
    assert!(findings.iter().all(|f| f["severity"] == "warning"));
}

#[test]
fn a11y_lint_works_against_a_live_url() {
    let port = 19039;
    spawn_interaction_server(port);
    wait_for_port(port);

    let output = Command::new(bin_path())
        .args(["a11y-lint", &format!("http://127.0.0.1:{port}/")])
        .output()
        .expect("failed to run kite-lite a11y-lint");

    assert!(output.status.success(), "stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert!(String::from_utf8_lossy(&output.stdout).contains("lang"));
}
