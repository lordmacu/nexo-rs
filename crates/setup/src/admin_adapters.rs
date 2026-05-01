//! Phase 82.10.h.3 — production adapters wiring the core admin
//! RPC domain traits to the on-disk yaml + secrets surfaces.
//!
//! Each adapter resolves the cycle that motivated its trait:
//! `nexo-core` declares the trait, `nexo-setup` implements it (and
//! is the only crate that may depend on `crate::yaml_patch`).
//!
//! Wired in main.rs via `AdminRpcDispatcher::with_*_domain`:
//!
//! ```ignore
//! let yaml = AgentsYamlPatcher::new(config.join("agents.yaml"));
//! let llm  = LlmYamlPatcherFs::new(config.join("llm.yaml"));
//! let cred = FilesystemCredentialStore::new(secrets_root);
//! dispatcher
//!     .with_agents_domain(yaml.clone())
//!     .with_credentials_domain(yaml, cred)
//!     .with_llm_providers_domain(llm);
//! ```
//!
//! Pairing challenge store + notifier are deferred to 82.10.h.b
//! because both need new infrastructure (a fresh challenge SQLite
//! schema for the QR state machine + a stdio-bridged notifier
//! that ties into the main JSON-RPC writer in src/main.rs).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use async_trait::async_trait;
use nexo_core::agent::admin_rpc::domains::agent_events::TranscriptReader;
use nexo_core::agent::admin_rpc::domains::agents::YamlPatcher;
use nexo_core::agent::admin_rpc::domains::credentials::CredentialStore;
use nexo_core::agent::admin_rpc::domains::llm_providers::LlmYamlPatcher;
use nexo_core::agent::admin_rpc::domains::pairing::{
    PairingChallengeStore, PairingNotifier, PAIRING_STATUS_NOTIFY_METHOD,
};
use nexo_core::agent::transcripts::{TranscriptLine, TranscriptWriter};
use nexo_core::agent::transcripts_index::TranscriptsIndex;
use nexo_tool_meta::admin::agent_events::{
    AgentEventKind, AgentEventsListFilter, AgentEventsReadParams, AgentEventsSearchParams,
    SearchHit, TranscriptRole as WireTranscriptRole,
};
use nexo_core::agent::admin_rpc::AdminOutboundWriter;
use nexo_tool_meta::admin::pairing::{PairingState, PairingStatus, PairingStatusData};
use std::sync::OnceLock;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::yaml_patch;

/// `agents.yaml` adapter for the agents + credentials admin
/// domains. Holds the path; every method opens the file fresh so
/// the writer stays consistent with operator-side edits made
/// outside the admin RPC surface (e.g. wizard, hand edits).
#[derive(Debug, Clone)]
pub struct AgentsYamlPatcher {
    path: PathBuf,
}

impl AgentsYamlPatcher {
    /// Path to the operator-managed `agents.yaml`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl YamlPatcher for AgentsYamlPatcher {
    fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        yaml_patch::list_agent_ids(&self.path)
    }

    fn read_agent_field(
        &self,
        agent_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        match yaml_patch::read_agent_field(&self.path, agent_id, dotted)? {
            Some(yaml_value) => Ok(Some(yaml_to_json(yaml_value)?)),
            None => Ok(None),
        }
    }

    fn upsert_agent_field(
        &self,
        agent_id: &str,
        dotted: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let yaml_value = json_to_yaml(value)?;
        yaml_patch::upsert_agent_field(&self.path, agent_id, dotted, yaml_value)
    }

    fn remove_agent(&self, agent_id: &str) -> anyhow::Result<()> {
        yaml_patch::remove_agent_block(&self.path, agent_id)?;
        Ok(())
    }
}

/// `llm.yaml` adapter for the llm_providers admin domain. Same
/// fresh-open semantics as the agents adapter.
#[derive(Debug, Clone)]
pub struct LlmYamlPatcherFs {
    path: PathBuf,
}

impl LlmYamlPatcherFs {
    /// Path to the operator-managed `llm.yaml`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl LlmYamlPatcher for LlmYamlPatcherFs {
    fn list_provider_ids(&self) -> anyhow::Result<Vec<String>> {
        yaml_patch::list_llm_provider_ids(&self.path)
    }

    fn read_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        match yaml_patch::read_llm_provider_field(&self.path, provider_id, dotted)? {
            Some(yaml_value) => Ok(Some(yaml_to_json(yaml_value)?)),
            None => Ok(None),
        }
    }

    fn upsert_provider_field(
        &self,
        provider_id: &str,
        dotted: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let yaml_value = json_to_yaml(value)?;
        yaml_patch::upsert_llm_provider_field(&self.path, provider_id, dotted, yaml_value)
    }

    fn remove_provider(&self, provider_id: &str) -> anyhow::Result<()> {
        yaml_patch::remove_llm_provider(&self.path, provider_id)?;
        Ok(())
    }
}

