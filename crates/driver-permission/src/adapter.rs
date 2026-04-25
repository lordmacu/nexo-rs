//! Translate `PermissionOutcome` to the JSON shape Claude Code
//! expects from a `permission_prompt_tool`.
//!
//! Anthropic wire shape:
//! - allow:  `{ "behavior": "allow", "updatedInput"?: object }`
//! - deny:   `{ "behavior": "deny",  "message": string }`

use serde_json::{json, Value};

use crate::types::PermissionOutcome;

pub fn outcome_to_claude_value(outcome: &PermissionOutcome) -> Value {
    match outcome {
        PermissionOutcome::AllowOnce { updated_input }
        | PermissionOutcome::AllowSession { updated_input, .. } => {
            let mut obj = json!({ "behavior": "allow" });
            if let Some(ui) = updated_input {
                obj["updatedInput"] = ui.clone();
            }
            obj
        }
        PermissionOutcome::Deny { message } => {
            json!({ "behavior": "deny", "message": message })
        }
        PermissionOutcome::Unavailable { reason } => {
            json!({ "behavior": "deny", "message": reason })
        }
        PermissionOutcome::Cancelled => {
            json!({ "behavior": "deny", "message": "goal cancelled" })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AllowScope;

    #[test]
    fn allow_once_without_updated_input() {
        let v = outcome_to_claude_value(&PermissionOutcome::AllowOnce {
            updated_input: None,
        });
        assert_eq!(v, json!({"behavior":"allow"}));
    }

    #[test]
    fn allow_once_with_updated_input() {
        let v = outcome_to_claude_value(&PermissionOutcome::AllowOnce {
            updated_input: Some(json!({"redacted": true})),
        });
        assert_eq!(
            v,
            json!({"behavior":"allow","updatedInput":{"redacted":true}})
        );
    }

    #[test]
    fn allow_session_maps_same_as_allow_once() {
        let v = outcome_to_claude_value(&PermissionOutcome::AllowSession {
            scope: AllowScope::Turn,
            updated_input: None,
        });
        assert_eq!(v, json!({"behavior":"allow"}));
    }

    #[test]
    fn deny_keeps_message() {
        let v = outcome_to_claude_value(&PermissionOutcome::Deny {
            message: "destructive".into(),
        });
        assert_eq!(v, json!({"behavior":"deny","message":"destructive"}));
    }

    #[test]
    fn unavailable_maps_to_deny_with_reason_as_message() {
        let v = outcome_to_claude_value(&PermissionOutcome::Unavailable {
            reason: "decider down".into(),
        });
        assert_eq!(v, json!({"behavior":"deny","message":"decider down"}));
    }

    #[test]
    fn cancelled_maps_to_deny_with_canned_message() {
        let v = outcome_to_claude_value(&PermissionOutcome::Cancelled);
        assert_eq!(v, json!({"behavior":"deny","message":"goal cancelled"}));
    }
}
