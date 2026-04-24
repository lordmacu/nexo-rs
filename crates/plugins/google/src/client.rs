//! Google OAuth 2.0 — installed-application flow.
//!
//! Lets an agent authenticate against Google APIs (Gmail, Drive,
//! Calendar, Sheets, etc.) using the user's own account. The flow:
//!
//! 1. `google_auth_start` → returns a URL; the agent tells the user
//!    (via chat) to open it.
//! 2. Listener on `127.0.0.1:<redirect_port>` catches the callback.
//! 3. Exchange auth code → `{access_token, refresh_token}`.
//! 4. Persist refresh_token to a JSON file in the agent's workspace.
//!    Subsequent boots read that file and mint fresh access tokens.
//! 5. `google_call` issues authenticated HTTP requests, refreshing the
//!    access token transparently when it's < 60s from expiring.
//!
//! Requires env vars `GOOGLE_CLIENT_ID` + `GOOGLE_CLIENT_SECRET`
//! (Desktop app OAuth client from Google Cloud Console).
//!
//! Storage format — JSON at `token_file`:
//!
//! ```json
//! {
//!   "access_token":  "ya29.a0Ad...",
//!   "refresh_token": "1//0g...",
//!   "expires_at":    1720000000,
//!   "scopes":        ["https://www.googleapis.com/auth/gmail.readonly", ...]
//! }
//! ```
//!
//! The file ships at mode 600 — whoever reads the workspace owns the
//! tokens. In prod bind-mount the workspace as a writable volume or
//! emplace the file via Docker secrets if you can swing it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, RwLock};

/// One-shot challenge returned by `request_device_code`. Show the
/// `user_code` to the operator + tell them to open `verification_url`.
#[derive(Debug, Clone)]
pub struct DeviceChallenge {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    /// Seconds the code stays valid. Default 1800 (30 min).
    pub expires_in: u64,
    /// Minimum seconds between polls. Bump on `slow_down` responses.
    pub interval: u64,
}

/// Shape of `agents.<id>.google_auth` in `agents.yaml`. `None` → the
/// google_* tools are not registered for this agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleAuthConfig {
    /// From Google Cloud Console → OAuth 2.0 Client ID (Desktop app).
    pub client_id: String,
    /// Paired with `client_id`; same screen.
    pub client_secret: String,
    /// List of OAuth scopes, e.g.
    /// `["https://www.googleapis.com/auth/gmail.readonly"]`. Short-form
    /// names (`gmail.readonly`, `drive.readonly`) are accepted and
    /// expanded to the canonical URL at load time.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Where to persist the refresh_token. Relative paths resolve from
    /// the agent's workspace directory. Default:
    /// `<workspace>/google_tokens.json`. Set absolute for tests.
    #[serde(default = "default_token_file")]
    pub token_file: String,
    /// Port the loopback callback server binds to during auth. Must
    /// match an "Authorized redirect URI" entry in the OAuth client
    /// config: `http://127.0.0.1:<port>/callback`. Default 8765.
    #[serde(default = "default_redirect_port")]
    pub redirect_port: u16,
}

fn default_token_file() -> String {
    "google_tokens.json".to_string()
}
fn default_redirect_port() -> u16 {
    8765
}

/// Expand short-form scopes (`gmail.readonly`) to full URLs. Leaves
/// already-qualified scopes untouched. Google's own SDKs do the same
/// convenience expansion, so users pasting from tutorials get both.
pub fn canonicalize_scopes(input: &[String]) -> Vec<String> {
    input
        .iter()
        .map(|s| {
            if s.starts_with("https://") || s.starts_with("openid") {
                s.clone()
            } else {
                format!("https://www.googleapis.com/auth/{s}")
            }
        })
        .collect()
}

/// Persisted OAuth state. `refresh_token` is optional because Google
/// only returns it on the FIRST exchange of an auth code with
/// `access_type=offline` — if the user already granted the app, a
/// later re-auth ships back an access_token without a refresh. We
/// then keep using the refresh we already have on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when `access_token` expires.
    pub expires_at: i64,
    pub scopes: Vec<String>,
}

