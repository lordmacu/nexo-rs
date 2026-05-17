//! Outbound delivery helper — channel-agnostic shape pollers can reuse
//! to describe "send this to that channel". Pure data + render helper.
//!
//! Lifted from the (now-deleted) `builtins::gmail::DeliverCfg` so
//! third-party pollers can compose against a public shape without
//! recreating it. The poller crate stays free of channel enums — this
//! shape only carries the strings; topic routing happens via
//! [`PollerHost::broker_publish`] downstream.

use serde::{Deserialize, Serialize};

/// Operator-facing `deliver:` block in a poller's per-job YAML config.
///
/// The `to` field accepts the alias `recipient:` so older YAML configs
/// that picked the alternate spelling continue to load.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverCfg {
    /// Channel slug — `"whatsapp"`, `"telegram"`, `"google"`, …
    /// Pollers map this to whatever topic / handle their host knows
    /// about; no enum here so a new channel never requires a code
    /// change in this crate.
    pub channel: String,

    /// Recipient identifier — JID, chat id, phone, email. Passed
    /// through verbatim to the outbound payload.
    #[serde(alias = "recipient")]
    pub to: String,
}

/// Tiny mustache-light template renderer: substitutes `{field_name}`
/// from a flat JSON object. Non-string fields are stringified via
/// `Display`. Unknown placeholders are left intact. Used by pollers
/// that ship `message_template` config knobs (webhook_poll, rss,
/// custom 3rd-party pollers).
pub fn render_template(template: &str, item: &serde_json::Value) -> String {
    let mut out = template.to_string();
    if let serde_json::Value::Object(map) = item {
        for (k, v) in map {
            let needle = format!("{{{k}}}");
            let replacement = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out = out.replace(&needle, &replacement);
        }
    }
    out = out.replace("{json}", &item.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_canonical_to() {
        let cfg: DeliverCfg =
            serde_json::from_value(json!({ "channel": "whatsapp", "to": "57300" })).unwrap();
        assert_eq!(cfg.channel, "whatsapp");
        assert_eq!(cfg.to, "57300");
    }

    #[test]
    fn parses_recipient_alias() {
        let cfg: DeliverCfg =
            serde_json::from_value(json!({ "channel": "telegram", "recipient": "-100" })).unwrap();
        assert_eq!(cfg.to, "-100");
    }

    #[test]
    fn rejects_unknown_field() {
        let r: Result<DeliverCfg, _> =
            serde_json::from_value(json!({ "channel": "x", "to": "y", "bogus": 1 }));
        assert!(r.is_err());
    }

    #[test]
    fn render_substitutes_string_fields() {
        let item = json!({ "name": "Ana", "phone": "+57" });
        assert_eq!(
            render_template("Hi {name} ({phone})", &item),
            "Hi Ana (+57)"
        );
    }

    #[test]
    fn render_stringifies_non_string_fields() {
        let item = json!({ "count": 42, "active": true });
        let out = render_template("{count} items, active={active}", &item);
        assert_eq!(out, "42 items, active=true");
    }

    #[test]
    fn render_leaves_unknown_placeholders_intact() {
        let item = json!({ "x": "y" });
        assert_eq!(render_template("hello {missing}", &item), "hello {missing}");
    }

    #[test]
    fn render_json_placeholder_dumps_whole_item() {
        let item = json!({ "a": 1 });
        let out = render_template("debug={json}", &item);
        assert!(out.starts_with("debug={"));
        assert!(out.contains("\"a\":1"));
    }
}
