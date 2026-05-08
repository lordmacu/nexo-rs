//! Sandboxed Handlebars renderer (F26).
//!
//! Strictly additive on top of the [`super::render`] mustache-
//! lite default. When operator templates outgrow simple
//! `{{path}}` substitution — they need conditionals
//! (`{{#if}}`), loops (`{{#each}}`), or scoped sub-blocks
//! (`{{#with}}`) — the caller opts into this module.
//!
//! ## Sandbox posture
//!
//! Three guards keep the renderer safe to expose to operator-
//! authored content:
//!
//! 1. **No custom helpers.** Only the built-in handlebars
//!    helpers (`if` / `else` / `unless` / `each` / `with` /
//!    `lookup` / `log`) fire. None of them touch the
//!    filesystem, network, or process state.
//! 2. **No partial loading.** `{{> partial}}` references
//!    fail at render time because no partial is registered;
//!    the `Handlebars` instance is built fresh per call so
//!    nothing leaks across operators.
//! 3. **Lenient missing-key handling.** Strict mode is OFF
//!    so a typo in `{{person.naem}}` renders an empty
//!    string instead of crashing the operator's pipeline.
//!    The mustache-lite renderer's `<missing>` placeholder
//!    convention does NOT carry over — Handlebars uses its
//!    own native semantics here so callers can pattern-
//!    match on `{{#if person.email}}` without false
//!    positives from the literal placeholder.
//!
//! ## When to use which renderer
//!
//! | Need                              | Renderer                |
//! |-----------------------------------|-------------------------|
//! | `{{path.to.field}}` only          | [`super::render`]       |
//! | Conditionals / loops / blocks     | [`render_handlebars`]   |
//! | Operator opt-in only              | always — never auto-promote |
//!
//! Default builds skip the `handlebars` dep entirely; it's
//! a heavy crate (~200 KB compiled) and most templates
//! don't need it.

use handlebars::Handlebars;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Snippet, Template};

/// Reasons the Handlebars renderer can refuse to render a
/// template. Caller maps each to a different operator UX —
/// a parse error means the operator typed bad syntax; a
/// render error means the context is missing a required
/// field that the template explicitly demands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum HandlebarsRenderError {
    /// The template body itself didn't compile (malformed
    /// `{{#if}}` / unclosed block / etc).
    #[error("handlebars template parse error: {0}")]
    Parse(String),
    /// Compiled OK but rendering against the context blew
    /// up — typically a forbidden helper invocation
    /// (`{{> partial}}` referencing an unregistered partial).
    #[error("handlebars render error: {0}")]
    Render(String),
}

/// Render a Handlebars template against `context` with the
/// sandbox guards documented at the module head.
///
/// Each call builds a fresh `Handlebars` instance with no
/// helpers / partials registered. Stateless — safe to call
/// from multiple threads.
pub fn render_handlebars(
    template: &str,
    context: &serde_json::Value,
) -> Result<String, HandlebarsRenderError> {
    let mut hb = Handlebars::new();
    // Lenient missing-key handling — operator typos render
    // empty strings instead of crashing the pipeline.
    hb.set_strict_mode(false);
    // Defense-in-depth: dev mode would auto-reload templates
    // from disk if they were registered with paths. We don't
    // register any, but flip OFF explicitly.
    hb.set_dev_mode(false);
    hb.render_template(template, context).map_err(|e| {
        // The handlebars crate fuses parse + render errors
        // through one `RenderError` type; the message text
        // distinguishes them.
        let message = e.to_string();
        if message.contains("Template")
            || message.contains("parse")
            || message.contains("syntax")
        {
            HandlebarsRenderError::Parse(message)
        } else {
            HandlebarsRenderError::Render(message)
        }
    })
}

/// Convenience: render a [`Template`] via the Handlebars
/// engine. Same shape as [`super::render`] but uses the
/// richer renderer.
pub fn render_template(
    template: &Template,
    context: &serde_json::Value,
) -> Result<String, HandlebarsRenderError> {
    render_handlebars(&template.body, context)
}

