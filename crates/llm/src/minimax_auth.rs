//! MiniMax auth sources: static API key and Token Plan OAuth bundle.
//!
//! A [`MiniMaxClient`] picks one of these at construction and consults
//! it before every request. Static mode is a single `String` clone —
//! essentially free. Token Plan mode stores `access + refresh + expires`
//! behind an async `RwLock` and auto-refreshes against
//! `https://api.minimax.{io,minimaxi.com}/oauth/token` a minute before
//! expiry. A dedicated `Mutex` serialises refreshes so concurrent
//! requests on a near-expiry token only hit the network once.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

/// Shared client id from the MiniMax Portal flow — matches OpenClaw's
/// reference implementation. Not a secret.
pub const PORTAL_CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
pub const PORTAL_REFRESH_GRANT: &str = "refresh_token";
/// Refresh window — if the access token dies within this many seconds
/// of "now", refresh eagerly.
const REFRESH_MARGIN_SECS: i64 = 60;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TokenPlanBundle {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    /// Base URL captured at wizard time — global vs cn determines which
    /// OAuth endpoint we refresh against.
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub obtained_at: Option<String>,
}

impl TokenPlanBundle {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let bundle: Self =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if bundle.access_token.is_empty() || bundle.refresh_token.is_empty() {
            bail!(
                "bundle {} is missing access_token or refresh_token",
                path.display()
            );
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

/// Inner mutable state for Token Plan mode. Access token and expiry
/// change on every refresh; `refresh_token` may be rotated by the
/// server (MiniMax does) so we update it too.
pub struct TokenPlanState {
    access: RwLock<String>,
    refresh: RwLock<String>,
    expires_at: AtomicI64,
    bundle_path: PathBuf,
    refresh_base_url: String,
    /// Anti-thundering-herd: only one in-flight refresh at a time.
    refresh_lock: Mutex<()>,
}

#[derive(Clone)]
pub enum AuthSource {
    Static(Arc<String>),
    TokenPlan(Arc<TokenPlanState>),
}

impl AuthSource {
    pub fn static_key(key: String) -> Self {
        Self::Static(Arc::new(key))
    }

    pub fn from_bundle(bundle: TokenPlanBundle, bundle_path: PathBuf) -> Self {
        let refresh_base_url = bundle
            .region
            .clone()
            .unwrap_or_else(|| "https://api.minimax.io".to_string());
        Self::TokenPlan(Arc::new(TokenPlanState {
            access: RwLock::new(bundle.access_token.clone()),
            refresh: RwLock::new(bundle.refresh_token.clone()),
            expires_at: AtomicI64::new(bundle.expires_at),
            bundle_path,
            refresh_base_url,
            refresh_lock: Mutex::new(()),
        }))
    }

    /// Return the current bearer token, refreshing first when the
    /// access token is within [`REFRESH_MARGIN_SECS`] of expiry.
    pub async fn bearer(&self, http: &reqwest::Client) -> Result<String> {
        match self {
            Self::Static(s) => Ok(s.as_ref().clone()),
            Self::TokenPlan(state) => {
                if should_refresh(state) {
                    self.refresh(http).await?;
                }
                Ok(state.access.read().await.clone())
            }
        }
    }

    /// Force a refresh — used when a request comes back 401 even
    /// though our cached expiry said we were still valid (clock skew,
    /// server-side revocation, etc).
    pub async fn force_refresh(&self, http: &reqwest::Client) -> Result<()> {
        match self {
            Self::Static(_) => Ok(()),
            Self::TokenPlan(_) => self.refresh(http).await,
        }
    }

    async fn refresh(&self, http: &reqwest::Client) -> Result<()> {
        let Self::TokenPlan(state) = self else {
            return Ok(());
        };

        // Only one refresh at a time; everyone else waits here and
        // re-checks the expiry once the leader finishes.
        let _guard = state.refresh_lock.lock().await;
        if !should_refresh(state) {
            return Ok(());
        }

        let refresh_token = state.refresh.read().await.clone();
        let resp = http
            .post(format!("{}/oauth/token", state.refresh_base_url))
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", PORTAL_REFRESH_GRANT),
                ("client_id", PORTAL_CLIENT_ID),
                ("refresh_token", refresh_token.as_str()),
            ])
            .send()
            .await
            .context("MiniMax refresh POST failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("MiniMax refresh HTTP {status}: {text}");
        }
        let parsed: RefreshResp =
            serde_json::from_str(&text).context("parsing MiniMax refresh response")?;
        let new_access = parsed
            .access_token
            .ok_or_else(|| anyhow!("refresh response missing access_token: {text}"))?;
        let new_refresh = parsed.refresh_token.unwrap_or(refresh_token);
        let ttl = parsed.expired_in.unwrap_or(3600).max(60);
        let now = unix_now();
        let new_expires = now + ttl;

        {
            let mut w = state.access.write().await;
            *w = new_access.clone();
        }
        {
            let mut w = state.refresh.write().await;
            *w = new_refresh.clone();
        }
        state.expires_at.store(new_expires, Ordering::SeqCst);