/// Filesystem-backed `CredentialStore` rooted at `secrets/`.
/// Layout: `secrets/<channel>/[<instance>/]payload.json`. The
/// `<instance>` segment is omitted for `instance = None`. Atomic
/// writes via `tempfile::NamedTempFile::persist` so a partial
/// crash never leaves a half-written payload on disk.
#[derive(Debug, Clone)]
pub struct FilesystemCredentialStore {
    secrets_root: PathBuf,
}

impl FilesystemCredentialStore {
    /// Build a store rooted at `secrets_root`. Created on first
    /// write — does not fail when the directory is absent.
    pub fn new(secrets_root: impl Into<PathBuf>) -> Self {
        Self {
            secrets_root: secrets_root.into(),
        }
    }

    fn payload_path(&self, channel: &str, instance: Option<&str>) -> PathBuf {
        let mut p = self.secrets_root.join(channel);
        if let Some(i) = instance {
            p.push(i);
        }
        p.push("payload.json");
        p
    }
}

impl CredentialStore for FilesystemCredentialStore {
    fn list_credentials(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
        if !self.secrets_root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for channel_entry in std::fs::read_dir(&self.secrets_root)? {
            let channel_entry = channel_entry?;
            if !channel_entry.file_type()?.is_dir() {
                continue;
            }
            let channel = channel_entry.file_name().to_string_lossy().into_owned();
            // Direct payload (no instance).
            if channel_entry.path().join("payload.json").exists() {
                out.push((channel.clone(), None));
            }
            // Per-instance payloads.
            for inst_entry in std::fs::read_dir(channel_entry.path())? {
                let inst_entry = inst_entry?;
                if !inst_entry.file_type()?.is_dir() {
                    continue;
                }
                if inst_entry.path().join("payload.json").exists() {
                    let instance = inst_entry.file_name().to_string_lossy().into_owned();
                    out.push((channel.clone(), Some(instance)));
                }
            }
        }
        Ok(out)
    }

    fn write_credential(
        &self,
        channel: &str,
        instance: Option<&str>,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        if channel.is_empty() {
            anyhow::bail!("channel must not be empty");
        }
        let target = self.payload_path(channel, instance);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(payload)?;
        write_atomic_bytes(&target, &body)
    }

    fn delete_credential(
        &self,
        channel: &str,
        instance: Option<&str>,
    ) -> anyhow::Result<bool> {
        let target = self.payload_path(channel, instance);
        if !target.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&target)?;
        // Best-effort dir prune so `list_credentials` doesn't see
        // the empty parent. Ignore errors — non-empty dirs are
        // legitimate.
        if let Some(parent) = target.parent() {
            let _ = std::fs::remove_dir(parent);
            if let Some(channel_dir) = parent.parent() {
                if channel_dir != self.secrets_root {
                    let _ = std::fs::remove_dir(channel_dir);
                }
            }
        }
        Ok(true)
    }
}

// ── Internal helpers ────────────────────────────────────────────

fn yaml_to_json(yaml_value: serde_yaml::Value) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(yaml_value).map_err(Into::into)
}

fn json_to_yaml(json_value: serde_json::Value) -> anyhow::Result<serde_yaml::Value> {
    serde_yaml::to_value(json_value).map_err(Into::into)
}

