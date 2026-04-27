//! Phase 12.3 — collect tools from N `StdioMcpClient`s and publish them in
//! the agent's `ToolRegistry`. Snapshot semantics: 12.4 will introduce
//! session-scoped refresh on `notifications/tools/list_changed`.
use super::mcp_tool::McpTool;
use super::tool_registry::ToolRegistry;
use futures::future::join_all;
use nexo_llm::ToolDef as LlmToolDef;
use nexo_mcp::{McpClient, ResourceCache};
use std::collections::HashSet;
use std::sync::Arc;
/// One tool exposed by a connected MCP server.
#[derive(Debug, Clone)]
pub struct McpCatalogEntry {
    pub server_name: String,
    pub tool_name: String,
    pub prefixed_name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}
/// Per-server build result — useful for operators diagnosing empty catalogs.
#[derive(Clone)]
pub struct McpServerSummary {
    pub server_name: String,
    pub client: Arc<dyn McpClient>,
    pub tool_count: usize,
    pub error: Option<String>,
    /// Phase 12.5 — `true` if the server advertised the `resources`
    /// capability during `initialize`. Used by `register_into` to decide
    /// whether to surface the 2 resource meta-tools.
    pub resources_capable: bool,
}
impl std::fmt::Debug for McpServerSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerSummary")
            .field("server_name", &self.server_name)
            .field("tool_count", &self.tool_count)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
