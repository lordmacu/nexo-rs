//! Phase 79.8 — `RemoteTrigger` tool: webhook + NATS publisher
//! gated by a per-session allowlist (agent-level or per-binding override).
//!
//! Diff vs leak: the leak's `RemoteTriggerTool` is a CRUD client
//! for claude.ai's hosted scheduled-agent API
//! (`/v1/code/triggers`). Different concept entirely. We adopt the
//! *name* and ship a generic outbound publisher per our PHASES.md
//! spec — webhook with HMAC sign + NATS publish, both gated by a
//! YAML allowlist so URLs never travel through the model.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/RemoteTriggerTool/RemoteTriggerTool.ts:1-161`
//!     (inputSchema name + the dispatcher pattern), but the core
//!     behaviour is our own design.
//!   * Spec: `proyecto/PHASES.md::79.8`.
//!
//! Reference (secondary):
//!   * OpenClaw `research/` — no equivalent. Single-process TS
//!     reference uses plugin outbound paths directly.
//!
//! Security model:
//!   * The model never sees the URL or NATS subject — it refers to
//!     a destination by `name`.
//!   * Webhook bodies are HMAC-SHA256 signed when `secret_env` is
//!     set; the runtime resolves the env var at call time.
//!   * Per-trigger token-bucket rate limit (default 10 calls/min,
//!     `0` = unlimited).
//!   * Hard cap 256 KiB per payload.
//!   * Plan-mode classified as `Outbound` (mutating) — refuses
//!     while plan mode is on.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use nexo_config::types::remote_triggers::{RemoteTriggerEntry, REMOTE_TRIGGER_MAX_BODY_BYTES};
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

/// Process-shared per-trigger rate limiter. Sliding window by
/// design: a `VecDeque` of timestamps trimmed to the last minute,
/// then `len() < limit` to admit. Memory bounded by the configured
/// limit; never grows unboundedly.
#[derive(Default)]
pub struct RemoteTriggerRateLimiter {
    buckets: DashMap<String, Mutex<std::collections::VecDeque<Instant>>>,
}

impl RemoteTriggerRateLimiter {
    /// `true` when the call is admitted; `false` when the trigger's
    /// per-minute budget is exhausted. `limit_per_minute == 0`
    /// always admits (no limit).
    pub fn try_acquire(&self, trigger_name: &str, limit_per_minute: u32) -> bool {
        if limit_per_minute == 0 {
            return true;
        }
        let entry = self.buckets.entry(trigger_name.to_string()).or_default();
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

/// Trait abstraction over the actual outbound webhook / NATS
/// publish so tests can substitute a fake without standing up a
/// real HTTP server / broker. Production wiring uses
/// [`ReqwestSink`] for webhook + `AnyBroker` for NATS.
#[async_trait]
pub trait RemoteTriggerSink: Send + Sync {
    /// Issue a POST with the supplied body + headers. Returns the
    /// response body + status. Errors when the network fails or the
    /// response status is ≥ 400.
    async fn post_webhook(
        &self,
        url: &str,
        body: &str,
        headers: Vec<(String, String)>,
        timeout: Duration,
    ) -> anyhow::Result<u16>;

    /// Publish a payload to a NATS subject. Errors propagate from
    /// the broker.
    async fn publish_nats(&self, subject: &str, payload: &[u8]) -> anyhow::Result<()>;
}

/// Production sink: reqwest + AnyBroker. Not used in tests.
pub struct ReqwestSink {
    pub broker: nexo_broker::AnyBroker,
    pub client: reqwest::Client,
}

impl ReqwestSink {
    pub fn new(broker: nexo_broker::AnyBroker) -> Self {
        Self {
            broker,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl RemoteTriggerSink for ReqwestSink {
    async fn post_webhook(
        &self,
        url: &str,
        body: &str,
        headers: Vec<(String, String)>,
        timeout: Duration,
    ) -> anyhow::Result<u16> {
        let mut req = self
            .client
            .post(url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let res = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("webhook POST failed: {e}"))?;
        let status = res.status().as_u16();
        if status >= 400 {
            let text = res.text().await.unwrap_or_default();
            anyhow::bail!(
                "webhook returned HTTP {status}: {body}",
                body = text.chars().take(200).collect::<String>()
            );
        }
        Ok(status)
    }

    async fn publish_nats(&self, subject: &str, payload: &[u8]) -> anyhow::Result<()> {
        use nexo_broker::BrokerHandle;
        let json: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|e| anyhow::anyhow!("NATS payload not JSON: {e}"))?;
        let event = nexo_broker::Event::new(subject, "remote_trigger", json);
        self.broker
            .publish(subject, event)
            .await
            .map_err(|e| anyhow::anyhow!("NATS publish failed: {e}"))
    }
}

pub struct RemoteTriggerTool {
    sink: Arc<dyn RemoteTriggerSink>,
    rate_limiter: Arc<RemoteTriggerRateLimiter>,
}

impl RemoteTriggerTool {
    pub fn new(sink: Arc<dyn RemoteTriggerSink>) -> Self {
        Self {
            sink,
            rate_limiter: Arc::new(RemoteTriggerRateLimiter::default()),
        }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "RemoteTrigger".to_string(),
            description: "Publish a JSON payload to a pre-configured outbound destination — webhook (HTTP POST, optionally HMAC-SHA256 signed) or NATS subject. Destinations are named in YAML (`agents[].remote_triggers` or binding override); the model passes only the name and payload, never URLs or subjects. Per-destination rate limit + 256 KiB body cap apply.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the destination as configured in `remote_triggers[].name` for this session's effective policy."
                    },
                    "payload": {
                        "description": "JSON payload to send. Object / array / scalar — any JSON. Capped at 256 KiB serialised."
                    }
                },
                "required": ["name", "payload"]
            }),
        }
    }
}

