//! Phase 79.5 — `Lsp` tool registration + handler.
//!
//! Single tool, discriminator `kind` inside the args, dynamically-
//! described based on the [`LspManager`]'s aggregated capabilities
//! at registration time.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/LSPTool/LSPTool.ts:127-251` —
//!     `buildTool` shape, `isEnabled()` gating, `call` skeleton.
//!     We collapse the leak's 9 ops into 5 MVP ops.
//!   * `claude-code-leak/src/tools/LSPTool/prompt.ts:1-22` —
//!     description format. We mirror the layout but drop the
//!     mention of MCP/plugin-contributed servers since our matrix
//!     is built-in.

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use nexo_lsp::{ExecutePolicy, LspManager, LspRequest};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct LspTool {
    manager: Arc<LspManager>,
    policy: ExecutePolicy,
    workspace_root: PathBuf,
    /// Set true when the originating tool call comes from a Phase
    /// 19/20 synthetic poller. The MVP wires `false` everywhere
    /// (Phase 67 already plumbed `is_synthetic` into the dispatch
    /// context; threading it into AgentContext is a 79.5.b
    /// follow-up).
    treat_origin_as_synthetic: bool,
}

impl LspTool {
    pub fn new(manager: Arc<LspManager>, policy: ExecutePolicy, workspace_root: PathBuf) -> Self {
        Self {
            manager,
            policy,
            workspace_root,
            treat_origin_as_synthetic: false,
        }
    }

    /// Build the static portion of the tool's `parameters` schema —
    /// the shape never changes; only the description strings adapt
    /// to the active capability set.
    pub fn parameters_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["go_to_def", "hover", "references", "workspace_symbol", "diagnostics"],
                    "description": "Operation to perform: go_to_def | hover | references | workspace_symbol | diagnostics"
                },
                "file": {
                    "type": "string",
                    "description": "Absolute or workspace-relative file path. Required for go_to_def, hover, references, diagnostics."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line number (matching editor UX). Required for go_to_def, hover, references."
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based character offset (matching editor UX). Required for go_to_def, hover, references."
                },
                "query": {
                    "type": "string",
                    "description": "Symbol query for workspace_symbol. Empty string returns all symbols."
                }
            },
            "required": ["kind"]
        })
    }

    /// Build a `ToolDef` whose description advertises only the
    /// capabilities currently supported by at least one running
    /// session. When zero capabilities are present (no servers
    /// running yet), the description includes all 5 kinds with a
    /// note that the result may be `ServerUnavailable` until a
    /// session warms up.
    pub async fn tool_def(&self) -> ToolDef {
        let caps = self.manager.aggregated_capabilities().await;
        let mut active = Vec::new();
        for kind in [
            "go_to_def",
            "hover",
            "references",
            "workspace_symbol",
            "diagnostics",
        ] {
            if caps.supports(kind) {
                active.push(kind);
            }
        }
        let description = if active.is_empty() {
            String::from(
                "Query a Language Server Protocol server in-process for code intelligence. \
                 No servers are warm yet — the first call to a supported language will spawn the server (~500 ms cold start). \
                 Supported kinds: go_to_def | hover | references | workspace_symbol | diagnostics.\n\n\
                 All `line` and `character` parameters are 1-based (matching editor UX, not the LSP wire which is 0-based).",
            )
        } else {
            format!(
                "Query a Language Server Protocol server in-process for code intelligence. \
                 Supported kinds (advertised based on running servers): {}.\n\n\
                 All `line` and `character` parameters are 1-based (matching editor UX, not the LSP wire which is 0-based).",
                active.join(", ")
            )
        };
        ToolDef {
            name: "Lsp".to_string(),
            description,
            parameters: Self::parameters_schema(),
        }
    }
}

#[async_trait]
impl ToolHandler for LspTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let req = match parse_request(&args) {
            Ok(r) => r,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "error": e,
                    "kind": "Wire"
                }))
            }
        };
        match self
            .manager
            .execute(
                req,
                &self.policy,
                &self.workspace_root,
                self.treat_origin_as_synthetic,
            )
            .await
        {
            Ok(out) => Ok(json!({
                "ok": true,
                "formatted": out.formatted,
                "structured": out.structured
            })),
            Err(e) => Ok(json!({
                "ok": false,
                "error": e.to_string(),
                "kind": e.kind()
            })),
        }
    }
}

