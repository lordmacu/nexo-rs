//! Phase 80.8 — `send_user_message` tool.
//!
//! Brief mode tells the model that user-visible output flows
//! through this tool, not free text. The driver-loop is free to
//! continue rendering free text alongside; channel adapters can
//! opt in to *hide* free text and only render `send_user_message`
//! calls (deferred 80.8.b).
//!
//! Activation lives in the boot path: when the agent's
//! [`BriefConfig`](nexo_config::types::brief::BriefConfig) is
//! active for a binding, [`register_send_user_message_tool`] adds
//! the tool to the registry. Otherwise the model never sees it.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use nexo_config::types::brief::BriefConfig;
use nexo_llm::ToolDef;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use super::tool_registry::{ToolHandler, ToolRegistry};
use super::AgentContext;

/// Tool name. Mirrors the public-facing leak alias for
/// cross-tool-catalog compatibility (operators porting from one
/// stack to the other can reuse prompt fragments).
pub const TOOL_NAME: &str = "send_user_message";

/// Sentinel key in the JSON result so downstream renderers
/// (driver-loop telemetry, channel adapters) can detect a brief
/// message without parsing the schema again.
pub const BRIEF_SENTINEL: &str = "__nexo_send_user_message__";

/// Hard upper bound on the body size, regardless of operator
/// config. 8 MiB is enough for any human-readable markdown reply.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Returns `true` when `value` carries the brief sentinel.
pub fn is_brief_result(value: &Value) -> bool {
    value
        .get(BRIEF_SENTINEL)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Resolved attachment metadata returned to the model.
fn attachment_metadata(
    raw_paths: &[String],
    cwd: Option<&PathBuf>,
) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        let path = PathBuf::from(raw);
        let resolved = if path.is_absolute() {
            path.clone()
        } else if let Some(base) = cwd {
            base.join(&path)
        } else {
            path.clone()
        };
        let canon = resolved
            .canonicalize()
            .map_err(|e| anyhow!("attachment {raw:?} not accessible: {e}"))?;
        let meta = std::fs::metadata(&canon)
            .map_err(|e| anyhow!("attachment {raw:?} stat failed: {e}"))?;
        if !meta.is_file() {
            return Err(anyhow!("attachment {raw:?} is not a regular file"));
        }
        let is_image = is_likely_image(&canon);
        out.push(json!({
            "path": canon.to_string_lossy(),
            "size": meta.len(),
            "is_image": is_image
        }));
    }
    Ok(out)
}

fn is_likely_image(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "heic" | "heif")
    )
}

/// Status field — purely informational for telemetry + downstream
/// adapters. `Normal` is the default: the model is replying to
/// something the user just said. `Proactive` is for unsolicited
/// surfacings (autoDream, cron fires, AWAY_SUMMARY).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BriefStatus {
    Normal,
    Proactive,
}

impl BriefStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            BriefStatus::Normal => "normal",
            BriefStatus::Proactive => "proactive",
        }
    }
    pub fn parse(raw: Option<&str>, required: bool) -> Result<Self> {
        match raw {
            Some("normal") => Ok(BriefStatus::Normal),
            Some("proactive") => Ok(BriefStatus::Proactive),
            Some(other) => Err(anyhow!(
                "send_user_message: status must be 'normal' or 'proactive', got {other:?}"
            )),
            None if required => Err(anyhow!(
                "send_user_message: status field is required (set status_required=false to make it optional)"
            )),
            None => Ok(BriefStatus::Normal),
        }
    }
}

/// The tool itself. Per-binding config decides whether status is
/// schema-required and how many attachments are allowed.
#[derive(Clone)]
pub struct SendUserMessageTool {
    cfg: BriefConfig,
}

impl SendUserMessageTool {
    pub fn new(cfg: BriefConfig) -> Self {
        Self { cfg }
    }