impl GoogleTokens {
    pub fn is_fresh(&self) -> bool {
        Utc::now().timestamp() + 60 < self.expires_at
    }
}

/// Async-safe OAuth client used by all `google_*` tools.
pub struct GoogleAuthClient {
    config: Arc<GoogleAuthConfig>,
    /// Path (absolute) where `token_file` lands. Computed at
    /// construction from `<workspace>/<token_file>`.
    token_path: PathBuf,
    tokens: RwLock<Option<GoogleTokens>>,
    /// Set by `start_auth_flow`; resolved by the loopback listener on
    /// receipt of the redirect. One listener at a time — a second
    /// `start_auth_flow` cancels the previous.
    pending_auth: RwLock<Option<oneshot::Sender<Result<GoogleTokens>>>>,
    http: reqwest::Client,
}

impl GoogleAuthClient {
    pub fn new(config: GoogleAuthConfig, workspace_dir: &std::path::Path) -> Arc<Self> {
        let token_path = if std::path::Path::new(&config.token_file).is_absolute() {
            PathBuf::from(&config.token_file)
        } else {
            workspace_dir.join(&config.token_file)
        };
        Arc::new(Self {
            config: Arc::new(config),
            token_path,
            tokens: RwLock::new(None),
            pending_auth: RwLock::new(None),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        })
    }

    pub fn config(&self) -> &GoogleAuthConfig {
        &self.config
    }

    pub fn token_path(&self) -> &std::path::Path {
        &self.token_path
    }

    /// Load persisted tokens from disk. Missing file → `Ok(())` with
    /// empty in-memory state; the caller's next `ensure_fresh` will
    /// report "not authenticated" instead of blowing up.
    pub async fn load_from_disk(&self) -> Result<()> {
        match tokio::fs::read(&self.token_path).await {
            Ok(bytes) => {
                let t: GoogleTokens = serde_json::from_slice(&bytes)
                    .context("google_tokens.json malformed")?;
                *self.tokens.write().await = Some(t);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn save_to_disk(&self, tokens: &GoogleTokens) -> Result<()> {
        if let Some(parent) = self.token_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let bytes = serde_json::to_vec_pretty(tokens)?;
        tokio::fs::write(&self.token_path, &bytes)
            .await
            .with_context(|| format!("writing {}", self.token_path.display()))?;
        // Tighten perms — refresh_token is bearer-equivalent to a
        // Google password for the scopes it covers.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = tokio::fs::set_permissions(&self.token_path, perms).await;
        }
        Ok(())
    }

    /// Build the URL the user must visit to authorise. We always ask
    /// for `access_type=offline` + `prompt=consent` so the first
    /// approval yields a refresh_token. Subsequent prompts reuse that
    /// refresh — see `GoogleTokens::refresh_token` docstring.
    pub fn build_auth_url(&self, state: &str) -> String {
        let redirect_uri = self.redirect_uri();
        let scopes = canonicalize_scopes(&self.config.scopes).join(" ");
        let params = [
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", &redirect_uri),
            ("response_type", "code"),
            ("scope", &scopes),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", state),
        ];
        let qs = serde_urlencoded::to_string(params).unwrap_or_default();
        format!("https://accounts.google.com/o/oauth2/v2/auth?{qs}")
    }

    fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.config.redirect_port)
    }

