#![allow(clippy::all)] // Phase 79 scaffolding — re-enable when 79.x fully shipped

use super::context::AgentContext;
use async_trait::async_trait;
use dashmap::DashMap;
use nexo_llm::ToolDef;
use serde_json::Value;
use std::sync::Arc;

/// Phase 79.2 — per-tool metadata kept in a side-channel map so we
/// don't churn `ToolDef` (48 literal sites across the workspace).
/// Default values keep existing tools behaving identically.
///
/// Reference (PRIMARY): `claude-code-leak/src/tools/ToolSearchTool/`
/// + `prompt.ts:62-108` (`isDeferredTool` semantics: MCP tools
/// auto-deferred, others opt-in via `shouldDefer: true`).
#[derive(Debug, Clone, Default)]
pub struct ToolMeta {
    /// When `true`, the tool's full JSONSchema may be omitted from
    /// the LLM request body and surfaced as a stub instead. The
    /// model fetches the schema via `ToolSearch(select:<name>)` on
    /// demand. MVP caveat: provider shims do not yet honour the
    /// flag — only `ToolSearch` discovery consumes it for now.
    pub deferred: bool,
    /// Short capability phrase used by `ToolSearch` keyword
    /// ranking. `None` falls back to the tool's description (lower
    /// weight in scoring).
    pub search_hint: Option<String>,
}

impl ToolMeta {
    pub fn deferred() -> Self {
        Self {
            deferred: true,
            search_hint: None,
        }
    }

    pub fn deferred_with_hint(hint: impl Into<String>) -> Self {
        Self {
            deferred: true,
            search_hint: Some(hint.into()),
        }
    }

    pub fn with_search_hint(mut self, hint: impl Into<String>) -> Self {
        self.search_hint = Some(hint.into());
        self
    }
}
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value>;
}
/// `(schema, boxed handler)` pair keyed by tool name inside the registry.
pub type HandlerEntry = (ToolDef, Arc<dyn ToolHandler>);

