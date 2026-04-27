//! [`CredentialStore`] impl for IMAP/SMTP email accounts (Phase 48).
//!
//! Three auth modes are supported, each emitting opaque [`SecretString`]
//! material to the plugin so a stray `tracing::debug!("{account:?}")`
//! cannot leak it:
//!
//! - **Password** — user + app-password (Outlook, iCloud, custom IMAP).
//! - **OAuth2Static** — pre-issued bearer; caller treats `expires_at`
//!   as advisory and refreshes externally if needed.
//! - **OAuth2Google** — username + an `id` that points at an account in
//!   the [`GoogleCredentialStore`]. Refresh + token rotation already
//!   live there — this variant simply delegates and reuses the
//!   per-account refresh mutex so concurrent IMAP IDLE workers do not
//!   trip Google's "concurrent refresh" 400.
//!
//! The TOML loader at `secrets/email/<instance>.toml` is the only
//! supported on-disk format; YAML mixes too many polymorphic shapes to
//! keep the secret-handling clear.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::error::{BuildError, CredentialError};
use crate::handle::{Channel, CredentialHandle, EMAIL};
use crate::store::{CredentialStore, ValidationReport};

#[derive(Clone)]
pub struct EmailAccount {
    pub instance: String,
    pub address: String,
    pub auth: EmailAuth,
    pub allow_agents: Vec<String>,
}

#[derive(Clone)]
pub enum EmailAuth {
    Password {
        username: String,
        password: SecretString,
    },
    OAuth2Static {
        username: String,
        access_token: SecretString,
        refresh_token: Option<SecretString>,
        /// Unix seconds when the access token expires. `None` = unknown
        /// or no-expiry; the plugin treats it as advisory.
        expires_at: Option<i64>,
    },
    OAuth2Google {
        username: String,
        /// Looks up token via [`GoogleCredentialStore`] at use-time.
        google_account_id: String,
    },
}

impl std::fmt::Debug for EmailAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("Password")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::OAuth2Static {
                username,
                refresh_token,
                expires_at,
                ..
            } => f
                .debug_struct("OAuth2Static")
                .field("username", username)
                .field("access_token", &"<redacted>")
                .field(
                    "refresh_token",
                    &refresh_token.as_ref().map(|_| "<redacted>"),
                )
                .field("expires_at", expires_at)
                .finish(),
            Self::OAuth2Google {
                username,
                google_account_id,
            } => f
                .debug_struct("OAuth2Google")
                .field("username", username)
                .field("google_account_id", google_account_id)
                .finish(),
        }
    }
}

impl std::fmt::Debug for EmailAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailAccount")
            .field("instance", &self.instance)
            .field("address", &self.address)
            .field("auth", &self.auth)
            .field("allow_agents", &self.allow_agents)
            .finish()
    }
}

impl EmailAccount {
    /// SASL `XOAUTH2` payload as specified by RFC 7628 §3.2.1, base64'd.
    /// Caller passes the freshly-resolved access token so this stays a
    /// pure function — easy to unit-test against vendor fixtures.
    pub fn xoauth2_sasl(username: &str, access_token: &str) -> String {
        let raw = format!("user={username}\x01auth=Bearer {access_token}\x01\x01");
        base64::engine::general_purpose::STANDARD.encode(raw)
    }

    /// Resolve a usable bearer token. For `Password` returns the
    /// password material itself (caller picks AUTH=PLAIN/LOGIN). For
    /// `OAuth2Static` returns the local access_token. For
    /// `OAuth2Google` delegates to the [`GoogleCredentialStore`] under
    /// its per-account refresh mutex; the caller is responsible for
    /// holding the returned `SecretString` no longer than necessary.
    pub async fn resolve_access_token(
        &self,
        google: &crate::google::GoogleCredentialStore,
    ) -> Result<SecretString, CredentialError> {
        match &self.auth {
            EmailAuth::Password { password, .. } => Ok(password.clone()),
            EmailAuth::OAuth2Static { access_token, .. } => Ok(access_token.clone()),
            EmailAuth::OAuth2Google {
                google_account_id, ..
            } => {
                let account = google.account(google_account_id).cloned().ok_or_else(|| {
                    CredentialError::NotFound {
                        channel: crate::handle::GOOGLE,
                        account: google_account_id.clone(),
                    }
                })?;
                // Acquire the per-account refresh mutex so two IDLE
                // workers do not race a token rotation on disk. The
                // lock is held only across the read; the actual rotate
                // is the Google plugin's job.
                let handle = CredentialHandle::new(
                    crate::handle::GOOGLE,
                    &account.id,
                    "<email-resolve>",
                );
                let lock = google.refresh_lock(&handle).ok_or(CredentialError::NotFound {
                    channel: crate::handle::GOOGLE,
                    account: google_account_id.clone(),
                })?;
                let _guard = lock.lock().await;
                let token =
                    std::fs::read_to_string(&account.token_path).map_err(|e| {
                        CredentialError::Unreadable {
                            path: account.token_path.clone(),
                            source: e,
                        }
                    })?;
                Ok(SecretString::new(token.trim().to_string()))
            }
        }
    }

