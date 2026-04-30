use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Value};

use super::tool_registry::ToolHandler;
use super::AgentContext;
use crate::telemetry::inc_proactive_event;

/// Sentinel key in the JSON result that signals "I want to sleep".
/// The driver-loop inspects every tool result for this key and intercepts
/// it before continuing the LLM turn loop.
pub const SLEEP_SENTINEL: &str = "__nexo_sleep__";

/// Returns `true` when `value` carries the Sleep sentinel produced by
/// `SleepTool::call`. Used by the driver-loop to intercept without knowing
/// the tool name.
pub fn is_sleep_result(value: &Value) -> bool {
    value
        .get(SLEEP_SENTINEL)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Extracts `duration_ms` from a Sleep sentinel result. Returns `None`
/// if the value is not a sleep result.
pub fn extract_sleep_ms(value: &Value) -> Option<u64> {
    if !is_sleep_result(value) {
        return None;
    }
    value.get("duration_ms")?.as_u64()
}

pub const SLEEP_MIN_MS: u64 = 60_000;
pub const SLEEP_MAX_MS: u64 = 86_400_000;
pub const CACHE_WARM_MAX_MS: u64 = 270_000;
pub const CACHE_COLD_MIN_MS: u64 = 1_200_000;

/// Phase 77.20 — Sleep tool.
///
/// When the model calls `Sleep { duration_ms, reason }` the driver-loop:
/// 1. Intercepts the sentinel result (does NOT pass it back to the LLM).
/// 2. Pauses the goal for the requested duration.
/// 3. Injects a `<tick>` prompt to wake the goal up.
///
/// Timing guidance injected into the system prompt:
/// - <= 270 000 ms → keeps the Anthropic prompt cache warm (5-min TTL)
/// - ≥ 1 200 000 ms → amortises the cache-miss cost over a long wait
/// - Avoid the 270 000–1 200 000 ms window (pays the miss without amortising)
#[derive(Clone)]
pub struct SleepTool;

impl SleepTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "Sleep".into(),
            description:
                "Pause this goal for the specified duration, then receive a <tick> wake-up. \
                 The user can interrupt the sleep at any time. Use when the user tells you to \
                 sleep or rest, when there is nothing to do, or when you are waiting for \
                 something. Prefer this over Bash(sleep ...); it does not hold a shell process.\n\n\
                 Cache timing guidance:\n\
                 - <= 270000 ms keeps prompt cache warm (Anthropic 5-minute TTL)\n\
                 - >= 1200000 ms amortises cache-miss cost over a long idle\n\
                 - Avoid 270000-1200000 ms range; it pays the miss without benefit"
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "duration_ms": {
                        "type": "integer",
                        "description": "Milliseconds to sleep. Clamped to [60000, 86400000]."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why are you sleeping? Used for observability logs."
                    }
                },
                "required": ["duration_ms", "reason"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for SleepTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> Result<Value> {
        let raw_ms = args["duration_ms"]
            .as_u64()
            .ok_or_else(|| anyhow!("Sleep: duration_ms must be a non-negative integer"))?;
        let clamped_ms = clamp_sleep_duration_ms(raw_ms);

        let reason = args["reason"]
            .as_str()
            .unwrap_or("no reason given")
            .to_string();
        let cache_aware = ctx
            .effective
            .as_ref()
            .map(|p| p.proactive.cache_aware_schedule)
            .unwrap_or(true);
        let ms = if cache_aware {
            cache_aware_sleep_duration_ms(clamped_ms)
        } else {
            clamped_ms
        };
        if ms != clamped_ms {
            inc_proactive_event(&ctx.agent_id, "cache_aware.snapped");
        }

        Ok(json!({
            SLEEP_SENTINEL: true,
            "duration_ms": ms,
            "requested_duration_ms": raw_ms,
            "clamped_duration_ms": clamped_ms,
            "cache_aware_adjusted": ms != clamped_ms,
            "reason": reason
        }))
    }
}

pub fn clamp_sleep_duration_ms(ms: u64) -> u64 {
    ms.clamp(SLEEP_MIN_MS, SLEEP_MAX_MS)
}

pub fn cache_aware_sleep_duration_ms(ms: u64) -> u64 {
    if !(CACHE_WARM_MAX_MS + 1..CACHE_COLD_MIN_MS).contains(&ms) {
        return ms;
    }
    let down_delta = ms.saturating_sub(CACHE_WARM_MAX_MS);
    let up_delta = CACHE_COLD_MIN_MS.saturating_sub(ms);
    if down_delta <= up_delta {
        CACHE_WARM_MAX_MS
    } else {
        CACHE_COLD_MIN_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig,
    };
    use std::sync::Arc;
    use std::time::Duration;

    fn ctx() -> AgentContext {
        let cfg = Arc::new(AgentConfig {
            id: "sleep-test".into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m0".into(),
            },
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            plugins: vec![],
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            outbound_allowlist: Default::default(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
        });
        let broker = AnyBroker::local();
        let sessions = Arc::new(SessionManager::new(Duration::from_secs(30), 4));
        AgentContext::new("sleep-test", cfg, broker, sessions)
    }

    #[tokio::test]
    async fn sleep_tool_returns_sentinel() {
        let result = SleepTool
            .call(
                &ctx(),
                json!({"duration_ms": 5000, "reason": "nothing to do"}),
            )
            .await
            .unwrap();
        assert!(is_sleep_result(&result));
        assert_eq!(extract_sleep_ms(&result), Some(60_000));
    }

    #[tokio::test]
    async fn sleep_tool_clamps_min() {
        let result = SleepTool
            .call(&ctx(), json!({"duration_ms": 0, "reason": "test"}))
            .await
            .unwrap();
        assert_eq!(extract_sleep_ms(&result), Some(60_000));
    }

    #[tokio::test]
    async fn sleep_tool_clamps_max() {
        let result = SleepTool
            .call(&ctx(), json!({"duration_ms": 99_999_999, "reason": "test"}))
            .await
            .unwrap();
        assert_eq!(extract_sleep_ms(&result), Some(86_400_000));
    }

    #[tokio::test]
    async fn is_sleep_result_false_for_normal() {
        let normal = json!({"text": "hello"});
        assert!(!is_sleep_result(&normal));
        assert_eq!(extract_sleep_ms(&normal), None);
    }

    #[test]
    fn cache_aware_scheduler_covers_windows() {
        assert_eq!(cache_aware_sleep_duration_ms(60_000), 60_000);
        assert_eq!(cache_aware_sleep_duration_ms(300_000), 270_000);
        assert_eq!(cache_aware_sleep_duration_ms(900_000), 1_200_000);
        assert_eq!(cache_aware_sleep_duration_ms(2_000_000), 2_000_000);
    }
}
