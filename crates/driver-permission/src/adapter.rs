//! Translate `PermissionOutcome` to the JSON shape Claude Code
//! expects from a `permission_prompt_tool`.
//!
//! Anthropic wire shape (Claude Code 2.1+, verified by reproducing
//! the validator's error message — `"expected record, received
//! undefined"`):
//! - allow:  `{ "behavior": "allow", "updatedInput": object }`
//!           — `updatedInput` is REQUIRED. When the decider has no
//!           override, the server must echo the caller's original
//!           tool input. Earlier Claude builds tolerated the field
//!           being absent; 2.1 rejects with a Zod
//!           `invalid_union` / `invalid_type` error and the tool
//!           call fails server-side.
//! - deny:   `{ "behavior": "deny",  "message": string }`

use serde_json::{json, Value};

use crate::types::PermissionOutcome;

/// `original_input` is the tool input the caller proposed; it is
/// echoed into `updatedInput` when the decider has no override so
/// the response satisfies Claude's strict schema.
pub fn outcome_to_claude_value(outcome: &PermissionOutcome, original_input: &Value) -> Value {
    match outcome {
        PermissionOutcome::AllowOnce { updated_input }
        | PermissionOutcome::AllowSession { updated_input, .. } => {
            let echo = updated_input
                .clone()
                .unwrap_or_else(|| original_input.clone());
            // Claude rejects `updatedInput` when it is not a record;
            // fall back to an empty object if the caller sent
            // something pathological (null / array / scalar).
            let echo = if echo.is_object() { echo } else { json!({}) };
            json!({ "behavior": "allow", "updatedInput": echo })
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
    fn allow_once_without_updated_input_echoes_original() {
        let original = json!({"path": "/tmp/x", "content": "hi"});
        let v = outcome_to_claude_value(
            &PermissionOutcome::AllowOnce {
                updated_input: None,
            },
            &original,
        );
        assert_eq!(
            v,
            json!({"behavior":"allow","updatedInput":{"path":"/tmp/x","content":"hi"}})
        );
    }

    #[test]
    fn allow_once_with_updated_input_overrides_original() {
        let original = json!({"path": "/tmp/x"});
        let v = outcome_to_claude_value(
            &PermissionOutcome::AllowOnce {
                updated_input: Some(json!({"redacted": true})),
            },
            &original,
        );
        assert_eq!(
            v,
            json!({"behavior":"allow","updatedInput":{"redacted":true}})
        );
    }

    #[test]
    fn allow_session_maps_same_as_allow_once() {
        let original = json!({"k": "v"});
        let v = outcome_to_claude_value(
            &PermissionOutcome::AllowSession {
                scope: AllowScope::Turn,
                updated_input: None,
            },
            &original,
        );
        assert_eq!(v, json!({"behavior":"allow","updatedInput":{"k":"v"}}));
    }

    #[test]
    fn allow_with_non_record_original_falls_back_to_empty_object() {
        // Defense in depth: if the original input is somehow an
        // array or scalar, echo `{}` instead of forwarding garbage
        // that would re-trigger the "expected record" Zod error.
        let v = outcome_to_claude_value(
            &PermissionOutcome::AllowOnce {
                updated_input: None,
            },
            &json!([1, 2, 3]),
        );
        assert_eq!(v, json!({"behavior":"allow","updatedInput":{}}));
    }

    #[test]
    fn deny_keeps_message() {
        let v = outcome_to_claude_value(
            &PermissionOutcome::Deny {
                message: "destructive".into(),
            },
            &Value::Null,
        );
        assert_eq!(v, json!({"behavior":"deny","message":"destructive"}));
    }

    #[test]
    fn unavailable_maps_to_deny_with_reason_as_message() {
        let v = outcome_to_claude_value(
            &PermissionOutcome::Unavailable {
                reason: "decider down".into(),
            },
            &Value::Null,
        );
        assert_eq!(v, json!({"behavior":"deny","message":"decider down"}));
    }

    #[test]
    fn cancelled_maps_to_deny_with_canned_message() {
        let v = outcome_to_claude_value(&PermissionOutcome::Cancelled, &Value::Null);
        assert_eq!(v, json!({"behavior":"deny","message":"goal cancelled"}));
    }
}
