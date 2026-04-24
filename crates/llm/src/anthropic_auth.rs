//! Anthropic auth sources: API key, setup-token (Bearer), and OAuth
//! subscription bundle with refresh.
//!
//! Mirrors `minimax_auth.rs`. Three variants:
//!
//! * [`AnthropicAuth::ApiKey`] — classic `sk-ant-…`; wire header
//!   `x-api-key`, no beta header.
//! * [`AnthropicAuth::SetupToken`] — `sk-ant-oat01-…` from
//!   `claude setup-token`. Wire header `Authorization: Bearer …`
//!   plus `anthropic-beta: oauth-2025-04-20`. Write-once upstream.
//! * [`AnthropicAuth::OAuth`] — subscription bundle
//!   (access + refresh + expires). Same headers as setup-token, but
//!   auto-refreshes a minute before expiry against the Claude Code
//!   OAuth endpoint. A dedicated `Mutex` serialises refreshes so
//!   concurrent callers hit the network once.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

/// Default OAuth token endpoint — matches the one Claude Code CLI
/// uses. Overridable via `LlmAuthConfig::refresh_endpoint`.
pub const DEFAULT_REFRESH_ENDPOINT: &str = "https://console.anthropic.com/v1/oauth/token";
/// Public OAuth client_id for Claude Code CLI. Not a secret.
pub const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Beta header required by Anthropic for Bearer-auth requests.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Setup-token prefix (OpenClaw `provider-auth-token.ts`).
pub const SETUP_TOKEN_PREFIX: &str = "sk-ant-oat01-";
pub const SETUP_TOKEN_MIN_LEN: usize = 80;

const REFRESH_MARGIN_SECS: i64 = 60;

/// On-disk OAuth bundle shape. Written by the setup wizard; read and
/// rewritten here on every refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthBundle {
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds since unix epoch.
    pub expires_at: i64,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(default)]
    pub obtained_at: Option<String>,
    /// `claude-cli` | `paste` | `wizard` — informational only.
    #[serde(default)]
    pub source: Option<String>,
}

impl OAuthBundle {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let bundle: Self =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if bundle.access_token.is_empty() {
            bail!("bundle {} is missing access_token", path.display());
        }
        Ok(bundle)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
        serde_json::to_writer_pretty(&tmp, self)?;
        tmp.as_file().sync_all().ok();
        tmp.persist(path)
            .map_err(|e| anyhow!("persist bundle {}: {e}", path.display()))?;
        Ok(())
    }
}

/// Headers resolved for a specific auth variant. `beta` is `Some`
/// when the variant uses Bearer auth and needs the OAuth beta header.
pub struct AuthHeaders {
    pub auth: (&'static str, String),
    pub beta: Option<&'static str>,
}

/// Mutable OAuth state — `access` / `refresh` rotate on refresh.
pub struct OAuthState {
    access: RwLock<String>,
    refresh: RwLock<String>,
    expires_at: AtomicI64,
    bundle_path: PathBuf,
    refresh_endpoint: String,
    client_id: String,
    refresh_lock: Mutex<()>,
    account_email: Option<String>,
}

impl OAuthState {
    pub fn new(
        bundle: OAuthBundle,
        bundle_path: PathBuf,
        refresh_endpoint: String,
        client_id: String,
    ) -> Self {
        Self {
            access: RwLock::new(bundle.access_token),
            refresh: RwLock::new(bundle.refresh_token),
            expires_at: AtomicI64::new(bundle.expires_at),
            bundle_path,
            refresh_endpoint,
            client_id,
            refresh_lock: Mutex::new(()),
            account_email: bundle.account_email,
        }
    }

    /// Mark the access token stale so the next `ensure_fresh()` call
    /// forces a refresh (used when the server hands back 401 despite
    /// our cached expiry saying we were fine).
    pub fn mark_stale(&self) {
        self.expires_at.store(0, Ordering::SeqCst);
    }

    /// Return a live access token, refreshing if we are within
    /// [`REFRESH_MARGIN_SECS`] of expiry.
    pub async fn ensure_fresh(&self, http: &reqwest::Client) -> Result<String> {
        if self.should_refresh() {
            self.refresh(http).await?;
        }
        Ok(self.access.read().await.clone())
    }

    fn should_refresh(&self) -> bool {
        let expires = self.expires_at.load(Ordering::SeqCst);
        let now = unix_now();
        expires - now <= REFRESH_MARGIN_SECS
    }