    fn auth_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.auth {
            EmailAuth::Password { username, password } => {
                if username.trim().is_empty() {
                    out.push(format!(
                        "email instance '{}': password auth has empty username",
                        self.instance
                    ));
                }
                if password.expose_secret().is_empty() {
                    out.push(format!(
                        "email instance '{}': password auth has empty password",
                        self.instance
                    ));
                }
            }
            EmailAuth::OAuth2Static {
                username,
                access_token,
                ..
            } => {
                if username.trim().is_empty() {
                    out.push(format!(
                        "email instance '{}': oauth2_static auth has empty username",
                        self.instance
                    ));
                }
                if access_token.expose_secret().is_empty() {
                    out.push(format!(
                        "email instance '{}': oauth2_static auth has empty access_token",
                        self.instance
                    ));
                }
            }
            EmailAuth::OAuth2Google {
                username,
                google_account_id,
            } => {
                if username.trim().is_empty() {
                    out.push(format!(
                        "email instance '{}': oauth2_google auth has empty username",
                        self.instance
                    ));
                }
                if google_account_id.trim().is_empty() {
                    out.push(format!(
                        "email instance '{}': oauth2_google auth has empty google_account_id",
                        self.instance
                    ));
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct EmailCredentialStore {
    accounts: Arc<HashMap<String, EmailAccount>>,
}

impl EmailCredentialStore {
    pub fn new(accounts: Vec<EmailAccount>) -> Self {
        let mut map = HashMap::with_capacity(accounts.len());
        for a in accounts {
            map.insert(a.instance.clone(), a);
        }
        Self {
            accounts: Arc::new(map),
        }
    }

    pub fn empty() -> Self {
        Self {
            accounts: Arc::new(HashMap::new()),
        }
    }

    pub fn account(&self, instance: &str) -> Option<&EmailAccount> {
        self.accounts.get(instance)
    }
}

impl CredentialStore for EmailCredentialStore {
    type Account = EmailAccount;

    fn channel(&self) -> Channel {
        EMAIL
    }

    fn get(&self, handle: &CredentialHandle) -> Result<Self::Account, CredentialError> {
        let id = handle.account_id_raw();
        self.accounts
            .get(id)
            .cloned()
            .ok_or_else(|| CredentialError::NotFound {
                channel: EMAIL,
                account: id.to_string(),
            })
    }

    fn issue(&self, account_id: &str, agent_id: &str) -> Result<CredentialHandle, CredentialError> {
        let account = self
            .accounts
            .get(account_id)
            .ok_or_else(|| CredentialError::NotFound {
                channel: EMAIL,
                account: account_id.to_string(),
            })?;
        if !account.allow_agents.is_empty() && !account.allow_agents.iter().any(|a| a == agent_id) {
            let handle = CredentialHandle::new(EMAIL, account_id, agent_id);
            return Err(CredentialError::NotPermitted {
                channel: EMAIL,
                agent: agent_id.to_string(),
                fp: handle.fingerprint(),
            });
        }
        Ok(CredentialHandle::new(EMAIL, account_id, agent_id))
    }

    fn list(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.accounts.keys().cloned().collect();
        ids.sort();
        ids
    }

    fn allow_agents(&self, account_id: &str) -> Vec<String> {
        self.accounts
            .get(account_id)
            .map(|a| a.allow_agents.clone())
            .unwrap_or_default()
    }

    fn validate(&self) -> ValidationReport {
        let mut report = ValidationReport::default();
        for (id, a) in self.accounts.iter() {
            let warnings = a.auth_warnings();
            if warnings.is_empty() {
                report.accounts_ok += 1;
            } else {
                let _ = id;
                for w in warnings {
                    report.warnings.push(w);
                }
            }
        }
        report
    }
}

// ── TOML loader ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailSecretFile {
    auth: EmailAuthFile,
    #[serde(default)]
    allow_agents: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EmailAuthFile {
    Password {
        username: String,
        password: String,
    },
    Oauth2Static {
        username: String,
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_at: Option<i64>,
    },
    Oauth2Google {
        username: String,
        google_account_id: String,
    },
}

impl From<EmailAuthFile> for EmailAuth {
    fn from(f: EmailAuthFile) -> Self {
        match f {
            EmailAuthFile::Password { username, password } => EmailAuth::Password {
                username,
                password: SecretString::new(password),
            },
            EmailAuthFile::Oauth2Static {
                username,
                access_token,
                refresh_token,
                expires_at,
            } => EmailAuth::OAuth2Static {
                username,
                access_token: SecretString::new(access_token),
                refresh_token: refresh_token.map(SecretString::new),
                expires_at,
            },
            EmailAuthFile::Oauth2Google {
                username,
                google_account_id,
            } => EmailAuth::OAuth2Google {
                username,
                google_account_id,
            },
        }
    }
}

/// Load `<secrets_dir>/email/<instance>.toml` for every declared
/// account. Caller passes `(instance, address)` pairs from the plugin
/// config so the address ends up on the resulting [`EmailAccount`]
/// without re-parsing YAML inside this crate.
///
/// Returns the list plus warnings (non-fatal: empty fields, missing
/// optional values). Fatal problems (file missing, malformed TOML,
/// unknown kind) come back as [`BuildError::Credential`] entries —
/// boot accumulates them rather than failing fast.
pub fn load_email_secrets(
    secrets_dir: &Path,
    declared: &[(String, String)],
) -> (Vec<EmailAccount>, Vec<String>, Vec<BuildError>) {
    let mut accounts = Vec::with_capacity(declared.len());
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    for (instance, address) in declared {
        let path = secrets_dir.join("email").join(format!("{instance}.toml"));
        if !path.exists() {
            errors.push(BuildError::Credential {
                channel: EMAIL,
                instance: instance.clone(),
                source: CredentialError::FileMissing { path: path.clone() },
            });
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(BuildError::Credential {
                    channel: EMAIL,
                    instance: instance.clone(),
                    source: CredentialError::Unreadable {
                        path: path.clone(),
                        source: e,
                    },
                });
                continue;
            }
        };
        let resolved =
            match nexo_config::env::resolve_placeholders(&raw, &format!("email/{instance}.toml")) {
                Ok(s) => s,
                Err(e) => {
                    errors.push(BuildError::Credential {
                        channel: EMAIL,
                        instance: instance.clone(),
                        source: CredentialError::InvalidSecret {
                            path: path.clone(),
                            message: e.to_string(),
                        },
                    });
                    continue;
                }
            };
        let parsed: EmailSecretFile = match toml::from_str(&resolved) {
            Ok(p) => p,
            Err(e) => {
                errors.push(BuildError::Credential {
                    channel: EMAIL,
                    instance: instance.clone(),
                    source: CredentialError::InvalidSecret {
                        path: path.clone(),
                        message: e.message().to_string(),
                    },
                });
                continue;
            }
        };
        let auth: EmailAuth = parsed.auth.into();
        let account = EmailAccount {
            instance: instance.clone(),
            address: address.clone(),
            auth,
            allow_agents: parsed.allow_agents,
        };
        warnings.extend(account.auth_warnings());
        accounts.push(account);
    }

    (accounts, warnings, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pwd_account(instance: &str, allow: &[&str]) -> EmailAccount {
        EmailAccount {
            instance: instance.into(),
            address: format!("{instance}@example.com"),
            auth: EmailAuth::Password {
                username: format!("{instance}@example.com"),
                password: SecretString::new("hunter2".into()),
            },
            allow_agents: allow.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn auth_debug_does_not_leak_password() {
        let auth = EmailAuth::Password {
            username: "u@x".into(),
            password: SecretString::new("super-secret-pw".into()),
        };
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("super-secret-pw"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn auth_debug_does_not_leak_access_token() {
        let auth = EmailAuth::OAuth2Static {
            username: "u@x".into(),
            access_token: SecretString::new("ya29.tk".into()),
            refresh_token: Some(SecretString::new("rt-1".into())),
            expires_at: Some(1_700_000_000),
        };
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("ya29.tk"));
        assert!(!rendered.contains("rt-1"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn account_debug_does_not_leak_secrets() {
        let acct = pwd_account("ops", &[]);
        let rendered = format!("{acct:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("ops"));
    }

    #[test]
    fn xoauth2_sasl_matches_rfc7628_fixture() {
        // RFC 7628 §3.2.1 example: user=someuser@example.com\x01auth=Bearer vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg==\x01\x01
        let out = EmailAccount::xoauth2_sasl(
            "someuser@example.com",
            "vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg==",
        );
        // Decode and verify the inner format rather than hard-coding the
        // base64 string — encoders can pick different alphabets.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(out)
            .unwrap();
        let s = String::from_utf8(decoded).unwrap();
        assert_eq!(
            s,
            "user=someuser@example.com\x01auth=Bearer vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg==\x01\x01"
        );
    }

    #[test]
    fn store_issue_returns_handle_when_permitted() {
        let s = EmailCredentialStore::new(vec![pwd_account("ops", &["ana"])]);
        let h = s.issue("ops", "ana").unwrap();
        assert_eq!(h.channel(), EMAIL);
        assert_eq!(h.agent_id(), "ana");
    }

    #[test]
    fn store_issue_rejects_non_allowed_agent() {
        let s = EmailCredentialStore::new(vec![pwd_account("ops", &["ana"])]);
        let err = s.issue("ops", "kate").unwrap_err();
        assert!(matches!(err, CredentialError::NotPermitted { .. }));
    }

    #[test]
    fn store_empty_allow_list_accepts_anyone() {
        let s = EmailCredentialStore::new(vec![pwd_account("ops", &[])]);
        assert!(s.issue("ops", "ana").is_ok());
        assert!(s.issue("ops", "kate").is_ok());
    }

    #[test]
    fn store_list_is_sorted() {
        let s = EmailCredentialStore::new(vec![
            pwd_account("b", &[]),
            pwd_account("a", &[]),
            pwd_account("c", &[]),
        ]);
        assert_eq!(s.list(), vec!["a", "b", "c"]);
    }

    #[test]
    fn store_validate_warns_empty_password() {
        let acct = EmailAccount {
            auth: EmailAuth::Password {
                username: "u@x".into(),
                password: SecretString::new(String::new()),
            },
            ..pwd_account("ops", &[])
        };
        let s = EmailCredentialStore::new(vec![acct]);
        let r = s.validate();
        assert_eq!(r.accounts_ok, 0);
        assert!(r.warnings.iter().any(|w| w.contains("empty password")));
    }

    #[test]
    fn store_validate_warns_empty_oauth_token() {
        let acct = EmailAccount {
            auth: EmailAuth::OAuth2Static {
                username: "u@x".into(),
                access_token: SecretString::new(String::new()),
                refresh_token: None,
                expires_at: None,
            },
            ..pwd_account("ops", &[])
        };
        let s = EmailCredentialStore::new(vec![acct]);
        let r = s.validate();
        assert_eq!(r.accounts_ok, 0);
        assert!(r
            .warnings
            .iter()
            .any(|w| w.contains("empty access_token")));
    }

    #[test]
    fn store_missing_instance_errors() {
        let s = EmailCredentialStore::empty();
        let err = s.issue("nope", "ana").unwrap_err();
        assert!(matches!(err, CredentialError::NotFound { .. }));
    }

    fn write_secret(dir: &Path, instance: &str, body: &str) {
        let inst_dir = dir.join("email");
        std::fs::create_dir_all(&inst_dir).unwrap();
        let path = inst_dir.join(format!("{instance}.toml"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn loader_password_account() {
        let tmp = tempfile::tempdir().unwrap();
        write_secret(
            tmp.path(),
            "ops",
            r#"
[auth]
kind = "password"
username = "ops@example.com"
password = "hunter2"
"#,
        );
        let (accs, warns, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@example.com".into())]);
        assert!(errs.is_empty(), "errs={errs:?}");
        assert!(warns.is_empty());
        assert_eq!(accs.len(), 1);
        match &accs[0].auth {
            EmailAuth::Password { username, password } => {
                assert_eq!(username, "ops@example.com");
                assert_eq!(password.expose_secret(), "hunter2");
            }
            _ => panic!("expected Password variant"),
        }
    }

    #[test]
    fn loader_oauth2_static_account() {
        let tmp = tempfile::tempdir().unwrap();
        write_secret(
            tmp.path(),
            "ops",
            r#"
[auth]
kind = "oauth2_static"
username = "ops@gmail.com"
access_token = "ya29.fresh"
refresh_token = "1//rt"
expires_at = 1735689600
"#,
        );
        let (accs, warns, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@gmail.com".into())]);
        assert!(errs.is_empty());
        assert!(warns.is_empty());
        match &accs[0].auth {
            EmailAuth::OAuth2Static {
                access_token,
                refresh_token,
                expires_at,
                ..
            } => {
                assert_eq!(access_token.expose_secret(), "ya29.fresh");
                assert_eq!(refresh_token.as_ref().unwrap().expose_secret(), "1//rt");
                assert_eq!(*expires_at, Some(1_735_689_600));
            }
            _ => panic!("expected OAuth2Static"),
        }
    }

    #[test]
    fn loader_oauth2_google_account() {
        let tmp = tempfile::tempdir().unwrap();
        write_secret(
            tmp.path(),
            "ops",
            r#"
[auth]
kind = "oauth2_google"
username = "ops@gmail.com"
google_account_id = "ops"
"#,
        );
        let (accs, _, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@gmail.com".into())]);
        assert!(errs.is_empty());
        assert!(matches!(accs[0].auth, EmailAuth::OAuth2Google { .. }));
    }

    #[test]
    fn loader_missing_file_yields_build_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (accs, _, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@example.com".into())]);
        assert!(accs.is_empty());
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            BuildError::Credential {
                channel,
                source: CredentialError::FileMissing { .. },
                ..
            } => assert_eq!(*channel, EMAIL),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn loader_malformed_toml_yields_build_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_secret(tmp.path(), "ops", "this is not toml @@@");
        let (_, _, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@example.com".into())]);
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            BuildError::Credential {
                source: CredentialError::InvalidSecret { .. },
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn loader_unknown_kind_yields_build_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_secret(
            tmp.path(),
            "ops",
            r#"
[auth]
kind = "totally_made_up"
username = "x"
"#,
        );
        let (_, _, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@example.com".into())]);
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            BuildError::Credential {
                source: CredentialError::InvalidSecret { .. },
                ..
            } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn loader_resolves_env_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: test is single-threaded for env var; nexo_config's env
        // resolver reads the live env, so we rely on the standard
        // ENV_LOCK convention used in load_test.rs. Use a unique name
        // to avoid collisions with other tests.
        std::env::set_var("EMAIL_TEST_PASS_48_2", "from-env");
        write_secret(
            tmp.path(),
            "ops",
            r#"
[auth]
kind = "password"
username = "ops@example.com"
password = "${EMAIL_TEST_PASS_48_2}"
"#,
        );
        let (accs, _, errs) =
            load_email_secrets(tmp.path(), &[("ops".into(), "ops@example.com".into())]);
        std::env::remove_var("EMAIL_TEST_PASS_48_2");
        assert!(errs.is_empty(), "errs={errs:?}");
        match &accs[0].auth {
            EmailAuth::Password { password, .. } => {
                assert_eq!(password.expose_secret(), "from-env");
            }
            _ => panic!("expected Password"),
        }
    }

    #[tokio::test]
    async fn resolve_token_password_returns_inline() {
        let acct = pwd_account("ops", &[]);
        let google = crate::google::GoogleCredentialStore::empty();
        let tok = acct.resolve_access_token(&google).await.unwrap();
        assert_eq!(tok.expose_secret(), "hunter2");
    }

    #[tokio::test]
    async fn resolve_token_oauth2_google_unknown_errors() {
        let acct = EmailAccount {
            instance: "ops".into(),
            address: "ops@gmail.com".into(),
            auth: EmailAuth::OAuth2Google {
                username: "ops@gmail.com".into(),
                google_account_id: "missing".into(),
            },
            allow_agents: vec![],
        };
        let google = crate::google::GoogleCredentialStore::empty();
        let err = acct.resolve_access_token(&google).await.unwrap_err();
        assert!(matches!(err, CredentialError::NotFound { .. }));
    }
}
