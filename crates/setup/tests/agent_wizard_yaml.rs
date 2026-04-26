//! Integration tests for the agent-aware YAML helpers used by the
//! per-agent setup wizard. They exercise the full read/upsert/remove
//! path against a tempdir fixture and re-deserialise the resulting
//! file through `nexo_config::AgentsConfig` to make sure the runtime
//! schema (with `deny_unknown_fields`) is still happy.

use std::fs;
use std::path::PathBuf;

use nexo_config::AgentsConfig;
use nexo_setup::yaml_patch;
use serde_yaml::Value;
use tempfile::TempDir;

const FIXTURE: &str = r#"agents:
- id: kate
  model:
    provider: anthropic
    model: claude-haiku-4-5
  plugins:
  - telegram
  skills:
  - weather
- id: ana
  model:
    provider: openai
    model: gpt-4o
  plugins: []
"#;

fn fresh_fixture() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("agents.yaml");
    fs::write(&file, FIXTURE).unwrap();
    (dir, file)
}

fn parse_agents(file: &PathBuf) -> AgentsConfig {
    let text = fs::read_to_string(file).unwrap();
    serde_yaml::from_str::<AgentsConfig>(&text)
        .unwrap_or_else(|e| panic!("re-parse AgentsConfig: {e}\n\nyaml:\n{text}"))
}

#[test]
fn read_agent_field_existing() {
    let (_dir, file) = fresh_fixture();
    let v = yaml_patch::read_agent_field(&file, "kate", "model.provider")
        .unwrap()
        .unwrap();
    assert_eq!(v.as_str(), Some("anthropic"));
}

#[test]
fn upsert_creates_new_field_and_stays_schema_valid() {
    let (_dir, file) = fresh_fixture();
    yaml_patch::upsert_agent_field(&file, "ana", "language", Value::String("es".into())).unwrap();
    let cfg = parse_agents(&file);
    let ana = cfg.agents.iter().find(|a| a.id == "ana").unwrap();
    assert_eq!(ana.language.as_deref(), Some("es"));
}

#[test]
fn remove_field_drops_key_and_stays_valid() {
    let (_dir, file) = fresh_fixture();
    yaml_patch::upsert_agent_field(&file, "kate", "language", Value::String("es".into())).unwrap();
    yaml_patch::remove_agent_field(&file, "kate", "language").unwrap();
    let cfg = parse_agents(&file);
    let kate = cfg.agents.iter().find(|a| a.id == "kate").unwrap();
    assert!(kate.language.is_none());
}

#[test]
fn append_list_item_is_idempotent() {
    let (_dir, file) = fresh_fixture();
    yaml_patch::append_agent_list_item(&file, "kate", "plugins", Value::String("whatsapp".into()))
        .unwrap();
    yaml_patch::append_agent_list_item(&file, "kate", "plugins", Value::String("whatsapp".into()))
        .unwrap();
    let cfg = parse_agents(&file);
    let kate = cfg.agents.iter().find(|a| a.id == "kate").unwrap();
    let count = kate.plugins.iter().filter(|p| *p == "whatsapp").count();
    assert_eq!(count, 1);
    assert!(kate.plugins.contains(&"telegram".to_string()));
}

#[test]
fn remove_list_item_by_predicate() {
    let (_dir, file) = fresh_fixture();
    yaml_patch::remove_agent_list_item(&file, "kate", "plugins", &|v| {
        v.as_str() == Some("telegram")
    })
    .unwrap();
    let cfg = parse_agents(&file);
    let kate = cfg.agents.iter().find(|a| a.id == "kate").unwrap();
    assert!(kate.plugins.is_empty());
}

#[test]
fn append_then_remove_inbound_binding_mapping() {
    let (_dir, file) = fresh_fixture();
    let mut binding = serde_yaml::Mapping::new();
    binding.insert(
        Value::String("plugin".into()),
        Value::String("telegram".into()),
    );
    yaml_patch::append_agent_list_item(&file, "kate", "inbound_bindings", Value::Mapping(binding))
        .unwrap();
    // Same payload again → idempotent.
    let mut dup = serde_yaml::Mapping::new();
    dup.insert(
        Value::String("plugin".into()),
        Value::String("telegram".into()),
    );
    yaml_patch::append_agent_list_item(&file, "kate", "inbound_bindings", Value::Mapping(dup))
        .unwrap();
    let v = yaml_patch::read_agent_field(&file, "kate", "inbound_bindings")
        .unwrap()
        .unwrap();
    assert_eq!(v.as_sequence().unwrap().len(), 1);

    // Remove via predicate.
    yaml_patch::remove_agent_list_item(&file, "kate", "inbound_bindings", &|v| {
        v.get("plugin").and_then(Value::as_str) == Some("telegram")
    })
    .unwrap();
    let v = yaml_patch::read_agent_field(&file, "kate", "inbound_bindings")
        .unwrap()
        .unwrap();
    assert!(v.as_sequence().unwrap().is_empty());

    // File still parses as AgentsConfig.
    let _cfg = parse_agents(&file);
}
