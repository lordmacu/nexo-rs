//! Inline secret redaction over the staged bundle's text files.
//!
//! Runs after the staging dir is populated and before the bundle is
//! sealed + packed. Each rule that fires emits a `[REDACTED:<rule_id>]`
//! placeholder so a forensic reader can tell where data was removed
//! without learning what was removed.
//!
//! The redactor is a [`RedactionPolicy`] trait so consumers can plug in
//! a richer scanner (e.g. the workspace's `nexo_memory::SecretGuard`)
//! without this crate growing a heavy dep. The default policy ships
//! provider-agnostic regexes covering the high-frequency leak shapes
//! across LLM / cloud / CRM ecosystems.
//!
//! Scope: `memory_files/**`, `state/extract_cursor.json`, and
//! `state/dream_run.json`. SQLite files are **not** redacted (binary
//! pages defeat regex matching) and `.git/objects/**` packed objects
//! are skipped for the same reason.

use std::fs;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::manifest::RedactionReport;

/// What a redactor returned for a single text input.
#[derive(Debug, Clone, Default)]
pub struct RedactionPass {
    pub redacted: String,
    pub matches: u32,
    pub rules_hit: Vec<String>,
}

/// Pluggable redaction policy. The default impl ([`DefaultRedactionPolicy`])
/// ships provider-agnostic patterns; production deployments can replace
/// it with a `SecretGuard`-backed adapter at boot.
pub trait RedactionPolicy: Send + Sync {
    fn id(&self) -> &str;
    fn redact(&self, text: &str) -> RedactionPass;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Rule {
    id: String,
    #[serde(skip)]
    re: Option<Regex>,
    pattern: String,
}

impl Rule {
    fn build(id: &str, pattern: &str) -> Self {
        Self {
            id: id.to_string(),
            re: Regex::new(pattern).ok(),
            pattern: pattern.to_string(),
        }
    }
}

/// Provider-agnostic built-in patterns. Tuned to keep false-positives
/// low (each pattern requires a recognisable prefix or shape) while
/// covering the high-frequency leak kinds operators see in agent
/// memory:
///
/// - LLM API keys: OpenAI (`sk-`), Anthropic (`sk-ant-`)
/// - Cloud: AWS access key IDs, GitHub PATs (`ghp_`, `gho_`, `ghu_`,
///   `ghs_`, `ghr_`), Slack tokens (`xox[bpsa]-`)
/// - Generic: bearer tokens following `Authorization: Bearer …`
/// - JWT shape: three base64url segments separated by `.`
pub struct DefaultRedactionPolicy {
    rules: Vec<Rule>,
}

impl Default for DefaultRedactionPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultRedactionPolicy {
    pub fn new() -> Self {
        let rules = vec![
            Rule::build("anthropic-api-key", r"sk-ant-[A-Za-z0-9_\-]{40,}"),
            Rule::build("openai-api-key", r"sk-[A-Za-z0-9]{20,}"),
            Rule::build("aws-access-key-id", r"AKIA[0-9A-Z]{16}"),
            Rule::build(
                "github-token",
                r"\bgh[pousr]_[A-Za-z0-9]{30,}\b",
            ),
            Rule::build("slack-token", r"xox[baprs]-[A-Za-z0-9\-]{10,}"),
            Rule::build(
                "bearer-token",
                r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9._\-]{20,}",
            ),
            Rule::build(
                "jwt",
                r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\b",
            ),
        ];
        Self { rules }
    }
}

impl RedactionPolicy for DefaultRedactionPolicy {
    fn id(&self) -> &str {
        "default"
    }

    fn redact(&self, text: &str) -> RedactionPass {
        let mut out = text.to_string();
        let mut total: u32 = 0;
        let mut hit: std::collections::BTreeSet<String> = Default::default();
        for rule in &self.rules {
            let Some(re) = &rule.re else { continue };
            let mut count: u32 = 0;
            // Owned replacement string per rule so the closure does
            // not need to live as long as the regex.
            let placeholder = format!("[REDACTED:{}]", rule.id);
            let new = re
                .replace_all(&out, |_: &regex::Captures<'_>| {
                    count += 1;
                    placeholder.clone()
                })
                .into_owned();
            if count > 0 {
                hit.insert(rule.id.clone());
                total += count;
            }
            out = new;
        }
        RedactionPass {
            redacted: out,
            matches: total,
            rules_hit: hit.into_iter().collect(),
        }
    }
}

/// Apply `policy` to every text file under `staging_dir/memory_files/`
/// and `staging_dir/state/`, rewriting in place. Returns a
/// [`RedactionReport`] suitable for the manifest, or `None` when no
/// redaction was needed (no staged text inputs at all).
pub fn redact_staging_dir(
    staging_dir: &Path,
    policy: &dyn RedactionPolicy,
) -> std::io::Result<Option<RedactionReport>> {
    let mut total: u32 = 0;
    let mut rules_set: std::collections::BTreeSet<String> = Default::default();
    let mut artifacts: Vec<String> = Vec::new();

    let mut visited_any = false;
    for sub in ["memory_files", "state"] {
        let dir = staging_dir.join(sub);
        if !dir.exists() {
            continue;
        }
        visited_any = true;
        walk_redact(staging_dir, &dir, policy, &mut total, &mut rules_set, &mut artifacts)?;
    }

    if !visited_any {
        return Ok(None);
    }

    Ok(Some(RedactionReport {
        policy: format!("memory-snapshot:{}", policy.id()),
        rules_applied: rules_set.into_iter().collect(),
        matches_redacted: total,
        artifacts_touched: artifacts,
    }))
}

