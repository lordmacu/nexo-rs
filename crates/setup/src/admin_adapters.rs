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
use nexo_core::agent::admin_rpc::transcript_appender::{
    TranscriptAppender, TranscriptEntry as AppenderEntry, TranscriptRole as AppenderRole,
};
use nexo_core::agent::admin_rpc::domains::agents::YamlPatcher;
use nexo_core::agent::admin_rpc::domains::credentials::CredentialStore;
use nexo_core::agent::admin_rpc::domains::llm_providers::LlmYamlPatcher;
use nexo_core::agent::admin_rpc::domains::pairing::{
    PairingChallengeStore, PairingNotifier, PAIRING_STATUS_NOTIFY_METHOD,
};
use nexo_core::agent::admin_rpc::domains::escalations::{
    filter_matches, EscalationStore,
};
use nexo_core::agent::admin_rpc::channel_outbound::{
    ChannelOutboundDispatcher, ChannelOutboundError, OutboundAck, OutboundMessage,
};
use nexo_core::agent::admin_rpc::domains::processing::ProcessingControlStore;
use nexo_core::agent::admin_rpc::domains::skills::SkillsStore;
use nexo_core::agent::admin_rpc::domains::tenants::TenantStore;
use nexo_tool_meta::admin::tenants::{
    TenantDetail, TenantSummary, TenantsListFilter, TenantsUpsertInput,
};
use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_tool_meta::admin::escalations::{
    EscalationEntry, EscalationState, EscalationsListParams, ResolvedBy,
};
use nexo_tool_meta::admin::skills::{
    SkillRecord, SkillRequiresRecord, SkillSummary, SkillsUpsertParams,
};
use nexo_tool_meta::admin::processing::{
    PendingInbound, ProcessingControlState, ProcessingScope,
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

/// Filesystem-backed skills CRUD adapter — Phase 83.8.3.
///
/// Mirrors the on-disk layout the runtime
/// `crate::agent::skills::SkillLoader` already reads from:
/// `<root>/<name>/SKILL.md`. The body is composed back into the
/// frontmatter format `SkillLoader::parse_frontmatter` recognises, so
/// a CRUD round-trip through admin RPC produces a file the loader
/// reads identically to a hand-authored one.
///
/// Concurrency: per-name `tokio::sync::Mutex` so concurrent
/// upserts to the same skill serialise; different skills proceed
/// in parallel.
///
/// Hot-reload: optional `on_change` callback fired after a
/// successful `upsert` or `delete`. Production wiring passes a
/// closure that triggers the existing Phase 18
/// `ConfigReloadCoordinator`, so an agent that uses the skill
/// reloads its system prompt on the next turn.
pub struct FsSkillsStore {
    root: PathBuf,
    locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    on_change: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for FsSkillsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsSkillsStore")
            .field("root", &self.root)
            .field("on_change", &self.on_change.is_some())
            .finish()
    }
}

impl FsSkillsStore {
    /// Build a store rooted at `root`. The directory is created on
    /// first write if missing.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            locks: DashMap::new(),
            on_change: None,
        }
    }

    /// Install a callback fired after every successful mutation.
    /// Production wiring passes the agents-domain reload trigger so
    /// hot-reload happens in lock-step with other yaml mutations.
    pub fn with_on_change(
        mut self,
        cb: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        self.on_change = Some(cb);
        self
    }

    fn lock_for(&self, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Resolve `<root>/<name>/SKILL.md`. Defense-in-depth on top
    /// of the handler-side regex check: rejects names containing
    /// path separators, parent-dir refs, or non-component
    /// characters that the regex `^[a-z0-9][a-z0-9-]{0,63}$`
    /// already filters at the handler boundary.
    fn resolve_skill_path(&self, name: &str) -> anyhow::Result<PathBuf> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || name.contains('\0')
            || name == "."
        {
            anyhow::bail!("skill path escapes root: invalid name `{name}`");
        }
        Ok(self.root.join(name).join("SKILL.md"))
    }

    fn compose_blob(record: &SkillRecord) -> String {
        // YAML frontmatter only when at least one field is set.
        let has_meta = record.display_name.is_some()
            || record.description.is_some()
            || record.max_chars.is_some()
            || !record.requires.bins.is_empty()
            || !record.requires.env.is_empty()
            || record.requires.mode != Default::default();
        if !has_meta {
            return record.body.clone();
        }
        let mut yaml = String::new();
        if let Some(n) = &record.display_name {
            yaml.push_str(&format!("name: {}\n", yaml_escape(n)));
        }
        if let Some(d) = &record.description {
            yaml.push_str(&format!("description: {}\n", yaml_escape(d)));
        }
        if let Some(c) = record.max_chars {
            yaml.push_str(&format!("max_chars: {c}\n"));
        }
        let r = &record.requires;
        if !r.bins.is_empty() || !r.env.is_empty() || r.mode != Default::default() {
            yaml.push_str("requires:\n");
            if !r.bins.is_empty() {
                yaml.push_str("  bins:\n");
                for b in &r.bins {
                    yaml.push_str(&format!("    - {}\n", yaml_escape(b)));
                }
            }
            if !r.env.is_empty() {
                yaml.push_str("  env:\n");
                for e in &r.env {
                    yaml.push_str(&format!("    - {}\n", yaml_escape(e)));
                }
            }
            yaml.push_str(&format!(
                "  mode: {}\n",
                match r.mode {
                    nexo_tool_meta::admin::skills::SkillDepsMode::Strict => "strict",
                    nexo_tool_meta::admin::skills::SkillDepsMode::Warn => "warn",
                    nexo_tool_meta::admin::skills::SkillDepsMode::Disable => "disable",
                }
            ));
        }
        format!("---\n{yaml}---\n\n{}", record.body)
    }

    async fn read_record(&self, name: &str) -> anyhow::Result<Option<SkillRecord>> {
        let path = self.resolve_skill_path(name)?;
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let updated_at = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(
                        d.as_secs() as i64,
                        d.subsec_nanos(),
                    )
                    .unwrap_or_else(chrono::Utc::now)
                })
                .unwrap_or_else(chrono::Utc::now),
            Err(_) => chrono::Utc::now(),
        };
        let (display_name, description, max_chars, requires, body) = parse_frontmatter(&raw);
        Ok(Some(SkillRecord {
            name: name.to_string(),
            display_name,
            description,
            body,
            max_chars,
            requires,
            updated_at,
        }))
    }
}

