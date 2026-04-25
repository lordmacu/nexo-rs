pub use nexo_config::types::agents::SkillDepsMode;
use semver::{Version, VersionReq};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::Mutex as AsyncMutex;
/// Optional YAML frontmatter parsed from the top of `SKILL.md`. All fields
/// are optional — skills without frontmatter behave exactly as before
/// (Phase 13.1 semantics).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SkillMetadata {
    /// Display name. Falls back to the directory name when absent.
    pub name: Option<String>,
    /// One-line description shown next to the skill heading in the system prompt.
    pub description: Option<String>,
    /// Soft constraints declared by the skill author.
    pub requires: SkillRequires,
    /// Hard cap on injected content size (chars). Truncated with a marker.
    pub max_chars: Option<usize>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SkillRequires {
    /// External binaries the skill expects on PATH (e.g. `["ffmpeg"]`).
    pub bins: Vec<String>,
    /// Environment variables the skill expects to be set
    /// (e.g. `["GITHUB_TOKEN"]`).
    pub env: Vec<String>,
    /// Per-bin semver constraints. Names declared here are implicitly
    /// required to be present on PATH, so an entry in `bin_versions`
    /// alone is enough — it does not have to be repeated in `bins`.
    pub bin_versions: BTreeMap<String, BinVersionSpec>,
    /// Skill-author-declared mode for missing dependencies. Defaults
    /// to `strict` (skip the skill). Can be overridden per-agent via
    /// `agents.<id>.skill_overrides`.
    pub mode: SkillDepsMode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BinVersionSpec {
    /// Shorthand: just the constraint string (e.g. `">=4.0"`).
    Constraint(String),
    /// Full form with custom version probe.
    Detailed {
        constraint: String,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        regex: Option<String>,
    },
}

impl BinVersionSpec {
    fn constraint(&self) -> &str {
        match self {
            BinVersionSpec::Constraint(s) => s.as_str(),
            BinVersionSpec::Detailed { constraint, .. } => constraint.as_str(),
        }
    }
    fn command(&self) -> &str {
        match self {
            BinVersionSpec::Constraint(_) => "--version",
            BinVersionSpec::Detailed { command, .. } => command.as_deref().unwrap_or("--version"),
        }
    }
    fn regex(&self) -> &str {
        match self {
            BinVersionSpec::Constraint(_) => DEFAULT_VERSION_REGEX,
            BinVersionSpec::Detailed { regex, .. } => {
                regex.as_deref().unwrap_or(DEFAULT_VERSION_REGEX)
            }
        }
    }
}

// Require at least MAJOR.MINOR. Allowing major-only (`\d+`) catches
// stray digits inside binary names like `oldbin2` before the real
// version appears later in the string. Tools that emit major-only
// (rare) can override via `bin_versions.<name>.regex`.
const DEFAULT_VERSION_REGEX: &str = r"\d+\.\d+(?:\.\d+)?";

#[derive(Debug, Clone)]
pub struct MissingVersion {
    pub bin: String,
    pub required: String,
    pub found: Option<String>,
    pub reason: VersionFailReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionFailReason {
    BinNotFound,
    ProbeFailed,
    ParseFailed,
    ConstraintUnsatisfied,
    InvalidConstraint,
}
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub content: String,
    pub metadata: SkillMetadata,
    /// Names of `requires.bins` that were not found on PATH at load time.
    pub missing_bins: Vec<String>,
    /// Names of `requires.env` vars that were unset or empty at load time.
    pub missing_env: Vec<String>,
    /// Bin versions that did not satisfy their declared constraint.
    pub missing_versions: Vec<MissingVersion>,
    /// Mode that was applied after resolving frontmatter + per-agent override.
    pub mode_applied: SkillDepsMode,
}

#[derive(Debug, Clone)]
pub struct SkillLoadStatus {
    pub name: String,
    pub action: SkillLoadAction,
    pub mode_applied: SkillDepsMode,
    pub missing_bins: Vec<String>,
    pub missing_env: Vec<String>,
    pub missing_versions: Vec<MissingVersion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillLoadAction {
    Loaded,
    LoadedWithBanner,
    SkippedStrict,
    SkippedDisabled,
    NotFound,
}
pub struct SkillLoader {
    root: PathBuf,
    overrides: BTreeMap<String, SkillDepsMode>,
}
impl SkillLoader {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            overrides: BTreeMap::new(),
        }
    }
    /// Per-agent mode override map. Names that match a loaded skill
    /// take precedence over the skill's frontmatter `requires.mode`.
    pub fn with_overrides(mut self, overrides: BTreeMap<String, SkillDepsMode>) -> Self {
        self.overrides = overrides;
        self
    }
    pub async fn load_many(&self, names: &[String]) -> Vec<LoadedSkill> {
        let (loaded, _status) = self.load_many_with_status(names).await;
        loaded
    }
    pub async fn load_many_with_status(
        &self,
        names: &[String],
    ) -> (Vec<LoadedSkill>, Vec<SkillLoadStatus>) {
        let mut loaded = Vec::new();
        let mut status = Vec::with_capacity(names.len());
        for name in names {
            match self.load_one(name).await {
                Some((maybe_skill, st)) => {
                    if let Some(skill) = maybe_skill {
                        loaded.push(skill);
                    }
                    status.push(st);
                }
                None => {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        status.push(SkillLoadStatus {
                            name: trimmed.to_string(),
                            action: SkillLoadAction::NotFound,
                            mode_applied: SkillDepsMode::Strict,
                            missing_bins: Vec::new(),
                            missing_env: Vec::new(),
                            missing_versions: Vec::new(),
                        });
                    }
                }
            }
        }
        (loaded, status)
    }
    async fn load_one(&self, name: &str) -> Option<(Option<LoadedSkill>, SkillLoadStatus)> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = self.root.join(trimmed).join("SKILL.md");
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    skill = trimmed,
                    path = %path.display(),
                    error = %e,
                    "failed to load skill; skipping"
                );
                return None;
            }
        };
        let raw_trimmed = raw.trim();
        if raw_trimmed.is_empty() {
            tracing::warn!(skill = trimmed, path = %path.display(), "skill file is empty");
            return None;
        }
        let (metadata, body) = parse_frontmatter(raw_trimmed, trimmed, &path);
        let body_trimmed = body.trim();
        if body_trimmed.is_empty() {
            tracing::warn!(skill = trimmed, path = %path.display(), "skill body is empty after frontmatter");
            return None;
        }

        let mode = resolve_mode(metadata.requires.mode, self.overrides.get(trimmed).copied());

        // Disable bypasses every check, including version probes —
        // operator decision wins.
        if mode == SkillDepsMode::Disable {
            tracing::info!(skill = trimmed, "skill disabled by mode=disable");
            return Some((
                None,
                SkillLoadStatus {
                    name: trimmed.to_string(),
                    action: SkillLoadAction::SkippedDisabled,
                    mode_applied: mode,
                    missing_bins: Vec::new(),
                    missing_env: Vec::new(),
                    missing_versions: Vec::new(),
                },
            ));
        }

        let missing_env: Vec<String> = metadata
            .requires
            .env
            .iter()
            .filter(|var| {
                std::env::var(var)
                    .ok()
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        let missing_bins: Vec<String> = metadata
            .requires
            .bins
            .iter()
            .filter(|bin| !bin_exists_on_path(bin))
            .cloned()
            .collect();
        let missing_versions = probe_version_requirements(&metadata.requires.bin_versions).await;

        let any_missing =
            !missing_env.is_empty() || !missing_bins.is_empty() || !missing_versions.is_empty();

        match mode {
            SkillDepsMode::Disable => unreachable!("handled above"),
            SkillDepsMode::Strict if any_missing => {
                if !missing_bins.is_empty() {
                    tracing::warn!(
                        skill = trimmed,
                        missing_bins = ?missing_bins,
                        "skill disabled: required bins not found on PATH"
                    );
                }
                if !missing_env.is_empty() {
                    tracing::warn!(
                        skill = trimmed,
                        missing_env = ?missing_env,
                        "skill disabled: required env vars unset or empty"
                    );
                }
                if !missing_versions.is_empty() {
                    tracing::warn!(
                        skill = trimmed,
                        missing_versions = ?missing_versions
                            .iter()
                            .map(|m| format!("{}={:?}", m.bin, m.reason))
                            .collect::<Vec<_>>(),
                        "skill disabled: bin version constraints not satisfied"
                    );
                }
                Some((
                    None,
                    SkillLoadStatus {
                        name: trimmed.to_string(),
                        action: SkillLoadAction::SkippedStrict,
                        mode_applied: mode,
                        missing_bins,
                        missing_env,
                        missing_versions,
                    },
                ))
            }
            SkillDepsMode::Strict => {
                let skill = LoadedSkill {
                    name: trimmed.to_string(),
                    content: body_trimmed.to_string(),
                    metadata: metadata.clone(),
                    missing_bins: Vec::new(),
                    missing_env: Vec::new(),
                    missing_versions: Vec::new(),
                    mode_applied: mode,
                };
                let status = SkillLoadStatus {
                    name: trimmed.to_string(),
                    action: SkillLoadAction::Loaded,
                    mode_applied: mode,
                    missing_bins: Vec::new(),
                    missing_env: Vec::new(),
                    missing_versions: Vec::new(),
                };
                Some((Some(skill), status))
            }
            SkillDepsMode::Warn => {
                let action = if any_missing {
                    SkillLoadAction::LoadedWithBanner
                } else {
                    SkillLoadAction::Loaded
                };
                let content = if any_missing {
                    let banner = render_missing_banner(
                        trimmed,
                        &missing_bins,
                        &missing_env,
                        &missing_versions,
                    );
                    format!("{banner}\n\n{body_trimmed}")
                } else {
                    body_trimmed.to_string()
                };
                let skill = LoadedSkill {
                    name: trimmed.to_string(),
                    content,
                    metadata: metadata.clone(),
                    missing_bins: missing_bins.clone(),
                    missing_env: missing_env.clone(),
                    missing_versions: missing_versions.clone(),
                    mode_applied: mode,
                };
                let status = SkillLoadStatus {
                    name: trimmed.to_string(),
                    action,
                    mode_applied: mode,
                    missing_bins,
                    missing_env,
                    missing_versions,
                };
                Some((Some(skill), status))
            }
        }
    }
}