    /// Bind the loopback listener AND return the auth URL. The listener
    /// handles exactly one redirect then exits; the resulting tokens
    /// land in `self.tokens` + the on-disk file. Caller typically
    /// forwards `url` to the user via chat.
    ///
    /// Returns `(url, join_handle)` — dropping the handle is OK, the
    /// task is self-contained, but holding it lets the caller `.await`
    /// the final outcome.
    pub async fn start_auth_flow(
        self: &Arc<Self>,
    ) -> Result<(String, tokio::task::JoinHandle<Result<GoogleTokens>>)> {
        let listener = TcpListener::bind(("127.0.0.1", self.config.redirect_port))
            .await
            .with_context(|| {
                format!(
                    "cannot bind 127.0.0.1:{} for OAuth callback — another process using it?",
                    self.config.redirect_port
                )
            })?;
        // Short-lived random state lets the listener verify the redirect
        // belongs to THIS flow (not a stale tab from minutes ago).
        let state = format!("{:016x}", rand_u64());
        let url = self.build_auth_url(&state);

        let (tx, rx) = oneshot::channel::<Result<GoogleTokens>>();
        {
            let mut slot = self.pending_auth.write().await;
            // If there's an older pending flow, drop its sender — the
            // caller gets "Err(channel closed)" when they poll.
            *slot = Some(tx);
        }

        let this = Arc::clone(self);
        let state_owned = state.clone();
        let handle = tokio::spawn(async move {
            let outcome = this.run_loopback_once(listener, &state_owned).await;
            let mut slot = this.pending_auth.write().await;
            if let Some(sender) = slot.take() {
                // Send the outcome both to any awaiter and to our own return.
                let _ = sender.send(match &outcome {
                    Ok(t) => Ok(t.clone()),
                    Err(e) => Err(anyhow!("{e}")),
                });
            }
            drop(rx); // we'll return outcome directly
            outcome
        });

        Ok((url, handle))
    }

    /// Block on a single TCP accept, parse `?code=...&state=...`, and
    /// exchange the code. Writes a small HTML "you can close this tab"
    /// response so the user's browser doesn't hang on a spinner.
    async fn run_loopback_once(
        &self,
        listener: TcpListener,
        expected_state: &str,
    ) -> Result<GoogleTokens> {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("oauth callback accept failed")?;
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let req = std::str::from_utf8(&buf[..n]).unwrap_or("");
        // Parse "GET /callback?code=X&state=Y HTTP/1.1"
        let line = req.lines().next().unwrap_or("");
        let path = line.split_whitespace().nth(1).unwrap_or("");
        let (code, state) = parse_callback_query(path);

        let html_ok = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
            <html><body style='font-family:sans-serif;padding:40px;text-align:center'>\
            <h2>Authorisation received ✓</h2>\
            <p>You can close this tab. Return to the agent.</p>\
            </body></html>";
        let html_err = |msg: &str| {
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n\
                <html><body style='font-family:sans-serif;padding:40px;text-align:center'>\
                <h2>Authorisation failed</h2><p>{msg}</p></body></html>"
            )
        };

        if state.as_deref() != Some(expected_state) {
            let _ = stream.write_all(html_err("state mismatch").as_bytes()).await;
            return Err(anyhow!("oauth state mismatch — possibly a replayed tab"));
        }
        let code = match code {
            Some(c) => c,
            None => {
                let _ = stream.write_all(html_err("no code in callback").as_bytes()).await;
                return Err(anyhow!("oauth callback missing ?code parameter"));
            }
        };

        // Exchange the code for tokens. This is the one step that can
        // fail for legitimate reasons (bad client_secret, network, etc);
        // surface the error both on the tab and to the caller.
        let tokens = match self.exchange_code(&code).await {
            Ok(t) => t,
            Err(e) => {
                let _ = stream
                    .write_all(html_err(&format!("token exchange failed: {e}")).as_bytes())
                    .await;
                return Err(e);
            }
        };

