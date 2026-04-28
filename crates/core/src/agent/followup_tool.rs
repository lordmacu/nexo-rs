use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexo_llm::ToolDef;
use nexo_memory::{EmailFollowupEntry, LongTermMemory};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::{uuid, Uuid};

// Must match `nexo-plugin-email`'s EMAIL_NS to keep deterministic
// `session_id` derivation stable across inbound email + follow-up flows.
const EMAIL_THREAD_NS: Uuid = uuid!("c1c0a700-48e6-5000-a000-000000000000");
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_CHECK_EVERY: &str = "24h";
const FOLLOWUP_MAX_ATTEMPTS_CAP: u32 = 20;

pub struct StartFollowupTool {
    memory: Arc<LongTermMemory>,
}

impl StartFollowupTool {
    pub fn new(memory: Arc<LongTermMemory>) -> Self {
        Self { memory }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "start_followup".to_string(),
            description: "Start a durable email follow-up flow bound to one thread/session. The heartbeat loop re-checks automatically after the configured delay."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "flow_id": { "type": "string", "description": "Optional UUID. If omitted, the tool generates one." },
                    "thread_root_id": { "type": "string", "description": "Email thread root id (Message-ID root)." },
                    "instance": { "type": "string", "description": "Email plugin instance to use (e.g. `ops`, `sales`)." },
                    "recipient": { "type": "string", "description": "Customer email address for this follow-up case." },
                    "session_id": { "type": "string", "description": "Optional UUID override. If omitted, derives UUIDv5 from `thread_root_id`." },
                    "next_check_at": { "type": "string", "description": "Optional RFC3339 timestamp for first re-check." },
                    "check_after": { "type": "string", "description": "Optional relative delay for first re-check (e.g. `2h`, `1d`). Used when `next_check_at` is absent." },
                    "check_every": { "type": "string", "description": "Optional relative interval between attempts (default 24h)." },
                    "max_attempts": { "type": "integer", "description": "Optional max follow-up attempts (default 3, cap 20)." },
                    "instruction": { "type": "string", "description": "Optional extra instruction injected on each automated re-check turn." }
                },
                "required": ["thread_root_id", "instance", "recipient"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for StartFollowupTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let thread_root_id = required_nonempty(&args, "thread_root_id")?;
        let instance = required_nonempty(&args, "instance")?;
        let recipient = required_nonempty(&args, "recipient")?;
        let flow_id = optional_uuid(&args, "flow_id")?.unwrap_or_else(Uuid::new_v4);
        let session_id = optional_uuid(&args, "session_id")?
            .unwrap_or_else(|| Uuid::new_v5(&EMAIL_THREAD_NS, thread_root_id.as_bytes()));
        let check_every = parse_interval_secs(
            optional_nonempty(&args, "check_every").as_deref(),
            DEFAULT_CHECK_EVERY,
        )?;
        let max_attempts = args
            .get("max_attempts")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(DEFAULT_MAX_ATTEMPTS)
            .clamp(1, FOLLOWUP_MAX_ATTEMPTS_CAP);
        let next_check_at = parse_next_check_at(
            optional_nonempty(&args, "next_check_at").as_deref(),
            optional_nonempty(&args, "check_after").as_deref(),
        )?;
        let instruction = optional_nonempty(&args, "instruction").unwrap_or_else(|| {
            "Revisa el hilo por respuesta del cliente. Si ya respondió o el caso está resuelto, llama cancel_followup. Si no respondió, envía follow-up manteniendo el threading."
                .to_string()
        });

        let (entry, created) = self
            .memory
            .start_email_followup(
                flow_id,
                &ctx.agent_id,
                session_id,
                "email",
                Some(&instance),
                &recipient,
                &thread_root_id,
                &instruction,
                check_every,
                max_attempts,
                next_check_at,
            )
            .await?;

        Ok(json!({
            "ok": true,
            "created": created,
            "flow": followup_json(&entry),
        }))
    }
}

pub struct CheckFollowupTool {
    memory: Arc<LongTermMemory>,
}

