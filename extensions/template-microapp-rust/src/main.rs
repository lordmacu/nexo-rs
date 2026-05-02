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
    build_app().run_stdio().await?;
    Ok(())
}

/// Construct the configured `Microapp` builder. Public-in-crate
/// so tests / harnesses (`tests/admin_mock_test.rs`) reuse the
/// exact same tool wiring the production binary ships — no risk
/// of test-only builders drifting from the deployed shape.
pub(crate) fn build_app() -> Microapp {
    Microapp::new(
        "template-microapp-rust",
        env!("CARGO_PKG_VERSION"),
    )
    // Phase 83.8.8.a — opt into the admin RPC surface so
    // `whoami_tool` can call `nexo/admin/agents/get`. Tools that
    // don't touch the daemon admin layer can omit this.
    .with_admin()
    .with_tool("greet", greet_tool)
    .with_tool("ping", ping_tool)
    .with_tool("whoami", whoami_tool)
    .with_hook("before_message", before_message_hook)
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

/// Example tool 3 (Phase 83.8.8 admin RPC): query the daemon
/// for the agent's own row via `nexo/admin/agents/get` and
/// surface a compact view to the caller. Demonstrates the full
/// admin call cycle: pull the client off `ctx`, call by method
/// name, propagate the typed `AdminError` → `ToolError::Internal`
/// when the daemon refuses (capability not granted, agent
/// missing, etc.). Tests in `tests/admin_mock_test.rs` exercise
/// this path with `MockAdminRpc` so a developer can assert on
/// shape + canned responses without a daemon in the loop.
async fn whoami_tool(_args: Value, ctx: ToolCtx) -> Result<ToolReply, ToolError> {
    let agent_id = ctx
        .binding()
        .map(|b| b.agent_id.clone())
        .ok_or_else(|| ToolError::InvalidArguments(
            "binding context missing — whoami needs `_meta.nexo.binding.agent_id`".into(),
        ))?;
    let admin = ctx.admin().ok_or_else(|| {
        ToolError::Internal("admin client not configured (build with `with_admin`)".into())
    })?;
    let detail = admin
        .call_raw(
            "nexo/admin/agents/get",
            json!({ "agent_id": agent_id }),
        )
        .await
        .map_err(|e| ToolError::Internal(format!("admin/agents/get: {e}")))?;
    Ok(ToolReply::ok_json(json!({
        "queried_agent": agent_id,
        "detail": detail,
    })))
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

#[cfg(test)]
mod tests {
    //! Phase 83.15.b — reference tests for microapp authors.
    //!
    //! Demonstrates `MicroappTestHarness` + `MockAdminRpc` on the
    //! exact `build_app()` the production binary ships. Authors
    //! copying this template inherit the wiring; their tool
    //! tests run without a live daemon.
    use super::*;
    use nexo_microapp_sdk::admin::MockAdminRpc;
    use nexo_microapp_sdk::{MicroappTestHarness, MockBindingContext};
    use serde_json::json;

    /// Smoke test for the simplest tool — no admin, no binding.
    #[tokio::test]
    async fn ping_returns_pong() {
        let h = MicroappTestHarness::new(build_app());
        let out = h.call_tool("ping", json!({})).await.unwrap();
        assert_eq!(out["pong"], true);
    }

    /// `greet_tool` reads `ctx.binding()` so the test threads a
    /// `MockBindingContext` into the call.
    #[tokio::test]
    async fn greet_uses_binding_context_when_provided() {
        let binding = MockBindingContext::new()
            .with_agent("ana")
            .with_channel("whatsapp")
            .with_account("acme")
            .build();
        let h = MicroappTestHarness::new(build_app());
        let out = h
            .call_tool_with_binding("greet", json!({"name": "world"}), binding)
            .await
            .unwrap();
        assert_eq!(out["greeting"], "hello, world");
        let from = out["from"].as_str().expect("from string");
        assert!(from.contains("agent=ana"), "{from}");
        assert!(from.contains("channel=whatsapp"), "{from}");
        assert!(from.contains("account=acme"), "{from}");
    }

    /// `whoami_tool` calls `nexo/admin/agents/get`. The test
    /// stands up a `MockAdminRpc`, registers a canned response,
    /// hands the mock to the harness, and asserts both the
    /// tool's reply shape and that the admin call was issued
    /// with the expected params.
    #[tokio::test]
    async fn whoami_routes_admin_call_and_surfaces_response() {
        let mock = MockAdminRpc::new();
        mock.on(
            "nexo/admin/agents/get",
            json!({
                "id": "ana",
                "active": true,
                "model": { "provider": "minimax", "model": "M2" },
            }),
        );
        let binding = MockBindingContext::new().with_agent("ana").build();
        let h = MicroappTestHarness::new(build_app())
            .with_admin_mock(&mock)
            .await;

        let out = h
            .call_tool_with_binding("whoami", json!({}), binding)
            .await
            .unwrap();
        assert_eq!(out["queried_agent"], "ana");
        assert_eq!(out["detail"]["model"]["provider"], "minimax");

        // Mock recorded the request — assert on shape so a
        // refactor that drops the param accidentally fails the
        // test.
        let calls = mock.requests_for("nexo/admin/agents/get");
        assert_eq!(calls.len(), 1, "exactly one admin call fired");
        assert_eq!(calls[0].params["agent_id"], "ana");
    }

    /// When the daemon refuses the admin call (e.g. capability
    /// not granted, agent missing) the typed `AdminError`
    /// surfaces as `ToolError::Internal`. Confirms the tool's
    /// error-mapping path stays sane under daemon-side failure.
    #[tokio::test]
    async fn whoami_propagates_admin_error_as_tool_internal() {
        use nexo_microapp_sdk::AdminError;
        let mock = MockAdminRpc::new();
        mock.on_err(
            "nexo/admin/agents/get",
            AdminError::CapabilityNotGranted {
                capability: "agents_crud".into(),
                method: "nexo/admin/agents/get".into(),
            },
        );
        let binding = MockBindingContext::new().with_agent("ana").build();
        let h = MicroappTestHarness::new(build_app())
            .with_admin_mock(&mock)
            .await;
        let err = h
            .call_tool_with_binding("whoami", json!({}), binding)
            .await
            .unwrap_err();
        // Tool wraps the AdminError into a JSON-RPC error frame;
        // the harness reports it as `RpcError`.
        match err {
            nexo_microapp_sdk::MicroappTestError::RpcError(frame) => {
                let msg = frame["message"].as_str().unwrap_or_default();
                assert!(
                    msg.contains("capability_not_granted"),
                    "expected typed error in message; got {frame}"
                );
            }
            other => panic!("expected RpcError, got {other:?}"),
        }
    }

    /// Hook smoke test — observer-only hook never aborts.
    #[tokio::test]
    async fn before_message_observes_and_continues() {
        let h = MicroappTestHarness::new(build_app());
        let outcome = h
            .fire_hook("before_message", json!({ "body": "hi" }))
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }
}
