//! Phase 82.10.n — production [`ChannelCredentialPersister`]
//! for the telegram channel.
//!
//! Bridges `nexo/admin/credentials/register {channel: "telegram",
//! …}` to:
//!
//! 1. **Secret file** — `<secrets_dir>/telegram_<instance>_token.txt`
//!    (mode 0600 on Unix), holding the bot API token.
//! 2. **Yaml entry** — appends or replaces an account in
//!    `<config_dir>/plugins/telegram.yaml` under the top-level
//!    `telegram:` sequence with the shape the runtime loader
//!    expects (`instance`, `token: ${file:./secrets/...}`,
//!    `polling`, `allow_agents`, `allowlist.chat_ids`).
//! 3. **Probe** — Telegram Bot API `GET /getMe`, bounded by a
//!    5-second timeout. Healthy = HTTP 200 + body `ok=true`.
//!
//! No circuit breaker is wired here: probes are one-shot at
//! register time, and a failed probe just lands an unhealthy
//! outcome in the response (the credential still persists). A CB
//! gives no benefit for non-repeated calls. Continuous health
//! monitoring is a 82.10.n FOLLOWUP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use serde_yaml::{Mapping, Value as YamlValue};

use nexo_core::agent::admin_rpc::dispatcher::AdminRpcError;
use nexo_core::agent::admin_rpc::domains::credentials::ChannelCredentialPersister;
use nexo_tool_meta::admin::credentials::{reason_code, CredentialValidationOutcome};

/// Default identifier used when the operator omits the `instance`
/// field — telegram supports a single bot per instance, so a
/// stable default keeps single-bot deployments ergonomic.
const DEFAULT_INSTANCE: &str = "default";

/// Cap on the `getMe` probe so a slow telegram API doesn't block
/// the operator's register call.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Production [`ChannelCredentialPersister`] for telegram bots.
pub struct TelegramPersister {
    yaml_path: PathBuf,
    secrets_dir: PathBuf,
    http: Arc<reqwest::Client>,
}

