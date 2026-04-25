//! Wire types for the permission flow. All `Serialize + Deserialize`
//! so they survive a NATS round-trip when 67.4 routes decisions
//! between the bin and the daemon.

use nexo_driver_types::GoalId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Goal id this request belongs to. The driver loop populates
    /// it on spawn (Phase 67.4); 67.3 standalone uses a fresh nil-ish
    /// id.
    #[serde(default = "GoalId::new")]
    pub goal_id: GoalId,
    /// Claude's `tool_use_id`. Opaque from our side. Used to correlate
    /// the response back to the calling assistant turn.
    #[serde(default)]
    pub tool_use_id: String,
    /// `"Edit"`, `"Bash"`, `"Write"`, ...
    pub tool_name: String,
    /// Tool input as Claude proposed it.
    #[serde(default)]
    pub input: serde_json::Value,
    /// Free-form context the bin or driver attaches.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    AllowOnce {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
    },
    AllowSession {
        scope: AllowScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
    },
    Deny {
        message: String,
    },
    Unavailable {
        reason: String,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowScope {
    Turn,
    Session,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub tool_use_id: String,
    pub outcome: PermissionOutcome,
    /// Decider's natural-language rationale — logged, never surfaced
    /// to Claude. Empty string is fine.
    #[serde(default)]
    pub rationale: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt<T: Serialize + for<'de> Deserialize<'de>>(v: &T) -> T {
        serde_json::from_str(&serde_json::to_string(v).unwrap()).unwrap()
    }

    #[test]
    fn roundtrip_outcome_each_variant() {
        for outcome in [
            PermissionOutcome::AllowOnce {
                updated_input: None,
            },
            PermissionOutcome::AllowOnce {
                updated_input: Some(serde_json::json!({"x": 1})),
            },
            PermissionOutcome::AllowSession {
                scope: AllowScope::Session,
                updated_input: None,
            },
            PermissionOutcome::AllowSession {
                scope: AllowScope::Turn,
                updated_input: None,
            },
            PermissionOutcome::Deny {
                message: "no".into(),
            },
            PermissionOutcome::Unavailable {
                reason: "timeout".into(),
            },
            PermissionOutcome::Cancelled,
        ] {
            let back = rt(&outcome);
            assert_eq!(back, outcome);
        }
    }

    #[test]
    fn roundtrip_request_with_defaults() {
        let raw = r#"{"tool_name":"Edit"}"#;
        let req: PermissionRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.tool_name, "Edit");
        assert_eq!(req.tool_use_id, "");
        assert!(req.input.is_null());
        assert!(req.metadata.is_empty());
    }

    #[test]
    fn outcome_uses_tag_field() {
        let s = serde_json::to_string(&PermissionOutcome::Cancelled).unwrap();
        assert_eq!(s, r#"{"outcome":"cancelled"}"#);
    }
}
