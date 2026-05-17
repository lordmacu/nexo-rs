//! End-to-end round-trip tests for Phase 81.33.b.real
//! auto-discovery contracts.
//!
//! Each capability's daemon-side helper (in `nexo-pairing`) is
//! paired with the reference plugin's handler (in
//! `nexo-reference-plugin`) over a `LocalBroker`. Tests prove
//! the JSON wire shape works end-to-end — not just that each
//! side compiles in isolation.
//!
//! Coverage matrix:
//!
//! | Stage | Daemon helper                         | Plugin handler                |
//! |-------|---------------------------------------|-------------------------------|
//! | 1     | `GenericBrokerPairingAdapter`         | `pairing::normalize_sender`   |
//! |       |                                       | `pairing::send_reply`         |
//! |       |                                       | `pairing::send_qr_image`      |
//! | 2     | `plugin_http::forward_request`        | `http::handle_request`        |
//! | 4     | `plugin_admin::forward_request`       | `admin::handle`               |
//! | 5     | `plugin_metrics::scrape_all`          | `metrics::scrape`             |

use std::sync::Arc;
use std::time::Duration;

use nexo_broker::{AnyBroker, BrokerHandle, Event, LocalBroker, Message};
use nexo_pairing::adapter::PairingChannelAdapter;
use nexo_pairing::generic_adapter::GenericBrokerPairingAdapter;
use nexo_pairing::plugin_admin::{forward_request as forward_admin, PluginAdminRouter};
use nexo_pairing::plugin_http::{forward_request as forward_http, PluginHttpRouter};
use nexo_pairing::plugin_metrics::{scrape_all, PluginMetricsDescriptor};
use nexo_plugin_manifest::pairing::PairingAdapterSection;
use serde_json::{json, Value};
use tokio::sync::Mutex;

const TOPIC_PREFIX: &str = "plugin.reference_demo";
const ADMIN_TOPIC_PREFIX: &str = "plugin.reference_demo.admin";

/// Subscribe to a topic + reply to every incoming request using
/// `handler`. Spawns a background tokio task; cancellation is by
/// dropping the broker (test scope).
async fn spawn_reply_handler<F>(
    broker: AnyBroker,
    topic: &str,
    handler: F,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(&Value) -> Value + Send + Sync + 'static,
{
    let topic_owned = topic.to_string();
    let mut sub = broker.subscribe(topic).await.expect("subscribe");
    let broker_clone = broker.clone();
    tokio::spawn(async move {
        loop {
            let next = sub.next().await;
            let Some(event) = next else {
                tracing::debug!(topic = %topic_owned, "subscriber stream ended");
                break;
            };
            let Ok(msg) = serde_json::from_value::<Message>(event.payload) else {
                continue;
            };
            let Some(reply_to) = msg.reply_to.clone() else {
                continue;
            };
            let reply_payload = handler(&msg.payload);
            // The broker `request` deserializes the reply event's
            // payload as a `Message`; we wrap our handler output
            // accordingly.
            let reply_msg = Message::new(reply_to.clone(), reply_payload);
            let reply_event = Event::new(
                reply_to.clone(),
                "reference_plugin",
                serde_json::to_value(&reply_msg).expect("serialize reply"),
            );
            broker_clone
                .publish(&reply_to, reply_event)
                .await
                .expect("publish reply");
        }
    })
}

// ── Stage 1 — pairing adapter ──────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_adapter_normalize_sender_round_trip() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{TOPIC_PREFIX}.pairing.normalize_sender"),
        |req| nexo_reference_plugin::pairing::normalize_sender(req),
    )
    .await;
    let adapter = GenericBrokerPairingAdapter::from_manifest(
        broker.clone(),
        &PairingAdapterSection {
            channel_id: "reference_demo".into(),
            broker_topic_prefix: TOPIC_PREFIX.into(),
            format_challenge_text_kind: None,
            normalize_cache_ttl_seconds: None,
        },
    );
    let task = tokio::task::spawn_blocking(move || adapter.normalize_sender("User.A@demo.local"));
    let normalized = task.await.expect("blocking task");
    assert_eq!(normalized.as_deref(), Some("user.a"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_adapter_send_reply_round_trip() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{TOPIC_PREFIX}.pairing.send_reply"),
        |req| nexo_reference_plugin::pairing::send_reply(req),
    )
    .await;
    let adapter = GenericBrokerPairingAdapter::from_manifest(
        broker.clone(),
        &PairingAdapterSection {
            channel_id: "reference_demo".into(),
            broker_topic_prefix: TOPIC_PREFIX.into(),
            format_challenge_text_kind: None,
            normalize_cache_ttl_seconds: None,
        },
    );
    let result = adapter.send_reply("default", "user.a", "code: 9999").await;
    assert!(result.is_ok(), "send_reply round-trip: {result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_adapter_send_qr_image_round_trip() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{TOPIC_PREFIX}.pairing.send_qr_image"),
        |req| nexo_reference_plugin::pairing::send_qr_image(req),
    )
    .await;
    let adapter = GenericBrokerPairingAdapter::from_manifest(
        broker.clone(),
        &PairingAdapterSection {
            channel_id: "reference_demo".into(),
            broker_topic_prefix: TOPIC_PREFIX.into(),
            format_challenge_text_kind: None,
            normalize_cache_ttl_seconds: None,
        },
    );
    let png = b"PNG-bytes-fixture";
    let result = adapter.send_qr_image("default", "user.a", png).await;
    assert!(result.is_ok(), "send_qr_image round-trip: {result:?}");
}

