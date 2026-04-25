//! Phase 26.y — integration tests asserting that pairing lifecycle
//! events bump the right Prometheus counters.
//!
//! Counters are process-global `LazyLock<DashMap<...>>`, so these
//! tests serialize via a `Mutex` and call `reset_for_test()` at the
//! top. Same pattern as `nexo_pairing::telemetry::tests`.

use nexo_pairing::{telemetry, PairingError, PairingStore, SetupCodeIssuer};
use serial_test::serial;

#[tokio::test]
#[serial(pairing_telemetry)]
async fn upsert_pending_inc_gauge_per_channel() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    store
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    store
        .upsert_pending("tg", "p", "u1", serde_json::json!({}))
        .await
        .unwrap();
    store
        .upsert_pending("tg", "p", "u2", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(telemetry::requests_pending("wa"), 1);
    assert_eq!(telemetry::requests_pending("tg"), 2);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn upsert_pending_existing_does_not_double_inc() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    store
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    store
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    // Same sender — the second call returns the existing code without
    // an INSERT, so the gauge must not double-count.
    assert_eq!(telemetry::requests_pending("wa"), 1);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn approve_ok_bumps_approvals_and_dec_gauge() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    let out = store
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    store.approve(&out.code).await.unwrap();
    assert_eq!(telemetry::approvals_total("wa", "ok"), 1);
    assert_eq!(telemetry::requests_pending("wa"), 0);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn approve_not_found_bumps_with_empty_channel() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    let err = store.approve("ZZZ999").await.unwrap_err();
    assert!(matches!(err, PairingError::UnknownCode));
    assert_eq!(telemetry::approvals_total("", "not_found"), 1);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn purge_expired_bumps_codes_expired_and_dec_gauge_per_channel() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    // Insert two rows each on two channels.
    store
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    store
        .upsert_pending("wa", "p", "+58", serde_json::json!({}))
        .await
        .unwrap();
    store
        .upsert_pending("tg", "p", "u1", serde_json::json!({}))
        .await
        .unwrap();
    // Backdate every row past the 60-min TTL.
    let ancient = chrono::Utc::now().timestamp() - 60 * 60 * 24;
    sqlx::query("UPDATE pairing_pending SET created_at = ?")
        .bind(ancient)
        .execute(store.pool_for_test())
        .await
        .unwrap();
    let n = store.purge_expired().await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(telemetry::codes_expired_total(), 3);
    assert_eq!(telemetry::requests_pending("wa"), 0);
    assert_eq!(telemetry::requests_pending("tg"), 0);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn bootstrap_issue_bumps_per_profile() {
    telemetry::reset_for_test();
    let dir = tempfile::tempdir().unwrap();
    let issuer = SetupCodeIssuer::open_or_create(&dir.path().join("secret")).unwrap();
    issuer
        .issue(
            "ws://x",
            "default",
            std::time::Duration::from_secs(60),
            None,
        )
        .unwrap();
    issuer
        .issue(
            "ws://x",
            "staging",
            std::time::Duration::from_secs(60),
            None,
        )
        .unwrap();
    issuer
        .issue(
            "ws://x",
            "default",
            std::time::Duration::from_secs(60),
            None,
        )
        .unwrap();
    assert_eq!(telemetry::bootstrap_tokens_issued_total("default"), 2);
    assert_eq!(telemetry::bootstrap_tokens_issued_total("staging"), 1);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn refresh_pending_gauge_resyncs_from_db() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    store
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    store
        .upsert_pending("wa", "p", "+58", serde_json::json!({}))
        .await
        .unwrap();
    // Simulate daemon restart — gauge memory wiped, DB intact.
    telemetry::reset_for_test();
    assert_eq!(telemetry::requests_pending("wa"), 0);
    store.refresh_pending_gauge().await.unwrap();
    assert_eq!(telemetry::requests_pending("wa"), 2);
}

#[tokio::test]
#[serial(pairing_telemetry)]
async fn refresh_pending_gauge_zeros_ghost_channels() {
    telemetry::reset_for_test();
    let store = PairingStore::open_memory().await.unwrap();
    // Stale gauge state for a channel with no DB rows.
    telemetry::set_requests_pending("wa", 4);
    store.refresh_pending_gauge().await.unwrap();
    assert_eq!(telemetry::requests_pending("wa"), 0);
}
