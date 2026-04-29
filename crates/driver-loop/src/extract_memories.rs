//! Phase 77.5 — post-turn LLM memory extraction.
//!
//! Ported from `claude-code-leak/src/services/extractMemories/`.
//!
//! After every N eligible turns, a small LLM call reads the recent
//! transcript and writes durable memories to the memory directory
//! (`~/.claude/projects/<path>/memory/*.md` + `MEMORY.md`).
//!
//! Single-turn approach: the manifest is pre-injected into the prompt so
//! the LLM can decide what to update without exploration. Response is
//! parsed as a JSON array of `{file_path, content}` objects.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use nexo_driver_types::{ExtractMemoriesConfig, GoalId};

use crate::events::{DriverEvent, ExtractSkipReason};
use crate::extract_memories_prompt::build_extract_prompt;

// ── LLM backend trait ────────────────────────────────────────────────

/// Narrow LLM interface for memory extraction. Decoupled from
/// `nexo_llm::LlmClient` so the binary crate can wire the real client
/// without pulling provider-specific code into the driver loop.
#[async_trait]
pub trait ExtractMemoriesLlm: Send + Sync + 'static {
    /// Send a chat request and return the text content of the response.
    async fn chat(
        &self,
        system_prompt: &str,
        user_messages: &str,
        max_tokens: u32,
    ) -> Result<String, String>;
}

// ── Extraction result ────────────────────────────────────────────────

/// Parsed from the LLM's JSON response.
#[derive(Debug, serde::Deserialize)]
struct MemoryFile {
    file_path: String,
    content: String,
}

/// Outcome of a single extraction run.
#[derive(Debug)]
pub struct ExtractMemoriesOutcome {
    pub memories_saved: u32,
    pub duration_ms: u64,
}

// ── State ────────────────────────────────────────────────────────────

/// Context stashed when extraction is already in-flight.
struct PendingExtraction {
    messages_text: String,
    memory_dir: PathBuf,
}

struct ExtractMemoriesState {
    /// UUID of the last message seen in a successful extraction.
    last_message_uuid: Option<String>,
    /// Guard against concurrent extractions.
    in_progress: bool,
    /// Turns since the last extraction attempt (success or failure).
    turns_since_last: u32,
    /// Queued extraction when one was already in-flight.
    pending: Option<PendingExtraction>,
    /// Consecutive failures for the circuit breaker.
    consecutive_failures: u32,
}

impl ExtractMemoriesState {
    fn new() -> Self {
        Self {
            last_message_uuid: None,
            in_progress: false,
            turns_since_last: 0,
            pending: None,
            consecutive_failures: 0,
        }
    }
}

// ── Public struct ────────────────────────────────────────────────────

pub struct ExtractMemories {
    config: ExtractMemoriesConfig,
    state: Mutex<ExtractMemoriesState>,
    llm: Arc<dyn ExtractMemoriesLlm>,
    /// How many recent messages to feed into the extraction prompt.
    new_message_count: u32,
    /// Phase 77.7 — secret guard for scanning extracted memory content
    /// before writing to disk. None = no scanning (backward compat).
    guard: Option<nexo_memory::SecretGuard>,
}

impl ExtractMemories {
    pub fn new(
        config: ExtractMemoriesConfig,
        llm: Arc<dyn ExtractMemoriesLlm>,
    ) -> Self {
        Self {
            config,
            state: Mutex::new(ExtractMemoriesState::new()),
            llm,
            new_message_count: 20,
            guard: None,
        }
    }

    /// Override the number of recent messages injected into the prompt.
    pub fn with_message_count(mut self, n: u32) -> Self {
        self.new_message_count = n;
        self
    }

    /// Phase 77.7 — attach a secret guard for scanning extracted memory
    /// content before writing to disk.
    pub fn with_guard(mut self, guard: nexo_memory::SecretGuard) -> Self {
        self.guard = Some(guard);
        self
    }

    // ── Gate checks ──────────────────────────────────────────────