impl TelegramPersister {
    /// Build a persister that patches `yaml_path` and writes
    /// secret tokens under `secrets_dir`. Both paths are created
    /// lazily on first persist.
    pub fn new(yaml_path: PathBuf, secrets_dir: PathBuf) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(PROBE_TIMEOUT)
            .build()
            .expect("build reqwest client (timeout-only config)");
        Arc::new(Self {
            yaml_path,
            secrets_dir,
            http: Arc::new(http),
        })
    }

    fn instance_or_default<'a>(instance: Option<&'a str>) -> &'a str {
        instance.unwrap_or(DEFAULT_INSTANCE)
    }

    fn secret_path(&self, instance: &str) -> PathBuf {
        self.secrets_dir
            .join(format!("telegram_{instance}_token.txt"))
    }

    fn token_placeholder(&self, instance: &str) -> String {
        format!("${{file:./secrets/telegram_{instance}_token.txt}}")
    }

    /// Atomic file write at mode 0600 on Unix. Mirrors the
    /// pattern used by `crate::secrets_store::FsSecretsStore`.
    fn write_secret(&self, instance: &str, token: &str) -> Result<(), AdminRpcError> {
        std::fs::create_dir_all(&self.secrets_dir).map_err(|e| {
            AdminRpcError::Internal(format!("create secrets dir: {e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &self.secrets_dir,
                std::fs::Permissions::from_mode(0o700),
            );
        }
        let final_path = self.secret_path(instance);
        let tmp_path = self
            .secrets_dir
            .join(format!(".telegram_{instance}_token.tmp"));
        std::fs::write(&tmp_path, token.as_bytes())
            .map_err(|e| AdminRpcError::Internal(format!("write tmp: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| AdminRpcError::Internal(format!("chmod tmp: {e}")))?;
        }
        std::fs::rename(&tmp_path, &final_path)
            .map_err(|e| AdminRpcError::Internal(format!("rename tmp: {e}")))?;
        Ok(())
    }

    /// Inverse of [`Self::write_secret`]. Idempotent — missing
    /// file = `Ok(false)`.
    fn delete_secret(&self, instance: &str) -> Result<bool, AdminRpcError> {
        let path = self.secret_path(instance);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AdminRpcError::Internal(format!(
                "delete telegram secret {}: {e}",
                path.display()
            ))),
        }
    }

    /// Build the yaml mapping for a single account entry. Pulled
    /// out so the upsert/test surface can share construction.
    fn build_entry(
        instance: &str,
        token_placeholder: &str,
        metadata: &HashMap<String, Value>,
    ) -> Mapping {
        let mut entry = Mapping::new();
        entry.insert(
            YamlValue::String("instance".into()),
            YamlValue::String(instance.into()),
        );
        entry.insert(
            YamlValue::String("token".into()),
            YamlValue::String(token_placeholder.into()),
        );

        // Polling: defaults to enabled = true, interval = 1000 ms.
        let polling_meta = metadata
            .get("polling")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut polling = Mapping::new();
        polling.insert(
            YamlValue::String("enabled".into()),
            YamlValue::Bool(
                polling_meta
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            ),
        );
        polling.insert(
            YamlValue::String("interval_ms".into()),
            YamlValue::Number(
                polling_meta
                    .get("interval_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(1000)
                    .into(),
            ),
        );
        entry.insert(YamlValue::String("polling".into()), YamlValue::Mapping(polling));

        // allow_agents (defaults to empty = no agent restriction).
        let allow_agents: Vec<YamlValue> = metadata
            .get("allow_agents")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| YamlValue::String(s.into())))
                    .collect()
            })
            .unwrap_or_default();
        entry.insert(
            YamlValue::String("allow_agents".into()),
            YamlValue::Sequence(allow_agents),
        );

        // allowed_chat_ids defaults to empty list.
        let chat_ids: Vec<YamlValue> = metadata
            .get("allowed_chat_ids")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_i64().map(|n| YamlValue::Number(n.into())))
                    .collect()
            })
            .unwrap_or_default();
        let mut allowlist = Mapping::new();
        allowlist.insert(
            YamlValue::String("chat_ids".into()),
            YamlValue::Sequence(chat_ids),
        );
        entry.insert(
            YamlValue::String("allowlist".into()),
            YamlValue::Mapping(allowlist),
        );

        entry
    }

    /// Read-modify-write `telegram.yaml` upserting `entry` keyed
    /// by `instance`. Atomic via tmp-file + rename.
    fn upsert_yaml_entry(&self, instance: &str, entry: Mapping) -> Result<(), AdminRpcError> {
        if let Some(parent) = self.yaml_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AdminRpcError::Internal(format!("create yaml dir: {e}"))
            })?;
        }
        let mut root: YamlValue = if self.yaml_path.exists() {
            let text = std::fs::read_to_string(&self.yaml_path)
                .map_err(|e| AdminRpcError::Internal(format!("read yaml: {e}")))?;
            if text.trim().is_empty() {
                YamlValue::Mapping(Mapping::new())
            } else {
                serde_yaml::from_str(&text).map_err(|e| {
                    AdminRpcError::Internal(format!("parse telegram.yaml: {e}"))
                })?
            }
        } else {
            YamlValue::Mapping(Mapping::new())
        };
        let map = root.as_mapping_mut().ok_or_else(|| {
            AdminRpcError::Internal("telegram.yaml root is not a mapping".into())
        })?;
        let existing = map.remove(YamlValue::String("telegram".into()));
        let mut seq: Vec<YamlValue> = match existing {
            Some(YamlValue::Sequence(s)) => s,
            Some(YamlValue::Mapping(m)) => vec![YamlValue::Mapping(m)],
            _ => Vec::new(),
        };
        let replaced = seq.iter_mut().any(|v| {
            if v.get("instance").and_then(YamlValue::as_str) == Some(instance) {
                *v = YamlValue::Mapping(entry.clone());
                true
            } else {
                false
            }
        });
        if !replaced {
            seq.push(YamlValue::Mapping(entry));
        }
        map.insert(
            YamlValue::String("telegram".into()),
            YamlValue::Sequence(seq),
        );
        self.atomic_write_yaml(&root)
    }

    /// Inverse of [`Self::upsert_yaml_entry`]. Returns `true`
    /// when the entry existed and was removed.
    fn remove_yaml_entry(&self, instance: &str) -> Result<bool, AdminRpcError> {
        if !self.yaml_path.exists() {
            return Ok(false);
        }
        let text = std::fs::read_to_string(&self.yaml_path)
            .map_err(|e| AdminRpcError::Internal(format!("read yaml: {e}")))?;
        if text.trim().is_empty() {
            return Ok(false);
        }
        let mut root: YamlValue = serde_yaml::from_str(&text)
            .map_err(|e| AdminRpcError::Internal(format!("parse telegram.yaml: {e}")))?;
        let Some(map) = root.as_mapping_mut() else {
            return Ok(false);
        };
        let Some(YamlValue::Sequence(mut seq)) =
            map.remove(YamlValue::String("telegram".into()))
        else {
            return Ok(false);
        };
        let before = seq.len();
        seq.retain(|v| v.get("instance").and_then(YamlValue::as_str) != Some(instance));
        let removed = seq.len() < before;
        map.insert(
            YamlValue::String("telegram".into()),
            YamlValue::Sequence(seq),
        );
        self.atomic_write_yaml(&root)?;
        Ok(removed)
    }

    fn atomic_write_yaml(&self, root: &YamlValue) -> Result<(), AdminRpcError> {
        let parent = self.yaml_path.parent().unwrap_or(std::path::Path::new("."));
        let tmp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| AdminRpcError::Internal(format!("tmp file: {e}")))?;
        {
            use std::io::Write;
            let mut f = tmp
                .reopen()
                .map_err(|e| AdminRpcError::Internal(format!("reopen tmp: {e}")))?;
            f.write_all(
                serde_yaml::to_string(root)
                    .map_err(|e| AdminRpcError::Internal(format!("yaml serialize: {e}")))?
                    .as_bytes(),
            )
            .map_err(|e| AdminRpcError::Internal(format!("write yaml: {e}")))?;
            f.flush()
                .map_err(|e| AdminRpcError::Internal(format!("flush yaml: {e}")))?;
        }
        tmp.persist(&self.yaml_path).map_err(|e| {
            AdminRpcError::Internal(format!("persist yaml {}: {e}", self.yaml_path.display()))
        })?;
        Ok(())
    }
}

