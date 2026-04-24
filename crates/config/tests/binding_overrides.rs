//! YAML parse coverage for per-binding capability overrides.
//!
//! The runtime-side merge / validation lands in later sessions; this file
//! only locks down the serde schema so the downstream work can rely on
//! what each form deserializes to.

use agent_config::{
    InboundBinding, SenderRateLimitKeyword, SenderRateLimitOverride,
};

fn parse(yaml: &str) -> InboundBinding {
    serde_yaml::from_str(yaml).expect("valid binding YAML")
}

#[test]
fn legacy_binding_parses_with_all_overrides_inheriting() {
    let b = parse("plugin: whatsapp\n");
    assert_eq!(b.plugin, "whatsapp");
    assert!(b.instance.is_none());
    assert!(b.allowed_tools.is_none());
    assert!(b.outbound_allowlist.is_none());
    assert!(b.skills.is_none());
    assert!(b.model.is_none());
    assert!(b.system_prompt_extra.is_none());
    assert!(b.allowed_delegates.is_none());
    assert!(matches!(
        b.sender_rate_limit,
        SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Inherit)
    ));
}

#[test]
fn full_override_round_trip() {
    let yaml = r#"
plugin: telegram
instance: ana_tg
allowed_tools: ["*"]
outbound_allowlist:
  telegram: [1194292426]
skills: [browser, github]
model:
  provider: anthropic
  model: claude-sonnet-4-5
system_prompt_extra: |
  Private Telegram channel.
sender_rate_limit:
  rps: 1.0
  burst: 3
allowed_delegates: ["*"]
"#;
    let b = parse(yaml);
    assert_eq!(b.plugin, "telegram");
    assert_eq!(b.instance.as_deref(), Some("ana_tg"));
    assert_eq!(b.allowed_tools.as_deref(), Some(&["*".to_string()][..]));
    let ob = b.outbound_allowlist.expect("outbound override present");
    assert_eq!(ob.telegram, vec![1_194_292_426]);
    assert_eq!(
        b.skills.as_deref(),
        Some(&["browser".to_string(), "github".to_string()][..])
    );
    let m = b.model.expect("model override present");
    assert_eq!(m.provider, "anthropic");
    assert_eq!(m.model, "claude-sonnet-4-5");
    assert!(b.system_prompt_extra.as_deref().unwrap().contains("Private"));
    assert!(matches!(
        b.sender_rate_limit,
        SenderRateLimitOverride::Config(_)
    ));
    assert_eq!(b.allowed_delegates.as_deref(), Some(&["*".to_string()][..]));
}

#[test]
fn sender_rate_limit_inherit_keyword() {
    let b = parse("plugin: telegram\nsender_rate_limit: inherit\n");
    assert!(matches!(
        b.sender_rate_limit,
        SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Inherit)
    ));
}

#[test]
fn sender_rate_limit_disable_keyword() {
    let b = parse("plugin: telegram\nsender_rate_limit: disable\n");
    assert!(matches!(
        b.sender_rate_limit,
        SenderRateLimitOverride::Keyword(SenderRateLimitKeyword::Disable)
    ));
}

#[test]
fn sender_rate_limit_config_object() {
    let b = parse("plugin: telegram\nsender_rate_limit:\n  rps: 0.5\n  burst: 3\n");
    match b.sender_rate_limit {
        SenderRateLimitOverride::Config(cfg) => {
            assert_eq!(cfg.rps, 0.5);
            assert_eq!(cfg.burst, 3);
        }
        other => panic!("expected Config variant, got {other:?}"),
    }
}

#[test]
fn allowed_tools_wildcard_survives_parse() {
    let b = parse("plugin: telegram\nallowed_tools: [\"*\"]\n");
    assert_eq!(b.allowed_tools.as_deref(), Some(&["*".to_string()][..]));
}

#[test]
fn unknown_field_under_binding_rejected() {
    let yaml = "plugin: telegram\nbogus_field: 1\n";
    let err = serde_yaml::from_str::<InboundBinding>(yaml)
        .expect_err("deny_unknown_fields should reject `bogus_field`");
    let msg = err.to_string();
    assert!(
        msg.contains("bogus_field") || msg.contains("unknown field"),
        "error should mention the unknown field, got: {msg}"
    );
}
