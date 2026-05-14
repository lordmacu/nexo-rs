//! Phase 93.3 — coverage for `load_plugin_entries` walker.
//!
//! Tests target the public helper directly rather than going
//! through `AppConfig::load`, since the walker has no dependency
//! on the rest of the config-load pipeline and the focused tests
//! stay fast.

use nexo_config::load_plugin_entries;
use std::fs;
use tempfile::TempDir;

fn write(dir: &std::path::Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

#[test]
fn entries_populated_from_single_plugin_file() {
    let tmp = TempDir::new().unwrap();
    let plugins = tmp.path().to_path_buf();
    write(
        &plugins,
        "slack.yaml",
        r##"
slack:
  token_env: SLACK_BOT_TOKEN
  default_channel: "#alerts"
"##,
    );
    let entries = load_plugin_entries(&plugins);
    let v = entries.get("slack").expect("slack entry present");
    // Outer `slack:` wrapper stripped: value is the inner map.
    let map = v.as_mapping().expect("slack value is a mapping");
    assert_eq!(
        map.get(serde_yaml::Value::String("token_env".into()))
            .and_then(|v| v.as_str()),
        Some("SLACK_BOT_TOKEN"),
    );
    assert_eq!(
        map.get(serde_yaml::Value::String("default_channel".into()))
            .and_then(|v| v.as_str()),
        Some("#alerts"),
    );
}

#[test]
fn entries_and_typed_both_populated_for_whatsapp() {
    let tmp = TempDir::new().unwrap();
    let plugins = tmp.path().to_path_buf();
    write(
        &plugins,
        "whatsapp.yaml",
        r#"
whatsapp:
  - instance: main
    session_dir: ./data/wa-main
"#,
    );
    let entries = load_plugin_entries(&plugins);
    let v = entries.get("whatsapp").expect("whatsapp entry");
    // After outer-key strip, value is the Vec<map>.
    let seq = v.as_sequence().expect("whatsapp value is a sequence");
    assert_eq!(seq.len(), 1);
    let first = seq[0].as_mapping().expect("first elem is mapping");
    assert_eq!(
        first
            .get(serde_yaml::Value::String("instance".into()))
            .and_then(|v| v.as_str()),
        Some("main"),
    );
}

#[test]
fn entries_empty_when_plugins_dir_missing() {
    let tmp = TempDir::new().unwrap();
    let nonexistent = tmp.path().join("definitely-not-here");
    let entries = load_plugin_entries(&nonexistent);
    assert!(entries.is_empty());
}

#[test]
fn discovery_yaml_filtered_from_entries() {
    let tmp = TempDir::new().unwrap();
    let plugins = tmp.path().to_path_buf();
    write(&plugins, "slack.yaml", "slack: { x: 1 }\n");
    write(
        &plugins,
        "discovery.yaml",
        "discovery:\n  search_paths: []\n",
    );
    let entries = load_plugin_entries(&plugins);
    assert!(entries.contains_key("slack"));
    assert!(
        !entries.contains_key("discovery"),
        "discovery.yaml is framework-internal and must be filtered",
    );
}

#[test]
fn outer_key_kept_when_does_not_match_stem() {
    let tmp = TempDir::new().unwrap();
    let plugins = tmp.path().to_path_buf();
    // Filename stem is `slack` but the YAML's top-level key is
    // `not_slack`. Strip is conservative — must NOT collapse the
    // wrapper in this case.
    write(
        &plugins,
        "slack.yaml",
        r#"
not_slack:
  token: 1
"#,
    );
    let entries = load_plugin_entries(&plugins);
    let v = entries.get("slack").expect("slack entry present");
    let outer = v.as_mapping().expect("value is a mapping");
    assert!(
        outer.contains_key(serde_yaml::Value::String("not_slack".into())),
        "outer `not_slack:` wrapper must survive (stem mismatch)",
    );
}