#[async_trait]
impl ChannelCredentialPersister for TelegramPersister {
    fn channel(&self) -> &str {
        "telegram"
    }

    fn validate_shape(
        &self,
        payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        let token = payload
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if token.is_empty() {
            return Err(AdminRpcError::InvalidParams(
                "telegram payload.token is required and must be a non-empty string".into(),
            ));
        }

        // polling.interval_ms, when present, must be u64 ≥ 100
        // (lower would melt the API rate limits).
        if let Some(p) = metadata.get("polling").and_then(Value::as_object) {
            if let Some(iv) = p.get("interval_ms") {
                let Some(n) = iv.as_u64() else {
                    return Err(AdminRpcError::InvalidParams(
                        "telegram metadata.polling.interval_ms must be a positive integer".into(),
                    ));
                };
                if n < 100 {
                    return Err(AdminRpcError::InvalidParams(
                        "telegram metadata.polling.interval_ms must be ≥ 100".into(),
                    ));
                }
            }
        }
        // allowed_chat_ids must be array of integers when set.
        if let Some(arr) = metadata.get("allowed_chat_ids") {
            let Some(a) = arr.as_array() else {
                return Err(AdminRpcError::InvalidParams(
                    "telegram metadata.allowed_chat_ids must be an array".into(),
                ));
            };
            if !a.iter().all(|v| v.is_i64()) {
                return Err(AdminRpcError::InvalidParams(
                    "telegram metadata.allowed_chat_ids entries must be integers".into(),
                ));
            }
        }
        Ok(())
    }

