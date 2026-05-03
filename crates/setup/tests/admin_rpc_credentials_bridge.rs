//! Phase 82.10.n integration test — wire a production
//! [`TelegramPersister`] / [`EmailPersister`] through the
//! [`AdminRpcDispatcher`] and verify a `nexo/admin/credentials/register`
//! end-to-end produces the per-channel yaml entry, the secret
//! file, the agent binding patch, and a probe outcome.
//!
//! The bridge unit tests (in `crates/core`) cover the dispatcher
//! routing + mock persister call order; this file proves the
//! production stack composes correctly.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nexo_core::agent::admin_rpc::{AdminRpcDispatcher, CapabilitySet};
use nexo_setup::admin_adapters::{AgentsYamlPatcher, FilesystemCredentialStore};
use nexo_setup::persisters::{EmailPersister, TelegramPersister};
use serde_json::json;

fn grants(microapp: &str, caps: &[&str]) -> Arc<CapabilitySet> {
    let mut g: HashMap<String, HashSet<String>> = HashMap::new();
    g.insert(
        microapp.into(),
        caps.iter().map(|c| c.to_string()).collect(),
    );
    CapabilitySet::from_grants(g)
}

/// Seed a minimal `agents.yaml` with a single agent so the
/// bridge has something to bind to. Returns the file path.
/// Production expects `agents:` as a sequence of `- id:` blocks
/// (verified against repo `config/agents.yaml`), not a mapping.
fn seed_agent_yaml(dir: &std::path::Path, agent_id: &str) -> std::path::PathBuf {
    let path = dir.join("agents.yaml");
    let body = format!(
        "agents:\n- id: {agent_id}\n  model:\n    provider: minimax\n  inbound_bindings: []\n"
    );
    std::fs::write(&path, body).unwrap();
    path
}

#[tokio::test]
async fn telegram_register_persists_yaml_writes_secret_binds_agent_then_revokes() {
    let dir = tempfile::TempDir::new().unwrap();
    let config_dir = dir.path().join("config");
    let plugins_dir = config_dir.join("plugins");
    let secrets_dir = dir.path().join("secrets");
    std::fs::create_dir_all(&config_dir).unwrap();

    // Production wires.
    let agents_yaml_path = seed_agent_yaml(&config_dir, "kate");
    let agents_yaml = Arc::new(AgentsYamlPatcher::new(agents_yaml_path.clone()));
    let cred_store = Arc::new(FilesystemCredentialStore::new(secrets_dir.clone()));
    let telegram_persister = TelegramPersister::new(
        plugins_dir.join("telegram.yaml"),
        secrets_dir.clone(),
    );

    let reload_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&reload_count);
    let reload_signal: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });

    let mut dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(grants(
            "agent-creator",
            &["credentials_crud", "agents_crud"],
        ))
        .with_agents_domain(agents_yaml.clone(), reload_signal.clone())
        .with_credentials_domain(cred_store.clone());
    dispatcher.register_persister(telegram_persister.clone());

    // ── register ─────────────────────────────────────────────────
    let resp = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/credentials/register",
            json!({
                "channel": "telegram",
                "instance": "kate",
                "agent_ids": ["kate"],
                "payload": { "token": "tg.SECRET" },
                "metadata": {
                    "polling": { "enabled": true, "interval_ms": 1500 },
                    "allow_agents": ["kate"],
                    "allowed_chat_ids": [123]
                }
            }),
        )
        .await;
    let err_dbg = format!("{:?}", resp.error);
    let body = resp.result.unwrap_or_else(|| panic!("register OK; err={err_dbg}"));
    assert_eq!(body["summary"]["channel"], "telegram");
    assert_eq!(body["summary"]["instance"], "kate");
    assert_eq!(body["summary"]["agent_ids"][0], "kate");
    // Validation block is present (persister was registered) but
    // probe will be unhealthy because we never hit api.telegram.org
    // with a real token; we just assert it ran (probed=true) +
    // surfaced a reason code.
    let validation = &body["validation"];
    assert_eq!(validation["probed"], true);
    assert!(validation.get("reason_code").is_some());

    // Secret file landed at the canonical path with mode 0600 on
    // Unix.
    let secret_path = secrets_dir.join("telegram_kate_token.txt");
    let on_disk = std::fs::read_to_string(&secret_path).unwrap();
    assert_eq!(on_disk, "tg.SECRET");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secret_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "expected mode 0600 got {mode:o}");
    }

    // telegram.yaml shape matches what the runtime loader expects.
    let yaml_text = std::fs::read_to_string(plugins_dir.join("telegram.yaml")).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_text).unwrap();
    let entries = parsed
        .get("telegram")
        .and_then(serde_yaml::Value::as_sequence)
        .unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(
        entry.get("instance").and_then(serde_yaml::Value::as_str),
        Some("kate")
    );
    assert_eq!(
        entry.get("token").and_then(serde_yaml::Value::as_str),
        Some("${file:./secrets/telegram_kate_token.txt}")
    );
    assert_eq!(
        entry
            .get("polling")
            .and_then(|p| p.get("interval_ms"))
            .and_then(serde_yaml::Value::as_u64),
        Some(1500)
    );

    // agents.yaml binding patched. Production agents.yaml is a
    // sequence (`agents: - id: kate ...`), not a mapping.
    let agents_text = std::fs::read_to_string(&agents_yaml_path).unwrap();
    let agents_parsed: serde_yaml::Value = serde_yaml::from_str(&agents_text).unwrap();
    let agent_entry = agents_parsed
        .get("agents")
        .and_then(serde_yaml::Value::as_sequence)
        .and_then(|seq| {
            seq.iter().find(|e| {
                e.get("id").and_then(serde_yaml::Value::as_str) == Some("kate")
            })
        })
        .expect("kate entry");
    let bindings = agent_entry
        .get("inbound_bindings")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("inbound_bindings sequence");
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        bindings[0].get("plugin").and_then(serde_yaml::Value::as_str),
        Some("telegram")
    );
    assert_eq!(
        bindings[0]
            .get("instance")
            .and_then(serde_yaml::Value::as_str),
        Some("kate")
    );

    // Reload signal fired at least once (persister persisted +
    // agent bound — both trigger reload).
    let reloads = reload_count.load(std::sync::atomic::Ordering::Relaxed);
    assert!(reloads >= 1, "expected reload signal, got {reloads}");

    // ── revoke ───────────────────────────────────────────────────
    let revoke_resp = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/credentials/revoke",
            json!({ "channel": "telegram", "instance": "kate" }),
        )
        .await;
    let r = revoke_resp.result.expect("revoke OK");
    // Opaque store had the entry → removed = true.
    assert_eq!(r["removed"], true);
    let unbound = r["unbound_agents"].as_array().unwrap();
    assert_eq!(unbound.len(), 1);
    assert_eq!(unbound[0], "kate");

    // Yaml entry gone.
    let yaml_after = std::fs::read_to_string(plugins_dir.join("telegram.yaml")).unwrap();
    let parsed_after: serde_yaml::Value = serde_yaml::from_str(&yaml_after).unwrap();
    let entries_after = parsed_after
        .get("telegram")
        .and_then(serde_yaml::Value::as_sequence)
        .unwrap();
    assert!(entries_after.is_empty());

    // Secret file gone.
    assert!(!secret_path.exists());

    // agents.yaml binding gone.
    let agents_after = std::fs::read_to_string(&agents_yaml_path).unwrap();
    let agents_parsed_after: serde_yaml::Value = serde_yaml::from_str(&agents_after).unwrap();
    let kate_after = agents_parsed_after
        .get("agents")
        .and_then(serde_yaml::Value::as_sequence)
        .and_then(|seq| {
            seq.iter().find(|e| {
                e.get("id").and_then(serde_yaml::Value::as_str) == Some("kate")
            })
        })
        .expect("kate entry");
    let bindings_after = kate_after
        .get("inbound_bindings")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("inbound_bindings sequence");
    assert!(bindings_after.is_empty());
}