    /// Run all pre-extraction gates. Returns `Ok(())` if extraction
    /// should proceed, or `Err(reason)` if it should be skipped.
    pub fn check_gates(&self) -> Result<(), ExtractSkipReason> {
        if !self.config.enabled {
            return Err(ExtractSkipReason::Disabled);
        }

        let state = self.state.lock().unwrap();

        // Throttle: run every N turns.
        if state.turns_since_last < self.config.turns_throttle.saturating_sub(1) {
            return Err(ExtractSkipReason::Throttled);
        }

        // Mutual exclusion: don't stack extractions.
        if state.in_progress {
            return Err(ExtractSkipReason::InProgress);
        }

        // Circuit breaker: stop after N consecutive failures.
        if self.config.max_consecutive_failures > 0
            && state.consecutive_failures >= self.config.max_consecutive_failures
        {
            return Err(ExtractSkipReason::CircuitBreakerOpen);
        }

        Ok(())
    }

    /// Bump the turn counter. Called every turn regardless of whether
    /// extraction runs.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap();
        state.turns_since_last = state.turns_since_last.saturating_add(1);
    }

    /// Mark the extraction as started (sets `in_progress = true`).
    /// Returns the current `turns_since_last` for later reset on success.
    fn mark_started(&self) {
        let mut state = self.state.lock().unwrap();
        state.in_progress = true;
    }

    /// Record a successful extraction.
    fn record_success(&self, last_message_uuid: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.in_progress = false;
        state.turns_since_last = 0;
        state.consecutive_failures = 0;
        state.last_message_uuid = last_message_uuid;
    }

    /// Record a failed extraction.
    fn record_failure(&self) {
        let mut state = self.state.lock().unwrap();
        state.in_progress = false;
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
    }

    /// Stash a pending extraction when one is already in-flight.
    /// The trailing extraction runs after the current one completes.
    pub fn stash_pending(&self, messages_text: String, memory_dir: PathBuf) {
        let mut state = self.state.lock().unwrap();
        state.pending = Some(PendingExtraction {
            messages_text,
            memory_dir,
        });
    }

    /// Take the stashed pending extraction, if any.
    fn take_pending(&self) -> Option<PendingExtraction> {
        let mut state = self.state.lock().unwrap();
        state.pending.take()
    }

    // ── Public entry point ───────────────────────────────────────

    /// Run memory extraction, spawning the LLM call on a background task.
    /// Returns immediately; the extraction runs concurrently.
    ///
    /// `messages_text` is the recent conversation serialized as text —
    /// it becomes the user message in the extraction prompt.
    /// `memory_dir` is the root of the memory filesystem
    /// (e.g. `~/.claude/projects/<project>/memory/`).
    pub fn extract(
        self: &Arc<Self>,
        goal_id: GoalId,
        turn_index: u32,
        messages_text: String,
        memory_dir: PathBuf,
    ) {
        // Gate checks.
        let skip_reason = match self.check_gates() {
            Ok(()) => None,
            Err(reason) => Some(reason),
        };

        if let Some(reason) = skip_reason {
            // Coalesce: stash for later if in-progress.
            if matches!(reason, ExtractSkipReason::InProgress) {
                self.stash_pending(messages_text, memory_dir);
            }
            debug!(
                goal_id = %goal_id.0,
                reason = ?reason,
                "ExtractMemories skipped"
            );
            return;
        }

        self.mark_started();

        let this = Arc::clone(self);
        tokio::spawn(async move {
            let start = Instant::now();
            match this.run_extraction(&messages_text, &memory_dir).await {
                Ok(memories_saved) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    info!(
                        goal_id = %goal_id.0,
                        memories_saved,
                        duration_ms,
                        "ExtractMemories completed"
                    );
                    this.record_success(None);
                    // TODO: emit DriverEvent::ExtractMemoriesCompleted via event_sink.
                    // The event_sink is not available here — the caller (orchestrator)
                    // should emit the event after inspecting the outcome.
                    let _ = (goal_id, turn_index, memories_saved, duration_ms);
                }
                Err(e) => {
                    warn!(
                        goal_id = %goal_id.0,
                        error = %e,
                        "ExtractMemories failed"
                    );
                    this.record_failure();
                }
            }

            // Drain any stashed pending extraction.
            if let Some(pending) = this.take_pending() {
                debug!("ExtractMemories: draining coalesced extraction");
                let start = Instant::now();
                match this.run_extraction(&pending.messages_text, &pending.memory_dir).await {
                    Ok(n) => {
                        this.record_success(None);
                        info!(memories_saved = n, "ExtractMemories coalesced ok");
                    }
                    Err(e) => {
                        warn!(error = %e, "ExtractMemories coalesced failed");
                        this.record_failure();
                    }
                }
                let _ = start; // duration tracking for coalesced
            }
        });
    }

    // ── Core extraction ──────────────────────────────────────────

    async fn run_extraction(
        &self,
        messages_text: &str,
        memory_dir: &Path,
    ) -> Result<u32, String> {
        // 1. Scan existing memory manifest.
        let manifest = scan_memory_manifest(memory_dir).unwrap_or_default();

        // 2. Build prompt.
        let system_prompt = build_extract_prompt(self.new_message_count, &manifest);

        // 3. Call LLM.
        let response_text = self
            .llm
            .chat(&system_prompt, messages_text, self.config.max_turns * 1024)
            .await?;

        // 4. Parse JSON response.
        let files: Vec<MemoryFile> = parse_extraction_response(&response_text)?;

        if files.is_empty() {
            return Ok(0);
        }

        // 5. Validate paths — all must be within memory_dir.
        for f in &files {
            let resolved = resolve_memory_path(memory_dir, &f.file_path)?;
            if !resolved.starts_with(memory_dir) {
                return Err(format!(
                    "path escape attempt: {} -> {}",
                    f.file_path,
                    resolved.display()
                ));
            }
        }

        // 6. Write files, scanning for secrets if guard is attached.
        let mut written = 0u32;
        for f in &files {
            let content_to_write = if let Some(ref guard) = self.guard {
                match guard.check(&f.content) {
                    Ok(redacted) => redacted,
                    Err(e) => {
                        tracing::warn!(
                            target = "memory.secret.blocked",
                            rule_ids = ?e.rule_ids,
                            content_hash = %e.content_hash,
                            file = %f.file_path,
                            "extract_memories: secret blocked, skipping file"
                        );
                        continue; // Block: skip this file, continue with next
                    }
                }
            } else {
                f.content.clone()
            };

            let dest = resolve_memory_path(memory_dir, &f.file_path)?;
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            fs::write(&dest, &content_to_write)
                .map_err(|e| format!("write {}: {e}", dest.display()))?;
            written += 1;
        }

        // 7. Update MEMORY.md index — append pointers for new files.
        update_memory_index(memory_dir, &files)?;

        Ok(written)
    }

    // ── Helpers exposed for testing ──────────────────────────────

    /// Check whether the main agent already wrote to the memory directory
    /// in the given turn messages. If so, skip extraction to avoid
    /// clobbering intentional user-directed writes.
    pub fn has_memory_writes(&self, messages_text: &str, memory_dir: &Path) -> bool {
        has_memory_writes_in_text(messages_text, memory_dir)
    }
}

