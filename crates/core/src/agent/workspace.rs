//! Agent workspace loader — Phase 10.1–10.3.
//!
//! Loads the set of markdown files that make up an agent's "self":
//! IDENTITY.md (parsed), SOUL.md + USER.md + AGENTS.md (raw text), recent
//! daily notes, and MEMORY.md. Modeled on OpenClaw's workspace layout
//! (`docs/concepts/agent-workspace.md`) but scoped to what `LlmAgentBehavior`
//! needs to inject into the system prompt.
//!
//! Design rules:
//! - `MEMORY.md` only loads in **main** sessions (direct DMs). Group/broadcast
//!   sessions must not see it — enforced at load time, not via query filters.
//! - Bootstrap char limits (`max_per_file`, `max_total`) mirror OpenClaw's
//!   defaults (12_000 / 60_000) to keep prompt overhead predictable.
//! - Missing files are silently skipped; a missing workspace is not an error
//!   (agents without workspaces still work — they just have no persona layer).
//! - No writes here — the loader is read-only. Writes belong to memory tools.
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
/// Parsed contents of a workspace directory — everything the LLM turn needs.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceBundle {
    pub identity: Option<AgentIdentity>,
    pub soul: Option<String>,
    pub user: Option<String>,
    pub agents: Option<String>,
    pub daily_notes: Vec<DailyNote>,
    pub long_term_memory: Option<String>,
    /// Extra rule / context MDs from `AgentConfig::extra_docs`. Each
    /// entry is `(filename, content)`; rendered as its own `# RULES —
    /// <filename>` block after the standard sections.
    pub extra_docs: Vec<(String, String)>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub name: Option<String>,
    pub creature: Option<String>,
    pub vibe: Option<String>,
    pub emoji: Option<String>,
    pub avatar: Option<String>,
}
#[derive(Debug, Clone)]
pub struct DailyNote {
    pub date: String,
    pub content: String,
}
/// What kind of session is being bootstrapped. Drives the MEMORY.md gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionScope {
    /// Private DM with the primary user. `MEMORY.md` is loaded.
    Main,
    /// Group chat, broadcast, or any shared context. `MEMORY.md` is NOT loaded.
    Shared,
}
/// Bootstrap char limits. OpenClaw defaults: 12_000 per file, 60_000 total.
#[derive(Debug, Clone, Copy)]
pub struct LoadLimits {
    pub max_per_file: usize,
    pub max_total: usize,
}
impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            max_per_file: 12_000,
            max_total: 60_000,
        }
    }
}
pub struct WorkspaceLoader {
    root: PathBuf,
    limits: LoadLimits,
}
impl WorkspaceLoader {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            limits: LoadLimits::default(),
        }
    }
    pub fn with_limits(mut self, limits: LoadLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Load everything applicable to the given session scope. Missing files
    /// are skipped; IO errors bubble up so callers can decide whether the
    /// workspace is misconfigured vs genuinely empty.
    pub async fn load(&self, scope: SessionScope) -> anyhow::Result<WorkspaceBundle> {
        self.load_with_extras(scope, &[]).await
    }
    /// Same as `load` but additionally reads each path in `extra_docs`
    /// (workspace-relative) into `WorkspaceBundle::extra_docs`. Intended
    /// for topic-scoped rule files (`SALES_SCRIPT.md`, etc.) that the
    /// agent should treat as hard context alongside IDENTITY/SOUL.
    // Sequential awaits against a shared budget make a single struct-literal
    // init unreadable; per-field mutation is clearer here. Silence the lint
    // for this one function rather than hurt legibility.
    #[allow(clippy::field_reassign_with_default)]
    pub async fn load_with_extras(
        &self,
        scope: SessionScope,
        extra_docs: &[String],
    ) -> anyhow::Result<WorkspaceBundle> {
        let mut budget = self.limits.max_total;
        let mut bundle = WorkspaceBundle::default();
        bundle.identity = self
            .read_opt("IDENTITY.md", &mut budget)
            .await?
            .map(|s| parse_identity(&s));
        bundle.soul = self.read_opt("SOUL.md", &mut budget).await?;
        bundle.user = self.read_opt("USER.md", &mut budget).await?;
        bundle.agents = self.read_opt("AGENTS.md", &mut budget).await?;
        // Daily notes: today and yesterday (UTC). Missing files are skipped.
        let today = Utc::now().date_naive();
        for offset in [0i64, 1] {
            let date = today - Duration::days(offset);
            let rel = format!("memory/{}.md", date.format("%Y-%m-%d"));
            if let Some(content) = self.read_opt(&rel, &mut budget).await? {
                bundle.daily_notes.push(DailyNote {
                    date: date.format("%Y-%m-%d").to_string(),
                    content,
                });
            }
        }
        // MEMORY.md is the privacy boundary — only loaded in main sessions.
        if scope == SessionScope::Main {
            bundle.long_term_memory = self.read_opt("MEMORY.md", &mut budget).await?;
        }
        // Extra rule docs — scoped context the agent must respect. Load
        // after MEMORY so the standard blocks always win the budget in
        // contention. A missing extra doc logs a warning but doesn't fail
        // the turn (could be a typo in config; better to keep the agent
        // running than to hard-stop).
        for rel in extra_docs {
            let rel = rel.trim();
            if rel.is_empty() {
                continue;
            }
            match self.read_opt(rel, &mut budget).await {
                Ok(Some(content)) => bundle.extra_docs.push((rel.to_string(), content)),
                Ok(None) => tracing::warn!(
                    workspace = %self.root.display(),
                    doc = %rel,
                    "extra_doc listed in config not found on disk",
                ),
                Err(e) => tracing::warn!(
                    workspace = %self.root.display(),
                    doc = %rel,
                    error = %e,
                    "failed to read extra_doc; continuing",
                ),
            }
        }
        Ok(bundle)
    }
    /// Read a workspace-relative file, apply per-file truncation, subtract
    /// from the global budget. Returns `Ok(None)` when the file is absent.
    /// Relative paths containing `..` or absolute paths are rejected so
    /// a crafted `extra_docs` entry cannot read files outside the
    /// agent's workspace root.
    async fn read_opt(&self, rel: &str, budget: &mut usize) -> anyhow::Result<Option<String>> {
        if *budget == 0 {
            return Ok(None);
        }
        let rel_path = std::path::Path::new(rel);
        if rel_path.is_absolute() {
            tracing::warn!(
                rel = %rel,
                "workspace read rejected: absolute path not allowed"
            );
            return Ok(None);
        }
        if rel_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            tracing::warn!(
                rel = %rel,
                "workspace read rejected: `..` traversal not allowed"
            );
            return Ok(None);
        }
        let path = self.root.join(rel);
        let limit = self.limits.max_per_file.min(*budget);
        // Read at most `limit + 1` bytes: enough to detect overflow
        // without paying for huge files (SOUL.md can grow to 100KB+).
        // `+1` guarantees `truncate` emits the [truncated] marker when
        // the file exceeds the limit.
        let read_cap = limit.saturating_add(1);
        let bytes = match read_bounded(&path, read_cap).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("failed to read {}: {e}", path.display())),
        };
        // File content must be valid UTF-8; if the cap chopped mid-char,
        // `from_utf8_lossy` handles it gracefully.
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        let trimmed = truncate(content, limit);
        *budget = budget.saturating_sub(trimmed.len());
        Ok(Some(trimmed))
    }
}
/// Read up to `limit` bytes from `path`. Uses `File::take` so huge
/// files don't trigger a full buffered read.
async fn read_bounded(path: &std::path::Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await?;
    let mut buf = Vec::with_capacity(limit.min(8192));
    file.take(limit as u64).read_to_end(&mut buf).await?;
    Ok(buf)
}
fn truncate(mut s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    // Find a char boundary at or below the limit so we never split a utf-8 scalar.
    let mut cut = limit;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n\n[truncated]");
    s
}
/// Parse IDENTITY.md — 5 bullet-list fields: Name, Creature, Vibe, Emoji, Avatar.
/// Values wrapped in parens (placeholder hints from the template) are skipped
/// so the bootstrap template doesn't leak into the agent's persona.
pub fn parse_identity(content: &str) -> AgentIdentity {
    let mut id = AgentIdentity::default();
    for line in content.lines() {
        let Some(stripped) = line.trim_start().strip_prefix('-') else {
            continue;
        };
        let stripped = stripped.trim_start();
        // Expected shape: `**Name:** value` or `Name: value` or with italics.
        let (key, val) = match extract_field(stripped) {
            Some(kv) => kv,
            None => continue,
        };
        let val = val.trim();
        if val.is_empty() || is_placeholder(val) {
            continue;
        }
        let slot = match key.to_ascii_lowercase().as_str() {
            "name" => &mut id.name,
            "creature" => &mut id.creature,
            "vibe" => &mut id.vibe,
            "emoji" => &mut id.emoji,
            "avatar" => &mut id.avatar,
            _ => continue,
        };
        *slot = Some(val.to_string());
    }
    id
}
fn extract_field(line: &str) -> Option<(String, String)> {
    // Strip markdown emphasis around the key: **Name:** or *Name:* or Name:
    let line = line.trim_start_matches('*').trim();
    let colon = line.find(':')?;
    let (raw_key, rest) = line.split_at(colon);
    let key = raw_key.trim_end_matches('*').trim().to_string();
    let value = rest[1..].trim_start_matches('*').trim().to_string();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}
