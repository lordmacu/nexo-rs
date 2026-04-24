//! Registry payloads exchanged over NATS during extension discovery.
//!
//! Subjects (under `{prefix}` from `ExtensionsConfig`, default `"ext"`):
//! - `{prefix}.registry.announce`         — extension → agent, boot broadcast
//! - `{prefix}.registry.request`          — agent → extensions, replay trigger
//! - `{prefix}.registry.heartbeat.{id}`   — extension liveness beacon
//! - `{prefix}.registry.shutdown.{id}`    — extension graceful bye

use serde::{Deserialize, Serialize};

/// Capabilities block mirrored from the extension manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnnounceCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
}

/// Highest announce schema version this code understands. Payloads
/// arriving with a higher number are logged and rejected by the
/// directory so forward-incompatible announcements surface cleanly
/// instead of being silently mis-parsed.
pub const ANNOUNCE_SCHEMA_VERSION: u32 = 1;

/// Published by an extension on boot and in response to a `RegistryRequest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnnouncePayload {
    /// Forward-compatibility marker. Legacy payloads without the field
    /// default to `1`. Extension authors bump this when the wire shape
    /// changes incompatibly; the directory rejects anything higher
    /// than [`ANNOUNCE_SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    #[serde(default = "default_prefix")]
    pub subject_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default)]
    pub capabilities: AnnounceCapabilities,
}

fn default_schema_version() -> u32 {
    1
}

/// Published by an agent on boot so any live extension re-announces itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryRequestPayload {
    pub requested_by: String,
}

/// Liveness beacon published by an extension on its heartbeat interval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatPayload {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub uptime_secs: u64,
}

/// Optional graceful-shutdown signal so the directory can drop the entry
/// without waiting for heartbeat timeout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShutdownPayload {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn default_prefix() -> String {
    "ext".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_round_trip() {
        let p = AnnouncePayload {
            schema_version: ANNOUNCE_SCHEMA_VERSION,
            id: "weather".into(),
            version: "0.3.1".into(),
            subject_prefix: "ext".into(),
            manifest_hash: Some("sha256:abc".into()),
            capabilities: AnnounceCapabilities {
                tools: vec!["get_weather".into()],
                hooks: vec!["before_message".into()],
                channels: vec![],
                providers: vec![],
            },
        };
        let j = serde_json::to_value(&p).unwrap();
        let back: AnnouncePayload = serde_json::from_value(j).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn announce_default_prefix_on_missing_field() {
        let j = serde_json::json!({ "id": "x", "version": "1.0.0" });
        let p: AnnouncePayload = serde_json::from_value(j).unwrap();
        assert_eq!(p.subject_prefix, "ext");
        assert!(p.capabilities.tools.is_empty());
    }

    #[test]
    fn announce_defaults_schema_version_to_one() {
        let j = serde_json::json!({ "id": "x", "version": "1.0.0" });
        let p: AnnouncePayload = serde_json::from_value(j).unwrap();
        assert_eq!(p.schema_version, 1);
    }

    #[test]
    fn announce_round_trip_preserves_schema_version() {
        let j = serde_json::json!({
            "id": "x", "version": "1.0.0", "schema_version": 7,
        });
        let p: AnnouncePayload = serde_json::from_value(j).unwrap();
        assert_eq!(p.schema_version, 7);
    }

    #[test]
    fn heartbeat_round_trip() {
        let h = HeartbeatPayload { id: "x".into(), version: "1.0.0".into(), uptime_secs: 42 };
        let back: HeartbeatPayload = serde_json::from_value(serde_json::to_value(&h).unwrap()).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn request_round_trip() {
        let r = RegistryRequestPayload { requested_by: "agent-uuid".into() };
        let back: RegistryRequestPayload =
            serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn shutdown_round_trip_minimal() {
        let s = ShutdownPayload { id: "x".into(), reason: None };
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("reason"));
        let back: ShutdownPayload = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