// ── Manifest scanner ─────────────────────────────────────────────────

/// Scan `memory_dir/*.md` and build a compact manifest string.
/// Each file's YAML frontmatter (`---\n...\n---`) is parsed for
/// `name`, `description`, and `type` fields. The output format is
/// `- [type] filename: description`.
pub fn scan_memory_manifest(memory_dir: &Path) -> Result<String, io::Error> {
    if !memory_dir.exists() {
        return Ok(String::new());
    }

    let mut lines: Vec<String> = Vec::new();
    let entries = fs::read_dir(memory_dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(true, |e| e != "md") {
            continue;
        }
        // Skip MEMORY.md — it's the index, not a memory.
        if path
            .file_name()
            .map_or(false, |n| n == "MEMORY.md")
        {
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        match read_frontmatter(&path) {
            Ok(Some(fm)) => {
                let mem_type = fm.get("type").and_then(|s| s.as_str()).unwrap_or("unknown");
                let _name = fm
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or(file_name.trim_end_matches(".md"));
                let desc = fm
                    .get("description")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                lines.push(format!("- [{mem_type}] {file_name}: {desc}"));
            }
            Ok(None) => {
                // No frontmatter — still list the file.
                lines.push(format!("- [unknown] {file_name}"));
            }
            Err(_) => {
                lines.push(format!("- [unknown] {file_name} (unreadable)"));
            }
        }
    }

    Ok(lines.join("\n"))
}

/// Parse minimal YAML frontmatter from a markdown file.
/// Returns `None` if the file has no frontmatter (no opening `---`).
fn read_frontmatter(path: &Path) -> Result<Option<serde_json::Map<String, serde_json::Value>>, io::Error> {
    let content = fs::read_to_string(path)?;
    let mut lines = content.lines();

    // First line must be exactly "---".
    if lines.next() != Some("---") {
        return Ok(None);
    }

    let mut yaml_lines: Vec<&str> = Vec::new();
    for line in &mut lines {
        if line == "---" {
            break;
        }
        yaml_lines.push(line);
    }

    if yaml_lines.is_empty() {
        return Ok(None);
    }

    let yaml_str = yaml_lines.join("\n");
    let map: serde_json::Map<String, serde_json::Value> =
        serde_yaml::from_str(&yaml_str).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("yaml parse: {e}"))
        })?;

    Ok(Some(map))
}

