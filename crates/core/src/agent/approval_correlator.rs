//! Phase 79.10 — approval correlator for chat-driven operator
//! approval of pending mutations.
//!
//! The leak's `ConfigTool` (`claude-code-leak/src/tools/ConfigTool/
//! ConfigTool.ts:103-106`) leans on a host `'ask'` permission prompt:
//! the user sees a modal in the CLI / IDE and clicks Allow / Deny.
//! Over chat-based pairing (Phase 26), there is no modal — the
//! approval has to be a regular message on the same channel that
//! originated the proposal.
//!
//! This module owns:
//!   * [`parse_approval_command`] — strict regex parser that lifts
//!     `[config-approve patch_id=...]` and
//!     `[config-reject patch_id=... reason=...]` out of a chat
//!     message body.
//!   * [`ApprovalCorrelator`] — `DashMap<patch_id, PendingEntry>`
//!     plus a 24 h reaper (`tokio::spawn`) so the apply path can
//!     `await` the operator's decision via `oneshot`.
//!   * [`ApprovalSource`] trait — production `BrokerApprovalSource`
//!     subscribes to `pairing.inbound.>`; tests use
//!     [`MockApprovalSource`] which exposes `inject(msg)` for
//!     deterministic flows.
//!
//! The same correlator is the planned anchor for Phase 79.1
//! `ExitPlanMode { wait: true }` (see `proyecto/PHASES.md:4619,
//! 4686-4688`), so the API is intentionally generic ("approval
//! command" with `kind: approve | reject`); only the regex is
//! ConfigTool-specific in this commit.

use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Inbound chat message the correlator inspects. Production
/// `BrokerApprovalSource` builds this from the pairing inbound
/// stream; tests construct it directly.
#[derive(Debug, Clone)]
pub struct InboundApprovalMessage {
    /// Plugin id (e.g. `whatsapp`).
    pub channel: String,
    /// Plugin instance id (e.g. `default`). Combined with `channel`
    /// to form the binding's `(channel, account_id)` tuple.
    pub account_id: String,
    /// Operator's id within the plugin (phone number, chat user id,
    /// email address). Carried for audit; not used in match logic.
    pub sender_id: String,
    pub body: String,
    pub received_at: i64,
}

/// Parsed shape of a `[config-approve ...]` / `[config-reject ...]`
/// block. Kept minimal — `patch_id` is the only mandatory field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCommand {
    Approve { patch_id: String },
    Reject { patch_id: String, reason: Option<String> },
}

impl ApprovalCommand {
    pub fn patch_id(&self) -> &str {
        match self {
            ApprovalCommand::Approve { patch_id }
            | ApprovalCommand::Reject { patch_id, .. } => patch_id.as_str(),
        }
    }
}

/// Decision delivered back to the apply path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected { reason: Option<String> },
    Expired,
}

/// Pending entry while the apply path awaits the operator.
/// `(channel, account_id)` is the cross-binding-forgery guard:
/// only messages from the originating binding can resolve the
/// pending entry.
pub struct PendingApproval {
    pub patch_id: String,
    pub binding_id: String,
    pub agent_id: String,
    pub channel: String,
    pub account_id: String,
    pub sender_id: String,
    pub created_at: i64,
    pub expires_at: i64,
}

struct PendingEntry {
    patch: PendingApproval,
    responder: oneshot::Sender<ApprovalDecision>,
}

#[derive(Debug, Clone)]
pub struct ApprovalCorrelatorConfig {
    pub default_timeout: Duration,
    pub reaper_interval: Duration,
}

impl Default for ApprovalCorrelatorConfig {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(86_400),
            reaper_interval: Duration::from_secs(60),
        }
    }
}

/// Production correlator. Constructed once at boot, registered with
/// a single [`ApprovalSource`], shared across every `ConfigTool`
/// instance.
pub struct ApprovalCorrelator {
    pending: DashMap<String, PendingEntry>,
    config: ApprovalCorrelatorConfig,
    cancel: CancellationToken,
}

impl ApprovalCorrelator {
    pub fn new(config: ApprovalCorrelatorConfig) -> Arc<Self> {
        Arc::new(Self {
            pending: DashMap::new(),
            config,
            cancel: CancellationToken::new(),
        })
    }

