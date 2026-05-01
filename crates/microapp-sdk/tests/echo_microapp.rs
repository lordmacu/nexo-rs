//! Integration test: build a `Microapp` with one `echo` tool,
//! drive it via `MicroappTestHarness`, assert the round-trip.
//!
//! Requires the `test-harness` feature (declared in
//! `Cargo.toml::dev-dependencies`).

#![cfg(feature = "test-harness")]

use nexo_microapp_sdk::{
    Microapp, MicroappTestHarness, ToolCtx, ToolError, ToolReply,
};
use serde_json::json;

async fn echo(args: serde_json::Value, _ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    Ok(ToolReply::ok_json(json!({ "echoed": args })))
}

#[tokio::test]
async fn echo_microapp_round_trip() {
    let app = Microapp::new("echo-test", "0.0.0").with_tool("echo", echo);
    let harness = MicroappTestHarness::new(app);
    let out = harness
        .call_tool("echo", json!({ "ping": "pong" }))
        .await
        .expect("call ok");
    assert_eq!(out["echoed"]["ping"], "pong");
}