fn resolve_mode(
    frontmatter: SkillDepsMode,
    override_for_skill: Option<SkillDepsMode>,
) -> SkillDepsMode {
    override_for_skill.unwrap_or(frontmatter)
}

fn render_missing_banner(
    skill_name: &str,
    missing_bins: &[String],
    missing_env: &[String],
    missing_versions: &[MissingVersion],
) -> String {
    let mut out = format!("> ⚠️ MISSING DEPS for skill `{skill_name}`:\n");
    for b in missing_bins {
        out.push_str(&format!(">   - bin not found: {b}\n"));
    }
    for e in missing_env {
        out.push_str(&format!(">   - env unset: {e}\n"));
    }
    for v in missing_versions {
        let detail = match (&v.found, v.reason) {
            (Some(found), VersionFailReason::ConstraintUnsatisfied) => {
                format!("requires {} (found {found})", v.required)
            }
            (_, VersionFailReason::BinNotFound) => format!("requires {} (bin missing)", v.required),
            (_, VersionFailReason::ProbeFailed) => {
                format!("requires {} (probe failed)", v.required)
            }
            (_, VersionFailReason::ParseFailed) => {
                format!("requires {} (version output unparseable)", v.required)
            }
            (_, VersionFailReason::InvalidConstraint) => {
                format!("invalid constraint `{}`", v.required)
            }
            _ => v.required.clone(),
        };
        out.push_str(&format!(">   - version mismatch: {} {}\n", v.bin, detail));
    }
    out.push_str("> Calls into this skill may fail.");
    out
}

