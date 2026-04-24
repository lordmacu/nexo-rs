//! Ops-facing snapshot of every agent running in this process.
//!
//! Built once at boot from the parsed `AgentConfig` list and shared
//! with the admin HTTP server. Exposed at `GET /admin/agents` so
//! operators (or a future `agent status` CLI) can answer:
//! "who's running, what plugin are they bound to, what tools can
//! they call, who can they delegate to?" without having to grep
//! the yaml.
//!
//! Read-only — mutating an agent's config at runtime is a separate
//! feature (hot-reload), intentionally out of scope here.

use std::sync::Arc;

use agent_config::types::agents::{AgentConfig, InboundBinding};

/// Serialisable snapshot of one agent. Fields are the operator-relevant
/// bits of `AgentConfig` (skip secrets like tokens, skip heavy stuff
/// like `dreaming` weights).
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub id: String,
    pub description: String,
    pub model_provider: String,
    pub model_name: String,
    pub inbound_bindings: Vec<InboundBinding>,
    pub allowed_tools: Vec<String>,
    pub allowed_delegates: Vec<String>,
    pub extra_docs: Vec<String>,
    pub has_sender_rate_limit: bool,
    pub has_workspace: bool,
}

impl AgentInfo {
    pub fn from_config(cfg: &AgentConfig) -> Self {
        Self {
            id: cfg.id.clone(),
            description: cfg.description.clone(),
            model_provider: cfg.model.provider.clone(),
            model_name: cfg.model.model.clone(),
            inbound_bindings: cfg.inbound_bindings.clone(),
            allowed_tools: cfg.allowed_tools.clone(),
            allowed_delegates: cfg.allowed_delegates.clone(),
            extra_docs: cfg.extra_docs.clone(),
            has_sender_rate_limit: cfg.sender_rate_limit.is_some(),
            has_workspace: !cfg.workspace.trim().is_empty(),
        }
    }
}

pub struct AgentsDirectory {
    agents: Vec<AgentInfo>,
}

impl AgentsDirectory {
    pub fn new(agents: Vec<AgentInfo>) -> Arc<Self> {
        Arc::new(Self { agents })
    }