// ── Response parsing ─────────────────────────────────────────────────

fn parse_extraction_response(text: &str) -> Result<Vec<MemoryFile>, String> {
    // The LLM may wrap the JSON in ```json fences.
    let json_str = text
        .trim()
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(text.trim());

    serde_json::from_str::<Vec<MemoryFile>>(json_str)
        .map_err(|e| format!("parse extraction response: {e}"))
}

// ── Path helpers ─────────────────────────────────────────────────────

/// Resolve a file path from the LLM against the memory directory.
/// Rejects absolute paths, `..` traversal, URL-encoded traversal,
/// Unicode fullwidth trickery, null bytes, and symlink escapes.
fn resolve_memory_path(memory_dir: &Path, file_path: &str) -> Result<PathBuf, String> {
    // ── Null byte rejection ──
    if file_path.contains('\0') {
        return Err(format!("null byte in path: {file_path}"));
    }

    // ── URL-encoded traversal rejection ──
    // Decode percent-encoded sequences and reject if the decoded form
    // contains `..` components. Handles %2e%2e%2f, %2e%2e/, etc.
    let lower = file_path.to_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        // Try to decode and check for traversal in decoded form.
        if let Ok(decoded) = urlencoding_maybe(file_path) {
            if decoded.contains("..") {
                return Err(format!("URL-encoded traversal rejected: {file_path}"));
            }
        }
    }

    // ── Unicode fullwidth traversal rejection ──
    // Fullwidth dots (U+FF0E) and slashes (U+FF0F, U+FF3C) are
    // visually identical to ASCII . and / but bypass naive checks.
    if file_path.contains('\u{FF0E}')
        || file_path.contains('\u{FF0F}')
        || file_path.contains('\u{FF3C}')
        || file_path.contains('\u{2215}') // division slash
    {
        return Err(format!("unicode traversal rejected: {file_path}"));
    }

    let p = Path::new(file_path);
    if p.is_absolute() {
        return Err(format!("absolute path rejected: {file_path}"));
    }

    // Normalize and reject `..` components.
    let mut normalized = PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(format!("path traversal rejected: {file_path}"));
            }
            std::path::Component::Normal(c) => normalized.push(c),
            std::path::Component::CurDir => {}
            _ => return Err(format!("invalid path component in: {file_path}")),
        }
    }

    let resolved = memory_dir.join(&normalized);

    // ── Symlink escape check ──
    // Only meaningful if memory_dir exists on disk.
    if memory_dir.exists() {
        match resolved.canonicalize() {
            Ok(real) => {
                if !real.starts_with(memory_dir) {
                    return Err(format!("symlink escape rejected: {file_path}"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File doesn't exist yet — check parent chain.
                let mut current = resolved.clone();
                while let Some(parent) = current.parent() {
                    if parent.as_os_str().is_empty() {
                        break;
                    }
                    match parent.canonicalize() {
                        Ok(real_parent) => {
                            if !real_parent.starts_with(memory_dir) {
                                return Err(format!(
                                    "symlink escape in parent of: {file_path}"
                                ));
                            }
                            break; // parent is safe
                        }
                        Err(_) => {
                            current = parent.to_path_buf();
                            continue; // check next ancestor
                        }
                    }
                }
            }
            Err(_) => {
                // Other error (permissions, IO) — don't block on it.
            }
        }
    }

    Ok(resolved)
}

/// Minimal URL percent-decoding for traversal detection.
/// Only handles the specific sequences we care about.
fn urlencoding_maybe(input: &str) -> Result<String, ()> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().ok_or(())?;
            let h2 = chars.next().ok_or(())?;
            let byte = u8::from_str_radix(&format!("{h1}{h2}"), 16).map_err(|_| ())?;
            out.push(byte as char);
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

// ── MEMORY.md index ──────────────────────────────────────────────────

/// Append pointers for new files to `MEMORY.md`. Creates the file if it
/// doesn't exist. Skips files already listed in the index.
fn update_memory_index(memory_dir: &Path, files: &[MemoryFile]) -> Result<(), String> {
    let index_path = memory_dir.join("MEMORY.md");
    let existing = if index_path.exists() {
        fs::read_to_string(&index_path).map_err(|e| format!("read MEMORY.md: {e}"))?
    } else {
        String::from("# Memory index\n\n")
    };

    let existing_paths: HashSet<&str> = existing
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("- [")
                .and_then(|rest| rest.split_once("]("))
                .and_then(|(_, rest)| rest.split_once(')').map(|(path, _)| path))
        })
        .collect();

    let mut new_lines: Vec<String> = Vec::new();
    for f in files {
        if existing_paths.contains(f.file_path.as_str()) {
            continue;
        }
        // Extract a short hook from the content: find first non-empty
        // line after the frontmatter block (delimited by `---`).
        let mut in_frontmatter = false;
        let mut closed_frontmatter = false;
        let hook = f
            .content
            .lines()
            .find(|l| {
                if l.trim() == "---" {
                    if !in_frontmatter {
                        in_frontmatter = true;
                    } else if in_frontmatter && !closed_frontmatter {
                        closed_frontmatter = true;
                    }
                    return false;
                }
                // Skip everything inside unclosed frontmatter.
                if in_frontmatter && !closed_frontmatter {
                    return false;
                }
                // After frontmatter, take first non-empty line.
                !l.is_empty()
            })
            .map(|l| {
                let trimmed = l.trim();
                // Truncate to ~80 chars.
                if trimmed.len() > 80 {
                    format!("{}…", &trimmed[..80])
                } else {
                    trimmed.to_string()
                }
            })
            .unwrap_or_default();
        new_lines.push(format!("- [{}]({}) — {}", f.file_path, f.file_path, hook));
    }

    if new_lines.is_empty() {
        return Ok(());
    }

    let mut updated = existing;
    // Trim trailing newlines before appending.
    while updated.ends_with('\n') {
        updated.pop();
    }
    updated.push('\n');
    for line in &new_lines {
        updated.push_str(line);
        updated.push('\n');
    }

    fs::write(&index_path, updated).map_err(|e| format!("write MEMORY.md: {e}"))?;
    Ok(())
}

