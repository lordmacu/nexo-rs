//! Phase 90.x.mcp — `nexo/admin/mcp/*` handlers.
//!
//! CRUD over `config/mcp.yaml.mcp.servers.<name>`. Operator UI
//! consumes these to manage the MCP servers the agent connects
//! to (vs the in-process MCP server the agent EXPOSES — that's
//! `mcp_server.yaml` which is a separate file with no admin RPC
//! v1).
//!
//! Backed by a [`McpServerStore`] trait + concrete YAML impl
//! [`McpYamlStore`] reading from `<config_dir>/mcp.yaml`. The
//! impl preserves unknown fields by round-tripping through
//! `serde_yaml::Value` so a daemon written before a future
//! schema bump never silently drops fields it doesn't recognise.
//!
//! Wire types: [`nexo_tool_meta::admin::mcp`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use serde_yaml::Value as YamlValue;
use tokio::sync::Mutex;

use nexo_tool_meta::admin::mcp::{
    McpServerDetail, McpServerSummary, McpServersDeleteParams, McpServersDeleteResponse,
    McpServersGetParams, McpServersGetResponse, McpServersListResponse, McpServersUpsertInput,
    McpServersUpsertResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Allowed transport tags. The daemon's
/// `nexo_config::types::mcp::McpServerYaml` enum has 4 variants
/// — keep in sync.
const VALID_TRANSPORTS: &[&str] = &["stdio", "streamable_http", "sse", "auto"];

/// Storage abstraction for the MCP server registry. Production
/// concrete `McpYamlStore` reads/writes `<config_dir>/mcp.yaml`.
/// Tests inject in-memory mocks.
#[async_trait]
pub trait McpServerStore: Send + Sync + std::fmt::Debug {
    /// List every server in stable alpha order by name.
    async fn list(&self) -> anyhow::Result<Vec<McpServerSummary>>;

    /// Fetch one server's full record. Unknown name returns
    /// `Ok(None)` (callers probe).
    async fn get(&self, name: &str) -> anyhow::Result<Option<McpServerDetail>>;

    /// Create-or-update one server. The boolean is `true` when
    /// this call created a new record, `false` on update.
    async fn upsert(&self, input: McpServerDetail) -> anyhow::Result<(McpServerDetail, bool)>;

    /// Remove one server. Returns `false` when the name had no
    /// record (idempotent retry safe).
    async fn delete(&self, name: &str) -> anyhow::Result<bool>;
}

// ─── Handlers ───────────────────────────────────────────────────

/// `nexo/admin/mcp/list` — every server, alpha-ordered.
pub async fn list(store: &dyn McpServerStore) -> AdminRpcResult {
    match store.list().await {
        Ok(servers) => {
            let resp = McpServersListResponse { servers };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("mcp_store.list: {e}"))),
    }
}

/// `nexo/admin/mcp/get` — one server's detail.
pub async fn get(store: &dyn McpServerStore, params: Value) -> AdminRpcResult {
    let p: McpServersGetParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    match store.get(&p.name).await {
        Ok(server) => {
            let resp = McpServersGetResponse { server };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("mcp_store.get: {e}"))),
    }
}

/// `nexo/admin/mcp/upsert` — create-or-update.
pub async fn upsert(store: &dyn McpServerStore, params: Value) -> AdminRpcResult {
    let input: McpServersUpsertInput = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_upsert(&input) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg));
    }
    match store.upsert(input).await {
        Ok((server, created)) => {
            let resp = McpServersUpsertResponse { server, created };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("mcp_store.upsert: {e}"))),
    }
}

/// `nexo/admin/mcp/delete` — idempotent removal.
pub async fn delete(store: &dyn McpServerStore, params: Value) -> AdminRpcResult {
    let p: McpServersDeleteParams = match serde_json::from_value(params) {
        Ok(v) => v,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    match store.delete(&p.name).await {
        Ok(removed) => {
            let resp = McpServersDeleteResponse { removed };
            AdminRpcResult::ok(serde_json::to_value(resp).unwrap_or(Value::Null))
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("mcp_store.delete: {e}"))),
    }
}

// ─── Validation ─────────────────────────────────────────────────