    /// Dispatch admin requests scoped to `/admin/agents*`. Returns
    /// `None` for routes this module doesn't own so the caller can
    /// fall through to the next handler.
    pub fn dispatch(&self, method: &str, path: &str) -> Option<(u16, String, &'static str)> {
        const JSON: &str = "application/json; charset=utf-8";
        match (method, path) {
            ("GET", "/admin/agents") => Some((200, self.render_list(), JSON)),
            ("GET", p) if p.starts_with("/admin/agents/") => {
                let id = &p["/admin/agents/".len()..];
                if id.is_empty() || id.contains('/') {
                    return Some((404, r#"{"error":"not found"}"#.to_string(), JSON));
                }
                match self.agents.iter().find(|a| a.id == id) {
                    Some(a) => Some((200, render_agent(a), JSON)),
                    None => Some((
                        404,
                        format!(r#"{{"error":"agent `{}` not found"}}"#, json_escape(id)),
                        JSON,
                    )),
                }
            }
            _ => None,
        }
    }

    fn render_list(&self) -> String {
        // Handwritten JSON keeps agent-core free of a serde_json
        // dependency for this module — the payload is tiny and the
        // field set is stable, so the cost of pulling in serde_json
        // isn't worth paying here.
        let mut out = String::from("[");
        for (i, a) in self.agents.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&render_agent(a));
        }
        out.push(']');
        out
    }
}

fn render_agent(a: &AgentInfo) -> String {
    let bindings = a
        .inbound_bindings
        .iter()
        .map(|b| match &b.instance {
            Some(inst) => format!(
                r#"{{"plugin":"{}","instance":"{}"}}"#,
                json_escape(&b.plugin),
                json_escape(inst)
            ),
            None => format!(r#"{{"plugin":"{}"}}"#, json_escape(&b.plugin)),
        })
        .collect::<Vec<_>>()
        .join(",");
    let allowed_tools = a
        .allowed_tools
        .iter()
        .map(|s| format!(r#""{}""#, json_escape(s)))
        .collect::<Vec<_>>()
        .join(",");
    let allowed_delegates = a
        .allowed_delegates
        .iter()
        .map(|s| format!(r#""{}""#, json_escape(s)))
        .collect::<Vec<_>>()
        .join(",");
    let extra_docs = a
        .extra_docs
        .iter()
        .map(|s| format!(r#""{}""#, json_escape(s)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"id":"{}","description":"{}","model":{{"provider":"{}","model":"{}"}},"inbound_bindings":[{}],"allowed_tools":[{}],"allowed_delegates":[{}],"extra_docs":[{}],"has_sender_rate_limit":{},"has_workspace":{}}}"#,
        json_escape(&a.id),
        json_escape(&a.description),
        json_escape(&a.model_provider),
        json_escape(&a.model_name),
        bindings,
        allowed_tools,
        allowed_delegates,
        extra_docs,
        a.has_sender_rate_limit,
        a.has_workspace,
    )
}

/// Minimal JSON string escaper: backslash, quote, control chars.
/// Enough for id/description/tool-name content we actually emit here;
/// callers who need full RFC 8259 compliance should use serde_json.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            description: format!("role for {id}"),
            model_provider: "minimax".into(),
            model_name: "m2.5".into(),
            inbound_bindings: vec![],
            allowed_tools: vec![],
            allowed_delegates: vec![],
            extra_docs: vec![],
            has_sender_rate_limit: false,
            has_workspace: false,
        }
    }

    #[test]
    fn list_route_renders_all_agents() {
        let dir = AgentsDirectory::new(vec![info("kate"), info("boss")]);
        let (status, body, ctype) = dir.dispatch("GET", "/admin/agents").unwrap();
        assert_eq!(status, 200);
        assert!(ctype.starts_with("application/json"));
        assert!(body.starts_with("["));
        assert!(body.ends_with("]"));
        assert!(body.contains(r#""id":"kate""#));
        assert!(body.contains(r#""id":"boss""#));
        assert!(body.contains(r#""description":"role for kate""#));
    }

    #[test]
    fn unknown_route_returns_none_for_fallthrough() {
        let dir = AgentsDirectory::new(vec![info("a")]);
        assert!(dir.dispatch("GET", "/admin/other").is_none());
        assert!(dir.dispatch("POST", "/admin/agents").is_none());
    }

    #[test]
    fn single_agent_route_returns_matching_or_404() {
        let dir = AgentsDirectory::new(vec![info("kate"), info("boss")]);

        let (status, body, _) = dir.dispatch("GET", "/admin/agents/kate").unwrap();
        assert_eq!(status, 200);
        assert!(body.contains(r#""id":"kate""#));
        assert!(
            !body.starts_with("["),
            "single-agent response is an object, not array"
        );

        let (status, body, _) = dir.dispatch("GET", "/admin/agents/nobody").unwrap();
        assert_eq!(status, 404);
        assert!(body.contains("not found"));

        // Empty or nested id → 404 (avoids matching `/admin/agents/foo/bar`).
        let (status, _, _) = dir.dispatch("GET", "/admin/agents/").unwrap();
        assert_eq!(status, 404);
        let (status, _, _) = dir.dispatch("GET", "/admin/agents/a/b").unwrap();
        assert_eq!(status, 404);
    }

    #[test]
    fn json_escape_handles_special_chars() {
        assert_eq!(json_escape("hello"), "hello");
        assert_eq!(json_escape("with \"quote\""), "with \\\"quote\\\"");
        assert_eq!(json_escape("line\nbreak"), "line\\nbreak");
        assert_eq!(json_escape("back\\slash"), "back\\\\slash");
    }

    #[test]
    fn bindings_render_with_optional_instance() {
        let mut a = info("k");
        a.inbound_bindings = vec![
            InboundBinding {
                plugin: "telegram".into(),
                instance: Some("sales".into()),
                ..Default::default()
            },
            InboundBinding {
                plugin: "whatsapp".into(),
                instance: None,
                ..Default::default()
            },
        ];
        let dir = AgentsDirectory::new(vec![a]);
        let (_, body, _) = dir.dispatch("GET", "/admin/agents").unwrap();
        assert!(body.contains(r#"{"plugin":"telegram","instance":"sales"}"#));
        assert!(body.contains(r#"{"plugin":"whatsapp"}"#));
    }
}