// ── Stage 2 — HTTP routes ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_proxy_get_hello_round_trip() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        "plugin.reference_demo.http.request",
        |req| nexo_reference_plugin::http::handle_request(req),
    )
    .await;
    let mut router = PluginHttpRouter::new();
    router
        .register("reference_demo", "/reference_demo", None)
        .expect("register");
    let info = router.match_path("/reference_demo/hello").expect("match");
    let reply = forward_http(
        &broker,
        info.0,
        "GET",
        "/reference_demo/hello",
        "",
        &[],
        &[],
        info.1,
    )
    .await
    .expect("forward_http");
    assert_eq!(reply.status, 200);
    let body = reply.decoded_body();
    assert!(String::from_utf8_lossy(&body).contains("hello from reference_demo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_proxy_post_echo_round_trips_body() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        "plugin.reference_demo.http.request",
        |req| nexo_reference_plugin::http::handle_request(req),
    )
    .await;
    let mut router = PluginHttpRouter::new();
    router
        .register("reference_demo", "/reference_demo", None)
        .expect("register");
    let info = router.match_path("/reference_demo/echo").expect("match");
    let body_in = b"hello-binary-body";
    let reply = forward_http(
        &broker,
        info.0,
        "POST",
        "/reference_demo/echo",
        "",
        &[],
        body_in,
        info.1,
    )
    .await
    .expect("forward_http");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.decoded_body(), body_in);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_proxy_unknown_path_returns_plugin_404() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        "plugin.reference_demo.http.request",
        |req| nexo_reference_plugin::http::handle_request(req),
    )
    .await;
    let mut router = PluginHttpRouter::new();
    router
        .register("reference_demo", "/reference_demo", None)
        .expect("register");
    let info = router.match_path("/reference_demo/missing").expect("match");
    let reply = forward_http(
        &broker,
        info.0,
        "GET",
        "/reference_demo/missing",
        "",
        &[],
        &[],
        info.1,
    )
    .await
    .expect("forward_http");
    assert_eq!(reply.status, 404);
}