fn write_atomic_bytes(path: &Path, body: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!("credential payload path has no parent: {}", path.display())
    })?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        use std::io::Write;
        let mut f = tmp.as_file();
        f.write_all(body)?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("persist credential {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_yaml(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn agents_adapter_lists_ids_from_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n  - id: bob\n    model:\n      provider: anthropic\n",
        );
        let adapter = AgentsYamlPatcher::new(path);
        let mut ids = adapter.list_agent_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["ana".to_string(), "bob".into()]);
    }

    #[test]
    fn agents_adapter_returns_empty_list_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = AgentsYamlPatcher::new(dir.path().join("missing.yaml"));
        assert!(adapter.list_agent_ids().unwrap().is_empty());
    }

    #[test]
    fn agents_adapter_read_and_upsert_field_roundtrip_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n      model: M\n",
        );
        let adapter = AgentsYamlPatcher::new(path);
        let v = adapter
            .read_agent_field("ana", "model.provider")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("minimax"));

        adapter
            .upsert_agent_field("ana", "language", json!("es"))
            .unwrap();
        let lang = adapter
            .read_agent_field("ana", "language")
            .unwrap()
            .unwrap();
        assert_eq!(lang.as_str(), Some("es"));

        // Round-trip a complex JSON object (inbound_bindings).
        let bindings = json!([{ "plugin": "whatsapp", "instance": "personal" }]);
        adapter
            .upsert_agent_field("ana", "inbound_bindings", bindings.clone())
            .unwrap();
        let read_back = adapter
            .read_agent_field("ana", "inbound_bindings")
            .unwrap()
            .unwrap();
        assert_eq!(read_back, bindings);
    }

    #[test]
    fn agents_adapter_remove_agent_drops_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n  - id: bob\n    model:\n      provider: anthropic\n",
        );
        let adapter = AgentsYamlPatcher::new(path);
        adapter.remove_agent("ana").unwrap();
        assert_eq!(adapter.list_agent_ids().unwrap(), vec!["bob".to_string()]);
        // Idempotent re-removal.
        adapter.remove_agent("ana").unwrap();
        assert_eq!(adapter.list_agent_ids().unwrap(), vec!["bob".to_string()]);
    }

    #[test]
    fn llm_adapter_lists_provider_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  minimax:\n    base_url: https://x\n  anthropic:\n    base_url: https://y\n",
        );
        let adapter = LlmYamlPatcherFs::new(path);
        let mut ids = adapter.list_provider_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["anthropic".to_string(), "minimax".into()]);
    }

    #[test]
    fn llm_adapter_upsert_creates_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.yaml");
        let adapter = LlmYamlPatcherFs::new(path.clone());
        adapter
            .upsert_provider_field("newp", "base_url", json!("https://x"))
            .unwrap();
        adapter
            .upsert_provider_field("newp", "api_key_env", json!("MY_KEY"))
            .unwrap();
        let v = adapter
            .read_provider_field("newp", "api_key_env")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("MY_KEY"));
    }

    #[test]
    fn llm_adapter_remove_provider_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  retired:\n    base_url: https://x\n",
        );
        let adapter = LlmYamlPatcherFs::new(path);
        adapter.remove_provider("retired").unwrap();
        adapter.remove_provider("retired").unwrap();
        assert!(adapter.list_provider_ids().unwrap().is_empty());
    }

    #[test]
    fn credential_store_write_then_read_back_via_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        store
            .write_credential("whatsapp", Some("personal"), &json!({ "token": "wa.X" }))
            .unwrap();
        store
            .write_credential("whatsapp", Some("business"), &json!({ "token": "wa.Y" }))
            .unwrap();
        let mut creds = store.list_credentials().unwrap();
        creds.sort();
        assert_eq!(
            creds,
            vec![
                ("whatsapp".to_string(), Some("business".to_string())),
                ("whatsapp".to_string(), Some("personal".to_string())),
            ]
        );
    }

    #[test]
    fn credential_store_delete_returns_true_then_false() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        store
            .write_credential("whatsapp", Some("p"), &json!({}))
            .unwrap();
        assert!(store.delete_credential("whatsapp", Some("p")).unwrap());
        assert!(!store.delete_credential("whatsapp", Some("p")).unwrap());
    }

    #[test]
    fn credential_store_supports_no_instance() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        store
            .write_credential("email", None, &json!({ "user": "x@y" }))
            .unwrap();
        let creds = store.list_credentials().unwrap();
        assert_eq!(creds, vec![("email".to_string(), None)]);
        assert!(store.delete_credential("email", None).unwrap());
    }

    #[test]
    fn credential_store_write_rejects_empty_channel() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("secrets"));
        let err = store
            .write_credential("", None, &json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("channel must not be empty"));
    }

    #[test]
    fn credential_store_list_on_missing_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::new(dir.path().join("never_created"));
        assert!(store.list_credentials().unwrap().is_empty());
    }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 82.10.h.b.1 — in-memory pairing challenge store.
// Mirrors OpenClaw's QR-login pattern (in-memory Map + TTL — daemon
// restart kills in-flight pairings; the QR client-side expiry is
// shorter than any reasonable TTL anyway).
// ─────────────────────────────────────────────────────────────────────

/// One challenge entry in the in-memory store.
#[derive(Debug, Clone)]
struct PairingChallengeEntry {
    status: PairingStatus,
    expires_at: Instant,
}

