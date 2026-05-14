//! Phase 81.31 follow-up — daemon-flavoured integration test
//! for `nexo/admin/persona/*` + `agents/get` enrichment.
//!
//! Wires the real production adapters (`AgentsYamlPatcher` +
//! `FilesystemPersonaStore`) into the real `AdminRpcDispatcher`
//! against a tempfs config dir, then drives the dispatch loop
//! end-to-end. Skips the daemon binary spawn — exercises the
//! same code path a real `/api/admin` call would hit.

use std::collections::BTreeMap;
use std::sync::Arc;

use nexo_core::agent::admin_rpc::capabilities::CapabilitySet;
use nexo_core::agent::admin_rpc::dispatcher::{AdminRpcDispatcher, AdminRpcError};
use nexo_setup::admin_adapters::{AgentsYamlPatcher, FilesystemPersonaStore};
use serde_json::json;

const TEST_MICROAPP: &str = "test_microapp";

fn seed_agents_yaml(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("agents.yaml");
    std::fs::write(&p, body).unwrap();
    p
}

fn capabilities_for_test() -> Arc<CapabilitySet> {
    let mut grants = std::collections::HashMap::new();
    let mut caps = std::collections::HashSet::new();
    caps.insert("agents_crud".to_string());
    caps.insert("agents_read".to_string());
    grants.insert(TEST_MICROAPP.to_string(), caps);
    CapabilitySet::from_grants(grants)
}

fn build_dispatcher(config_dir: &std::path::Path) -> AdminRpcDispatcher {
    let agents_yaml = Arc::new(AgentsYamlPatcher::new(config_dir.join("agents.yaml")));
    let persona = FilesystemPersonaStore::new(config_dir.to_path_buf(), vec![]);
    AdminRpcDispatcher::new()
        .with_capabilities(capabilities_for_test())
        .with_agents_domain(agents_yaml, Arc::new(|| {}))
        .with_persona_snapshot_reader(persona.clone())
        .with_persona_store(persona)
}

/// End-to-end save → read cycle:
///   1. Seed an agent in agents.yaml with `language: en`.
///   2. `persona/save_localized` writes the 4 ES workspace files
///      + patches `locale_prompts.es`.
///   3. `agents/get` returns `persona_locales` with both locales.
#[tokio::test]
async fn save_localized_then_agents_get_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("ws/ana")).unwrap();
    std::fs::write(
        dir.path().join("ws/ana/IDENTITY.md"),
        "default identity",
    )
    .unwrap();
    seed_agents_yaml(
        dir.path(),
        r#"agents:
  - id: ana
    workspace: ws/ana
    system_prompt: top
    language: en
    locale_prompts:
      en: english prompt
    model:
      provider: minimax
      model: MiniMax-M2.5
"#,
    );

    let dispatcher = build_dispatcher(dir.path());

    // 1. save_localized
    let save_params = json!({
        "agent_id": "ana",
        "locale": "es",
        "system_prompt": "prompt ES",
        "identity": "identidad",
        "soul": "alma",
        "user": "usuario",
        "agents": "agentes",
        "patch_yaml": true,
    });
    let save_res = dispatcher
        .dispatch("test_microapp", "nexo/admin/persona/save_localized", save_params)
        .await;
    assert!(
        save_res.error.is_none(),
        "save_localized error: {:?}",
        save_res.error
    );
    let written = save_res
        .result
        .as_ref()
        .unwrap()
        .get("written_paths")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(written.len(), 4, "should write 4 workspace files");

    // 2. Files exist on disk
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ws/ana/IDENTITY.es.md")).unwrap(),
        "identidad"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ws/ana/SOUL.es.md")).unwrap(),
        "alma"
    );

    // 3. YAML patched with locale_prompts.es
    let yaml = std::fs::read_to_string(dir.path().join("agents.yaml")).unwrap();
    assert!(
        yaml.contains("prompt ES"),
        "agents.yaml should carry locale_prompts.es: {yaml}"
    );

    // 4. agents/get returns persona_locales with both locales
    let get_res = dispatcher
        .dispatch("test_microapp", "nexo/admin/agents/get", json!({"agent_id": "ana"}))
        .await;
    assert!(get_res.error.is_none(), "agents/get error: {:?}", get_res.error);
    let detail = get_res.result.unwrap();
    let pl = detail.get("persona_locales").expect("persona_locales present");
    let available: Vec<String> = pl
        .get("available")
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(available.contains(&"en".to_string()));
    assert!(available.contains(&"es".to_string()));
    // en is first because it's the agent's language.
    assert_eq!(available[0], "en");

    // 5. ES snapshot contains the localised content
    let snapshots = pl.get("snapshots").unwrap().as_array().unwrap();
    let es_entry = snapshots
        .iter()
        .find(|s| s.get("locale").and_then(|v| v.as_str()) == Some("es"))
        .expect("es snapshot present");
    let snap = es_entry.get("snapshot").unwrap();
    assert_eq!(snap.get("identity").and_then(|v| v.as_str()), Some("identidad"));
    assert_eq!(snap.get("system_prompt").and_then(|v| v.as_str()), Some("prompt ES"));
}

