//! Item 2: hot-reload of the resolver state. The same
//! `Arc<AgentCredentialResolver>` handed to plugins must observe new
//! bindings after a `rebuild()` call without being re-allocated, and
//! lookups during the swap must not panic.

use std::collections::HashMap;
use std::sync::Arc;

use nexo_auth::handle::{CredentialHandle, WHATSAPP};
use nexo_auth::resolver::{
    AgentCredentialResolver, AgentCredentialsInput, CredentialStores, StrictLevel,
};
use nexo_auth::telegram::TelegramCredentialStore;
use nexo_auth::whatsapp::{WhatsappAccount, WhatsappCredentialStore};

fn wa(instance: &str, agents: &[&str]) -> WhatsappAccount {
    WhatsappAccount {
        instance: instance.into(),
        session_dir: format!("/tmp/wa-{instance}").into(),
        media_dir: format!("/tmp/wa-{instance}/media").into(),
        allow_agents: agents.iter().map(|s| s.to_string()).collect(),
    }
}

fn input(agent: &str, account: &str) -> AgentCredentialsInput {
    let mut outbound = HashMap::new();
    outbound.insert(WHATSAPP, account.to_string());
    AgentCredentialsInput {
        agent_id: agent.into(),
        outbound,
        inbound: HashMap::new(),
        asymmetric_allowed: HashMap::new(),
    }
}

#[test]
fn rebuild_swaps_bindings_in_place() {
    let stores = CredentialStores {
        whatsapp: Arc::new(WhatsappCredentialStore::new(vec![
            wa("personal", &["ana"]),
            wa("work", &["kate"]),
        ])),
        telegram: Arc::new(TelegramCredentialStore::empty()),
        google: Arc::new(nexo_auth::google::GoogleCredentialStore::empty()),
    };

    let resolver = Arc::new(
        AgentCredentialResolver::build(
            &[input("ana", "personal")],
            &stores,
            StrictLevel::Strict,
        )
        .unwrap(),
    );
    assert_eq!(resolver.version(), 1);
    assert_eq!(
        resolver.resolve("ana", WHATSAPP).unwrap().account_id_raw(),
        "personal"
    );
    assert!(resolver.resolve("kate", WHATSAPP).is_err());

    // Imagine the operator edited agents.d/kate.yaml and reloaded.
    resolver
        .rebuild(
            &[input("ana", "personal"), input("kate", "work")],
            &stores,
            StrictLevel::Strict,
        )
        .unwrap();
    assert_eq!(resolver.version(), 2);

    // Same Arc, new state.
    assert_eq!(
        resolver.resolve("ana", WHATSAPP).unwrap().account_id_raw(),
        "personal"
    );
    assert_eq!(
        resolver.resolve("kate", WHATSAPP).unwrap().account_id_raw(),
        "work"
    );
}

#[test]
fn rebuild_failure_leaves_old_state_intact() {
    let stores = CredentialStores {
        whatsapp: Arc::new(WhatsappCredentialStore::new(vec![wa("personal", &["ana"])])),
        telegram: Arc::new(TelegramCredentialStore::empty()),
        google: Arc::new(nexo_auth::google::GoogleCredentialStore::empty()),
    };
    let resolver = AgentCredentialResolver::build(
        &[input("ana", "personal")],
        &stores,
        StrictLevel::Strict,
    )
    .unwrap();
    let before_version = resolver.version();

    // Attempt to bind kate to a non-existent instance — must error
    // and the resolver state must remain ana → personal.
    let res = resolver.rebuild(
        &[input("ana", "personal"), input("kate", "nonexistent")],
        &stores,
        StrictLevel::Strict,
    );
    assert!(res.is_err());
    assert_eq!(resolver.version(), before_version);
    assert!(resolver.resolve("kate", WHATSAPP).is_err());
    assert_eq!(
        resolver.resolve("ana", WHATSAPP).unwrap().account_id_raw(),
        "personal"
    );
}

#[test]
fn handles_issued_before_reload_keep_working() {
    let stores = CredentialStores {
        whatsapp: Arc::new(WhatsappCredentialStore::new(vec![wa("personal", &["ana"])])),
        telegram: Arc::new(TelegramCredentialStore::empty()),
        google: Arc::new(nexo_auth::google::GoogleCredentialStore::empty()),
    };
    let resolver = AgentCredentialResolver::build(
        &[input("ana", "personal")],
        &stores,
        StrictLevel::Strict,
    )
    .unwrap();
    let in_flight: CredentialHandle = resolver.resolve("ana", WHATSAPP).unwrap();

    // Reload with the same bindings — the in-flight handle must
    // continue to identify the same account.
    resolver
        .rebuild(&[input("ana", "personal")], &stores, StrictLevel::Strict)
        .unwrap();
    assert_eq!(in_flight.account_id_raw(), "personal");
    assert_eq!(in_flight.fingerprint(), resolver.resolve("ana", WHATSAPP).unwrap().fingerprint());
}
