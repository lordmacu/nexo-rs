//! `PermissionMcpServer<D>` — `McpServerHandler` impl that exposes a
//! single `permission_prompt` tool. Caches `AllowSession` outcomes
//! in-process so a Claude turn that re-reads the same file doesn't
//! pay a decider round-trip per call.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_mcp::server::McpServerHandler;
use nexo_mcp::{McpContent, McpError, McpServerInfo, McpTool, McpToolResult};
use serde_json::Value;

use crate::adapter::outcome_to_claude_value;
use crate::bash_destructive;
use crate::cache::SessionCacheKey;
use crate::decider::PermissionDecider;
use crate::path_extractor::{
    classify_command, extract_paths, filter_out_flags, parse_command_args,
};
use crate::sed_validator::sed_command_is_allowed;
use crate::types::{PermissionOutcome, PermissionRequest};

const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(30);

pub struct PermissionMcpServer<D: ?Sized + PermissionDecider = dyn PermissionDecider> {
    decider: Arc<D>,
    server_info: McpServerInfo,
    session_cache: DashMap<SessionCacheKey, PermissionOutcome>,
    decision_timeout: Duration,
}

impl<D: ?Sized + PermissionDecider> PermissionMcpServer<D> {
    pub fn new(decider: Arc<D>) -> Self {
        Self {
            decider,
            server_info: McpServerInfo {
                // Phase 73 — must match the config-key used in
                // `.nexo-mcp.json` ("nexo-driver"). Claude Code 2.1
                // namespaces tools by `mcp__<serverInfo.name>__<tool>`
                // and resolves `--permission-prompt-tool` against
                // that prefix; if `serverInfo.name` and the JSON
                // config-key disagree, Claude registers the server
                // (`status: connected`) but no tool ever lands in
                // the permission registry, surfacing as
                // "Available MCP tools: none" while every Edit /
                // Bash gets denied.
                name: "nexo-driver-permission".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            session_cache: DashMap::new(),
            decision_timeout: DEFAULT_DECISION_TIMEOUT,
        }
    }

    pub fn server_info(mut self, info: McpServerInfo) -> Self {
        self.server_info = info;
        self
    }

    pub fn decision_timeout(mut self, t: Duration) -> Self {
        self.decision_timeout = t;
        self
    }

    fn input_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["tool_name", "input"],
            "properties": {
                "tool_name":   { "type": "string" },
                "input":       { "type": "object" },
                "tool_use_id": { "type": "string" },
                "metadata":    { "type": "object" },
                "goal_id":     { "type": "string" },
            }
        })
    }

    /// Phase 74.2 — explicit output schema. Mirrors the Zod union
    /// Claude Code 2.1 validates internally: every successful
    /// permission_prompt call returns either `{behavior:"allow",
    /// updatedInput: object}` or `{behavior:"deny", message:
    /// string}`. Declaring this on the tool definition lets Claude
    /// type-check the response before forwarding to the model
    /// instead of silently dropping the tool when its inferred
    /// schema and ours drift apart.
    fn output_schema() -> Value {
        // Phase 75 retry — the previous strict variant declared
        // `additionalProperties: false` and a `oneOf` union, which
        // made Claude Code 2.1 silently drop the tool from its
        // permission registry while still reporting the server as
        // `connected`. The accepted shape is a permissive object
        // schema describing the discriminator (`behavior`); Claude
        // tolerates extra fields and the union-by-required-keys
        // pattern when nothing closes the schema.
        serde_json::json!({
            "type": "object",
            "required": ["behavior"],
            "properties": {
                "behavior":     { "type": "string", "enum": ["allow", "deny"] },
                "updatedInput": { "type": "object" },
                "message":      { "type": "string" }
            }
        })
    }
}

#[async_trait]
impl<D: ?Sized + PermissionDecider> McpServerHandler for PermissionMcpServer<D> {
    fn server_info(&self) -> McpServerInfo {
        self.server_info.clone()
    }

