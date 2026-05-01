//! Lifecycle: bind a TCP listener, mount the axum router, run
//! `axum::serve` with `with_graceful_shutdown` listening on the
//! provided cancellation token.
//!
//! Pattern adapted from `crates/mcp/src/server/http_transport.rs`
//! (already shipped) so the runtime keeps a single mental model
//! for axum-based listeners.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio_util::sync::CancellationToken;

use crate::router::RouterState;

pub struct WebhookServerHandle {
    pub bind_addr: SocketAddr,
    pub router_state: Arc<RouterState>,
    pub join: tokio::task::JoinHandle<()>,
}

/// Bind, mount, serve. Returns a handle the caller can hold for
/// the daemon's lifetime; cancelling the token triggers graceful
/// shutdown.
pub async fn spawn_server(
    bind: SocketAddr,
    router: Router,
    router_state: Arc<RouterState>,
    cancel: CancellationToken,
) -> std::io::Result<WebhookServerHandle> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let bind_addr = listener.local_addr()?;
    tracing::info!(addr = %bind_addr, "webhook receiver listening");

    let app = router
        .into_make_service_with_connect_info::<SocketAddr>();

    let cancel_for_shutdown = cancel.clone();
    let join = tokio::spawn(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancel_for_shutdown.cancelled().await;
            })
            .await;
        if let Err(e) = result {
            tracing::error!(error = %e, "webhook server stopped with error");
        } else {
            tracing::info!("webhook server stopped");
        }
    });

    Ok(WebhookServerHandle {
        bind_addr,
        router_state,
        join,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::build_router;
    use nexo_config::types::webhook_receiver::{WebhookServerConfig, WebhookSourceWithLimits};
    use nexo_webhook_receiver::{
        EventKindSource, RecordingWebhookDispatcher, SignatureAlgorithm, SignatureSpec,
        WebhookSourceConfig,
    };

    fn mk_cfg(secret_env: &str) -> WebhookServerConfig {
        WebhookServerConfig {
            enabled: true,
            sources: vec![WebhookSourceWithLimits {
                source: WebhookSourceConfig {
                    id: "ci".into(),
                    path: "/hooks/ci".into(),
                    signature: SignatureSpec {
                        algorithm: SignatureAlgorithm::HmacSha256,
                        header: "X-Sig".into(),
                        prefix: "sha256=".into(),
                        secret_env: secret_env.into(),
                    },
                    publish_to: "webhook.ci.${event_kind}".into(),
                    event_kind_from: EventKindSource::Header {
                        name: "X-Event".into(),
                    },
                    body_cap_bytes: None,
                },
                rate_limit: None,
                concurrency_cap: None,
            }],
            ..Default::default()
        }
    }

    fn hmac_sha256_hex(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_bind_happy_path_via_reqwest() {
        std::env::set_var("WEBHOOK_TEST_SECRET_LIFE1", "supersecret");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_LIFE1");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, state) = build_router(&cfg, dispatcher.clone()).unwrap();

        let cancel = CancellationToken::new();
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let handle = spawn_server(bind, router, state, cancel.clone())
            .await
            .expect("bind");
        let addr = handle.bind_addr;

        let body = serde_json::to_vec(&serde_json::json!({"build":"green"})).unwrap();
        let sig = format!("sha256={}", hmac_sha256_hex("supersecret", &body));
        let url = format!("http://{addr}/hooks/ci");
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("X-Sig", sig)
            .header("X-Event", "ci_finished")
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .expect("post");
        assert_eq!(resp.status().as_u16(), 204);

        // Dispatch happened.
        let captured = dispatcher.captured().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "webhook.ci.ci_finished");

        // Cancel triggers graceful shutdown.
        cancel.cancel();
        // Give axum a moment to drain.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle.join).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_token_shuts_down_listener() {
        std::env::set_var("WEBHOOK_TEST_SECRET_LIFE2", "x");
        let cfg = mk_cfg("WEBHOOK_TEST_SECRET_LIFE2");
        let dispatcher = RecordingWebhookDispatcher::new();
        let (router, state) = build_router(&cfg, dispatcher).unwrap();

        let cancel = CancellationToken::new();
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let handle = spawn_server(bind, router, state, cancel.clone())
            .await
            .expect("bind");

        cancel.cancel();
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), handle.join).await;
        assert!(res.is_ok(), "server task should join within 2s of cancel");
    }
}
