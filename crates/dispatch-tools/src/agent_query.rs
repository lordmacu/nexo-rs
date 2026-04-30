//! Phase 67.G.3 — read-only query tools.
//!
//! Each tool returns a markdown string capped to ~3.5 KiB so a
//! single chat message stays under Telegram / WhatsApp's 4 KiB
//! limit without the adapter having to truncate mid-line.

use std::sync::Arc;

use nexo_agent_registry::{AgentRegistry, AgentRunStatus, LogBuffer, TurnLogStore};
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
        AgentRunStatus::Sleeping => "💤",
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
    /// `running | sleeping | queued | paused | done | failed | cancelled |
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
            "sleeping" => Some(AgentRunStatus::Sleeping),
            "queued" => Some(AgentRunStatus::Queued),
            "paused" => Some(AgentRunStatus::Paused),
            "done" => Some(AgentRunStatus::Done),
            "failed" => Some(AgentRunStatus::Failed),
            "cancelled" => Some(AgentRunStatus::Cancelled),
            "lost_on_restart" | "lost-on-restart" => Some(AgentRunStatus::LostOnRestart),
            _ => None,
        });

    let mut out =
        String::from("| status | id | phase | turn | wall | origin |\n|---|---|---|---|---|---|\n");
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
            r.goal_id.0,
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
        return format!("goal `{}` not in registry", input.goal_id.0);
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
    if let Some(sleep) = &h.snapshot.sleep {
        out.push_str(&format!(
            "sleep: wake_at `{}` · duration {} · reason: {}\n",
            sleep.wake_at.format("%Y-%m-%d %H:%M:%SZ"),
            humantime::format_duration(std::time::Duration::from_millis(sleep.duration_ms)),
            sleep.reason
        ));
    }
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
        let ts =
            l.at.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
        out.push_str(&format!("[{ts}] {} — {}\n", l.subject, l.summary));
    }
    cap(out)
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentTurnsTailInput {
    pub goal_id: GoalId,
    #[serde(default = "default_turns")]
    pub n: usize,
}

fn default_turns() -> usize {
    20
}

/// Phase 72.3 — read the durable turn log. Markdown table with one
/// row per recorded turn, oldest first so the operator can scroll
/// the table top-to-bottom and follow the run. The header reports
/// `<shown> of <total>` so a 200-turn goal isn't silently
/// truncated.
pub async fn agent_turns_tail(input: AgentTurnsTailInput, store: Arc<dyn TurnLogStore>) -> String {
    let n = input.n.clamp(1, 1000);
    let total = match store.count(input.goal_id).await {
        Ok(c) => c,
        Err(e) => return format!("turn log error: {e}"),
    };
    let rows = match store.tail(input.goal_id, n).await {
        Ok(r) => r,
        Err(e) => return format!("turn log error: {e}"),
    };
    if rows.is_empty() {
        return format!(
            "no recorded turns for `{}` (the goal may have started before \
             Phase 72 wired the turn log, or it was evicted)",
            input.goal_id.0,
        );
    }
    let mut out = format!(
        "showing {} of {total} turn(s) for `{}`\n\n\
         | turn | outcome | decision | summary | error |\n\
         |---|---|---|---|---|\n",
        rows.len(),
        input.goal_id.0,
    );
    for r in &rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            r.turn_index,
            r.outcome,
            cell(r.decision.as_deref(), 80),
            cell(r.summary.as_deref(), 80),
            cell(r.error.as_deref(), 80),
        ));
    }
    cap(out)
}

/// Markdown-table-safe cell: collapse newlines, escape pipes,
/// truncate to `max` chars. Empty / `None` → `-`.
fn cell(s: Option<&str>, max: usize) -> String {
    let raw = s.unwrap_or("").trim();
    if raw.is_empty() {
        return "-".into();
    }
    let mut cleaned: String = raw
        .chars()
        .map(|c| match c {
            '\n' | '\r' => ' ',
            '|' => '/',
            _ => c,
        })
        .collect();
    if cleaned.chars().count() > max {
        cleaned = cleaned.chars().take(max).collect::<String>() + "…";
    }
    cleaned
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
                plugin,
                instance,
                recipient,
            } => format!("notify_channel({plugin}:{instance}@{recipient})"),
            crate::hooks::types::HookAction::DispatchAudit { .. } => {
                "dispatch_audit(parent diff)".into()
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nexo_agent_registry::{SqliteTurnLogStore, TurnLogStore, TurnRecord};
    use uuid::Uuid;

    fn record(goal: GoalId, turn: u32, outcome: &str) -> TurnRecord {
        TurnRecord {
            goal_id: goal,
            turn_index: turn,
            recorded_at: Utc::now(),
            outcome: outcome.into(),
            decision: Some(format!("Edit (allow) — touch crate {turn}")),
            summary: Some(format!("turn {turn} summary")),
            diff_stat: None,
            error: if outcome == "needs_retry" {
                Some("E0432".into())
            } else {
                None
            },
            raw_json: "{}".into(),
            source: None,
        }
    }

    #[tokio::test]
    async fn agent_turns_tail_renders_table_with_count_header() {
        let store: Arc<dyn TurnLogStore> =
            Arc::new(SqliteTurnLogStore::open_memory().await.unwrap());
        let goal = GoalId(Uuid::new_v4());
        for i in 1..=5u32 {
            store.append(&record(goal, i, "continue")).await.unwrap();
        }
        let out = agent_turns_tail(
            AgentTurnsTailInput {
                goal_id: goal,
                n: 3,
            },
            Arc::clone(&store),
        )
        .await;
        assert!(out.contains("showing 3 of 5"));
        assert!(out.contains(&goal.0.to_string()));
        assert!(out.contains("Edit"));
        assert!(out.contains("turn 5 summary"));
        // Older turns (1, 2) excluded by the n=3 cap.
        assert!(!out.contains("turn 1 summary"));
    }

    #[tokio::test]
    async fn agent_turns_tail_empty_goal_returns_helpful_message() {
        let store: Arc<dyn TurnLogStore> =
            Arc::new(SqliteTurnLogStore::open_memory().await.unwrap());
        let goal = GoalId(Uuid::new_v4());
        let out = agent_turns_tail(
            AgentTurnsTailInput {
                goal_id: goal,
                n: 10,
            },
            store,
        )
        .await;
        assert!(out.starts_with("no recorded turns"));
        assert!(out.contains(&goal.0.to_string()));
    }

    #[test]
    fn cell_truncates_and_sanitises() {
        assert_eq!(cell(None, 10), "-");
        assert_eq!(cell(Some(""), 10), "-");
        let c = cell(Some("with | pipe\nand newline"), 80);
        assert!(!c.contains('|'), "pipes must be escaped: {c}");
        assert!(!c.contains('\n'));
        let too_long = "x".repeat(200);
        let trimmed = cell(Some(&too_long), 50);
        assert!(trimmed.chars().count() <= 51); // 50 + ellipsis
        assert!(trimmed.ends_with('…'));
    }
}