    fn capabilities(&self) -> Value {
        serde_json::json!({ "tools": { "listChanged": false } })
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        Ok(vec![McpTool {
            name: "permission_prompt".into(),
            description: Some(
                "Ask the nexo-rs driver agent whether to allow the proposed tool call.".into(),
            ),
            input_schema: Self::input_schema(),
            output_schema: Some(Self::output_schema()),
        }])
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        if name != "permission_prompt" {
            return Err(McpError::Protocol(format!("unknown tool {name}")));
        }
        let req: PermissionRequest = serde_json::from_value(arguments)
            .map_err(|e| McpError::Protocol(format!("invalid arguments: {e}")))?;

        // Keep a clone of the original tool input so we can echo it
        // back in the AllowOnce / AllowSession `updatedInput` field
        // — Claude 2.1's permission schema rejects the response when
        // `updatedInput` is absent or non-object (see adapter.rs).
        let original_input = req.input.clone();
        let tool_name = req.tool_name.clone();
        let warnings = gather_bash_warnings(&tool_name, &original_input);

        let cache_key = SessionCacheKey::from_request(&tool_name, &req.input);
        if let Some(cached) = self.session_cache.get(&cache_key) {
            return Ok(text_result(
                outcome_to_claude_value(cached.value(), &original_input),
                warnings,
            ));
        }
        let resp = tokio::time::timeout(self.decision_timeout, self.decider.decide(req))
            .await
            .map_err(|_| McpError::Protocol("decider timeout".into()))?
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        if matches!(&resp.outcome, PermissionOutcome::AllowSession { .. }) {
            self.session_cache.insert(cache_key, resp.outcome.clone());
        }

        Ok(text_result(
            outcome_to_claude_value(&resp.outcome, &original_input),
            warnings,
        ))
    }
}

fn text_result(value: Value, warnings: Option<String>) -> McpToolResult {
    let is_error = matches!(value.get("behavior").and_then(Value::as_str), Some("deny"));
    // Phase 74.3 — emit BOTH the legacy text content (for clients
    // that still parse it) AND the structured form (for Claude
    // 2.1+ which validates `structuredContent` against the
    // tool's `outputSchema`). Same payload, two channels — costs
    // a clone but eliminates the "re-parse text as JSON" round-
    // trip that surfaced the Zod `updatedInput` flap in Phase 73.
    //
    // Phase 77.8 — prepend bash safety warnings to text content.
    // Warnings never touch structured_content (strict Claude schema).
    let text = if let Some(w) = warnings {
        format!("{w}\n{value}")
    } else {
        value.to_string()
    };
    McpToolResult {
        content: vec![McpContent::Text { text }],
        is_error,
        structured_content: Some(value),
    }
}

