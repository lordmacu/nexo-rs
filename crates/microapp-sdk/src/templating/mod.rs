//! Operator template library + snippets (M15.23.b).
//!
//! Two layers on top of the workspace's `render_template`:
//!
//! - [`Template`] — a named, operator-authored template.
//!   The microapp resolves a template by id, builds a
//!   context, calls [`render`] (or [`render_template`]
//!   directly).
//! - [`Snippet`] — a short, reusable phrase the operator
//!   inserts inline in the draft editor. Same renderer
//!   under the hood; the snippet just carries a shorter
//!   body + an optional shortcut key.
//!
//! ## Sandboxing
//!
//! The renderer is sandboxed by construction:
//!
//! - No helpers / no eval / no filesystem / no network.
//! - Only string interpolation against a `serde_json::Value`
//!   context (dotted paths + array indexing).
//! - Object / array leaves render `<missing>` instead of
//!   leaking JSON structure.
//!
//! Future: a `templating-handlebars` feature could pull
//! `handlebars` for full conditionals + loops behind the
//! same trait. Today's slice doesn't need that — operator
//! templates land in the 5–50 char range.

use serde::{Deserialize, Serialize};

pub use nexo_tool_meta::template::{render_template, MISSING_PLACEHOLDER};

#[cfg(feature = "templating-handlebars")]
pub mod handlebars;

/// Named operator template. Stable id for analytics joins +
/// admin URL paths; human-facing label for the picker; body
/// holds the `{{path}}` substitution string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    /// Stable machine id. Convention: `snake_case`.
    pub id: String,
    /// Operator-facing name shown in the picker.
    pub name: String,
    /// Optional one-line description of what the template
    /// covers (when to use, what context fields to wire).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Mustache-lite body. `{{person.name}}`, `{{seller.signature}}`,
    /// etc. Whatever the caller's context exposes.
    pub body: String,
}

/// Short reusable snippet for inline insertion in the draft
/// editor. Same renderer as [`Template`]; the separation is
/// UX-driven (snippets show in a different picker, support
/// keyboard shortcuts, render shorter labels).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// Stable machine id.
    pub id: String,
    /// Operator-facing label.
    pub name: String,
    /// Optional keyboard shortcut hint (`/saludo`,
    /// `/firma`). Frontend renders next to the picker entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// Mustache-lite body — same renderer as `Template`.
    pub body: String,
}

/// Convenience: render a [`Template`] against a context.
/// Identical to `render_template(&t.body, ctx)` — exists so
/// callers can pass the typed shape directly + the function
/// reads at the call site.
pub fn render(template: &Template, context: &serde_json::Value) -> String {
    render_template(&template.body, context)
}

/// Convenience: render a [`Snippet`].
pub fn render_snippet(snippet: &Snippet, context: &serde_json::Value) -> String {
    render_template(&snippet.body, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn template(id: &str, body: &str) -> Template {
        Template {
            id: id.into(),
            name: id.into(),
            description: None,
            body: body.into(),
        }
    }

    fn snippet(id: &str, body: &str) -> Snippet {
        Snippet {
            id: id.into(),
            name: id.into(),
            shortcut: None,
            body: body.into(),
        }
    }

    #[test]
    fn template_renders_dotted_paths() {
        let t = template(
            "welcome",
            "Hola {{person.name}}, soy {{seller.name}}.",
        );
        let ctx = json!({
            "person": { "name": "Juan" },
            "seller": { "name": "Pedro" }
        });
        assert_eq!(render(&t, &ctx), "Hola Juan, soy Pedro.");
    }

    #[test]
    fn template_missing_path_renders_placeholder() {
        let t = template("x", "{{person.email}}");
        let ctx = json!({ "person": {} });
        assert_eq!(render(&t, &ctx), MISSING_PLACEHOLDER);
    }

    #[test]
    fn snippet_renders_same_as_template() {
        let s = snippet("saludo", "Hola {{person.name}}");
        let ctx = json!({ "person": { "name": "Ana" } });
        assert_eq!(render_snippet(&s, &ctx), "Hola Ana");
    }

    #[test]
    fn template_serde_roundtrips() {
        let t = Template {
            id: "welcome".into(),
            name: "Welcome".into(),
            description: Some("First-touch greeting".into()),
            body: "Hola {{person.name}}".into(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Template = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn snippet_skips_none_shortcut_in_serialised_form() {
        let s = snippet("x", "y");
        let json = serde_json::to_string(&s).unwrap();
        // `skip_serializing_if = "Option::is_none"` keeps the
        // YAML / JSON tidy when the shortcut isn't set.
        assert!(!json.contains("shortcut"));
    }

    #[test]
    fn snippet_shortcut_round_trips() {
        let s = Snippet {
            id: "saludo".into(),
            name: "Saludo".into(),
            shortcut: Some("/saludo".into()),
            body: "Hola".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"shortcut\":\"/saludo\""));
        let back: Snippet = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn template_description_optional() {
        let t = template("x", "y");
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("description"));
    }

    #[test]
    fn empty_body_renders_empty() {
        let t = template("x", "");
        assert_eq!(render(&t, &json!({})), "");
    }

    #[test]
    fn sandboxed_no_helpers() {
        // Defensive: confirm the renderer treats `{{#if ...}}`
        // as a substitution span, NOT a Handlebars helper.
        // Both `#if person.name` and `/if` resolve to missing
        // (no helper executes) — the literal `HI` between the
        // two `}}` `{{` boundaries survives verbatim. Future
        // Handlebars upgrade MUST land behind a feature flag,
        // never silently activate.
        let t = template("x", "{{#if person.name}}HI{{/if}}");
        let ctx = json!({ "person": { "name": "Ana" } });
        let out = render(&t, &ctx);
        // Two `<missing>` placeholders flank the literal `HI`.
        assert_eq!(
            out,
            format!("{m}HI{m}", m = MISSING_PLACEHOLDER),
        );
        // Helper-style output (e.g. Handlebars conditional
        // suppressing the literal because the test condition
        // was true) would have produced just `HI` or `<empty>`.
        // The renderer doesn't know `#if` exists.
        assert!(out.starts_with(MISSING_PLACEHOLDER));
        assert!(out.ends_with(MISSING_PLACEHOLDER));
    }

    #[test]
    fn array_index_in_template() {
        let t = template("x", "First tag: {{tags.0}}");
        let ctx = json!({ "tags": ["alpha", "beta"] });
        assert_eq!(render(&t, &ctx), "First tag: alpha");
    }
}
