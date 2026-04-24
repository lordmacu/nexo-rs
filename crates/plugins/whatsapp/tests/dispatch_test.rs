//! Unit tests for the outbound dispatcher's reactive resolver. Exercises
//! the session_id → oneshot routing decision without needing a real
//! wa-agent `Session` (the send_* branches land in step 6.8 live tests).

use std::sync::Arc;

use agent_broker::Event;
use agent_plugin_whatsapp::dispatch::{self, TOPIC_OUTBOUND};
use agent_plugin_whatsapp::session_id::session_id_for_jid;
use dashmap::DashMap;
use tokio::sync::oneshot;

fn make_event(session_id: Option<uuid::Uuid>, payload: serde_json::Value) -> Event {
    let mut ev = Event::new(TOPIC_OUTBOUND, "test", payload);
    ev.session_id = session_id;
    ev
}

#[tokio::test]
async fn reactive_resolve_delivers_to_pending_sender() {
    let pending: Arc<DashMap<uuid::Uuid, oneshot::Sender<String>>> = Arc::new(DashMap::new());
    let sid = session_id_for_jid("573@s.whatsapp.net");
    let (tx, rx) = oneshot::channel::<String>();
    pending.insert(sid, tx);

    let ev = make_event(
        Some(sid),
        serde_json::json!({ "kind": "text", "to": "573@s.whatsapp.net", "text": "ok" }),
    );
    let outcome = dispatch::__test_try_resolve(&ev, &pending);
    assert!(matches!(
        outcome,
        dispatch::__test::ResolveOutcome::Delivered
    ));
    assert_eq!(rx.await.unwrap(), "ok");
    assert!(pending.is_empty());
}

#[tokio::test]
async fn reactive_skips_when_no_session_id() {
    let pending: Arc<DashMap<uuid::Uuid, oneshot::Sender<String>>> = Arc::new(DashMap::new());
    let ev = make_event(
        None,
        serde_json::json!({ "kind": "text", "to": "x", "text": "hi" }),
    );
    let outcome = dispatch::__test_try_resolve(&ev, &pending);
    assert!(matches!(
        outcome,
        dispatch::__test::ResolveOutcome::NotReactive
    ));
}

#[tokio::test]
async fn reactive_skips_when_no_pending_sender() {
    let pending: Arc<DashMap<uuid::Uuid, oneshot::Sender<String>>> = Arc::new(DashMap::new());
    let sid = session_id_for_jid("573@s.whatsapp.net");
    let ev = make_event(
        Some(sid),
        serde_json::json!({ "kind": "text", "to": "x", "text": "hi" }),
    );
    let outcome = dispatch::__test_try_resolve(&ev, &pending);
    assert!(matches!(
        outcome,
        dispatch::__test::ResolveOutcome::NoPending
    ));
}

#[tokio::test]
async fn non_text_kinds_skip_reactive_path() {
    let pending: Arc<DashMap<uuid::Uuid, oneshot::Sender<String>>> = Arc::new(DashMap::new());
    let sid = session_id_for_jid("573@s.whatsapp.net");
    let (tx, _rx) = oneshot::channel::<String>();
    pending.insert(sid, tx);
    let ev = make_event(
        Some(sid),
        serde_json::json!({ "kind": "react", "to": "x", "msg_id": "M1", "emoji": "👍" }),
    );
    let outcome = dispatch::__test_try_resolve(&ev, &pending);
    assert!(matches!(
        outcome,
        dispatch::__test::ResolveOutcome::NotReactive
    ));
    // Sender stays in the map — only the send_* side consumes it.
    assert_eq!(pending.len(), 1);
}