/// Minimal YAML scalar escaping — wraps in double quotes when the
/// value contains characters that would break the scalar.
fn yaml_escape(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s.contains('\n')
        || s.starts_with(' ')
        || s.ends_with(' ');
    if needs_quote {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

fn parse_frontmatter(
    raw: &str,
) -> (Option<String>, Option<String>, Option<usize>, SkillRequiresRecord, String) {
    if !raw.starts_with("---") {
        return (None, None, None, SkillRequiresRecord::default(), raw.to_string());
    }
    let after_open = match raw.find('\n') {
        Some(n) => &raw[n + 1..],
        None => return (None, None, None, SkillRequiresRecord::default(), String::new()),
    };
    let mut offset = 0;
    let mut end_idx = None;
    for line in after_open.split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        if stripped.trim() == "---" {
            end_idx = Some(offset);
            break;
        }
        offset += line.len();
    }
    let Some(end_idx) = end_idx else {
        return (None, None, None, SkillRequiresRecord::default(), raw.to_string());
    };
    let yaml = &after_open[..end_idx];
    let after_close = &after_open[end_idx..];
    let body = after_close
        .find('\n')
        .map(|i| &after_close[i + 1..])
        .unwrap_or("");
    let body_owned = body.trim_start_matches('\n').to_string();
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct ParsedFrontmatter {
        name: Option<String>,
        description: Option<String>,
        max_chars: Option<usize>,
        requires: ParsedRequires,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct ParsedRequires {
        bins: Vec<String>,
        env: Vec<String>,
        mode: nexo_tool_meta::admin::skills::SkillDepsMode,
    }
    let parsed: ParsedFrontmatter = serde_yaml::from_str(yaml).unwrap_or_default();
    let requires = SkillRequiresRecord {
        bins: parsed.requires.bins,
        env: parsed.requires.env,
        mode: parsed.requires.mode,
    };
    (parsed.name, parsed.description, parsed.max_chars, requires, body_owned)
}

#[async_trait]
impl SkillsStore for FsSkillsStore {
    async fn list(&self, prefix: Option<&str>) -> anyhow::Result<Vec<SkillSummary>> {
        let mut out = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let ft = match entry.file_type().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            if !ft.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if let Some(p) = prefix {
                if !name.starts_with(p) {
                    continue;
                }
            }
            let Some(record) = self.read_record(&name).await? else {
                continue;
            };
            out.push(SkillSummary {
                name: record.name,
                display_name: record.display_name,
                description: record.description,
                updated_at: record.updated_at,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn get(&self, name: &str) -> anyhow::Result<Option<SkillRecord>> {
        self.read_record(name).await
    }

    async fn upsert(
        &self,
        params: SkillsUpsertParams,
    ) -> anyhow::Result<(SkillRecord, bool)> {
        let lock = self.lock_for(&params.name);
        let _guard = lock.lock().await;
        let path = self.resolve_skill_path(&params.name)?;
        let dir = path.parent().expect("SKILL.md has parent");
        let created = !path.exists();
        tokio::fs::create_dir_all(dir).await?;
        let record = SkillRecord {
            name: params.name.clone(),
            display_name: params.display_name,
            description: params.description,
            body: params.body,
            max_chars: params.max_chars,
            requires: params.requires.unwrap_or_default(),
            updated_at: chrono::Utc::now(),
        };
        let blob = Self::compose_blob(&record);
        let tmp = path.with_extension("md.tmp");
        tokio::fs::write(&tmp, &blob).await?;
        tokio::fs::rename(&tmp, &path).await?;
        if let Some(cb) = &self.on_change {
            (cb)();
        }
        Ok((record, created))
    }

    async fn delete(&self, name: &str) -> anyhow::Result<bool> {
        let lock = self.lock_for(name);
        let _guard = lock.lock().await;
        let path = self.resolve_skill_path(name)?;
        let dir = path.parent().expect("SKILL.md has parent");
        if !dir.exists() {
            return Ok(false);
        }
        tokio::fs::remove_dir_all(dir).await?;
        if let Some(cb) = &self.on_change {
            (cb)();
        }
        Ok(true)
    }
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
                    tenant_id: None,
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
                tenant_id: None,
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

// ─────────────────────────────────────────────────────────────────────
// Phase 82.13.b.1.3 — production `TranscriptAppender` adapter wrapping
// the same `TranscriptWriter` the daemon already uses for native
// agent appends. The adapter translates the admin-RPC-side
// `AppenderEntry` / `AppenderRole` into the writer's own types at
// the boundary so the admin-RPC crate stays decoupled from the
// concrete writer module.
// ─────────────────────────────────────────────────────────────────────

/// Production `TranscriptAppender` impl. Holds an `Arc<TranscriptWriter>`
/// (cheap to clone — the writer's internal state is `Arc`d) and
/// forwards `append()` calls into `writer.append_entry()`. Reuses the
/// writer's redactor + per-session header lock + FTS index + firehose
/// emitter — operator stamps go through the same pipeline as native
/// agent stamps, so subscribers see them via Phase 82.11
/// `nexo/notify/agent_event` notifications without any new wiring.
pub struct TranscriptWriterAppender {
    writer: Arc<TranscriptWriter>,
}

impl std::fmt::Debug for TranscriptWriterAppender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptWriterAppender")
            .finish_non_exhaustive()
    }
}

impl TranscriptWriterAppender {
    /// Build an appender pinned to the daemon's `TranscriptWriter`.
    /// The agent_id stamped on each entry is supplied by the
    /// admin-RPC handler from `ProcessingScope.agent_id()` — the
    /// adapter doesn't pin its own.
    pub fn new(writer: Arc<TranscriptWriter>) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl TranscriptAppender for TranscriptWriterAppender {
    async fn append(
        &self,
        agent_id: &str,
        session_id: uuid::Uuid,
        entry: AppenderEntry,
    ) -> anyhow::Result<()> {
        // Defense-in-depth: refuse to stamp a session that does
        // not belong to this writer's pinned agent. Without this
        // check, a buggy admin RPC caller (or compromised
        // operator token) could write into a different agent's
        // transcript via the shared writer instance.
        let writer_agent = self.writer.agent_id();
        if writer_agent != agent_id {
            return Err(anyhow::anyhow!(
                "agent_id mismatch: writer pinned to {writer_agent}, request for {agent_id}"
            ));
        }
        let core_entry = nexo_core::agent::transcripts::TranscriptEntry {
            timestamp: chrono::Utc::now(),
            role: match entry.role {
                AppenderRole::User => nexo_core::agent::transcripts::TranscriptRole::User,
                AppenderRole::Assistant => {
                    nexo_core::agent::transcripts::TranscriptRole::Assistant
                }
                AppenderRole::Tool => nexo_core::agent::transcripts::TranscriptRole::Tool,
                AppenderRole::System => nexo_core::agent::transcripts::TranscriptRole::System,
            },
            content: entry.content,
            message_id: entry.message_id,
            source_plugin: entry.source_plugin,
            sender_id: entry.sender_id,
        };
        self.writer.append_entry(session_id, core_entry).await
    }
}

#[cfg(test)]
mod transcript_appender_adapter_tests {
    use super::*;
    use nexo_core::agent::transcripts::TranscriptWriter;
    use tempfile::TempDir;

    #[tokio::test]
    async fn append_writes_through_to_jsonl() {
        let tmp = TempDir::new().unwrap();
        let writer = Arc::new(TranscriptWriter::new(tmp.path(), "ana"));
        let adapter = TranscriptWriterAppender::new(writer.clone());
        let session_id = uuid::Uuid::new_v4();
        let entry = AppenderEntry {
            role: AppenderRole::Assistant,
            content: "hola desde operador".into(),
            source_plugin: "intervention:whatsapp".into(),
            sender_id: Some("operator:tokhash".into()),
            message_id: None,
        };
        adapter.append("ana", session_id, entry).await.unwrap();
        // Read the JSONL back — header + 1 entry expected.
        let jsonl = tmp.path().join(format!("{session_id}.jsonl"));
        let raw = tokio::fs::read_to_string(&jsonl).await.unwrap();
        let lines: Vec<_> = raw.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("\"role\":\"assistant\""));
        assert!(lines[1].contains("\"content\":\"hola desde operador\""));
        assert!(lines[1].contains("intervention:whatsapp"));
        assert!(lines[1].contains("operator:tokhash"));
    }

    #[tokio::test]
    async fn append_rejects_cross_agent_request() {
        let tmp = TempDir::new().unwrap();
        let writer = Arc::new(TranscriptWriter::new(tmp.path(), "ana"));
        let adapter = TranscriptWriterAppender::new(writer);
        let entry = AppenderEntry {
            role: AppenderRole::Assistant,
            content: "x".into(),
            source_plugin: "intervention:whatsapp".into(),
            sender_id: None,
            message_id: None,
        };
        let r = adapter
            .append("OTHER_AGENT", uuid::Uuid::new_v4(), entry)
            .await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("agent_id mismatch"));
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

// ─────────────────────────────────────────────────────────────────────
// Phase 82.13 — in-memory `ProcessingControlStore` adapter.
// Mirrors the in-memory pairing store pattern from 82.10.h.b.1:
// `DashMap` keyed by scope, lazy initialisation, drop = forget.
// SQLite-backed durable variant is a 82.13.b follow-up.
// ─────────────────────────────────────────────────────────────────────

/// In-memory `ProcessingControlStore` adapter.
#[derive(Debug, Default, Clone)]
pub struct InMemoryProcessingControlStore {
    inner: Arc<DashMap<ProcessingScope, ProcessingControlState>>,
    /// Phase 82.13.b.3 — per-scope pending inbound queue.
    /// FIFO `VecDeque`. Capped at `pending_cap` per scope; on
    /// overflow the oldest entry is evicted. A shared cap is
    /// kept on the store so boot can override via env var
    /// without retro-fitting per-call params.
    pending: Arc<DashMap<ProcessingScope, std::collections::VecDeque<PendingInbound>>>,
    /// Phase 82.13.b.3 — max entries per scope. Defaults to
    /// `DEFAULT_PENDING_INBOUNDS_CAP`; production boot reads
    /// `NEXO_PROCESSING_PENDING_QUEUE_CAP` and overrides.
    pending_cap: usize,
}

impl InMemoryProcessingControlStore {
    /// Build an empty store with the default per-scope queue
    /// cap. Daemon boot constructs one shared instance and
    /// feeds it to every per-microapp dispatcher via
    /// `with_processing_domain` so an operator pause reaches
    /// every admin RPC route at once.
    pub fn new() -> Self {
        Self::with_pending_cap(
            nexo_tool_meta::admin::processing::DEFAULT_PENDING_INBOUNDS_CAP,
        )
    }

    /// Phase 82.13.b.3 — build with a custom per-scope queue
    /// cap. Boot wiring uses this when the operator sets
    /// `NEXO_PROCESSING_PENDING_QUEUE_CAP`. `0` disables
    /// buffering entirely (every push reports a drop).
    pub fn with_pending_cap(cap: usize) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            pending: Arc::new(DashMap::new()),
            pending_cap: cap,
        }
    }

    /// Live row count — boot diagnostics + tests.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when no scope is paused.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[async_trait]
impl ProcessingControlStore for InMemoryProcessingControlStore {
    async fn get(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<ProcessingControlState> {
        Ok(self
            .inner
            .get(scope)
            .map(|r| r.value().clone())
            .unwrap_or(ProcessingControlState::AgentActive))
    }

    async fn set(
        &self,
        scope: ProcessingScope,
        state: ProcessingControlState,
    ) -> anyhow::Result<bool> {
        // `changed` semantics: prior value differs from the new
        // one. AgentActive default counts as "no row" so storing
        // it clears the entry.
        let prev = match state {
            ProcessingControlState::AgentActive => self.inner.remove(&scope).map(|(_, v)| v),
            _ => self.inner.insert(scope, state.clone()),
        };
        Ok(prev.as_ref() != Some(&state))
    }

    async fn clear(&self, scope: &ProcessingScope) -> anyhow::Result<bool> {
        // Phase 82.13.b.3 — clearing a scope (resume) does NOT
        // implicitly drain the pending queue here; the resume
        // handler calls `drain_pending` first so it can stamp
        // entries on the transcript before they vanish.
        Ok(self.inner.remove(scope).is_some())
    }

    async fn push_pending(
        &self,
        scope: &ProcessingScope,
        inbound: PendingInbound,
    ) -> anyhow::Result<(usize, u32)> {
        let mut entry = self
            .pending
            .entry(scope.clone())
            .or_insert_with(std::collections::VecDeque::new);
        let mut dropped = 0u32;
        if self.pending_cap == 0 {
            // Cap of 0 disables buffering: count every push as
            // dropped + don't keep the entry.
            dropped = 1;
        } else {
            entry.push_back(inbound);
            // FIFO eviction loop in case the cap was lowered
            // dynamically (env var change between boots).
            while entry.len() > self.pending_cap {
                if entry.pop_front().is_some() {
                    dropped = dropped.saturating_add(1);
                }
            }
        }
        Ok((entry.len(), dropped))
    }

    async fn drain_pending(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<Vec<PendingInbound>> {
        Ok(match self.pending.remove(scope) {
            Some((_, queue)) => queue.into_iter().collect(),
            None => Vec::new(),
        })
    }

    async fn pending_depth(
        &self,
        scope: &ProcessingScope,
    ) -> anyhow::Result<usize> {
        Ok(self
            .pending
            .get(scope)
            .map(|r| r.value().len())
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod processing_store_tests {
    use super::*;
    use nexo_tool_meta::admin::processing::{
        ProcessingControlState, ProcessingScope,
    };

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    #[tokio::test]
    async fn get_returns_agent_active_for_unknown_scope() {
        let store = InMemoryProcessingControlStore::new();
        let state = store.get(&convo()).await.unwrap();
        assert!(matches!(state, ProcessingControlState::AgentActive));
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn set_paused_then_get_round_trip() {
        let store = InMemoryProcessingControlStore::new();
        let paused = ProcessingControlState::PausedByOperator {
            scope: convo(),
            paused_at_ms: 1_700_000_000_000,
            operator_token_hash: "h".into(),
            reason: None,
        };
        let changed = store.set(convo(), paused.clone()).await.unwrap();
        assert!(changed);
        let read = store.get(&convo()).await.unwrap();
        assert_eq!(read, paused);
    }

    #[tokio::test]
    async fn set_same_state_twice_returns_false() {
        let store = InMemoryProcessingControlStore::new();
        let paused = ProcessingControlState::PausedByOperator {
            scope: convo(),
            paused_at_ms: 1,
            operator_token_hash: "h".into(),
            reason: None,
        };
        store.set(convo(), paused.clone()).await.unwrap();
        let changed = store.set(convo(), paused).await.unwrap();
        assert!(!changed, "second identical set is a no-op");
    }

    #[tokio::test]
    async fn clear_removes_row() {
        let store = InMemoryProcessingControlStore::new();
        let paused = ProcessingControlState::PausedByOperator {
            scope: convo(),
            paused_at_ms: 1,
            operator_token_hash: "h".into(),
            reason: None,
        };
        store.set(convo(), paused).await.unwrap();
        let cleared = store.clear(&convo()).await.unwrap();
        assert!(cleared);
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn set_agent_active_drops_row() {
        let store = InMemoryProcessingControlStore::new();
        let paused = ProcessingControlState::PausedByOperator {
            scope: convo(),
            paused_at_ms: 1,
            operator_token_hash: "h".into(),
            reason: None,
        };
        store.set(convo(), paused).await.unwrap();
        // Setting back to AgentActive should drop the row so
        // `get` returns the implicit default without holding
        // the slot in the map.
        let changed = store
            .set(convo(), ProcessingControlState::AgentActive)
            .await
            .unwrap();
        assert!(changed, "transition counts as change");
        assert!(store.is_empty(), "no leftover row");
    }

    // ──────────────────────────────────────────────────────────
    // Phase 82.13.b.3 — pending inbound queue tests.
    // ──────────────────────────────────────────────────────────

    fn other_convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            // Different contact_id → different scope.
            contact_id: "wa.99".into(),
            mcp_channel_source: None,
        }
    }

    fn pending(seq: u64) -> PendingInbound {
        PendingInbound {
            message_id: Some(uuid::Uuid::nil()),
            from_contact_id: format!("wa.55+{seq}"),
            body: format!("msg {seq}"),
            timestamp_ms: 1_700_000_000_000 + seq,
            source_plugin: "whatsapp".into(),
        }
    }

    #[tokio::test]
    async fn push_under_cap_no_drops() {
        let store = InMemoryProcessingControlStore::with_pending_cap(5);
        for i in 0..3 {
            let (depth, dropped) = store.push_pending(&convo(), pending(i)).await.unwrap();
            assert_eq!(depth, (i as usize) + 1);
            assert_eq!(dropped, 0);
        }
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn push_at_cap_drops_oldest_fifo() {
        let store = InMemoryProcessingControlStore::with_pending_cap(2);
        store.push_pending(&convo(), pending(0)).await.unwrap();
        store.push_pending(&convo(), pending(1)).await.unwrap();
        // 3rd push exceeds cap → 1 drop, depth stays at 2.
        let (depth, dropped) = store.push_pending(&convo(), pending(2)).await.unwrap();
        assert_eq!(depth, 2);
        assert_eq!(dropped, 1);
        // Drained queue holds the surviving FIFO order.
        let drained = store.drain_pending(&convo()).await.unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].body, "msg 1");
        assert_eq!(drained[1].body, "msg 2");
    }

    #[tokio::test]
    async fn push_isolates_scopes_from_each_other() {
        let store = InMemoryProcessingControlStore::with_pending_cap(10);
        store.push_pending(&convo(), pending(0)).await.unwrap();
        store
            .push_pending(&other_convo(), pending(99))
            .await
            .unwrap();
        store
            .push_pending(&other_convo(), pending(100))
            .await
            .unwrap();
        // First scope has 1, second has 2.
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 1);
        assert_eq!(store.pending_depth(&other_convo()).await.unwrap(), 2);
        // Drain one doesn't affect the other.
        let _ = store.drain_pending(&convo()).await.unwrap();
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 0);
        assert_eq!(store.pending_depth(&other_convo()).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn drain_returns_fifo_and_clears() {
        let store = InMemoryProcessingControlStore::with_pending_cap(10);
        for i in 0..4 {
            store.push_pending(&convo(), pending(i)).await.unwrap();
        }
        let drained = store.drain_pending(&convo()).await.unwrap();
        assert_eq!(drained.len(), 4);
        for (i, p) in drained.iter().enumerate() {
            assert_eq!(p.body, format!("msg {i}"));
        }
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 0);
        // Second drain returns empty (no error).
        assert!(store.drain_pending(&convo()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pending_depth_for_unknown_scope_is_zero() {
        let store = InMemoryProcessingControlStore::new();
        assert_eq!(store.pending_depth(&convo()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cap_zero_treats_every_push_as_dropped() {
        let store = InMemoryProcessingControlStore::with_pending_cap(0);
        let (depth, dropped) = store.push_pending(&convo(), pending(0)).await.unwrap();
        assert_eq!(depth, 0);
        assert_eq!(dropped, 1);
        assert!(store.drain_pending(&convo()).await.unwrap().is_empty());
    }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 82.14 — in-memory `EscalationStore` adapter.
// Mirrors the in-memory processing store. Durable SQLite
// variant is a 82.14.b follow-up alongside the
// `escalate_to_human` built-in tool.
// ─────────────────────────────────────────────────────────────────────

/// In-memory `EscalationStore` adapter.
#[derive(Debug, Default, Clone)]
pub struct InMemoryEscalationStore {
    inner: Arc<DashMap<ProcessingScope, EscalationEntry>>,
}

impl InMemoryEscalationStore {
    /// Build an empty store.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Live row count — boot diagnostics + tests.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when no escalations are tracked.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[async_trait]
impl EscalationStore for InMemoryEscalationStore {
    async fn list(
        &self,
        filter: &EscalationsListParams,
    ) -> anyhow::Result<Vec<EscalationEntry>> {
        let mut out: Vec<EscalationEntry> = self
            .inner
            .iter()
            .filter(|r| filter_matches(r.value(), filter))
            .map(|r| r.value().clone())
            .collect();
        // Newest first by request/resolve time. Pending wins
        // over resolved at the same scope (latest mutation
        // takes precedence).
        out.sort_by_key(|e| match &e.state {
            EscalationState::Pending {
                requested_at_ms, ..
            } => std::cmp::Reverse(*requested_at_ms),
            EscalationState::Resolved {
                resolved_at_ms, ..
            } => std::cmp::Reverse(*resolved_at_ms),
            _ => std::cmp::Reverse(0u64),
        });
        out.truncate(filter.limit);
        Ok(out)
    }

    async fn get(&self, scope: &ProcessingScope) -> anyhow::Result<EscalationState> {
        Ok(self
            .inner
            .get(scope)
            .map(|r| r.value().state.clone())
            .unwrap_or(EscalationState::None))
    }

    async fn resolve(
        &self,
        scope: &ProcessingScope,
        by: ResolvedBy,
        resolved_at_ms: u64,
    ) -> anyhow::Result<bool> {
        let Some(mut entry_ref) = self.inner.get_mut(scope) else {
            return Ok(false);
        };
        if !matches!(entry_ref.state, EscalationState::Pending { .. }) {
            return Ok(false);
        }
        entry_ref.state = EscalationState::Resolved {
            scope: scope.clone(),
            resolved_at_ms,
            by,
        };
        Ok(true)
    }

    async fn upsert_pending(
        &self,
        agent_id: String,
        state: EscalationState,
    ) -> anyhow::Result<bool> {
        let scope = match &state {
            EscalationState::Pending { scope, .. }
            | EscalationState::Resolved { scope, .. } => scope.clone(),
            _ => anyhow::bail!("upsert_pending requires Pending or Resolved state"),
        };
        let entry = EscalationEntry {
            agent_id,
            scope: scope.clone(),
            state,
        };
        let prev = self.inner.insert(scope, entry);
        Ok(prev.is_none())
    }
}

#[cfg(test)]
mod escalation_store_tests {
    use super::*;
    use nexo_tool_meta::admin::escalations::{
        EscalationReason, EscalationUrgency, EscalationsListFilter,
    };
    use std::collections::BTreeMap;

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    fn pending(scope: ProcessingScope, ts: u64) -> EscalationState {
        EscalationState::Pending {
            scope,
            summary: "x".into(),
            reason: EscalationReason::OutOfScope,
            urgency: EscalationUrgency::Normal,
            context: BTreeMap::new(),
            requested_at_ms: ts,
        }
    }

    #[tokio::test]
    async fn list_returns_pending_newest_first() {
        let store = InMemoryEscalationStore::new();
        store
            .upsert_pending("ana".into(), pending(convo(), 1_000))
            .await
            .unwrap();
        let scope_b = ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.99".into(),
            mcp_channel_source: None,
        };
        store
            .upsert_pending("ana".into(), pending(scope_b.clone(), 2_000))
            .await
            .unwrap();
        let rows = store
            .list(&EscalationsListParams {
                filter: EscalationsListFilter::Pending,
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        match &rows[0].state {
            EscalationState::Pending {
                requested_at_ms, ..
            } => assert_eq!(*requested_at_ms, 2_000, "newest first"),
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_filters_by_agent_id() {
        let store = InMemoryEscalationStore::new();
        store
            .upsert_pending("ana".into(), pending(convo(), 1_000))
            .await
            .unwrap();
        let other_scope = ProcessingScope::Conversation {
            agent_id: "bob".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.99".into(),
            mcp_channel_source: None,
        };
        store
            .upsert_pending("bob".into(), pending(other_scope, 1_000))
            .await
            .unwrap();
        let rows = store
            .list(&EscalationsListParams {
                filter: EscalationsListFilter::Pending,
                agent_id: Some("ana".into()),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "ana");
    }

    #[tokio::test]
    async fn resolve_pending_flips_to_resolved() {
        let store = InMemoryEscalationStore::new();
        store
            .upsert_pending("ana".into(), pending(convo(), 1_000))
            .await
            .unwrap();
        let changed = store
            .resolve(&convo(), ResolvedBy::OperatorTakeover, 2_000)
            .await
            .unwrap();
        assert!(changed);
        let state = store.get(&convo()).await.unwrap();
        assert!(matches!(state, EscalationState::Resolved { .. }));
        // Resolving an already-resolved row is a no-op.
        let again = store
            .resolve(&convo(), ResolvedBy::OperatorTakeover, 3_000)
            .await
            .unwrap();
        assert!(!again);
    }

    #[tokio::test]
    async fn resolve_unknown_scope_returns_false() {
        let store = InMemoryEscalationStore::new();
        let changed = store
            .resolve(&convo(), ResolvedBy::OperatorTakeover, 1_000)
            .await
            .unwrap();
        assert!(!changed);
    }

    #[tokio::test]
    async fn list_resolved_filter_excludes_pending() {
        let store = InMemoryEscalationStore::new();
        store
            .upsert_pending("ana".into(), pending(convo(), 1_000))
            .await
            .unwrap();
        // Pending → Resolved
        let scope_b = ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "acc".into(),
            contact_id: "wa.99".into(),
            mcp_channel_source: None,
        };
        store
            .upsert_pending("ana".into(), pending(scope_b.clone(), 1_500))
            .await
            .unwrap();
        store
            .resolve(&scope_b, ResolvedBy::OperatorTakeover, 2_500)
            .await
            .unwrap();

        let rows = store
            .list(&EscalationsListParams {
                filter: EscalationsListFilter::Resolved,
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].state, EscalationState::Resolved { .. }));
    }

    // ── FsSkillsStore tests ──────────────────────────────────────

    fn skills_tmp() -> PathBuf {
        std::env::temp_dir().join(format!("nexo-fs-skills-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn fs_skills_upsert_then_list_round_trip() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let (rec, created) = store
            .upsert(SkillsUpsertParams {
                name: "weather".into(),
                display_name: Some("Weather".into()),
                description: Some("Forecasts.".into()),
                body: "Use for forecasts.".into(),
                max_chars: None,
                requires: None,
            })
            .await
            .unwrap();
        assert!(created);
        assert_eq!(rec.name, "weather");
        let list = store.list(None).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "weather");
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_list_empty_root_is_not_error() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let list = store.list(None).await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn fs_skills_get_missing_returns_none() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let r = store.get("ghost").await.unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn fs_skills_upsert_twice_reports_created_false_second() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let p = SkillsUpsertParams {
            name: "twice".into(),
            display_name: None,
            description: None,
            body: "first".into(),
            max_chars: None,
            requires: None,
        };
        let (_, c1) = store.upsert(p.clone()).await.unwrap();
        let (rec2, c2) = store
            .upsert(SkillsUpsertParams {
                body: "second".into(),
                ..p
            })
            .await
            .unwrap();
        assert!(c1);
        assert!(!c2);
        assert_eq!(rec2.body, "second");
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_delete_missing_returns_false() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let d = store.delete("absent").await.unwrap();
        assert!(!d);
    }

    #[tokio::test]
    async fn fs_skills_delete_then_get_returns_none() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let _ = store
            .upsert(SkillsUpsertParams {
                name: "tmp".into(),
                display_name: None,
                description: None,
                body: "x".into(),
                max_chars: None,
                requires: None,
            })
            .await
            .unwrap();
        let d = store.delete("tmp").await.unwrap();
        assert!(d);
        let r = store.get("tmp").await.unwrap();
        assert!(r.is_none());
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_path_traversal_rejected() {
        let root = skills_tmp();
        tokio::fs::create_dir_all(&root).await.unwrap();
        let store = FsSkillsStore::new(&root);
        // resolve_skill_path is the gatekeeper; bad names must not
        // escape the root.
        let r = store.resolve_skill_path("../escape");
        assert!(r.is_err(), "path traversal must be rejected");
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_on_change_callback_fires_after_upsert_and_delete() {
        let root = skills_tmp();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_for_cb = counter.clone();
        let store = FsSkillsStore::new(&root).with_on_change(Arc::new(move || {
            counter_for_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let _ = store
            .upsert(SkillsUpsertParams {
                name: "cb".into(),
                display_name: None,
                description: None,
                body: "body".into(),
                max_chars: None,
                requires: None,
            })
            .await
            .unwrap();
        let _ = store.delete("cb").await.unwrap();
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_list_prefix_filters() {
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        for name in ["alpha", "beta", "alpine"] {
            let _ = store
                .upsert(SkillsUpsertParams {
                    name: name.into(),
                    display_name: None,
                    description: None,
                    body: "x".into(),
                    max_chars: None,
                    requires: None,
                })
                .await
                .unwrap();
        }
        let list = store.list(Some("alp")).await.unwrap();
        let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "alpine"]);
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_frontmatter_round_trips_through_skill_loader_format() {
        // Write via FsSkillsStore, read raw bytes, confirm shape
        // matches what crate::agent::skills::SkillLoader expects:
        // `---\n<yaml>\n---\n\n<body>`.
        let root = skills_tmp();
        let store = FsSkillsStore::new(&root);
        let _ = store
            .upsert(SkillsUpsertParams {
                name: "rt".into(),
                display_name: Some("Round Trip".into()),
                description: Some("Probe.".into()),
                body: "body line".into(),
                max_chars: Some(1024),
                requires: None,
            })
            .await
            .unwrap();
        let blob = tokio::fs::read_to_string(root.join("rt").join("SKILL.md"))
            .await
            .unwrap();
        assert!(blob.starts_with("---\n"));
        assert!(blob.contains("name: Round Trip"));
        assert!(blob.contains("description: Probe."));
        assert!(blob.contains("max_chars: 1024"));
        assert!(blob.contains("\n---\n\nbody line"));
        // Read back through FsSkillsStore — frontmatter parses
        // back into typed fields.
        let rec = store.get("rt").await.unwrap().unwrap();
        assert_eq!(rec.display_name.as_deref(), Some("Round Trip"));
        assert_eq!(rec.description.as_deref(), Some("Probe."));
        assert_eq!(rec.max_chars, Some(1024));
        assert_eq!(rec.body, "body line");
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn fs_skills_concurrent_upserts_same_name_dont_corrupt() {
        let root = skills_tmp();
        let store = Arc::new(FsSkillsStore::new(&root));
        let mut handles = Vec::new();
        for i in 0..16 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                s.upsert(SkillsUpsertParams {
                    name: "race".into(),
                    display_name: None,
                    description: None,
                    body: format!("v{i}"),
                    max_chars: None,
                    requires: None,
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let rec = store.get("race").await.unwrap().unwrap();
        // Body must be one of the 16 versions, not garbage.
        assert!(rec.body.starts_with("v"));
        let n: i32 = rec.body[1..].parse().unwrap();
        assert!((0..16).contains(&n));
        tokio::fs::remove_dir_all(&root).await.ok();
    }
}

// ── Phase 83.8.4.b — production ChannelOutboundDispatcher ────────

/// Per-plugin payload translator. Each plugin's existing
/// outbound dispatcher (`crates/plugins/<channel>/src/dispatch.rs`
/// and friends) listens on its own NATS topic and consumes its
/// own JSON shape. The translator owns the `OutboundMessage` →
/// `(topic, payload)` projection so the
/// `BrokerOutboundDispatcher` stays channel-agnostic.
pub trait ChannelPayloadTranslator: Send + Sync + std::fmt::Debug {
    /// Channel name this translator serves
    /// (`"whatsapp"` / `"telegram"` / `"email"` / future).
    fn channel(&self) -> &str;

    /// Translate the operator-driven `OutboundMessage` into the
    /// `(topic, payload)` pair the channel plugin's existing
    /// dispatcher consumes. Errors propagate to the admin RPC
    /// layer as `-32602 invalid_params`.
    fn translate(
        &self,
        msg: &OutboundMessage,
    ) -> Result<(String, serde_json::Value), TranslationError>;
}

/// Errors a translator may return. Map to JSON-RPC at the
/// dispatcher layer.
#[derive(Debug, thiserror::Error)]
pub enum TranslationError {
    /// Required field absent from the `OutboundMessage`.
    #[error("missing field: {0}")]
    MissingField(&'static str),
    /// Field present but value cannot be honoured.
    #[error("invalid value for {field}: {reason}")]
    InvalidValue {
        /// Offending field name.
        field: &'static str,
        /// Why the value was rejected.
        reason: String,
    },
    /// `msg_kind` not implemented by this translator.
    #[error("unsupported msg_kind: {0}")]
    UnsupportedKind(String),
}

/// Production [`ChannelOutboundDispatcher`] that publishes the
/// translated payload to the broker. Each per-channel
/// `plugin.outbound.<channel>[.<account>]` topic already has a
/// plugin-side dispatcher subscribed (see
/// `crates/plugins/whatsapp/src/dispatch.rs` for the canonical
/// shape) — this adapter is just the operator-driven entry
/// point.
pub struct BrokerOutboundDispatcher {
    broker: AnyBroker,
    translators: std::collections::HashMap<String, Box<dyn ChannelPayloadTranslator>>,
}

impl std::fmt::Debug for BrokerOutboundDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let channels: Vec<&String> = self.translators.keys().collect();
        f.debug_struct("BrokerOutboundDispatcher")
            .field("channels", &channels)
            .finish()
    }
}

impl BrokerOutboundDispatcher {
    /// Build a dispatcher with the given broker handle. Add
    /// translators with [`Self::with_translator`] before
    /// installing into the admin RPC dispatcher.
    pub fn new(broker: AnyBroker) -> Self {
        Self {
            broker,
            translators: std::collections::HashMap::new(),
        }
    }

    /// Register a translator for one channel. Re-adding the
    /// same channel name overwrites the previous translator.
    pub fn with_translator(
        mut self,
        translator: Box<dyn ChannelPayloadTranslator>,
    ) -> Self {
        self.translators
            .insert(translator.channel().to_string(), translator);
        self
    }
}

#[async_trait]
impl ChannelOutboundDispatcher for BrokerOutboundDispatcher {
    async fn send(
        &self,
        msg: OutboundMessage,
    ) -> Result<OutboundAck, ChannelOutboundError> {
        let translator = self
            .translators
            .get(&msg.channel)
            .ok_or_else(|| ChannelOutboundError::ChannelUnavailable(msg.channel.clone()))?;
        let (topic, payload) = translator
            .translate(&msg)
            .map_err(|e| match e {
                TranslationError::MissingField(_)
                | TranslationError::InvalidValue { .. }
                | TranslationError::UnsupportedKind(_) => {
                    ChannelOutboundError::InvalidParams(e.to_string())
                }
            })?;
        let event = Event::new(&topic, &msg.channel, payload);
        self.broker
            .publish(&topic, event)
            .await
            .map_err(|e| ChannelOutboundError::Transport(e.to_string()))?;
        // Plugin dispatchers are fire-and-forget; outbound
        // message id correlation is logged in FOLLOWUPS as
        // 83.8.4.c.
        Ok(OutboundAck {
            outbound_message_id: None,
        })
    }
}

/// Telegram translator. Topic is
/// `plugin.outbound.telegram[.<account_id>]` matching the
/// plugin's `tool.rs::publish_outbound` convention. Payload
/// mirrors what `TelegramSendMessageTool` publishes:
/// `{ "to": chat_id (i64), "text": body }`. Reply variant adds
/// `reply_to_message_id` (parsed from `OutboundMessage.reply_to_msg_id`).
///
/// `OutboundMessage.to` is parsed as `i64` — Telegram chat ids
/// are signed 64-bit (negative for groups/channels). The
/// translator returns `InvalidValue` on parse failure so the
/// admin RPC layer surfaces a typed `-32602`.
#[derive(Debug, Default)]
pub struct TelegramTranslator;

impl ChannelPayloadTranslator for TelegramTranslator {
    fn channel(&self) -> &str {
        "telegram"
    }

    fn translate(
        &self,
        msg: &OutboundMessage,
    ) -> Result<(String, serde_json::Value), TranslationError> {
        let topic = if msg.account_id.is_empty() || msg.account_id == "default" {
            "plugin.outbound.telegram".to_string()
        } else {
            format!("plugin.outbound.telegram.{}", msg.account_id)
        };
        let chat_id: i64 = msg.to.parse().map_err(|e: std::num::ParseIntError| {
            TranslationError::InvalidValue {
                field: "to",
                reason: format!("Telegram chat_id must be i64: {e}"),
            }
        })?;
        let payload = match msg.msg_kind.as_str() {
            "text" => serde_json::json!({
                "to": chat_id,
                "text": msg.body,
            }),
            "reply" => {
                let reply_id_raw = msg
                    .reply_to_msg_id
                    .as_deref()
                    .ok_or(TranslationError::MissingField("reply_to_msg_id"))?;
                let reply_id: i64 = reply_id_raw.parse().map_err(
                    |e: std::num::ParseIntError| TranslationError::InvalidValue {
                        field: "reply_to_msg_id",
                        reason: format!("Telegram message id must be i64: {e}"),
                    },
                )?;
                serde_json::json!({
                    "to": chat_id,
                    "text": msg.body,
                    "reply_to_message_id": reply_id,
                })
            }
            other => return Err(TranslationError::UnsupportedKind(other.into())),
        };
        Ok((topic, payload))
    }
}

/// Email translator. Topic is
/// `plugin.outbound.email[.<account_id>]` matching the email
/// plugin's `outbound_topic_for(instance)` convention
/// (`crates/plugins/email/src/plugin.rs::outbound_topic_for`).
/// Payload mirrors `crates/plugins/email/src/events.rs::OutboundCommand`
/// — single-recipient list `[to]`, subject parsed from
/// `OutboundMessage.attachments[0].subject` (operator UI passes
/// it as a header), and body from `OutboundMessage.body`. CC /
/// BCC / multi-recipient is out of scope for v1 takeover (the
/// operator is replying to ONE conversation).
///
/// `msg_kind == "reply"` maps `reply_to_msg_id` to the
/// `in_reply_to` Message-ID header so the threading
/// stays coherent with the inbound message the operator is
/// answering.
#[derive(Debug, Default)]
pub struct EmailTranslator;

impl ChannelPayloadTranslator for EmailTranslator {
    fn channel(&self) -> &str {
        "email"
    }

    fn translate(
        &self,
        msg: &OutboundMessage,
    ) -> Result<(String, serde_json::Value), TranslationError> {
        let topic = if msg.account_id.is_empty() || msg.account_id == "default" {
            "plugin.outbound.email".to_string()
        } else {
            format!("plugin.outbound.email.{}", msg.account_id)
        };
        // Subject is required by the email plugin. Operator UI
        // ships it through the `attachments[0]` extras slot
        // (free-form Value) since `OutboundMessage` does not
        // have a `subject` field. Defaults to `"(no subject)"`
        // when absent — matches the lenient default the email
        // plugin already accepts on the `email_send` tool.
        let subject = msg
            .attachments
            .first()
            .and_then(|a| a.get("subject"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(no subject)");
        let mut payload = serde_json::json!({
            "to": [msg.to.clone()],
            "subject": subject,
            "body": msg.body,
        });
        if msg.msg_kind == "reply" {
            let in_reply_to = msg
                .reply_to_msg_id
                .as_deref()
                .ok_or(TranslationError::MissingField("reply_to_msg_id"))?;
            payload["in_reply_to"] = serde_json::Value::String(in_reply_to.to_string());
        } else if msg.msg_kind != "text" {
            return Err(TranslationError::UnsupportedKind(msg.msg_kind.clone()));
        }
        Ok((topic, payload))
    }
}

/// WhatsApp translator. Topic is
/// `nexo_plugin_whatsapp::dispatch::TOPIC_OUTBOUND` plus an
/// optional `.<account_id>` suffix matching the plugin's
/// existing convention. Payload mirrors the plugin's
/// `OutboundPayload` (text, reply, media variants).
#[derive(Debug, Default)]
pub struct WhatsAppTranslator;

impl ChannelPayloadTranslator for WhatsAppTranslator {
    fn channel(&self) -> &str {
        "whatsapp"
    }

    fn translate(
        &self,
        msg: &OutboundMessage,
    ) -> Result<(String, serde_json::Value), TranslationError> {
        let base = nexo_plugin_whatsapp::dispatch::TOPIC_OUTBOUND;
        let topic = if msg.account_id.is_empty() || msg.account_id == "default" {
            base.to_string()
        } else {
            format!("{base}.{}", msg.account_id)
        };
        let payload = match msg.msg_kind.as_str() {
            "text" => serde_json::json!({
                "to": msg.to,
                "text": msg.body,
                "kind": "text",
            }),
            "reply" => {
                let msg_id = msg
                    .reply_to_msg_id
                    .as_deref()
                    .ok_or(TranslationError::MissingField("reply_to_msg_id"))?;
                serde_json::json!({
                    "kind": "reply",
                    "name": "reply",
                    "payload": { "to": msg.to, "msg_id": msg_id, "text": msg.body },
                })
            }
            "media" => {
                let attachment = msg.attachments.first().ok_or(
                    TranslationError::MissingField("attachments[0]"),
                )?;
                let url = attachment
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(TranslationError::MissingField("attachments[0].url"))?;
                let mut p = serde_json::json!({
                    "kind": "media",
                    "to": msg.to,
                    "url": url,
                    "caption": msg.body,
                });
                if let Some(file_name) = attachment.get("file_name") {
                    p["file_name"] = file_name.clone();
                }
                p
            }
            other => return Err(TranslationError::UnsupportedKind(other.into())),
        };
        Ok((topic, payload))
    }
}

#[cfg(test)]
mod broker_outbound_tests {
    use super::*;
    use nexo_broker::LocalBroker;
    use serde_json::json;

    fn text_msg() -> OutboundMessage {
        OutboundMessage {
            channel: "whatsapp".into(),
            account_id: "wa.0".into(),
            to: "wa.55".into(),
            body: "hi".into(),
            msg_kind: "text".into(),
            attachments: vec![],
            reply_to_msg_id: None,
        }
    }

    #[test]
    fn whatsapp_translator_text_round_trips() {
        let (topic, payload) = WhatsAppTranslator.translate(&text_msg()).unwrap();
        assert_eq!(topic, "plugin.outbound.whatsapp.wa.0");
        assert_eq!(payload["to"], json!("wa.55"));
        assert_eq!(payload["text"], json!("hi"));
        assert_eq!(payload["kind"], json!("text"));
    }

    #[test]
    fn whatsapp_translator_default_account_uses_base_topic() {
        let mut msg = text_msg();
        msg.account_id = "default".into();
        let (topic, _) = WhatsAppTranslator.translate(&msg).unwrap();
        assert_eq!(topic, "plugin.outbound.whatsapp");
        msg.account_id = "".into();
        let (topic2, _) = WhatsAppTranslator.translate(&msg).unwrap();
        assert_eq!(topic2, "plugin.outbound.whatsapp");
    }

    #[test]
    fn whatsapp_translator_reply_includes_msg_id() {
        let mut msg = text_msg();
        msg.msg_kind = "reply".into();
        msg.reply_to_msg_id = Some("ABC123".into());
        let (_, payload) = WhatsAppTranslator.translate(&msg).unwrap();
        assert_eq!(payload["kind"], json!("reply"));
        assert_eq!(payload["payload"]["msg_id"], json!("ABC123"));
        assert_eq!(payload["payload"]["text"], json!("hi"));
    }

    #[test]
    fn whatsapp_translator_reply_missing_msg_id_errors() {
        let mut msg = text_msg();
        msg.msg_kind = "reply".into();
        let r = WhatsAppTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::MissingField("reply_to_msg_id"))));
    }

    #[test]
    fn whatsapp_translator_media_reads_first_attachment() {
        let mut msg = text_msg();
        msg.msg_kind = "media".into();
        msg.body = "look".into();
        msg.attachments = vec![json!({
            "url": "https://x/y.jpg",
            "file_name": "y.jpg"
        })];
        let (_, payload) = WhatsAppTranslator.translate(&msg).unwrap();
        assert_eq!(payload["kind"], json!("media"));
        assert_eq!(payload["url"], json!("https://x/y.jpg"));
        assert_eq!(payload["caption"], json!("look"));
        assert_eq!(payload["file_name"], json!("y.jpg"));
    }

    #[test]
    fn whatsapp_translator_media_missing_url_errors() {
        let mut msg = text_msg();
        msg.msg_kind = "media".into();
        msg.attachments = vec![json!({})];
        let r = WhatsAppTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::MissingField("attachments[0].url"))));
    }

    #[test]
    fn whatsapp_translator_unknown_kind_errors() {
        let mut msg = text_msg();
        msg.msg_kind = "voice".into();
        let r = WhatsAppTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::UnsupportedKind(s)) if s == "voice"));
    }

    #[tokio::test]
    async fn broker_dispatcher_routes_known_channel_to_translator() {
        let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
        let mut sub = broker
            .subscribe("plugin.outbound.whatsapp.wa.0")
            .await
            .unwrap();
        let dispatcher = BrokerOutboundDispatcher::new(broker)
            .with_translator(Box::new(WhatsAppTranslator));
        let ack = dispatcher.send(text_msg()).await.unwrap();
        assert!(ack.outbound_message_id.is_none());
        let evt = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sub.next(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(evt.payload["text"], json!("hi"));
        assert_eq!(evt.payload["to"], json!("wa.55"));
    }

    #[tokio::test]
    async fn broker_dispatcher_unknown_channel_returns_channel_unavailable() {
        let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
        let dispatcher = BrokerOutboundDispatcher::new(broker)
            .with_translator(Box::new(WhatsAppTranslator));
        let mut msg = text_msg();
        msg.channel = "telegram".into();
        let r = dispatcher.send(msg).await;
        assert!(matches!(r, Err(ChannelOutboundError::ChannelUnavailable(s)) if s == "telegram"));
    }

    #[tokio::test]
    async fn broker_dispatcher_translator_error_maps_to_invalid_params() {
        let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
        let dispatcher = BrokerOutboundDispatcher::new(broker)
            .with_translator(Box::new(WhatsAppTranslator));
        let mut msg = text_msg();
        msg.msg_kind = "voice".into();
        let r = dispatcher.send(msg).await;
        assert!(matches!(r, Err(ChannelOutboundError::InvalidParams(_))));
    }

    // ── Telegram translator ─────────────────────────────────

    fn telegram_msg() -> OutboundMessage {
        OutboundMessage {
            channel: "telegram".into(),
            account_id: "primary".into(),
            to: "12345".into(),
            body: "hola".into(),
            msg_kind: "text".into(),
            attachments: vec![],
            reply_to_msg_id: None,
        }
    }

    #[test]
    fn telegram_translator_text_round_trips() {
        let (topic, payload) = TelegramTranslator.translate(&telegram_msg()).unwrap();
        assert_eq!(topic, "plugin.outbound.telegram.primary");
        assert_eq!(payload["to"], json!(12345));
        assert_eq!(payload["text"], json!("hola"));
    }

    #[test]
    fn telegram_translator_negative_chat_id_for_groups() {
        let mut msg = telegram_msg();
        msg.to = "-100123456".into();
        let (_, payload) = TelegramTranslator.translate(&msg).unwrap();
        assert_eq!(payload["to"], json!(-100123456));
    }

    #[test]
    fn telegram_translator_non_numeric_to_errors_invalid_value() {
        let mut msg = telegram_msg();
        msg.to = "wa.55@s.whatsapp.net".into(); // clearly non-numeric
        let r = TelegramTranslator.translate(&msg);
        assert!(matches!(
            r,
            Err(TranslationError::InvalidValue { field: "to", .. })
        ));
    }

    #[test]
    fn telegram_translator_reply_includes_reply_to_message_id() {
        let mut msg = telegram_msg();
        msg.msg_kind = "reply".into();
        msg.reply_to_msg_id = Some("999".into());
        let (_, payload) = TelegramTranslator.translate(&msg).unwrap();
        assert_eq!(payload["reply_to_message_id"], json!(999));
    }

    #[test]
    fn telegram_translator_reply_missing_id_errors() {
        let mut msg = telegram_msg();
        msg.msg_kind = "reply".into();
        let r = TelegramTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::MissingField("reply_to_msg_id"))));
    }

    #[test]
    fn telegram_translator_default_account_uses_base_topic() {
        let mut msg = telegram_msg();
        msg.account_id = "default".into();
        let (topic, _) = TelegramTranslator.translate(&msg).unwrap();
        assert_eq!(topic, "plugin.outbound.telegram");
    }

    #[test]
    fn telegram_translator_unknown_kind_errors() {
        let mut msg = telegram_msg();
        msg.msg_kind = "media".into();
        let r = TelegramTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::UnsupportedKind(_))));
    }

    // ── Email translator ────────────────────────────────────

    fn email_msg() -> OutboundMessage {
        OutboundMessage {
            channel: "email".into(),
            account_id: "ops".into(),
            to: "ana@example.com".into(),
            body: "Hola, te respondo desde el operador.".into(),
            msg_kind: "text".into(),
            attachments: vec![json!({ "subject": "Re: tu consulta" })],
            reply_to_msg_id: None,
        }
    }

    #[test]
    fn email_translator_text_round_trips() {
        let (topic, payload) = EmailTranslator.translate(&email_msg()).unwrap();
        assert_eq!(topic, "plugin.outbound.email.ops");
        assert_eq!(payload["to"], json!(["ana@example.com"]));
        assert_eq!(payload["subject"], json!("Re: tu consulta"));
        assert_eq!(payload["body"], json!("Hola, te respondo desde el operador."));
    }

    #[test]
    fn email_translator_default_subject_when_attachments_empty() {
        let mut msg = email_msg();
        msg.attachments = vec![];
        let (_, payload) = EmailTranslator.translate(&msg).unwrap();
        assert_eq!(payload["subject"], json!("(no subject)"));
    }

    #[test]
    fn email_translator_reply_threads_in_reply_to_header() {
        let mut msg = email_msg();
        msg.msg_kind = "reply".into();
        msg.reply_to_msg_id = Some("<abc@x>".into());
        let (_, payload) = EmailTranslator.translate(&msg).unwrap();
        assert_eq!(payload["in_reply_to"], json!("<abc@x>"));
    }

    #[test]
    fn email_translator_reply_missing_id_errors() {
        let mut msg = email_msg();
        msg.msg_kind = "reply".into();
        let r = EmailTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::MissingField("reply_to_msg_id"))));
    }

    #[test]
    fn email_translator_default_account_uses_base_topic() {
        let mut msg = email_msg();
        msg.account_id = "".into();
        let (topic, _) = EmailTranslator.translate(&msg).unwrap();
        assert_eq!(topic, "plugin.outbound.email");
    }

    #[test]
    fn email_translator_unknown_kind_errors() {
        let mut msg = email_msg();
        msg.msg_kind = "media".into();
        let r = EmailTranslator.translate(&msg);
        assert!(matches!(r, Err(TranslationError::UnsupportedKind(_))));
    }

    #[tokio::test]
    async fn dispatcher_routes_telegram_payload_to_topic() {
        let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
        let mut sub = broker.subscribe("plugin.outbound.telegram.primary").await.unwrap();
        let dispatcher = BrokerOutboundDispatcher::new(broker)
            .with_translator(Box::new(TelegramTranslator));
        dispatcher.send(telegram_msg()).await.unwrap();
        let evt = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sub.next(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(evt.payload["to"], json!(12345));
    }

    #[tokio::test]
    async fn dispatcher_routes_email_payload_to_topic() {
        let broker: AnyBroker = AnyBroker::Local(LocalBroker::new());
        let mut sub = broker.subscribe("plugin.outbound.email.ops").await.unwrap();
        let dispatcher = BrokerOutboundDispatcher::new(broker)
            .with_translator(Box::new(EmailTranslator));
        dispatcher.send(email_msg()).await.unwrap();
        let evt = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            sub.next(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(evt.payload["to"], json!(["ana@example.com"]));
        assert_eq!(evt.payload["body"], json!("Hola, te respondo desde el operador."));
    }
}

// ── Phase 83.8.12.3 — TenantsYamlPatcher ─────────────────────────

/// Filesystem-backed [`TenantStore`] implementation. Mirrors the
/// pattern of `AgentsYamlPatcher` + `FsSkillsStore`: holds the
/// `config/tenants.yaml` path, performs atomic writes via
/// `tmp+rename`, computes `agent_count` for [`TenantSummary`] on
/// demand by scanning `agents.yaml` for the matching `tenant_id`.
///
/// Concurrency: a single per-instance `tokio::sync::Mutex<()>`
/// serialises all writes (the file is small and writes are
/// infrequent — the operator UI gates them). Reads do not lock.
pub struct TenantsYamlPatcher {
    path: PathBuf,
    /// `agents.yaml` path used to compute `agent_count` and the
    /// orphan list during `delete(purge=false)`.
    agents_yaml_path: PathBuf,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for TenantsYamlPatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantsYamlPatcher")
            .field("path", &self.path)
            .field("agents_yaml_path", &self.agents_yaml_path)
            .finish()
    }
}

impl TenantsYamlPatcher {
    /// Build a patcher over `tenants.yaml` and the matching
    /// `agents.yaml` (used for orphan detection).
    pub fn new(
        tenants_yaml: impl Into<PathBuf>,
        agents_yaml: impl Into<PathBuf>,
    ) -> Self {
        Self {
            path: tenants_yaml.into(),
            agents_yaml_path: agents_yaml.into(),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Read the entire tenants.yaml into a typed Vec<TenantDetail>.
    /// Returns empty Vec when the file is absent (legacy / fresh
    /// deployment).
    fn read_all(&self) -> anyhow::Result<Vec<TenantDetail>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        #[derive(serde::Deserialize)]
        struct File {
            #[serde(default)]
            tenants: Vec<TenantDetail>,
        }
        let parsed: File = serde_yaml::from_str(&raw)?;
        Ok(parsed.tenants)
    }

    /// Write the entire tenants.yaml atomically (tmp + rename).
    fn write_all(&self, tenants: &[TenantDetail]) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[derive(serde::Serialize)]
        struct File<'a> {
            tenants: &'a [TenantDetail],
        }
        let body = serde_yaml::to_string(&File { tenants })?;
        let tmp = self.path.with_extension("yaml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn agent_count(&self, tenant_id: &str) -> usize {
        crate::yaml_patch::list_agents_by_tenant(&self.agents_yaml_path, tenant_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

#[async_trait]
impl TenantStore for TenantsYamlPatcher {
    async fn list(
        &self,
        filter: &TenantsListFilter,
    ) -> anyhow::Result<Vec<TenantSummary>> {
        let all = self.read_all()?;
        let mut out: Vec<TenantSummary> = all
            .into_iter()
            .filter(|t| !filter.active_only || t.active)
            .filter(|t| match &filter.prefix {
                Some(p) => t.id.starts_with(p),
                None => true,
            })
            .map(|t| TenantSummary {
                id: t.id.clone(),
                display_name: t.display_name,
                active: t.active,
                agent_count: self.agent_count(&t.id),
                created_at: t.created_at,
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    async fn get(&self, tenant_id: &str) -> anyhow::Result<Option<TenantDetail>> {
        let all = self.read_all()?;
        Ok(all.into_iter().find(|t| t.id == tenant_id))
    }

    async fn upsert(
        &self,
        params: TenantsUpsertInput,
    ) -> anyhow::Result<(TenantDetail, bool)> {
        let _guard = self.write_lock.lock().await;
        let mut all = self.read_all()?;
        let existing_idx = all.iter().position(|t| t.id == params.id);
        let created = existing_idx.is_none();
        let detail = if let Some(idx) = existing_idx {
            // Update path: preserve created_at, override fields.
            let prev = all[idx].clone();
            TenantDetail {
                id: params.id,
                display_name: params.display_name,
                active: params.active.unwrap_or(prev.active),
                created_at: prev.created_at,
                llm_provider_refs: params
                    .llm_provider_refs
                    .unwrap_or(prev.llm_provider_refs),
                metadata: params.metadata.unwrap_or(prev.metadata),
            }
        } else {
            // Create path: stamp now() for created_at.
            TenantDetail {
                id: params.id,
                display_name: params.display_name,
                active: params.active.unwrap_or(true),
                created_at: chrono::Utc::now(),
                llm_provider_refs: params.llm_provider_refs.unwrap_or_default(),
                metadata: params.metadata.unwrap_or_default(),
            }
        };
        if let Some(idx) = existing_idx {
            all[idx] = detail.clone();
        } else {
            all.push(detail.clone());
        }
        all.sort_by(|a, b| a.id.cmp(&b.id));
        self.write_all(&all)?;
        Ok((detail, created))
    }

    async fn delete(
        &self,
        tenant_id: &str,
        purge: bool,
    ) -> anyhow::Result<(bool, Vec<String>)> {
        let _guard = self.write_lock.lock().await;
        let mut all = self.read_all()?;
        let Some(idx) = all.iter().position(|t| t.id == tenant_id) else {
            // Idempotent unknown-tenant case.
            return Ok((false, Vec::new()));
        };
        let orphans = crate::yaml_patch::list_agents_by_tenant(
            &self.agents_yaml_path,
            tenant_id,
        )
        .unwrap_or_default();
        if !orphans.is_empty() && !purge {
            // Refuse delete; surface orphan list so UI can confirm.
            return Ok((false, orphans));
        }
        all.remove(idx);
        self.write_all(&all)?;
        // purge=true: orphan agents stay in agents.yaml but their
        // tenant_id no longer matches a record — operator handles
        // explicit agent_delete or reassignment via the agents
        // domain. We acknowledge orphans by returning the list.
        Ok((true, orphans))
    }
}

#[cfg(test)]
mod tenants_yaml_tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn tmp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nexo-tenants-yaml-{}",
            uuid::Uuid::new_v4()
        ))
    }

    fn fixture(dir: &std::path::Path) -> TenantsYamlPatcher {
        std::fs::create_dir_all(dir).unwrap();
        let tenants = dir.join("tenants.yaml");
        let agents = dir.join("agents.yaml");
        // seed agents.yaml so list_agents_by_tenant works.
        std::fs::write(
            &agents,
            "agents:\n  - id: bot-acme-1\n    tenant_id: acme-corp\n    model: { provider: c, name: claude-opus-4-7 }\n  - id: bot-globex-1\n    tenant_id: globex\n    model: { provider: c, name: claude-opus-4-7 }\n  - id: bot-legacy\n    model: { provider: c, name: claude-opus-4-7 }\n",
        )
        .unwrap();
        TenantsYamlPatcher::new(tenants, agents)
    }

    fn upsert_input(id: &str, name: &str) -> TenantsUpsertInput {
        TenantsUpsertInput {
            id: id.into(),
            display_name: name.into(),
            active: None,
            llm_provider_refs: None,
            metadata: None,
        }
    }

    #[tokio::test]
    async fn upsert_then_list_round_trips_with_agent_count() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let (_d, created) = store.upsert(upsert_input("acme-corp", "Acme")).await.unwrap();
        assert!(created);
        let list = store.list(&TenantsListFilter::default()).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "acme-corp");
        assert_eq!(list[0].agent_count, 1, "agent bot-acme-1 references tenant");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn upsert_twice_preserves_created_at_returns_false_second() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let (d1, created1) = store
            .upsert(upsert_input("acme-corp", "Acme"))
            .await
            .unwrap();
        let (d2, created2) = store
            .upsert(upsert_input("acme-corp", "Acme v2"))
            .await
            .unwrap();
        assert!(created1);
        assert!(!created2);
        assert_eq!(d1.created_at, d2.created_at, "created_at preserved");
        assert_eq!(d2.display_name, "Acme v2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let r = store.get("absent").await.unwrap();
        assert!(r.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_unknown_returns_false_no_orphans() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let (removed, orphans) = store.delete("absent", false).await.unwrap();
        assert!(!removed);
        assert!(orphans.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_with_orphans_purge_false_returns_orphans() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let _ = store.upsert(upsert_input("acme-corp", "Acme")).await.unwrap();
        let (removed, orphans) = store.delete("acme-corp", false).await.unwrap();
        assert!(!removed, "delete rejected because of orphan agents");
        assert_eq!(orphans, vec!["bot-acme-1"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_with_orphans_purge_true_removes_tenant_acks_orphans() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let _ = store.upsert(upsert_input("acme-corp", "Acme")).await.unwrap();
        let (removed, orphans) = store.delete("acme-corp", true).await.unwrap();
        assert!(removed);
        assert_eq!(orphans, vec!["bot-acme-1"]);
        // tenant gone from yaml.
        assert!(store.get("acme-corp").await.unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_no_orphans_succeeds_purge_false() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        // Tenant with ZERO referencing agents.
        let _ = store
            .upsert(upsert_input("ghost-co", "Ghost"))
            .await
            .unwrap();
        let (removed, orphans) = store.delete("ghost-co", false).await.unwrap();
        assert!(removed);
        assert!(orphans.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_filter_active_only_drops_inactive() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let mut live = upsert_input("live-co", "Live");
        live.active = Some(true);
        let mut frozen = upsert_input("frozen-co", "Frozen");
        frozen.active = Some(false);
        store.upsert(live).await.unwrap();
        store.upsert(frozen).await.unwrap();
        let active_only = TenantsListFilter {
            active_only: true,
            prefix: None,
        };
        let list = store.list(&active_only).await.unwrap();
        let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["live-co"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_filter_prefix() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        for id in ["alpha", "alpine", "beta"] {
            let _ = store.upsert(upsert_input(id, id)).await.unwrap();
        }
        let f = TenantsListFilter {
            active_only: false,
            prefix: Some("alp".into()),
        };
        let list = store.list(&f).await.unwrap();
        let ids: Vec<&str> = list.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "alpine"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn upsert_preserves_metadata_on_partial_update() {
        let dir = tmp_dir();
        let store = fixture(&dir);
        let mut first = upsert_input("acme-corp", "Acme");
        let mut meta = BTreeMap::new();
        meta.insert("tier".into(), serde_json::json!("pro"));
        first.metadata = Some(meta);
        let _ = store.upsert(first).await.unwrap();
        // Second upsert without metadata → preserve previous.
        let (d, _) = store
            .upsert(upsert_input("acme-corp", "Acme v2"))
            .await
            .unwrap();
        assert_eq!(d.metadata.get("tier"), Some(&serde_json::json!("pro")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn empty_tenants_yaml_lists_zero() {
        let dir = tmp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let store = TenantsYamlPatcher::new(
            dir.join("tenants.yaml"),
            dir.join("agents.yaml"),
        );
        let list = store
            .list(&TenantsListFilter::default())
            .await
            .unwrap();
        assert!(list.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[allow(dead_code)]
    fn _suppress_unused_imports(_t: &TenantDetail, _u: &Utc) {}
}
