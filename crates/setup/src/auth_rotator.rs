//! Phase 82.10.o — production `AuthRotator` adapter.
//!
//! Persists the rotated bearer to
//! `<state_root>/secrets/operator_token.txt` (atomic rename
//! pattern, mode 0600), pushes the live
//! `nexo/notify/token_rotated` frame to connected microapp
//! listeners, and emits the durable
//! `AgentEventKind::SecurityEvent::TokenRotated` audit row on
//! the firehose broadcast so subscribers persist to SQLite.
//!
//! Operator workflow:
//!
//! 1. Operator clicks "Rotar token" in the microapp UI.
//! 2. Microapp calls `nexo/admin/auth/rotate_token { new_token?, reason? }`.
//! 3. Daemon dispatches to this adapter.
//! 4. Adapter writes new value, broadcasts notify, emits audit.
//! 5. Microapp's `LiveTokenState` listener swaps in-place.
//! 6. SPA receives 401 on next call → toast → re-login screen
//!    (M2.b.notify-spa).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::agent::admin_rpc::domains::auth::{AuthRotator, TokenRotatedNotifier};
use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::agent_events::AgentEventEmitter;
use nexo_tool_meta::admin::agent_events::{AgentEventKind, SecurityEventKind};
use nexo_tool_meta::admin::auth::{AuthRotateInput, AuthRotateResponse, REASON_MAX_LEN};
use nexo_tool_meta::http_server::TokenRotated;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

/// Number of random bytes the daemon generates when the
/// operator omits `new_token`. 32 bytes (256 bits) is far above
/// the 16-char `MIN_TOKEN_LEN` floor; URL-safe base64 stretches
/// to ~43 chars.
pub const GENERATED_TOKEN_BYTES: usize = 32;

/// Filesystem-backed implementation. Wires the SDK's stdio
/// notification multicaster + the firehose `AgentEventEmitter`
/// so a successful rotation lands BOTH the live notify AND the
/// durable audit row in one shot.
pub struct FsAuthRotator {
    /// Canonical operator token file path.
    /// `<state_root>/secrets/operator_token.txt` in production.
    token_path: PathBuf,
    /// Notifier that pushes `nexo/notify/token_rotated` frames
    /// to connected microapp listeners. Production binds an
    /// stdio sender; tests use an in-memory mock.
    notifier: Arc<dyn TokenRotatedNotifier>,
    /// Firehose emitter shared with the transcript subsystem.
    /// Production threads `BroadcastAgentEventEmitter`; tests
    /// pass `NoopAgentEventEmitter` when audit assertions
    /// aren't needed.
    audit_emitter: Arc<dyn AgentEventEmitter>,
    /// Cached current operator-token-hash so the notify
    /// payload's `old_hash` matches what microapp listeners
    /// saw last. Mutex<String> because rotations are
    /// infrequent + already serialised by the admin RPC
    /// dispatcher.
    current_hash: Mutex<String>,
}

impl std::fmt::Debug for FsAuthRotator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsAuthRotator")
            .field("token_path", &self.token_path)
            .finish_non_exhaustive()
    }
}

impl FsAuthRotator {
    /// Build a new rotator. `initial_hash` is the operator-
    /// token-hash the daemon computed at boot from the env
    /// var; it's the value the very first rotation will use as
    /// `old_hash`.
    pub fn new(
        token_path: PathBuf,
        notifier: Arc<dyn TokenRotatedNotifier>,
        audit_emitter: Arc<dyn AgentEventEmitter>,
        initial_hash: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            token_path,
            notifier,
            audit_emitter,
            current_hash: Mutex::new(initial_hash),
        })
    }
}