// ── Memory-write detection ───────────────────────────────────────────

/// Heuristic: scan the message text for tool calls that wrote to the
/// memory directory. Looks for `Write` or `Edit` tool invocations whose
/// file paths fall inside `memory_dir`.
fn has_memory_writes_in_text(messages_text: &str, memory_dir: &Path) -> bool {
    let mem_dir_str = memory_dir.to_string_lossy();
    // Simple heuristic: check if the messages contain the memory dir path
    // near a Write/Edit tool mention.
    let has_memory_path = messages_text.contains(mem_dir_str.as_ref());
    if !has_memory_path {
        return false;
    }
    // Look for Write or Edit tool invocations.
    let write_patterns = [
        "Write",
        "\"name\": \"Write\"",
        "\"name\":\"Write\"",
        "Edit",
        "\"name\": \"Edit\"",
        "\"name\":\"Edit\"",
        "file_write",
        "file_edit",
        "write_to_file",
    ];
    write_patterns.iter().any(|p| messages_text.contains(p))
}

// ── Event helpers ────────────────────────────────────────────────────

impl ExtractSkipReason {
    /// Build the corresponding `DriverEvent` for this skip reason.
    pub fn to_event(self, goal_id: GoalId) -> DriverEvent {
        DriverEvent::ExtractMemoriesSkipped {
            goal_id,
            reason: self,
        }
    }
}

