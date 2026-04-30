#![allow(clippy::all)] // Phase 79 scaffolding — re-enable when 79.x fully shipped

//! Phase 79.2 — `ToolSearch` deferred-schema discovery tool.
//!
//! Lets the model fetch the full JSONSchema for tools whose schema is
//! omitted from the system prompt. The model sees the deferred tool
//! by name in the catalogue stub, then calls
//! `ToolSearch(select:Foo)` (or a keyword query) to retrieve the
//! schema; the matching tool becomes invokable in the next turn.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/ToolSearchTool/ToolSearchTool.ts:21-302`
//!     (input schema, select: prefix, keyword search with required
//!     `+token` prefix, scoring weights for name parts vs description
//!     vs searchHint).
//!   * `claude-code-leak/src/tools/ToolSearchTool/prompt.ts:27-51`
//!     (the canonical "Result format: <functions>" wording).
//!
//! Reference (secondary):
//!   * OpenClaw — no equivalent (`grep -rln "ToolSearch" research/src/`
//!     returns nothing relevant). Single-process TS reference does
//!     not face the wide-surface MCP token cost that motivates this
//!     tool.
//!
//! MVP scope (Phase 79.2):
//!   * Discovery surface — `ToolSearch` returns matching tool names
//!     PLUS their full schemas, ready to consume.
//!   * Per-turn rate limit (default 5) so a runaway model can't
//!     pathologically explode the surface.
//!   * Out of scope: filtering deferred tools out of the LLM request
//!     body. The LLM provider shims (anthropic, minimax, gemini,
//!     openai-compat) still emit the full schema today; the savings
//!     land when a follow-up wires the four shims to consult
//!     `ToolRegistry::deferred_tools()`. See FOLLOWUPS.md Phase 79.2.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use dashmap::DashMap;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const TOOL_SEARCH_DEFAULT_MAX_RESULTS: usize = 5;
pub const TOOL_SEARCH_HARD_CAP: usize = 25;

/// Default cap on `ToolSearch` calls per agent per minute. Lift
/// from the leak's spec (5 / turn). We use per-minute instead of
/// per-turn because nexo-rs's runtime does not surface a clean
/// turn boundary to the tool layer.
pub const TOOL_SEARCH_DEFAULT_RATE_PER_MINUTE: u32 = 5;

/// Process-shared sliding-window rate limiter for `ToolSearch`.
/// Keyed by `agent_id`. Used to cap exploration so a runaway
/// model can't pathologically explode the surface (PHASES.md
/// 79.2 follow-up).
#[derive(Default)]
pub struct ToolSearchRateLimiter {
    buckets: DashMap<String, Mutex<std::collections::VecDeque<Instant>>>,
}

impl ToolSearchRateLimiter {
    /// `true` when the call is admitted. `limit_per_minute == 0`
    /// always admits (no limit).
    pub fn try_acquire(&self, agent_id: &str, limit_per_minute: u32) -> bool {
        if limit_per_minute == 0 {
            return true;
        }
        let entry = self.buckets.entry(agent_id.to_string()).or_default();
        let mut q = entry.lock().unwrap();
        let now = Instant::now();
        let cutoff = now - Duration::from_secs(60);
        while let Some(front) = q.front() {
            if *front < cutoff {
                q.pop_front();
            } else {
                break;
            }
        }
        if q.len() < limit_per_minute as usize {
            q.push_back(now);
            true
        } else {
            false
        }
    }
}

pub struct ToolSearchTool {
    rate_limiter: std::sync::Arc<ToolSearchRateLimiter>,
    rate_per_minute: u32,
}

impl Default for ToolSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolSearchTool {
    pub fn new() -> Self {
        Self {
            rate_limiter: std::sync::Arc::new(ToolSearchRateLimiter::default()),
            rate_per_minute: TOOL_SEARCH_DEFAULT_RATE_PER_MINUTE,
        }
    }

    /// Override the rate limit (test convenience). `0` disables.
    pub fn with_rate_per_minute(mut self, rate: u32) -> Self {
        self.rate_per_minute = rate;
        self
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "ToolSearch".to_string(),
            description: "Fetches full schema definitions for deferred tools so they can be called. Deferred tools appear by name in the catalogue stub. Until fetched, only the name is known — there is no parameter schema, so the tool cannot be invoked. This tool takes a query, matches it against the deferred tool list, and returns the matched tools' complete JSONSchema definitions. Once a tool's schema appears in the result, it is callable like any tool defined at the top of the prompt.\n\nQuery forms:\n- \"select:Read,Edit,Grep\" — fetch these exact tools by name (comma-separated)\n- \"notebook jupyter\" — keyword search, up to max_results best matches\n- \"+slack send\" — require \"slack\" in the name, rank by remaining terms".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": TOOL_SEARCH_HARD_CAP,
                        "description": "Maximum number of results to return (default 5)."
                    }
                },
                "required": ["query"]
            }),
        }
    }
}

