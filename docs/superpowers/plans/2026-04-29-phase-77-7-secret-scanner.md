## Plan: 77.7 — memdir secretScanner + teamMemSecretGuard

### Crates afectados
- `crates/memory/` — main implementation
- `crates/core/` — events

### Archivos nuevos
- `crates/memory/src/secret_scanner.rs` — SecretScanner, CompiledRule, 34 regex rules, SecretMatch, SecretGuard, OnSecret, SecretBlockedError
- `crates/memory/src/secret_config.rs` — SecretGuardConfig (YAML deserializable)

### Archivos modificados
- `crates/memory/src/lib.rs` — `pub mod secret_scanner;` + `pub mod secret_config;` + re-exports
- `crates/memory/src/long_term.rs` — `remember_typed()` gains optional `&SecretGuard` param, scan before INSERT
- `crates/memory/src/extract_memories.rs` — scan each MemoryFile.content before fs::write, skip on Block, redact on Redact
- `crates/memory/src/dreaming.rs` — `append_to_memory_md()` scan candidate.content before write
- `crates/memory/src/paths.rs` — path sandbox hardening (Unicode normalization, URL-encoded traversal, null byte, symlink resolution)
- `crates/memory/src/workspace_git.rs` — `commit_all()` scan staged diff before commit
- `crates/core/src/events.rs` — add MemorySecretBlocked, MemorySecretRedacted, MemorySecretWarned variants
- `PHASES.md` — mark 77.7 ✅, bump counters
- `CLAUDE.md` — 237→238, 7/20→8/20

### Pasos

1. [ ] **SecretScanner + 34 regex rules** — `crates/memory/src/secret_scanner.rs`
   - `SecretMatch { rule_id: &'static str, label: &'static str, offset: usize }` — never contains secret value
   - `once_cell::sync::Lazy<Vec<CompiledRule>>` with 34 compiled regexes
   - Anthropic API key prefix assembled at runtime: `["sk", "ant", "api"].join("-")` — avoids literal in binary
   - `SecretScanner::scan(content) -> Vec<SecretMatch>` — ordered by offset, deduplicated by rule_id
   - `SecretScanner::has_secrets(content) -> bool` — fast path, short-circuit on first match
   - `SecretScanner::new()` — all 34 rules, `SecretScanner::with_rules(ids)` / `SecretScanner::without_rules(excludes)` — filtered
   - `SecretGuard { scanner, on_secret }` struct — wraps scanner with policy
   - `SecretGuard::check(content) -> Result<String, SecretBlockedError>`
     - No secrets or disabled → Ok(content)
     - Block + match → Err(SecretBlockedError { labels, rule_ids, content_hash })
     - Redact + match → Ok(content with `[REDACTED:rule_id]` replacements)
     - Warn + match → Ok(content) unchanged
   - `SecretBlockedError` derives `Error`, impl Display
   - `content_hash` = SHA-256 hex via `sha2` crate (already in workspace)
   - ~20 unit tests: each rule family detection, has_secrets short-circuit, guard block/redact/warn/disabled, multi-secret, content_hash stability

2. [ ] **Config schema** — `crates/memory/src/secret_config.rs`
   - `SecretGuardConfig { enabled: bool, on_secret: OnSecret, rules: RuleSelection, exclude_rules: Vec<String> }`
   - `RuleSelection::All | RuleSelection::List(Vec<String>)` — serde untagged
   - Default: `enabled=true, on_secret=Block, rules=All, exclude_rules=[]`
   - `SecretGuardConfig::build_guard(&self) -> SecretGuard` — constructs scanner + guard from config
   - ~5 unit tests: deserialize YAML, defaults, build_guard All vs List, build_guard with excludes

3. [ ] **Integrate into remember_typed()** — `crates/memory/src/long_term.rs`
   - `LongTermMemory` gains `guard: Option<SecretGuard>` field
   - `with_guard(mut self, guard: SecretGuard) -> Self` builder
   - `remember_typed()`: if guard is Some, call `guard.check(&content)?` before INSERT
   - On Block → return Err (INSERT never happens)
   - On Redact → INSERT redacted content
   - On Warn → INSERT original content
   - `remember()` delegates to `remember_typed()` unchanged
   - ~3 unit tests: block prevents INSERT, redact stores redacted string, no guard backward compat