impl CheckFollowupTool {
    pub fn new(memory: Arc<LongTermMemory>) -> Self {
        Self { memory }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "check_followup".to_string(),
            description: "Read one durable email follow-up flow by flow_id.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "flow_id": { "type": "string", "description": "UUID returned by start_followup." }
                },
                "required": ["flow_id"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for CheckFollowupTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let flow_id = required_uuid(&args, "flow_id")?;
        let flow = self.memory.get_email_followup(flow_id).await?;
        Ok(match flow {
            Some(entry) => json!({
                "ok": true,
                "found": true,
                "flow": followup_json(&entry),
            }),
            None => json!({
                "ok": true,
                "found": false,
                "flow_id": flow_id.to_string(),
            }),
        })
    }
}

pub struct CancelFollowupTool {
    memory: Arc<LongTermMemory>,
}

impl CancelFollowupTool {
    pub fn new(memory: Arc<LongTermMemory>) -> Self {
        Self { memory }
    }

    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "cancel_followup".to_string(),
            description: "Cancel a durable follow-up flow so heartbeat stops scheduling checks."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "flow_id": { "type": "string", "description": "UUID returned by start_followup." },
                    "reason": { "type": "string", "description": "Optional cancellation note." }
                },
                "required": ["flow_id"]
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for CancelFollowupTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let flow_id = required_uuid(&args, "flow_id")?;
        let reason = optional_nonempty(&args, "reason");
        let cancelled = self
            .memory
            .cancel_email_followup(flow_id, reason.as_deref())
            .await?;
        Ok(json!({
            "ok": true,
            "flow_id": flow_id.to_string(),
            "cancelled": cancelled,
        }))
    }
}

fn followup_json(e: &EmailFollowupEntry) -> Value {
    json!({
        "flow_id": e.flow_id.to_string(),
        "agent_id": e.agent_id,
        "session_id": e.session_id.to_string(),
        "source_plugin": e.source_plugin,
        "source_instance": e.source_instance,
        "recipient": e.recipient,
        "thread_root_id": e.thread_root_id,
        "instruction": e.instruction,
        "check_every_secs": e.check_every_secs,
        "max_attempts": e.max_attempts,
        "attempts": e.attempts,
        "next_check_at": e.next_check_at.to_rfc3339(),
        "claimed_at": e.claimed_at.map(|t| t.to_rfc3339()),
        "status": e.status,
        "status_note": e.status_note,
        "created_at": e.created_at.to_rfc3339(),
        "updated_at": e.updated_at.to_rfc3339(),
    })
}

fn parse_next_check_at(
    next_check_at: Option<&str>,
    check_after: Option<&str>,
) -> anyhow::Result<DateTime<Utc>> {
    if let Some(raw) = next_check_at {
        let at = DateTime::parse_from_rfc3339(raw)
            .map_err(|e| anyhow::anyhow!("invalid next_check_at `{raw}`: {e}"))?;
        return Ok(at.with_timezone(&Utc));
    }
    let fallback = check_after.unwrap_or(DEFAULT_CHECK_EVERY);
    let dur = humantime::parse_duration(fallback)
        .map_err(|e| anyhow::anyhow!("invalid check_after `{fallback}`: {e}"))?;
    let delta = chrono::Duration::from_std(dur)
        .map_err(|_| anyhow::anyhow!("check_after out of range: {fallback}"))?;
    Ok(Utc::now() + delta)
}

fn parse_interval_secs(raw: Option<&str>, default_raw: &str) -> anyhow::Result<u64> {
    let value = raw.unwrap_or(default_raw);
    let dur = humantime::parse_duration(value)
        .map_err(|e| anyhow::anyhow!("invalid interval `{value}`: {e}"))?;
    Ok(dur.as_secs().max(60))
}

fn optional_nonempty(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn required_nonempty(args: &Value, key: &str) -> anyhow::Result<String> {
    optional_nonempty(args, key).ok_or_else(|| anyhow::anyhow!("missing required `{key}`"))
}

fn optional_uuid(args: &Value, key: &str) -> anyhow::Result<Option<Uuid>> {
    let Some(raw) = optional_nonempty(args, key) else {
        return Ok(None);
    };
    Uuid::parse_str(&raw)
        .map(Some)
        .map_err(|e| anyhow::anyhow!("invalid `{key}` UUID `{raw}`: {e}"))
}

fn required_uuid(args: &Value, key: &str) -> anyhow::Result<Uuid> {
    optional_uuid(args, key)?.ok_or_else(|| anyhow::anyhow!("missing required `{key}`"))
}
