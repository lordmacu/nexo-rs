//! Phase 82.10.s.4 integration test — exercise the full
//! multi-instance LLM flow end-to-end:
//!
//! 1. `nexo/admin/llm_providers/upsert` with `factory_type` +
//!    `api_key_secret_value` writes through the SecretsStore +
//!    patches `llm.yaml` with `factory_type` + `api_key_secret_id`
//!    (and NO inline `api_key`).
//! 2. The on-disk secret file lands at `<secrets_dir>/LLM_<ID>.txt`
//!    with mode 0600.
//! 3. `LlmConfig::resolve_all_keys(&FsSecretsStore)` populates the
//!    runtime `api_key` from the stored secret reference.
//! 4. `LlmRegistry::with_builtins().validate_config(&cfg)` accepts
//!    the resolved config — the registered factory matches.
//!
//! This proves the admin RPC + sync boot resolver + registry
//! validator + filesystem store all compose against real I/O.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nexo_config::{LlmConfig, SecretsSource};
use nexo_core::agent::admin_rpc::domains::agents::YamlPatcher;
use nexo_core::agent::admin_rpc::{AdminRpcDispatcher, CapabilitySet};
use nexo_llm::registry::LlmRegistry;
use nexo_setup::admin_adapters::LlmYamlPatcherFs;
use nexo_setup::secrets_store::FsSecretsStore;
use serde_json::{json, Value};

struct NoopAgentsYaml;

impl YamlPatcher for NoopAgentsYaml {
    fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn read_agent_field(&self, _: &str, _: &str) -> anyhow::Result<Option<Value>> {
        Ok(None)
    }
    fn upsert_agent_field(&self, _: &str, _: &str, _: Value) -> anyhow::Result<()> {
        Ok(())
    }
    fn remove_agent(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn upsert_writes_secret_yaml_then_resolve_then_registry_validates() {
    let dir = tempfile::TempDir::new().unwrap();
    let secrets_dir = dir.path().join("secrets");
    let llm_yaml_path = dir.path().join("llm.yaml");

    let secrets_store = FsSecretsStore::with_secrets_dir(secrets_dir.clone());
    let llm_yaml = Arc::new(LlmYamlPatcherFs::new(llm_yaml_path.clone()));

    let mut grants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut caps = HashSet::new();
    caps.insert("llm_keys_crud".to_string());
    caps.insert("agents_crud".to_string());
    grants.insert("agent-creator".to_string(), caps);
    let capabilities = CapabilitySet::from_grants(grants);

    let reload_signal: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});

    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities)
        .with_agents_domain(Arc::new(NoopAgentsYaml), reload_signal)
        .with_llm_providers_domain(llm_yaml)
        .with_secrets_domain(secrets_store.clone());

    // Step 1: upsert with factory_type + api_key_secret_value.
    let instance_id = "minimax-cliente-a";
    let key_value = "sk-multi-instance-e2e";
    let result = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/llm_providers/upsert",
            json!({
                "id": instance_id,
                "factory_type": "minimax",
                "base_url": "https://api.minimax.chat/v1",
                "api_key_secret_value": key_value,
            }),
        )
        .await;

    let err_dbg = format!("{:?}", result.error);
    let body = result
        .result
        .unwrap_or_else(|| panic!("upsert OK; err={err_dbg}"));
    assert_eq!(body["id"], instance_id, "summary echoes instance id");

    // Step 2: secret file lives on disk under derived id.
    let secret_path = secrets_dir.join("LLM_MINIMAX_CLIENTE_A.txt");
    let on_disk = std::fs::read_to_string(&secret_path)
        .unwrap_or_else(|e| panic!("secret file at {secret_path:?}: {e}"));
    assert_eq!(on_disk, key_value);
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

    // Step 3: yaml has factory_type + api_key_secret_id, no inline
    // api_key (the cleartext lives ONLY in the SecretsStore).
    let raw_yaml = std::fs::read_to_string(&llm_yaml_path).unwrap();
    assert!(
        raw_yaml.contains("factory_type: minimax"),
        "yaml missing factory_type; got:\n{raw_yaml}"
    );
    assert!(
        raw_yaml.contains("api_key_secret_id: LLM_MINIMAX_CLIENTE_A"),
        "yaml missing api_key_secret_id; got:\n{raw_yaml}"
    );
    assert!(
        !raw_yaml.contains(key_value),
        "cleartext api key leaked into yaml: {raw_yaml}"
    );
    assert!(
        !raw_yaml.contains("api_key:"),
        "yaml should not carry inline api_key for secret-backed instance: {raw_yaml}"
    );

    // Step 4: load yaml + resolve keys against the SAME secrets dir.
    let mut cfg: LlmConfig = serde_yaml::from_str(&raw_yaml).expect("parse llm.yaml");
    let resolver: &dyn SecretsSource = secrets_store.as_ref();
    cfg.resolve_all_keys(resolver)
        .unwrap_or_else(|errs| panic!("resolve_all_keys failed: {errs:?}"));
    let resolved = cfg
        .providers
        .get(instance_id)
        .expect("instance present in config");
    assert_eq!(
        resolved.api_key, key_value,
        "resolved api_key must match stored secret"
    );

    // Step 5: registry accepts the yaml — factory_type=minimax is
    // a registered builtin so validate_config returns Ok.
    let registry = LlmRegistry::with_builtins();
    registry
        .validate_config(&cfg)
        .unwrap_or_else(|errs| panic!("validate_config failed: {errs:?}"));
}

#[tokio::test]
async fn upsert_rejects_conflicting_key_sources() {
    let dir = tempfile::TempDir::new().unwrap();
    let secrets_dir = dir.path().join("secrets");
    let llm_yaml_path = dir.path().join("llm.yaml");

    let secrets_store = FsSecretsStore::with_secrets_dir(secrets_dir);
    let llm_yaml = Arc::new(LlmYamlPatcherFs::new(llm_yaml_path));

    let mut grants: HashMap<String, HashSet<String>> = HashMap::new();
    let mut caps = HashSet::new();
    caps.insert("llm_keys_crud".to_string());
    caps.insert("agents_crud".to_string());
    grants.insert("agent-creator".to_string(), caps);
    let capabilities = CapabilitySet::from_grants(grants);

    let reload_signal: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});

    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities)
        .with_agents_domain(Arc::new(NoopAgentsYaml), reload_signal)
        .with_llm_providers_domain(llm_yaml)
        .with_secrets_domain(secrets_store);

    // Two key sources at once → loud reject. Single boot fail beats
    // a runtime LLM dispatch error mid-traffic.
    let result = dispatcher
        .dispatch(
            "agent-creator",
            "nexo/admin/llm_providers/upsert",
            json!({
                "id": "minimax-conflict",
                "factory_type": "minimax",
                "base_url": "https://api.minimax.chat/v1",
                "api_key_env": "SOME_ENV_VAR",
                "api_key_secret_value": "sk-also-this",
            }),
        )
        .await;

    let err = result
        .error
        .expect("upsert must reject conflicting sources");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("conflicting"),
        "expected conflict error, got: {msg}"
    );
}
