//! Phase 82.10.n — production [`ChannelCredentialPersister`]
//! for the email channel.
//!
//! Bridges `nexo/admin/credentials/register {channel: "email",
//! …}` to:
//!
//! 1. **Secret file** — `<secrets_dir>/email/<instance>.toml`
//!    (mode 0600), TOML-encoded auth block (password XOR
//!    XOAUTH2 token).
//! 2. **Yaml entry** — appends or replaces an entry in the
//!    `email.accounts` sequence of `<config_dir>/plugins/email.yaml`
//!    with the shape `crates/plugins/email/src/config.rs` parses.
//! 3. **Probe** — TCP connect + TLS handshake to the IMAP
//!    endpoint, bounded by 5 seconds. We do NOT issue `LOGIN`
//!    here (auth probing requires constructing the runtime
//!    `EmailAccount` + GoogleCredentialStore — overkill for a
//!    one-shot register-time check). Auth health surfaces on
//!    the first inbound poll cycle. Continuous health monitoring
//!    is a 82.10.n FOLLOWUP.

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

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Production [`ChannelCredentialPersister`] for IMAP/SMTP
/// accounts.
pub struct EmailPersister {
    yaml_path: PathBuf,
    secrets_dir: PathBuf,
}

impl EmailPersister {
    /// Build a persister that patches `yaml_path` and writes
    /// per-account TOML secrets under `secrets_dir/email/`.
    pub fn new(yaml_path: PathBuf, secrets_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            yaml_path,
            secrets_dir,
        })
    }

    fn secret_path(&self, instance: &str) -> PathBuf {
        self.secrets_dir.join("email").join(format!("{instance}.toml"))
    }

    fn write_secret_toml(&self, instance: &str, payload: &Value) -> Result<(), AdminRpcError> {
        let dir = self.secrets_dir.join("email");
        std::fs::create_dir_all(&dir).map_err(|e| {
            AdminRpcError::Internal(format!("create email secrets dir: {e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let final_path = self.secret_path(instance);
        let tmp_path = dir.join(format!(".{instance}.tmp"));

        // Build TOML body. We hand-format (no toml crate dep)
        // because the shape is trivial and adding a dep for two
        // keys is wasteful.
        let mut body = String::from("[auth]\n");
        if let Some(pw) = payload.get("password").and_then(Value::as_str) {
            // Single-quoted TOML literal (no escaping needed
            // unless the password contains `'`, which is
            // exceedingly rare; we reject it in validate_shape).
            body.push_str(&format!("password = '{pw}'\n"));
        }
        if let Some(tok) = payload.get("xoauth2_token").and_then(Value::as_str) {
            body.push_str(&format!("xoauth2_token = '{tok}'\n"));
        }
        std::fs::write(&tmp_path, body.as_bytes()).map_err(|e| {
            AdminRpcError::Internal(format!("write tmp email secret: {e}"))
        })?;
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

    fn delete_secret(&self, instance: &str) -> Result<bool, AdminRpcError> {
        let path = self.secret_path(instance);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AdminRpcError::Internal(format!(
                "delete email secret {}: {e}",
                path.display()
            ))),
        }
    }

    fn build_account_entry(
        instance: &str,
        payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> Result<Mapping, AdminRpcError> {
        let address = payload
            .get("address")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AdminRpcError::Internal(
                    "email persist: payload.address missing (validate_shape escaped?)".into(),
                )
            })?;
        let mut entry = Mapping::new();
        entry.insert(
            YamlValue::String("instance".into()),
            YamlValue::String(instance.into()),
        );
        entry.insert(
            YamlValue::String("address".into()),
            YamlValue::String(address.into()),
        );
        if let Some(provider) = metadata.get("provider").and_then(Value::as_str) {
            entry.insert(
                YamlValue::String("provider".into()),
                YamlValue::String(provider.into()),
            );
        }
        // Endpoints from metadata.imap / metadata.smtp.
        for key in ["imap", "smtp"] {
            if let Some(obj) = metadata.get(key).and_then(Value::as_object) {
                let mut m = Mapping::new();
                for (k, v) in obj {
                    let yv = match v {
                        Value::String(s) => YamlValue::String(s.clone()),
                        Value::Number(n) if n.is_u64() => {
                            YamlValue::Number(n.as_u64().unwrap().into())
                        }
                        Value::Bool(b) => YamlValue::Bool(*b),
                        other => YamlValue::String(other.to_string()),
                    };
                    m.insert(YamlValue::String(k.clone()), yv);
                }
                entry.insert(YamlValue::String(key.into()), YamlValue::Mapping(m));
            }
        }
        // Folders default — operator UI may override later.
        let mut folders = Mapping::new();
        folders.insert(
            YamlValue::String("inbox".into()),
            YamlValue::String("INBOX".into()),
        );
        entry.insert(YamlValue::String("folders".into()), YamlValue::Mapping(folders));
        Ok(entry)
    }

    fn upsert_yaml_entry(
        &self,
        instance: &str,
        entry: Mapping,
    ) -> Result<(), AdminRpcError> {
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
                serde_yaml::from_str(&text)
                    .map_err(|e| AdminRpcError::Internal(format!("parse email.yaml: {e}")))?
            }
        } else {
            YamlValue::Mapping(Mapping::new())
        };
        let map = root.as_mapping_mut().ok_or_else(|| {
            AdminRpcError::Internal("email.yaml root is not a mapping".into())
        })?;

        // Ensure top-level `email` block exists; preserve other
        // top-level fields.
        let email_block = map
            .entry(YamlValue::String("email".into()))
            .or_insert_with(|| YamlValue::Mapping(Mapping::new()));
        let email_map = email_block.as_mapping_mut().ok_or_else(|| {
            AdminRpcError::Internal("email.yaml `email:` block is not a mapping".into())
        })?;

        // Default top-level toggles when absent (so a fresh file
        // is functional immediately).
        email_map
            .entry(YamlValue::String("enabled".into()))
            .or_insert(YamlValue::Bool(true));

        let existing = email_map.remove(YamlValue::String("accounts".into()));
        let mut seq: Vec<YamlValue> = match existing {
            Some(YamlValue::Sequence(s)) => s,
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
        email_map.insert(
            YamlValue::String("accounts".into()),
            YamlValue::Sequence(seq),
        );
        self.atomic_write_yaml(&root)
    }

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
            .map_err(|e| AdminRpcError::Internal(format!("parse email.yaml: {e}")))?;
        let Some(map) = root.as_mapping_mut() else {
            return Ok(false);
        };
        let Some(email_block) = map.get_mut(YamlValue::String("email".into())) else {
            return Ok(false);
        };
        let Some(email_map) = email_block.as_mapping_mut() else {
            return Ok(false);
        };
        let Some(YamlValue::Sequence(mut seq)) =
            email_map.remove(YamlValue::String("accounts".into()))
        else {
            return Ok(false);
        };
        let before = seq.len();
        seq.retain(|v| v.get("instance").and_then(YamlValue::as_str) != Some(instance));
        let removed = seq.len() < before;
        email_map.insert(
            YamlValue::String("accounts".into()),
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
            AdminRpcError::Internal(format!(
                "persist yaml {}: {e}",
                self.yaml_path.display()
            ))
        })?;
        Ok(())
    }
}

