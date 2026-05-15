//! Integration test: the reference plugin's `nexo-plugin.toml`
//! parses against the production `nexo-plugin-manifest` parser
//! and exercises every Phase 81.33.b.real manifest section.
//!
//! This locks down the contract — if a future schema bump
//! breaks a field shape, this test catches it BEFORE plugin
//! authors copy the broken template.

use std::path::PathBuf;

use nexo_plugin_manifest::dashboard::{AuthCheck, InstanceLayout};
use nexo_plugin_manifest::manifest::PluginManifest;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("nexo-plugin.toml")
}

fn parse_manifest() -> PluginManifest {
    let raw = std::fs::read_to_string(manifest_path()).expect("read manifest");
    toml::from_str::<PluginManifest>(&raw).expect("manifest parses")
}

#[test]
fn manifest_has_version_2() {
    let m = parse_manifest();
    assert_eq!(m.manifest_version, 2);
}

#[test]
fn pairing_adapter_section_declared() {
    let m = parse_manifest();
    let adapter = m
        .plugin
        .pairing
        .adapter
        .as_ref()
        .expect("pairing.adapter present");
    assert_eq!(adapter.channel_id, "reference_demo");
    assert_eq!(adapter.broker_topic_prefix, "plugin.reference_demo");
}

#[test]
fn http_section_declared() {
    let m = parse_manifest();
    let http = m.plugin.http.as_ref().expect("http present");
    assert_eq!(http.mount_prefix, "/reference_demo");
    assert!(http.validate().is_ok());
}

#[test]
fn admin_section_declared() {
    let m = parse_manifest();
    let admin = m.plugin.admin.as_ref().expect("admin present");
    assert_eq!(admin.method_prefix, "nexo/admin/reference_demo/");
    assert_eq!(admin.broker_topic_prefix, "plugin.reference_demo.admin");
    assert!(admin.validate().is_ok());
}

#[test]
fn metrics_section_declared() {
    let m = parse_manifest();
    let metrics = m.plugin.metrics.as_ref().expect("metrics present");
    assert!(metrics.prometheus);
    assert_eq!(metrics.broker_topic_prefix, "plugin.reference_demo");
    assert!(metrics.validate().is_ok());
}

#[test]
fn dashboard_section_uses_single_layout_and_file_presence() {
    let m = parse_manifest();
    let dashboard = m.plugin.dashboard.as_ref().expect("dashboard present");
    assert!(matches!(dashboard.layout, InstanceLayout::Single));
    match &dashboard.auth_check {
        AuthCheck::FilePresence { path } => {
            assert_eq!(path, "reference_demo_token.txt");
        }
        other => panic!("expected FilePresence, got {other:?}"),
    }
    assert!(dashboard.validate().is_ok());
}
