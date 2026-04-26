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
use nexo_driver_types::GoalId;
use nexo_pairing::PairingAdapterRegistry;
use thiserror::Error;

use super::types::{CompletionHook, HookAction, HookPayload};

/// Plugin ids whose origin does NOT want a chat-style notify.
/// Extended as new non-chat origins (`cron`, `webhook`) land. PT-9
/// upgrades this to a trait method on a future `OriginAdapter` so
/// each plugin declares its own preference.
pub const NON_CHAT_ORIGIN_PLUGINS: &[&str] = &["console", "cron", "webhook", "heartbeat"];

#[derive(Debug, Error, Clone, PartialEq)]
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
    #[error("shell hook exited {code:?}: {stderr}")]
    ShellExitNonZero { code: Option<i32>, stderr: String },
    #[error("shell hook io error: {0}")]
    ShellIo(String),
    #[error("dispatch_phase chaining is not wired (no chainer provided)")]
    ChainingNotWired,
    #[error("chain depth {0} exceeds max {1}")]
    ChainDepthExceeded(u32, u32),
    #[error("chain trigger guard {0:?} did not match transition {1:?}")]
    ChainGuardSkipped(super::types::HookTrigger, super::types::HookTransition),
    #[error("chain dispatch failed: {0}")]
    ChainDispatch(String),
    #[error("hook already dispatched (idempotency hit)")]
    AlreadyDispatched,
    #[error("idempotency store: {0}")]
    Idempotency(String),
}

/// Phase 67.F.2 — pluggable chainer the dispatcher consults when a
/// `DispatchPhase` action fires. Decouples the hook layer from the
/// concrete `program_phase_dispatch` plumbing so tests can supply a
/// capturing implementation and runtime callers wrap up the
/// orchestrator + registry + tracker context behind one trait.
#[async_trait]
pub trait DispatchPhaseChainer: Send + Sync + 'static {
    /// Spawn a child goal for `phase_id` inheriting the parent's
    /// origin and accumulating chain bookkeeping. Returns the new
    /// `GoalId` on success.
    async fn chain(
        &self,
        parent: &HookPayload,
        phase_id: &str,
    ) -> Result<GoalId, String>;

    /// Cap on chain depth. Default 5 — keeps fan-out bounded under
    /// a runaway hook bug. Implementations override when an
    /// operator widens the cap via config.
    fn max_chain_depth(&self) -> u32 {
        5
    }
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
    /// 67.F.2 — handler for `DispatchPhase` action. None disables
    /// chaining (returns `ChainingNotWired`). Production wiring
    /// will populate this with a chainer that calls the runtime
    /// `program_phase_dispatch`.
    pub chainer: Option<Arc<dyn DispatchPhaseChainer>>,
    /// PT-4 — when set, the dispatcher claims a slot in the
    /// idempotency store BEFORE running the action. A duplicate
    /// firing (NATS replay, daemon restart between decide and
    /// commit) finds the slot taken and returns
    /// `AlreadyDispatched` instead of running the side effect a
    /// second time.
    pub idempotency: Option<Arc<crate::hooks::idempotency::HookIdempotencyStore>>,
}

impl DefaultHookDispatcher {
    pub fn new(pairing: PairingAdapterRegistry, nats: Arc<dyn NatsHookPublisher>) -> Self {
        Self {
            pairing,
            nats,
            timeout: Duration::from_secs(30),
            allow_shell: false,
            chainer: None,
            idempotency: None,
        }
    }

    pub fn with_idempotency(
        mut self,
        store: Arc<crate::hooks::idempotency::HookIdempotencyStore>,
    ) -> Self {
        self.idempotency = Some(store);
        self
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    pub fn with_allow_shell(mut self, v: bool) -> Self {
        self.allow_shell = v;
        self
    }

    pub fn with_chainer(mut self, c: Arc<dyn DispatchPhaseChainer>) -> Self {
        self.chainer = Some(c);
        self
    }
}

#[async_trait]
impl HookDispatcher for DefaultHookDispatcher {
    async fn dispatch(&self, hook: &CompletionHook, payload: &HookPayload) -> Result<(), HookError> {
        // PT-4 — claim the idempotency slot atomically before
        // running the action. Duplicate firings find the slot
        // taken and skip without re-publishing.
        if let Some(store) = &self.idempotency {
            let claimed = store
                .try_claim(payload.goal_id, payload.transition, &hook.action, &hook.id)
                .await
                .map_err(|e| HookError::Idempotency(e.to_string()))?;
            if !claimed {
                return Err(HookError::AlreadyDispatched);
            }
        }
        let timeout = self.timeout;
        let fut = run_action(
            &hook.action,
            payload,
            &self.pairing,
            &self.nats,
            self.allow_shell,
            self.chainer.as_ref(),
        );
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
    chainer: Option<&Arc<dyn DispatchPhaseChainer>>,
) -> Result<(), HookError> {
    match action {
        HookAction::NotifyOrigin => {
            let Some(origin) = payload.origin.clone() else {
                return Err(HookError::MissingOrigin);
            };
            // Non-chat origins (CLI dispatch, scheduled cron jobs,
            // inbound webhooks) consume completion signals through
            // NATS / stdout, not via a chat reply. We list them
            // explicitly so the lookup never silently shadows a
            // legitimate channel id; new non-chat plugins extend
            // NON_CHAT_ORIGIN_PLUGINS until PT-9 (trait-based
            // wants_notify) lands.
            if NON_CHAT_ORIGIN_PLUGINS.contains(&origin.plugin.as_str()) {
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
        HookAction::DispatchPhase { phase_id, only_if } => {
            let Some(c) = chainer else {
                return Err(HookError::ChainingNotWired);
            };
            if !only_if.matches(payload.transition) {
                return Err(HookError::ChainGuardSkipped(
                    only_if.clone(),
                    payload.transition,
                ));
            }
            c.chain(payload, phase_id)
                .await
                .map(|_| ())
                .map_err(HookError::ChainDispatch)
        }
        HookAction::Shell { .. } if !allow_shell => Err(HookError::ShellDisabled),
        HookAction::Shell { cmd, timeout } => {
            // Render summary into the env so the script can use
            // it without the dispatcher having to template the
            // command string. JSON payload available too for
            // structured access.
            let payload_json = serde_json::to_string(payload)
                .map_err(|e| HookError::ShellIo(e.to_string()))?;
            let mut child = tokio::process::Command::new("sh");
            child
                .arg("-c")
                .arg(cmd)
                .env("NEXO_HOOK_GOAL_ID", payload.goal_id.0.to_string())
                .env("NEXO_HOOK_PHASE_ID", &payload.phase_id)
                .env("NEXO_HOOK_TRANSITION", payload.transition.as_str())
                .env("NEXO_HOOK_PAYLOAD_JSON", payload_json)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let fut = child.output();
            let out = match tokio::time::timeout(*timeout, fut).await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => return Err(HookError::ShellIo(e.to_string())),
                Err(_) => return Err(HookError::Timeout(*timeout)),
            };
            if out.status.success() {
                Ok(())
            } else {
                Err(HookError::ShellExitNonZero {
                    code: out.status.code(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                })
            }
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
