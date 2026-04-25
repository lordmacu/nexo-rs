//! Stable text representation of `Decision`s and `PermissionRequest`s
//! for embedding. Critical that requests and decisions share the same
//! template so cosine/L2 distances reflect semantic similarity, not
//! template noise.

use nexo_driver_permission::PermissionRequest;
use nexo_driver_types::{Decision, DecisionChoice};

const MAX_INPUT_CHARS: usize = 512;

pub(crate) fn decision_to_text(d: &Decision) -> String {
    let canonical = canonical_input(&d.input);
    let truncated = truncate_chars(&canonical, MAX_INPUT_CHARS);
    let (choice_label, message) = match &d.choice {
        DecisionChoice::Allow => ("allow", String::new()),
        DecisionChoice::Deny { message } => ("deny", format!(" — {message}")),
        DecisionChoice::Observe { note } => ("observe", format!(" — {note}")),
    };
    format!(
        "Tool: {tool}\nInput: {input}\nChoice: {choice}{msg}\nRationale: {rationale}",
        tool = d.tool,
        input = truncated,
        choice = choice_label,
        msg = message,
        rationale = d.rationale,
    )
}

pub(crate) fn request_to_text(req: &PermissionRequest) -> String {
    let canonical = canonical_input(&req.input);
    let truncated = truncate_chars(&canonical, MAX_INPUT_CHARS);
    format!(
        "Tool: {tool}\nInput: {input}\nChoice: ?\nRationale:",
        tool = req.tool_name,
        input = truncated,
    )
}

/// Canonicalise a JSON value: object keys sorted recursively, then
/// rendered as compact JSON. Equal logical values produce identical
/// strings regardless of source key order.
pub(crate) fn canonical_input(v: &serde_json::Value) -> String {
    serde_json::to_string(&sort_keys(v.clone())).unwrap_or_default()
}

fn sort_keys(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::new();
            for (k, val) in entries {
                out.insert(k, sort_keys(val));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sort_keys).collect())
        }
        other => other,
    }
}

/// Truncate to at most `n` chars at a UTF-8 char boundary.
pub(crate) fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nexo_driver_types::{Decision, DecisionId, GoalId};
    use uuid::Uuid;

    fn d(choice: DecisionChoice, tool: &str, input: serde_json::Value) -> Decision {
        Decision {
            id: DecisionId(Uuid::nil()),
            goal_id: GoalId(Uuid::nil()),
            turn_index: 0,
            tool: tool.into(),
            input,
            choice,
            rationale: "ok".into(),
            decided_at: Utc::now(),
        }
    }

    #[test]
    fn decision_to_text_includes_each_section() {
        let dec = d(
            DecisionChoice::Allow,
            "Edit",
            serde_json::json!({"file":"x"}),
        );
        let s = decision_to_text(&dec);
        assert!(s.contains("Tool: Edit"));
        assert!(s.contains("Input: "));
        assert!(s.contains("Choice: allow"));
        assert!(s.contains("Rationale: ok"));
    }

    #[test]
    fn request_to_text_uses_question_mark_choice() {
        let req = PermissionRequest {
            goal_id: GoalId::new(),
            tool_use_id: "tu".into(),
            tool_name: "Bash".into(),
            input: serde_json::json!({"cmd":"ls"}),
            metadata: serde_json::Map::new(),
        };
        let s = request_to_text(&req);
        assert!(s.contains("Tool: Bash"));
        assert!(s.contains("Choice: ?"));
    }

    #[test]
    fn canonical_input_is_key_order_invariant() {
        let a = serde_json::json!({"a": 1, "b": [{"x":1, "y":2}]});
        let b = serde_json::json!({"b": [{"y":2, "x":1}], "a": 1});
        assert_eq!(canonical_input(&a), canonical_input(&b));
    }

    #[test]
    fn truncate_chars_safely_at_unicode_boundary() {
        let s = "ñañañañaña";
        let t = truncate_chars(s, 4);
        assert_eq!(t.chars().count(), 4);
        assert!(s.starts_with(&t));
    }
}