// ── Stage 4 — admin RPC ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_list_round_trip_returns_known_instances() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{ADMIN_TOPIC_PREFIX}.list"),
        |req| nexo_reference_plugin::admin::handle(req),
    )
    .await;
    let router = PluginAdminRouter::new();
    router
        .register(
            "reference_demo",
            "nexo/admin/reference_demo/",
            ADMIN_TOPIC_PREFIX,
            None,
        )
        .expect("register");
    let info = router
        .match_method("nexo/admin/reference_demo/list")
        .expect("match");
    let reply = forward_admin(&broker, info, "nexo/admin/reference_demo/list", json!({}))
        .await
        .expect("forward_admin");
    assert!(reply.ok);
    let items = reply
        .result
        .get("items")
        .and_then(|v| v.as_array())
        .expect("items");
    assert_eq!(items.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_ping_round_trip_echoes_params() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{ADMIN_TOPIC_PREFIX}.ping"),
        |req| nexo_reference_plugin::admin::handle(req),
    )
    .await;
    let router = PluginAdminRouter::new();
    router
        .register(
            "reference_demo",
            "nexo/admin/reference_demo/",
            ADMIN_TOPIC_PREFIX,
            None,
        )
        .expect("register");
    let info = router
        .match_method("nexo/admin/reference_demo/ping")
        .expect("match");
    let reply = forward_admin(
        &broker,
        info,
        "nexo/admin/reference_demo/ping",
        json!({ "msg": "hi" }),
    )
    .await
    .expect("forward_admin");
    assert!(reply.ok);
    assert_eq!(
        reply
            .result
            .get("echo")
            .and_then(|v| v.get("msg"))
            .and_then(|v| v.as_str()),
        Some("hi"),
    );
}

// ── Stage 5 — metrics scrape ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_scrape_round_trip_returns_prometheus_text() {
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{TOPIC_PREFIX}.metrics.scrape"),
        |req| nexo_reference_plugin::metrics::scrape(req),
    )
    .await;
    let descriptors = vec![PluginMetricsDescriptor::new("reference_demo", TOPIC_PREFIX)
        .with_timeout(Duration::from_secs(2))];
    let out = scrape_all(&broker, &descriptors).await;
    assert!(
        out.contains("reference_demo_handler_calls_total"),
        "expected reference_demo metrics in output, got: {out}",
    );
    assert!(out.contains("reference_demo_ready 1"));
}

// ── Failure isolation across mixed plugins ─────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_scrape_isolates_one_failing_plugin() {
    let broker = AnyBroker::Local(LocalBroker::new());
    // Healthy reference_demo subscriber.
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{TOPIC_PREFIX}.metrics.scrape"),
        |req| nexo_reference_plugin::metrics::scrape(req),
    )
    .await;
    // Unhealthy "broken_plugin" — no subscriber for its topic.
    let descriptors = vec![
        PluginMetricsDescriptor::new("broken_plugin", "plugin.broken_plugin")
            .with_timeout(Duration::from_millis(100)),
        PluginMetricsDescriptor::new("reference_demo", TOPIC_PREFIX)
            .with_timeout(Duration::from_secs(2)),
    ];
    let out = scrape_all(&broker, &descriptors).await;
    // Healthy plugin still contributes; broken one warn-logs +
    // empty.
    assert!(out.contains("reference_demo_ready 1"));
    assert!(!out.contains("broken_plugin"));
}

// ── Cache invariant ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_normalize_cache_avoids_second_round_trip() {
    // Counter incremented on every plugin-side handler call.
    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = counter.clone();
    let broker = AnyBroker::Local(LocalBroker::new());
    let _h = spawn_reply_handler(
        broker.clone(),
        &format!("{TOPIC_PREFIX}.pairing.normalize_sender"),
        move |req| {
            let counter = counter_clone.clone();
            // Use blocking-mutex-style atomic increment.
            tokio::task::block_in_place(|| {
                let mut g = tokio::runtime::Handle::current().block_on(counter.lock());
                *g += 1;
            });
            nexo_reference_plugin::pairing::normalize_sender(req)
        },
    )
    .await;
    let adapter = GenericBrokerPairingAdapter::from_manifest(
        broker.clone(),
        &PairingAdapterSection {
            channel_id: "reference_demo".into(),
            broker_topic_prefix: TOPIC_PREFIX.into(),
            format_challenge_text_kind: None,
            normalize_cache_ttl_seconds: None,
        },
    );
    let adapter = Arc::new(adapter);
    let a1 = adapter.clone();
    let _ = tokio::task::spawn_blocking(move || a1.normalize_sender("user.a@demo.local"))
        .await
        .unwrap();
    let a2 = adapter.clone();
    let _ = tokio::task::spawn_blocking(move || a2.normalize_sender("user.a@demo.local"))
        .await
        .unwrap();
    let calls = *counter.lock().await;
    assert_eq!(calls, 1, "second call should hit the cache, not the broker");
}