/// Process-wide cache of `(absolute_bin_path, semver::Version)`. We
/// keep `Option<Version>` so we remember probes that failed too —
/// retrying every load would amplify boot time on hosts where the
/// binary is missing or hangs.
fn version_cache() -> &'static AsyncMutex<HashMap<PathBuf, Option<Version>>> {
    static CACHE: OnceLock<AsyncMutex<HashMap<PathBuf, Option<Version>>>> = OnceLock::new();
    CACHE.get_or_init(|| AsyncMutex::new(HashMap::new()))
}

/// Resolve a bin name to an absolute path on PATH. Returns `None`
/// when the bin is not found.
fn resolve_bin_path(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('/') {
        let p = PathBuf::from(trimmed);
        return p.is_file().then_some(p);
    }
    let path = std::env::var("PATH").ok()?;
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(sep) {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(trimmed);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

async fn probe_version_requirements(
    specs: &BTreeMap<String, BinVersionSpec>,
) -> Vec<MissingVersion> {
    let mut futures = Vec::with_capacity(specs.len());
    for (bin, spec) in specs {
        futures.push(probe_one_bin(bin.clone(), spec.clone()));
    }
    let results = futures::future::join_all(futures).await;
    results.into_iter().flatten().collect()
}

async fn probe_one_bin(bin: String, spec: BinVersionSpec) -> Option<MissingVersion> {
    let constraint_str = spec.constraint().to_string();
    let req = match VersionReq::parse(&constraint_str) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                bin = %bin,
                constraint = %constraint_str,
                error = %e,
                "skill bin_versions: invalid semver constraint"
            );
            return Some(MissingVersion {
                bin,
                required: constraint_str,
                found: None,
                reason: VersionFailReason::InvalidConstraint,
            });
        }
    };
    let Some(path) = resolve_bin_path(&bin) else {
        return Some(MissingVersion {
            bin,
            required: constraint_str,
            found: None,
            reason: VersionFailReason::BinNotFound,
        });
    };
    let probe_result = run_probe(&path, spec.command(), spec.regex()).await;
    let version = match probe_result {
        Ok(v) => v,
        Err(reason) => {
            return Some(MissingVersion {
                bin,
                required: constraint_str,
                found: None,
                reason,
            });
        }
    };
    if !req.matches(&version) {
        return Some(MissingVersion {
            bin,
            required: constraint_str,
            found: Some(version.to_string()),
            reason: VersionFailReason::ConstraintUnsatisfied,
        });
    }
    None
}