#[async_trait]
impl AuthRotator for FsAuthRotator {
    async fn rotate(
        &self,
        input: AuthRotateInput,
    ) -> Result<AuthRotateResponse, AdminRpcError> {
        // 1. Resolve the new value (operator-supplied or daemon-generated).
        let new_token = match input.new_token {
            Some(t) => t,
            None => generate_url_safe(GENERATED_TOKEN_BYTES),
        };
        let new_hash = token_hash_16(&new_token);

        // 2. Pull cached hash for the notify `old_hash` field
        //    + replace it atomically with the new one. Holding
        //    the mutex across the file write keeps concurrent
        //    rotations sequential (admin RPC dispatcher
        //    already does, but defence-in-depth).
        let mut guard = self.current_hash.lock().await;
        let old_hash = guard.clone();

        // 3. Atomic file write (mode 0600 on unix).
        write_atomic_secret(&self.token_path, new_token.as_bytes()).map_err(|e| {
            AdminRpcError::Internal(format!(
                "persist new operator token to {}: {}",
                self.token_path.display(),
                e
            ))
        })?;

        // 4. Update cached hash AFTER successful persist so a
        //    failed write doesn't poison the in-memory state.
        *guard = new_hash.clone();
        drop(guard);

        // 5. Broadcast the live notify to microapp listeners.
        //    Errors logged inside the impl; we never propagate.
        self.notifier.notify_token_rotated(&TokenRotated {
            old_hash: old_hash.clone(),
            new: new_token.clone(),
        });

        // 6. Emit the durable audit row onto the firehose.
        let at_ms = now_ms();
        let reason = input
            .reason
            .map(|r| truncate_chars(&r, REASON_MAX_LEN));
        self.audit_emitter
            .emit(AgentEventKind::SecurityEvent {
                event: SecurityEventKind::TokenRotated {
                    at_ms,
                    prev_hash: old_hash,
                    new_hash: new_hash.clone(),
                    reason,
                },
            })
            .await;

        Ok(AuthRotateResponse {
            ok: true,
            new_hash,
            at_ms,
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// SHA-256 hex of `token`, truncated to 16 chars (8 bytes).
/// Byte-parity with `nexo_setup::http_supervisor::token_hash`.
fn token_hash_16(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut s = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Generate a URL-safe random token of the given byte length.
/// Avoids `+` / `/` / `=` chars so the value is safe to drop
/// into env vars + URLs without quoting. Source of randomness
/// is `getrandom` via `uuid::Uuid::new_v4` repeated as needed
/// — already in the dep tree, no extra crate required.
fn generate_url_safe(bytes_wanted: usize) -> String {
    use std::fmt::Write;
    // Each Uuid yields 16 random bytes → ceil(bytes_wanted / 16) Uuids.
    let needed_uuids = (bytes_wanted + 15) / 16;
    let mut raw = Vec::with_capacity(needed_uuids * 16);
    for _ in 0..needed_uuids {
        raw.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    raw.truncate(bytes_wanted);
    // Hex encode — gives 2x the bytes_wanted in chars,
    // url-safe by construction. Avoids base64 to skip a crate.
    let mut out = String::with_capacity(raw.len() * 2);
    for b in raw {
        write!(&mut out, "{b:02x}").expect("hex encode never fails");
    }
    out
}

/// Atomic write with restrictive permissions on unix.
/// Same pattern the existing `write_atomic_bytes` adapter uses
/// for credential payloads. Inlined so the auth rotator stays
/// self-contained.
fn write_atomic_secret(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "operator token path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        use std::io::Write;
        let mut f = tmp.as_file();
        f.write_all(body)?;
        f.flush()?;
        f.sync_all().ok();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.persist(path).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("persist: {e}"))
    })?;
    Ok(())
}

/// Char-bounded truncation. The wire shape's `reason` field is
/// capped to `REASON_MAX_LEN` chars, not bytes — UTF-8 truncation
/// at byte boundary mid-glyph would corrupt the audit row.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

fn now_ms() -> u64 {
    use chrono::Utc;
    Utc::now().timestamp_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Captures `notify_token_rotated` invocations so tests can
    /// assert the rotator wired the live notify.
    #[derive(Default)]
    struct CaptureNotifier {
        calls: StdMutex<Vec<TokenRotated>>,
    }

    impl TokenRotatedNotifier for CaptureNotifier {
        fn notify_token_rotated(&self, payload: &TokenRotated) {
            self.calls.lock().unwrap().push(payload.clone());
        }
    }

    /// Captures `emit` invocations so audit assertions don't
    /// need a full broadcast channel.
    #[derive(Default)]
    struct CaptureEmitter {
        calls: tokio::sync::Mutex<Vec<AgentEventKind>>,
    }

    impl std::fmt::Debug for CaptureEmitter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CaptureEmitter").finish_non_exhaustive()
        }
    }

    #[async_trait]
    impl AgentEventEmitter for CaptureEmitter {
        async fn emit(&self, event: AgentEventKind) {
            self.calls.lock().await.push(event);
        }
    }

    fn make_rotator(
        token_path: PathBuf,
    ) -> (Arc<FsAuthRotator>, Arc<CaptureNotifier>, Arc<CaptureEmitter>) {
        let notifier = Arc::new(CaptureNotifier::default());
        let emitter = Arc::new(CaptureEmitter::default());
        let r = FsAuthRotator::new(
            token_path,
            notifier.clone(),
            emitter.clone(),
            // initial hash, simulates token computed from env var at boot.
            "initialhash00000".into(),
        );
        (r, notifier, emitter)
    }

    #[tokio::test]
    async fn rotate_with_supplied_token_writes_file_and_returns_new_hash() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("operator_token.txt");
        let (r, _notifier, _emitter) = make_rotator(path.clone());

        let resp = r
            .rotate(AuthRotateInput {
                new_token: Some("supplied-bearer-32-chars-long".into()),
                reason: None,
            })
            .await
            .expect("rotate ok");

        assert!(resp.ok);
        assert_eq!(resp.new_hash.len(), 16);
        assert!(resp.new_hash.chars().all(|c| c.is_ascii_hexdigit()));

        // File written with operator-supplied bytes.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "supplied-bearer-32-chars-long");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rotate_writes_file_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("operator_token.txt");
        let (r, _notifier, _emitter) = make_rotator(path.clone());

        r.rotate(AuthRotateInput {
            new_token: Some("perm-test-bearer-token-32".into()),
            reason: None,
        })
        .await
        .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        // Mask off file-type bits, keep permission bits.
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn rotate_without_supplied_token_generates_random() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("operator_token.txt");
        let (r, _notifier, _emitter) = make_rotator(path.clone());

        let resp = r
            .rotate(AuthRotateInput {
                new_token: None,
                reason: None,
            })
            .await
            .unwrap();

        // 32 bytes hex-encoded → 64 chars.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk.len(), 64);
        assert!(on_disk.chars().all(|c| c.is_ascii_hexdigit()));
        // Returned hash is the SHA-256 prefix of the generated value.
        assert_eq!(resp.new_hash, token_hash_16(&on_disk));
    }