/// In-memory `PairingChallengeStore` adapter. Suitable for the v1
/// admin RPC pipeline — daemon restart drops any in-flight QR
/// challenge, which matches WhatsApp client-side behaviour (the QR
/// itself expires within ~30 s).
///
/// Boot wiring spawns a periodic [`prune_expired`] task (default
/// every 30 s) so the map stays bounded. `read_challenge`
/// transparently flips entries past their TTL to
/// `PairingState::Expired` before returning them.
#[derive(Debug, Clone)]
pub struct InMemoryPairingChallengeStore {
    inner: Arc<DashMap<Uuid, PairingChallengeEntry>>,
    default_ttl: Duration,
}

impl InMemoryPairingChallengeStore {
    /// Build a fresh store. `default_ttl` is the upper bound used
    /// when `create_challenge` is called with `ttl_secs == 0`.
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            default_ttl,
        }
    }

    /// Drop every entry whose `expires_at` is in the past. Returns
    /// the number of entries removed. Cheap to call frequently —
    /// `DashMap::retain` walks each bucket once.
    pub fn prune_expired(&self) -> usize {
        let now = Instant::now();
        let before = self.inner.len();
        self.inner.retain(|_, entry| entry.expires_at > now);
        before.saturating_sub(self.inner.len())
    }

    /// Test/observability hook: current entry count (including
    /// stale-but-not-pruned).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when no challenges are tracked.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl PairingChallengeStore for InMemoryPairingChallengeStore {
    fn create_challenge(
        &self,
        _agent_id: &str,
        _channel: &str,
        _instance: Option<&str>,
        ttl_secs: u64,
    ) -> anyhow::Result<(Uuid, u64)> {
        let id = Uuid::new_v4();
        let ttl = if ttl_secs == 0 {
            self.default_ttl
        } else {
            Duration::from_secs(ttl_secs)
        };
        let expires_at = Instant::now() + ttl;
        let expires_at_ms = epoch_ms_now().saturating_add(ttl.as_millis() as u64);
        self.inner.insert(
            id,
            PairingChallengeEntry {
                status: PairingStatus {
                    challenge_id: id,
                    state: PairingState::Pending,
                    data: PairingStatusData::default(),
                },
                expires_at,
            },
        );
        Ok((id, expires_at_ms))
    }

    fn read_challenge(
        &self,
        challenge_id: Uuid,
    ) -> anyhow::Result<Option<PairingStatus>> {
        let now = Instant::now();
        // First pass: check + flip-to-expired without holding the
        // entry lock past the read.
        if let Some(mut entry) = self.inner.get_mut(&challenge_id) {
            if entry.expires_at <= now
                && !matches!(
                    entry.status.state,
                    PairingState::Linked
                        | PairingState::Expired
                        | PairingState::Cancelled
                )
            {
                entry.status.state = PairingState::Expired;
                entry.status.data = PairingStatusData {
                    error: Some("ttl exceeded before user completed pairing".into()),
                    ..Default::default()
                };
            }
            return Ok(Some(entry.status.clone()));
        }
        Ok(None)
    }

    fn cancel_challenge(&self, challenge_id: Uuid) -> anyhow::Result<bool> {
        let Some(mut entry) = self.inner.get_mut(&challenge_id) else {
            return Ok(false);
        };
        if matches!(
            entry.status.state,
            PairingState::Linked | PairingState::Expired | PairingState::Cancelled
        ) {
            return Ok(false);
        }
        entry.status.state = PairingState::Cancelled;
        entry.status.data = PairingStatusData::default();
        Ok(true)
    }
}

fn epoch_ms_now() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Phase 82.10.h.b.2 — `PairingNotifier` adapter that pushes
/// `nexo/notify/pairing_status_changed` JSON-RPC notification
/// frames into the same `mpsc::Sender<String>` the extension
/// host uses for outbound stdin writes (the `outbox_tx` in
/// `nexo_extensions::runtime::stdio`). Boot wires one notifier
/// per microapp that holds an admin RPC dispatcher with the
/// pairing domain enabled.
///
/// Send semantics: `try_send`, so a slow microapp can never
/// block the dispatcher thread. When the channel is full the
/// frame is dropped and a `warn` is emitted — losing a status
/// update is preferable to backpressuring the daemon onto a
/// laggy reader. Microapps that need every frame should poll
/// `pairing/status` instead of relying solely on notifications.
#[derive(Debug, Clone)]
pub struct StdioPairingNotifier {
    sender: mpsc::Sender<String>,
}

impl StdioPairingNotifier {
    /// Build a notifier wrapping the given outbox sender.
    pub fn new(sender: mpsc::Sender<String>) -> Self {
        Self { sender }
    }
}