        let _ = stream.write_all(html_ok.as_bytes()).await;
        let _ = stream.shutdown().await;
        Ok(tokens)
    }

    pub async fn exchange_code(&self, code: &str) -> Result<GoogleTokens> {
        let redirect_uri = self.redirect_uri();
        let form = [
            ("code", code),
            ("client_id", &self.config.client_id),
            ("client_secret", &self.config.client_secret),
            ("redirect_uri", &redirect_uri),
            ("grant_type", "authorization_code"),
        ];
        let resp = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&form)
            .send()
            .await
            .context("POST oauth2.googleapis.com/token failed")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("malformed token response")?;
        if !status.is_success() {
            return Err(anyhow!(
                "token exchange HTTP {}: {}",
                status,
                body["error_description"]
                    .as_str()
                    .or_else(|| body["error"].as_str())
                    .unwrap_or("(no error body)")
            ));
        }
        let tokens = tokens_from_response(&body, &canonicalize_scopes(&self.config.scopes))?;
        *self.tokens.write().await = Some(tokens.clone());
        self.save_to_disk(&tokens).await?;
        Ok(tokens)
    }

    /// Request a device code from Google. The returned challenge
    /// contains a short `user_code` the operator types into
    /// `verification_url` on any second device (phone, laptop).
    ///
    /// Client type in Google Cloud Console must be
    /// **"TVs and Limited Input devices"** — Desktop/Web OAuth clients
    /// get `client_type_disabled` here.
    pub async fn request_device_code(&self) -> Result<DeviceChallenge> {
        let scopes_joined = canonicalize_scopes(&self.config.scopes).join(" ");
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("scope", scopes_joined.as_str()),
        ];
        let resp = self
            .http
            .post("https://oauth2.googleapis.com/device/code")
            .form(&form)
            .send()
            .await
            .context("POST oauth2.googleapis.com/device/code failed")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("malformed device/code response")?;
        if !status.is_success() {
            return Err(anyhow!(
                "device/code HTTP {}: {}",
                status,
                body["error_description"]
                    .as_str()
                    .or_else(|| body["error"].as_str())
                    .unwrap_or("(no error body)")
            ));
        }
        Ok(DeviceChallenge {
            device_code: body["device_code"]
                .as_str()
                .ok_or_else(|| anyhow!("missing device_code"))?
                .to_string(),
            user_code: body["user_code"]
                .as_str()
                .ok_or_else(|| anyhow!("missing user_code"))?
                .to_string(),
            verification_url: body["verification_url"]
                .as_str()
                .or_else(|| body["verification_uri"].as_str())
                .unwrap_or("https://www.google.com/device")
                .to_string(),
            expires_in: body["expires_in"].as_u64().unwrap_or(1800),
            interval: body["interval"].as_u64().unwrap_or(5),
        })
    }

    /// Poll `oauth2.googleapis.com/token` with the device_code grant
    /// until the user approves or the code expires. Sleeps
    /// `challenge.interval` seconds between polls and backs off on
    /// `slow_down`. On success writes tokens to disk + in-memory slot
    /// identically to the loopback exchange path.
    pub async fn poll_device_token(&self, challenge: &DeviceChallenge) -> Result<GoogleTokens> {
        let deadline = std::time::Instant::now() + Duration::from_secs(challenge.expires_in);
        let mut interval = Duration::from_secs(challenge.interval);
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(anyhow!("device code expired before user approved"));
            }
            tokio::time::sleep(interval).await;
            let form = [
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("device_code", challenge.device_code.as_str()),
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code",
                ),
            ];
            let resp = self
                .http
                .post("https://oauth2.googleapis.com/token")
                .form(&form)
                .send()
                .await
                .context("POST oauth2.googleapis.com/token (device) failed")?;
            let status = resp.status();
            let body: Value = resp.json().await.context("malformed token response")?;
            if status.is_success() {
                let tokens =
                    tokens_from_response(&body, &canonicalize_scopes(&self.config.scopes))?;
                *self.tokens.write().await = Some(tokens.clone());
                self.save_to_disk(&tokens).await?;
                return Ok(tokens);
            }
            let err = body["error"].as_str().unwrap_or("");
            match err {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval = interval.saturating_add(Duration::from_secs(5));
                    continue;
                }
                "access_denied" => {
                    return Err(anyhow!("user denied the consent request"))
                }
                "expired_token" => return Err(anyhow!("device code expired")),
                other => {
                    return Err(anyhow!(
                        "token poll HTTP {}: {} — {}",
                        status,
                        other,
                        body["error_description"].as_str().unwrap_or("")
                    ))
                }
            }
        }
    }

    /// Renew `access_token` using the on-file `refresh_token`. Called
    /// transparently by `authorized_call` when a cached access is
    /// within 60s of expiring.
    pub async fn refresh_if_needed(&self) -> Result<GoogleTokens> {
        {
            let guard = self.tokens.read().await;
            if let Some(t) = guard.as_ref() {
                if t.is_fresh() {
                    return Ok(t.clone());
                }
            }
        }
        let refresh = {
            let guard = self.tokens.read().await;
            guard
                .as_ref()
                .and_then(|t| t.refresh_token.clone())
                .ok_or_else(|| {
                    anyhow!(
                        "no refresh_token on file — run `google_auth_start` to \
                         authorise the agent for the first time"
                    )
                })?
        };
        let form = [
            ("client_id", self.config.client_id.as_str()),
            ("client_secret", self.config.client_secret.as_str()),
            ("refresh_token", &refresh),
            ("grant_type", "refresh_token"),
        ];
        let resp = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&form)
            .send()
            .await
            .context("POST oauth2.googleapis.com/token (refresh) failed")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("malformed refresh response")?;
        if !status.is_success() {
            // A 400 "invalid_grant" here usually means the user revoked
            // access from their account page, OR the 7-day testing
            // window elapsed on an unpublished OAuth app. Surface the
            // canonical error so the caller can tell the human to
            // re-authorise via `google_auth_start`.
            return Err(anyhow!(
                "refresh HTTP {}: {} — re-auth required",
                status,
                body["error_description"]
                    .as_str()
                    .or_else(|| body["error"].as_str())
                    .unwrap_or("(no error body)")
            ));
        }
        // Google keeps the same refresh unless it rotates (uncommon);
        // merge into the existing stored copy so we don't lose it.
        let mut new_tokens = tokens_from_response(&body, &canonicalize_scopes(&self.config.scopes))?;
        if new_tokens.refresh_token.is_none() {
            new_tokens.refresh_token = Some(refresh);
        }
        *self.tokens.write().await = Some(new_tokens.clone());
        self.save_to_disk(&new_tokens).await?;
        Ok(new_tokens)
    }

    /// Make an authenticated HTTP call. Transparently refreshes the
    /// access_token when stale. Body is arbitrary JSON; pass `None`
    /// for GET/DELETE. Returns parsed JSON so the caller can walk the
    /// Google API response directly.
    pub async fn authorized_call(
        &self,
        method: &str,
        url: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let tokens = self.refresh_if_needed().await?;
        let m = method.to_uppercase();
        let method_enum = match m.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            other => return Err(anyhow!("unsupported HTTP method `{other}`")),
        };
        let mut req = self
            .http
            .request(method_enum, url)
            .bearer_auth(&tokens.access_token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.with_context(|| format!("{m} {url}"))?;
        let status = resp.status();
        let body_val: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => Value::Null,
        };
        if !status.is_success() {
            return Err(anyhow!(
                "{m} {url} → HTTP {}: {}",
                status,
                serde_json::to_string(&body_val).unwrap_or_default()
            ));
        }
        Ok(body_val)
    }

    pub async fn snapshot(&self) -> Value {
        let guard = self.tokens.read().await;
        match guard.as_ref() {
            None => serde_json::json!({
                "authenticated": false,
                "reason": "no tokens on file"
            }),
            Some(t) => {
                let now = Utc::now().timestamp();
                serde_json::json!({
                    "authenticated": true,
                    "fresh": t.is_fresh(),
                    "expires_in_secs": (t.expires_at - now).max(0),
                    "has_refresh": t.refresh_token.is_some(),
                    "scopes": t.scopes,
                })
            }
        }
    }

    /// Revoke the refresh_token at Google and wipe the local state.
    /// Next `google_*` call will 401 until the user re-authorises.
    pub async fn revoke(&self) -> Result<()> {
        let refresh = {
            let guard = self.tokens.read().await;
            guard.as_ref().and_then(|t| t.refresh_token.clone())
        };
        if let Some(r) = refresh {
            let _ = self
                .http
                .post("https://oauth2.googleapis.com/revoke")
                .form(&[("token", &r)])
                .send()
                .await;
        }
        *self.tokens.write().await = None;
        let _ = tokio::fs::remove_file(&self.token_path).await;
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn tokens_from_response(body: &Value, requested_scopes: &[String]) -> Result<GoogleTokens> {
    let access_token = body["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("token response missing access_token"))?
        .to_string();
    let refresh_token = body["refresh_token"].as_str().map(|s| s.to_string());
    let expires_in = body["expires_in"].as_i64().unwrap_or(3600);
    // Google sometimes echoes back the scope string separated by spaces;
    // prefer that over whatever we asked for, so we record what we
    // actually got.
    let scopes = body["scope"]
        .as_str()
        .map(|s| s.split(' ').map(|x| x.to_string()).collect())
        .unwrap_or_else(|| requested_scopes.to_vec());
    Ok(GoogleTokens {
        access_token,
        refresh_token,
        expires_at: Utc::now().timestamp() + expires_in - 30, // 30s buffer
        scopes,
    })
}

fn parse_callback_query(path: &str) -> (Option<String>, Option<String>) {
    let q = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let v = urldecode(v);
            match k {
                "code" => code = Some(v),
                "state" => state = Some(v),
                _ => {}
            }
        }
    }
    (code, state)
}