    async fn refresh(&self, http: &reqwest::Client) -> Result<()> {
        let _guard = self.refresh_lock.lock().await;
        if !self.should_refresh() {
            return Ok(());
        }
        let refresh_token = self.refresh.read().await.clone();
        let resp = http
            .post(&self.refresh_endpoint)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": self.client_id,
            }))
            .send()
            .await
            .context("Anthropic OAuth refresh POST failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // Surface invalid_grant distinctly so callers can ask the
            // operator to re-run the setup wizard instead of retrying.
            if text.contains("invalid_grant") || text.contains("refresh_token_reused") {
                bail!(
                    "Anthropic OAuth refresh invalid_grant (HTTP {status}): {} — \
                     run `agent setup anthropic` to re-authenticate",
                    truncate(&text, 200)
                );
            }
            bail!(
                "Anthropic OAuth refresh HTTP {status}: {}",
                truncate(&text, 200)
            );
        }
        let parsed: RefreshResp = serde_json::from_str(&text)
            .with_context(|| format!("parsing Anthropic refresh response: {text}"))?;
        let new_access = parsed
            .access_token
            .ok_or_else(|| anyhow!("refresh response missing access_token: {text}"))?;
        let new_refresh = parsed.refresh_token.unwrap_or(refresh_token);
        let ttl = parsed.expires_in.unwrap_or(3600).max(60);
        let now = unix_now();
        let new_expires = now + ttl;

        {
            let mut w = self.access.write().await;
            *w = new_access.clone();
        }
        {
            let mut w = self.refresh.write().await;
            *w = new_refresh.clone();
        }
        self.expires_at.store(new_expires, Ordering::SeqCst);

        let bundle = OAuthBundle {
            access_token: new_access,
            refresh_token: new_refresh,
            expires_at: new_expires,
            account_email: self.account_email.clone(),
            obtained_at: Some(chrono::Utc::now().to_rfc3339()),
            source: Some("refresh".into()),
        };
        if let Err(e) = bundle.save_atomic(&self.bundle_path) {
            tracing::warn!(error = %e, path = %self.bundle_path.display(), "persist refreshed anthropic bundle");
        } else {
            tracing::info!(
                path = %self.bundle_path.display(),
                expires_at = new_expires,
                "Anthropic OAuth token refreshed"
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RefreshResp {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Resolved auth the client uses at request time.
#[derive(Clone)]
pub enum AnthropicAuth {
    ApiKey(Arc<String>),
    SetupToken(Arc<String>),
    OAuth(Arc<OAuthState>),
}

impl AnthropicAuth {
    pub fn api_key(key: String) -> Self {
        Self::ApiKey(Arc::new(key))
    }

    pub fn setup_token(token: String) -> Self {
        Self::SetupToken(Arc::new(token))
    }

    pub fn oauth(state: Arc<OAuthState>) -> Self {
        Self::OAuth(state)
    }

    /// Return the headers to attach to the next Anthropic request.
    /// For OAuth this may trigger a network refresh when the access
    /// token is near expiry.
    pub async fn resolve_headers(&self, http: &reqwest::Client) -> Result<AuthHeaders> {
        match self {
            Self::ApiKey(k) => Ok(AuthHeaders {
                auth: ("x-api-key", k.as_ref().clone()),
                beta: None,
            }),
            Self::SetupToken(t) => Ok(AuthHeaders {
                auth: ("Authorization", format!("Bearer {}", t.as_ref())),
                beta: Some(OAUTH_BETA),
            }),
            Self::OAuth(state) => {
                let access = state.ensure_fresh(http).await?;
                Ok(AuthHeaders {
                    auth: ("Authorization", format!("Bearer {access}")),
                    beta: Some(OAUTH_BETA),
                })
            }
        }
    }

    /// Mark OAuth state stale so the next request refreshes. No-op for
    /// ApiKey / SetupToken.
    pub fn mark_stale(&self) {
        if let Self::OAuth(state) = self {
            state.mark_stale();
        }
    }

    /// True when the variant signals user subscription credentials
    /// (used by error classification to surface "re-run wizard" hints).
    pub fn is_subscription(&self) -> bool {
        matches!(self, Self::SetupToken(_) | Self::OAuth(_))
    }
}

/// Validate a pasted setup-token. Returns the trimmed token or the
/// reason the input is rejected.
pub fn validate_setup_token(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with(SETUP_TOKEN_PREFIX) {
        bail!("setup-token must start with `{SETUP_TOKEN_PREFIX}`");
    }
    if trimmed.len() < SETUP_TOKEN_MIN_LEN {
        bail!(
            "setup-token looks too short ({} < {SETUP_TOKEN_MIN_LEN})",
            trimmed.len()
        );
    }
    Ok(trimmed.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Read credentials deposited by the Claude Code CLI. See
/// [`read_claude_cli_credentials_from_file`] for the file path; the
/// macOS Keychain fallback lives in platform-gated code.
pub fn read_claude_cli_credentials() -> Option<OAuthBundle> {
    #[cfg(target_os = "macos")]
    {
        if let Some(b) = read_from_macos_keychain() {
            return Some(b);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".claude").join(".credentials.json");
    read_claude_cli_credentials_from_file(&path).ok()
}

/// Pure-file variant used by tests (no Keychain access).
pub fn read_claude_cli_credentials_from_file(path: &Path) -> Result<OAuthBundle> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_claude_cli_json(&text)
        .with_context(|| format!("parse {}", path.display()))
}

fn parse_claude_cli_json(text: &str) -> Result<OAuthBundle> {
    let root: serde_json::Value = serde_json::from_str(text)?;
    let creds = root
        .get("claudeAiOauth")
        .ok_or_else(|| anyhow!("missing `claudeAiOauth` field"))?;
    let access = creds
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing accessToken"))?
        .to_string();
    let refresh = creds
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    // Claude CLI stores `expiresAt` in **milliseconds** since epoch.
    let expires_ms = creds
        .get("expiresAt")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let expires_at = if expires_ms > 10_000_000_000 {
        expires_ms / 1000
    } else {
        expires_ms
    };
    let account_email = creds
        .get("account")
        .and_then(|v| v.get("emailAddress"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(OAuthBundle {
        access_token: access,
        refresh_token: refresh,
        expires_at,
        account_email,
        obtained_at: Some(chrono::Utc::now().to_rfc3339()),
        source: Some("claude-cli".into()),
    })
}

#[cfg(target_os = "macos")]
fn read_from_macos_keychain() -> Option<OAuthBundle> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    parse_claude_cli_json(raw.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_bundle() -> OAuthBundle {
        OAuthBundle {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            expires_at: 1_700_000_000,
            account_email: Some("x@y.z".into()),
            obtained_at: Some("2025-01-01T00:00:00Z".into()),
            source: Some("wizard".into()),
        }
    }

    #[test]
    fn bundle_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sub").join("bundle.json");
        let b = sample_bundle();
        b.save_atomic(&path).unwrap();
        let loaded = OAuthBundle::load(&path).unwrap();
        assert_eq!(loaded.access_token, "acc");
        assert_eq!(loaded.refresh_token, "ref");
        assert_eq!(loaded.account_email.as_deref(), Some("x@y.z"));
    }

    #[test]
    fn bundle_load_rejects_empty_access() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("b.json");
        std::fs::write(
            &path,
            r#"{"access_token":"","refresh_token":"r","expires_at":0}"#,
        )
        .unwrap();
        assert!(OAuthBundle::load(&path).is_err());
    }

    #[test]
    fn validate_setup_token_good() {
        let t = format!("{}{}", SETUP_TOKEN_PREFIX, "x".repeat(80));
        assert!(validate_setup_token(&t).is_ok());
    }

    #[test]
    fn validate_setup_token_bad_prefix() {
        assert!(validate_setup_token("sk-ant-api03-abc...").is_err());
    }

    #[test]
    fn validate_setup_token_too_short() {
        assert!(validate_setup_token("sk-ant-oat01-short").is_err());
    }

    #[tokio::test]
    async fn api_key_headers_use_x_api_key() {
        let auth = AnthropicAuth::api_key("sk-ant-test".into());
        let h = auth.resolve_headers(&reqwest::Client::new()).await.unwrap();
        assert_eq!(h.auth.0, "x-api-key");
        assert_eq!(h.auth.1, "sk-ant-test");
        assert!(h.beta.is_none());
    }

    #[tokio::test]
    async fn setup_token_headers_bearer_with_beta() {
        let auth = AnthropicAuth::setup_token("sk-ant-oat01-xxx".into());
        let h = auth.resolve_headers(&reqwest::Client::new()).await.unwrap();
        assert_eq!(h.auth.0, "Authorization");
        assert!(h.auth.1.starts_with("Bearer sk-ant-oat01-"));
        assert_eq!(h.beta, Some(OAUTH_BETA));
    }

    #[test]
    fn parse_claude_cli_json_converts_ms_expiry() {
        let j = r#"{"claudeAiOauth":{
            "accessToken":"acc","refreshToken":"r",
            "expiresAt":1735689600000,
            "account":{"emailAddress":"u@example.com"}}}"#;
        let b = parse_claude_cli_json(j).unwrap();
        assert_eq!(b.access_token, "acc");
        assert_eq!(b.refresh_token, "r");
        assert_eq!(b.expires_at, 1_735_689_600);
        assert_eq!(b.account_email.as_deref(), Some("u@example.com"));
        assert_eq!(b.source.as_deref(), Some("claude-cli"));
    }

    #[test]
    fn read_claude_cli_credentials_from_file_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":0}}"#,
        )
        .unwrap();
        let b = read_claude_cli_credentials_from_file(&path).unwrap();
        assert_eq!(b.access_token, "a");
    }

    #[test]
    fn mark_stale_forces_refresh_flag() {
        let dir = tempdir().unwrap();
        let state = OAuthState::new(
            OAuthBundle {
                access_token: "a".into(),
                refresh_token: "r".into(),
                // Far in the future so should_refresh() is false.
                expires_at: unix_now() + 10_000,
                account_email: None,
                obtained_at: None,
                source: None,
            },
            dir.path().join("b.json"),
            DEFAULT_REFRESH_ENDPOINT.into(),
            DEFAULT_CLIENT_ID.into(),
        );
        assert!(!state.should_refresh());
        state.mark_stale();
        assert!(state.should_refresh());
    }
}
