use std::sync::Arc;
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::Value;
use agent_llm::ToolDef;
use super::context::AgentContext;
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value>;
}
#[derive(Default, Clone)]
pub struct ToolRegistry {
    handlers: Arc<DashMap<String, (ToolDef, Arc<dyn ToolHandler>)>>,
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
        let rules: Vec<(&str, bool)> = patterns
            .iter()
            .map(|p| match p.strip_suffix('*') {
                Some(stem) => (stem, true),
                None => (p.as_str(), false),
            })
            .collect();
        let victims: Vec<String> = self
            .handlers
            .iter()
            .filter(|e| {
                let name = e.key();
                !rules.iter().any(|(stem, wildcard)| {
                    if *wildcard {
                        name.starts_with(*stem)
                    } else {
                        name == *stem
                    }
                })
            })
            .map(|e| e.key().clone())
            .collect();
        let n = victims.len();
        for k in victims {
            self.handlers.remove(&k);
        }
        n
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
        ToolDef { name: name.to_string(),
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
        ToolDef { name: name.into(),
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
    fn contains_reflects_state() {
        let reg = ToolRegistry::new();
        assert!(!reg.contains("missing"));
        reg.register(mk_def("present"), Noop);
        assert!(reg.contains("present"));
        assert!(!reg.contains("still-missing"));
    }
}
