use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub broker: BrokerInner,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerInner {
    #[serde(rename = "type")]
    pub kind: BrokerKind,
    // Phase 92 — `url` is no longer required for every broker
    // kind. `BrokerKind::StdioBridge` (a daemon-derived value
    // that the operator never picks in YAML) inherits its
    // transport from the parent process's stdin/stdout, so no
    // url applies. `Local` also ignores it. `Nats` still requires
    // a non-empty value; that's enforced at the `AnyBroker::
    // from_config` layer, not here, so YAML omitting `url:`
    // round-trips cleanly for the other kinds.
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub auth: BrokerAuthConfig,
    #[serde(default)]
    pub persistence: BrokerPersistenceConfig,
    #[serde(default)]
    pub limits: BrokerLimitsConfig,
    #[serde(default)]
    pub fallback: BrokerFallbackConfig,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum BrokerKind {
    #[default]
    Local,
    Nats,
    // Phase 92 — pipes broker traffic over the parent daemon's
    // stdin/stdout JSON-RPC channel. NOT operator-set in YAML;
    // the daemon derives it for every subprocess plugin it
    // spawns when its own broker is `Local`, so the plugin
    // reaches the host broker without a network hop. Override
    // the lowercase auto-rename so the wire string is the
    // snake_case form `"stdio_bridge"` and not `"stdiobridge"`.
    #[serde(rename = "stdio_bridge")]
    StdioBridge,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerAuthConfig {
    #[serde(default)]
    pub enabled: bool,
    pub nkey_file: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerPersistenceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_queue_path")]
    pub path: String,
}

fn default_queue_path() -> String {
    "./data/queue".to_string()
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerLimitsConfig {
    #[serde(default = "default_max_payload")]
    pub max_payload: String,
    #[serde(default = "default_max_pending")]
    pub max_pending: usize,
}

fn default_max_payload() -> String {
    "4MB".to_string()
}
fn default_max_pending() -> usize {
    10_000
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrokerFallbackConfig {
    #[serde(default = "default_fallback_mode")]
    pub mode: String,
    #[serde(default = "default_drain_on_reconnect")]
    pub drain_on_reconnect: bool,
}

fn default_fallback_mode() -> String {
    "local_queue".to_string()
}
fn default_drain_on_reconnect() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> BrokerConfig {
        serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("parse: {e}\nyaml:\n{yaml}"))
    }

    #[test]
    fn broker_kind_local_parses() {
        let cfg = parse("broker:\n  type: local\n  url: \"\"\n");
        assert_eq!(cfg.broker.kind, BrokerKind::Local);
    }

    #[test]
    fn broker_kind_nats_parses() {
        let cfg = parse("broker:\n  type: nats\n  url: \"nats://127.0.0.1:4222\"\n");
        assert_eq!(cfg.broker.kind, BrokerKind::Nats);
        assert_eq!(cfg.broker.url, "nats://127.0.0.1:4222");
    }

    // Phase 92 — `stdio_bridge` is the new variant; verifies the
    // explicit `#[serde(rename = "stdio_bridge")]` is honored so
    // the wire string is snake_case rather than the lowercase
    // default that `#[serde(rename_all = "lowercase")]` would
    // produce (`stdiobridge`).
    #[test]
    fn broker_kind_stdio_bridge_parses() {
        let cfg = parse("broker:\n  type: stdio_bridge\n");
        assert_eq!(cfg.broker.kind, BrokerKind::StdioBridge);
        assert!(
            cfg.broker.url.is_empty(),
            "stdio_bridge omits url; default empty string expected"
        );
    }

    // Phase 92 — guard against the auto-lowercase rename leaking
    // a second wire alias. `stdiobridge` should NOT parse so
    // operators don't accidentally pick this kind via a typo.
    #[test]
    fn broker_kind_rejects_lowercase_concatenated_form() {
        let result: Result<BrokerConfig, _> =
            serde_yaml::from_str("broker:\n  type: stdiobridge\n");
        assert!(
            result.is_err(),
            "stdiobridge must not match; only stdio_bridge is valid"
        );
    }

    #[test]
    fn broker_kind_rejects_unknown_variant() {
        let result: Result<BrokerConfig, _> =
            serde_yaml::from_str("broker:\n  type: redis\n");
        assert!(result.is_err());
    }

    // Phase 92 — `url` is now `#[serde(default)]`. Configs that
    // omit it (legitimate for `Local` / `StdioBridge`) must
    // still round-trip; `Nats` validation happens later in
    // `AnyBroker::from_config`, not here.
    #[test]
    fn broker_inner_url_is_optional() {
        let cfg = parse("broker:\n  type: local\n");
        assert_eq!(cfg.broker.kind, BrokerKind::Local);
        assert!(cfg.broker.url.is_empty());
    }

    #[test]
    fn broker_kind_default_is_local() {
        assert_eq!(BrokerKind::default(), BrokerKind::Local);
    }
}