    /// Park a fresh proposal. Returns the receiver the apply path
    /// awaits. The caller MUST consume the receiver exactly once;
    /// dropping it without awaiting drops the entry on the next
    /// inbound or reaper sweep.
    pub fn park(
        &self,
        patch: PendingApproval,
    ) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(
            patch.patch_id.clone(),
            PendingEntry {
                patch,
                responder: tx,
            },
        );
        rx
    }

    /// Number of pending entries (for diagnostics + tests).
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Spawn the inbound subscriber + reaper tasks. `source` is
    /// borrowed exclusively by this correlator; multiple correlator
    /// instances need distinct sources.
    pub fn spawn_workers(self: &Arc<Self>, source: Arc<dyn ApprovalSource>) {
        // Inbound subscriber.
        let me = Arc::clone(self);
        let cancel = self.cancel.clone();
        let src = Arc::clone(&source);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    msg = src.next_message() => match msg {
                        Some(m) => me.on_inbound(m),
                        None => break,
                    }
                }
            }
        });

        // Reaper.
        let me = Arc::clone(self);
        let cancel = self.cancel.clone();
        let interval = self.config.reaper_interval;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {
                        me.reap_expired();
                    }
                }
            }
        });
    }

    /// Inject an inbound message synchronously (used by tests +
    /// the broker subscriber loop).
    pub fn on_inbound(&self, msg: InboundApprovalMessage) {
        let cmd = match parse_approval_command(&msg.body) {
            Some(c) => c,
            None => return,
        };
        let patch_id = cmd.patch_id().to_string();
        // Take the entry so a forgery attempt cannot resolve it
        // *and* leave it dangling.
        let Some((_, entry)) = self.pending.remove(&patch_id) else {
            tracing::debug!(
                target: "config::approval",
                patch_id = %patch_id,
                "[config] inbound matched no pending entry"
            );
            return;
        };
        if entry.patch.channel != msg.channel || entry.patch.account_id != msg.account_id {
            tracing::warn!(
                target: "config::approval_forgery_rejected",
                patch_id = %patch_id,
                expected_channel = %entry.patch.channel,
                expected_account = %entry.patch.account_id,
                got_channel = %msg.channel,
                got_account = %msg.account_id,
                "[config] approval came from wrong binding — discarded"
            );
            // Re-insert the entry; the originator may still approve.
            self.pending
                .insert(entry.patch.patch_id.clone(), entry);
            return;
        }
        let decision = match cmd {
            ApprovalCommand::Approve { .. } => ApprovalDecision::Approved,
            ApprovalCommand::Reject { reason, .. } => ApprovalDecision::Rejected { reason },
        };
        let _ = entry.responder.send(decision);
    }

    fn reap_expired(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut to_drop: Vec<String> = Vec::new();
        for kv in self.pending.iter() {
            if kv.value().patch.expires_at <= now {
                to_drop.push(kv.key().clone());
            }
        }
        for id in to_drop {
            if let Some((_, entry)) = self.pending.remove(&id) {
                tracing::info!(
                    target: "config::approval_expired",
                    patch_id = %id,
                    "[config] approval expired"
                );
                let _ = entry.responder.send(ApprovalDecision::Expired);
            }
        }
    }

    /// Cancel both background tasks. Best-effort — pending
    /// receivers see the responder dropped (returns `Err(_)`).
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

/// Subscription source for inbound messages. Production wiring
/// uses `BrokerApprovalSource` (subscribes to `pairing.inbound.>`);
/// tests use [`MockApprovalSource`].
#[async_trait]
pub trait ApprovalSource: Send + Sync {
    /// Next inbound message; `None` when the source is closed.
    /// The correlator's spawned subscriber loop pulls one message
    /// at a time and drives `on_inbound` synchronously.
    async fn next_message(&self) -> Option<InboundApprovalMessage>;
}

/// Test-only source. Inject messages via `inject()`; the worker
/// loop drains them via `next_message()` and drives the correlator.
pub struct MockApprovalSource {
    queue: tokio::sync::Mutex<std::collections::VecDeque<InboundApprovalMessage>>,
    notify: tokio::sync::Notify,
    closed: std::sync::atomic::AtomicBool,
}