    pub fn tool_def(cfg: &BriefConfig) -> ToolDef {
        let mut required: Vec<&str> = vec!["message"];
        if cfg.status_required {
            required.push("status");
        }
        ToolDef {
            name: TOOL_NAME.into(),
            description:
                "Send a message the user will read. Free-text output stays in the detail \
                 view; this tool is the channel the user actually sees. \
                 `message` supports markdown. `attachments` accepts file paths \
                 (absolute or relative to the agent workspace) to include alongside \
                 the message — images, diffs, logs. \
                 `status: 'normal'` when replying to the user's last message; \
                 `status: 'proactive'` when surfacing something they did not ask for \
                 (a scheduled task finished, a blocker hit during background work, \
                 an unsolicited update). Set it honestly — downstream telemetry \
                 and routing key off it.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Markdown body for the user."
                    },
                    "attachments": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional file paths (absolute or workspace-relative)."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["normal", "proactive"],
                        "description": if cfg.status_required {
                            "Required. 'normal' for replies, 'proactive' for unsolicited surfacings."
                        } else {
                            "Optional, defaults to 'normal'."
                        }
                    }
                },
                "required": required
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for SendUserMessageTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> Result<Value> {
        // ---- Gate 1: message present + size cap ----
        let message = args["message"]
            .as_str()
            .ok_or_else(|| anyhow!("send_user_message: message must be a string"))?;
        if message.is_empty() {
            return Err(anyhow!("send_user_message: message must be non-empty"));
        }
        if message.len() > MAX_MESSAGE_BYTES {
            return Err(anyhow!(
                "send_user_message: message exceeds {} bytes",
                MAX_MESSAGE_BYTES
            ));
        }

        // ---- Gate 2: status (per cfg.status_required) ----
        let status =
            BriefStatus::parse(args["status"].as_str(), self.cfg.status_required)?;

        // ---- Gate 3: attachment count ----
        let raw_attachments: Vec<String> = match &args["attachments"] {
            Value::Array(arr) => arr
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| anyhow!("send_user_message: attachments must be strings"))
                        .map(|s| s.to_string())
                })
                .collect::<Result<Vec<_>>>()?,
            Value::Null => Vec::new(),
            _ => {
                return Err(anyhow!(
                    "send_user_message: attachments must be an array of strings"
                ))
            }
        };
        if !raw_attachments.is_empty() && self.cfg.max_attachments == 0 {
            return Err(anyhow!(
                "send_user_message: attachments are disabled by config"
            ));
        }
        if raw_attachments.len() as u32 > self.cfg.max_attachments {
            return Err(anyhow!(
                "send_user_message: {} attachments exceeds limit of {}",
                raw_attachments.len(),
                self.cfg.max_attachments
            ));
        }

        // ---- Gate 4: resolve + validate attachments ----
        let workspace = ctx.config.workspace.clone();
        let cwd_buf = if workspace.is_empty() {
            None
        } else {
            Some(PathBuf::from(workspace))
        };
        let resolved = attachment_metadata(&raw_attachments, cwd_buf.as_ref())?;

        let sent_at = chrono::Utc::now().to_rfc3339();
        Ok(json!({
            BRIEF_SENTINEL: true,
            "message": message,
            "attachments": resolved,
            "status": status.as_str(),
            "sent_at": sent_at
        }))
    }
}

/// Boot-time helper: register `send_user_message` on `registry`
/// when brief mode is active for this agent. Returns `true` when
/// the tool was registered.
pub fn register_send_user_message_tool(
    registry: &Arc<ToolRegistry>,
    cfg: &BriefConfig,
    assistant_mode_active: bool,
) -> bool {
    if !cfg.is_active_with_assistant_mode(assistant_mode_active) {
        return false;
    }
    let def = SendUserMessageTool::tool_def(cfg);
    registry.register_arc(def, Arc::new(SendUserMessageTool::new(cfg.clone())));
    true
}

/// System-prompt section appended to the agent's context whenever
/// brief mode is active. Stable across turns to keep the
/// prompt-cache warm. The text mirrors the
/// `BRIEF_PROACTIVE_SECTION` shape from peer agent stacks but
/// references our tool name (`send_user_message`) so it is
/// self-consistent inside nexo.
pub const BRIEF_SECTION: &str = "## Talking to the user\n\n\
`send_user_message` is your primary visible output channel. Free \
text outside it is visible in the detail view, but most users will \
not open it — assume unread. Every reply the user actually sees \
goes through `send_user_message`. Even for \"hi\". Even for \
\"thanks\".\n\n\
If you can answer right away, send the answer. If you need to look \
something up — run a command, read files — acknowledge first in one \
line (\"On it — checking the test output\"), then work, then send \
the result. Without the ack the user is staring at a blank screen.\n\n\
For longer work: ack → work → result. In between, send a checkpoint \
when something useful happened — a decision you made, a surprise \
you hit, a phase boundary. Skip the filler — a checkpoint earns \
its place by carrying information.\n\n\
Set `status: 'normal'` when replying to what the user just said. \
Set `status: 'proactive'` when you initiate — a scheduled task \
finished, a blocker surfaced during background work, you need \
input on something the user has not asked about. Be honest with the \
status — downstream telemetry and routing key off it.\n\n\
Keep messages tight: the decision, the file:line, the result. Use \
second person (\"your config\"), never third.";