4. [ ] **Integrate into ExtractMemories** — `crates/memory/src/extract_memories.rs`
   - `ExtractMemories::extract()` gains `guard: Option<&SecretGuard>` param
   - For each MemoryFile: `guard.check(&file.content)` before `fs::write`
   - On Block → skip file, continue with next file (don't abort batch)
   - On Redact → write redacted content
   - On Warn → write intact
   - ~3 unit tests: block skips file, redact writes redacted, no guard backward compat

5. [ ] **Events** — `crates/core/src/events.rs`
   - `MemorySecretBlocked { rule_ids, content_hash, source }`
   - `MemorySecretRedacted { rule_ids, content_hash, source }`
   - `MemorySecretWarned { rule_ids, content_hash, source }`
   - `source: String` — "remember", "extract_memories", "dreaming", "git_commit"
   - Emit via EventBus in each integration point
   - ~1 unit test: event payload round-trips through serde_json

6. [ ] **Path sandbox hardening** — `crates/memory/src/paths.rs`
   - URL-encoded traversal rejection (`%2e%2e%2f`, `%2e%2e/`, etc.) — decode before check
   - Unicode fullwidth dots/slashes rejection (`.`, `/` fullwidth variants)
   - Null byte rejection in any path component
   - `canonicalize()` symlink resolution: resolved path must stay inside `memory_dir`
   - Add all checks to `resolve_memory_path()`
   - ~6 unit tests: each traversal vector rejected, legit paths pass

7. [ ] **Integrate into Dreaming** — `crates/memory/src/dreaming.rs`
   - `Dreaming::append_to_memory_md()` gains `guard: Option<&SecretGuard>` param
   - Scan `candidate.content` (the block being written) before appending
   - On Block → skip candidate, emit event
   - On Redact → append redacted version
   - On Warn → append intact, emit event
   - ~2 unit tests: candidate with secret blocked, candidate with secret redacted

8. [ ] **Integrate into git commit** — `crates/memory/src/workspace_git.rs`
   - `workspace_git::commit_all()` gains `guard: Option<&SecretGuard>` param
   - Read staged diff via `git diff --cached` before commit
   - On Block → abort commit, emit MemorySecretBlocked { source: "git_commit" }
   - On Redact → abort commit (cannot safely redact in-flight git diff), emit MemorySecretBlocked
   - On Warn → proceed with commit, emit MemorySecretWarned
   - ~1 unit test: secret in staged file blocks commit

9. [ ] **Wire guard creation at boot** — caller sites
   - In the agent boot path (likely `nexo-driver-loop` or `nexo-core` agent init): read config, build guard, pass to LongTermMemory + ExtractMemories + Dreaming + workspace_git
   - If config block absent: guard is None (existing behavior, no change)
   - Verify `cargo build --workspace` passes with all integration points wired

10. [ ] **Tests + docs sync**
    - `cargo test --workspace` — all new + existing tests pass
    - Mark 77.7 ✅ in PHASES.md, update counter
    - Update CLAUDE.md: 237→238, 7/20→8/20
    - Update `docs/src/` if memory config YAML docs page exists

### Tests a escribir (~40)
- `secret_scanner.rs` — ~20 tests: each rule family detects its target, has_secrets short-circuits, with_rules/without_rules filtering, guard block/redact/warn/disabled, multi-secret, content_hash stability
- `secret_config.rs` — ~5 tests: deserialize YAML all fields, defaults, build_guard, rule list parsing
- `long_term.rs` — ~3 tests: remember_typed block prevents INSERT, redact stores redacted, no guard backward compat
- `extract_memories.rs` — ~3 tests: block skips file, redact writes redacted, no guard backward compat
- `paths.rs` — ~6 tests: URL-encoded traversal, Unicode fullwidth dots, null byte, symlink escape, legit path passes, canonicalize within memory_dir
- `dreaming.rs` — ~2 tests: candidate with secret blocked, candidate with secret redacted
- `workspace_git.rs` — ~1 test: secret in staged diff blocks commit

### Riesgos
- **`regex` crate compilation time** — 34 regexes compiled once via Lazy, negligible at startup
- **False positives on legitimate content** — mitigated by `exclude_rules` config, high-specificity prefixes only
- **Performance in hot paths** — `has_secrets()` short-circuits on first match, scan is O(content × rules) but rules are few and content is memory-sized (small)
- **Backward compat** — all guard params are `Option<&SecretGuard>`, None means no-op (existing behavior)
- **Anthropic rule binary literal** — prefix assembled at runtime from segments, no literal `sk-ant-api` in binary
- **git diff reading** — `git diff --cached` output can be large; scan only the diff hunks (added lines), not the full file