// ── Noop LLM backend for tests ───────────────────────────────────────

pub struct NoopExtractMemoriesLlm {
    /// If set, returned as the LLM response (for testing the parse path).
    pub canned_response: Mutex<Option<String>>,
}

impl NoopExtractMemoriesLlm {
    pub fn new() -> Self {
        Self {
            canned_response: Mutex::new(None),
        }
    }

    pub fn with_response(response: impl Into<String>) -> Self {
        Self {
            canned_response: Mutex::new(Some(response.into())),
        }
    }
}

impl Default for NoopExtractMemoriesLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtractMemoriesLlm for NoopExtractMemoriesLlm {
    async fn chat(
        &self,
        _system_prompt: &str,
        _user_messages: &str,
        _max_tokens: u32,
    ) -> Result<String, String> {
        self.canned_response
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "NoopExtractMemoriesLlm: no canned response set".to_string())
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── Manifest scanner ──────────────────────────────────────────

    #[test]
    fn scan_manifest_empty_dir() {
        let dir = TempDir::new().unwrap();
        let manifest = scan_memory_manifest(dir.path()).unwrap();
        assert!(manifest.is_empty());
    }

    #[test]
    fn scan_manifest_nonexistent_dir() {
        let manifest = scan_memory_manifest(Path::new("/tmp/nonexistent-memdir-77-5"))
            .unwrap();
        assert!(manifest.is_empty());
    }