#[tokio::test]
async fn email_register_persists_yaml_and_toml_secret_with_imap_metadata() {
    let dir = tempfile::TempDir::new().unwrap();
    let config_dir = dir.path().join("config");
    let plugins_dir = config_dir.join("plugins");
    let secrets_dir = dir.path().join("secrets");
    std::fs::create_dir_all(&config_dir).unwrap();
    let agents_yaml_path = seed_agent_yaml(&config_dir, "ana");
    let agents_yaml = Arc::new(AgentsYamlPatcher::new(agents_yaml_path));
    let cred_store = Arc::new(FilesystemCredentialStore::new(secrets_dir.clone()));
    let email_persister =
        EmailPersister::new(plugins_dir.join("email.yaml"), secrets_dir.clone());

    let reload_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&reload_count);
    let reload_signal: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });

    let mut dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(grants(
            "agent-creator",
            &["credentials_crud", "agents_crud"],
        ))
        .with_agents_domain(agents_yaml, reload_signal.clone())
        .with_credentials_domain(cred_store);
    dispatcher.register_persister(email_persister);

    let resp = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/credentials/register",
            json!({
                "channel": "email",
                "instance": "ops",
                "agent_ids": ["ana"],
                "payload": { "address": "ops@example.com", "password": "p@ss" },
                "metadata": {
                    "imap": { "host": "127.0.0.1", "port": 1, "tls": "implicit_tls" },
                    "smtp": { "host": "127.0.0.1", "port": 1, "tls": "starttls" }
                }
            }),
        )
        .await;
    let err_dbg = format!("{:?}", resp.error);
    let body = resp.result.unwrap_or_else(|| panic!("register OK; err={err_dbg}"));
    assert_eq!(body["summary"]["channel"], "email");
    // Probe ran but failed (port 1 is unreachable); register
    // still succeeds end-to-end.
    let validation = &body["validation"];
    assert_eq!(validation["probed"], true);
    assert_eq!(validation["healthy"], false);
    assert_eq!(validation["reason_code"], "connectivity_failed");

    // TOML secret on disk with mode 0600.
    let toml_path = secrets_dir.join("email").join("ops.toml");
    let body_toml = std::fs::read_to_string(&toml_path).unwrap();
    assert!(body_toml.contains("[auth]"));
    assert!(body_toml.contains("password = 'p@ss'"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&toml_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // email.yaml has the account with the imap host wired.
    let yaml_text = std::fs::read_to_string(plugins_dir.join("email.yaml")).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml_text).unwrap();
    let accounts = parsed
        .get("email")
        .and_then(|e| e.get("accounts"))
        .and_then(serde_yaml::Value::as_sequence)
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].get("address").and_then(serde_yaml::Value::as_str),
        Some("ops@example.com")
    );
    assert_eq!(
        accounts[0]
            .get("imap")
            .and_then(|i| i.get("host"))
            .and_then(serde_yaml::Value::as_str),
        Some("127.0.0.1")
    );
}