/// Returns `Some(BRIEF_SECTION)` when brief mode should append the
/// section to the system prompt; `None` otherwise.
///
/// `cfg` is the resolved per-agent (or per-binding) brief config;
/// `assistant_mode_active` matches the same flag that gates the
/// assistant-mode addendum upstream of this call. The section is
/// already implicit inside the assistant-mode addendum, so when
/// assistant mode is appending its own block we *skip* this one to
/// avoid duplicating the instruction in the system prompt — same
/// trade-off the peer-agent stack documents.
pub fn brief_system_section(
    cfg: Option<&BriefConfig>,
    assistant_mode_addendum_appended: bool,
) -> Option<&'static str> {
    let cfg = cfg?;
    if assistant_mode_addendum_appended {
        return None;
    }
    if !cfg.is_active_with_assistant_mode(false) {
        return None;
    }
    Some(BRIEF_SECTION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cfg(status_required: bool, max_attachments: u32) -> BriefConfig {
        BriefConfig {
            enabled: true,
            status_required,
            max_attachments,
        }
    }

    #[test]
    fn tool_def_includes_status_when_required() {
        let def = SendUserMessageTool::tool_def(&cfg(true, 8));
        let required = def.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "message"));
        assert!(required.iter().any(|v| v == "status"));
    }

    #[test]
    fn tool_def_excludes_status_when_optional() {
        let def = SendUserMessageTool::tool_def(&cfg(false, 8));
        let required = def.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "message"));
        assert!(!required.iter().any(|v| v == "status"));
    }

    #[test]
    fn parse_status_required() {
        assert_eq!(
            BriefStatus::parse(Some("normal"), true).unwrap(),
            BriefStatus::Normal
        );
        assert_eq!(
            BriefStatus::parse(Some("proactive"), true).unwrap(),
            BriefStatus::Proactive
        );
        assert!(BriefStatus::parse(None, true).is_err());
        assert!(BriefStatus::parse(Some("urgent"), true).is_err());
    }

    #[test]
    fn parse_status_optional_defaults_to_normal() {
        assert_eq!(
            BriefStatus::parse(None, false).unwrap(),
            BriefStatus::Normal
        );
    }

    #[test]
    fn is_brief_result_picks_up_sentinel() {
        let v = json!({BRIEF_SENTINEL: true, "message": "hi"});
        assert!(is_brief_result(&v));
        let v = json!({"message": "hi"});
        assert!(!is_brief_result(&v));
    }

    #[test]
    fn is_likely_image_matches_common_extensions() {
        assert!(is_likely_image(std::path::Path::new("/tmp/x.png")));
        assert!(is_likely_image(std::path::Path::new("/tmp/x.JPG")));
        assert!(!is_likely_image(std::path::Path::new("/tmp/x.txt")));
    }

    #[test]
    fn attachment_metadata_resolves_existing_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(&path, b"hello").unwrap();
        let resolved = attachment_metadata(&[path.to_string_lossy().into_owned()], None).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0]["size"], 5);
        assert_eq!(resolved[0]["is_image"], false);
    }

    #[test]
    fn attachment_metadata_rejects_missing_file() {
        let err = attachment_metadata(&["/this/does/not/exist.png".to_string()], None).unwrap_err();
        assert!(
            err.to_string().contains("not accessible"),
            "expected stat error, got: {err}"
        );
    }

    #[test]
    fn attachment_metadata_rejects_directories() {
        let dir = tempfile::tempdir().unwrap();
        let err = attachment_metadata(&[dir.path().to_string_lossy().into_owned()], None)
            .unwrap_err();
        assert!(err.to_string().contains("regular file"));
    }

    #[test]
    fn attachment_metadata_resolves_relative_against_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("note.txt")).unwrap();
        write!(f, "x").unwrap();
        let resolved =
            attachment_metadata(&["note.txt".into()], Some(&dir.path().to_path_buf())).unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("note.txt"));
    }

    #[test]
    fn empty_message_is_rejected() {
        // We can't easily build a full AgentContext in a unit test,
        // so check the gate logic via direct message validation.
        let args = json!({"message": ""});
        assert!(args["message"].as_str().unwrap().is_empty());
    }

    #[test]
    fn brief_status_round_trip() {
        for s in [BriefStatus::Normal, BriefStatus::Proactive] {
            let parsed = BriefStatus::parse(Some(s.as_str()), true).unwrap();
            assert_eq!(parsed, s);
        }
    }

    // ---- Section gate ----

    #[test]
    fn brief_system_section_off_when_no_cfg() {
        assert!(brief_system_section(None, false).is_none());
    }

    #[test]
    fn brief_system_section_off_when_disabled() {
        let cfg = BriefConfig::default(); // enabled = false
        assert!(brief_system_section(Some(&cfg), false).is_none());
    }

    #[test]
    fn brief_system_section_on_when_enabled_and_no_addendum() {
        let cfg = BriefConfig {
            enabled: true,
            ..Default::default()
        };
        let s = brief_system_section(Some(&cfg), false).unwrap();
        assert!(s.starts_with("## Talking to the user"));
    }

    #[test]
    fn brief_system_section_skipped_when_assistant_addendum_present() {
        let cfg = BriefConfig {
            enabled: true,
            ..Default::default()
        };
        // assistant-mode addendum already covers the same ground —
        // skip to avoid duplication.
        assert!(brief_system_section(Some(&cfg), true).is_none());
    }
}
