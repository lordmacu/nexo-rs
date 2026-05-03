//! Phase 82.10.k integration test — wire the production
//! `FsSecretsStore` adapter through `AdminRpcDispatcher` and
//! verify a `nexo/admin/secrets/write` dispatch produces:
//! - a file at `<secrets_dir>/<NAME>.txt` with mode 0600,
//! - the value in the daemon's process env,
//! - a typed success response with the resolved path.
//!
//! Capability denial path is covered by the dispatcher unit
//! tests; this file covers the production adapter end-to-end
//! (the dispatcher tests use a mock store).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use nexo_core::agent::admin_rpc::{AdminRpcDispatcher, CapabilitySet};
use nexo_setup::secrets_store::FsSecretsStore;
use serde_json::json;

/// `std::env::set_var` is process-global; serialise the test
/// case so concurrent integration tests don't collide.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn unique_name(suffix: &str) -> String {
    format!("NEXO_INTG_{}_{}_KEY", std::process::id(), suffix)
}

#[tokio::test]
async fn end_to_end_secrets_write_persists_file_and_sets_env() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let store = FsSecretsStore::with_secrets_dir(dir.path().to_path_buf());

    let mut grants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut caps_set = HashSet::new();
    caps_set.insert("secrets_write".to_string());
    grants.insert("agent-creator".to_string(), caps_set);
    let capabilities = CapabilitySet::from_grants(grants);

    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities)
        .with_secrets_domain(store);

    let name = unique_name("E2E");
    std::env::remove_var(&name);
    let result = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/secrets/write",
            json!({"name": name.clone(), "value": "sk-integration-test"}),
        )
        .await;

    let value = result.result.expect("ok");
    let path: PathBuf = serde_json::from_value(value["path"].clone()).unwrap();
    assert_eq!(path, dir.path().join(format!("{name}.txt")));
    assert_eq!(value["overwrote_env"], false);

    // File on disk has the value.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, "sk-integration-test");

    // Daemon process env now sees the new value.
    assert_eq!(std::env::var(&name).unwrap(), "sk-integration-test");

    std::env::remove_var(&name);
}