    #[tokio::test]
    async fn rotate_pushes_notify_with_old_and_new_pair() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("operator_token.txt");
        let (r, notifier, _emitter) = make_rotator(path);

        r.rotate(AuthRotateInput {
            new_token: Some("notify-test-bearer-token-32".into()),
            reason: None,
        })
        .await
        .unwrap();

        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly one notify per rotation");
        assert_eq!(calls[0].old_hash, "initialhash00000");
        assert_eq!(calls[0].new, "notify-test-bearer-token-32");
    }

    #[tokio::test]
    async fn rotate_emits_audit_event_with_reason_truncated() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("operator_token.txt");
        let (r, _notifier, emitter) = make_rotator(path);

        let long_reason = "x".repeat(REASON_MAX_LEN + 50);
        r.rotate(AuthRotateInput {
            new_token: Some("audit-test-bearer-token-32".into()),
            reason: Some(long_reason.clone()),
        })
        .await
        .unwrap();

        let calls = emitter.calls.lock().await;
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            AgentEventKind::SecurityEvent {
                event:
                    SecurityEventKind::TokenRotated {
                        prev_hash,
                        new_hash,
                        reason,
                        ..
                    },
            } => {
                assert_eq!(prev_hash, "initialhash00000");
                assert_eq!(new_hash.len(), 16);
                assert_eq!(
                    reason.as_deref().unwrap().chars().count(),
                    REASON_MAX_LEN,
                    "reason capped to wire-shape limit"
                );
            }
            other => panic!("expected SecurityEvent::TokenRotated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn second_rotate_uses_first_rotates_hash_as_old_hash() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("operator_token.txt");
        let (r, notifier, _emitter) = make_rotator(path);

        let first = r
            .rotate(AuthRotateInput {
                new_token: Some("first-rotation-bearer-token".into()),
                reason: None,
            })
            .await
            .unwrap();

        r.rotate(AuthRotateInput {
            new_token: Some("second-rotation-bearer-toke".into()),
            reason: None,
        })
        .await
        .unwrap();

        let calls = notifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        // Second notify's old_hash equals first's new_hash.
        assert_eq!(calls[1].old_hash, first.new_hash);
        assert_eq!(calls[0].old_hash, "initialhash00000");
    }

    #[test]
    fn token_hash_16_matches_supervisor_signature() {
        // Byte-parity sanity check — same algorithm as
        // `nexo_setup::http_supervisor::token_hash`. Cross-
        // verify against the supervisor helper at runtime so a
        // future change to either function breaks loud here.
        use crate::http_supervisor::token_hash as supervisor_hash;
        for sample in &["super-secret", "abc", "x", "hello world"] {
            assert_eq!(
                token_hash_16(sample),
                supervisor_hash(sample),
                "byte-parity violation for input {sample:?}"
            );
        }
        let h = token_hash_16("super-secret");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