    async fn persist(
        &self,
        instance: Option<&str>,
        payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        let instance = Self::instance_or_default(instance);
        let token = payload
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AdminRpcError::Internal(
                    "telegram persist: payload.token missing (validate_shape escaped?)".into(),
                )
            })?;

        // Step 1 — secret file (mode 0600).
        self.write_secret(instance, token)?;

        // Step 2 — yaml entry referencing the secret via
        // `${file:...}` placeholder so the loader's existing
        // env/file resolver materialises the live token.
        let placeholder = self.token_placeholder(instance);
        let entry = Self::build_entry(instance, &placeholder, metadata);
        self.upsert_yaml_entry(instance, entry)?;
        Ok(())
    }

    async fn revoke(&self, instance: Option<&str>) -> Result<bool, AdminRpcError> {
        let instance = Self::instance_or_default(instance);
        let yaml_removed = self.remove_yaml_entry(instance)?;
        let secret_removed = self.delete_secret(instance)?;
        Ok(yaml_removed || secret_removed)
    }

    async fn probe(
        &self,
        _instance: Option<&str>,
        payload: &Value,
        _metadata: &HashMap<String, Value>,
    ) -> CredentialValidationOutcome {
        let Some(token) = payload.get("token").and_then(Value::as_str) else {
            return CredentialValidationOutcome {
                probed: true,
                healthy: false,
                detail: Some("payload.token missing at probe time".into()),
                reason_code: Some(reason_code::INVALID_PAYLOAD.into()),
            };
        };
        let url = format!("https://api.telegram.org/bot{token}/getMe");
        let req = self.http.get(&url).send();

        let resp = match tokio::time::timeout(PROBE_TIMEOUT, req).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return CredentialValidationOutcome {
                    probed: true,
                    healthy: false,
                    detail: Some(format!("getMe transport failed: {e}")),
                    reason_code: Some(reason_code::CONNECTIVITY_FAILED.into()),
                };
            }
            Err(_) => {
                return CredentialValidationOutcome {
                    probed: true,
                    healthy: false,
                    detail: Some(format!(
                        "getMe timed out after {}s",
                        PROBE_TIMEOUT.as_secs()
                    )),
                    reason_code: Some(reason_code::CONNECTIVITY_FAILED.into()),
                };
            }
        };

        let status = resp.status();
        if status.as_u16() == 401 {
            return CredentialValidationOutcome {
                probed: true,
                healthy: false,
                detail: Some("Telegram API returned 401 Unauthorized".into()),
                reason_code: Some(reason_code::AUTH_FAILED.into()),
            };
        }
        if !status.is_success() {
            return CredentialValidationOutcome {
                probed: true,
                healthy: false,
                detail: Some(format!("Telegram API returned HTTP {status}")),
                reason_code: Some(reason_code::CONNECTIVITY_FAILED.into()),
            };
        }
        match resp.json::<Value>().await {
            Ok(body) => {
                if body.get("ok").and_then(Value::as_bool) == Some(true) {
                    CredentialValidationOutcome {
                        probed: true,
                        healthy: true,
                        detail: body
                            .get("result")
                            .and_then(|r| r.get("username"))
                            .and_then(Value::as_str)
                            .map(|u| format!("authenticated as @{u}")),
                        reason_code: Some(reason_code::OK.into()),
                    }
                } else {
                    CredentialValidationOutcome {
                        probed: true,
                        healthy: false,
                        detail: body
                            .get("description")
                            .and_then(Value::as_str)
                            .map(String::from),
                        reason_code: Some(reason_code::AUTH_FAILED.into()),
                    }
                }
            }
            Err(e) => CredentialValidationOutcome {
                probed: true,
                healthy: false,
                detail: Some(format!("getMe body parse failed: {e}")),
                reason_code: Some(reason_code::CONNECTIVITY_FAILED.into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Arc<TelegramPersister>) {
        let dir = TempDir::new().unwrap();
        let yaml = dir.path().join("plugins").join("telegram.yaml");
        let secrets = dir.path().join("secrets");
        let p = TelegramPersister::new(yaml, secrets);
        (dir, p)
    }

    #[test]
    fn validate_shape_rejects_missing_token() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(&json!({}), &HashMap::new())
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_empty_token() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(&json!({ "token": "   " }), &HashMap::new())
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_polling_interval_below_100ms() {
        let (_d, p) = fixture();
        let mut metadata = HashMap::new();
        metadata.insert("polling".into(), json!({ "interval_ms": 50 }));
        let err = p
            .validate_shape(&json!({ "token": "tg.X" }), &metadata)
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_non_integer_chat_ids() {
        let (_d, p) = fixture();
        let mut metadata = HashMap::new();
        metadata.insert("allowed_chat_ids".into(), json!(["not-an-int"]));
        let err = p
            .validate_shape(&json!({ "token": "tg.X" }), &metadata)
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_accepts_full_metadata() {
        let (_d, p) = fixture();
        let mut metadata = HashMap::new();
        metadata.insert(
            "polling".into(),
            json!({ "enabled": true, "interval_ms": 1500 }),
        );
        metadata.insert("allow_agents".into(), json!(["kate"]));
        metadata.insert("allowed_chat_ids".into(), json!([123, 456]));
        p.validate_shape(&json!({ "token": "tg.X" }), &metadata)
            .expect("valid");
    }

    #[tokio::test]
    async fn persist_writes_secret_file_with_mode_0600() {
        let (_d, p) = fixture();
        p.persist(
            Some("kate"),
            &json!({ "token": "tg.SECRET" }),
            &HashMap::new(),
        )
        .await
        .unwrap();

        let secret_path = p.secret_path("kate");
        let on_disk = std::fs::read_to_string(&secret_path).unwrap();
        assert_eq!(on_disk, "tg.SECRET");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&secret_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "expected mode 0600 got {mode:o}");
        }
    }

    #[tokio::test]
    async fn persist_upserts_yaml_entry_with_polling_and_allow_agents() {
        let (_d, p) = fixture();
        let mut metadata = HashMap::new();
        metadata.insert(
            "polling".into(),
            json!({ "enabled": false, "interval_ms": 2000 }),
        );
        metadata.insert("allow_agents".into(), json!(["kate"]));
        p.persist(Some("kate"), &json!({ "token": "tg.X" }), &metadata)
            .await
            .unwrap();

        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let seq = parsed
            .get("telegram")
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert_eq!(seq.len(), 1);
        let entry = &seq[0];
        assert_eq!(entry.get("instance").and_then(YamlValue::as_str), Some("kate"));
        assert_eq!(
            entry.get("token").and_then(YamlValue::as_str),
            Some("${file:./secrets/telegram_kate_token.txt}")
        );
        assert_eq!(
            entry
                .get("polling")
                .and_then(|p| p.get("enabled"))
                .and_then(YamlValue::as_bool),
            Some(false)
        );
        assert_eq!(
            entry
                .get("polling")
                .and_then(|p| p.get("interval_ms"))
                .and_then(YamlValue::as_u64),
            Some(2000)
        );
        let allow: Vec<&str> = entry
            .get("allow_agents")
            .and_then(YamlValue::as_sequence)
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(allow, vec!["kate"]);
    }

    #[tokio::test]
    async fn persist_idempotent_replaces_existing_instance() {
        let (_d, p) = fixture();
        p.persist(Some("kate"), &json!({ "token": "v1" }), &HashMap::new())
            .await
            .unwrap();
        p.persist(Some("kate"), &json!({ "token": "v2" }), &HashMap::new())
            .await
            .unwrap();

        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let seq = parsed
            .get("telegram")
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert_eq!(seq.len(), 1, "second persist must replace not append");
        let secret = std::fs::read_to_string(p.secret_path("kate")).unwrap();
        assert_eq!(secret, "v2");
    }

    #[tokio::test]
    async fn persist_two_distinct_instances_coexist() {
        let (_d, p) = fixture();
        p.persist(Some("kate"), &json!({ "token": "k" }), &HashMap::new())
            .await
            .unwrap();
        p.persist(Some("cody"), &json!({ "token": "c" }), &HashMap::new())
            .await
            .unwrap();

        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let seq = parsed
            .get("telegram")
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert_eq!(seq.len(), 2);
    }

    #[tokio::test]
    async fn revoke_removes_yaml_entry_and_secret() {
        let (_d, p) = fixture();
        p.persist(Some("kate"), &json!({ "token": "x" }), &HashMap::new())
            .await
            .unwrap();
        let removed = p.revoke(Some("kate")).await.unwrap();
        assert!(removed);

        // Yaml entry gone (file may still exist with empty
        // sequence — that's the steady state).
        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let seq = parsed
            .get("telegram")
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert!(seq.is_empty());

        // Secret file gone.
        assert!(!p.secret_path("kate").exists());
    }

    #[tokio::test]
    async fn revoke_unknown_instance_is_idempotent_returns_false() {
        let (_d, p) = fixture();
        let removed = p.revoke(Some("ghost")).await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn probe_returns_invalid_payload_when_token_missing() {
        let (_d, p) = fixture();
        let outcome = p.probe(None, &json!({}), &HashMap::new()).await;
        assert!(outcome.probed);
        assert!(!outcome.healthy);
        assert_eq!(
            outcome.reason_code.as_deref(),
            Some(reason_code::INVALID_PAYLOAD)
        );
    }
}