fn sign_body(secret: &[u8], body: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!("sha256={hex}")
}

#[async_trait]
impl ToolHandler for RemoteTriggerTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("RemoteTrigger requires `name` (string)"))?
            .to_string();
        let payload = args
            .get("payload")
            .ok_or_else(|| anyhow::anyhow!("RemoteTrigger requires `payload`"))?;

        // Resolve via the effective per-session allowlist:
        // `InboundBinding::remote_triggers` override when present,
        // otherwise `AgentConfig::remote_triggers`.
        let effective = ctx.effective_policy();
        let entry = effective
            .remote_triggers
            .iter()
            .find(|e| e.name() == name)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "RemoteTrigger: no destination named `{name}` in this session allowlist. Operator must add it under `agents[].remote_triggers[]` or the matched `inbound_bindings[].remote_triggers[]` override."
                )
            })?;

        // Serialise + cap before doing anything else so we never
        // even hit the rate-limiter on a bad payload.
        let body = serde_json::to_string(payload)
            .map_err(|e| anyhow::anyhow!("RemoteTrigger: payload not serialisable: {e}"))?;
        if body.len() > REMOTE_TRIGGER_MAX_BODY_BYTES {
            return Err(anyhow::anyhow!(
                "RemoteTrigger: payload too large ({actual} bytes > max {max})",
                actual = body.len(),
                max = REMOTE_TRIGGER_MAX_BODY_BYTES
            ));
        }

        let rate_limit_key = match effective.binding_index {
            Some(idx) => format!("{idx}:{}", entry.name()),
            None => format!("agent:{}", entry.name()),
        };
        if !self
            .rate_limiter
            .try_acquire(&rate_limit_key, entry.rate_limit_per_minute())
        {
            return Err(anyhow::anyhow!(
                "RemoteTrigger: rate limit exceeded for `{name}` ({} calls/min). Wait or raise `rate_limit_per_minute` in YAML.",
                entry.rate_limit_per_minute()
            ));
        }

        match &entry {
            RemoteTriggerEntry::Webhook {
                url,
                secret_env,
                timeout_ms,
                ..
            } => {
                let now_unix = chrono::Utc::now().timestamp();
                let mut headers: Vec<(String, String)> = Vec::with_capacity(3);
                headers.push(("X-Nexo-Trigger-Name".to_string(), entry.name().to_string()));
                headers.push(("X-Nexo-Timestamp".to_string(), now_unix.to_string()));
                if let Some(env) = secret_env {
                    let secret = std::env::var(env).map_err(|_| {
                        anyhow::anyhow!(
                            "RemoteTrigger: secret_env `{env}` is not set; refusing to send unsigned"
                        )
                    })?;
                    headers.push((
                        "X-Nexo-Signature".to_string(),
                        sign_body(secret.as_bytes(), &body),
                    ));
                }
                let timeout = Duration::from_millis(*timeout_ms);
                let started = Instant::now();
                let status = self.sink.post_webhook(url, &body, headers, timeout).await?;
                Ok(json!({
                    "ok": true,
                    "kind": "webhook",
                    "name": name,
                    "status": status,
                    "signed": secret_env.is_some(),
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "bytes_sent": body.len(),
                }))
            }
            RemoteTriggerEntry::Nats { subject, .. } => {
                let started = Instant::now();
                self.sink.publish_nats(subject, body.as_bytes()).await?;
                Ok(json!({
                    "ok": true,
                    "kind": "nats",
                    "name": name,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "bytes_sent": body.len(),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;
    use std::sync::Mutex;

    #[derive(Default)]
    #[allow(dead_code)]
    struct CapturedCall {
        url: String,
        body: String,
        headers: Vec<(String, String)>,
    }

    #[derive(Default)]
    struct FakeSink {
        webhook_calls: Mutex<Vec<CapturedCall>>,
        nats_calls: Mutex<Vec<(String, Vec<u8>)>>,
        force_status: Mutex<u16>,
    }

    impl FakeSink {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                force_status: Mutex::new(200),
                ..Default::default()
            })
        }
    }

    #[async_trait]
    impl RemoteTriggerSink for FakeSink {
        async fn post_webhook(
            &self,
            url: &str,
            body: &str,
            headers: Vec<(String, String)>,
            _timeout: Duration,
        ) -> anyhow::Result<u16> {
            self.webhook_calls.lock().unwrap().push(CapturedCall {
                url: url.to_string(),
                body: body.to_string(),
                headers,
            });
            let s = *self.force_status.lock().unwrap();
            if s >= 400 {
                anyhow::bail!("simulated HTTP {s}");
            }
            Ok(s)
        }
        async fn publish_nats(&self, subject: &str, payload: &[u8]) -> anyhow::Result<()> {
            self.nats_calls
                .lock()
                .unwrap()
                .push((subject.to_string(), payload.to_vec()));
            Ok(())
        }
    }

    fn agent_config_with_triggers(triggers: Vec<RemoteTriggerEntry>) -> AgentConfig {
        AgentConfig {
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
            remote_triggers: triggers,
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
            event_subscribers: Vec::new(),
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        }
    }

    fn ctx_with_triggers(triggers: Vec<RemoteTriggerEntry>) -> AgentContext {
        let cfg = agent_config_with_triggers(triggers);
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    fn ctx_with_binding_override(
        agent_level: Vec<RemoteTriggerEntry>,
        binding_level: Vec<RemoteTriggerEntry>,
        binding_index: usize,
    ) -> AgentContext {
        let cfg = Arc::new(agent_config_with_triggers(agent_level));
        let mut eff = crate::agent::EffectiveBindingPolicy::from_agent_defaults(&cfg);
        eff.binding_index = Some(binding_index);
        eff.remote_triggers = binding_level;
        AgentContext::new(
            "a",
            Arc::clone(&cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
        .with_effective(Arc::new(eff))
    }

    fn webhook(name: &str, secret: Option<&str>, rate: u32) -> RemoteTriggerEntry {
        RemoteTriggerEntry::Webhook {
            name: name.into(),
            url: format!("https://example.test/{name}"),
            secret_env: secret.map(str::to_string),
            timeout_ms: 5000,
            rate_limit_per_minute: rate,
        }
    }

    fn nats(name: &str, subject: &str, rate: u32) -> RemoteTriggerEntry {
        RemoteTriggerEntry::Nats {
            name: name.into(),
            subject: subject.into(),
            rate_limit_per_minute: rate,
        }
    }

    fn tool(sink: Arc<dyn RemoteTriggerSink>) -> RemoteTriggerTool {
        RemoteTriggerTool::new(sink)
    }

    #[tokio::test]
    async fn refuses_name_not_in_allowlist() {
        let ctx = ctx_with_triggers(vec![webhook("ops", None, 10)]);
        let sink = FakeSink::new();
        let err = tool(sink.clone())
            .call(&ctx, json!({"name": "imaginary", "payload": {"a": 1}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not in allowlist") || err.contains("no destination"));
        assert!(sink.webhook_calls.lock().unwrap().is_empty());
        assert!(sink.nats_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn binding_override_allowlist_wins_over_agent_level() {
        let ctx = ctx_with_binding_override(
            vec![webhook("agent_only", None, 10)],
            vec![webhook("binding_only", None, 10)],
            4,
        );
        let sink = FakeSink::new();
        let t = tool(sink.clone());

        t.call(&ctx, json!({"name": "binding_only", "payload": {"a": 1}}))
            .await
            .unwrap();
        let err = t
            .call(&ctx, json!({"name": "agent_only", "payload": {"a": 1}}))
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("no destination"),
            "agent-level destination must be hidden by binding override, got: {err}"
        );
        let calls = sink.webhook_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, "https://example.test/binding_only");
    }

    #[tokio::test]
    async fn rate_limit_isolated_per_binding_for_same_trigger_name() {
        let ctx_a = ctx_with_binding_override(vec![], vec![webhook("ops", None, 1)], 0);
        let ctx_b = ctx_with_binding_override(vec![], vec![webhook("ops", None, 1)], 1);
        let sink = FakeSink::new();
        let t = tool(sink.clone());

        // Same trigger name, different binding index: each gets its own bucket.
        t.call(&ctx_a, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap();
        t.call(&ctx_b, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap();

        let err_a = t
            .call(&ctx_a, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap_err()
            .to_string();
        let err_b = t
            .call(&ctx_b, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err_a.contains("rate limit"), "got: {err_a}");
        assert!(err_b.contains("rate limit"), "got: {err_b}");
        assert_eq!(sink.webhook_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn webhook_unsigned_emits_no_signature_header() {
        let ctx = ctx_with_triggers(vec![webhook("ops", None, 10)]);
        let sink = FakeSink::new();
        let res = tool(sink.clone())
            .call(&ctx, json!({"name": "ops", "payload": {"a": 1}}))
            .await
            .unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["kind"], "webhook");
        assert_eq!(res["signed"], false);
        let calls = sink.webhook_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0]
            .headers
            .iter()
            .any(|(k, _)| k == "X-Nexo-Signature"));
        assert!(calls[0]
            .headers
            .iter()
            .any(|(k, v)| k == "X-Nexo-Trigger-Name" && v == "ops"));
        assert!(calls[0]
            .headers
            .iter()
            .any(|(k, _)| k == "X-Nexo-Timestamp"));
    }

    #[tokio::test]
    async fn webhook_signed_emits_valid_hmac() {
        let env_var = "NEXO_TEST_RT_SECRET_VALID";
        std::env::set_var(env_var, "topsecret");
        let ctx = ctx_with_triggers(vec![webhook("ops", Some(env_var), 10)]);
        let sink = FakeSink::new();
        let _res = tool(sink.clone())
            .call(&ctx, json!({"name": "ops", "payload": {"a": 1}}))
            .await
            .unwrap();
        let calls = sink.webhook_calls.lock().unwrap();
        let sig = calls[0]
            .headers
            .iter()
            .find(|(k, _)| k == "X-Nexo-Signature")
            .map(|(_, v)| v.clone())
            .expect("signature header missing");
        assert!(sig.starts_with("sha256="));
        // Verify by recomputing.
        let expected = sign_body(b"topsecret", &calls[0].body);
        assert_eq!(sig, expected);
        std::env::remove_var(env_var);
    }

    #[tokio::test]
    async fn webhook_missing_secret_env_refuses() {
        let env_var = "NEXO_TEST_RT_SECRET_MISSING_PLEASE_DO_NOT_SET";
        std::env::remove_var(env_var);
        let ctx = ctx_with_triggers(vec![webhook("ops", Some(env_var), 10)]);
        let sink = FakeSink::new();
        let err = tool(sink.clone())
            .call(&ctx, json!({"name": "ops", "payload": {"a": 1}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not set"), "got: {err}");
        assert!(
            sink.webhook_calls.lock().unwrap().is_empty(),
            "must not send unsigned"
        );
    }

    #[tokio::test]
    async fn nats_publishes_to_subject() {
        let ctx = ctx_with_triggers(vec![nats("ops", "agent.outbound.ops", 10)]);
        let sink = FakeSink::new();
        let res = tool(sink.clone())
            .call(&ctx, json!({"name": "ops", "payload": {"x": 42}}))
            .await
            .unwrap();
        assert_eq!(res["kind"], "nats");
        let calls = sink.nats_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "agent.outbound.ops");
        let body: Value = serde_json::from_slice(&calls[0].1).unwrap();
        assert_eq!(body["x"], 42);
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_budget() {
        let ctx = ctx_with_triggers(vec![webhook("ops", None, 2)]);
        let sink = FakeSink::new();
        let t = tool(sink.clone());
        // 2 admitted, 3rd refused.
        t.call(&ctx, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap();
        t.call(&ctx, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap();
        let err = t
            .call(&ctx, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("rate limit"), "got: {err}");
        assert_eq!(sink.webhook_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rate_limit_zero_means_unlimited() {
        let ctx = ctx_with_triggers(vec![webhook("ops", None, 0)]);
        let sink = FakeSink::new();
        let t = tool(sink.clone());
        for _ in 0..50 {
            t.call(&ctx, json!({"name": "ops", "payload": {}}))
                .await
                .unwrap();
        }
        assert_eq!(sink.webhook_calls.lock().unwrap().len(), 50);
    }

    #[tokio::test]
    async fn payload_too_large_is_rejected_before_send() {
        let ctx = ctx_with_triggers(vec![webhook("ops", None, 10)]);
        let sink = FakeSink::new();
        // 257 KiB string — exceeds 256 KiB cap.
        let big = "x".repeat(REMOTE_TRIGGER_MAX_BODY_BYTES + 1);
        let err = tool(sink.clone())
            .call(&ctx, json!({"name": "ops", "payload": big}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("too large"), "got: {err}");
        assert!(sink.webhook_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn webhook_4xx_propagates_as_error() {
        let ctx = ctx_with_triggers(vec![webhook("ops", None, 10)]);
        let sink = FakeSink::new();
        *sink.force_status.lock().unwrap() = 503;
        let err = tool(sink.clone())
            .call(&ctx, json!({"name": "ops", "payload": {}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("503"), "got: {err}");
    }

    #[tokio::test]
    async fn missing_name_arg_errors() {
        let ctx = ctx_with_triggers(vec![]);
        let sink = FakeSink::new();
        let err = tool(sink)
            .call(&ctx, json!({"payload": {}}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires `name`"), "got: {err}");
    }

    #[tokio::test]
    async fn missing_payload_arg_errors() {
        let ctx = ctx_with_triggers(vec![]);
        let sink = FakeSink::new();
        let err = tool(sink)
            .call(&ctx, json!({"name": "ops"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires `payload`"), "got: {err}");
    }

    #[tokio::test]
    async fn sign_body_is_deterministic_and_hex() {
        let s1 = sign_body(b"k", "{}");
        let s2 = sign_body(b"k", "{}");
        assert_eq!(s1, s2);
        assert!(s1.starts_with("sha256="));
        let hex = s1.trim_start_matches("sha256=");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
