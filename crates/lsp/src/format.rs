//! LSP-result formatters: convert raw `lsp_types` payloads into
//! `path:line:col` strings suitable for `tool_result` display.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/LSPTool/formatters.ts:24-100` —
//!     `formatUri`, group-by-file, location formatter. We mirror
//!     the `path:line:col` shape and the relative-path heuristic
//!     ("only use relative when shorter and not starting with
//!     `../../`").
//!   * `claude-code-leak/src/services/lsp/passiveFeedback.ts:18-35`
//!     — `mapLSPSeverity` (1=Error, 2=Warning, 3=Info, 4=Hint).
//!
//! Reference (secondary):
//!   * `research/src/agents/pi-bundle-lsp-runtime.ts:279-292` —
//!     OpenClaw stringifies the raw JSON. We reject that pattern;
//!     `path:line:col` is 3-5x cheaper in tokens.

use std::path::{Path, PathBuf};

/// Workspace-symbol cap on the formatted output. Matches
/// `claude-code-leak/src/tools/LSPTool/LSPTool.ts:130`'s
/// `maxResultSizeChars: 100_000`. We truncate with a `+N more`
/// note rather than splitting into chunks.
pub const MAX_FORMATTED_BYTES: usize = 100_000;

/// Convert a `file://` URL into a path string. When `workspace_root`
/// is provided AND the path lies inside it AND the relative form
/// is shorter, we render the relative form. Otherwise the absolute
/// path. On platforms with backslashes (Windows under WSL) the
/// output is normalised to forward slashes.
pub fn uri_to_path_string(uri: &url::Url, workspace_root: Option<&Path>) -> String {
    let absolute = match uri.to_file_path() {
        Ok(p) => p,
        Err(_) => return uri.as_str().to_string(),
    };
    let absolute_str = absolute.to_string_lossy().replace('\\', "/");
    let Some(root) = workspace_root else {
        return absolute_str;
    };
    let relative = match absolute.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return absolute_str,
    };
    let rel_str = relative.to_string_lossy().replace('\\', "/");
    if rel_str.len() < absolute_str.len() && !rel_str.starts_with("../../") {
        rel_str
    } else {
        absolute_str
    }
}

/// Format a single `lsp_types::Location` as `path:line:col` (1-based
/// to match editor UX, even though the LSP wire is 0-based).
pub fn format_location(location: &lsp_types::Location, workspace_root: Option<&Path>) -> String {
    // lsp-types 0.97 wraps the URI in `lsp_types::Uri`; we already
    // operate on raw JSON in the client, so format expects an
    // already-parsed `url::Url`. Convert via the lsp Uri's string
    // form.
    let uri_str = location.uri.to_string();
    let url = match url::Url::parse(&uri_str) {
        Ok(u) => u,
        Err(_) => return uri_str,
    };
    let path = uri_to_path_string(&url, workspace_root);
    let start = &location.range.start;
    format!(
        "{path}:{line}:{col}",
        line = start.line + 1,
        col = start.character + 1
    )
}

