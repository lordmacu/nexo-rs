//! Step 19 E2E (scoped): assert that a config with two agents + two
//! accounts per channel cannot cross-send — i.e. Kate cannot resolve
//! Ana's handle even if her declared binding names Ana's instance.
//! The resolver catches this at boot with
//! [`BuildError::AllowAgentsExcludes`] because Ana's instance lists
//! `allow_agents: [ana]` and Kate is not on the list.

use nexo_auth::error::BuildError;
use nexo_auth::handle::{TELEGRAM, WHATSAPP};
use nexo_auth::resolver::{
    AgentCredentialResolver, AgentCredentialsInput, CredentialStores, StrictLevel,
};
use nexo_auth::telegram::{TelegramAccount, TelegramCredentialStore};
use nexo_auth::whatsapp::{WhatsappAccount, WhatsappCredentialStore};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn wa(instance: &str, agent: &str) -> WhatsappAccount {
    WhatsappAccount {
        instance: instance.into(),
        session_dir: PathBuf::from(format!("/tmp/wa-{instance}")),
        media_dir: PathBuf::from(format!("/tmp/wa-{instance}/media")),
        allow_agents: vec![agent.to_string()],
    }
}

fn tg(instance: &str, agent: &str) -> TelegramAccount {
    TelegramAccount {
        instance: instance.into(),
        token: "t".into(),
        allow_agents: vec![agent.to_string()],
        allowed_chat_ids: vec![],
    }
}

fn input(agent_id: &str, wa_account: &str, tg_account: &str) -> AgentCredentialsInput {
    let mut outbound = HashMap::new();
    outbound.insert(WHATSAPP, wa_account.to_string());
    outbound.insert(TELEGRAM, tg_account.to_string());
    AgentCredentialsInput {
        agent_id: agent_id.into(),
        outbound,
        inbound: HashMap::new(),
        asymmetric_allowed: HashMap::new(),
    }
}

fn stores_of(
    wa_accounts: Vec<WhatsappAccount>,
    tg_accounts: Vec<TelegramAccount>,
) -> CredentialStores {
    CredentialStores {
        whatsapp: Arc::new(WhatsappCredentialStore::new(wa_accounts)),
        telegram: Arc::new(TelegramCredentialStore::new(tg_accounts)),
        google: Arc::new(nexo_auth::google::GoogleCredentialStore::empty()),
    }
}

#[test]
fn two_agents_each_bound_to_own_channels() {
    let s = stores_of(
        vec![wa("personal", "ana"), wa("work", "kate")],
        vec![tg("ana_bot", "ana"), tg("kate_bot", "kate")],
    );
    let inputs = vec![
        input("ana", "personal", "ana_bot"),
        input("kate", "work", "kate_bot"),
    ];
    let r = AgentCredentialResolver::build(&inputs, &s, StrictLevel::Strict).unwrap();

    // Ana resolves her own channels.
    assert_eq!(
        r.resolve("ana", WHATSAPP).unwrap().account_id_raw(),
        "personal"
    );
    assert_eq!(
        r.resolve("ana", TELEGRAM).unwrap().account_id_raw(),
        "ana_bot"
    );

    // Kate resolves hers.
    assert_eq!(
        r.resolve("kate", WHATSAPP).unwrap().account_id_raw(),
        "work"
    );
    assert_eq!(
        r.resolve("kate", TELEGRAM).unwrap().account_id_raw(),
        "kate_bot"
    );
}

#[test]
fn cross_agent_intent_blocked_at_boot() {
    let s = stores_of(
        vec![wa("personal", "ana"), wa("work", "kate")],
        vec![tg("ana_bot", "ana")],
    );
    // Kate declares credentials.whatsapp = "personal" (Ana's). Resolver
    // must reject — `personal` has allow_agents=[ana].
    let inputs = vec![
        input("ana", "personal", "ana_bot"),
        AgentCredentialsInput {
            agent_id: "kate".into(),
            outbound: {
                let mut m = HashMap::new();
                m.insert(WHATSAPP, "personal".to_string());
                m
            },
            inbound: HashMap::new(),
            asymmetric_allowed: HashMap::new(),
        },
    ];
    let err = AgentCredentialResolver::build(&inputs, &s, StrictLevel::Lenient).unwrap_err();
    assert!(
        err.iter().any(|e| matches!(
            e,
            BuildError::AllowAgentsExcludes {
                channel: WHATSAPP,
                instance,
                agent
            } if instance == "personal" && agent == "kate"
        )),
        "expected AllowAgentsExcludes for kate→personal: {err:#?}"
    );
}

#[test]
fn fingerprint_never_contains_account_id() {
    let s = stores_of(vec![wa("+573001234567", "ana")], vec![]);
    // An instance whose label is a phone number — the handle's fp must
    // not contain the digits anywhere in its hex, and the handle's
    // Debug representation must redact the raw id.
    let inputs = vec![input("ana", "+573001234567", "nonexistent")];
    // Telegram input references a missing instance — expected error.
    let err = AgentCredentialResolver::build(&inputs, &s, StrictLevel::Lenient).unwrap_err();
    assert!(err
        .iter()
        .any(|e| matches!(e, BuildError::MissingInstance { .. })));

    // Build a second time with just WA to get a real handle.
    let inputs2 = vec![AgentCredentialsInput {
        agent_id: "ana".into(),
        outbound: {
            let mut m = HashMap::new();
            m.insert(WHATSAPP, "+573001234567".to_string());
            m
        },
        inbound: HashMap::new(),
        asymmetric_allowed: HashMap::new(),
    }];
    let r = AgentCredentialResolver::build(&inputs2, &s, StrictLevel::Strict).unwrap();
    let handle = r.resolve("ana", WHATSAPP).unwrap();
    let dbg = format!("{handle:?}");
    assert!(
        !dbg.contains("573001234567"),
        "Debug leaked raw account: {dbg}"
    );
    assert_eq!(handle.fingerprint().to_hex().len(), 16);
}