/// Gather bash safety warnings for a tool call. Only inspects Bash
/// commands; returns `None` for all other tools.
///
/// Composes four advisory tiers, mirroring the upstream Claude Code
/// permission UI prompt (see refs below). All tiers are advisory:
/// the final allow/deny decision rides on the upstream LLM decider —
/// `gather_bash_warnings` only enriches the prompt context.
///
/// Tiers, in order:
/// 1. **Destructive command** — known-bad shapes (`rm -rf /`, etc.).
/// 2. **Sed in-place shallow** — flags `-i` / `-i.bak` patterns.
/// 3. **Sed deep validator** — gated on first token == `sed`. Calls
///    `sed_validator::sed_command_is_allowed(cmd, allow_file_writes=false)`;
///    fires when result is `false`. Catches `e` (exec) / `w` (file-write)
///    flags + dangerous patterns the shallow check misses.
/// 4. **Path extractor** — when first token classifies as a
///    `PathCommand`, list up to 10 paths the command touches with
///    the matching action verb, so the upstream decider can reason
///    about workspace vs. system paths without re-parsing.
///
/// Scope: only the first clause is inspected. Pipes / `&&` chains
/// past the first command are out of scope here — the destructive
/// check above already covers downstream `rm` / `dd` / etc.
///
/// Provider-agnostic: operates on the bash command string; no LLM
/// provider assumption — same warnings emitted whether the upstream
/// decider is Anthropic, MiniMax, OpenAI, Gemini, DeepSeek, xAI, or
/// Mistral.
///
/// IRROMPIBLE refs (claude-code-leak):
/// - `src/tools/BashTool/bashSecurity.ts` — composes the tiers in
///   the upstream permission UI prompt.
/// - `src/tools/BashTool/sedValidation.ts:247-301` — exact source
///   pattern for `sed_command_is_allowed`.
/// - `src/tools/BashTool/pathValidation.ts:27-509` — command-aware
///   path extraction (`classify_command` / `filter_out_flags` /
///   `extract_paths`).
///
/// IRROMPIBLE refs (research/): no significant prior art —
/// OpenClaw is channel-side and does not implement bash command
/// safety analysis.
fn gather_bash_warnings(tool_name: &str, input: &Value) -> Option<String> {
    if tool_name != "Bash" {
        return None;
    }
    let command = input.get("command")?.as_str()?;
    let mut warnings: Vec<String> = Vec::new();

    if let Some(w) = bash_destructive::check_destructive_command(command) {
        warnings.push(w.to_string());
    }
    if let Some(w) = bash_destructive::check_sed_in_place(command) {
        warnings.push(w.to_string());
    }

    // Tier 3 — sed deep validator. Gate on first token == "sed"
    // because `sed_command_is_allowed` returns false for any
    // non-sed input (it expects to find sed expressions to
    // validate). Scope: first clause only — pipes / `&&` chains
    // past the first `sed` are out of scope here, the destructive
    // check above already covers `rm` / `dd` / etc downstream.
    let tokens = parse_command_args(command);
    let first = tokens.first().map(String::as_str).unwrap_or("");
    if first == "sed" && !sed_command_is_allowed(command, false) {
        warnings.push(
            "sed expression outside the safe allowlist (line-printing or simple substitution); review for `e` (exec) or `w` (file-write) flags".to_string(),
        );
    }

    // Tier 4 — path extractor. Surface which paths the command
    // touches so the upstream LLM decider can reason about
    // workspace vs. system paths without re-parsing the command.
    if let Some(cmd) = classify_command(first) {
        let filtered: Vec<String> = filter_out_flags(&tokens[1..]);
        let paths = extract_paths(cmd, &filtered);
        if !paths.is_empty() {
            const MAX_LISTED: usize = 10;
            let listed: Vec<&str> = paths.iter().take(MAX_LISTED).map(String::as_str).collect();
            let suffix = if paths.len() > MAX_LISTED {
                format!(" ({} more)", paths.len() - MAX_LISTED)
            } else {
                String::new()
            };
            warnings.push(format!(
                "{} the following paths: [{}]{}",
                cmd.action_verb(),
                listed.join(", "),
                suffix
            ));
        }
    }

    if warnings.is_empty() {
        None
    } else {
        Some(format!(
            "WARNING — bash security:\n- {}",
            warnings.join("\n- ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gather_bash_warnings_skips_non_bash() {
        let input = json!({ "command": "rm -rf /" });
        assert!(gather_bash_warnings("FileEdit", &input).is_none());
    }

    #[test]
    fn gather_bash_warnings_returns_none_for_simple_sed() {
        // `sed -n '1,5p' f.txt` is a line-printing command — allowed
        // by `sed_command_is_allowed` and not destructive nor in-place.
        let input = json!({ "command": "sed -n '1,5p' f.txt" });
        let out = gather_bash_warnings("Bash", &input);
        // Path wire still fires (sed is a classified PathCommand);
        // sed deep wire must NOT fire.
        let text = out.unwrap_or_default();
        assert!(
            !text.contains("outside the safe allowlist"),
            "simple sed should not trigger the deep validator: got {text:?}",
        );
    }

    #[test]
    fn gather_bash_warnings_flags_complex_sed() {
        // `e` flag executes shell — outside allowlist.
        let input = json!({ "command": "sed 's/foo/bar/e' file.txt" });
        let out = gather_bash_warnings("Bash", &input).expect("warning expected");
        assert!(
            out.contains("outside the safe allowlist"),
            "expected sed deep warning, got {out:?}",
        );
    }

    #[test]
    fn gather_bash_warnings_lists_paths_for_classified_commands() {
        let input = json!({ "command": "cat /etc/passwd /etc/shadow" });
        let out = gather_bash_warnings("Bash", &input).expect("warning expected");
        assert!(
            out.contains("the following paths:"),
            "expected path-list warning, got {out:?}",
        );
        assert!(
            out.contains("/etc/passwd") && out.contains("/etc/shadow"),
            "both paths should be listed: {out:?}",
        );
    }
}