fn parse_request(args: &Value) -> Result<LspRequest, String> {
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Lsp tool requires `kind` (string)".to_string())?;
    match kind {
        "go_to_def" => Ok(LspRequest::GoToDef {
            file: read_required_string(args, "file")?,
            line: read_required_position(args, "line")?,
            character: read_required_position(args, "character")?,
        }),
        "hover" => Ok(LspRequest::Hover {
            file: read_required_string(args, "file")?,
            line: read_required_position(args, "line")?,
            character: read_required_position(args, "character")?,
        }),
        "references" => Ok(LspRequest::References {
            file: read_required_string(args, "file")?,
            line: read_required_position(args, "line")?,
            character: read_required_position(args, "character")?,
        }),
        "workspace_symbol" => Ok(LspRequest::WorkspaceSymbol {
            query: args
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_default(),
        }),
        "diagnostics" => Ok(LspRequest::Diagnostics {
            file: read_required_string(args, "file")?,
        }),
        other => Err(format!(
            "unknown Lsp kind `{other}`. Supported: go_to_def, hover, references, workspace_symbol, diagnostics"
        )),
    }
}

fn read_required_string(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Lsp tool requires `{field}` (non-empty string)"))
}

fn read_required_position(args: &Value, field: &str) -> Result<u32, String> {
    let n = args
        .get(field)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("Lsp tool requires `{field}` (positive integer)"))?;
    if n == 0 {
        return Err(format!(
            "Lsp tool `{field}` must be 1-based (>= 1); got 0"
        ));
    }
    Ok(n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_lsp::{LspLauncher, SessionConfig};

    fn manager_for_tests() -> Arc<LspManager> {
        // Empty launcher — no binaries; LspTool still constructs.
        let launcher = LspLauncher::probe_with(|_| None);
        LspManager::with_launcher(launcher, SessionConfig::default())
    }

    #[tokio::test]
    async fn tool_def_empty_caps_describes_all_kinds_with_warmup_note() {
        let manager = manager_for_tests();
        let tool = LspTool::new(
            manager.clone(),
            ExecutePolicy::default(),
            std::path::PathBuf::from("/tmp"),
        );
        let def = tool.tool_def().await;
        assert_eq!(def.name, "Lsp");
        assert!(def.description.contains("go_to_def"));
        assert!(def.description.contains("hover"));
        assert!(def.description.contains("references"));
        assert!(def.description.contains("1-based"));
        assert!(def.description.contains("No servers are warm yet"));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn parse_request_go_to_def() {
        let args = json!({
            "kind": "go_to_def",
            "file": "src/foo.rs",
            "line": 42,
            "character": 8
        });
        let req = parse_request(&args).unwrap();
        match req {
            LspRequest::GoToDef {
                file,
                line,
                character,
            } => {
                assert_eq!(file, "src/foo.rs");
                assert_eq!(line, 42);
                assert_eq!(character, 8);
            }
            _ => panic!("expected GoToDef"),
        }
    }

    #[tokio::test]
    async fn parse_request_workspace_symbol_empty_query() {
        let args = json!({ "kind": "workspace_symbol" });
        let req = parse_request(&args).unwrap();
        match req {
            LspRequest::WorkspaceSymbol { query } => assert_eq!(query, ""),
            _ => panic!("expected WorkspaceSymbol"),
        }
    }

    #[tokio::test]
    async fn parse_request_unknown_kind_errors() {
        let args = json!({ "kind": "rename_symbol" });
        let err = parse_request(&args).unwrap_err();
        assert!(err.contains("unknown Lsp kind"));
        assert!(err.contains("rename_symbol"));
    }

    #[tokio::test]
    async fn parse_request_zero_position_rejected() {
        let args = json!({
            "kind": "hover",
            "file": "x.rs",
            "line": 0,
            "character": 1
        });
        let err = parse_request(&args).unwrap_err();
        assert!(err.contains("1-based"));
    }

    #[tokio::test]
    async fn parse_request_missing_file_rejected_for_position_kinds() {
        let args = json!({ "kind": "hover", "line": 1, "character": 1 });
        let err = parse_request(&args).unwrap_err();
        assert!(err.contains("`file`"));
    }

    // The full call-path exercises (`call_returns_unavailable...`,
    // `call_bad_args...`) require an `AgentContext` with a real
    // `AgentConfig` + broker + sessions, mirroring
    // `cron_tool.rs::ctx_with_origin`. Defer those to a separate
    // integration test file in the follow-up — the manager-level
    // tests in `nexo-lsp` already cover ServerUnavailable + the
    // parse_request unit tests cover the bad-args path.
}
