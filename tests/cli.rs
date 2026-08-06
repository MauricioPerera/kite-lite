//! Black-box integration tests that spawn the real `kite-lite` binary.
//!
//! These exist because `env::current_exe()` inside a unit test points at the
//! test harness, not at `kite-lite` — so the child-process JS isolation used
//! by `eval`/`serve`/CDP's `Runtime.evaluate` can only be exercised here,
//! via `CARGO_BIN_EXE_kite-lite`, which Cargo only sets for `tests/`.

use std::io::{Read, Write};
use std::net::TcpStream;
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
