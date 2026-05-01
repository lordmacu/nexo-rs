//! Phase 82.10.f — `nexo/admin/channels/*` wire types (Phase 80.9
//! MCP-channel servers in `agents.yaml.<id>.channels.approved`).
//!
//! Distinct from `credentials/*` (Phase 17 native-channel
//! credentials). Channels here are MCP server adapters
//! (slack / telegram-bridge / iMessage relay) the operator
//! pre-approves per agent.

use serde::{Deserialize, Serialize};

/// Filter for `channels/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ChannelsListFilter {
    /// Restrict to a single agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// One MCP-channel approval row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelEntry {
    /// Agent id this channel is approved for.
    pub agent_id: String,
    /// MCP server name as advertised at runtime
    /// (e.g. `plugin:telegram:tg`, `slack`).
    pub server_name: String,
    /// Optional binding indices restricting which agent bindings
    /// can use this channel. `None` = all bindings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<usize>>,
}

/// Response for `channels/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelsListResponse {
    /// Matching entries in stable order
    /// (alpha agent_id, then server_name).
    pub entries: Vec<ChannelEntry>,
}

/// Params for `channels/approve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelsApproveInput {
    /// Agent id.
    pub agent_id: String,
    /// MCP server name.
    pub server_name: String,
    /// Optional binding allowlist. `None` = all bindings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowlist: Option<Vec<usize>>,
}

/// Params for `channels/revoke`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelsRevokeParams {
    /// Agent id.
    pub agent_id: String,
    /// MCP server name to remove.
    pub server_name: String,
}

/// Response for `channels/revoke`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelsRevokeResponse {
    /// `true` when the yaml entry was removed.
    pub removed: bool,
}

/// Params for `channels/doctor`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ChannelsDoctorParams {
    /// Optional agent id to scope the report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Optional binding index to scope further.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_index: Option<usize>,
}

/// One verdict in the doctor report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelDoctorVerdict {
    /// Agent id.
    pub agent_id: String,
    /// MCP server name being verified.
    pub server_name: String,
    /// `"ok"` | `"warning"` | `"error"`.
    pub status: String,
    /// Operator-readable message.
    pub message: String,
}

/// Response for `channels/doctor`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ChannelDoctorReport {
    /// One verdict per (agent, server_name) pair examined.
    pub verdicts: Vec<ChannelDoctorVerdict>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_entry_round_trip() {
        let e = ChannelEntry {
            agent_id: "ana".into(),
            server_name: "plugin:telegram:tg".into(),
            allowlist: Some(vec![0, 1]),
        };
        let v = serde_json::to_value(&e).unwrap();
        let back: ChannelEntry = serde_json::from_value(v).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn channel_entry_skips_none_allowlist() {
        let e = ChannelEntry {
            agent_id: "ana".into(),
            server_name: "slack".into(),
            allowlist: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("allowlist"));
    }
}
