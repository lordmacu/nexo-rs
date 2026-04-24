// Phase 10.8 — agent self-report tools.
//
// Three LLM-callable tools so the agent can describe its own state on demand:
//
//   * `who_am_i`      — identity fields + SOUL excerpt
//   * `what_do_i_know` — MEMORY.md parsed into sections + bullets
//   * `my_stats`      — numeric counts + top concept tags
//
// All read-only; no state mutation. Bounded response size per tool.
use super::context::AgentContext;
use super::tool_registry::ToolHandler;
use super::workspace::parse_identity;
use agent_llm::ToolDef;
use agent_memory::LongTermMemory;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
const SOUL_EXCERPT_MAX: usize = 2048;
const MEMORY_BODY_MAX: usize = 6144;
const MEMORY_DEFAULT_LIMIT: usize = 20;
const MEMORY_HARD_CAP: usize = 50;
const MEMORY_BULLETS_PER_SECTION: usize = 10;
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const TOP_TAGS_LIMIT: usize = 10;
const WORKSPACE_FILES: &[&str] = &[
    "IDENTITY.md",
    "SOUL.md",
    "USER.md",
    "AGENTS.md",
    "MEMORY.md",
    "DREAMS.md",
];
// ─── who_am_i ─────────────────────────────────────────────────────────────────
pub struct WhoAmITool {
    agent_id: String,
    model_name: String,
    workspace_dir: Option<PathBuf>,
}
impl WhoAmITool {
    pub fn new(
        agent_id: impl Into<String>,
        model_name: impl Into<String>,
        workspace_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            model_name: model_name.into(),
            workspace_dir,
        }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "who_am_i".to_string(),
            description:
                "Return the agent's own identity: id, configured LLM model, workspace path, \
                 parsed IDENTITY.md fields, and a short SOUL.md excerpt. No arguments."
                    .to_string(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }
}
#[async_trait]
impl ToolHandler for WhoAmITool {
    async fn call(&self, _ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let workspace_dir_str = self.workspace_dir.as_ref().map(|p| p.display().to_string());
        let identity_json = match self.workspace_dir.as_deref() {
            Some(dir) => match read_optional(&dir.join("IDENTITY.md")).await {
                Some(content) => {
                    let id = parse_identity(&content);
                    json!({
                        "name": id.name,
                        "creature": id.creature,
                        "vibe": id.vibe,
                        "emoji": id.emoji,
                        "avatar": id.avatar,
                    })
                }
                None => Value::Null,
            },
            None => Value::Null,
        };
        let soul_excerpt = match self.workspace_dir.as_deref() {
            Some(dir) => read_optional(&dir.join("SOUL.md"))
                .await
                .map(|s| truncate_with_ellipsis(&s, SOUL_EXCERPT_MAX)),
            None => None,
        };
        Ok(json!({
            "agent_id": self.agent_id,
            "model": self.model_name,
            "workspace_dir": workspace_dir_str,
            "identity": identity_json,
            "soul_excerpt": soul_excerpt,
        }))
    }
}
// ─── what_do_i_know ───────────────────────────────────────────────────────────
pub struct WhatDoIKnowTool {
    workspace_dir: Option<PathBuf>,
}
impl WhatDoIKnowTool {
    pub fn new(workspace_dir: Option<PathBuf>) -> Self {
        Self { workspace_dir }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "what_do_i_know".to_string(),
            description:
                "Return a structured summary of MEMORY.md — consolidated facts the agent has \
                 promoted via dreaming. Optional: filter by section heading substring, limit \
                 number of sections."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "section": {
                        "type": "string",
                        "description": "Case-insensitive substring; only sections whose heading matches are returned"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max sections (default 20, hard cap 50)"
                    }
                },
                "required": []
            }),
        }
    }
}
#[async_trait]
impl ToolHandler for WhatDoIKnowTool {
    async fn call(&self, _ctx: &AgentContext, args: Value) -> anyhow::Result<Value> {
        let workspace_dir_str = self.workspace_dir.as_ref().map(|p| p.display().to_string());
        let Some(dir) = self.workspace_dir.as_deref() else {
            return Ok(json!({
                "workspace_dir": null,
                "sections": [],
                "truncated": false,
            }));
        };
        let Some(raw) = read_optional(&dir.join("MEMORY.md")).await else {
            return Ok(json!({
                "workspace_dir": workspace_dir_str,
                "sections": [],
                "truncated": false,
            }));
        };
        let section_filter = args
            .get("section")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(MEMORY_HARD_CAP))
            .unwrap_or(MEMORY_DEFAULT_LIMIT);
        let all_sections = parse_memory_sections(&raw);
        let mut out: Vec<Value> = Vec::new();
        let mut bytes_used = 0usize;
        let mut truncated = false;
        for section in all_sections {
            if let Some(filter) = section_filter.as_deref() {
                if !section.heading.to_lowercase().contains(filter) {
                    continue;
                }
            }
            if out.len() >= limit {
                truncated = true;
                break;
            }
            let (bullets, extra) = if section.bullets.len() > MEMORY_BULLETS_PER_SECTION {
                (
                    section.bullets[..MEMORY_BULLETS_PER_SECTION].to_vec(),
                    Some(section.bullets.len() - MEMORY_BULLETS_PER_SECTION),
                )
            } else {
                (section.bullets.clone(), None)
            };
            let entry = json!({
                "heading": section.heading,
                "bullets": bullets,
                "extra_bullets": extra,
            });
            let serialized_len = serde_json::to_string(&entry).map(|s| s.len()).unwrap_or(0);
            if bytes_used + serialized_len > MEMORY_BODY_MAX && !out.is_empty() {
                truncated = true;
                break;
            }
            bytes_used += serialized_len;
            out.push(entry);
        }
        Ok(json!({
            "workspace_dir": workspace_dir_str,
            "sections": out,
            "truncated": truncated,
        }))
    }
}
// ─── my_stats ─────────────────────────────────────────────────────────────────
pub struct MyStatsTool {
    memory: Arc<LongTermMemory>,
    workspace_dir: Option<PathBuf>,
}
impl MyStatsTool {
    pub fn new(memory: Arc<LongTermMemory>, workspace_dir: Option<PathBuf>) -> Self {
        Self {
            memory,
            workspace_dir,
        }
    }
    pub fn tool_def() -> ToolDef {
        ToolDef {
            name: "my_stats".to_string(),
            description:
                "Return numeric self-stats: session count, memories stored, memories promoted \
                 by dreaming, last dream timestamp, recall events in the last 7 days, and top \
                 concept tags from recent recalls. No arguments."
                    .to_string(),
            parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }
}
#[async_trait]
impl ToolHandler for MyStatsTool {
    async fn call(&self, ctx: &AgentContext, _args: Value) -> anyhow::Result<Value> {
        let agent_id = ctx.agent_id.as_str();
        let now_ms = Utc::now().timestamp_millis();
        let since_ms = now_ms - WEEK_MS;
        let (sessions_total, memories_stored, memories_promoted, last_dream, recall_7d, top_tags) =
            tokio::try_join!(
                self.memory.count_sessions(agent_id),
                self.memory.count_memories(agent_id),
                self.memory.count_promotions(agent_id),
                self.memory.last_promotion_ts(agent_id),
                self.memory.count_recall_events_since(agent_id, since_ms),
                self.memory
                    .top_concept_tags_since(agent_id, since_ms, TOP_TAGS_LIMIT),
            )?;
        let workspace_files_present = match self.workspace_dir.as_deref() {
            Some(dir) => workspace_files_present(dir).await,
            None => Vec::new(),
        };
        Ok(json!({
            "agent_id": agent_id,
            "sessions_total": sessions_total,
            "memories_stored": memories_stored,
            "memories_promoted": memories_promoted,
            "last_dream_ts": last_dream.map(|ts| ts.to_rfc3339()),
            "recall_events_7d": recall_7d,
            "top_concept_tags_7d": top_tags
                .into_iter()
                .map(|(t, c)| json!([t, c]))
                .collect::<Vec<_>>(),
            "workspace_files_present": workspace_files_present,
        }))
    }
}
// ─── helpers ──────────────────────────────────────────────────────────────────
async fn read_optional(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}
async fn workspace_files_present(dir: &Path) -> Vec<String> {
    let mut present = Vec::new();
    for name in WORKSPACE_FILES {
        let p = dir.join(name);
        if tokio::fs::metadata(&p).await.is_ok() {
            present.push((*name).to_string());
        }
    }
    present
}
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}
#[derive(Debug, Clone)]
pub(crate) struct MemorySection {
    pub heading: String,
    pub bullets: Vec<String>,
}
/// Parse MEMORY.md into sections:
///   - A line starting with `## ` opens a new section (heading = remainder).
///   - A line starting with `- ` or `* ` is appended as a bullet to the current
///     section. Trailing whitespace stripped.
///   - Everything else is ignored (paragraphs, code fences, etc).
///
/// Sections that start before any heading are dropped. Empty heading → skipped.
pub(crate) fn parse_memory_sections(src: &str) -> Vec<MemorySection> {
    let mut out: Vec<MemorySection> = Vec::new();
    let mut current: Option<MemorySection> = None;
    for raw in src.lines() {
        let line = raw.trim_end();
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(prev) = current.take() {
                if !prev.heading.is_empty() {
                    out.push(prev);
                }
            }
            let heading = heading.trim().to_string();
            if !heading.is_empty() {
                current = Some(MemorySection {
                    heading,
                    bullets: Vec::new(),
                });
            }
            continue;
        }
        if let Some(sec) = current.as_mut() {
            let bullet = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "));
            if let Some(b) = bullet {
                let b = b.trim().to_string();
                if !b.is_empty() {
                    sec.bullets.push(b);
                }
            }
        }
    }
    if let Some(prev) = current {
        if !prev.heading.is_empty() {
            out.push(prev);
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_memory_empty() {
        assert!(parse_memory_sections("").is_empty());
    }
    #[test]
    fn parse_memory_basic_sections() {
        let src = "# Title\n\n## Preferences\n- short answers\n- spanish\n\n## Facts\n- lives in madrid\n- likes coffee\n";
        let secs = parse_memory_sections(src);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].heading, "Preferences");
        assert_eq!(secs[0].bullets, vec!["short answers", "spanish"]);
        assert_eq!(secs[1].heading, "Facts");
        assert_eq!(secs[1].bullets.len(), 2);
    }
    #[test]
    fn parse_memory_ignores_stray_bullets_before_heading() {
        let src = "- orphan bullet\n## Real\n- real bullet\n";
        let secs = parse_memory_sections(src);
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].heading, "Real");
        assert_eq!(secs[0].bullets, vec!["real bullet"]);
    }
    #[test]
    fn parse_memory_supports_asterisk_bullets() {
        let src = "## H\n* one\n* two\n";
        let secs = parse_memory_sections(src);
        assert_eq!(secs[0].bullets, vec!["one", "two"]);
    }
    #[test]
    fn truncate_respects_char_limit() {
        let s = "a".repeat(3000);
        let t = truncate_with_ellipsis(&s, SOUL_EXCERPT_MAX);
        assert_eq!(t.chars().count(), SOUL_EXCERPT_MAX + 1); // + ellipsis
        assert!(t.ends_with('…'));
    }
    #[test]
    fn truncate_below_limit_unchanged() {
        let s = "hello";
        assert_eq!(truncate_with_ellipsis(s, 100), "hello");
    }
}