async fn run_probe(
    path: &Path,
    command: &str,
    regex_pattern: &str,
) -> Result<Version, VersionFailReason> {
    {
        let cache = version_cache().lock().await;
        if let Some(cached) = cache.get(path) {
            return cached.clone().ok_or(VersionFailReason::ProbeFailed);
        }
    }
    let result = invoke_and_parse(path, command, regex_pattern).await;
    let mut cache = version_cache().lock().await;
    match &result {
        Ok(v) => {
            cache.insert(path.to_path_buf(), Some(v.clone()));
        }
        Err(_) => {
            cache.insert(path.to_path_buf(), None);
        }
    }
    result
}

async fn invoke_and_parse(
    path: &Path,
    command: &str,
    regex_pattern: &str,
) -> Result<Version, VersionFailReason> {
    // Cap the compiled NFA so a hostile or sloppy skill author can't
    // ship a catastrophic-backtracking pattern that hangs the agent.
    // 64 KiB is generous for a single-line version regex.
    let re = match regex::RegexBuilder::new(regex_pattern)
        .size_limit(64 * 1024)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                bin = %path.display(),
                regex = %regex_pattern,
                error = %e,
                "skill bin_versions: invalid version probe regex"
            );
            return Err(VersionFailReason::ParseFailed);
        }
    };
    let mut cmd = tokio::process::Command::new(path);
    if !command.trim().is_empty() {
        cmd.arg(command);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let timeout = std::time::Duration::from_secs(5);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(
                bin = %path.display(),
                error = %e,
                "skill bin_versions: spawn failed"
            );
            return Err(VersionFailReason::ProbeFailed);
        }
        Err(_) => {
            tracing::warn!(bin = %path.display(), "skill bin_versions: probe timed out");
            return Err(VersionFailReason::ProbeFailed);
        }
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push('\n');
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let cap = re
        .find(&combined)
        .ok_or(VersionFailReason::ParseFailed)?
        .as_str();
    let normalized = normalize_version_string(cap);
    Version::parse(&normalized).map_err(|_| VersionFailReason::ParseFailed)
}