/// Format a `Hover` result into a flat string. Combines all
/// `MarkupContent` / `MarkedString` entries; preserves markdown
/// when present (the LLM is fine with markdown in tool_result).
pub fn format_hover(hover: &lsp_types::Hover) -> String {
    use lsp_types::HoverContents::*;
    match &hover.contents {
        Scalar(s) => marked_string_text(s).unwrap_or_default(),
        Array(arr) => arr
            .iter()
            .filter_map(marked_string_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Markup(m) => m.value.clone(),
    }
}

fn marked_string_text(s: &lsp_types::MarkedString) -> Option<String> {
    match s {
        lsp_types::MarkedString::String(s) => Some(s.clone()),
        lsp_types::MarkedString::LanguageString(ls) => {
            if ls.language.is_empty() {
                Some(ls.value.clone())
            } else {
                Some(format!("```{}\n{}\n```", ls.language, ls.value))
            }
        }
    }
}

/// Format a list of `Location`s grouped by file. Each line is
/// `path:line:col`; group separator is a blank line. Truncates at
/// [`MAX_FORMATTED_BYTES`] with a `+N more` note.
pub fn format_locations(
    locations: &[lsp_types::Location],
    workspace_root: Option<&Path>,
) -> String {
    if locations.is_empty() {
        return "(no results)".to_string();
    }
    let mut by_file: std::collections::BTreeMap<String, Vec<&lsp_types::Location>> =
        Default::default();
    for loc in locations {
        let uri_str = loc.uri.to_string();
        let path = match url::Url::parse(&uri_str) {
            Ok(u) => uri_to_path_string(&u, workspace_root),
            Err(_) => uri_str,
        };
        by_file.entry(path).or_default().push(loc);
    }
    let mut out = String::new();
    let mut emitted = 0usize;
    let total = locations.len();
    for (file, locs) in &by_file {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(file);
        out.push('\n');
        for loc in locs {
            let line = loc.range.start.line + 1;
            let col = loc.range.start.character + 1;
            let entry = format!("  {file}:{line}:{col}\n", file = file, line = line, col = col);
            if out.len() + entry.len() > MAX_FORMATTED_BYTES {
                let remaining = total - emitted;
                out.push_str(&format!("  +{remaining} more\n"));
                return out.trim_end().to_string();
            }
            out.push_str(&entry);
            emitted += 1;
        }
    }
    out.trim_end().to_string()
}

/// Format `WorkspaceSymbol` (or legacy `SymbolInformation`) entries.
pub fn format_workspace_symbols(
    symbols: &[lsp_types::SymbolInformation],
    workspace_root: Option<&Path>,
) -> String {
    if symbols.is_empty() {
        return "(no symbols)".to_string();
    }
    let mut out = String::new();
    for sym in symbols {
        let uri_str = sym.location.uri.to_string();
        let path = match url::Url::parse(&uri_str) {
            Ok(u) => uri_to_path_string(&u, workspace_root),
            Err(_) => uri_str,
        };
        let line = sym.location.range.start.line + 1;
        let col = sym.location.range.start.character + 1;
        let kind = symbol_kind_name(sym.kind);
        let entry = format!("[{kind}] {} — {path}:{line}:{col}\n", sym.name);
        if out.len() + entry.len() > MAX_FORMATTED_BYTES {
            let remaining = symbols.len() - out.lines().count();
            out.push_str(&format!("+{remaining} more\n"));
            break;
        }
        out.push_str(&entry);
    }
    out.trim_end().to_string()
}

/// Format diagnostics for a single file. Each line:
/// `path:line:col [Severity] (source) message`.
/// Severity mapping: 1→Error, 2→Warning, 3→Info, 4→Hint
/// (`claude-code-leak/src/services/lsp/passiveFeedback.ts:18-35`).
pub fn format_diagnostics(
    file_path: &Path,
    workspace_root: Option<&Path>,
    diagnostics: &[lsp_types::Diagnostic],
) -> String {
    if diagnostics.is_empty() {
        return "(no diagnostics)".to_string();
    }
    let path = display_path(file_path, workspace_root);
    let mut out = String::new();
    for d in diagnostics {
        let line = d.range.start.line + 1;
        let col = d.range.start.character + 1;
        let sev = severity_name(d.severity);
        let source = d
            .source
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{path}:{line}:{col} [{sev}]{source} {}\n",
            d.message
        ));
    }
    out.trim_end().to_string()
}

/// Stable severity label. Defaults to `Error` when the server
/// omits the field, mirroring the leak's
/// `passiveFeedback.ts:32-34`.
pub fn severity_name(sev: Option<lsp_types::DiagnosticSeverity>) -> &'static str {
    match sev {
        Some(lsp_types::DiagnosticSeverity::ERROR) | None => "Error",
        Some(lsp_types::DiagnosticSeverity::WARNING) => "Warning",
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => "Info",
        Some(lsp_types::DiagnosticSeverity::HINT) => "Hint",
        Some(_) => "Error",
    }
}

