//! Phase 82.10.p — `PairingChannelTrigger` bridges admin
//! `pairing/start` to the actual channel plugin.
//!
//! Pre-82.10.p, `nexo/admin/pairing/start` only inserted a row
//! in [`PairingChallengeStore`] with `state: Pending` and never
//! told the WhatsApp plugin (or any future channel) to produce
//! a QR / pairing link. The trigger trait is the missing wire
//! — implementations spawn the channel-specific flow, push QR
//! frames into the store as they rotate, and flip the state to
//! `Linked` (or `Error`) when the user completes (or fails to
//! complete) the handshake.
//!
//! ## Lifecycle
//!
//! 1. Dispatcher receives `pairing/start` with `channel: "whatsapp"`.
//! 2. Looks up `PairingChannelTriggers["whatsapp"]`. Missing →
//!    `invalid_params: channel not supported` (no garbage row).
//! 3. `store.create_challenge` reserves an id + epoch.
//! 4. `trigger.start(ctx)` returns immediately with a
//!    [`PairingHandle`] holding a `CancellationToken`. The QR
//!    flow runs on a spawned task; `start` itself does NOT
//!    block on the user scanning.
//! 5. Dispatcher inserts the handle in its registry keyed by
//!    `challenge_id`.
//! 6. As the channel plugin rotates QRs, the trigger pushes
//!    them via `ctx.store.update_qr` + `ctx.notifier.notify_status`.
//! 7. On success / failure, the trigger flips state via
//!    `ctx.store.update_state` and exits.
//! 8. `pairing/cancel` (or TTL eviction) calls
//!    `handle.abort()` — trigger task observes
//!    `ctx.cancel.cancelled().await` and tears down the
//!    underlying client cleanly.
//!
//! ## Why a trait (and not direct plugin coupling)
//!
//! Symmetrical with [`crate::agent::admin_rpc::channel_outbound::ChannelOutboundDispatcher`]
//! (Phase 83.8.4.b) and `ChannelCredentialPersister`
//! (Phase 82.10.n). New channels (telegram-link, email-link)
//! plug in by registering a trigger; admin handler stays
//! channel-agnostic. Capability gates (`pairing_initiate`)
//! apply BEFORE trigger dispatch — same audit story.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::domains::pairing::{PairingChallengeStore, PairingNotifier};

/// Default timeout passed to the trigger when the operator
/// doesn't override. Triggers SHOULD respect this as the
/// upper bound for the entire handshake (QR generation +
/// user scan + device confirmation).
pub const PAIRING_DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// Per-channel pairing flow trigger.
///
/// Implementations are channel-scoped (one per channel id —
/// `"whatsapp"`, future `"telegram"`, `"email-link"`).
/// Dispatch sees the request's `channel` field, looks up the
/// matching trigger, and calls `start`. Channels without a
/// registered trigger surface a clean `channel_not_supported`
/// error to the operator.
#[async_trait]
pub trait PairingChannelTrigger: Send + Sync + std::fmt::Debug {
    /// Channel id this trigger handles. Stable string —
    /// matches the wire `channel` param verbatim.
    fn channel_id(&self) -> &str;

    /// Kick off the channel-specific pairing flow.
    ///
    /// Returns when the underlying client has accepted the work
    /// (NOT when the QR arrives — QR push is async via
    /// `ctx.store.update_qr`). Errors here are immediate
    /// (config invalid, instance already paired, dial failure);
    /// transient errors AFTER start arrive via
    /// `ctx.store.update_state(...)` carrying `data.error`
    /// (or via cancel + reset by the operator).
    async fn start(&self, ctx: PairingContext) -> Result<PairingHandle, PairingTriggerError>;
}

/// Everything a trigger needs to update the challenge as the
/// flow progresses + tear down cleanly.
///
/// Cloning is cheap (Arcs + small Copy fields). The trigger
/// implementation typically clones once into the spawned task.
#[derive(Clone)]
pub struct PairingContext {
    /// Stable correlation id the dispatcher generated. Used
    /// when the trigger pushes updates back into the store.
    pub challenge_id: Uuid,
    /// Agent the credential will be bound to once pairing
    /// completes. Trigger doesn't bind here — that lives with
    /// `credentials/register`. Carried for audit + future
    /// per-agent observability.
    pub agent_id: String,
    /// Optional instance discriminator. `None` for single-
    /// instance channels; trigger may resolve a default.
    pub instance: Option<String>,
    /// Store the trigger pushes QR + state updates into.
    pub store: Arc<dyn PairingChallengeStore>,
    /// Optional notifier for SSE / stdin push notifications.
    /// `None` when the broker / stdin queue is offline; trigger
    /// MUST tolerate (operators can still poll
    /// `pairing/status`).
    pub notifier: Option<Arc<dyn PairingNotifier>>,
    /// Upper bound for the whole pairing handshake. Trigger
    /// SHOULD honor — task should self-cancel past this
    /// (in addition to observing `cancel`).
    pub timeout: Duration,
    /// Cancellation source. Trigger task MUST honor — both
    /// for `pairing/cancel` and for TTL eviction. Dispatcher
    /// owns the parent token; the trigger gets a child via
    /// `CancellationToken::child_token`.
    pub cancel: CancellationToken,
}

