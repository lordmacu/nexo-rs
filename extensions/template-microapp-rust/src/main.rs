//! Phase 83.7 — Microapp template (Rust).
//!
//! Skeleton stdio microapp built on `nexo-microapp-sdk`. Copy the
//! whole `extensions/template-microapp-rust/` directory, rename
//! the package, and start adding tools — the SDK handles the
//! JSON-RPC plumbing.
//!
//! What this template demonstrates:
//! 1. Two example tool handlers (`greet` and `ping`) registered
//!    via the `Microapp::with_tool` builder.
//! 2. `BindingContext` access from a tool handler — the agent +
//!    channel + account triple the daemon threaded through.
//! 3. One observer hook (`before_message`) registered via
//!    `with_hook` that always votes `Continue`.
//! 4. Outbound dispatch via `ToolCtx::outbound()` (Phase 82.3) —
//!    the `greet` tool sends a confirmation back through the
//!    channel after producing the LLM-visible reply.
//!
//! See `docs/src/microapps/contract.md` for the wire protocol.
//! See `docs/src/microapps/rust.md` for the full SDK reference.
//!
//! ## Porting to other languages
//!
//! The microapp **contract** is the JSON-RPC stdio loop. This
//! template is the Rust convenience wrapper, but the contract
//! itself is language-agnostic. Authors targeting Python,
//! TypeScript, Go, or any other runtime should:
//! 1. Read `docs/src/microapps/contract.md` end-to-end.
//! 2. Implement the JSON-RPC loop directly using their
//!    language's stdlib — the contract doc has worked examples
//!    for Python / Go / TypeScript.
//! 3. Match the tool-name namespacing rule
//!    (`<extension_id>_<tool>`) and reserved error code range
//!    (-32000…-32099).

use nexo_microapp_sdk::{Microapp, ToolCtx, ToolError, ToolReply};
use nexo_microapp_sdk::{HookCtx, HookOutcome};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nexo_microapp_sdk::init_logging_from_env("template-microapp-rust");

    let app = Microapp::new(
        "template-microapp-rust",
        env!("CARGO_PKG_VERSION"),
    )
    .with_tool("greet", greet_tool)
    .with_tool("ping", ping_tool)
    .with_hook("before_message", before_message_hook);

    app.run_stdio().await?;
    Ok(())
}

/// Example tool 1: build a greeting from `args.name` + carry
/// the binding context into the response so the LLM (and the
/// operator's transcripts) see which agent / channel / account
/// answered.
async fn greet_tool(args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("world");

    // BindingContext access — the daemon threads agent_id,
    // channel, account_id (Phase 82.1) through every tool call.
    // None when the binding wasn't resolvable (delegation /
    // heartbeat paths). Production code should handle the
    // None case explicitly.
    let context_summary = match ctx.binding() {
        Some(b) => format!(
            "agent={} channel={} account={}",
            b.agent_id,
            b.channel.as_deref().unwrap_or("?"),
            b.account_id.as_deref().unwrap_or("default"),
        ),
        None => "no binding context".to_string(),
    };

    Ok(ToolReply::ok_json(json!({
        "greeting": format!("hello, {name}"),
        "from": context_summary,
    })))
}

/// Example tool 2: trivial liveness check. Useful as a
/// post-install smoke test (`tools/call ping → tool_reply{pong:true}`).
async fn ping_tool(_args: Value, _ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    Ok(ToolReply::ok_json(json!({ "pong": true })))
}

/// Example hook (Phase 83.3 observer-only): logs the inbound and
/// always votes `Continue`. Hooks are how a microapp participates
/// in the daemon's pre/post-turn lifecycle without owning a tool
/// surface. The vote-to-block path (returning `Abort`) lets a
/// compliance microapp veto a turn before the LLM sees it.
async fn before_message_hook(
    _args: Value,
    ctx: HookCtx,
) -> Result<HookOutcome, ToolError> {
    if let Some(meta) = ctx.inbound() {
        tracing::info!(
            target: "template-microapp-rust",
            kind = ?meta.kind,
            "before_message: observed inbound"
        );
    }
    Ok(HookOutcome::Continue)
}