fn symbol_kind_name(kind: lsp_types::SymbolKind) -> &'static str {
    use lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "File",
        K::MODULE => "Module",
        K::NAMESPACE => "Namespace",
        K::PACKAGE => "Package",
        K::CLASS => "Class",
        K::METHOD => "Method",
        K::PROPERTY => "Property",
        K::FIELD => "Field",
        K::CONSTRUCTOR => "Constructor",
        K::ENUM => "Enum",
        K::INTERFACE => "Interface",
        K::FUNCTION => "Function",
        K::VARIABLE => "Variable",
        K::CONSTANT => "Constant",
        K::STRING => "String",
        K::NUMBER => "Number",
        K::BOOLEAN => "Boolean",
        K::ARRAY => "Array",
        K::OBJECT => "Object",
        K::KEY => "Key",
        K::NULL => "Null",
        K::ENUM_MEMBER => "EnumMember",
        K::STRUCT => "Struct",
        K::EVENT => "Event",
        K::OPERATOR => "Operator",
        K::TYPE_PARAMETER => "TypeParameter",
        _ => "Symbol",
    }
}

fn display_path(p: &Path, workspace_root: Option<&Path>) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(root) = workspace_root {
        root.join(p)
    } else {
        p.to_path_buf()
    };
    let abs_str = abs.to_string_lossy().replace('\\', "/");
    if let Some(root) = workspace_root {
        if let Ok(rel) = abs.strip_prefix(root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if rel_str.len() < abs_str.len() && !rel_str.starts_with("../../") {
                return rel_str;
            }
        }
    }
    abs_str
}

/// Convenience wrapper used by the session to bundle the formatted
/// + structured outputs.
pub fn build_output(formatted: String, structured: serde_json::Value) -> crate::types::LspToolOutput {
    crate::types::LspToolOutput {
        formatted,
        structured,
    }
}