impl PairingNotifier for StdioPairingNotifier {
    fn notify_status(&self, status: &PairingStatus) {
        let params = serde_json::to_value(status).unwrap_or(serde_json::Value::Null);
        let frame = json_rpc_notification(PAIRING_STATUS_NOTIFY_METHOD, params);
        if let Err(e) = self.sender.try_send(frame) {
            tracing::warn!(
                challenge_id = %status.challenge_id,
                error = %e,
                "pairing notifier outbox full or closed; dropping notification frame",
            );
        }
    }
}

/// Phase 82.10.h.b.5 — `AdminOutboundWriter` whose backing
/// `mpsc::Sender<String>` is bound after construction. Solves
/// the chicken-and-egg between [`DispatcherAdminRouter`] (needs
/// a writer at build time, which happens before the extension
/// spawns) and [`StdioRuntime::outbox_sender`] (the writer's
/// real `mpsc::Sender` is created inside spawn).
///
/// Boot wiring:
/// 1. `let writer = Arc::new(DeferredAdminOutboundWriter::new());`
/// 2. Build router with `writer.clone() as Arc<dyn ...>` and pass
///    it to `StdioSpawnOptions::admin_router`.
/// 3. Spawn the extension.
/// 4. `writer.bind(runtime.outbox_sender());` — every queued
///    response from this point on flows to the live stdin.
///
/// Frames `send`-ed before `bind` is called are dropped with a
/// `warn` (the dispatcher already considers admin response
/// delivery best-effort so this matches the `try_send`-on-full
/// semantics microapps already see when the channel saturates).
#[derive(Debug, Default)]
pub struct DeferredAdminOutboundWriter {
    sender: OnceLock<mpsc::Sender<String>>,
}

impl DeferredAdminOutboundWriter {
    /// Build an unbound writer. Frames sent before [`bind`] is
    /// called will be dropped + logged.
    pub fn new() -> Self {
        Self {
            sender: OnceLock::new(),
        }
    }

    /// Attach the live extension stdin sender. Idempotent — a
    /// second call is a no-op (`OnceLock` semantics) so a
    /// post-restart re-bind still works through the original
    /// channel.
    pub fn bind(&self, sender: mpsc::Sender<String>) {
        let _ = self.sender.set(sender);
    }
}

#[async_trait]
impl AdminOutboundWriter for DeferredAdminOutboundWriter {
    async fn send(&self, line: String) {
        match self.sender.get() {
            Some(s) => {
                if let Err(e) = s.try_send(line) {
                    tracing::warn!(error = %e, "admin response outbox full or closed; frame dropped");
                }
            }
            None => {
                tracing::warn!(
                    "admin response sent before outbound writer was bound; frame dropped",
                );
            }
        }
    }
}

/// Build a single-line JSON-RPC 2.0 notification frame
/// (`{"jsonrpc":"2.0","method":"…","params":…}`, no `id`). Used
/// by [`StdioPairingNotifier`] and any future notifier the admin
/// pipeline grows. Returns the line without trailing newline so
/// callers control framing.
pub fn json_rpc_notification(method: &str, params: serde_json::Value) -> String {
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    serde_json::to_string(&frame).unwrap_or_else(|_| "{}".into())
}

// ─────────────────────────────────────────────────────────────────────
// Phase 82.11.3 — production `TranscriptReader` adapter wrapping the
// existing `TranscriptWriter` (JSONL files) + `TranscriptsIndex`
// (SQLite FTS5). Both subsystems are already alive in the daemon —
// the adapter is purely a wire-shape mapper.
// ─────────────────────────────────────────────────────────────────────

/// `TranscriptReader` impl backed by the on-disk transcript root +
/// the FTS5 index. Constructed once per agent at boot. The
/// `agent_id` filter on `list/read/search` calls is enforced
/// against the writer's own `agent_id` (defensive — adapters
/// reject calls scoped to a different agent).
pub struct TranscriptReaderFs {
    writer: Arc<TranscriptWriter>,
    /// Optional FTS5 index. `None` disables search (returns
    /// `Internal: search index not configured`); list + read
    /// still work over the JSONL files.
    index: Option<Arc<TranscriptsIndex>>,
    /// Owner agent — fixed at construction. Calls scoped to a
    /// different `agent_id` return empty results.
    agent_id: String,
}

impl std::fmt::Debug for TranscriptReaderFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptReaderFs")
            .field("agent_id", &self.agent_id)
            .field("index", &self.index.is_some())
            .finish_non_exhaustive()
    }
}

impl TranscriptReaderFs {
    /// Build an adapter pinned to one agent. Pass the same
    /// `TranscriptWriter` the daemon already uses for appends so
    /// reader + writer share the per-session header lock and the
    /// same redactor (the firehose downstream emits the
    /// already-redacted body).
    pub fn new(
        writer: Arc<TranscriptWriter>,
        index: Option<Arc<TranscriptsIndex>>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            writer,
            index,
            agent_id: agent_id.into(),
        }
    }
}

