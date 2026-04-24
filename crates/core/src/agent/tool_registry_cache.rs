//! Per-binding tool registry cache.
//!
//! A single agent can expose a different tool surface per inbound binding
//! (e.g. one whatsapp-only tool on the sales channel, the full catalogue
//! on a private telegram channel). Filtering the registry at every LLM
//! turn would waste work, so we cache one filtered [`ToolRegistry`] per
//! `(agent_id, binding_index)` tuple. The base registry owns all the
//! `Arc<dyn ToolHandler>` instances; each filtered clone is a fresh
//! `DashMap` over the same handlers, so the cache is cheap in memory
//! too.
//!
//! Invalidation: there is no hot reload today. Config changes require a
//! process restart; the cache is wiped implicitly when the process
//! restarts. A future `clear(agent_id)` helper can be added when we
//! support live reconfiguration (tracked in FOLLOWUPS.md).

use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use super::tool_registry::ToolRegistry;

/// Cache keyed by `(agent_id, binding_index)`. Clones are cheap — share
/// the same `Arc<DashMap>` — so callers can hold one instance per
/// runtime and pass it into every session without worrying about
/// synchronising setup.
#[derive(Clone, Default)]
pub struct ToolRegistryCache {
    entries: Arc<DashMap<(String, usize), Arc<ToolRegistry>>>,
}

impl ToolRegistryCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached registry for `(agent_id, binding_index)`,
    /// building it with `base.filtered_clone(allowed_tools)` on first
    /// access. The closure form avoids an eager clone for every cache
    /// hit.
    pub fn get_or_build(
        &self,
        agent_id: &str,
        binding_index: usize,
        base: &ToolRegistry,
        allowed_tools: &[String],
    ) -> Arc<ToolRegistry> {
        let key = (agent_id.to_string(), binding_index);
        // Atomic get-or-insert: two racing callers with the same key must
        // observe the same Arc. A plain `get` + `insert` split leaves a
        // TOCTOU window where the loser's Arc is orphaned (functionally
        // equivalent but wastes a filtered_clone and diverges from the
        // cached identity — breaks Arc::ptr_eq expectations).
        match self.entries.entry(key) {
            Entry::Occupied(e) => Arc::clone(e.get()),
            Entry::Vacant(slot) => {
                let filtered = Arc::new(base.filtered_clone(allowed_tools));
                slot.insert(Arc::clone(&filtered));
                filtered
            }
        }
    }

    /// Number of cached filtered registries. Exposed for tests and
    /// diagnostics.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_llm::ToolDef;
    use async_trait::async_trait;
    use serde_json::{json, Value};

    use crate::agent::{AgentContext, ToolHandler};

    struct NoopTool;

    #[async_trait]
    impl ToolHandler for NoopTool {
        async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
            Ok(json!({}))
        }
    }

    fn tool_def(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: String::new(),
            parameters: json!({"type": "object"}),
        }
    }

    fn base_registry() -> ToolRegistry {
        let r = ToolRegistry::new();
        r.register(tool_def("whatsapp_send_message"), NoopTool);
        r.register(tool_def("memory_write"), NoopTool);
        r.register(tool_def("memory_query"), NoopTool);
        r.register(tool_def("browser_open"), NoopTool);
        r
    }

    #[test]
    fn filtered_registry_reflects_allowlist() {
        let base = base_registry();
        let cache = ToolRegistryCache::new();
        let narrow = cache.get_or_build(
            "ana",
            0,
            &base,
            &["whatsapp_send_message".to_string()],
        );
        assert!(narrow.contains("whatsapp_send_message"));
        assert!(!narrow.contains("memory_write"));
        assert!(!narrow.contains("browser_open"));
    }

    #[test]
    fn wildcard_entry_keeps_everything() {
        let base = base_registry();
        let cache = ToolRegistryCache::new();
        let full = cache.get_or_build("ana", 1, &base, &["*".to_string()]);
        assert!(full.contains("whatsapp_send_message"));
        assert!(full.contains("memory_write"));
        assert!(full.contains("browser_open"));
    }

    #[test]
    fn empty_allowlist_keeps_everything() {
        // Back-compat: agents that don't set allowed_tools must see the
        // full surface.
        let base = base_registry();
        let cache = ToolRegistryCache::new();
        let full = cache.get_or_build("legacy", 0, &base, &[]);
        assert_eq!(full.to_tool_defs().len(), 4);
    }

    #[test]
    fn prefix_glob_matches() {
        let base = base_registry();
        let cache = ToolRegistryCache::new();
        let mem_only = cache.get_or_build("ana", 2, &base, &["memory_*".to_string()]);
        assert!(mem_only.contains("memory_write"));
        assert!(mem_only.contains("memory_query"));
        assert!(!mem_only.contains("whatsapp_send_message"));
    }

    #[test]
    fn repeated_get_is_cache_hit() {
        let base = base_registry();
        let cache = ToolRegistryCache::new();
        let a = cache.get_or_build("ana", 0, &base, &["*".to_string()]);
        let b = cache.get_or_build("ana", 0, &base, &["*".to_string()]);
        assert_eq!(cache.len(), 1);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn different_bindings_produce_independent_entries() {
        let base = base_registry();
        let cache = ToolRegistryCache::new();
        let wa = cache.get_or_build("ana", 0, &base, &["whatsapp_send_message".into()]);
        let tg = cache.get_or_build("ana", 1, &base, &["*".into()]);
        assert_eq!(cache.len(), 2);
        assert!(!Arc::ptr_eq(&wa, &tg));
        assert_eq!(wa.to_tool_defs().len(), 1);
        assert_eq!(tg.to_tool_defs().len(), 4);
    }

    #[test]
    fn filtered_clone_leaves_base_untouched() {
        let base = base_registry();
        let _narrow = base.filtered_clone(&["whatsapp_send_message".to_string()]);
        // Base keeps every tool — the filter only touched the clone.
        assert_eq!(base.to_tool_defs().len(), 4);
    }
}