/// `4.2` → `4.2.0` so it round-trips through `semver::Version::parse`.
fn normalize_version_string(s: &str) -> String {
    let dots = s.matches('.').count();
    match dots {
        0 => format!("{s}.0.0"),
        1 => format!("{s}.0"),
        _ => s.to_string(),
    }
}
/// Splits the YAML frontmatter (if present) from the markdown body.
///
/// A frontmatter block must start at the very first line with a `---` and end
/// with another `---` line. Anything else is treated as plain markdown.
/// Malformed YAML inside the block is logged at warn and the skill loads with
/// default metadata — never a hard failure.
fn parse_frontmatter(raw: &str, skill_name: &str, path: &Path) -> (SkillMetadata, String) {
    if !raw.starts_with("---") {
        return (SkillMetadata::default(), raw.to_string());
    }
    // Drop the opening `---` line and split on the next `---` line.
    let after_open = match raw.find('\n') {
        Some(n) => &raw[n + 1..],
        None => return (SkillMetadata::default(), String::new()),
    };
    let Some(end_idx) = find_closing_delim(after_open) else {
        // Opened frontmatter never closed — treat the whole file as body.
        tracing::warn!(skill = skill_name, path = %path.display(), "frontmatter opened but never closed; treating as plain markdown");
        return (SkillMetadata::default(), raw.to_string());
    };
    let yaml = &after_open[..end_idx];
    // Body starts after the closing `---` line.
    let after_close = &after_open[end_idx..];
    let body = after_close
        .find('\n')
        .map(|i| &after_close[i + 1..])
        .unwrap_or("");
    match serde_yaml::from_str::<SkillMetadata>(yaml) {
        Ok(meta) => (meta, body.to_string()),
        Err(e) => {
            tracing::warn!(
                skill = skill_name,
                path = %path.display(),
                error = %e,
                "skill frontmatter is invalid YAML; loading with default metadata"
            );
            (SkillMetadata::default(), body.to_string())
        }
    }
}
fn find_closing_delim(after_open: &str) -> Option<usize> {
    let mut offset = 0;
    for line in after_open.split_inclusive('\n') {
        let line_no_nl = line.strip_suffix('\n').unwrap_or(line);
        if line_no_nl.trim() == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}
/// Best-effort `which`-style lookup. We do not invoke the binary; presence on
/// PATH is enough to clear the warning.
fn bin_exists_on_path(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.contains('/') {
        return Path::new(trimmed).is_file();
    }
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(sep) {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(trimmed);
        if candidate.is_file() {
            return true;
        }
    }
    false
}
pub fn render_system_blocks(skills: &[LoadedSkill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str("# SKILLS\n\n");
    for skill in skills {
        let display_name = skill.metadata.name.as_deref().unwrap_or(&skill.name);
        out.push_str(&format!("## {}\n", display_name));
        if let Some(desc) = &skill.metadata.description {
            let trimmed = desc.trim();
            if !trimmed.is_empty() {
                out.push_str(&format!("> {}\n\n", trimmed));
            }
        }
        let body = if let Some(cap) = skill.metadata.max_chars {
            let chars: Vec<char> = skill.content.chars().collect();
            if chars.len() > cap {
                let truncated: String = chars.into_iter().take(cap).collect();
                format!("{truncated}\n\n…[truncated to {cap} chars]")
            } else {
                skill.content.clone()
            }
        } else {
            skill.content.clone()
        };
        out.push_str(body.trim());
        out.push_str("\n\n");
    }
    Some(out.trim_end().to_string())
}
#[cfg(test)]
mod tests {
    use super::{render_system_blocks, SkillLoader};
    fn tmpdir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("nexo-core-skills-{}", uuid::Uuid::new_v4()))
    }
    #[tokio::test]
    async fn load_many_skips_missing_and_renders_loaded() -> anyhow::Result<()> {
        let tmp = tmpdir();
        tokio::fs::create_dir_all(tmp.join("weather")).await?;
        tokio::fs::write(tmp.join("weather").join("SKILL.md"), "Use for forecasts.").await?;
        let loader = SkillLoader::new(&tmp);
        let loaded = loader
            .load_many(&["weather".to_string(), "missing".to_string()])
            .await;
        assert_eq!(loaded.len(), 1);
        let rendered = render_system_blocks(&loaded).expect("rendered");
        assert!(rendered.contains("# SKILLS"));
        assert!(rendered.contains("## weather"));
        assert!(rendered.contains("Use for forecasts."));
        tokio::fs::remove_dir_all(tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn parses_frontmatter_and_uses_display_name_and_description() -> anyhow::Result<()> {
        let tmp = tmpdir();
        tokio::fs::create_dir_all(tmp.join("weather")).await?;
        let body = "---\nname: Weather Pro\ndescription: Forecast for any city.\n---\n\nUse for forecasts.";
        tokio::fs::write(tmp.join("weather").join("SKILL.md"), body).await?;
        let loader = SkillLoader::new(&tmp);
        let loaded = loader.load_many(&["weather".to_string()]).await;
        assert_eq!(loaded.len(), 1);
        let s = &loaded[0];
        assert_eq!(s.metadata.name.as_deref(), Some("Weather Pro"));
        assert_eq!(
            s.metadata.description.as_deref(),
            Some("Forecast for any city.")
        );
        assert_eq!(s.content, "Use for forecasts.");
        let rendered = render_system_blocks(&loaded).unwrap();
        assert!(rendered.contains("## Weather Pro"));
        assert!(rendered.contains("> Forecast for any city."));
        tokio::fs::remove_dir_all(tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn malformed_yaml_does_not_block_load() -> anyhow::Result<()> {
        let tmp = tmpdir();
        tokio::fs::create_dir_all(tmp.join("bad")).await?;
        let body = "---\nname: [oops not closed\n---\n\nbody body";
        tokio::fs::write(tmp.join("bad").join("SKILL.md"), body).await?;
        let loader = SkillLoader::new(&tmp);
        let loaded = loader.load_many(&["bad".to_string()]).await;
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].metadata.name.is_none());
        assert_eq!(loaded[0].content, "body body");
        tokio::fs::remove_dir_all(tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn requires_env_records_missing_vars_warn_only() -> anyhow::Result<()> {
        // Resolved by the strict/warn/disable refactor: now the
        // skill author opts into `mode: warn` and the missing env
        // is reported in `missing_env` while the skill still loads
        // (with a banner the LLM can read).
        let tmp = tmpdir();
        tokio::fs::create_dir_all(tmp.join("github")).await?;
        let var = "AGENT_CORE_SKILLS_TEST_TOKEN_XYZ";
        std::env::remove_var(var);
        let body = format!("---\nrequires:\n  env: [\"{var}\"]\n  mode: warn\n---\n\nbody");
        tokio::fs::write(tmp.join("github").join("SKILL.md"), body).await?;
        let loader = SkillLoader::new(&tmp);
        let loaded = loader.load_many(&["github".to_string()]).await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].missing_env, vec![var.to_string()]);
        assert!(loaded[0].content.contains("MISSING DEPS"));
        assert_eq!(loaded[0].mode_applied, SkillDepsMode::Warn);
        tokio::fs::remove_dir_all(tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn max_chars_truncates_with_marker() -> anyhow::Result<()> {
        let tmp = tmpdir();
        tokio::fs::create_dir_all(tmp.join("long")).await?;
        let big = "abcdefghij".repeat(100); // 1000 chars
        let body = format!("---\nmax_chars: 50\n---\n\n{big}");
        tokio::fs::write(tmp.join("long").join("SKILL.md"), body).await?;
        let loader = SkillLoader::new(&tmp);
        let loaded = loader.load_many(&["long".to_string()]).await;
        let rendered = render_system_blocks(&loaded).unwrap();
        assert!(rendered.contains("[truncated to 50 chars]"));
        // Truncated content is 50 chars (then marker)
        assert!(!rendered.contains(&"abcdefghij".repeat(10))); // 100 chars not present
        tokio::fs::remove_dir_all(tmp).await.ok();
        Ok(())
    }
    #[tokio::test]
    async fn no_frontmatter_keeps_old_behavior() -> anyhow::Result<()> {
        let tmp = tmpdir();
        tokio::fs::create_dir_all(tmp.join("plain")).await?;
        tokio::fs::write(tmp.join("plain").join("SKILL.md"), "Just markdown.").await?;
        let loader = SkillLoader::new(&tmp);
        let loaded = loader.load_many(&["plain".to_string()]).await;
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].metadata.name.is_none());
        assert_eq!(loaded[0].content, "Just markdown.");
        let rendered = render_system_blocks(&loaded).unwrap();
        assert!(rendered.contains("## plain"));
        tokio::fs::remove_dir_all(tmp).await.ok();
        Ok(())
    }

    use super::*;

    async fn write_skill(root: &std::path::Path, name: &str, content: &str) {
        tokio::fs::create_dir_all(root.join(name)).await.unwrap();
        tokio::fs::write(root.join(name).join("SKILL.md"), content)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn warn_mode_loads_with_banner_when_bin_missing() {
        let tmp = tmpdir();
        let body =
            "---\nrequires:\n  bins: [definitely-not-a-real-bin-xyz]\n  mode: warn\n---\n\nBody.";
        write_skill(&tmp, "weather", body).await;
        let loader = SkillLoader::new(&tmp);
        let (loaded, status) = loader.load_many_with_status(&["weather".to_string()]).await;
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].content.contains("MISSING DEPS"));
        assert!(loaded[0].content.contains("definitely-not-a-real-bin-xyz"));
        assert_eq!(status[0].action, SkillLoadAction::LoadedWithBanner);
        tokio::fs::remove_dir_all(tmp).await.ok();
    }

    #[tokio::test]
    async fn strict_mode_skips_when_env_missing() {
        let tmp = tmpdir();
        let body = "---\nrequires:\n  env: [DEFINITELY_UNSET_VAR_XYZ_123]\n---\n\nBody.";
        write_skill(&tmp, "gh", body).await;
        let loader = SkillLoader::new(&tmp);
        let (loaded, status) = loader.load_many_with_status(&["gh".to_string()]).await;
        assert!(loaded.is_empty());
        assert_eq!(status[0].action, SkillLoadAction::SkippedStrict);
        assert_eq!(status[0].missing_env, vec!["DEFINITELY_UNSET_VAR_XYZ_123"]);
        tokio::fs::remove_dir_all(tmp).await.ok();
    }

    #[tokio::test]
    async fn disable_mode_skips_even_when_deps_ok() {
        let tmp = tmpdir();
        let body = "---\nrequires:\n  mode: disable\n---\n\nBody.";
        write_skill(&tmp, "off", body).await;
        let loader = SkillLoader::new(&tmp);
        let (loaded, status) = loader.load_many_with_status(&["off".to_string()]).await;
        assert!(loaded.is_empty());
        assert_eq!(status[0].action, SkillLoadAction::SkippedDisabled);
    }

    #[tokio::test]
    async fn agent_override_warn_beats_skill_strict() {
        let tmp = tmpdir();
        let body = "---\nrequires:\n  bins: [nope-not-real-bin]\n---\n\nBody.";
        write_skill(&tmp, "skill1", body).await;
        let mut overrides = BTreeMap::new();
        overrides.insert("skill1".to_string(), SkillDepsMode::Warn);
        let loader = SkillLoader::new(&tmp).with_overrides(overrides);
        let (loaded, status) = loader.load_many_with_status(&["skill1".to_string()]).await;
        assert_eq!(loaded.len(), 1, "override should rescue the skill");
        assert_eq!(status[0].action, SkillLoadAction::LoadedWithBanner);
    }

    #[tokio::test]
    async fn agent_override_disable_beats_skill_warn() {
        let tmp = tmpdir();
        let body = "---\nrequires:\n  mode: warn\n---\n\nBody.";
        write_skill(&tmp, "always", body).await;
        let mut overrides = BTreeMap::new();
        overrides.insert("always".to_string(), SkillDepsMode::Disable);
        let loader = SkillLoader::new(&tmp).with_overrides(overrides);
        let (loaded, status) = loader.load_many_with_status(&["always".to_string()]).await;
        assert!(loaded.is_empty());
        assert_eq!(status[0].action, SkillLoadAction::SkippedDisabled);
    }

    /// Build a tiny shell script that prints a fake `--version` to
    /// stdout. Returns a directory to prepend to PATH.
    #[cfg(unix)]
    fn fake_bin_dir(bin_name: &str, version_output: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(bin_name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, "echo '{version_output}'").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        dir
    }

    #[cfg(unix)]
    fn with_path_prefix(extra_dir: &std::path::Path) -> String {
        let orig = std::env::var("PATH").unwrap_or_default();
        format!("{}:{orig}", extra_dir.display())
    }

    #[cfg(unix)]
    #[serial_test::serial]
    #[tokio::test]
    async fn bin_version_satisfies_constraint() {
        let bin_name = format!("fakeprobe-{}", uuid::Uuid::new_v4().simple());
        let path_dir = fake_bin_dir(&bin_name, "fakeprobe version 4.2.1");
        std::env::set_var("PATH", with_path_prefix(&path_dir));

        let tmp = tmpdir();
        let body =
            format!("---\nrequires:\n  bin_versions:\n    {bin_name}: \">=4.0\"\n---\n\nBody.",);
        write_skill(&tmp, "v", &body).await;
        let loader = SkillLoader::new(&tmp);
        let (loaded, status) = loader.load_many_with_status(&["v".to_string()]).await;
        assert_eq!(loaded.len(), 1, "{:?}", status);
        assert_eq!(status[0].action, SkillLoadAction::Loaded);
    }

    #[cfg(unix)]
    #[serial_test::serial]
    #[tokio::test]
    async fn bin_version_unsatisfied_in_strict_skips() {
        let bin_name = format!("oldbin-{}", uuid::Uuid::new_v4().simple());
        let path_dir = fake_bin_dir(&bin_name, "oldbin v3.4.2");
        std::env::set_var("PATH", with_path_prefix(&path_dir));

        let tmp = tmpdir();
        let body =
            format!("---\nrequires:\n  bin_versions:\n    {bin_name}: \">=4.0\"\n---\n\nBody.",);
        write_skill(&tmp, "old", &body).await;
        let loader = SkillLoader::new(&tmp);
        let (loaded, status) = loader.load_many_with_status(&["old".to_string()]).await;
        assert!(
            loaded.is_empty(),
            "strict should skip; status: {:?}",
            status
        );
        assert_eq!(status[0].action, SkillLoadAction::SkippedStrict);
        assert_eq!(status[0].missing_versions.len(), 1);
        assert_eq!(
            status[0].missing_versions[0].reason,
            VersionFailReason::ConstraintUnsatisfied
        );
        assert_eq!(
            status[0].missing_versions[0].found.as_deref(),
            Some("3.4.2")
        );
    }

    #[cfg(unix)]
    #[serial_test::serial]
    #[tokio::test]
    async fn bin_version_unsatisfied_in_warn_loads_with_banner() {
        let bin_name = format!("oldbin2-{}", uuid::Uuid::new_v4().simple());
        let path_dir = fake_bin_dir(&bin_name, "oldbin2 v3.4.2");
        std::env::set_var("PATH", with_path_prefix(&path_dir));

        let tmp = tmpdir();
        let body = format!(
            "---\nrequires:\n  mode: warn\n  bin_versions:\n    {bin_name}: \">=4.0\"\n---\n\nBody.",
        );
        write_skill(&tmp, "old2", &body).await;
        let loader = SkillLoader::new(&tmp);
        let (loaded, status) = loader.load_many_with_status(&["old2".to_string()]).await;
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].content.contains("MISSING DEPS"));
        assert!(loaded[0].content.contains("found 3.4.2"));
        assert_eq!(status[0].action, SkillLoadAction::LoadedWithBanner);
    }

    #[tokio::test]
    async fn invalid_constraint_logs_and_treats_as_missing() {
        let tmp = tmpdir();
        let body = "---\nrequires:\n  bin_versions:\n    whatever: \">=banana\"\n---\n\nBody.";
        write_skill(&tmp, "bad", body).await;
        let loader = SkillLoader::new(&tmp);
        let (_, status) = loader.load_many_with_status(&["bad".to_string()]).await;
        assert_eq!(status[0].action, SkillLoadAction::SkippedStrict);
        assert!(matches!(
            status[0].missing_versions.iter().map(|m| m.reason).next(),
            Some(VersionFailReason::InvalidConstraint)
        ));
    }

    #[test]
    fn normalize_pads_partial_versions() {
        assert_eq!(normalize_version_string("4"), "4.0.0");
        assert_eq!(normalize_version_string("4.2"), "4.2.0");
        assert_eq!(normalize_version_string("4.2.1"), "4.2.1");
        assert_eq!(normalize_version_string("4.2.1.0"), "4.2.1.0");
    }

    #[test]
    fn resolve_mode_override_wins() {
        assert_eq!(
            resolve_mode(SkillDepsMode::Strict, Some(SkillDepsMode::Warn)),
            SkillDepsMode::Warn
        );
        assert_eq!(
            resolve_mode(SkillDepsMode::Warn, Some(SkillDepsMode::Strict)),
            SkillDepsMode::Strict
        );
        assert_eq!(resolve_mode(SkillDepsMode::Warn, None), SkillDepsMode::Warn);
        assert_eq!(
            resolve_mode(SkillDepsMode::default(), None),
            SkillDepsMode::Strict
        );
    }
}