fn validate_upsert(input: &McpServerDetail) -> Result<(), String> {
    let trimmed = input.name.trim();
    if trimmed.is_empty() {
        return Err("name is empty".into());
    }
    if trimmed.len() > 64 {
        return Err("name exceeds 64 chars".into());
    }
    if !VALID_TRANSPORTS.contains(&input.transport.as_str()) {
        return Err(format!(
            "transport `{}` not one of: stdio | streamable_http | sse | auto",
            input.transport
        ));
    }
    match input.transport.as_str() {
        "stdio" => {
            if input.command.as_deref().unwrap_or("").trim().is_empty() {
                return Err("stdio transport requires `command`".into());
            }
        }
        "streamable_http" | "sse" | "auto" => {
            if input.url.as_deref().unwrap_or("").trim().is_empty() {
                return Err(format!("{} transport requires `url`", input.transport));
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

// ─── Yaml-backed concrete impl ──────────────────────────────────

/// Reads / writes `<config_dir>/mcp.yaml`. Round-trips through
/// `serde_yaml::Value` so unknown top-level keys (e.g. future
/// schema_version bumps, sampling overrides we haven't typed
/// yet) survive an upsert / delete cycle untouched.
#[derive(Debug)]
pub struct McpYamlStore {
    path: PathBuf,
    /// File-level lock so concurrent upserts serialise. Without
    /// this, two operators clicking save in quick succession
    /// would race on the read-modify-write cycle.
    write_lock: Mutex<()>,
}

impl McpYamlStore {
    /// Build the store pointed at `<config_dir>/mcp.yaml`.
    pub fn new(config_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path: config_dir.join("mcp.yaml"),
            write_lock: Mutex::new(()),
        })
    }

    fn read_doc(&self) -> anyhow::Result<YamlValue> {
        if !self.path.exists() {
            // No file yet → fresh empty doc.
            return Ok(empty_doc());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        let doc: YamlValue = serde_yaml::from_str(&raw)?;
        Ok(doc)
    }

    fn write_doc(&self, doc: &YamlValue) -> anyhow::Result<()> {
        // Atomic write via temp file + rename.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("yaml.tmp");
        let serialised = serde_yaml::to_string(doc)?;
        std::fs::write(&tmp, serialised)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[async_trait]
impl McpServerStore for McpYamlStore {
    async fn list(&self) -> anyhow::Result<Vec<McpServerSummary>> {
        let doc = self.read_doc()?;
        let servers = servers_map(&doc).cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(servers.len());
        for (name, val) in servers {
            let transport = transport_of(&val).unwrap_or_else(|| "unknown".into());
            let log_level = string_field(&val, "log_level");
            out.push(McpServerSummary {
                name: yaml_key_to_string(&name),
                transport,
                log_level,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn get(&self, name: &str) -> anyhow::Result<Option<McpServerDetail>> {
        let doc = self.read_doc()?;
        let Some(servers) = servers_map(&doc) else {
            return Ok(None);
        };
        let key = YamlValue::String(name.to_string());
        let Some(val) = servers.get(&key) else {
            return Ok(None);
        };
        Ok(Some(yaml_to_detail(name, val)))
    }

    async fn upsert(&self, input: McpServerDetail) -> anyhow::Result<(McpServerDetail, bool)> {
        let _guard = self.write_lock.lock().await;
        let mut doc = self.read_doc()?;
        let mut created = true;
        {
            let servers = servers_map_mut(&mut doc)?;
            let key = YamlValue::String(input.name.clone());
            if servers.contains_key(&key) {
                created = false;
            }
            servers.insert(key, detail_to_yaml(&input));
        }
        self.write_doc(&doc)?;
        Ok((input, created))
    }

    async fn delete(&self, name: &str) -> anyhow::Result<bool> {
        let _guard = self.write_lock.lock().await;
        let mut doc = self.read_doc()?;
        let removed = {
            let servers = servers_map_mut(&mut doc)?;
            let key = YamlValue::String(name.to_string());
            servers.remove(&key).is_some()
        };
        if removed {
            self.write_doc(&doc)?;
        }
        Ok(removed)
    }
}

// ─── Yaml helpers ───────────────────────────────────────────────

fn empty_doc() -> YamlValue {
    let mut mcp = serde_yaml::Mapping::new();
    mcp.insert(
        YamlValue::String("servers".into()),
        YamlValue::Mapping(serde_yaml::Mapping::new()),
    );
    let mut root = serde_yaml::Mapping::new();
    root.insert(YamlValue::String("mcp".into()), YamlValue::Mapping(mcp));
    YamlValue::Mapping(root)
}

fn servers_map(doc: &YamlValue) -> Option<&serde_yaml::Mapping> {
    let YamlValue::Mapping(root) = doc else {
        return None;
    };
    let mcp = root.get(YamlValue::String("mcp".into()))?;
    let YamlValue::Mapping(mcp_map) = mcp else {
        return None;
    };
    let servers = mcp_map.get(YamlValue::String("servers".into()))?;
    let YamlValue::Mapping(map) = servers else {
        return None;
    };
    Some(map)
}

fn servers_map_mut(doc: &mut YamlValue) -> anyhow::Result<&mut serde_yaml::Mapping> {
    let root = match doc {
        YamlValue::Mapping(m) => m,
        _ => anyhow::bail!("mcp.yaml root is not a mapping"),
    };
    let mcp_entry = root
        .entry(YamlValue::String("mcp".into()))
        .or_insert_with(|| YamlValue::Mapping(serde_yaml::Mapping::new()));
    let mcp_map = match mcp_entry {
        YamlValue::Mapping(m) => m,
        _ => anyhow::bail!("mcp section is not a mapping"),
    };
    let servers_entry = mcp_map
        .entry(YamlValue::String("servers".into()))
        .or_insert_with(|| YamlValue::Mapping(serde_yaml::Mapping::new()));
    match servers_entry {
        YamlValue::Mapping(m) => Ok(m),
        _ => anyhow::bail!("mcp.servers section is not a mapping"),
    }
}

fn transport_of(val: &YamlValue) -> Option<String> {
    let YamlValue::Mapping(m) = val else {
        return None;
    };
    let v = m.get(YamlValue::String("transport".into()))?;
    let YamlValue::String(s) = v else {
        return None;
    };
    Some(s.clone())
}

fn string_field(val: &YamlValue, field: &str) -> Option<String> {
    let YamlValue::Mapping(m) = val else {
        return None;
    };
    let v = m.get(YamlValue::String(field.into()))?;
    let YamlValue::String(s) = v else {
        return None;
    };
    Some(s.clone())
}

fn yaml_key_to_string(k: &YamlValue) -> String {
    match k {
        YamlValue::String(s) => s.clone(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .into(),
    }
}

fn yaml_to_detail(name: &str, val: &YamlValue) -> McpServerDetail {
    let YamlValue::Mapping(m) = val else {
        return McpServerDetail {
            name: name.into(),
            transport: "unknown".into(),
            ..Default::default()
        };
    };
    let transport = m
        .get(YamlValue::String("transport".into()))
        .and_then(|v| match v {
            YamlValue::String(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "unknown".into());

    McpServerDetail {
        name: name.into(),
        transport,
        command: m
            .get(YamlValue::String("command".into()))
            .and_then(|v| match v {
                YamlValue::String(s) => Some(s.clone()),
                _ => None,
            }),
        args: m
            .get(YamlValue::String("args".into()))
            .and_then(|v| match v {
                YamlValue::Sequence(s) => Some(
                    s.iter()
                        .filter_map(|e| match e {
                            YamlValue::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default(),
        env: m
            .get(YamlValue::String("env".into()))
            .and_then(yaml_to_string_map)
            .unwrap_or_default(),
        cwd: m
            .get(YamlValue::String("cwd".into()))
            .and_then(|v| match v {
                YamlValue::String(s) => Some(s.clone()),
                _ => None,
            }),
        url: m
            .get(YamlValue::String("url".into()))
            .and_then(|v| match v {
                YamlValue::String(s) => Some(s.clone()),
                _ => None,
            }),
        headers: m
            .get(YamlValue::String("headers".into()))
            .and_then(yaml_to_string_map)
            .unwrap_or_default(),
        log_level: m
            .get(YamlValue::String("log_level".into()))
            .and_then(|v| match v {
                YamlValue::String(s) => Some(s.clone()),
                _ => None,
            }),
        context_passthrough: m
            .get(YamlValue::String("context_passthrough".into()))
            .and_then(|v| match v {
                YamlValue::Bool(b) => Some(*b),
                _ => None,
            }),
    }
}

fn yaml_to_string_map(val: &YamlValue) -> Option<BTreeMap<String, String>> {
    let YamlValue::Mapping(m) = val else {
        return None;
    };
    let mut out = BTreeMap::new();
    for (k, v) in m {
        if let (YamlValue::String(ks), YamlValue::String(vs)) = (k, v) {
            out.insert(ks.clone(), vs.clone());
        }
    }
    Some(out)
}

fn detail_to_yaml(d: &McpServerDetail) -> YamlValue {
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        YamlValue::String("transport".into()),
        YamlValue::String(d.transport.clone()),
    );
    if let Some(cmd) = &d.command {
        m.insert(
            YamlValue::String("command".into()),
            YamlValue::String(cmd.clone()),
        );
    }
    if !d.args.is_empty() {
        m.insert(
            YamlValue::String("args".into()),
            YamlValue::Sequence(
                d.args
                    .iter()
                    .map(|s| YamlValue::String(s.clone()))
                    .collect(),
            ),
        );
    }
    if !d.env.is_empty() {
        let mut env_map = serde_yaml::Mapping::new();
        for (k, v) in &d.env {
            env_map.insert(YamlValue::String(k.clone()), YamlValue::String(v.clone()));
        }
        m.insert(YamlValue::String("env".into()), YamlValue::Mapping(env_map));
    }
    if let Some(cwd) = &d.cwd {
        m.insert(
            YamlValue::String("cwd".into()),
            YamlValue::String(cwd.clone()),
        );
    }
    if let Some(url) = &d.url {
        m.insert(
            YamlValue::String("url".into()),
            YamlValue::String(url.clone()),
        );
    }
    if !d.headers.is_empty() {
        let mut h_map = serde_yaml::Mapping::new();
        for (k, v) in &d.headers {
            h_map.insert(YamlValue::String(k.clone()), YamlValue::String(v.clone()));
        }
        m.insert(
            YamlValue::String("headers".into()),
            YamlValue::Mapping(h_map),
        );
    }
    if let Some(level) = &d.log_level {
        m.insert(
            YamlValue::String("log_level".into()),
            YamlValue::String(level.clone()),
        );
    }
    if let Some(b) = d.context_passthrough {
        m.insert(
            YamlValue::String("context_passthrough".into()),
            YamlValue::Bool(b),
        );
    }
    YamlValue::Mapping(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_store() -> (Arc<McpYamlStore>, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = McpYamlStore::new(dir.path().to_path_buf());
        (store, dir)
    }

    #[tokio::test]
    async fn empty_store_list_returns_empty() {
        let (store, _dir) = make_store();
        let servers = store.list().await.unwrap();
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn upsert_then_list_roundtrips() {
        let (store, _dir) = make_store();
        let detail = McpServerDetail {
            name: "echo".into(),
            transport: "stdio".into(),
            command: Some("/bin/cat".into()),
            args: vec!["--keep-going".into()],
            ..Default::default()
        };
        let (_, created) = store.upsert(detail.clone()).await.unwrap();
        assert!(created);
        let servers = store.list().await.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "echo");
        assert_eq!(servers[0].transport, "stdio");
    }

    #[tokio::test]
    async fn upsert_idempotent_returns_created_false_on_second_call() {
        let (store, _dir) = make_store();
        let detail = McpServerDetail {
            name: "tag".into(),
            transport: "stdio".into(),
            command: Some("/bin/echo".into()),
            ..Default::default()
        };
        let (_, c1) = store.upsert(detail.clone()).await.unwrap();
        let (_, c2) = store.upsert(detail).await.unwrap();
        assert!(c1);
        assert!(!c2);
    }

    #[tokio::test]
    async fn get_returns_full_detail() {
        let (store, _dir) = make_store();
        let detail = McpServerDetail {
            name: "remote".into(),
            transport: "streamable_http".into(),
            url: Some("https://api.example.com".into()),
            headers: BTreeMap::from([("X-Auth".into(), "${TOKEN}".into())]),
            ..Default::default()
        };
        store.upsert(detail.clone()).await.unwrap();
        let got = store.get("remote").await.unwrap();
        assert_eq!(
            got.as_ref().map(|d| d.transport.as_str()),
            Some("streamable_http")
        );
        assert_eq!(
            got.as_ref().and_then(|d| d.url.as_deref()),
            Some("https://api.example.com")
        );
    }

    #[tokio::test]
    async fn get_unknown_returns_none() {
        let (store, _dir) = make_store();
        let got = store.get("nope").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn delete_removes_entry() {
        let (store, _dir) = make_store();
        store
            .upsert(McpServerDetail {
                name: "drop".into(),
                transport: "stdio".into(),
                command: Some("/bin/true".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let removed = store.delete("drop").await.unwrap();
        assert!(removed);
        let removed_again = store.delete("drop").await.unwrap();
        assert!(!removed_again);
    }

    #[test]
    fn validate_upsert_rejects_invalid_transport() {
        let bad = McpServerDetail {
            name: "x".into(),
            transport: "ws".into(),
            ..Default::default()
        };
        assert!(validate_upsert(&bad).is_err());
    }

    #[test]
    fn validate_upsert_rejects_stdio_without_command() {
        let bad = McpServerDetail {
            name: "x".into(),
            transport: "stdio".into(),
            ..Default::default()
        };
        assert!(validate_upsert(&bad).is_err());
    }

    #[test]
    fn validate_upsert_rejects_http_without_url() {
        let bad = McpServerDetail {
            name: "x".into(),
            transport: "streamable_http".into(),
            ..Default::default()
        };
        assert!(validate_upsert(&bad).is_err());
    }

    #[tokio::test]
    async fn yaml_round_trip_preserves_unknown_fields_at_top_level() {
        let (store, dir) = make_store();
        // Seed a doc with operator-set top-level fields.
        let path = dir.path().join("mcp.yaml");
        std::fs::write(
            &path,
            "schema_version: 11\nmcp:\n  enabled: true\n  servers: {}\n",
        )
        .unwrap();
        store
            .upsert(McpServerDetail {
                name: "new-one".into(),
                transport: "stdio".into(),
                command: Some("/bin/echo".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        // Top-level operator keys survived.
        assert!(raw.contains("schema_version"), "got: {raw}");
        assert!(raw.contains("enabled: true"), "got: {raw}");
        assert!(raw.contains("new-one"), "got: {raw}");
    }
}
