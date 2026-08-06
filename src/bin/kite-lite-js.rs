//! Standalone JavaScript evaluator, spawned as a child process by `kite-lite`.
//!
//! This binary intentionally does not depend on `reqwest`/`tokio`, so it
//! cannot make network requests even if it wanted to: the isolation is a
//! property of what's linked into the executable, not just of what the
//! code chooses to call. It reads a JSON `EvalRequest` from stdin and
//! writes a JSON `EvalResponse` to stdout.

use anyhow::Result;
use kite_lite_core::{EvalRequest, EvalResponse, JsRuntime};
use std::io::Read;

fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: EvalRequest = serde_json::from_str(&input)?;
    let response = match JsRuntime::default().evaluate_page(&request.page, &request.script) {
        Ok(value) => EvalResponse {
            value: Some(value),
            error: None,
        },
        Err(error) => EvalResponse {
            value: None,
            error: Some(error.to_string()),
        },
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