#[async_trait]
impl TranscriptReader for TranscriptReaderFs {
    async fn list_recent_events(
        &self,
        filter: &AgentEventsListFilter,
    ) -> anyhow::Result<Vec<AgentEventKind>> {
        if filter.agent_id != self.agent_id {
            return Ok(Vec::new());
        }
        let root = self.writer.root().to_path_buf();
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut entries = match tokio::fs::read_dir(&root).await {
            Ok(rd) => rd,
            Err(_) => return Ok(Vec::new()),
        };
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                paths.push(p);
            }
        }
        // Most-recently-modified first so newest events surface
        // even before parsing each file.
        paths.sort_by_key(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });
        paths.reverse();

        let since_ms = filter.since_ms.unwrap_or(0);
        let mut out: Vec<AgentEventKind> = Vec::new();
        for path in paths {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let Ok(session_id) = uuid::Uuid::parse_str(stem) else {
                continue;
            };
            let lines = self
                .writer
                .read_session(session_id)
                .await
                .unwrap_or_default();
            let mut entry_seq: u64 = 0;
            for line in lines.iter() {
                let TranscriptLine::Entry(e) = line else {
                    continue;
                };
                let ts_ms = e.timestamp.timestamp_millis() as u64;
                let seq = entry_seq;
                entry_seq += 1;
                if ts_ms < since_ms {
                    continue;
                }
                out.push(AgentEventKind::TranscriptAppended {
                    agent_id: self.agent_id.clone(),
                    session_id,
                    seq,
                    role: map_role(e.role),
                    body: e.content.clone(),
                    sent_at_ms: ts_ms,
                    sender_id: e.sender_id.clone(),
                    source_plugin: if e.source_plugin.is_empty() {
                        "internal".into()
                    } else {
                        e.source_plugin.clone()
                    },
                });
            }
        }
        // Newest-first global ordering across sessions.
        out.sort_by_key(|e| match e {
            AgentEventKind::TranscriptAppended { sent_at_ms, .. } => {
                std::cmp::Reverse(*sent_at_ms)
            }
            _ => std::cmp::Reverse(0u64),
        });
        out.truncate(filter.limit);
        Ok(out)
    }

    async fn read_session_events(
        &self,
        params: &AgentEventsReadParams,
    ) -> anyhow::Result<Vec<AgentEventKind>> {
        if params.agent_id != self.agent_id {
            return Ok(Vec::new());
        }
        let lines = self
            .writer
            .read_session(params.session_id)
            .await
            .unwrap_or_default();
        let after = params.since_seq.unwrap_or(u64::MAX);
        let want_all = params.since_seq.is_none();
        let mut out: Vec<AgentEventKind> = Vec::new();
        let mut entry_seq: u64 = 0;
        for line in lines.iter() {
            let TranscriptLine::Entry(e) = line else {
                continue;
            };
            let seq = entry_seq;
            entry_seq += 1;
            if !want_all && seq <= after {
                continue;
            }
            out.push(AgentEventKind::TranscriptAppended {
                agent_id: self.agent_id.clone(),
                session_id: params.session_id,
                seq,
                role: map_role(e.role),
                body: e.content.clone(),
                sent_at_ms: e.timestamp.timestamp_millis() as u64,
                sender_id: e.sender_id.clone(),
                source_plugin: if e.source_plugin.is_empty() {
                    "internal".into()
                } else {
                    e.source_plugin.clone()
                },
            });
            if out.len() >= params.limit {
                break;
            }
        }
        Ok(out)
    }

    async fn search_events(
        &self,
        params: &AgentEventsSearchParams,
    ) -> anyhow::Result<Vec<SearchHit>> {
        if params.agent_id != self.agent_id {
            return Ok(Vec::new());
        }
        let Some(index) = self.index.as_ref() else {
            anyhow::bail!("transcripts FTS index not configured");
        };
        let hits = index
            .search(&self.agent_id, &params.query, params.limit)
            .await?;
        Ok(hits
            .into_iter()
            .map(|h| SearchHit {
                session_id: h.session_id,
                timestamp_ms: (h.timestamp_unix as u64).saturating_mul(1000),
                role: parse_wire_role(&h.role),
                source_plugin: h.source_plugin,
                snippet: h.snippet,
            })
            .collect())
    }
}