/// Helper: build an absolute `Path` from a string the model passed
/// in, resolving against the workspace_root if relative.
pub fn resolve_input_path(input: &str, workspace_root: &Path) -> PathBuf {
    let p = Path::new(input);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Diagnostic, DiagnosticSeverity, Location, Position, Range, SymbolInformation};

    fn loc(uri: &str, line: u32, character: u32) -> Location {
        Location {
            uri: lsp_types::Uri::from_str(uri).unwrap(),
            range: Range {
                start: Position { line, character },
                end: Position {
                    line,
                    character: character + 1,
                },
            },
        }
    }

    use std::str::FromStr;

    #[test]
    fn uri_to_path_string_within_workspace_uses_relative() {
        let url = url::Url::parse("file:///work/proj/src/foo.rs").unwrap();
        let root = std::path::Path::new("/work/proj");
        let s = uri_to_path_string(&url, Some(root));
        assert_eq!(s, "src/foo.rs");
    }

    #[test]
    fn uri_to_path_string_outside_workspace_keeps_absolute() {
        let url = url::Url::parse("file:///opt/lib/x.rs").unwrap();
        let root = std::path::Path::new("/work/proj");
        let s = uri_to_path_string(&url, Some(root));
        assert_eq!(s, "/opt/lib/x.rs");
    }

    #[test]
    fn uri_to_path_string_no_root_keeps_absolute() {
        let url = url::Url::parse("file:///work/proj/src/foo.rs").unwrap();
        let s = uri_to_path_string(&url, None);
        assert_eq!(s, "/work/proj/src/foo.rs");
    }

    #[test]
    fn format_location_uses_1_based_position() {
        let location = loc("file:///work/proj/src/foo.rs", 41, 7);
        let s = format_location(&location, Some(std::path::Path::new("/work/proj")));
        assert_eq!(s, "src/foo.rs:42:8");
    }

    #[test]
    fn format_locations_groups_by_file() {
        let locs = vec![
            loc("file:///work/proj/a.rs", 0, 0),
            loc("file:///work/proj/a.rs", 5, 0),
            loc("file:///work/proj/b.rs", 0, 0),
        ];
        let s = format_locations(&locs, Some(std::path::Path::new("/work/proj")));
        assert!(s.contains("a.rs\n  a.rs:1:1\n  a.rs:6:1"));
        assert!(s.contains("b.rs"));
    }

    #[test]
    fn format_locations_empty_returns_no_results() {
        assert_eq!(format_locations(&[], None), "(no results)");
    }

    #[test]
    fn format_hover_extracts_markup() {
        let h = lsp_types::Hover {
            contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: "**Foo**".into(),
            }),
            range: None,
        };
        assert_eq!(format_hover(&h), "**Foo**");
    }

    #[test]
    fn format_hover_extracts_marked_string_array() {
        let h = lsp_types::Hover {
            contents: lsp_types::HoverContents::Array(vec![
                lsp_types::MarkedString::String("Type:".into()),
                lsp_types::MarkedString::LanguageString(lsp_types::LanguageString {
                    language: "rust".into(),
                    value: "fn foo()".into(),
                }),
            ]),
            range: None,
        };
        let out = format_hover(&h);
        assert!(out.contains("Type:"));
        assert!(out.contains("```rust\nfn foo()\n```"));
    }

    #[test]
    fn format_diagnostics_severity_mapping() {
        let path = std::path::Path::new("src/x.rs");
        let diags = vec![
            Diagnostic {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 1,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: "boom".into(),
                source: Some("rustc".into()),
                ..Default::default()
            },
            Diagnostic {
                range: Range {
                    start: Position {
                        line: 1,
                        character: 0,
                    },
                    end: Position {
                        line: 1,
                        character: 1,
                    },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                message: "watch out".into(),
                source: None,
                ..Default::default()
            },
        ];
        let s = format_diagnostics(path, None, &diags);
        assert!(s.contains("[Error] (rustc) boom"));
        assert!(s.contains("[Warning] watch out"));
        assert!(s.contains("src/x.rs:1:1"));
        assert!(s.contains("src/x.rs:2:1"));
    }

    #[test]
    fn format_diagnostics_empty_returns_message() {
        let s = format_diagnostics(std::path::Path::new("x.rs"), None, &[]);
        assert_eq!(s, "(no diagnostics)");
    }

    #[test]
    fn severity_name_defaults_to_error_when_missing() {
        assert_eq!(severity_name(None), "Error");
        assert_eq!(severity_name(Some(DiagnosticSeverity::ERROR)), "Error");
        assert_eq!(severity_name(Some(DiagnosticSeverity::WARNING)), "Warning");
        assert_eq!(severity_name(Some(DiagnosticSeverity::INFORMATION)), "Info");
        assert_eq!(severity_name(Some(DiagnosticSeverity::HINT)), "Hint");
    }

    #[test]
    fn format_workspace_symbols_renders_kind_and_path() {
        let sym = SymbolInformation {
            name: "Foo".into(),
            kind: lsp_types::SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            location: loc("file:///work/proj/src/foo.rs", 9, 0),
            container_name: None,
        };
        let s = format_workspace_symbols(&[sym], Some(std::path::Path::new("/work/proj")));
        assert!(s.contains("[Struct] Foo"));
        assert!(s.contains("src/foo.rs:10:1"));
    }

    #[test]
    fn resolve_input_path_relative_joins_workspace_root() {
        let root = std::path::Path::new("/work/proj");
        let abs = resolve_input_path("src/foo.rs", root);
        assert_eq!(abs, std::path::Path::new("/work/proj/src/foo.rs"));
    }

    #[test]
    fn resolve_input_path_absolute_stays_absolute() {
        let root = std::path::Path::new("/work/proj");
        let abs = resolve_input_path("/etc/hosts", root);
        assert_eq!(abs, std::path::Path::new("/etc/hosts"));
    }
}