fn walk_redact(
    staging_root: &Path,
    cur: &Path,
    policy: &dyn RedactionPolicy,
    total: &mut u32,
    rules_set: &mut std::collections::BTreeSet<String>,
    artifacts: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_redact(staging_root, &path, policy, total, rules_set, artifacts)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        // Read; if not valid UTF-8 the file is binary (or just
        // non-text) and we leave it alone. SQLite snapshots and any
        // binary attachment fall into this branch.
        let bytes = fs::read(&path)?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let pass = policy.redact(text);
        if pass.matches == 0 {
            continue;
        }
        fs::write(&path, pass.redacted.as_bytes())?;
        *total += pass.matches;
        for r in pass.rules_hit {
            rules_set.insert(r);
        }
        if let Ok(rel) = path.strip_prefix(staging_root) {
            artifacts.push(rel.display().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_redacts_anthropic_key() {
        let p = DefaultRedactionPolicy::new();
        let raw = "key=sk-ant-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA tail";
        let pass = p.redact(raw);
        assert!(pass.redacted.contains("[REDACTED:anthropic-api-key]"));
        assert_eq!(pass.matches, 1);
        assert!(pass.rules_hit.contains(&"anthropic-api-key".to_string()));
    }

    #[test]
    fn default_policy_redacts_openai_key() {
        let p = DefaultRedactionPolicy::new();
        let pass = p.redact("OPENAI=sk-AaaaaaaaaaaaaaaaaaaaaaA done");
        assert!(pass.redacted.contains("[REDACTED:openai-api-key]"));
    }

    #[test]
    fn default_policy_redacts_github_token() {
        let p = DefaultRedactionPolicy::new();
        let pass = p.redact("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert!(pass.redacted.contains("[REDACTED:github-token]"));
        assert_eq!(pass.matches, 1);
    }

    #[test]
    fn default_policy_redacts_aws_access_key_id() {
        let p = DefaultRedactionPolicy::new();
        let pass = p.redact("AWS_KEY=AKIA1234567890ABCDEF rest");
        assert!(pass.redacted.contains("[REDACTED:aws-access-key-id]"));
    }

    #[test]
    fn default_policy_redacts_bearer_token() {
        let p = DefaultRedactionPolicy::new();
        let raw = "Authorization: Bearer abcdefghij1234567890XYZ-_.";
        let pass = p.redact(raw);
        assert!(pass.redacted.contains("[REDACTED:bearer-token]"));
    }

    #[test]
    fn default_policy_passes_clean_text() {
        let p = DefaultRedactionPolicy::new();
        let pass = p.redact("nothing to see here\nplain\nwords\n");
        assert_eq!(pass.matches, 0);
        assert!(pass.rules_hit.is_empty());
        assert_eq!(pass.redacted, "nothing to see here\nplain\nwords\n");
    }

    #[test]
    fn redact_staging_dir_rewrites_text_files_and_returns_report() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path();
        let mem_dir = staging.join("memory_files");
        let state_dir = staging.join("state");
        fs::create_dir_all(&mem_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            mem_dir.join("notes.md"),
            "live key sk-ant-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .unwrap();
        fs::write(
            state_dir.join("extract_cursor.json"),
            "{\"github\":\"ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}",
        )
        .unwrap();
        // A binary-ish file must be ignored.
        fs::write(mem_dir.join("blob.bin"), [0u8, 0xff, 0xfe, 0]).unwrap();

        let policy = DefaultRedactionPolicy::new();
        let report = redact_staging_dir(staging, &policy).unwrap().unwrap();
        assert!(report.matches_redacted >= 2);
        assert!(report.policy.starts_with("memory-snapshot:"));
        assert!(report
            .rules_applied
            .iter()
            .any(|r| r == "anthropic-api-key" || r == "github-token"));
        assert!(report
            .artifacts_touched
            .iter()
            .any(|a| a.contains("notes.md")));
        assert!(report
            .artifacts_touched
            .iter()
            .any(|a| a.contains("extract_cursor.json")));

        let after = fs::read_to_string(mem_dir.join("notes.md")).unwrap();
        assert!(after.contains("[REDACTED:anthropic-api-key]"));
        // Binary file untouched.
        let bin = fs::read(mem_dir.join("blob.bin")).unwrap();
        assert_eq!(bin, vec![0u8, 0xff, 0xfe, 0]);
    }

    #[test]
    fn redact_staging_dir_returns_none_for_empty_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = DefaultRedactionPolicy::new();
        let r = redact_staging_dir(tmp.path(), &policy).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn redact_staging_dir_returns_zero_matches_when_clean() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("memory_files")).unwrap();
        fs::write(tmp.path().join("memory_files/clean.md"), "no secrets here").unwrap();
        let policy = DefaultRedactionPolicy::new();
        let report = redact_staging_dir(tmp.path(), &policy).unwrap().unwrap();
        assert_eq!(report.matches_redacted, 0);
        assert!(report.rules_applied.is_empty());
        assert!(report.artifacts_touched.is_empty());
    }
}