    #[test]
    fn scan_manifest_reads_frontmatter() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("preferences.md"),
            "---\nname: user preferences\ndescription: likes dark mode\ntype: user\n---\n\nUser prefers dark mode.",
        )
        .unwrap();
        fs::write(
            dir.path().join("deploy.md"),
            "---\nname: deploy notes\ndescription: deploy process\ntype: project\n---\n\nDeploy on Fridays.",
        )
        .unwrap();

        let manifest = scan_memory_manifest(dir.path()).unwrap();
        assert!(manifest.contains("preferences.md"), "missing preferences: {manifest}");
        assert!(manifest.contains("dark mode"), "missing description: {manifest}");
        assert!(manifest.contains("[user]"), "missing type tag: {manifest}");
        assert!(manifest.contains("deploy.md"), "missing deploy: {manifest}");
        assert!(manifest.contains("[project]"), "missing project type: {manifest}");
    }

    #[test]
    fn scan_manifest_skips_memory_index() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("MEMORY.md"),
            "# Memory index\n\n- [prefs](preferences.md)\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("preferences.md"),
            "---\nname: prefs\ndescription: x\ntype: user\n---\n\nContent.",
        )
        .unwrap();

        let manifest = scan_memory_manifest(dir.path()).unwrap();
        assert!(
            !manifest.contains("MEMORY.md"),
            "MEMORY.md should be excluded: {manifest}"
        );
        assert!(manifest.contains("preferences.md"), "should list preferences: {manifest}");
    }

    #[test]
    fn scan_manifest_file_without_frontmatter() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("notes.md"), "Just some notes.\nNo frontmatter here.").unwrap();

        let manifest = scan_memory_manifest(dir.path()).unwrap();
        assert!(manifest.contains("[unknown]"), "should tag as unknown: {manifest}");
        assert!(manifest.contains("notes.md"), "should list the file: {manifest}");
    }

    // ── Memory-write detection ────────────────────────────────────

    #[test]
    fn has_memory_writes_detects_write_tool() {
        let text = r#"Tool: Write
Arguments: {"file_path": "/home/user/.claude/projects/test/memory/foo.md", "content": "..."}"#;
        assert!(has_memory_writes_in_text(
            text,
            Path::new("/home/user/.claude/projects/test/memory")
        ));
    }

    #[test]
    fn has_memory_writes_detects_file_write_tool() {
        let text = r#"I'll use file_write to save this memory.
{"tool": "file_write", "path": "/home/user/.claude/projects/x/memory/bar.md"}"#;
        assert!(has_memory_writes_in_text(
            text,
            Path::new("/home/user/.claude/projects/x/memory")
        ));
    }

    #[test]
    fn has_memory_writes_no_write() {
        let text = "Just a normal conversation.\nNo tool calls here.";
        assert!(!has_memory_writes_in_text(
            text,
            Path::new("/home/user/.claude/projects/test/memory")
        ));
    }

    #[test]
    fn has_memory_writes_write_outside_memory_dir() {
        let text = r#"Write to /tmp/some-other-file.txt"#;
        assert!(!has_memory_writes_in_text(text, Path::new("/home/user/memory")));
    }

    // ── Path resolution ───────────────────────────────────────────

    #[test]
    fn resolve_memory_path_rejects_absolute() {
        assert!(resolve_memory_path(Path::new("/mem"), "/etc/passwd").is_err());
    }

    #[test]
    fn resolve_memory_path_rejects_parent_traversal() {
        assert!(resolve_memory_path(Path::new("/mem"), "../outside.md").is_err());
        assert!(resolve_memory_path(Path::new("/mem"), "sub/../../outside.md").is_err());
    }

    #[test]
    fn resolve_memory_path_accepts_normal() {
        let result = resolve_memory_path(Path::new("/mem"), "user_role.md").unwrap();
        assert_eq!(result, PathBuf::from("/mem/user_role.md"));
    }

    #[test]
    fn resolve_memory_path_accepts_subdir() {
        let result = resolve_memory_path(Path::new("/mem"), "sub/dir/file.md").unwrap();
        assert_eq!(result, PathBuf::from("/mem/sub/dir/file.md"));
    }

    // ── Response parsing ──────────────────────────────────────────

    #[test]
    fn parse_response_bare_json() {
        let json = r#"[{"file_path": "test.md", "content": "hello"}]"#;
        let files = parse_extraction_response(json).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_path, "test.md");
        assert_eq!(files[0].content, "hello");
    }

    #[test]
    fn parse_response_json_fenced() {
        let json = "```json\n[{\"file_path\": \"x.md\", \"content\": \"y\"}]\n```";
        let files = parse_extraction_response(json).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_path, "x.md");
    }

    #[test]
    fn parse_response_empty_array() {
        let files = parse_extraction_response("[]").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn parse_response_invalid_json() {
        assert!(parse_extraction_response("not json").is_err());
    }

    // ── Gate checks ───────────────────────────────────────────────

    fn make_config() -> ExtractMemoriesConfig {
        ExtractMemoriesConfig {
            enabled: true,
            turns_throttle: 1,
            max_turns: 5,
            max_consecutive_failures: 3,
        }
    }

    fn make_extractor(config: ExtractMemoriesConfig) -> Arc<ExtractMemories> {
        Arc::new(ExtractMemories::new(config, Arc::new(NoopExtractMemoriesLlm::new())))
    }

    #[test]
    fn gate_disabled_when_enabled_false() {
        let mut cfg = make_config();
        cfg.enabled = false;
        let ext = make_extractor(cfg);
        assert!(matches!(
            ext.check_gates(),
            Err(ExtractSkipReason::Disabled)
        ));
    }

    #[test]
    fn gate_throttled_when_not_enough_turns() {
        let mut cfg = make_config();
        cfg.turns_throttle = 3;
        let ext = make_extractor(cfg);
        // turns_since_last starts at 0, throttle=3 means run every 3 turns
        // (i.e. skip when turns_since_last < 2).
        assert!(matches!(
            ext.check_gates(),
            Err(ExtractSkipReason::Throttled)
        ));
    }

    #[test]
    fn gate_passes_when_throttle_satisfied() {
        let cfg = make_config(); // throttle=1 means run every turn
        let ext = make_extractor(cfg);
        assert!(ext.check_gates().is_ok());
    }

    #[test]
    fn gate_passes_after_tick_accumulates() {
        let mut cfg = make_config();
        cfg.turns_throttle = 2;
        let ext = make_extractor(cfg);
        // First check: turns_since_last=0 < (2-1)=1 → throttled
        assert!(ext.check_gates().is_err());
        ext.tick();
        // Second check: turns_since_last=1 >= 1 → passes
        assert!(ext.check_gates().is_ok());
    }

    #[test]
    fn gate_circuit_breaker_trips_after_n_failures() {
        let ext = make_extractor(make_config());
        ext.record_failure();
        ext.record_failure();
        ext.record_failure();
        assert!(matches!(
            ext.check_gates(),
            Err(ExtractSkipReason::CircuitBreakerOpen)
        ));
    }

    #[test]
    fn gate_circuit_breaker_disabled_when_max_zero() {
        let mut cfg = make_config();
        cfg.max_consecutive_failures = 0;
        let ext = make_extractor(cfg);
        ext.record_failure();
        ext.record_failure();
        ext.record_failure();
        // Breaker disabled — should pass (throttle=1 means always).
        assert!(ext.check_gates().is_ok());
    }

    #[test]
    fn record_success_resets_failures_and_turns() {
        let ext = make_extractor(make_config());
        ext.record_failure();
        ext.record_failure();
        ext.record_success(None);
        // After success, consecutive_failures=0, turns_since_last=0.
        assert!(ext.check_gates().is_ok());
    }

    // ── MEMORY.md index ───────────────────────────────────────────

    #[test]
    fn update_index_creates_file_when_missing() {
        let dir = TempDir::new().unwrap();
        let files = vec![MemoryFile {
            file_path: "new_memory.md".to_string(),
            content: "---\nname: test\ntype: user\n---\n\nSome content here.".to_string(),
        }];
        update_memory_index(dir.path(), &files).unwrap();

        let index = fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        assert!(index.contains("new_memory.md"), "should list new file");
        assert!(index.contains("Some content here"), "should include hook");
    }

    #[test]
    fn update_index_skips_duplicates() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("MEMORY.md"),
            "# Memory index\n\n- [existing](existing.md) — already there\n",
        )
        .unwrap();

        let files = vec![
            MemoryFile {
                file_path: "existing.md".to_string(),
                content: "duplicate".to_string(),
            },
            MemoryFile {
                file_path: "new_one.md".to_string(),
                content: "new content here".to_string(),
            },
        ];
        update_memory_index(dir.path(), &files).unwrap();

        let index = fs::read_to_string(dir.path().join("MEMORY.md")).unwrap();
        let existing_count = index.matches("existing.md").count();
        assert_eq!(existing_count, 1, "duplicate should not be appended");
        assert!(index.contains("new_one.md"), "new file should be appended");
    }

    // ── Path sandbox hardening tests (Phase 77.7) ──

    #[test]
    fn resolve_memory_path_rejects_null_byte() {
        assert!(resolve_memory_path(Path::new("/mem"), "foo\0bar.md").is_err());
    }

    #[test]
    fn resolve_memory_path_rejects_url_encoded_traversal() {
        assert!(resolve_memory_path(Path::new("/mem"), "%2e%2e%2foutside.md").is_err());
        assert!(resolve_memory_path(Path::new("/mem"), "%2e%2e%2Foutside.md").is_err());
        assert!(resolve_memory_path(Path::new("/mem"), "sub/%2e%2e/outside.md").is_err());
    }

    #[test]
    fn resolve_memory_path_rejects_unicode_fullwidth_dots() {
        assert!(
            resolve_memory_path(Path::new("/mem"), "foo/\u{FF0E}\u{FF0E}/bar.md").is_err()
        );
    }

    #[test]
    fn resolve_memory_path_rejects_unicode_fullwidth_slash() {
        assert!(
            resolve_memory_path(Path::new("/mem"), "foo\u{FF0F}bar.md").is_err()
        );
    }

    #[test]
    fn resolve_memory_path_accepts_normal_path_phase77_7() {
        let result = resolve_memory_path(Path::new("/mem"), "notes.md").unwrap();
        assert_eq!(result, Path::new("/mem/notes.md"));
    }
}
