//! Standalone JavaScript evaluator, spawned as a child process by `kite-lite`.
//!
//! This is its own crate (`kite-lite-js/Cargo.toml`), and that Cargo.toml
//! does not list `reqwest`/`tokio` as dependencies — so it cannot make
//! network requests even if it wanted to, and no future change to this
//! file or to `kite-lite-core` can silently add that capability back: the
//! isolation is enforced by the dependency graph, not by a convention
//! someone has to remember to uphold. It reads a JSON `EvalRequest` from
//! stdin and writes a JSON `EvalResponse` to stdout.

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