pub struct McpToolCatalog {
    servers: Vec<McpServerSummary>,
    entries: Vec<McpCatalogEntry>,
    /// Phase 12.8 — propagate caller identity to every `tools/call`
    /// emitted by tools born from this catalog.
    context_passthrough: bool,
    /// Phase 12.8 — per-server override (keyed by server name). `None`
    /// in the map defers to `context_passthrough`.
    per_server_passthrough: std::collections::HashMap<String, bool>,
    /// Phase 12.5 follow-up — optional shared `resources/read` cache.
    /// When `Some`, every `McpResourceReadTool` built by `register_into`
    /// is wired to it.
    resource_cache: Option<Arc<ResourceCache>>,
    /// Phase 12.5 follow-up — optional URI scheme allowlist. Empty =
    /// permissive. Threaded into every `McpResourceReadTool`.
    resource_uri_allowlist: Arc<Vec<String>>,
}
impl McpToolCatalog {
    /// Build by calling `list_tools()` on every client in parallel. A
    /// single client failure does not abort the catalog: that server
    /// shows up in `servers()` with `error = Some(...)` and contributes
    /// zero entries.
    ///
    /// Collision policy: two entries with the same `prefixed_name` collapse
    /// to the first — the second is logged at `warn` and skipped.
    pub async fn build(clients: Vec<Arc<dyn McpClient>>) -> Self {
        Self::build_with_context(clients, false).await
    }
    /// Phase 12.8 — variant that records a global `context_passthrough`
    /// flag which `register_into` then propagates to every produced
    /// `McpTool`. Per-server overrides are empty.
    pub async fn build_with_context(
        clients: Vec<Arc<dyn McpClient>>,
        context_passthrough: bool,
    ) -> Self {
        Self::build_with_overrides(
            clients,
            context_passthrough,
            std::collections::HashMap::new(),
        )
        .await
    }
    /// Phase 12.8 — like `build_with_context` but with per-server
    /// overrides. `overrides.get(server_name)` wins over the global flag
    /// when present.
    pub async fn build_with_overrides(
        clients: Vec<Arc<dyn McpClient>>,
        context_passthrough: bool,
        overrides: std::collections::HashMap<String, bool>,
    ) -> Self {
        let mut c = Self::build_inner(clients).await;
        c.context_passthrough = context_passthrough;
        c.per_server_passthrough = overrides;
        c
    }
    /// Phase 12.5 follow-up — attach a shared `resources/read` cache.
    /// Returns self so callers can chain after `build_with_overrides`.
    pub fn with_resource_cache(mut self, cache: Arc<ResourceCache>) -> Self {
        self.resource_cache = Some(cache);
        self
    }
    pub fn with_resource_uri_allowlist(mut self, list: Arc<Vec<String>>) -> Self {
        self.resource_uri_allowlist = list;
        self
    }
    async fn build_inner(clients: Vec<Arc<dyn McpClient>>) -> Self {
        let futures: Vec<_> = clients
            .into_iter()
            .map(|c| async move {
                let name = c.name().to_string();
                let result = c.list_tools().await;
                (name, c, result)
            })
            .collect();
        let results = join_all(futures).await;
        let mut servers: Vec<McpServerSummary> = Vec::with_capacity(results.len());
        let mut entries: Vec<McpCatalogEntry> = Vec::new();
        let mut seen_prefixed: HashSet<String> = HashSet::new();
        for (server_name, client, list_result) in results {
            match list_result {
                Ok(tools) => {
                    let mut tool_count = 0usize;
                    for t in &tools {
                        let prefixed = McpTool::prefixed_name(&server_name, &t.name);
                        if !seen_prefixed.insert(prefixed.clone()) {
                            tracing::warn!(
                                mcp = %server_name,
                                tool = %t.name,
                                prefixed = %prefixed,
                                "skipping duplicate mcp tool"
                            );
                            continue;
                        }
                        entries.push(McpCatalogEntry {
                            server_name: server_name.clone(),
                            tool_name: t.name.clone(),
                            prefixed_name: prefixed,
                            description: t.description.clone(),
                            input_schema: t.input_schema.clone(),
                        });
                        tool_count += 1;
                    }
                    let resources_capable = client.capabilities().resources;
                    servers.push(McpServerSummary {
                        server_name,
                        client,
                        tool_count,
                        error: None,
                        resources_capable,
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    tracing::warn!(
                        mcp = %server_name,
                        error = %msg,
                        "mcp server list_tools failed; keeping zero entries"
                    );
                    servers.push(McpServerSummary {
                        server_name,
                        client,
                        tool_count: 0,
                        error: Some(msg),
                        resources_capable: false,
                    });
                }
            }
        }
        Self {
            servers,
            entries,
            context_passthrough: false,
            per_server_passthrough: std::collections::HashMap::new(),
            resource_cache: None,
            resource_uri_allowlist: Arc::new(Vec::new()),
        }
    }
    pub fn servers(&self) -> &[McpServerSummary] {
        &self.servers
    }
    pub fn entries(&self) -> &[McpCatalogEntry] {
        &self.entries
    }
    /// Register every entry into `registry` as an `McpTool` handler.
    ///
    /// - Entries whose name exceeds `LlmToolDef::MAX_NAME_LEN` are logged at `warn`
    ///   and skipped.
    /// - If the owning client cannot be located (impossible if the catalog
    ///   was produced by `build` but guarded defensively), the entry is
    ///   logged at `error` and skipped.
    pub fn register_into(&self, registry: &ToolRegistry) {
        let mut registered = 0usize;
        let mut skipped_collision = 0usize;
        for entry in &self.entries {
            if entry.prefixed_name.len() > LlmToolDef::MAX_NAME_LEN {
                tracing::warn!(
                    mcp = %entry.server_name,
                    tool = %entry.tool_name,
                    prefixed = %entry.prefixed_name,
                    limit = LlmToolDef::MAX_NAME_LEN,
                    "skipping mcp tool: name exceeds provider limit"
                );
                continue;
            }
            let client = match self
                .servers
                .iter()
                .find(|s| s.server_name == entry.server_name)
                .map(|s| s.client.clone())
            {
                Some(c) => c,
                None => {
                    tracing::error!(
                        mcp = %entry.server_name,
                        tool = %entry.tool_name,
                        "mcp catalog inconsistency: entry has no matching server"
                    );
                    continue;
                }
            };
            let desc = nexo_mcp::McpTool {
                name: entry.tool_name.clone(),
                description: entry.description.clone(),
                input_schema: entry.input_schema.clone(),
                output_schema: None,
            };
            let def = McpTool::tool_def(&desc, &entry.server_name);
            let effective_passthrough = self
                .per_server_passthrough
                .get(&entry.server_name)
                .copied()
                .unwrap_or(self.context_passthrough);
            let prefixed = def.name.clone();
            let description_for_hint = entry.description.clone().unwrap_or_default();
            let inserted = registry.register_if_absent(
                def,
                McpTool::new(&entry.server_name, &entry.tool_name, client)
                    .with_context_passthrough(effective_passthrough),
            );
            if inserted {
                // Phase 79.2 follow-up — MCP-imported tools are
                // auto-marked as deferred so `ToolSearch` can fetch
                // their schemas on demand. Search hint = first 80
                // chars of the description, lower-case (lift from
                // leak `ToolSearchTool/prompt.ts:62-68` —
                // "MCP tools are always deferred (workflow-specific)").
                let hint: String = description_for_hint
                    .chars()
                    .take(80)
                    .collect::<String>()
                    .to_ascii_lowercase();
                let meta = if hint.trim().is_empty() {
                    super::tool_registry::ToolMeta::deferred()
                } else {
                    super::tool_registry::ToolMeta::deferred_with_hint(hint)
                };
                registry.set_meta(&prefixed, meta);
                registered += 1;
            } else {
                skipped_collision += 1;
                tracing::warn!(
                    mcp = %entry.server_name,
                    tool = %entry.tool_name,
                    prefixed = %entry.prefixed_name,
                    "mcp tool skipped: name already registered"
                );
            }
        }
        if skipped_collision > 0 {
            tracing::info!(
                registered,
                skipped_collision,
                "mcp catalog registration summary"
            );
        }
        // Phase 12.5 — surface resource meta-tools for capable servers.
        for summary in &self.servers {
            if !summary.resources_capable {
                continue;
            }
            let effective_passthrough = self
                .per_server_passthrough
                .get(&summary.server_name)
                .copied()
                .unwrap_or(self.context_passthrough);
            register_resource_meta_tools(
                &summary.server_name,
                summary.client.clone(),
                registry,
                effective_passthrough,
                self.resource_cache.clone(),
                Arc::clone(&self.resource_uri_allowlist),
            );
        }
    }
}
fn register_resource_meta_tools(
    server_name: &str,
    client: Arc<dyn McpClient>,
    registry: &ToolRegistry,
    context_passthrough: bool,
    resource_cache: Option<Arc<ResourceCache>>,
    resource_uri_allowlist: Arc<Vec<String>>,
) {
    use super::mcp_resource_tool::{
        McpResourceListTemplatesTool, McpResourceListTool, McpResourceReadTool,
    };
    let list_def = McpResourceListTool::tool_def(server_name);
    let list_prefixed = list_def.name.clone();
    if !registry.register_if_absent(
        list_def,
        McpResourceListTool::new(server_name, client.clone())
            .with_context_passthrough(context_passthrough),
    ) {
        tracing::warn!(
            mcp = %server_name,
            prefixed = %list_prefixed,
            "mcp resource list meta-tool skipped: name already registered"
        );
    }
    let read_def = McpResourceReadTool::tool_def(server_name);
    let read_prefixed = read_def.name.clone();
    let mut read_tool = McpResourceReadTool::new(server_name, client.clone())
        .with_context_passthrough(context_passthrough)
        .with_uri_allowlist(resource_uri_allowlist);
    if let Some(cache) = resource_cache {
        read_tool = read_tool.with_cache(cache);
    }
    if !registry.register_if_absent(read_def, read_tool) {
        tracing::warn!(
            mcp = %server_name,
            prefixed = %read_prefixed,
            "mcp resource read meta-tool skipped: name already registered"
        );
    }
    let tpl_def = McpResourceListTemplatesTool::tool_def(server_name);
    let tpl_prefixed = tpl_def.name.clone();
    if !registry.register_if_absent(
        tpl_def,
        McpResourceListTemplatesTool::new(server_name, client),
    ) {
        tracing::warn!(
            mcp = %server_name,
            prefixed = %tpl_prefixed,
            "mcp resource templates meta-tool skipped: name already registered"
        );
    }
}