#[async_trait]
impl ChannelCredentialPersister for EmailPersister {
    fn channel(&self) -> &str {
        "email"
    }

    fn validate_shape(
        &self,
        payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), AdminRpcError> {
        let address = payload.get("address").and_then(Value::as_str).unwrap_or("");
        if address.is_empty() || !address.contains('@') {
            return Err(AdminRpcError::InvalidParams(
                "email payload.address is required and must contain `@`".into(),
            ));
        }
        let has_password = payload
            .get("password")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let has_xoauth = payload
            .get("xoauth2_token")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if has_password == has_xoauth {
            return Err(AdminRpcError::InvalidParams(
                "email payload requires exactly one of `password` or `xoauth2_token`".into(),
            ));
        }
        // Reject single-quote in password (unhandled in our
        // hand-rolled TOML writer).
        if let Some(pw) = payload.get("password").and_then(Value::as_str) {
            if pw.contains('\'') {
                return Err(AdminRpcError::InvalidParams(
                    "email payload.password cannot contain `'` (TOML literal restriction)"
                        .into(),
                ));
            }
        }

        // imap + smtp metadata: each must have host (string) +
        // port (u16) + tls (string in {implicit_tls, starttls,
        // none}).
        for key in ["imap", "smtp"] {
            let block = metadata.get(key).ok_or_else(|| {
                AdminRpcError::InvalidParams(format!("email metadata.{key} is required"))
            })?;
            let obj = block.as_object().ok_or_else(|| {
                AdminRpcError::InvalidParams(format!("email metadata.{key} must be an object"))
            })?;
            let host = obj.get("host").and_then(Value::as_str).unwrap_or("");
            if host.is_empty() {
                return Err(AdminRpcError::InvalidParams(format!(
                    "email metadata.{key}.host is required"
                )));
            }
            let port = obj.get("port").and_then(Value::as_u64).unwrap_or(0);
            if !(1..=65535).contains(&port) {
                return Err(AdminRpcError::InvalidParams(format!(
                    "email metadata.{key}.port must be 1-65535"
                )));
            }
            let tls = obj.get("tls").and_then(Value::as_str).unwrap_or("");
            if !matches!(tls, "implicit_tls" | "starttls" | "none") {
                return Err(AdminRpcError::InvalidParams(format!(
                    "email metadata.{key}.tls must be implicit_tls|starttls|none"
                )));
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
        let instance = instance.ok_or_else(|| {
            AdminRpcError::InvalidParams(
                "email persist requires `instance` (no implicit default for multi-account channel)"
                    .into(),
            )
        })?;
        self.write_secret_toml(instance, payload)?;
        let entry = Self::build_account_entry(instance, payload, metadata)?;
        self.upsert_yaml_entry(instance, entry)?;
        Ok(())
    }

    async fn revoke(&self, instance: Option<&str>) -> Result<bool, AdminRpcError> {
        let instance = instance.ok_or_else(|| {
            AdminRpcError::InvalidParams("email revoke requires `instance`".into())
        })?;
        let yaml_removed = self.remove_yaml_entry(instance)?;
        let secret_removed = self.delete_secret(instance)?;
        Ok(yaml_removed || secret_removed)
    }

    async fn probe(
        &self,
        _instance: Option<&str>,
        _payload: &Value,
        metadata: &HashMap<String, Value>,
    ) -> CredentialValidationOutcome {
        let Some(imap) = metadata.get("imap").and_then(Value::as_object) else {
            return CredentialValidationOutcome {
                probed: false,
                healthy: false,
                detail: Some("metadata.imap missing — skipping probe".into()),
                reason_code: Some(reason_code::NOT_PROBED.into()),
            };
        };
        let host = match imap.get("host").and_then(Value::as_str) {
            Some(h) => h.to_string(),
            None => {
                return CredentialValidationOutcome {
                    probed: true,
                    healthy: false,
                    detail: Some("metadata.imap.host missing".into()),
                    reason_code: Some(reason_code::INVALID_METADATA.into()),
                };
            }
        };
        let port = match imap.get("port").and_then(Value::as_u64) {
            Some(p) => p as u16,
            None => {
                return CredentialValidationOutcome {
                    probed: true,
                    healthy: false,
                    detail: Some("metadata.imap.port missing".into()),
                    reason_code: Some(reason_code::INVALID_METADATA.into()),
                };
            }
        };
        let tls_mode = imap
            .get("tls")
            .and_then(Value::as_str)
            .unwrap_or("implicit_tls");

        let probe_fut = async move {
            // TCP connect, bounded by the same outer timeout.
            let tcp = match tokio::net::TcpStream::connect((host.as_str(), port)).await {
                Ok(s) => s,
                Err(e) => {
                    return CredentialValidationOutcome {
                        probed: true,
                        healthy: false,
                        detail: Some(format!("TCP connect to {host}:{port} failed: {e}")),
                        reason_code: Some(reason_code::CONNECTIVITY_FAILED.into()),
                    };
                }
            };
            // For implicit_tls: do TLS handshake. For starttls
            // we'd need to consume the IMAP greeting first;
            // skipping the IMAP layer keeps this probe tiny so
            // we accept TCP-only as the success criterion for
            // starttls (the real STARTTLS happens at first
            // poll).
            if tls_mode != "implicit_tls" {
                drop(tcp);
                return CredentialValidationOutcome {
                    probed: true,
                    healthy: true,
                    detail: Some(format!("TCP reachable on {host}:{port} (tls={tls_mode})")),
                    reason_code: Some(reason_code::OK.into()),
                };
            }
            let mut roots = rustls::RootCertStore::empty();
            match rustls_native_certs::load_native_certs() {
                Ok(certs) => {
                    for c in certs {
                        let _ = roots.add(c);
                    }
                }
                Err(e) => {
                    return CredentialValidationOutcome {
                        probed: true,
                        healthy: false,
                        detail: Some(format!("rustls-native-certs: {e}")),
                        reason_code: Some(reason_code::TLS_FAILED.into()),
                    };
                }
            }
            let cfg = Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            );
            let connector = tokio_rustls::TlsConnector::from(cfg);
            let server_name = match rustls::pki_types::ServerName::try_from(host.clone()) {
                Ok(n) => n,
                Err(e) => {
                    return CredentialValidationOutcome {
                        probed: true,
                        healthy: false,
                        detail: Some(format!("invalid TLS server name `{host}`: {e}")),
                        reason_code: Some(reason_code::TLS_FAILED.into()),
                    };
                }
            };
            match connector.connect(server_name, tcp).await {
                Ok(_tls) => CredentialValidationOutcome {
                    probed: true,
                    healthy: true,
                    detail: Some(format!("TLS handshake succeeded with {host}:{port}")),
                    reason_code: Some(reason_code::OK.into()),
                },
                Err(e) => CredentialValidationOutcome {
                    probed: true,
                    healthy: false,
                    detail: Some(format!("TLS handshake to {host}:{port} failed: {e}")),
                    reason_code: Some(reason_code::TLS_FAILED.into()),
                },
            }
        };
        match tokio::time::timeout(PROBE_TIMEOUT, probe_fut).await {
            Ok(o) => o,
            Err(_) => CredentialValidationOutcome {
                probed: true,
                healthy: false,
                detail: Some(format!(
                    "IMAP probe timed out after {}s",
                    PROBE_TIMEOUT.as_secs()
                )),
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

    fn fixture() -> (TempDir, Arc<EmailPersister>) {
        let dir = TempDir::new().unwrap();
        let yaml = dir.path().join("plugins").join("email.yaml");
        let secrets = dir.path().join("secrets");
        let p = EmailPersister::new(yaml, secrets);
        (dir, p)
    }

    fn full_metadata() -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert(
            "imap".into(),
            json!({ "host": "imap.example.com", "port": 993, "tls": "implicit_tls" }),
        );
        m.insert(
            "smtp".into(),
            json!({ "host": "smtp.example.com", "port": 587, "tls": "starttls" }),
        );
        m
    }

    #[test]
    fn validate_shape_rejects_missing_address() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(
                &json!({ "password": "p" }),
                &full_metadata(),
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_address_without_at_sign() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(
                &json!({ "address": "no-at-sign", "password": "p" }),
                &full_metadata(),
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_both_password_and_xoauth2() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(
                &json!({
                    "address": "ops@example.com",
                    "password": "p",
                    "xoauth2_token": "tok"
                }),
                &full_metadata(),
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_neither_password_nor_xoauth2() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(
                &json!({ "address": "ops@example.com" }),
                &full_metadata(),
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_password_with_single_quote() {
        let (_d, p) = fixture();
        let err = p
            .validate_shape(
                &json!({ "address": "ops@example.com", "password": "p'q" }),
                &full_metadata(),
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_missing_imap_metadata() {
        let (_d, p) = fixture();
        let mut m = full_metadata();
        m.remove("imap");
        let err = p
            .validate_shape(
                &json!({ "address": "ops@example.com", "password": "p" }),
                &m,
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_rejects_invalid_tls_mode() {
        let (_d, p) = fixture();
        let mut m = HashMap::new();
        m.insert(
            "imap".into(),
            json!({ "host": "h", "port": 993, "tls": "bogus" }),
        );
        m.insert(
            "smtp".into(),
            json!({ "host": "h", "port": 587, "tls": "starttls" }),
        );
        let err = p
            .validate_shape(
                &json!({ "address": "a@b.c", "password": "p" }),
                &m,
            )
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[test]
    fn validate_shape_accepts_full_payload_password() {
        let (_d, p) = fixture();
        p.validate_shape(
            &json!({ "address": "ops@example.com", "password": "p" }),
            &full_metadata(),
        )
        .expect("valid");
    }

    #[test]
    fn validate_shape_accepts_full_payload_xoauth2() {
        let (_d, p) = fixture();
        p.validate_shape(
            &json!({ "address": "ops@example.com", "xoauth2_token": "tok" }),
            &full_metadata(),
        )
        .expect("valid");
    }

    #[tokio::test]
    async fn persist_writes_toml_secret_with_mode_0600() {
        let (_d, p) = fixture();
        p.persist(
            Some("ops"),
            &json!({ "address": "ops@example.com", "password": "p" }),
            &full_metadata(),
        )
        .await
        .unwrap();
        let path = p.secret_path("ops");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[auth]"));
        assert!(body.contains("password = 'p'"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test]
    async fn persist_upserts_email_yaml_account_entry() {
        let (_d, p) = fixture();
        p.persist(
            Some("ops"),
            &json!({ "address": "ops@example.com", "password": "p" }),
            &full_metadata(),
        )
        .await
        .unwrap();
        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let accounts = parsed
            .get("email")
            .and_then(|e| e.get("accounts"))
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert_eq!(accounts.len(), 1);
        let entry = &accounts[0];
        assert_eq!(entry.get("instance").and_then(YamlValue::as_str), Some("ops"));
        assert_eq!(
            entry.get("address").and_then(YamlValue::as_str),
            Some("ops@example.com")
        );
        assert_eq!(
            entry
                .get("imap")
                .and_then(|i| i.get("host"))
                .and_then(YamlValue::as_str),
            Some("imap.example.com")
        );
    }

    #[tokio::test]
    async fn persist_idempotent_replaces_existing_account() {
        let (_d, p) = fixture();
        p.persist(
            Some("ops"),
            &json!({ "address": "ops@example.com", "password": "v1" }),
            &full_metadata(),
        )
        .await
        .unwrap();
        p.persist(
            Some("ops"),
            &json!({ "address": "ops@example.com", "password": "v2" }),
            &full_metadata(),
        )
        .await
        .unwrap();
        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let accounts = parsed
            .get("email")
            .and_then(|e| e.get("accounts"))
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert_eq!(accounts.len(), 1);
        let secret = std::fs::read_to_string(p.secret_path("ops")).unwrap();
        assert!(secret.contains("password = 'v2'"));
    }

    #[tokio::test]
    async fn revoke_removes_yaml_and_secret() {
        let (_d, p) = fixture();
        p.persist(
            Some("ops"),
            &json!({ "address": "ops@example.com", "password": "p" }),
            &full_metadata(),
        )
        .await
        .unwrap();
        let removed = p.revoke(Some("ops")).await.unwrap();
        assert!(removed);
        let yaml_text = std::fs::read_to_string(&p.yaml_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&yaml_text).unwrap();
        let accounts = parsed
            .get("email")
            .and_then(|e| e.get("accounts"))
            .and_then(YamlValue::as_sequence)
            .unwrap();
        assert!(accounts.is_empty());
        assert!(!p.secret_path("ops").exists());
    }

    #[tokio::test]
    async fn revoke_unknown_instance_returns_false() {
        let (_d, p) = fixture();
        let removed = p.revoke(Some("ghost")).await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn persist_requires_instance() {
        let (_d, p) = fixture();
        let err = p
            .persist(
                None,
                &json!({ "address": "a@b.c", "password": "p" }),
                &full_metadata(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AdminRpcError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn probe_with_no_listener_returns_connectivity_failed() {
        let (_d, p) = fixture();
        let mut m = HashMap::new();
        // Port 1 is reserved + nothing listens — fast TCP refusal.
        m.insert(
            "imap".into(),
            json!({ "host": "127.0.0.1", "port": 1, "tls": "implicit_tls" }),
        );
        m.insert(
            "smtp".into(),
            json!({ "host": "127.0.0.1", "port": 1, "tls": "starttls" }),
        );
        let outcome = p.probe(None, &json!({}), &m).await;
        assert!(outcome.probed);
        assert!(!outcome.healthy);
        assert_eq!(
            outcome.reason_code.as_deref(),
            Some(reason_code::CONNECTIVITY_FAILED)
        );
    }

    #[tokio::test]
    async fn probe_with_starttls_only_does_tcp_reach_check() {
        // Spawn a transient listener accepting then closing.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Accept once; client disconnects on TLS attempt.
            let _ = listener.accept().await;
        });
        let (_d, p) = fixture();
        let mut m = HashMap::new();
        m.insert(
            "imap".into(),
            json!({ "host": "127.0.0.1", "port": port, "tls": "starttls" }),
        );
        m.insert(
            "smtp".into(),
            json!({ "host": "127.0.0.1", "port": 0, "tls": "starttls" }),
        );
        let outcome = p.probe(None, &json!({}), &m).await;
        assert!(outcome.probed);
        assert!(outcome.healthy, "expected TCP-reach check to pass for starttls");
        assert_eq!(outcome.reason_code.as_deref(), Some(reason_code::OK));
    }

    #[tokio::test]
    async fn probe_with_missing_imap_metadata_returns_not_probed() {
        let (_d, p) = fixture();
        let outcome = p.probe(None, &json!({}), &HashMap::new()).await;
        assert!(!outcome.probed);
        assert_eq!(
            outcome.reason_code.as_deref(),
            Some(reason_code::NOT_PROBED)
        );
    }
}
