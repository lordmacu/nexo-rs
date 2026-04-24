//! Smoke tests that the shipped `config/plugins/whatsapp.yaml` example
//! parses and the serde defaults fill in every optional section.

use agent_config::{WhatsappPluginConfig, WhatsappPluginConfigFile};

fn load_yaml(s: &str) -> WhatsappPluginConfig {
    let file: WhatsappPluginConfigFile =
        serde_yaml::from_str(s).expect("yaml parses");
    // Tests assume the single-account shape; unwrap into one entry.
    let mut vec = file.whatsapp.into_vec();
    assert_eq!(vec.len(), 1, "config_parse_test yaml must describe one account");
    vec.remove(0)
}

#[test]
fn minimal_yaml_parses_with_defaults() {
    let cfg = load_yaml("whatsapp:\n  session_dir: ./x\n");
    assert_eq!(cfg.session_dir, "./x");
    assert!(!cfg.enabled, "enabled defaults to false");
    assert!(cfg.behavior.ignore_chat_meta);
    assert!(cfg.behavior.ignore_from_me);
    assert!(!cfg.behavior.ignore_groups);
    assert_eq!(cfg.acl.from_env, "WA_AGENT_ALLOW");
    assert_eq!(cfg.bridge.response_timeout_ms, 30_000);
    assert_eq!(cfg.bridge.on_timeout, "noop");
    assert!(cfg.daemon.prefer_existing);
    assert!(!cfg.transcriber.enabled);
}

#[test]
fn shipped_example_yaml_parses() {
    let raw = include_str!("../../../../config/plugins/whatsapp.yaml");
    let cfg = load_yaml(raw);
    // Shipped yaml has `enabled: true` — this plugin is the user's
    // primary channel in production. Just assert it parses with the
    // expected fields; don't pin the `enabled` value since deployments
    // may toggle it.
    assert_eq!(cfg.media_dir, "./data/media/whatsapp");
    assert_eq!(cfg.rate_limit.burst, 5);
}

#[test]
fn overrides_win_over_defaults() {
    let cfg = load_yaml(
        "whatsapp:\n  session_dir: /tmp/x\n  enabled: true\n  \
         bridge:\n    response_timeout_ms: 5000\n    on_timeout: apology_text\n",
    );
    assert!(cfg.enabled);
    assert_eq!(cfg.bridge.response_timeout_ms, 5000);
    assert_eq!(cfg.bridge.on_timeout, "apology_text");
}
