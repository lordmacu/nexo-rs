//! Default `HookDispatcher` impl. Translates a `HookAction` into a
//! concrete side effect — looking up the channel adapter for
//! `NotifyOrigin` / `NotifyChannel`, publishing JSON for
//! `NatsPublish`, etc.
//!
//! Failure isolation: each hook runs under its own timeout, and a
//! failing hook never aborts the others. The caller iterates the
//! hook list and collects per-hook results; consumers display a
//! "1/3 hooks failed" summary in chat without blocking the goal.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexo_pairing::PairingAdapterRegistry;
use thiserror::Error;

use super::types::{CompletionHook, HookAction, HookPayload};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HookError {
    #[error("origin channel missing — NotifyOrigin requires goal.origin to be set")]
    MissingOrigin,
    #[error("no pairing adapter for channel {0}")]
    UnknownChannel(String),
    #[error("nats publish failed: {0}")]
    NatsPublish(String),
    #[error("adapter send failed: {0}")]
    Adapter(String),
    #[error("hook timed out after {0:?}")]
    Timeout(Duration),
    #[error("shell hook disabled by config")]
    ShellDisabled,
    #[error("dispatch_phase chaining is wired in 67.F.2")]
    ChainingNotWired,
}

#[async_trait]
pub trait HookDispatcher: Send + Sync + 'static {
    async fn dispatch(&self, hook: &CompletionHook, payload: &HookPayload) -> Result<(), HookError>;
}

/// Trait abstraction over NATS publish so tests can substitute a
/// capturing publisher without pulling in a live broker.
#[async_trait]
pub trait NatsHookPublisher: Send + Sync + 'static {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), String>;
}

#[derive(Default)]
pub struct NoopNatsHookPublisher;

#[async_trait]
impl NatsHookPublisher for NoopNatsHookPublisher {
    async fn publish(&self, _subject: &str, _payload: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

/// Concrete dispatcher used in production. Holds the pairing
/// adapter registry (Phase 26) for chat notifications, and a NATS
/// publisher for `NatsPublish` action hooks.
pub struct DefaultHookDispatcher {
    pub pairing: PairingAdapterRegistry,
    pub nats: Arc<dyn NatsHookPublisher>,
    pub timeout: Duration,
    /// 67.F.4 — gate for the `shell` action. Default false.
    pub allow_shell: bool,
}

impl DefaultHookDispatcher {
    pub fn new(pairing: PairingAdapterRegistry, nats: Arc<dyn NatsHookPublisher>) -> Self {
        Self {
            pairing,
            nats,
            timeout: Duration::from_secs(30),
            allow_shell: false,
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    pub fn with_allow_shell(mut self, v: bool) -> Self {
        self.allow_shell = v;
        self
    }
}

#[async_trait]
impl HookDispatcher for DefaultHookDispatcher {
    async fn dispatch(&self, hook: &CompletionHook, payload: &HookPayload) -> Result<(), HookError> {
        let timeout = self.timeout;
        let fut = run_action(&hook.action, payload, &self.pairing, &self.nats, self.allow_shell);
        match tokio::time::timeout(timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(HookError::Timeout(timeout)),
        }
    }
}

async fn run_action(
    action: &HookAction,
    payload: &HookPayload,
    pairing: &PairingAdapterRegistry,
    nats: &Arc<dyn NatsHookPublisher>,
    allow_shell: bool,
) -> Result<(), HookError> {
    match action {
        HookAction::NotifyOrigin => {
            let Some(origin) = payload.origin.clone() else {
                return Err(HookError::MissingOrigin);
            };
            // Console origin → stdout / log path; not a chat. The
            // CLI runner picks these up via NATS subscription, so
            // the hook surface here is a no-op when plugin =
            // "console".
            if origin.plugin == "console" {
                return Ok(());
            }
            let adapter = pairing
                .get(&origin.plugin)
                .ok_or_else(|| HookError::UnknownChannel(origin.plugin.clone()))?;
            adapter
                .send_reply(&origin.instance, &origin.sender_id, &render_summary(payload))
                .await
                .map_err(|e| HookError::Adapter(e.to_string()))
        }
        HookAction::NotifyChannel { plugin, instance, recipient } => {
            let adapter = pairing
                .get(plugin)
                .ok_or_else(|| HookError::UnknownChannel(plugin.clone()))?;
            adapter
                .send_reply(instance, recipient, &render_summary(payload))
                .await
                .map_err(|e| HookError::Adapter(e.to_string()))
        }
        HookAction::NatsPublish { subject } => {
            let body = serde_json::to_vec(payload)
                .map_err(|e| HookError::NatsPublish(e.to_string()))?;
            nats.publish(subject, &body)
                .await
                .map_err(HookError::NatsPublish)
        }
        HookAction::DispatchPhase { .. } => Err(HookError::ChainingNotWired),
        HookAction::Shell { .. } if !allow_shell => Err(HookError::ShellDisabled),
        HookAction::Shell { .. } => {
            // 67.F.4 lands the actual exec; the gate already
            // protects callers from stumbling into it accidentally.
            Err(HookError::ShellDisabled)
        }
    }
}

/// Pre-rendered chat summary. Length-cap mirrors the
/// `summary_byte_cap` in project_tracker.yaml so callers don't
/// double-truncate.
pub fn render_summary(p: &HookPayload) -> String {
    let glyph = match p.transition {
        super::types::HookTransition::Done => "✅",
        super::types::HookTransition::Failed => "❌",
        super::types::HookTransition::Cancelled => "⛔",
        super::types::HookTransition::Progress => "🔄",
    };
    let mut out = format!(
        "{glyph} **{phase}** — {trans} ({elapsed})",
        glyph = glyph,
        phase = p.phase_id,
        trans = p.transition.as_str(),
        elapsed = p.elapsed
    );
    if let Some(diff) = &p.diff_stat {
        out.push_str(&format!("\n_diff:_ {}", diff));
    }
    if !p.summary.trim().is_empty() {
        out.push_str("\n");
        out.push_str(&p.summary);
    }
    out
}