        // Persist so the next process start picks up the new tokens.
        let bundle = TokenPlanBundle {
            access_token: new_access,
            refresh_token: new_refresh,
            expires_at: new_expires,
            region: Some(state.refresh_base_url.clone()),
            obtained_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        if let Err(e) = bundle.save_atomic(&state.bundle_path) {
            tracing::warn!(error = %e, path = %state.bundle_path.display(), "persist refreshed bundle");
        } else {
            tracing::info!(
                path = %state.bundle_path.display(),
                expires_at = new_expires,
                "MiniMax token refreshed"
            );
        }
        Ok(())
    }
}

fn should_refresh(state: &TokenPlanState) -> bool {
    let expires = state.expires_at.load(Ordering::SeqCst);
    let now = unix_now();
    expires - now <= REFRESH_MARGIN_SECS
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Deserialize)]
struct RefreshResp {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expired_in: Option<i64>,
}

/// Decide what [`AuthSource`] to build for a given provider config.
///
/// Resolution order (mirrors OpenClaw's `minimax` provider):
///
/// 1. Explicit `auth.mode = token_plan` + existing bundle → Token Plan.
/// 2. `auth.mode = auto` + bundle file present → Token Plan.
/// 3. Env `MINIMAX_CODE_PLAN_KEY` (the "Coding/Token Plan key",
///    `sk-cp-…`) → static, takes precedence over `MINIMAX_API_KEY`.
/// 4. Env `MINIMAX_CODING_API_KEY` → static.
/// 5. `secrets/minimax_code_plan_key.txt` on disk → static.
/// 6. Fall back to `cfg.api_key` (typically `${MINIMAX_API_KEY}`).
pub fn build_auth_source(cfg: &nexo_config::LlmProviderConfig) -> Result<AuthSource> {
    let auth = cfg.auth.as_ref();
    let mode = auth.map(|a| a.mode.as_str()).unwrap_or("auto");
    let bundle_path = auth
        .and_then(|a| a.bundle.as_ref())
        .map(PathBuf::from)
        .unwrap_or_else(default_bundle_path);

    match mode {
        "static" => Ok(AuthSource::static_key(resolve_static_key(cfg))),
        "token_plan" => {
            let bundle = TokenPlanBundle::load(&bundle_path)
                .with_context(|| format!("loading Token Plan bundle {}", bundle_path.display()))?;
            Ok(AuthSource::from_bundle(bundle, bundle_path))
        }
        "auto" => {
            if bundle_path.exists() {
                match TokenPlanBundle::load(&bundle_path) {
                    Ok(bundle) => return Ok(AuthSource::from_bundle(bundle, bundle_path)),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "Token Plan bundle unreadable — falling back to static keys"
                    ),
                }
            }
            Ok(AuthSource::static_key(resolve_static_key(cfg)))
        }
        other => bail!("unknown auth.mode `{other}` (expected auto|static|token_plan)"),
    }
}

/// Walk the same fallback chain as OpenClaw's `minimax` provider.
///
/// Env vars win over anything in YAML so operators can override at
/// runtime without patching config files. File-on-disk checks come
/// last so the wizard's `secrets/*.txt` files flow through even when
/// the operator didn't remember to `export`.
fn resolve_static_key(cfg: &nexo_config::LlmProviderConfig) -> String {
    for env_var in ["MINIMAX_CODE_PLAN_KEY", "MINIMAX_CODING_API_KEY"] {
        if let Ok(v) = std::env::var(env_var) {
            if !v.is_empty() {
                tracing::info!(source = env_var, "MiniMax using key from env");
                return v;
            }
        }
    }
    for candidate in [
        "./secrets/minimax_code_plan_key.txt",
        "./secrets/minimax_coding_api_key.txt",
    ] {
        if let Ok(v) = std::fs::read_to_string(candidate) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                tracing::info!(source = candidate, "MiniMax using key from disk");
                return v;
            }
        }
    }
    cfg.api_key.clone()
}

fn default_bundle_path() -> PathBuf {
    PathBuf::from("./secrets/minimax_portal.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_bearer_returns_plain_string() {
        let src = AuthSource::static_key("sk-test".into());
        let http = reqwest::Client::new();
        let v = tokio_test::block_on(async { src.bearer(&http).await.unwrap() });
        assert_eq!(v, "sk-test");
    }

    #[test]
    fn bundle_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bundle.json");
        let b = TokenPlanBundle {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            expires_at: 1_700_000_000,
            region: Some("https://api.minimax.io".into()),
            obtained_at: None,
        };
        b.save_atomic(&path).unwrap();
        let loaded = TokenPlanBundle::load(&path).unwrap();
        assert_eq!(loaded.access_token, "acc");
        assert_eq!(loaded.refresh_token, "ref");
    }

    #[test]
    fn bundle_rejects_empty_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bundle.json");
        std::fs::write(
            &path,
            r#"{"access_token":"","refresh_token":"","expires_at":0}"#,
        )
        .unwrap();
        assert!(TokenPlanBundle::load(&path).is_err());
    }

    #[test]
    fn auto_mode_falls_back_when_bundle_missing() {
        let cfg = nexo_config::LlmProviderConfig {
            api_key: "sk-fallback".into(),
            base_url: "https://x".into(),
            group_id: None,
            rate_limit: Default::default(),
            auth: Some(nexo_config::LlmAuthConfig {
                mode: "auto".into(),
                bundle: Some("/tmp/definitely-does-not-exist-xyz.json".into()),
                setup_token_file: None,
                refresh_endpoint: None,
                client_id: None,
            }),
            api_flavor: None,
            embedding_model: None,
            safety_settings: None,
        };
        let src = build_auth_source(&cfg).unwrap();
        match src {
            AuthSource::Static(s) => assert_eq!(s.as_ref(), "sk-fallback"),
            _ => panic!("expected static fallback"),
        }
    }
}