fn is_placeholder(val: &str) -> bool {
    let v = val.trim();
    // Template placeholders are wrapped in italics + parens: "_(pick something)_".
    (v.starts_with("_(") && v.ends_with(")_")) || (v.starts_with('(') && v.ends_with(')'))
}
impl WorkspaceBundle {
    /// Render the bundle as a set of ordered blocks suitable for prefixing
    /// the system prompt. Each block is tagged so the model can distinguish
    /// persona/identity from memory. Returns `None` when nothing was loaded.
    pub fn render_system_blocks(&self) -> Option<String> {
        let mut out = String::new();
        if let Some(id) = &self.identity {
            let mut fields = Vec::new();
            if let Some(v) = &id.name {
                fields.push(format!("name={v}"));
            }
            if let Some(v) = &id.creature {
                fields.push(format!("creature={v}"));
            }
            if let Some(v) = &id.vibe {
                fields.push(format!("vibe={v}"));
            }
            if let Some(v) = &id.emoji {
                fields.push(format!("emoji={v}"));
            }
            if !fields.is_empty() {
                out.push_str("# IDENTITY\n");
                out.push_str(&fields.join(", "));
                out.push_str("\n\n");
            }
        }
        if let Some(soul) = &self.soul {
            out.push_str("# SOUL\n");
            out.push_str(soul.trim());
            out.push_str("\n\n");
        }
        if let Some(user) = &self.user {
            out.push_str("# USER\n");
            out.push_str(user.trim());
            out.push_str("\n\n");
        }
        if let Some(agents) = &self.agents {
            out.push_str("# AGENTS\n");
            out.push_str(agents.trim());
            out.push_str("\n\n");
        }
        if !self.daily_notes.is_empty() {
            out.push_str("# RECENT NOTES\n");
            for note in &self.daily_notes {
                out.push_str(&format!("## {}\n{}\n\n", note.date, note.content.trim()));
            }
        }
        if let Some(memory) = &self.long_term_memory {
            out.push_str("# MEMORY\n");
            out.push_str(memory.trim());
            out.push_str("\n\n");
        }
        for (filename, content) in &self.extra_docs {
            out.push_str(&format!("# RULES — {}\n", filename));
            out.push_str(content.trim());
            out.push_str("\n\n");
        }
        if out.is_empty() {
            None
        } else {
            Some(out.trim_end().to_string())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_identity_fields_skipping_placeholders() {
        let src = "\
# IDENTITY.md
- **Name:** Kate
- **Creature:** _(AI? robot? familiar?)_
- **Vibe:** warm but sharp
- **Emoji:** 🐙
- **Avatar:**
";
        let id = parse_identity(src);
        assert_eq!(id.name.as_deref(), Some("Kate"));
        assert!(
            id.creature.is_none(),
            "template placeholder must be skipped"
        );
        assert_eq!(id.vibe.as_deref(), Some("warm but sharp"));
        assert_eq!(id.emoji.as_deref(), Some("🐙"));
        assert!(id.avatar.is_none(), "empty value must be skipped");
    }
    #[test]
    fn parses_identity_without_bold_markers() {
        let id = parse_identity("- Name: Plain\n- Emoji: 🤖\n");
        assert_eq!(id.name.as_deref(), Some("Plain"));
        assert_eq!(id.emoji.as_deref(), Some("🤖"));
    }
    #[test]
    fn truncate_respects_char_boundaries() {
        // 3 bytes per ñ — must not split mid-codepoint.
        let s = "ññññññ".to_string(); // 12 bytes
        let out = truncate(s, 5);
        assert!(out.starts_with('ñ'));
        assert!(out.ends_with("[truncated]"));
    }
    #[tokio::test]
    async fn missing_workspace_yields_empty_bundle() {
        let tmp =
            std::env::temp_dir().join(format!("agent-core-ws-missing-{}", uuid::Uuid::new_v4()));
        let loader = WorkspaceLoader::new(&tmp);
        let bundle = loader.load(SessionScope::Main).await.unwrap();
        assert!(bundle.identity.is_none());
        assert!(bundle.soul.is_none());
        assert!(bundle.daily_notes.is_empty());
        assert!(bundle.long_term_memory.is_none());
    }
    #[tokio::test]
    async fn shared_scope_omits_long_term_memory() -> anyhow::Result<()> {
        let tmp =
            std::env::temp_dir().join(format!("agent-core-ws-shared-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await?;
        tokio::fs::write(tmp.join("MEMORY.md"), "secret preferences").await?;
        tokio::fs::write(tmp.join("SOUL.md"), "be useful").await?;
        let loader = WorkspaceLoader::new(&tmp);
        let main = loader.load(SessionScope::Main).await?;
        assert!(main.long_term_memory.is_some());
        assert_eq!(main.soul.as_deref(), Some("be useful"));
        let shared = loader.load(SessionScope::Shared).await?;
        assert!(
            shared.long_term_memory.is_none(),
            "MEMORY.md must not leak into shared sessions"
        );
        assert_eq!(shared.soul.as_deref(), Some("be useful"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn render_system_blocks_includes_only_loaded_sections() -> anyhow::Result<()> {
        let tmp =
            std::env::temp_dir().join(format!("agent-core-ws-render-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await?;
        tokio::fs::write(
            tmp.join("IDENTITY.md"),
            "- **Name:** Kate\n- **Emoji:** 🐙\n",
        )
        .await?;
        tokio::fs::write(tmp.join("SOUL.md"), "have opinions").await?;
        let bundle = WorkspaceLoader::new(&tmp).load(SessionScope::Main).await?;
        let rendered = bundle.render_system_blocks().unwrap();
        assert!(rendered.contains("# IDENTITY"));
        assert!(rendered.contains("name=Kate"));
        assert!(rendered.contains("# SOUL"));
        assert!(rendered.contains("have opinions"));
        assert!(!rendered.contains("# USER"), "no USER.md was written");
        assert!(!rendered.contains("# MEMORY"), "no MEMORY.md was written");
        tokio::fs::remove_dir_all(&tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn extra_docs_render_after_standard_blocks() -> anyhow::Result<()> {
        let tmp =
            std::env::temp_dir().join(format!("agent-core-ws-extra-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await?;
        tokio::fs::write(tmp.join("SOUL.md"), "be useful").await?;
        tokio::fs::write(
            tmp.join("SALES_SCRIPT.md"),
            "Step 1: greet. Step 2: qualify. Step 3: close.",
        )
        .await?;
        tokio::fs::write(
            tmp.join("PRODUCT_CATALOG.md"),
            "- Widget A: $10\n- Widget B: $20",
        )
        .await?;
        let loader = WorkspaceLoader::new(&tmp);
        let bundle = loader
            .load_with_extras(
                SessionScope::Main,
                &[
                    "SALES_SCRIPT.md".to_string(),
                    "PRODUCT_CATALOG.md".to_string(),
                    "".to_string(),           // empty entry is ignored
                    "MISSING.md".to_string(), // logs warning, doesn't fail
                ],
            )
            .await?;
        let rendered = bundle.render_system_blocks().unwrap();
        let soul_idx = rendered.find("# SOUL").expect("soul present");
        let sales_idx = rendered
            .find("# RULES — SALES_SCRIPT.md")
            .expect("sales block");
        let catalog_idx = rendered
            .find("# RULES — PRODUCT_CATALOG.md")
            .expect("catalog block");
        // Extra blocks render AFTER the core SOUL block.
        assert!(soul_idx < sales_idx, "SOUL must come before RULES");
        assert!(sales_idx < catalog_idx, "RULES order preserved");
        assert!(rendered.contains("Step 1: greet"));
        assert!(rendered.contains("Widget A: $10"));
        assert!(!rendered.contains("MISSING"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn per_file_truncation_applied() -> anyhow::Result<()> {
        let tmp =
            std::env::temp_dir().join(format!("agent-core-ws-trunc-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&tmp).await?;
        let big = "a".repeat(20_000);
        tokio::fs::write(tmp.join("SOUL.md"), &big).await?;
        let loader = WorkspaceLoader::new(&tmp).with_limits(LoadLimits {
            max_per_file: 1_000,
            max_total: 60_000,
        });
        let bundle = loader.load(SessionScope::Main).await?;
        let soul = bundle.soul.unwrap();
        assert!(soul.len() <= 1_000 + "\n\n[truncated]".len());
        assert!(soul.ends_with("[truncated]"));
        tokio::fs::remove_dir_all(&tmp).await.ok();
        Ok(())
    }
}
