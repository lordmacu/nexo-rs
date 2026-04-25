use nexo_pairing::{Decision, PairingError, PairingStore};

#[tokio::test]
async fn upsert_returns_existing_code_for_same_sender() {
    let s = PairingStore::open_memory().await.unwrap();
    let a = s
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    let b = s
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(a.code, b.code);
    assert!(a.created);
    assert!(!b.created);
}

#[tokio::test]
async fn max_pending_per_account_enforced() {
    let s = PairingStore::open_memory().await.unwrap();
    for i in 0..3 {
        s.upsert_pending("wa", "p", &format!("+5710{i}"), serde_json::json!({}))
            .await
            .unwrap();
    }
    let err = s
        .upsert_pending("wa", "p", "+57104", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, PairingError::MaxPending { .. }));
}

#[tokio::test]
async fn approve_moves_to_allow_from() {
    let s = PairingStore::open_memory().await.unwrap();
    let out = s
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    let approved = s.approve(&out.code).await.unwrap();
    assert_eq!(approved.sender_id, "+57");
    assert!(s.is_allowed("wa", "p", "+57").await.unwrap());
    // Pending row is gone after approve.
    let pending = s.list_pending(None).await.unwrap();
    assert!(pending.is_empty());
}

#[tokio::test]
async fn revoke_is_soft_delete() {
    let s = PairingStore::open_memory().await.unwrap();
    s.seed("wa", "p", &["+57".into()]).await.unwrap();
    assert!(s.is_allowed("wa", "p", "+57").await.unwrap());
    let did = s.revoke("wa", "+57").await.unwrap();
    assert!(did);
    assert!(!s.is_allowed("wa", "p", "+57").await.unwrap());
}

#[tokio::test]
async fn seed_is_idempotent_and_reactivates_revoked() {
    let s = PairingStore::open_memory().await.unwrap();
    s.seed("wa", "p", &["+57".into(), "+58".into()])
        .await
        .unwrap();
    let n = s.seed("wa", "p", &["+57".into()]).await.unwrap();
    assert!(n >= 1, "seed should still ack the upsert");
    // Revoke + re-seed reactivates.
    s.revoke("wa", "+57").await.unwrap();
    assert!(!s.is_allowed("wa", "p", "+57").await.unwrap());
    s.seed("wa", "p", &["+57".into()]).await.unwrap();
    assert!(s.is_allowed("wa", "p", "+57").await.unwrap());
}

#[tokio::test]
async fn approve_unknown_code_errors() {
    let s = PairingStore::open_memory().await.unwrap();
    let err = s.approve("NONEXIST").await.unwrap_err();
    assert!(matches!(err, PairingError::UnknownCode));
}

#[tokio::test]
async fn list_pending_filters_by_channel() {
    let s = PairingStore::open_memory().await.unwrap();
    s.upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    s.upsert_pending("tg", "p", "@user", serde_json::json!({}))
        .await
        .unwrap();
    let wa = s.list_pending(Some("wa")).await.unwrap();
    let all = s.list_pending(None).await.unwrap();
    assert_eq!(wa.len(), 1);
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn full_decision_admit_after_approve() {
    // Smoke: combine store + the decision states the gate uses.
    let s = PairingStore::open_memory().await.unwrap();
    let upsert = s
        .upsert_pending("wa", "p", "+57", serde_json::json!({}))
        .await
        .unwrap();
    let _approved = s.approve(&upsert.code).await.unwrap();
    assert!(s.is_allowed("wa", "p", "+57").await.unwrap());
    // Sanity: Decision enum compiles into the public surface.
    let _: Decision = Decision::Admit;
}
