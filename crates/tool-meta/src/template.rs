//! Minimal mustache-lite template renderer.
//!
//! Supports a single feature: `{{path.to.field}}` substitution
//! against a [`serde_json::Value`] context. No conditionals, no
//! loops, no escape syntax — operators write 5–20 character
//! templates in YAML, this is enough to cover them.
//!
//! Missing paths render as the literal `<missing>` placeholder
//! instead of failing, so a slightly-misnamed key in the operator
//! config doesn't crash the event-subscriber loop.

use serde_json::Value;

/// Placeholder rendered when a `{{path}}` expression resolves to
/// a missing/null/non-leaf value.
pub const MISSING_PLACEHOLDER: &str = "<missing>";

/// Render `template` against `context`, expanding every
/// `{{path.to.field}}` span. Unmatched/empty `{{ }}` is preserved
/// verbatim so a literal pair of braces survives if the operator
/// genuinely wanted them in the body.
///
/// # Example
///
/// ```
/// use nexo_tool_meta::render_template;
///
/// let ctx = serde_json::json!({
///     "event_kind": "pull_request",
///     "body_json": { "repository": { "full_name": "anthropic/repo" } }
/// });
/// let out = render_template(
///     "GitHub {{event_kind}}: {{body_json.repository.full_name}}",
///     &ctx,
/// );
/// assert_eq!(out, "GitHub pull_request: anthropic/repo");
/// ```
pub fn render_template(template: &str, context: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `{{`.
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Find the closing `}}`.
            if let Some(end) = find_close(bytes, i + 2) {
                let raw = &template[i + 2..end];
                let path = raw.trim();
                if path.is_empty() {
                    // Literal `{{ }}` — preserve verbatim.
                    out.push_str(&template[i..end + 2]);
                } else {
                    out.push_str(&resolve_path(context, path));
                }
                i = end + 2;
                continue;
            }
        }
        // Single byte literal.
        out.push(template[i..].chars().next().unwrap());
        i += template[i..].chars().next().unwrap().len_utf8();
    }
    out
}

/// Walk a dotted JSON path against `context`. Numeric segments
/// index into arrays. Returns the resolved leaf value as its
/// natural string form (string verbatim, number/bool/null via
/// `to_string`), or [`MISSING_PLACEHOLDER`] when any segment is
/// missing or the leaf isn't a renderable scalar.
fn resolve_path(context: &Value, path: &str) -> String {
    let mut current = context;
    for segment in path.split('.') {
        if segment.is_empty() {
            return MISSING_PLACEHOLDER.to_string();
        }
        current = match (current, segment.parse::<usize>()) {
            (Value::Object(map), _) => match map.get(segment) {
                Some(v) => v,
                None => return MISSING_PLACEHOLDER.to_string(),
            },
            (Value::Array(items), Ok(idx)) => match items.get(idx) {
                Some(v) => v,
                None => return MISSING_PLACEHOLDER.to_string(),
            },
            _ => return MISSING_PLACEHOLDER.to_string(),
        };
    }
    match current {
        Value::String(s) => s.clone(),
        Value::Null => MISSING_PLACEHOLDER.to_string(),
        // Number / Bool render via their natural Display.
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        // Object / Array don't render — too easy to leak structure.
        Value::Object(_) | Value::Array(_) => MISSING_PLACEHOLDER.to_string(),
    }
}

fn find_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_passthrough_no_placeholders() {
        let ctx = serde_json::json!({});
        assert_eq!(render_template("hello world", &ctx), "hello world");
    }

    #[test]
    fn single_substitution() {
        let ctx = serde_json::json!({ "name": "ana" });
        assert_eq!(render_template("hi {{name}}", &ctx), "hi ana");
    }

    #[test]
    fn deep_path() {
        let ctx = serde_json::json!({
            "body_json": {
                "repository": { "full_name": "anthropic/repo" }
            }
        });
        let out = render_template("repo: {{body_json.repository.full_name}}", &ctx);
        assert_eq!(out, "repo: anthropic/repo");
    }

    #[test]
    fn missing_path_renders_placeholder() {
        let ctx = serde_json::json!({ "body": {} });
        assert_eq!(
            render_template("nope: {{body.does.not.exist}}", &ctx),
            "nope: <missing>"
        );
    }

    #[test]
    fn array_index_path() {
        let ctx = serde_json::json!({
            "tags": ["alpha", "beta", "gamma"]
        });
        assert_eq!(render_template("first: {{tags.0}}", &ctx), "first: alpha");
        assert_eq!(render_template("third: {{tags.2}}", &ctx), "third: gamma");
        assert_eq!(
            render_template("oob: {{tags.99}}", &ctx),
            "oob: <missing>"
        );
    }

    #[test]
    fn null_leaf_renders_placeholder() {
        let ctx = serde_json::json!({ "value": null });
        assert_eq!(
            render_template("v={{value}}", &ctx),
            "v=<missing>"
        );
    }

    #[test]
    fn unmatched_braces_preserved_literal() {
        let ctx = serde_json::json!({});
        // No closing `}}` — leaves the opening `{{` verbatim.
        assert_eq!(
            render_template("text {{ never closes", &ctx),
            "text {{ never closes"
        );
    }

    #[test]
    fn empty_template_returns_empty() {
        let ctx = serde_json::json!({});
        assert_eq!(render_template("", &ctx), "");
    }

    #[test]
    fn empty_braces_preserved_verbatim() {
        let ctx = serde_json::json!({ "x": "y" });
        // `{{ }}` with nothing inside — operator either typo'd or
        // genuinely wanted braces. Keep verbatim.
        assert_eq!(render_template("a {{ }} b", &ctx), "a {{ }} b");
    }

    #[test]
    fn number_and_bool_leaves_render() {
        let ctx = serde_json::json!({
            "count": 42,
            "enabled": true
        });
        assert_eq!(
            render_template("c={{count}} e={{enabled}}", &ctx),
            "c=42 e=true"
        );
    }

    #[test]
    fn object_leaf_renders_placeholder_not_json() {
        // Don't leak struct shape via accidental JSON-string in the
        // body — render `<missing>` and let the operator fix the
        // path explicitly.
        let ctx = serde_json::json!({ "obj": { "a": 1 } });
        assert_eq!(
            render_template("o={{obj}}", &ctx),
            "o=<missing>"
        );
    }
}