/// Split a tool name into searchable lowercase parts. Lift from
/// `ToolSearchTool.ts:132-161` — handles both MCP convention
/// (`mcp__server__action`) and CamelCase / snake_case.
fn parse_tool_name(name: &str) -> Vec<String> {
    if let Some(stripped) = name.strip_prefix("mcp__") {
        return stripped
            .to_ascii_lowercase()
            .split("__")
            .flat_map(|p| p.split('_').map(str::to_string).collect::<Vec<_>>())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '.' {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
        } else if i > 0 && c.is_ascii_uppercase() && !chars[i - 1].is_ascii_uppercase() {
            // CamelCase split: `FileEdit` → `file edit`.
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current).to_ascii_lowercase());
            }
            current.push(c);
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        parts.push(current.to_ascii_lowercase());
    }
    parts
}

/// Score one tool against the query terms. Mirror of the weights in
/// `ToolSearchTool.ts:266-291` adapted to MCP-aware dotted names.
fn score_tool(
    name: &str,
    description: &str,
    search_hint: Option<&str>,
    required: &[&str],
    optional: &[&str],
) -> Option<u32> {
    let parts = parse_tool_name(name);
    let is_mcp = name.starts_with("mcp__");
    let desc_lower = description.to_ascii_lowercase();
    let hint_lower = search_hint
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    // Required terms must match SOMETHING (name part / desc / hint),
    // otherwise the tool is filtered out entirely.
    for term in required {
        let lower = term.to_ascii_lowercase();
        let in_parts = parts.iter().any(|p| p == &lower || p.contains(&lower));
        let in_desc = desc_lower.contains(&lower);
        let in_hint = !hint_lower.is_empty() && hint_lower.contains(&lower);
        if !(in_parts || in_desc || in_hint) {
            return None;
        }
    }

    let mut score: u32 = 0;
    for term in required.iter().chain(optional.iter()) {
        let lower = term.to_ascii_lowercase();
        let exact_part = parts.iter().any(|p| p == &lower);
        let partial_part = parts.iter().any(|p| p != &lower && p.contains(&lower));

        if exact_part {
            score += if is_mcp { 12 } else { 10 };
        } else if partial_part {
            score += if is_mcp { 6 } else { 5 };
        }

        if !hint_lower.is_empty() && hint_lower.contains(&lower) {
            score += 4;
        }
        if desc_lower.contains(&lower) {
            score += 2;
        }
    }

    if score == 0 {
        None
    } else {
        Some(score)
    }
}

