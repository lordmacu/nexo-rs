//! Per-turn loop. Owns the spawned `claude` subprocess for one
//! turn, projects events back to the orchestrator, and synthesises
//! an `AttemptResult` at the end.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use nexo_driver_claude::{
    spawn_turn, ClaudeCommand, ClaudeError, ClaudeEvent, ResultEvent, SessionBinding,
    SessionBindingStore,
};
use nexo_driver_types::{
    AttemptOutcome, AttemptParams, AttemptResult, BudgetUsage, CancellationToken, GoalId,
};

use crate::acceptance::AcceptanceEvaluator;
use crate::error::DriverError;

/// Bundle of refs the loop borrows for one attempt.
pub(crate) struct AttemptContext<'a> {
    pub claude_cfg: &'a nexo_driver_claude::ClaudeConfig,
    pub binding_store: &'a Arc<dyn SessionBindingStore>,
    pub acceptance: &'a Arc<dyn AcceptanceEvaluator>,
    pub workspace: &'a Path,
    pub mcp_config_path: &'a Path,
    pub bin_path: &'a Path,
    pub cancel: CancellationToken,
}

pub(crate) async fn run_attempt(
    ctx: AttemptContext<'_>,
    params: AttemptParams,
) -> Result<AttemptResult, DriverError> {
    let goal_id = params.goal.id;
    let mut usage = params.usage.clone();

    // Compose the command. Reuse binding session_id when present.
    let binary = ctx
        .claude_cfg
        .binary
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("claude"));
    let prior = ctx.binding_store.get(goal_id).await?;
    // Phase 67.9 — when the orchestrator scheduled a compact turn it
    // pre-fills `extras["compact_turn"] = true`; substitute the
    // prompt with a `/compact <focus>` slash command so Claude Code
    // compacts its context.
    let prompt = if params
        .extras
        .get("compact_turn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let focus = params
            .extras
            .get("compact_focus")
            .and_then(|v| v.as_str())
            .unwrap_or("continue working");
        format!("/compact {focus}")
    } else {
        params.goal.description.clone()
    };
    let mut cmd = ClaudeCommand::new(binary, prompt)
        .apply_defaults(&ctx.claude_cfg.default_args)
        .cwd(ctx.workspace)
        .mcp_config(ctx.mcp_config_path);
    cmd = match &prior {
        Some(b) => cmd.resume(b.session_id.clone()),
        None => cmd, // first turn — claude assigns its own session id
    };
    let _ = ctx.bin_path; // bin path lives in mcp_config; kept here for future env injection.

    let turn_start = Instant::now();
    let mut turn = match spawn_turn(
        cmd,
        &ctx.cancel,
        ctx.claude_cfg.turn_timeout,
        ctx.claude_cfg.forced_kill_after,
    )
    .await
    {
        Ok(t) => t,
        Err(ClaudeError::Cancelled) => {
            return Ok(synthetic(
                goal_id,
                params.turn_index,
                AttemptOutcome::Cancelled,
                usage,
            ));
        }
        Err(e) => {
            return Ok(synthetic(
                goal_id,
                params.turn_index,
                AttemptOutcome::Escalate {
                    reason: format!("spawn failed: {e}"),
                },
                usage,
            ));
        }
    };

    let mut last_session_id: Option<String> = prior.map(|b| b.session_id);
    let mut final_text: Option<String> = None;
    let mut claimed_done = false;
    let mut session_invalid = false;
    let mut error_message: Option<String> = None;

    loop {
        let ev = match turn.next_event().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(ClaudeError::Cancelled) => {
                let _ = turn.shutdown().await;
                return Ok(synthetic(
                    goal_id,
                    params.turn_index,
                    AttemptOutcome::Cancelled,
                    usage,
                ));
            }
            Err(ClaudeError::Timeout) => {
                let _ = turn.shutdown().await;
                return Ok(synthetic(
                    goal_id,
                    params.turn_index,
                    AttemptOutcome::Continue {
                        reason: "turn timeout".into(),
                    },
                    usage,
                ));
            }
            Err(e) => {
                let _ = turn.shutdown().await;
                return Ok(synthetic(
                    goal_id,
                    params.turn_index,
                    AttemptOutcome::Escalate {
                        reason: format!("stream error: {e}"),
                    },
                    usage,
                ));
            }
        };
        if let Some(sid) = ev.session_id() {
            last_session_id = Some(sid.to_string());
        }
        match &ev {
            ClaudeEvent::Result(ResultEvent::Success {
                result, usage: tu, ..
            }) => {
                final_text = result.clone();
                let total = tu.input_tokens + tu.output_tokens + tu.cache_read_input_tokens;
                usage.tokens = usage.tokens.saturating_add(total);
                claimed_done = true;
                break;
            }
            ClaudeEvent::Result(ResultEvent::ErrorMaxTurns { .. }) => {
                error_message = Some("claude reported max turns".into());
                break;
            }
            ClaudeEvent::Result(ResultEvent::ErrorDuringExecution { message, .. }) => {
                let m = message.clone().unwrap_or_default();
                if m.to_lowercase().contains("session") {
                    session_invalid = true;
                }
                error_message = Some(m);
                break;
            }
            _ => {}
        }
    }

    let _ = turn.shutdown().await;

    // Persist binding (whatever session id Claude reported last).
    // B1 — lift origin_channel + dispatcher out of goal.metadata so
    // the binding carries the chat that triggered the goal across
    // reattach, and the completion router knows where to send the
    // notify_origin summary.
    if let Some(sid) = &last_session_id {
        let workspace_pb: std::path::PathBuf = ctx.workspace.to_path_buf();
        let mut binding = SessionBinding::new(
            goal_id,
            sid.clone(),
            ctx.claude_cfg.default_args.model.clone(),
            Some(workspace_pb),
        );
        if let Some(o) = params.goal.metadata.get("origin_channel") {
            if !o.is_null() {
                if let Ok(parsed) =
                    serde_json::from_value::<nexo_driver_claude::OriginChannel>(o.clone())
                {
                    binding = binding.with_origin(parsed);
                }
            }
        }
        if let Some(d) = params.goal.metadata.get("dispatcher") {
            if !d.is_null() {
                if let Ok(parsed) =
                    serde_json::from_value::<nexo_driver_claude::DispatcherIdentity>(d.clone())
                {
                    binding = binding.with_dispatcher(parsed);
                }
            }
        }
        ctx.binding_store.upsert(binding).await?;
    }

    // Wall-time portion of usage.
    usage.wall_time = usage.wall_time.saturating_add(turn_start.elapsed());

    // Session-invalid: mark, return Continue so orchestrator retries.
    if session_invalid {
        ctx.binding_store.mark_invalid(goal_id).await?;
        return Ok(AttemptResult {
            goal_id,
            turn_index: params.turn_index,
            outcome: AttemptOutcome::Continue {
                reason: "session invalid: retrying".into(),
            },
            decisions_recorded: vec![],
            usage_after: usage,
            acceptance: None,
            final_text,
            harness_extras: harness_extras_with_session(&last_session_id),
        });
    }

    if let Some(msg) = error_message {
        return Ok(AttemptResult {
            goal_id,
            turn_index: params.turn_index,
            outcome: AttemptOutcome::Escalate { reason: msg },
            decisions_recorded: vec![],
            usage_after: usage,
            acceptance: None,
            final_text,
            harness_extras: harness_extras_with_session(&last_session_id),
        });
    }

    if !claimed_done {
        return Ok(AttemptResult {
            goal_id,
            turn_index: params.turn_index,
            outcome: AttemptOutcome::Continue {
                reason: "stream ended without result event".into(),
            },
            decisions_recorded: vec![],
            usage_after: usage,
            acceptance: None,
            final_text,
            harness_extras: harness_extras_with_session(&last_session_id),
        });
    }

    // Acceptance check post-claim.
    let verdict = ctx
        .acceptance
        .evaluate(&params.goal.acceptance, ctx.workspace)
        .await?;

    let outcome = if verdict.met {
        AttemptOutcome::Done
    } else {
        AttemptOutcome::NeedsRetry {
            failures: verdict.failures.clone(),
        }
    };

    Ok(AttemptResult {
        goal_id,
        turn_index: params.turn_index,
        outcome,
        decisions_recorded: vec![],
        usage_after: usage,
        acceptance: Some(verdict),
        final_text,
        harness_extras: harness_extras_with_session(&last_session_id),
    })
}

fn synthetic(
    goal_id: GoalId,
    turn_index: u32,
    outcome: AttemptOutcome,
    usage: BudgetUsage,
) -> AttemptResult {
    AttemptResult {
        goal_id,
        turn_index,
        outcome,
        decisions_recorded: vec![],
        usage_after: usage,
        acceptance: None,
        final_text: None,
        harness_extras: serde_json::Map::new(),
    }
}

fn harness_extras_with_session(sid: &Option<String>) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    if let Some(s) = sid {
        m.insert(
            "claude_code.session_id".into(),
            serde_json::Value::String(s.clone()),
        );
    }
    m
}
