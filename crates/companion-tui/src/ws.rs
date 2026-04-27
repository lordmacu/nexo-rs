//! Client-side WebSocket handshake for the Nexo companion pairing protocol.
//!
//! After `nexo pair start` issues a setup-code, the companion connects to
//! `payload.url`, sends the bootstrap token, and receives a session token
//! that is persisted at `<sessions_dir>/<device_label>.token` (mode 0600).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use nexo_pairing::SetupCode;
use tokio_tungstenite::tungstenite::Message;

#[allow(dead_code)]
pub struct HandshakeOutcome {
    pub session_token: String,
    /// Absolute path where the session token was persisted.
    pub token_path: PathBuf,
}

/// Perform the WS handshake against `payload.url`.
///
/// `device_label` is used only to name the persisted token file.
/// `sessions_dir` must be writable; it is created if absent.
pub async fn perform_handshake(
    payload: &SetupCode,
    device_label: &str,
    sessions_dir: &Path,
) -> Result<HandshakeOutcome> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(&payload.url)
        .await
        .with_context(|| format!("open WebSocket to {}", payload.url))?;
    let (mut tx, mut rx) = ws_stream.split();

    let req = serde_json::json!({ "bootstrap_token": payload.bootstrap_token }).to_string();
    tx.send(Message::Text(req))
        .await
        .context("send bootstrap token")?;

    let msg = match rx.next().await {
        Some(Ok(m)) => m,
        Some(Err(e)) => return Err(anyhow!("WS error waiting for session token: {e}")),
        None => {
            return Err(anyhow!(
                "server closed connection without sending session token"
            ))
        }
    };

    let text = match msg {
        Message::Text(t) => t,
        _ => return Err(anyhow!("expected text frame from server")),
    };

    let parsed: serde_json::Value = serde_json::from_str(&text).context("parse server response")?;

    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        return Err(anyhow!("server rejected bootstrap token: {err}"));
    }

    let session_token = parsed
        .get("session_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("server response missing session_token field"))?
        .to_string();

    // Persist session token atomically with restricted permissions.
    std::fs::create_dir_all(sessions_dir).context("create sessions directory")?;
    let file_name = sanitize_label(device_label);
    let token_path = sessions_dir.join(format!("{file_name}.token"));
    let tmp_path = token_path.with_extension("tmp");

    std::fs::write(&tmp_path, &session_token).context("write session token (tmp)")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp_path)
            .context("stat tmp token file")?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&tmp_path, perms).context("chmod session token")?;
    }
    std::fs::rename(&tmp_path, &token_path).context("rename session token into place")?;

    Ok(HandshakeOutcome {
        session_token,
        token_path,
    })
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_label;

    #[test]
    fn sanitize_alphanumeric() {
        assert_eq!(sanitize_label("myphone-1"), "myphone-1");
    }

    #[test]
    fn sanitize_replaces_special() {
        assert_eq!(sanitize_label("my phone!"), "my_phone_");
    }

    #[test]
    fn sanitize_empty() {
        assert_eq!(sanitize_label(""), "");
    }
}