#[async_trait]
impl ToolHandler for ToolSearchTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ToolSearch requires `query` (string)"))?
            .trim()
            .to_string();
        if query.is_empty() {
            return Err(anyhow::anyhow!("ToolSearch: `query` cannot be empty"));
        }

        // Phase 79.2 follow-up — sliding-window rate limit per
        // agent. Cap defaults to 5 / minute (matches the leak's
        // 5 / turn intent — we use minutes because there is no
        // clean turn boundary at the tool layer). Per-instance
        // limiter so each tests construct its own.
        if !self
            .rate_limiter
            .try_acquire(&ctx.agent_id, self.rate_per_minute)
        {
            return Err(anyhow::anyhow!(
                "ToolSearch: rate limit exceeded ({} calls/min for agent `{}`). Wait or refine the query so fewer calls are needed.",
                self.rate_per_minute,
                ctx.agent_id
            ));
        }

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(TOOL_SEARCH_HARD_CAP).max(1))
            .unwrap_or(TOOL_SEARCH_DEFAULT_MAX_RESULTS);

        // Source registry: prefer the binding's effective tool set
        // (post per-binding allowlist filter) so the query never
        // surfaces a tool the binding cannot actually call.
        let registry = ctx.effective_tools.as_deref().map(|r| r.clone());
        let registry = registry.as_ref();

        let deferred = match registry {
            Some(r) => r.deferred_tools(),
            None => Vec::new(),
        };
        let total_deferred = deferred.len();

        // `select:Foo,Bar` — exact match path. Lift from
        // `ToolSearchTool.ts:362-406`. We look up against the
        // FULL registry so that selecting an already-loaded tool is
        // a harmless no-op; the result's "matched" array reports
        // actual hits.
        if let Some(rest) = query
            .strip_prefix("select:")
            .or_else(|| query.strip_prefix("SELECT:"))
        {
            let mut found: Vec<(String, ToolDef)> = Vec::new();
            let mut missing: Vec<String> = Vec::new();
            for raw in rest.split(',') {
                let name = raw.trim();
                if name.is_empty() {
                    continue;
                }
                let hit = registry
                    .and_then(|r| r.get(name))
                    .map(|(def, _)| (def.name.clone(), def));
                match hit {
                    Some(pair) => {
                        if !found.iter().any(|(n, _)| n == &pair.0) {
                            found.push(pair);
                        }
                    }
                    None => missing.push(name.to_string()),
                }
            }

            return Ok(json!({
                "query": query,
                "query_kind": "select",
                "total_deferred_tools": total_deferred,
                "matches": found
                    .iter()
                    .map(|(name, def)| json!({
                        "name": name,
                        "description": def.description,
                        "parameters": def.parameters,
                    }))
                    .collect::<Vec<_>>(),
                "missing": missing,
            }));
        }

        // Keyword path. Split by whitespace; `+token` flags required.
        let tokens: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
        let mut required: Vec<&str> = Vec::new();
        let mut optional: Vec<&str> = Vec::new();
        for t in tokens {
            if let Some(rest) = t.strip_prefix('+') {
                if !rest.is_empty() {
                    required.push(rest);
                }
            } else {
                optional.push(t);
            }
        }
        let scoring_terms: Vec<&str> = if required.is_empty() {
            optional.clone()
        } else {
            required.iter().chain(optional.iter()).copied().collect()
        };
        if scoring_terms.is_empty() {
            return Err(anyhow::anyhow!(
                "ToolSearch: query has no scorable terms (only delimiters?)"
            ));
        }

        // Score each deferred tool. We score against the registered
        // (def.description, meta.search_hint) pair pulled from the
        // registry on the fly.
        let mut scored: Vec<(String, u32, ToolDef)> = Vec::new();
        if let Some(reg) = registry {
            for (name, meta) in deferred.iter() {
                let Some((def, _handler)) = reg.get(name) else {
                    continue;
                };
                if let Some(s) = score_tool(
                    name,
                    &def.description,
                    meta.search_hint.as_deref(),
                    &required,
                    &optional,
                ) {
                    scored.push((name.clone(), s, def));
                }
            }
        }
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(max_results);

        Ok(json!({
            "query": query,
            "query_kind": "keyword",
            "total_deferred_tools": total_deferred,
            "matches": scored
                .iter()
                .map(|(name, score, def)| json!({
                    "name": name,
                    "score": score,
                    "description": def.description,
                    "parameters": def.parameters,
                }))
                .collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool_registry::{ToolMeta, ToolRegistry};
    use crate::session::SessionManager;
    use async_trait::async_trait;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;

    struct Stub;
    #[async_trait]
    impl ToolHandler for Stub {
        async fn call(&self, _: &AgentContext, _: Value) -> anyhow::Result<Value> {
            Ok(json!(null))
        }
    }

    fn def(name: &str, description: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: description.into(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }

    fn ctx_with(reg: ToolRegistry) -> AgentContext {
        let cfg = AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        repl: Default::default(),
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
        .with_effective_tools(Arc::new(reg))
    }

    fn reg_with_deferred(items: &[(&str, &str, Option<&str>)]) -> ToolRegistry {
        let r = ToolRegistry::new();
        for (name, desc, hint) in items {
            let mut meta = ToolMeta::deferred();
            if let Some(h) = hint {
                meta = meta.with_search_hint(*h);
            }
            r.register_with_meta(def(name, desc), Stub, meta);
        }
        r
    }

    #[test]
    fn parse_tool_name_camel_and_underscore_and_mcp() {
        assert_eq!(parse_tool_name("FileEdit"), vec!["file", "edit"]);
        assert_eq!(
            parse_tool_name("send_user_file"),
            vec!["send", "user", "file"]
        );
        assert_eq!(
            parse_tool_name("mcp__slack__send_message"),
            vec!["slack", "send", "message"]
        );
        assert_eq!(parse_tool_name("whatsapp.send"), vec!["whatsapp", "send"]);
    }

    #[tokio::test]
    async fn select_returns_matching_tool_with_schema() {
        let reg = reg_with_deferred(&[
            ("FileEdit", "Edit a file", Some("edit a file")),
            ("Glob", "Match paths by glob", None),
        ]);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "select:FileEdit"}))
            .await
            .unwrap();
        assert_eq!(res["query_kind"], "select");
        assert_eq!(res["matches"].as_array().unwrap().len(), 1);
        assert_eq!(res["matches"][0]["name"], "FileEdit");
        assert!(res["matches"][0]["parameters"].is_object());
    }

    #[tokio::test]
    async fn select_multi_with_some_missing_reports_missing() {
        let reg = reg_with_deferred(&[("FileEdit", "Edit a file", None)]);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "select:FileEdit,Imaginary,Glob"}))
            .await
            .unwrap();
        assert_eq!(res["matches"].as_array().unwrap().len(), 1);
        let missing: Vec<String> = res["missing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(missing.contains(&"Imaginary".to_string()));
        assert!(missing.contains(&"Glob".to_string()));
    }

    #[tokio::test]
    async fn keyword_match_ranks_by_score() {
        let reg = reg_with_deferred(&[
            ("FileEdit", "Edit a file", None),
            ("FileRead", "Read a file", None),
            ("Glob", "Match by glob", None),
        ]);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "file edit"}))
            .await
            .unwrap();
        assert_eq!(res["query_kind"], "keyword");
        let names: Vec<&str> = res["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        // FileEdit hits both terms exactly → highest.
        assert_eq!(names[0], "FileEdit");
    }

    #[tokio::test]
    async fn keyword_required_token_filters_out_non_matches() {
        let reg = reg_with_deferred(&[
            ("FileEdit", "Edit a file", None),
            ("Glob", "Match by glob", None),
        ]);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "+glob match"}))
            .await
            .unwrap();
        let names: Vec<&str> = res["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Glob"]);
    }

    #[tokio::test]
    async fn keyword_max_results_caps_output() {
        let many: Vec<(&str, &str, Option<&str>)> = (0..10)
            .map(|i| Box::leak(format!("Tool{i}").into_boxed_str()) as &str)
            .map(|name| (name, "shared description token", None))
            .collect();
        let reg = reg_with_deferred(&many);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(
                &ctx,
                json!({"query": "shared description", "max_results": 3}),
            )
            .await
            .unwrap();
        assert_eq!(res["matches"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn keyword_no_deferred_tools_returns_empty() {
        let reg = ToolRegistry::new();
        // Register a non-deferred tool — must be ignored.
        reg.register(def("FileEdit", "edit"), Stub);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "file"}))
            .await
            .unwrap();
        assert_eq!(res["total_deferred_tools"], 0);
        assert_eq!(res["matches"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn empty_query_errors() {
        let reg = ToolRegistry::new();
        let ctx = ctx_with(reg);
        let err = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "   "}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be empty"), "got: {err}");
    }

    #[tokio::test]
    async fn search_hint_outranks_description_only_match() {
        let reg = reg_with_deferred(&[
            ("ToolA", "this description mentions slack indirectly", None),
            ("ToolB", "totally unrelated", Some("send a slack message")),
        ]);
        let ctx = ctx_with(reg);
        let res = ToolSearchTool::new()
            .with_rate_per_minute(0)
            .call(&ctx, json!({"query": "slack"}))
            .await
            .unwrap();
        let names: Vec<&str> = res["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names[0], "ToolB", "search_hint should beat description");
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_budget() {
        let reg = reg_with_deferred(&[("FileEdit", "Edit a file", None)]);
        let ctx = ctx_with(reg);
        let tool = ToolSearchTool::new().with_rate_per_minute(2);
        // 2 admitted, 3rd refused.
        tool.call(&ctx, json!({"query": "select:FileEdit"}))
            .await
            .unwrap();
        tool.call(&ctx, json!({"query": "select:FileEdit"}))
            .await
            .unwrap();
        let err = tool
            .call(&ctx, json!({"query": "select:FileEdit"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("rate limit exceeded"), "got: {err}");
    }

    #[tokio::test]
    async fn rate_limit_zero_means_unlimited() {
        let reg = reg_with_deferred(&[("FileEdit", "Edit a file", None)]);
        let ctx = ctx_with(reg);
        let tool = ToolSearchTool::new().with_rate_per_minute(0);
        for _ in 0..50 {
            tool.call(&ctx, json!({"query": "select:FileEdit"}))
                .await
                .unwrap();
        }
    }
}