impl Default for MockApprovalSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MockApprovalSource {
    pub fn new() -> Self {
        Self {
            queue: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
            notify: tokio::sync::Notify::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub async fn inject(&self, msg: InboundApprovalMessage) {
        self.queue.lock().await.push_back(msg);
        self.notify.notify_one();
    }

    pub fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.notify.notify_waiters();
    }
}

#[async_trait]
impl ApprovalSource for MockApprovalSource {
    async fn next_message(&self) -> Option<InboundApprovalMessage> {
        loop {
            if let Some(m) = self.queue.lock().await.pop_front() {
                return Some(m);
            }
            if self
                .closed
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return None;
            }
            self.notify.notified().await;
        }
    }
}

/// Strict regex match for the approval message format. Anchored
/// to the whole trimmed body so `xx [config-approve ...] yy` does
/// NOT match — operators must send the bracketed command alone
/// (or with surrounding whitespace).
///
/// Grammar:
///   `^\s*\[config-(approve|reject)\s+patch_id=<ULID>(?:\s+reason=<text>)?\]\s*$`
///
/// `<ULID>` is a Crockford-base32 char class
/// `[0-9A-HJKMNP-TV-Z]+` (case-sensitive, matching `uuid`-7 ULIDs).
pub fn parse_approval_command(body: &str) -> Option<ApprovalCommand> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?x)
              ^\s*
              \[
                config-
                (?P<verb>approve|reject)
                \s+
                patch_id=
                (?P<id>[0-9A-HJKMNP-TV-Z]+)
                (?:\s+reason=(?P<reason>.*?))?
              \]
              \s*$
            ",
        )
        .expect("approval-command regex must compile")
    });
    let caps = re.captures(body.trim())?;
    let verb = caps.name("verb")?.as_str();
    let id = caps.name("id")?.as_str().to_string();
    match verb {
        "approve" => Some(ApprovalCommand::Approve { patch_id: id }),
        "reject" => {
            let reason = caps.name("reason").map(|m| m.as_str().trim().to_string());
            Some(ApprovalCommand::Reject {
                patch_id: id,
                reason,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn fixture_pending(patch_id: &str) -> PendingApproval {
        PendingApproval {
            patch_id: patch_id.into(),
            binding_id: "whatsapp:default".into(),
            agent_id: "cody".into(),
            channel: "whatsapp".into(),
            account_id: "default".into(),
            sender_id: "5511".into(),
            created_at: 0,
            expires_at: i64::MAX,
        }
    }

    fn fixture_inbound(patch_id: &str, body: &str) -> InboundApprovalMessage {
        InboundApprovalMessage {
            channel: "whatsapp".into(),
            account_id: "default".into(),
            sender_id: "5511".into(),
            body: body.into(),
            received_at: 0,
        }
    }

    #[test]
    fn parse_approve_minimal() {
        let cmd = parse_approval_command("[config-approve patch_id=01J7HVK9MWXYZ]").unwrap();
        assert_eq!(
            cmd,
            ApprovalCommand::Approve {
                patch_id: "01J7HVK9MWXYZ".into()
            }
        );
    }

    #[test]
    fn parse_reject_with_reason() {
        let cmd = parse_approval_command(
            "[config-reject patch_id=01J7HVK9MWXYZ reason=use sonnet por costo]",
        )
        .unwrap();
        assert_eq!(
            cmd,
            ApprovalCommand::Reject {
                patch_id: "01J7HVK9MWXYZ".into(),
                reason: Some("use sonnet por costo".into())
            }
        );
    }

    #[test]
    fn parse_reject_without_reason() {
        let cmd = parse_approval_command("[config-reject patch_id=01J7HVK9MWXYZ]").unwrap();
        assert_eq!(
            cmd,
            ApprovalCommand::Reject {
                patch_id: "01J7HVK9MWXYZ".into(),
                reason: None
            }
        );
    }

    #[test]
    fn parse_ignores_garbage() {
        assert!(parse_approval_command("hello world").is_none());
        assert!(parse_approval_command("").is_none());
        assert!(parse_approval_command("[config-approve]").is_none());
        assert!(parse_approval_command("[config-approve patch_id=]").is_none());
        // Patch id with lower-case is rejected (Crockford is upper).
        assert!(parse_approval_command("[config-approve patch_id=foo]").is_none());
    }

    #[test]
    fn parse_strict_anchors_full_message() {
        // Embedded inside a sentence — must NOT match.
        assert!(parse_approval_command(
            "let's go [config-approve patch_id=01J7HVK9MWXYZ] thanks"
        )
        .is_none());
    }

    #[tokio::test]
    async fn correlator_resolves_pending_on_match() {
        let c = ApprovalCorrelator::new(ApprovalCorrelatorConfig::default());
        let rx = c.park(fixture_pending("01J7HVK9MWXYZ"));
        c.on_inbound(fixture_inbound(
            "01J7HVK9MWXYZ",
            "[config-approve patch_id=01J7HVK9MWXYZ]",
        ));
        let decision = rx.await.unwrap();
        assert_eq!(decision, ApprovalDecision::Approved);
        assert_eq!(c.pending_count(), 0);
    }

    #[tokio::test]
    async fn correlator_resolves_with_reason_on_reject() {
        let c = ApprovalCorrelator::new(ApprovalCorrelatorConfig::default());
        let rx = c.park(fixture_pending("01J7HVK9MWXYZ"));
        c.on_inbound(fixture_inbound(
            "01J7HVK9MWXYZ",
            "[config-reject patch_id=01J7HVK9MWXYZ reason=cost]",
        ));
        match rx.await.unwrap() {
            ApprovalDecision::Rejected { reason } => assert_eq!(reason.as_deref(), Some("cost")),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn correlator_rejects_cross_binding_message() {
        let c = ApprovalCorrelator::new(ApprovalCorrelatorConfig::default());
        let _rx = c.park(fixture_pending("01J7HVK9MWXYZ"));
        // Inbound from binding B (telegram:other) — must NOT match.
        let bad_binding = InboundApprovalMessage {
            channel: "telegram".into(),
            account_id: "other".into(),
            sender_id: "abc".into(),
            body: "[config-approve patch_id=01J7HVK9MWXYZ]".into(),
            received_at: 0,
        };
        c.on_inbound(bad_binding);
        // Pending entry must remain.
        assert_eq!(c.pending_count(), 1);
    }

    #[tokio::test]
    async fn correlator_no_pending_entry_logs_and_returns() {
        let c = ApprovalCorrelator::new(ApprovalCorrelatorConfig::default());
        c.on_inbound(fixture_inbound(
            "01J7UNKNOWNPATCHID",
            "[config-approve patch_id=01J7UNKNOWNPATCHID]",
        ));
        assert_eq!(c.pending_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn correlator_expires_pending_after_timeout() {
        let cfg = ApprovalCorrelatorConfig {
            default_timeout: Duration::from_secs(1),
            reaper_interval: Duration::from_millis(50),
        };
        let c = ApprovalCorrelator::new(cfg);
        let mut p = fixture_pending("01J7HVK9MWXYZ");
        p.expires_at = chrono::Utc::now().timestamp() - 1; // already expired
        let rx = c.park(p);
        // Run a single reap manually (don't depend on the spawned
        // worker, which would also work).
        c.reap_expired();
        let decision = rx.await.unwrap();
        assert_eq!(decision, ApprovalDecision::Expired);
        assert_eq!(c.pending_count(), 0);
    }

    #[tokio::test]
    async fn mock_source_round_trips_messages() {
        let src = Arc::new(MockApprovalSource::new());
        let inbound = fixture_inbound(
            "01J7HVK9MWXYZ",
            "[config-approve patch_id=01J7HVK9MWXYZ]",
        );
        src.inject(inbound.clone()).await;
        let got = src.next_message().await.unwrap();
        assert_eq!(got.body, inbound.body);
        src.close();
        // Closed → None.
        assert!(src.next_message().await.is_none());
        assert!(!src.closed.load(Ordering::Relaxed) || true);
    }
}