fn urldecode(s: &str) -> String {
    // Minimal decoder — enough for OAuth callback values (alphanumeric
    // + `.`, `-`, `_`, `%xx`). Full RFC 3986 support would pull in
    // `percent-encoding`; not needed for this path.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(b) = u8::from_str_radix(&hex, 16) {
                    out.push(b as char);
                    continue;
                }
            }
            out.push('%');
            out.push_str(&hex);
        } else if c == '+' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Not cryptographic — state's job is CSRF protection between the
    // URL we just minted and the callback 10s later, both local.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
        ^ (std::process::id() as u64).wrapping_mul(2654435761)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_short_scopes() {
        let out = canonicalize_scopes(&[
            "gmail.readonly".into(),
            "https://www.googleapis.com/auth/drive".into(),
            "openid".into(),
        ]);
        assert_eq!(
            out,
            vec![
                "https://www.googleapis.com/auth/gmail.readonly",
                "https://www.googleapis.com/auth/drive",
                "openid"
            ]
        );
    }

    #[test]
    fn parse_callback_roundtrip() {
        let (code, state) = parse_callback_query(
            "/callback?code=4%2F0AbcdEF&state=abc123&scope=email+profile",
        );
        assert_eq!(code.as_deref(), Some("4/0AbcdEF"));
        assert_eq!(state.as_deref(), Some("abc123"));
    }

    #[test]
    fn tokens_fresh_semantics() {
        let t = GoogleTokens {
            access_token: "x".into(),
            refresh_token: Some("r".into()),
            expires_at: Utc::now().timestamp() + 600,
            scopes: vec![],
        };
        assert!(t.is_fresh());

        let stale = GoogleTokens {
            access_token: "x".into(),
            refresh_token: None,
            expires_at: Utc::now().timestamp() + 10, // < 60s buffer
            scopes: vec![],
        };
        assert!(!stale.is_fresh());
    }

    #[test]
    fn urldecode_handles_percent_and_plus() {
        assert_eq!(urldecode("hello+world"), "hello world");
        assert_eq!(urldecode("hello%20world"), "hello world");
        assert_eq!(urldecode("4%2F0AbcdEF"), "4/0AbcdEF");
    }
}
