#![allow(clippy::all)] // Phase 79 scaffolding — re-enable when 79.x fully shipped

//! Phase 79.13 — `NotebookEdit` Jupyter `.ipynb` cell editor.
//!
//! Cell-level edits with output-preservation. Pure Rust round-trip
//! through `serde_json::Value` — no `jupyter` binary required, no
//! `nbformat` Python dep. The notebook is a well-defined JSON
//! document (nbformat 4.x); unknown top-level fields survive
//! untouched (forward-compat).
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/NotebookEditTool/NotebookEditTool.ts:30-489`
//!     (input schema, validate-input, JSON parse + mutate + write,
//!     `IPYNB_INDENT = 1` formatting, replace-at-end auto-converts
//!     to insert).
//!   * `claude-code-leak/src/utils/notebook.ts::parseCellId` —
//!     accepts both UUIDv4-style ids and `cell-N` numeric fallback.
//!
//! Reference (secondary):
//!   * OpenClaw `research/` — no equivalent
//!     (`grep -rln "ipynb|jupyter|nbformat" research/src/` returns
//!     nothing relevant).
//!
//! MVP scope (Phase 79.13):
//!   * Replace / insert / delete a single cell.
//!   * cell_id may be a UUID-style id (`notebook.cells[i].id`) or a
//!     `cell-N` numeric index fallback.
//!   * Code-cell replaces clear `execution_count` + `outputs`.
//!   * Out of scope: `Read-before-Edit` guard (no shared file-state
//!     in the agent runtime); attribution tracking
//!     (`fileHistoryTrackEdit`); cell-type conversion mid-replace
//!     (we accept `cell_type` arg but do not transmute existing
//!     cells).

use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use async_trait::async_trait;
use nexo_llm::ToolDef;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Indent used by Jupyter's canonical writer (`json.dumps(indent=1)`
/// in `nbformat`). Matches `claude-code-leak/src/tools/NotebookEditTool/NotebookEditTool.ts:430`.
pub const IPYNB_INDENT: usize = 1;

pub struct NotebookEditTool;

impl NotebookEditTool {
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "NotebookEdit".to_string(),
            description: "Edit a single cell in a Jupyter notebook (.ipynb). Three edit modes: `replace` (default — overwrite the cell's source), `insert` (add a new cell after the anchor), `delete` (remove the cell). Round-trips through serde_json so unknown nbformat fields survive untouched. Code-cell replaces clear execution_count + outputs (the diff stays sane); markdown cells preserve all metadata.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "notebook_path": {
                        "type": "string",
                        "description": "Absolute path to the .ipynb file."
                    },
                    "cell_id": {
                        "type": "string",
                        "description": "ID of the cell to operate on. Either a UUID-style id (notebook.cells[i].id) or a `cell-N` numeric index fallback. For `insert`, the new cell goes AFTER this anchor; omit (or empty) to insert at position 0."
                    },
                    "new_source": {
                        "type": "string",
                        "description": "Source code / markdown body. Required for `replace` + `insert`; ignored for `delete`."
                    },
                    "cell_type": {
                        "type": "string",
                        "enum": ["code", "markdown"],
                        "description": "Cell type — required for `insert`, optional for `replace` (defaults to the existing cell's type)."
                    },
                    "edit_mode": {
                        "type": "string",
                        "enum": ["replace", "insert", "delete"],
                        "description": "Defaults to `replace`."
                    }
                },
                "required": ["notebook_path"]
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditMode {
    Replace,
    Insert,
    Delete,
}

fn parse_edit_mode(s: Option<&str>) -> anyhow::Result<EditMode> {
    match s.unwrap_or("replace") {
        "replace" => Ok(EditMode::Replace),
        "insert" => Ok(EditMode::Insert),
        "delete" => Ok(EditMode::Delete),
        other => Err(anyhow::anyhow!(
            "edit_mode must be replace|insert|delete, got `{other}`"
        )),
    }
}

/// Lift from `claude-code-leak/src/utils/notebook.ts::parseCellId` —
/// accepts `cell-N` form returning `Some(N)`. Only used as a
/// fallback when the literal id lookup misses.
fn parse_cell_index(cell_id: &str) -> Option<usize> {
    cell_id
        .strip_prefix("cell-")
        .and_then(|rest| rest.parse::<usize>().ok())
}

/// Resolve `cell_id` to a position in `cells`. Tries the literal
/// `cells[i].id == cell_id` match first, then the `cell-N`
/// numeric-index fallback.
fn find_cell_index(cells: &[Value], cell_id: &str) -> Option<usize> {
    for (idx, cell) in cells.iter().enumerate() {
        if cell
            .get("id")
            .and_then(|v| v.as_str())
            .map_or(false, |s| s == cell_id)
        {
            return Some(idx);
        }
    }
    parse_cell_index(cell_id).filter(|&n| n < cells.len())
}

fn nbformat_supports_cell_id(notebook: &Value) -> bool {
    let major = notebook
        .get("nbformat")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let minor = notebook
        .get("nbformat_minor")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    major > 4 || (major == 4 && minor >= 5)
}

/// 12-char base-36 id matching the leak's
/// `Math.random().toString(36).substring(2, 15)`. Generated only
/// when `nbformat >= 4.5` so older notebooks stay valid.
fn fresh_cell_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Cheap pseudo-random — combines high-resolution time with a
    // process-local counter. Good enough for nbformat ids; the leak
    // also uses `Math.random()` (not a CSPRNG).
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut value =
        nanos.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ n.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let alphabet: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = String::with_capacity(12);
    for _ in 0..12 {
        let idx = (value % 36) as usize;
        out.push(alphabet[idx] as char);
        value /= 36;
        if value == 0 {
            value = nanos.wrapping_add(n).wrapping_mul(2654435769);
        }
    }
    out
}

fn build_new_cell(cell_type: &str, source: &str, fresh_id: Option<String>) -> Value {
    let mut cell = Map::new();
    cell.insert("cell_type".to_string(), json!(cell_type));
    if let Some(id) = fresh_id {
        cell.insert("id".to_string(), json!(id));
    }
    cell.insert("source".to_string(), json!(source));
    cell.insert("metadata".to_string(), json!({}));
    if cell_type == "code" {
        cell.insert("execution_count".to_string(), Value::Null);
        cell.insert("outputs".to_string(), json!([]));
    }
    Value::Object(cell)
}

#[async_trait]
impl ToolHandler for NotebookEditTool {
    #[allow(unused_assignments)]
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let notebook_path = args
            .get("notebook_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("NotebookEdit requires `notebook_path`"))?;
        let path = PathBuf::from(notebook_path);
        if !path.is_absolute() {
            return Err(anyhow::anyhow!(
                "NotebookEdit: `notebook_path` must be absolute"
            ));
        }
        if path.extension().and_then(|s| s.to_str()) != Some("ipynb") {
            return Err(anyhow::anyhow!(
                "NotebookEdit: file must end in .ipynb (use FileEdit for other types)"
            ));
        }

        let edit_mode = parse_edit_mode(args.get("edit_mode").and_then(|v| v.as_str()))?;
        let cell_id = args.get("cell_id").and_then(|v| v.as_str()).unwrap_or("");
        let new_source = args
            .get("new_source")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let cell_type = args.get("cell_type").and_then(|v| v.as_str());

        if matches!(edit_mode, EditMode::Replace | EditMode::Insert)
            && new_source.is_empty()
            && args.get("new_source").is_none()
        {
            return Err(anyhow::anyhow!(
                "NotebookEdit: `new_source` is required for replace/insert"
            ));
        }
        if matches!(edit_mode, EditMode::Insert) && cell_type.is_none() {
            return Err(anyhow::anyhow!(
                "NotebookEdit: `cell_type` is required when edit_mode=insert"
            ));
        }

        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("NotebookEdit: read failed: {e}"))?;
        let mut notebook: Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("NotebookEdit: invalid JSON: {e}"))?;
        let nbformat_minor_5 = nbformat_supports_cell_id(&notebook);

        let language = notebook
            .pointer("/metadata/language_info/name")
            .and_then(|v| v.as_str())
            .unwrap_or("python")
            .to_string();

        let cells = notebook
            .get_mut("cells")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| anyhow::anyhow!("NotebookEdit: notebook has no `cells` array"))?;

        let original_cell_count = cells.len();
        let mut effective_mode = edit_mode;
        let mut effective_cell_type = cell_type.map(str::to_string);
        let mut returned_cell_id: Option<String> = None;
        let mut anchor_index: Option<usize> = None;

        // Resolve anchor index. For `insert`, the new cell goes
        // AFTER the anchor; an empty cell_id means "position 0".
        // For replace/delete, anchor must exist.
        if cell_id.is_empty() {
            if matches!(edit_mode, EditMode::Insert) {
                anchor_index = Some(0);
            } else {
                return Err(anyhow::anyhow!(
                    "NotebookEdit: `cell_id` is required for {} (only insert may omit it)",
                    match edit_mode {
                        EditMode::Replace => "replace",
                        EditMode::Delete => "delete",
                        EditMode::Insert => unreachable!(),
                    }
                ));
            }
        } else {
            anchor_index = find_cell_index(cells, cell_id);
            if anchor_index.is_none() {
                let available_ids: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        c.get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("cell-{i}"))
                    })
                    .take(10)
                    .collect();
                return Err(anyhow::anyhow!(
                    "NotebookEdit: cell_id `{cell_id}` not found. Available (up to 10): {available}",
                    available = available_ids.join(", ")
                ));
            }
        }

        match effective_mode {
            EditMode::Delete => {
                let idx = anchor_index.unwrap();
                cells.remove(idx);
                returned_cell_id = Some(cell_id.to_string());
            }
            EditMode::Insert => {
                let mut idx = anchor_index.unwrap();
                if !cell_id.is_empty() {
                    idx += 1; // insert AFTER the anchor
                }
                let ct = effective_cell_type.clone().unwrap_or_else(|| "code".into());
                let fresh_id = if nbformat_minor_5 {
                    Some(fresh_cell_id())
                } else {
                    None
                };
                returned_cell_id = fresh_id.clone();
                let cell = build_new_cell(&ct, new_source, fresh_id);
                if idx > cells.len() {
                    return Err(anyhow::anyhow!(
                        "NotebookEdit: anchor index {idx} > total cells {}",
                        cells.len()
                    ));
                }
                cells.insert(idx, cell);
            }
            EditMode::Replace => {
                let idx = anchor_index.unwrap();
                if idx == cells.len() {
                    // Defensive: replace-at-end auto-converts to
                    // insert. Lift from leak `:372-377`.
                    let ct = effective_cell_type.clone().unwrap_or_else(|| "code".into());
                    let fresh_id = if nbformat_minor_5 {
                        Some(fresh_cell_id())
                    } else {
                        None
                    };
                    returned_cell_id = fresh_id.clone();
                    cells.push(build_new_cell(&ct, new_source, fresh_id));
                    effective_mode = EditMode::Insert;
                } else {
                    let target = cells.get_mut(idx).unwrap();
                    let target_obj = target
                        .as_object_mut()
                        .ok_or_else(|| anyhow::anyhow!("NotebookEdit: cell is not an object"))?;
                    target_obj.insert("source".to_string(), json!(new_source));
                    let current_type = target_obj
                        .get("cell_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("code")
                        .to_string();
                    if current_type == "code" {
                        target_obj.insert("execution_count".to_string(), Value::Null);
                        target_obj.insert("outputs".to_string(), json!([]));
                    }
                    if let Some(ct) = &effective_cell_type {
                        if ct != &current_type {
                            target_obj.insert("cell_type".to_string(), json!(ct));
                        }
                    } else {
                        effective_cell_type = Some(current_type);
                    }
                    returned_cell_id = target_obj
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
        }

        // Re-serialise with Jupyter's canonical 1-space indent.
        let updated = pretty_indent(&notebook, IPYNB_INDENT);
        std::fs::write(&path, &updated)
            .map_err(|e| anyhow::anyhow!("NotebookEdit: write failed: {e}"))?;

        let total_cells = notebook
            .get("cells")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        Ok(json!({
            "notebook_path": path.display().to_string(),
            "edit_mode": match effective_mode {
                EditMode::Replace => "replace",
                EditMode::Insert => "insert",
                EditMode::Delete => "delete",
            },
            "cell_id": returned_cell_id,
            "cell_type": effective_cell_type.unwrap_or_else(|| "code".into()),
            "language": language,
            "total_cells": total_cells,
            "cells_delta": total_cells as i64 - original_cell_count as i64,
        }))
    }
}

/// Custom pretty-print emitting Jupyter's 1-space indent. The
/// stdlib `serde_json::to_string_pretty` always uses 2 spaces, and
/// Jupyter's `nbformat` writes with `indent=1` — diffs against the
/// canonical format would be huge otherwise.
fn pretty_indent(value: &Value, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes()),
    );
    use serde::Serialize;
    value.serialize(&mut ser).expect("serde to Vec never fails");
    String::from_utf8(out).expect("serde emits valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use std::sync::Arc;
    use tempfile::TempDir;

    fn ctx() -> AgentContext {
        let cfg = AgentConfig {
            id: "a".into(),
            model: ModelConfig {
                provider: "x".into(),
                model: "y".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
        repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
        };
        AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
        )
    }

    fn sample_notebook() -> Value {
        json!({
            "cells": [
                {
                    "cell_type": "code",
                    "id": "alpha",
                    "metadata": {},
                    "source": "print('hello')",
                    "execution_count": 7,
                    "outputs": [{"output_type": "stream", "name": "stdout", "text": "hello\n"}]
                },
                {
                    "cell_type": "markdown",
                    "id": "beta",
                    "metadata": {},
                    "source": "# header"
                }
            ],
            "metadata": {
                "language_info": {"name": "python"},
                "kernelspec": {"name": "python3", "display_name": "Python 3"}
            },
            "nbformat": 4,
            "nbformat_minor": 5,
            "x_unknown_field": "must round-trip"
        })
    }

    fn write_notebook(dir: &TempDir, name: &str, body: &Value) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, pretty_indent(body, IPYNB_INDENT)).unwrap();
        p
    }

    fn read_notebook(p: &PathBuf) -> Value {
        let raw = std::fs::read_to_string(p).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[tokio::test]
    async fn replace_clears_outputs_and_execution_count() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        let res = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "alpha",
                    "new_source": "print('world')",
                    "edit_mode": "replace"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["edit_mode"], "replace");
        assert_eq!(res["cell_id"], "alpha");
        let nb = read_notebook(&p);
        let alpha = &nb["cells"][0];
        assert_eq!(alpha["source"], "print('world')");
        assert_eq!(alpha["execution_count"], Value::Null);
        assert_eq!(alpha["outputs"].as_array().unwrap().len(), 0);
        // Untouched cell preserved.
        assert_eq!(nb["cells"][1]["source"], "# header");
        // Unknown top-level field round-trips.
        assert_eq!(nb["x_unknown_field"], "must round-trip");
    }

    #[tokio::test]
    async fn insert_after_anchor_grows_total_cells() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        let res = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "alpha",
                    "new_source": "x = 1",
                    "cell_type": "code",
                    "edit_mode": "insert"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["edit_mode"], "insert");
        assert_eq!(res["cells_delta"], 1);
        assert_eq!(res["total_cells"], 3);
        let nb = read_notebook(&p);
        // alpha kept first, new cell at index 1, beta pushed to 2.
        assert_eq!(nb["cells"][0]["id"], "alpha");
        assert_eq!(nb["cells"][1]["source"], "x = 1");
        assert_eq!(nb["cells"][2]["id"], "beta");
        // Fresh id present for nbformat 4.5+.
        assert!(nb["cells"][1]["id"].is_string());
    }

    #[tokio::test]
    async fn insert_with_empty_cell_id_goes_to_position_zero() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "",
                    "new_source": "import os",
                    "cell_type": "code",
                    "edit_mode": "insert"
                }),
            )
            .await
            .unwrap();
        let nb = read_notebook(&p);
        assert_eq!(nb["cells"][0]["source"], "import os");
        assert_eq!(nb["cells"][1]["id"], "alpha");
    }

    #[tokio::test]
    async fn delete_shrinks_total_cells() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        let res = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "beta",
                    "edit_mode": "delete"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["edit_mode"], "delete");
        assert_eq!(res["cells_delta"], -1);
        let nb = read_notebook(&p);
        assert_eq!(nb["cells"].as_array().unwrap().len(), 1);
        assert_eq!(nb["cells"][0]["id"], "alpha");
    }

    #[tokio::test]
    async fn cell_n_index_fallback_when_no_uuid_id() {
        let dir = TempDir::new().unwrap();
        let mut nb = sample_notebook();
        // Drop the explicit `id` fields so the model must fall back
        // to the cell-N convention.
        for cell in nb["cells"].as_array_mut().unwrap() {
            cell.as_object_mut().unwrap().remove("id");
        }
        let p = write_notebook(&dir, "n.ipynb", &nb);
        let res = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "cell-0",
                    "new_source": "y = 2",
                    "edit_mode": "replace"
                }),
            )
            .await
            .unwrap();
        assert_eq!(res["edit_mode"], "replace");
        let written = read_notebook(&p);
        assert_eq!(written["cells"][0]["source"], "y = 2");
    }

    #[tokio::test]
    async fn missing_cell_id_lists_available() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        let err = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "imaginary",
                    "new_source": "noop",
                    "edit_mode": "replace"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "got: {err}");
        assert!(err.contains("alpha"), "got: {err}");
    }

    #[tokio::test]
    async fn refuses_non_ipynb_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("plain.json");
        std::fs::write(&p, "{}").unwrap();
        let err = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "x",
                    "new_source": "y",
                    "edit_mode": "replace"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(".ipynb"), "got: {err}");
    }

    #[tokio::test]
    async fn refuses_relative_path() {
        let err = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": "notebooks/n.ipynb",
                    "cell_id": "x",
                    "new_source": "y",
                    "edit_mode": "replace"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("absolute"), "got: {err}");
    }

    #[tokio::test]
    async fn insert_requires_cell_type() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        let err = NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "alpha",
                    "new_source": "x",
                    "edit_mode": "insert"
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("cell_type"), "got: {err}");
    }

    #[tokio::test]
    async fn parse_cell_index_works() {
        assert_eq!(parse_cell_index("cell-3"), Some(3));
        assert_eq!(parse_cell_index("cell-0"), Some(0));
        assert_eq!(parse_cell_index("cell-foo"), None);
        assert_eq!(parse_cell_index("alpha"), None);
    }

    #[tokio::test]
    async fn fresh_cell_id_is_12_chars_lower_alnum() {
        let id = fresh_cell_id();
        assert_eq!(id.len(), 12);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[tokio::test]
    async fn round_trip_preserves_unknown_fields() {
        let dir = TempDir::new().unwrap();
        let p = write_notebook(&dir, "n.ipynb", &sample_notebook());
        // Touch any cell — the rest must round-trip.
        NotebookEditTool
            .call(
                &ctx(),
                json!({
                    "notebook_path": p.display().to_string(),
                    "cell_id": "alpha",
                    "new_source": "x = 1",
                    "edit_mode": "replace"
                }),
            )
            .await
            .unwrap();
        let nb = read_notebook(&p);
        assert_eq!(nb["x_unknown_field"], "must round-trip");
        assert_eq!(nb["nbformat"], 4);
        assert_eq!(nb["nbformat_minor"], 5);
        assert_eq!(nb["metadata"]["kernelspec"]["name"], "python3");
    }
}