fn map_role(r: nexo_core::agent::transcripts::TranscriptRole) -> WireTranscriptRole {
    use nexo_core::agent::transcripts::TranscriptRole as Core;
    match r {
        Core::User => WireTranscriptRole::User,
        Core::Assistant => WireTranscriptRole::Assistant,
        Core::Tool => WireTranscriptRole::Tool,
        Core::System => WireTranscriptRole::System,
    }
}

fn parse_wire_role(s: &str) -> WireTranscriptRole {
    match s {
        "user" => WireTranscriptRole::User,
        "assistant" => WireTranscriptRole::Assistant,
        "tool" => WireTranscriptRole::Tool,
        _ => WireTranscriptRole::System,
    }
}

#[cfg(test)]
mod pairing_store_tests {
    use super::*;
    use std::thread::sleep;

    fn store_with(ttl_ms: u64) -> InMemoryPairingChallengeStore {
        InMemoryPairingChallengeStore::new(Duration::from_millis(ttl_ms))
    }

    #[test]
    fn create_then_read_returns_pending_status() {
        let store = store_with(60_000);
        let (id, expires_at_ms) = store
            .create_challenge("ana", "whatsapp", Some("personal"), 0)
            .unwrap();
        assert!(expires_at_ms > 0);

        let status = store.read_challenge(id).unwrap().unwrap();
        assert_eq!(status.challenge_id, id);
        assert_eq!(status.state, PairingState::Pending);
    }

    #[test]
    fn read_unknown_id_returns_none() {
        let store = store_with(60_000);
        assert!(store.read_challenge(Uuid::nil()).unwrap().is_none());
    }

    #[test]
    fn read_after_ttl_flips_to_expired_with_reason() {
        let store = store_with(10);
        let (id, _) = store
            .create_challenge("ana", "whatsapp", None, 0)
            .unwrap();
        sleep(Duration::from_millis(30));
        let status = store.read_challenge(id).unwrap().unwrap();
        assert_eq!(status.state, PairingState::Expired);
        assert!(status
            .data
            .error
            .as_deref()
            .is_some_and(|s| s.contains("ttl exceeded")));
    }

    #[test]
    fn cancel_pending_succeeds_and_is_idempotent() {
        let store = store_with(60_000);
        let (id, _) = store
            .create_challenge("ana", "whatsapp", None, 0)
            .unwrap();
        assert!(store.cancel_challenge(id).unwrap());
        // Second cancel — already terminal.
        assert!(!store.cancel_challenge(id).unwrap());
        let status = store.read_challenge(id).unwrap().unwrap();
        assert_eq!(status.state, PairingState::Cancelled);
    }

    #[test]
    fn cancel_unknown_returns_false() {
        let store = store_with(60_000);
        assert!(!store.cancel_challenge(Uuid::nil()).unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notifier_sends_pairing_status_changed_frame() {
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let notifier = StdioPairingNotifier::new(tx);
        let status = PairingStatus {
            challenge_id: Uuid::nil(),
            state: PairingState::QrReady,
            data: PairingStatusData {
                qr_ascii: Some("##".into()),
                ..Default::default()
            },
        };
        notifier.notify_status(&status);
        let line = rx.recv().await.expect("frame received");
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["method"], PAIRING_STATUS_NOTIFY_METHOD);
        assert_eq!(parsed["params"]["state"], "qr_ready");
        assert!(parsed.get("id").is_none(), "notifications carry no id");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notifier_drops_frames_when_channel_full() {
        // Bound = 1; first frame fills, second is dropped.
        let (tx, mut rx) = mpsc::channel::<String>(1);
        let notifier = StdioPairingNotifier::new(tx);
        let status_a = PairingStatus {
            challenge_id: Uuid::nil(),
            state: PairingState::Pending,
            data: PairingStatusData::default(),
        };
        let status_b = PairingStatus {
            challenge_id: Uuid::nil(),
            state: PairingState::Cancelled,
            data: PairingStatusData::default(),
        };
        notifier.notify_status(&status_a);
        notifier.notify_status(&status_b); // dropped — channel is full, no reader yet
        let first = rx.recv().await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(parsed["params"]["state"], "pending");
        // Second was dropped — channel is now empty until next sender call.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn json_rpc_notification_helper_shape() {
        let line = json_rpc_notification("nexo/notify/test", serde_json::json!({"k": 1}));
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["method"], "nexo/notify/test");
        assert_eq!(parsed["params"]["k"], 1);
        assert!(parsed.get("id").is_none());
    }

    #[test]
    fn prune_expired_drops_only_stale_entries() {
        let store = store_with(10);
        let (alive, _) = store
            .create_challenge("ana", "whatsapp", None, 60)
            .unwrap();
        let (_dead, _) = store
            .create_challenge("bob", "whatsapp", None, 0)
            .unwrap(); // uses 10ms default
        sleep(Duration::from_millis(30));
        let removed = store.prune_expired();
        assert_eq!(removed, 1);
        assert_eq!(store.len(), 1);
        assert!(store.read_challenge(alive).unwrap().is_some());
    }
}

#[cfg(test)]
mod transcript_reader_tests {
    use super::*;
    use chrono::Utc;
    use nexo_core::agent::transcripts::{
        TranscriptEntry, TranscriptRole as CoreRole, TranscriptWriter,
    };
    use uuid::Uuid;

