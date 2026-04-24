//! Phase 12.4 — bridges between `SessionMcpRuntime` (owned by `agent-mcp`)
//! and the agent's `ToolRegistry` (lives here).
//!
//! The runtime intentionally does NOT cache a catalog to avoid pulling in
//! the `agent-core` dep cycle. Instead this module rebuilds on demand.
//! Future `notifications/tools/list_changed` wiring will call
//! `SessionMcpRuntime::invalidate_catalog`, and any cache held by the
//! caller should refresh through these helpers.
use super::mcp_catalog::McpToolCatalog;
use super::tool_registry::ToolRegistry;
use agent_mcp::SessionMcpRuntime;
use std::sync::Arc;
/// Build a fresh `McpToolCatalog` from the runtime's currently connected
/// clients. Touches `last_used_at` so building the catalog counts as
/// activity for reap purposes.
pub async fn build_session_catalog(runtime: &SessionMcpRuntime) -> Arc<McpToolCatalog> {
    build_session_catalog_with_context(runtime, false).await
}
/// Phase 12.8 — context-aware catalog build. `context_passthrough=true`
/// tags every produced `McpTool` so its wire requests carry
/// `params._meta = { agent_id, session_id }`.
pub async fn build_session_catalog_with_context(
    runtime: &SessionMcpRuntime,
    context_passthrough: bool,
) -> Arc<McpToolCatalog> {
    runtime.touch();
    let clients: Vec<_> = runtime.clients().into_iter().map(|(_, c)| c).collect();
    Arc::new(McpToolCatalog::build_with_context(clients, context_passthrough).await)
}
/// Convenience: build the catalog and register every tool into `registry`.
/// Safe to call multiple times — re-registering replaces the previous
/// handler entry (DashMap::insert semantics).
pub async fn register_session_tools(runtime: &SessionMcpRuntime, registry: &ToolRegistry) {
    register_session_tools_with_context(runtime, registry, false).await
}
pub async fn register_session_tools_with_context(
    runtime: &SessionMcpRuntime,
    registry: &ToolRegistry,
    context_passthrough: bool,
) {
    let catalog = build_session_catalog_with_context(runtime, context_passthrough).await;
    catalog.register_into(registry);
}
/// Phase 12.8 — context-aware variant with per-server overrides.
/// `overrides.get(server_name)` wins over the global flag when set.
pub async fn register_session_tools_with_overrides(
    runtime: &SessionMcpRuntime,
    registry: &ToolRegistry,
    context_passthrough: bool,
    overrides: std::collections::HashMap<String, bool>,
) {
    runtime.touch();
    let clients: Vec<_> = runtime.clients().into_iter().map(|(_, c)| c).collect();
    let catalog = McpToolCatalog::build_with_overrides(clients, context_passthrough, overrides)
        .await
        .with_resource_cache(runtime.resource_cache())
        .with_resource_uri_allowlist(runtime.resource_uri_allowlist());
    catalog.register_into(registry);
}