/// Convenience: render a [`Snippet`] via Handlebars.
pub fn render_snippet(
    snippet: &Snippet,
    context: &serde_json::Value,
) -> Result<String, HandlebarsRenderError> {
    render_handlebars(&snippet.body, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simple_substitution_works() {
        let out = render_handlebars(
            "Hola {{person.name}}, soy {{seller.name}}.",
            &json!({
                "person": { "name": "Juan" },
                "seller": { "name": "Pedro" }
            }),
        )
        .unwrap();
        assert_eq!(out, "Hola Juan, soy Pedro.");
    }

    #[test]
    fn if_block_branches_on_truthy_field() {
        let tmpl = "{{#if vip}}VIP{{else}}standard{{/if}}";
        assert_eq!(
            render_handlebars(tmpl, &json!({ "vip": true })).unwrap(),
            "VIP",
        );
        assert_eq!(
            render_handlebars(tmpl, &json!({ "vip": false })).unwrap(),
            "standard",
        );
        // Missing key — lenient mode treats as falsy.
        assert_eq!(
            render_handlebars(tmpl, &json!({})).unwrap(),
            "standard",
        );
    }

    #[test]
    fn unless_block_inverts_if() {
        let out = render_handlebars(
            "{{#unless paid}}reminder{{/unless}}",
            &json!({ "paid": false }),
        )
        .unwrap();
        assert_eq!(out, "reminder");
    }

    #[test]
    fn each_loops_over_array() {
        let out = render_handlebars(
            "{{#each tags}}#{{this}} {{/each}}",
            &json!({ "tags": ["alpha", "beta", "gamma"] }),
        )
        .unwrap();
        assert_eq!(out, "#alpha #beta #gamma ");
    }

    #[test]
    fn each_with_index_metadata() {
        let out = render_handlebars(
            "{{#each items}}{{@index}}={{this}};{{/each}}",
            &json!({ "items": ["a", "b", "c"] }),
        )
        .unwrap();
        assert_eq!(out, "0=a;1=b;2=c;");
    }

    #[test]
    fn with_block_scopes_lookups() {
        let out = render_handlebars(
            "{{#with person}}{{name}} <{{email}}>{{/with}}",
            &json!({ "person": { "name": "Ana", "email": "ana@x.io" } }),
        )
        .unwrap();
        assert_eq!(out, "Ana <ana@x.io>");
    }

    #[test]
    fn html_escapes_by_default() {
        // Handlebars escapes `<`, `>`, `&`, `"` by default —
        // operator-authored bodies that get rendered into a
        // mail body don't accidentally inject HTML.
        let out = render_handlebars(
            "{{body}}",
            &json!({ "body": "<script>alert(1)</script>" }),
        )
        .unwrap();
        assert!(out.contains("&lt;script&gt;"));
        assert!(!out.contains("<script>"));
    }

    #[test]
    fn triple_braces_render_raw() {
        // `{{{body}}}` opt-out of HTML escaping. Operators
        // who need raw HTML must type the explicit form, so
        // the default stays safe.
        let out = render_handlebars(
            "{{{body}}}",
            &json!({ "body": "<b>bold</b>" }),
        )
        .unwrap();
        assert_eq!(out, "<b>bold</b>");
    }

    #[test]
    fn missing_key_renders_empty_in_lenient_mode() {
        let out = render_handlebars(
            "v={{not.present}}",
            &json!({}),
        )
        .unwrap();
        assert_eq!(out, "v=");
    }

    #[test]
    fn unregistered_partial_is_render_error() {
        // Sandboxing posture — partials would let an
        // operator template reference disk paths if the
        // engine loaded them. We never register any, so
        // `{{> outside}}` fails cleanly.
        let r = render_handlebars(
            "{{> some_partial}}",
            &json!({}),
        );
        assert!(matches!(r, Err(HandlebarsRenderError::Render(_))));
    }

    #[test]
    fn malformed_template_returns_parse_error() {
        // Unclosed `{{#if}}` block.
        let r = render_handlebars("{{#if x}}only", &json!({}));
        assert!(matches!(r, Err(HandlebarsRenderError::Parse(_))));
    }

    #[test]
    fn dotted_path_resolution() {
        let out = render_handlebars(
            "company={{lead.company.name}}",
            &json!({
                "lead": { "company": { "name": "Acme" } }
            }),
        )
        .unwrap();
        assert_eq!(out, "company=Acme");
    }

    #[test]
    fn nested_if_each_compose() {
        let tmpl = "{{#each leads}}{{#if vip}}⭐{{/if}}{{name}}\n{{/each}}";
        let out = render_handlebars(
            tmpl,
            &json!({
                "leads": [
                    { "name": "Juan", "vip": true },
                    { "name": "Ana",  "vip": false }
                ]
            }),
        )
        .unwrap();
        assert_eq!(out, "⭐Juan\nAna\n");
    }

    #[test]
    fn render_template_typed_helper() {
        let t = Template {
            id: "x".into(),
            name: "x".into(),
            description: None,
            body: "Hi {{name}}".into(),
        };
        assert_eq!(
            render_template(&t, &json!({ "name": "Pedro" })).unwrap(),
            "Hi Pedro",
        );
    }

    #[test]
    fn render_snippet_typed_helper() {
        let s = Snippet {
            id: "saludo".into(),
            name: "Saludo".into(),
            shortcut: None,
            body: "{{#if formal}}Estimado{{else}}Hola{{/if}} {{name}}".into(),
        };
        assert_eq!(
            render_snippet(
                &s,
                &json!({ "formal": false, "name": "Ana" })
            )
            .unwrap(),
            "Hola Ana",
        );
    }

    #[test]
    fn empty_body_renders_empty() {
        assert_eq!(render_handlebars("", &json!({})).unwrap(), "");
    }

    #[test]
    fn array_index_via_lookup() {
        let out = render_handlebars(
            "first={{lookup tags 0}}",
            &json!({ "tags": ["alpha", "beta"] }),
        )
        .unwrap();
        assert_eq!(out, "first=alpha");
    }
}