    fn user_entry(text: &str) -> TranscriptEntry {
        TranscriptEntry {
            timestamp: Utc::now(),
            role: CoreRole::User,
            content: text.into(),
            message_id: None,
            source_plugin: "whatsapp".into(),
            sender_id: Some("wa.55".into()),
        }
    }

    async fn seeded_writer(
        dir: &std::path::Path,
        agent: &str,
        sessions: &[(Uuid, &[&str])],
    ) -> Arc<TranscriptWriter> {
        let writer = Arc::new(TranscriptWriter::new(dir, agent));
        for (sid, msgs) in sessions {
            for body in *msgs {
                writer
                    .append_entry(*sid, user_entry(body))
                    .await
                    .expect("append");
            }
        }
        writer
    }

    #[tokio::test]
    async fn list_recent_events_projects_jsonl_into_wire_events() {
        let tmp = tempfile::tempdir().unwrap();
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        let writer = seeded_writer(
            tmp.path(),
            "ana",
            &[(s1, &["hi", "hola"]), (s2, &["yo"])],
        )
        .await;
        let reader = TranscriptReaderFs::new(writer, None, "ana");
        let events = reader
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "ana".into(),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        // 3 entries across 2 sessions.
        assert_eq!(events.len(), 3);
        // Each entry maps to TranscriptAppended with the right
        // source_plugin echoed.
        for e in &events {
            match e {
                AgentEventKind::TranscriptAppended {
                    source_plugin,
                    role,
                    sender_id,
                    ..
                } => {
                    assert_eq!(source_plugin, "whatsapp");
                    assert_eq!(*role, WireTranscriptRole::User);
                    assert_eq!(sender_id.as_deref(), Some("wa.55"));
                }
                _ => panic!("expected TranscriptAppended"),
            }
        }
    }

    #[tokio::test]
    async fn list_recent_events_returns_empty_for_other_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = seeded_writer(tmp.path(), "ana", &[(Uuid::new_v4(), &["x"])]).await;
        let reader = TranscriptReaderFs::new(writer, None, "ana");
        let events = reader
            .list_recent_events(&AgentEventsListFilter {
                agent_id: "bob".into(),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn read_session_events_filters_by_since_seq_and_caps_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = Uuid::new_v4();
        let writer =
            seeded_writer(tmp.path(), "ana", &[(sid, &["a", "b", "c", "d"])]).await;
        let reader = TranscriptReaderFs::new(writer, None, "ana");

        // Caller wants entries strictly AFTER seq=0 (so b, c, d
        // — but the file also stores a SessionHeader line so the
        // first entry in raw lines is at logical seq=1).
        // Run without since_seq first to learn the seq of the
        // first entry, then filter.
        let all = reader
            .read_session_events(&AgentEventsReadParams {
                agent_id: "ana".into(),
                session_id: sid,
                since_seq: None,
                limit: 50,
            })
            .await
            .unwrap();
        assert_eq!(all.len(), 4);
        let first_seq = match &all[0] {
            AgentEventKind::TranscriptAppended { seq, .. } => *seq,
            _ => panic!(),
        };
        let after_first = reader
            .read_session_events(&AgentEventsReadParams {
                agent_id: "ana".into(),
                session_id: sid,
                since_seq: Some(first_seq),
                limit: 50,
            })
            .await
            .unwrap();
        assert_eq!(after_first.len(), 3);

        // Limit cap.
        let capped = reader
            .read_session_events(&AgentEventsReadParams {
                agent_id: "ana".into(),
                session_id: sid,
                since_seq: None,
                limit: 2,
            })
            .await
            .unwrap();
        assert_eq!(capped.len(), 2);
    }

    #[tokio::test]
    async fn search_events_returns_internal_when_index_is_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = seeded_writer(tmp.path(), "ana", &[]).await;
        let reader = TranscriptReaderFs::new(writer, None, "ana");
        let err = reader
            .search_events(&AgentEventsSearchParams {
                agent_id: "ana".into(),
                query: "foo".into(),
                kind: None,
                limit: 10,
            })
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("FTS index"), "got: {msg}");
    }
}