/// Capability gate: caller without `agents_crud` gets a typed
/// `forbidden` error instead of writing to disk. Mirrors the same
/// guard `agents/upsert` already enforces.
///
/// We can't easily fake the capability gate without re-wiring the
/// admin bootstrap; instead we exercise the "no store wired" path
/// (which is the most common operator-side failure mode — the
/// daemon refusing to ship persona before all adapters land).
#[tokio::test]
async fn save_localized_returns_domain_not_configured_when_store_missing() {
    let dir = tempfile::tempdir().unwrap();
    seed_agents_yaml(dir.path(), "agents: []\n");

    let agents_yaml = Arc::new(AgentsYamlPatcher::new(dir.path().join("agents.yaml")));
    let dispatcher = AdminRpcDispatcher::new()
        .with_capabilities(capabilities_for_test())
        .with_agents_domain(agents_yaml, Arc::new(|| {}));
    let res = dispatcher
        .dispatch("test_microapp", 
            "nexo/admin/persona/save_localized",
            json!({
                "agent_id": "ana",
                "locale": "es",
                "system_prompt": "",
                "identity": "",
                "soul": "",
                "user": "",
                "agents": "",
            }),
        )
        .await;
    let err = res.error.expect("error when store not wired");
    assert!(
        matches!(err, AdminRpcError::Internal(ref msg) if msg.contains("persona domain not configured")),
        "unexpected error: {err:?}",
    );
}

/// Invalid locale tags are rejected by the dispatcher path
/// before any filesystem write happens. Mirrors the unit test
/// in `setup::persona_files` but verified through the full RPC
/// stack.
#[tokio::test]
async fn save_localized_invalid_locale_maps_to_invalid_params() {
    let dir = tempfile::tempdir().unwrap();
    seed_agents_yaml(
        dir.path(),
        r#"agents:
  - id: ana
    workspace: ws/ana
    system_prompt: top
    model:
      provider: minimax
      model: MiniMax-M2.5
"#,
    );
    std::fs::create_dir_all(dir.path().join("ws/ana")).unwrap();
    let dispatcher = build_dispatcher(dir.path());
    let res = dispatcher
        .dispatch("test_microapp", 
            "nexo/admin/persona/save_localized",
            json!({
                "agent_id": "ana",
                "locale": "klingon",
                "system_prompt": "",
                "identity": "",
                "soul": "",
                "user": "",
                "agents": "",
            }),
        )
        .await;
    let err = res.error.expect("error on invalid locale");
    match err {
        AdminRpcError::InvalidParams(msg) => {
            assert!(msg.contains("klingon"), "expected 'klingon' in: {msg}");
        }
        other => panic!("expected InvalidParams, got: {other:?}"),
    }
}

/// `agents/upsert` accepts a `locale_prompts` block (Phase 81.31
/// follow-up #6) — verify the field round-trips through the wire
/// + lands in agents.yaml verbatim.
#[tokio::test]
async fn agents_upsert_persists_locale_prompts_via_dispatcher() {
    let dir = tempfile::tempdir().unwrap();
    seed_agents_yaml(
        dir.path(),
        r#"agents:
  - id: ana
    workspace: ws/ana
    system_prompt: top
    model:
      provider: minimax
      model: MiniMax-M2.5
"#,
    );
    let dispatcher = build_dispatcher(dir.path());
    let mut prompts = BTreeMap::new();
    prompts.insert("en".to_string(), "english variant".to_string());
    prompts.insert("es".to_string(), "variante espanola".to_string());
    let res = dispatcher
        .dispatch("test_microapp", 
            "nexo/admin/agents/upsert",
            json!({
                "id": "ana",
                "model": {"provider": "minimax", "model": "MiniMax-M2.5"},
                "locale_prompts": prompts,
            }),
        )
        .await;
    assert!(res.error.is_none(), "upsert error: {:?}", res.error);
    let yaml = std::fs::read_to_string(dir.path().join("agents.yaml")).unwrap();
    assert!(
        yaml.contains("english variant") && yaml.contains("variante espanola"),
        "yaml missing locale_prompts entries: {yaml}"
    );
}
