//! Phase 67.G.3 — read-only query tools.
//!
//! Each tool returns a markdown string capped to ~3.5 KiB so a
//! single chat message stays under Telegram / WhatsApp's 4 KiB
//! limit without the adapter having to truncate mid-line.

use std::sync::Arc;

use nexo_agent_registry::{AgentRegistry, AgentRunStatus, LogBuffer};
use nexo_driver_types::GoalId;
use serde::Deserialize;

use crate::hooks::registry::HookRegistry;

const SUMMARY_BYTE_CAP: usize = 3_500;

fn cap(mut s: String) -> String {
    if s.len() <= SUMMARY_BYTE_CAP {
        return s;
    }
    let mut end = SUMMARY_BYTE_CAP.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

fn glyph(s: AgentRunStatus) -> &'static str {
    match s {
        AgentRunStatus::Running => "🟢",
        AgentRunStatus::Queued => "⏳",
        AgentRunStatus::Paused => "⏸",
        AgentRunStatus::Done => "✅",
        AgentRunStatus::Failed => "❌",
        AgentRunStatus::Cancelled => "⛔",
        AgentRunStatus::LostOnRestart => "❓",
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListAgentsInput {
    /// `running | queued | paused | done | failed | cancelled |
    /// lost_on_restart` — case-insensitive. Missing → no filter.
    #[serde(default)]
    pub filter: Option<String>,
    /// Optional phase id substring filter (case-sensitive prefix
    /// or exact). Lets a chat user say "only 67.x agents".
    #[serde(default)]
    pub phase_prefix: Option<String>,
}

pub async fn list_agents(input: ListAgentsInput, registry: Arc<AgentRegistry>) -> String {
    let rows = match registry.list().await {
        Ok(v) => v,
        Err(e) => return format!("registry error: {e}"),
    };
    let filter_status = input
        .filter
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .and_then(|s| match s.as_str() {
            "running" => Some(AgentRunStatus::Running),
            "queued" => Some(AgentRunStatus::Queued),
            "paused" => Some(AgentRunStatus::Paused),
            "done" => Some(AgentRunStatus::Done),
            "failed" => Some(AgentRunStatus::Failed),
            "cancelled" => Some(AgentRunStatus::Cancelled),
            "lost_on_restart" | "lost-on-restart" => Some(AgentRunStatus::LostOnRestart),
            _ => None,
        });

    let mut out = String::from("| status | id | phase | turn | wall | origin |\n|---|---|---|---|---|---|\n");
    let mut shown = 0usize;
    for r in rows {
        if let Some(f) = filter_status {
            if r.status != f {
                continue;
            }
        }
        if let Some(prefix) = &input.phase_prefix {
            if !r.phase_id.starts_with(prefix) {
                continue;
            }
        }
        out.push_str(&format!(
            "| {} {} | `{}` | {} | {} | {} | {} |\n",
            glyph(r.status),
            r.status.as_str(),
            short_id(r.goal_id),
            r.phase_id,
            r.turn,
            humantime::format_duration(r.wall),
            r.origin,
        ));
        shown += 1;
    }
    if shown == 0 {
        out.push_str("| _no agents matching_ |\n");
    }
    cap(out)
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentStatusInput {
    pub goal_id: GoalId,
}

pub async fn agent_status(input: AgentStatusInput, registry: Arc<AgentRegistry>) -> String {
    let Some(h) = registry.handle(input.goal_id) else {
        return format!("goal `{}` not in registry", short_id(input.goal_id));
    };
    let mut out = format!(
        "{glyph} **{phase}** — {status} (turn {turn}/{maxt})\n",
        glyph = glyph(h.status),
        phase = h.phase_id,
        status = h.status.as_str(),
        turn = h.snapshot.turn_index,
        maxt = h.snapshot.max_turns,
    );
    out.push_str(&format!(
        "started: `{}` · elapsed: {}\n",
        h.started_at.format("%Y-%m-%d %H:%M:%SZ"),
        humantime::format_duration(h.elapsed())
    ));
    out.push_str(&format!(
        "tokens: {} · turns_used: {}\n",
        h.snapshot.usage.tokens, h.snapshot.usage.turns
    ));
    if let Some(diff) = &h.snapshot.last_diff_stat {
        out.push_str(&format!("diff: {}\n", diff));
    }
    if let Some(t) = &h.snapshot.last_progress_text {
        out.push_str(&format!("last_progress: {}\n", t));
    }
    if let Some(verdict) = &h.snapshot.last_acceptance {
        let met = if verdict.met { "ok" } else { "fail" };
        out.push_str(&format!(
            "last_acceptance: {met} ({} checks)\n",
            verdict.failures.len()
        ));
    }
    if let Some(o) = &h.origin {
        out.push_str(&format!(
            "origin: {}:{}@{}\n",
            o.plugin, o.instance, o.sender_id
        ));
    }
    cap(out)
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentLogsTailInput {
    pub goal_id: GoalId,
    #[serde(default = "default_lines")]
    pub lines: usize,
}

fn default_lines() -> usize {
    50
}

pub async fn agent_logs_tail(input: AgentLogsTailInput, log_buf: Arc<LogBuffer>) -> String {
    let lines = log_buf.tail(input.goal_id, input.lines);
    if lines.is_empty() {
        return "no logs".into();
    }
    let mut out = String::new();
    for l in lines {
        let ts = l
            .at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push_str(&format!("[{ts}] {} — {}\n", l.subject, l.summary));
    }
    cap(out)
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentHooksListInput {
    pub goal_id: GoalId,
}

pub async fn agent_hooks_list(input: AgentHooksListInput, hooks: Arc<HookRegistry>) -> String {
    let list = hooks.list(input.goal_id);
    if list.is_empty() {
        return "no hooks attached".into();
    }
    let mut out = String::from("| id | on | action |\n|---|---|---|\n");
    for h in list {
        let on = match &h.on {
            crate::hooks::types::HookTrigger::Done => "done".into(),
            crate::hooks::types::HookTrigger::Failed => "failed".into(),
            crate::hooks::types::HookTrigger::Cancelled => "cancelled".into(),
            crate::hooks::types::HookTrigger::Progress { every_turns } => {
                format!("progress(every={every_turns})")
            }
        };
        let action = match &h.action {
            crate::hooks::types::HookAction::NotifyOrigin => "notify_origin".into(),
            crate::hooks::types::HookAction::NotifyChannel {
                plugin, instance, recipient,
            } => format!("notify_channel({plugin}:{instance}@{recipient})"),
            crate::hooks::types::HookAction::DispatchAudit { .. } => "dispatch_audit(parent diff)".into(),
            crate::hooks::types::HookAction::DispatchPhase { phase_id, only_if } => {
                let on = match only_if {
                    crate::hooks::types::HookTrigger::Done => "done",
                    crate::hooks::types::HookTrigger::Failed => "failed",
                    crate::hooks::types::HookTrigger::Cancelled => "cancelled",
                    crate::hooks::types::HookTrigger::Progress { .. } => "progress",
                };
                format!("dispatch_phase({phase_id} only_if={on})")
            }
            crate::hooks::types::HookAction::NatsPublish { subject } => {
                format!("nats_publish({subject})")
            }
            crate::hooks::types::HookAction::Shell { cmd, .. } => {
                let preview: String = cmd.chars().take(40).collect();
                format!("shell({preview}…)")
            }
        };
        out.push_str(&format!("| `{}` | {} | {} |\n", h.id, on, action));
    }
    cap(out)
}

fn short_id(g: GoalId) -> String {
    let s = g.0.to_string();
    s.chars().take(8).collect()
}