impl std::fmt::Debug for PairingContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingContext")
            .field("challenge_id", &self.challenge_id)
            .field("agent_id", &self.agent_id)
            .field("instance", &self.instance)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Owns the spawned task. Dispatcher tracks one handle per
/// in-flight challenge; on cancel / TTL / store-eviction it
/// calls `abort()` to drop the task.
#[derive(Debug)]
pub struct PairingHandle {
    /// Same id as `PairingContext.challenge_id`.
    pub challenge_id: Uuid,
    /// Channel id the trigger reported via
    /// `PairingChannelTrigger::channel_id`. Stamped into
    /// audit + observability dumps.
    pub channel: String,
    /// Cancel token wired into the trigger task. Calling
    /// `abort` cancels it and the task should exit promptly.
    pub cancel: CancellationToken,
}

impl PairingHandle {
    /// Cancel the underlying trigger task. Idempotent — a
    /// second `abort` is a no-op (`CancellationToken::cancel`
    /// semantics).
    pub fn abort(&self) {
        self.cancel.cancel();
    }
}

/// Errors a trigger surfaces BEFORE the spawned task takes
/// over. Post-spawn errors flow through
/// `store.update_state(...)` carrying `data.error` instead.
#[derive(Debug, thiserror::Error)]
pub enum PairingTriggerError {
    /// The channel id has no trigger registered with the
    /// dispatcher. Maps to `invalid_params` at the wire.
    #[error("channel `{0}` not supported by this build")]
    ChannelNotSupported(String),
    /// Operator asked to pair an instance that already has
    /// valid creds. Forces explicit rotate before re-pair.
    #[error("instance `{0}` is already paired")]
    AlreadyPaired(String),
    /// Channel plugin needs an instance configured in its
    /// YAML; operator passed one we don't know about.
    #[error("instance `{0}` is not configured for this channel")]
    InstanceNotConfigured(String),
    /// Underlying client refused to dial / write. Maps to
    /// `internal_error`. Distinct from `update_state(Error)`
    /// — this is start-time failure, the spawn never
    /// happened.
    #[error("transport: {0}")]
    Transport(String),
    /// Catch-all wrapping arbitrary anyhow chains.
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Type alias the dispatcher accepts. Operators inject one
/// `Arc<dyn PairingChannelTrigger>` per channel id.
pub type PairingChannelTriggers = HashMap<String, Arc<dyn PairingChannelTrigger>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedChannelTrigger {
        channel: &'static str,
    }

    #[async_trait]
    impl PairingChannelTrigger for FixedChannelTrigger {
        fn channel_id(&self) -> &str {
            self.channel
        }
        async fn start(&self, ctx: PairingContext) -> Result<PairingHandle, PairingTriggerError> {
            Ok(PairingHandle {
                challenge_id: ctx.challenge_id,
                channel: self.channel.into(),
                cancel: ctx.cancel,
            })
        }
    }

    #[test]
    fn pairing_handle_abort_signals_cancel_token() {
        let cancel = CancellationToken::new();
        let handle = PairingHandle {
            challenge_id: Uuid::nil(),
            channel: "whatsapp".into(),
            cancel: cancel.clone(),
        };
        assert!(!cancel.is_cancelled());
        handle.abort();
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn pairing_handle_abort_is_idempotent() {
        let cancel = CancellationToken::new();
        let handle = PairingHandle {
            challenge_id: Uuid::nil(),
            channel: "whatsapp".into(),
            cancel: cancel.clone(),
        };
        handle.abort();
        // Second call must not panic — cancel is already set.
        cancel.cancel();
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn channel_id_returns_stable_string() {
        let trigger = FixedChannelTrigger {
            channel: "whatsapp",
        };
        assert_eq!(trigger.channel_id(), "whatsapp");
    }

    #[test]
    fn pairing_triggers_alias_is_hashmap() {
        let mut triggers: PairingChannelTriggers = HashMap::new();
        triggers.insert(
            "whatsapp".into(),
            Arc::new(FixedChannelTrigger {
                channel: "whatsapp",
            }),
        );
        assert_eq!(triggers.len(), 1);
        assert!(triggers.contains_key("whatsapp"));
        assert!(!triggers.contains_key("telegram"));
    }
}