#[derive(Default, Clone)]
pub struct ToolRegistry {
    handlers: Arc<DashMap<String, HandlerEntry>>,
    /// Phase 79.2 — per-tool metadata side-channel keyed by tool
    /// name. Empty for tools registered via the legacy `register`
    /// path; populated by `register_with_meta`. Reads via
    /// [`ToolRegistry::meta`] return `None` when no meta is recorded
    /// (effectively the same as `ToolMeta::default()`), keeping the
    /// addition byte-compatible for every existing call site.
    meta: Arc<DashMap<String, ToolMeta>>,
}
impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&self, def: ToolDef, handler: impl ToolHandler + 'static) {
        // Silent last-wins overwrites used to make it easy for an
        // extension reload to clobber a built-in tool without any
        // breadcrumb. Log the collision so operators can spot double
        // registrations in the startup stream.
        let name = def.name.clone();
        if self.handlers.contains_key(&name) {
            tracing::warn!(
                tool = %name,
                "tool registered twice — previous handler overwritten (use `register_if_absent` to preserve)"
            );
        }
        self.handlers.insert(name, (def, Arc::new(handler)));
    }

    /// Phase 79.M — register an already-boxed handler. Used by the
    /// MCP-server boot dispatcher which constructs handlers via
    /// `Arc<dyn ToolHandler>` so the `EXPOSABLE_TOOLS` match arm
    /// can return a uniform type per tool.
    pub fn register_arc(&self, def: ToolDef, handler: Arc<dyn ToolHandler>) {
        let name = def.name.clone();
        if self.handlers.contains_key(&name) {
            tracing::warn!(
                tool = %name,
                "tool registered twice — previous handler overwritten (use `register_if_absent` to preserve)"
            );
        }
        self.handlers.insert(name, (def, handler));
    }

    /// Phase 79.2 — register a tool together with its metadata
    /// (deferred flag, search hint). Useful for callers that mark
    /// MCP tools as deferred at import time, or built-ins that want
    /// to surface a curated `searchHint` for `ToolSearch` ranking.
    pub fn register_with_meta(
        &self,
        def: ToolDef,
        handler: impl ToolHandler + 'static,
        meta: ToolMeta,
    ) {
        let name = def.name.clone();
        self.register(def, handler);
        self.meta.insert(name, meta);
    }

    /// Phase 79.2 — set or replace the meta for an already-registered
    /// tool. No-op when the name is unknown — callers that want to
    /// see the failure should check `contains` first.
    pub fn set_meta(&self, tool_name: &str, meta: ToolMeta) {
        if self.handlers.contains_key(tool_name) {
            self.meta.insert(tool_name.to_string(), meta);
        }
    }

    /// Phase 79.2 — read the meta for `tool_name`. `None` for tools
    /// registered without meta (the default-meta semantic — not
    /// deferred, no search hint).
    pub fn meta(&self, tool_name: &str) -> Option<ToolMeta> {
        self.meta.get(tool_name).map(|e| e.value().clone())
    }

    /// Phase 79.2 — list every (name, meta) pair where
    /// `meta.deferred == true`. Empty when no tool is deferred.
    pub fn deferred_tools(&self) -> Vec<(String, ToolMeta)> {
        self.meta
            .iter()
            .filter(|e| e.value().deferred)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }
    /// Insert only if `def.name` is not already registered. Returns `true`
    /// when the handler was inserted, `false` when the slot was taken and
    /// left untouched. Lets callers (MCP catalog, extension bundlers)
    /// avoid overwriting tools that something else registered first.
    pub fn register_if_absent(&self, def: ToolDef, handler: impl ToolHandler + 'static) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.handlers.entry(def.name.clone()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert((def, Arc::new(handler)));
                true
            }
        }
    }

    /// Phase 81.3 — Arc-friendly variant of `register_if_absent`.
    /// Used by `ScopedToolRegistry` (the per-plugin proxy) so plugin
    /// callers go through a path that rejects collisions atomically
    /// without re-Arc-ing an already-boxed handler.
    pub fn register_if_absent_arc(&self, def: ToolDef, handler: Arc<dyn ToolHandler>) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.handlers.entry(def.name.clone()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert((def, handler));
                true
            }
        }
    }
    /// True if a handler is registered under `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }
    pub fn get(&self, name: &str) -> Option<(ToolDef, Arc<dyn ToolHandler>)> {
        self.handlers.get(name).map(|e| e.value().clone())
    }
    pub fn to_tool_defs(&self) -> Vec<ToolDef> {
        self.handlers.iter().map(|e| e.value().0.clone()).collect()
    }

    /// Phase 79.2 — same as [`to_tool_defs`] but skips tools marked
    /// [`ToolMeta::deferred`]. The filtered schemas are surfaced via
    /// the system prompt instead (see [`deferred_tools_summary`]) so
    /// the model still knows their names + descriptions and can load
    /// a full schema with `ToolSearch(select:<name>)`.
    pub fn to_tool_defs_non_deferred(&self) -> Vec<ToolDef> {
        self.handlers
            .iter()
            .filter(|e| {
                !self
                    .meta
                    .get(e.key())
                    .map(|m| m.deferred)
                    .unwrap_or(false)
            })
            .map(|e| e.value().0.clone())
            .collect()
    }

    /// Phase 79.2 — compact text block listing every deferred tool
    /// by name + description (capped at 120 chars). Returns `None`
    /// when no tool is deferred so callers can skip the block.
    pub fn deferred_tools_summary(&self) -> Option<String> {
        let deferred: Vec<(String, String)> = self
            .meta
            .iter()
            .filter(|e| e.value().deferred)
            .filter_map(|e| {
                let name = e.key().clone();
                let desc = self
                    .handlers
                    .get(&name)
                    .map(|h| h.0.description.clone())
                    .unwrap_or_default();
                Some((name, desc))
            })
            .collect();
        if deferred.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::with_capacity(deferred.len() + 2);
        parts.push(
            "<deferred-tools>\n\
             The following tools are available. Their full schemas are omitted to save tokens.\n\
             Use ToolSearch(select:<name>) to load one when needed.\n"
                .to_string(),
        );
        for (name, desc) in &deferred {
            let desc = if desc.len() > 120 {
                format!("{}…", &desc[..119])
            } else {
                desc.clone()
            };
            parts.push(format!("- {name}: {desc}"));
        }
        parts.push("</deferred-tools>".to_string());
        Some(parts.join("\n"))
    }
    /// All registered tool names — cheap because [`HandlerEntry`]
    /// already owns the name string. Phase 79.1 boot guard consumes
    /// this list to verify every tool is classified for plan-mode
    /// gating.
    pub fn names(&self) -> Vec<String> {
        self.handlers.iter().map(|e| e.key().clone()).collect()
    }
    /// Phase 79.1 — return tool names that are NOT classified for
    /// plan-mode gating. Empty result = every registered tool is
    /// recognised by `nexo_core::plan_mode::classify_tool`. Callers
    /// (main.rs boot path) typically `tracing::warn!` the slice and
    /// hard-fail under a feature flag.
    pub fn plan_mode_unclassified(&self) -> Vec<String> {
        crate::plan_mode::unclassified_tools(self.names())
    }
    /// Hard-fail companion to [`Self::plan_mode_unclassified`]. Panics
    /// with a descriptive message naming every unclassified tool.
    /// Wire into the boot path when you want plan-mode gating to be
    /// strict (production deployments where an unguarded mutator
    /// would be dangerous).
    pub fn assert_plan_mode_classified(&self) {
        crate::plan_mode::assert_registry_classified(self.names());
    }
    /// Drop every tool whose name does not match at least one of the
    /// glob patterns (trailing `*` or exact). Used by per-agent
    /// `allowed_tools`: register everything first, then prune down to
    /// the agent's whitelist so the LLM never sees disallowed tools.
    /// An empty `patterns` list leaves the registry untouched (the
    /// no-whitelist = "accept all" back-compat case). Returns the
    /// number of tools removed.
    pub fn retain_matching(&self, patterns: &[String]) -> usize {
        if patterns.is_empty() {
            return 0;
        }
        // Share the exact matching semantics with EffectiveBinding
        // Policy::tool_allowed so an agent-level allowlist and a
        // per-binding allowlist cannot drift apart (e.g. one honours
        // `"*"` but the other treats it as a literal name).
        let victims: Vec<String> = self
            .handlers
            .iter()
            .filter(|e| !super::effective::allowlist_matches(patterns, e.key()))
            .map(|e| e.key().clone())
            .collect();
        let n = victims.len();
        for k in victims {
            self.handlers.remove(&k);
            self.meta.remove(&k);
        }
        n
    }
    /// Build a fresh registry that shares the current handlers but drops
    /// every entry whose name does not match `allowed_tools`. Used by the
    /// per-binding tool registry cache: the base registry is assembled
    /// once at boot (plugins + extensions + MCP), and each binding gets
    /// its own filtered clone cheaply — handlers stay behind `Arc`, only
    /// the `DashMap` is fresh. An empty `allowed_tools` slice yields a
    /// full clone (back-compat with agents that don't narrow the set).
    pub fn filtered_clone(&self, allowed_tools: &[String]) -> ToolRegistry {
        let clone = ToolRegistry {
            handlers: Arc::new(DashMap::new()),
            meta: Arc::new(DashMap::new()),
        };
        for entry in self.handlers.iter() {
            clone
                .handlers
                .insert(entry.key().clone(), entry.value().clone());
        }
        for entry in self.meta.iter() {
            clone
                .meta
                .insert(entry.key().clone(), entry.value().clone());
        }
        clone.retain_matching(allowed_tools);
        clone
    }
    /// Phase 67.D.3 — drop every dispatch-related tool whose
    /// `DispatchKind` is not allowed by the binding's resolved
    /// `DispatchPolicy`. Returns the number of tools removed. Read
    /// tools survive when `mode >= ReadOnly`; write tools only when
    /// `mode == Full`; admin tools only when `is_admin`.
    ///
    /// Run AFTER all tools (built-ins, plugins, MCP, extensions) are
    /// registered. Idempotent — re-running removes nothing the
    /// second time.
    pub fn apply_dispatch_capability(
        &self,
        policy: &nexo_config::DispatchPolicy,
        is_admin: bool,
    ) -> usize {
        use nexo_dispatch_tools::tool_names::{
            should_register, ToolGroup, ADMIN_TOOL_NAMES, READ_TOOL_NAMES, WRITE_TOOL_NAMES,
        };
        let mut removed = 0usize;
        let drop_set: Vec<&'static str> = [
            (ToolGroup::Read, READ_TOOL_NAMES),
            (ToolGroup::Write, WRITE_TOOL_NAMES),
            (ToolGroup::Admin, ADMIN_TOOL_NAMES),
        ]
        .into_iter()
        .filter(|(group, _)| !should_register(policy, *group, is_admin))
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
        for name in drop_set {
            if self.handlers.remove(name).is_some() {
                removed += 1;
            }
        }
        removed
    }

    /// Phase 12.8 — remove every tool whose name starts with `prefix`.
    /// Used to drop a server's previous tool set before re-registering
    /// after a `notifications/tools/list_changed`. Returns the number of
    /// tools removed.
    pub fn clear_by_prefix(&self, prefix: &str) -> usize {
        let keys: Vec<String> = self
            .handlers
            .iter()
            .filter(|e| e.key().starts_with(prefix))
            .map(|e| e.key().clone())
            .collect();
        let n = keys.len();
        for k in keys {
            self.handlers.remove(&k);
        }
        n
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Noop;
    #[async_trait]
    impl ToolHandler for Noop {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            Ok(Value::Null)
        }
    }
    fn mk_def(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: "".into(),
            parameters: serde_json::json!({"type": "object" }),
        }
    }
    #[test]
    fn clear_by_prefix_removes_matching() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("mcp_srv_a"), Noop);
        reg.register(mk_def("mcp_srv_b"), Noop);
        reg.register(mk_def("memory_recall"), Noop);
        let n = reg.clear_by_prefix("mcp_srv_");
        assert_eq!(n, 2);
        assert!(reg.get("memory_recall").is_some());
        assert!(reg.get("mcp_srv_a").is_none());
    }
    struct TagHandler(&'static str);
    #[async_trait]
    impl ToolHandler for TagHandler {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            Ok(Value::String(self.0.into()))
        }
    }
    fn tagged_def(name: &str, desc: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: desc.into(),
            parameters: serde_json::json!({"type": "object" }),
        }
    }
    #[test]
    fn register_if_absent_preserves_original() {
        let reg = ToolRegistry::new();
        assert!(reg.register_if_absent(tagged_def("tool_a", "first"), TagHandler("first")));
        assert!(!reg.register_if_absent(tagged_def("tool_a", "second"), TagHandler("second")));
        let (def, _) = reg.get("tool_a").unwrap();
        assert_eq!(def.description, "first");
    }
    #[test]
    fn register_if_absent_accepts_empty_slot() {
        let reg = ToolRegistry::new();
        assert!(reg.register_if_absent(mk_def("fresh"), Noop));
        assert!(reg.contains("fresh"));
    }
    #[test]
    fn retain_matching_keeps_globs_and_exacts_drops_rest() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("memory_recall"), Noop);
        reg.register(mk_def("memory_save"), Noop);
        reg.register(mk_def("ext_weather_now"), Noop);
        reg.register(mk_def("ext_github_comment"), Noop);
        reg.register(mk_def("delegate"), Noop);
        assert_eq!(reg.to_tool_defs().len(), 5);
        // Keep memory_*, ext_weather_now (exact), and delegate; drop github.
        let removed = reg.retain_matching(&[
            "memory_*".into(),
            "ext_weather_now".into(),
            "delegate".into(),
        ]);
        assert_eq!(removed, 1);
        assert!(reg.contains("memory_recall"));
        assert!(reg.contains("memory_save"));
        assert!(reg.contains("ext_weather_now"));
        assert!(reg.contains("delegate"));
        assert!(!reg.contains("ext_github_comment"));
    }
    #[test]
    fn retain_matching_empty_patterns_is_noop() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("tool_a"), Noop);
        reg.register(mk_def("tool_b"), Noop);
        assert_eq!(reg.retain_matching(&[]), 0);
        assert_eq!(reg.to_tool_defs().len(), 2);
    }
    #[test]
    fn apply_dispatch_capability_none_drops_every_dispatch_tool() {
        let reg = ToolRegistry::new();
        for n in nexo_dispatch_tools::READ_TOOL_NAMES {
            reg.register(mk_def(n), Noop);
        }
        for n in nexo_dispatch_tools::WRITE_TOOL_NAMES {
            reg.register(mk_def(n), Noop);
        }
        for n in nexo_dispatch_tools::ADMIN_TOOL_NAMES {
            reg.register(mk_def(n), Noop);
        }
        // An unrelated tool must survive — only dispatch tools get pruned.
        reg.register(mk_def("memory_recall"), Noop);

        let policy = nexo_config::DispatchPolicy {
            mode: nexo_config::DispatchCapability::None,
            ..Default::default()
        };
        let removed = reg.apply_dispatch_capability(&policy, true);
        assert!(removed > 0);
        assert!(reg.contains("memory_recall"));
        assert!(!reg.contains("project_status"));
        assert!(!reg.contains("program_phase"));
        assert!(!reg.contains("set_concurrency_cap"));
    }

    #[test]
    fn apply_dispatch_capability_read_only_keeps_reads_drops_writes() {
        let reg = ToolRegistry::new();
        for n in nexo_dispatch_tools::READ_TOOL_NAMES {
            reg.register(mk_def(n), Noop);
        }
        for n in nexo_dispatch_tools::WRITE_TOOL_NAMES {
            reg.register(mk_def(n), Noop);
        }
        let policy = nexo_config::DispatchPolicy {
            mode: nexo_config::DispatchCapability::ReadOnly,
            ..Default::default()
        };
        reg.apply_dispatch_capability(&policy, false);
        assert!(reg.contains("project_status"));
        assert!(!reg.contains("program_phase"));
    }

    #[test]
    fn apply_dispatch_capability_full_admin_keeps_admin_tools() {
        let reg = ToolRegistry::new();
        for n in nexo_dispatch_tools::ADMIN_TOOL_NAMES {
            reg.register(mk_def(n), Noop);
        }
        let policy = nexo_config::DispatchPolicy {
            mode: nexo_config::DispatchCapability::Full,
            ..Default::default()
        };
        reg.apply_dispatch_capability(&policy, true);
        assert!(reg.contains("set_concurrency_cap"));
        // Same policy, non-admin caller — admin tools removed.
        reg.apply_dispatch_capability(&policy, false);
        assert!(!reg.contains("set_concurrency_cap"));
    }

    #[test]
    fn contains_reflects_state() {
        let reg = ToolRegistry::new();
        assert!(!reg.contains("missing"));
        reg.register(mk_def("present"), Noop);
        assert!(reg.contains("present"));
        assert!(!reg.contains("still-missing"));
    }

    #[test]
    fn plan_mode_unclassified_reports_unknown_names() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("FileRead"), Noop); // classified read-only
        reg.register(mk_def("totally_unknown_tool"), Noop); // unclassified
        let bad = reg.plan_mode_unclassified();
        assert_eq!(bad, vec!["totally_unknown_tool".to_string()]);
    }

    #[test]
    fn plan_mode_unclassified_empty_when_all_known() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("FileRead"), Noop);
        reg.register(mk_def("FileEdit"), Noop);
        reg.register(mk_def("EnterPlanMode"), Noop);
        reg.register(mk_def("whatsapp.send"), Noop); // dotted convention
        assert!(reg.plan_mode_unclassified().is_empty());
    }

    #[test]
    #[should_panic(expected = "plan_mode:")]
    fn assert_plan_mode_classified_panics_on_unknown() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("totally_unknown_tool"), Noop);
        reg.assert_plan_mode_classified();
    }

    #[test]
    fn to_tool_defs_non_deferred_skips_deferred() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("normal_a"), Noop);
        reg.register(mk_def("normal_b"), Noop);
        reg.register_with_meta(mk_def("deferred_x"), Noop, ToolMeta::deferred());
        let defs = reg.to_tool_defs_non_deferred();
        let mut names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["normal_a", "normal_b"]);
    }

    #[test]
    fn to_tool_defs_non_deferred_empty_when_all_deferred() {
        let reg = ToolRegistry::new();
        reg.register_with_meta(mk_def("a"), Noop, ToolMeta::deferred());
        reg.register_with_meta(mk_def("b"), Noop, ToolMeta::deferred());
        assert!(reg.to_tool_defs_non_deferred().is_empty());
    }

    #[test]
    fn deferred_tools_summary_returns_none_when_empty() {
        let reg = ToolRegistry::new();
        reg.register(mk_def("normal"), Noop);
        assert!(reg.deferred_tools_summary().is_none());
    }

    #[test]
    fn deferred_tools_summary_includes_deferred_names_and_descriptions() {
        let reg = ToolRegistry::new();
        reg.register_with_meta(
            tagged_def("mcp_x", "Create issues on GitHub"),
            Noop,
            ToolMeta::deferred(),
        );
        reg.register_with_meta(
            tagged_def("mcp_y", "Search Slack messages"),
            Noop,
            ToolMeta::deferred(),
        );
        // Normal tool should not appear.
        reg.register(tagged_def("normal", "Just a normal tool"), Noop);
        let summary = reg.deferred_tools_summary().unwrap();
        assert!(summary.contains("<deferred-tools>"));
        assert!(summary.contains("</deferred-tools>"));
        assert!(summary.contains("mcp_x"));
        assert!(summary.contains("Create issues on GitHub"));
        assert!(summary.contains("mcp_y"));
        assert!(summary.contains("Search Slack messages"));
        assert!(!summary.contains("normal"));
    }

    // ── Phase M8 — built-in deferred tools sweep ──

    #[test]
    fn mark_built_in_deferred_excludes_listed_tools() {
        use super::super::built_in_deferred::mark_built_in_deferred;
        let reg = ToolRegistry::new();
        // Register 3 tools that ARE in BUILT_IN_DEFERRED_TOOLS plus
        // one that is not.
        reg.register(mk_def("TodoWrite"), Noop);
        reg.register(mk_def("Lsp"), Noop);
        reg.register(mk_def("Repl"), Noop);
        reg.register(mk_def("FileRead"), Noop); // not in the list

        mark_built_in_deferred(&reg);

        let defs = reg.to_tool_defs_non_deferred();
        let mut names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["FileRead"]);
        // Three names show up in the deferred side-channel.
        let deferred: Vec<String> = reg
            .deferred_tools()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for expected in &["TodoWrite", "Lsp", "Repl"] {
            assert!(
                deferred.iter().any(|n| n == expected),
                "expected {expected} to appear in deferred_tools(): got {deferred:?}",
            );
        }
    }

    #[test]
    fn mark_built_in_deferred_skips_absent_tools() {
        use super::super::built_in_deferred::mark_built_in_deferred;
        let reg = ToolRegistry::new();
        // No tools registered. Sweep must not panic; it writes
        // side-channel meta even without handlers (acceptable —
        // `to_tool_defs_non_deferred` returns empty regardless
        // because there are no handlers).
        mark_built_in_deferred(&reg);
        assert!(reg.to_tool_defs_non_deferred().is_empty());
    }

    #[test]
    fn mark_built_in_deferred_propagates_search_hints() {
        use super::super::built_in_deferred::mark_built_in_deferred;
        let reg = ToolRegistry::new();
        reg.register(mk_def("TodoWrite"), Noop);
        mark_built_in_deferred(&reg);
        let meta = reg.meta("TodoWrite").expect("TodoWrite meta present");
        assert!(meta.deferred);
        assert_eq!(
            meta.search_hint.as_deref(),
            Some("todo, tasks, in-progress checklist"),
        );
    }
}
