//! Pre-persistence redaction for transcript content. Patterns are
//! applied in declared order over the input string; each match is
//! replaced with `[REDACTED:<label>]`. Built-in patterns target
//! common credentials and home-directory paths. Operators can append
//! custom patterns via config.
//!
//! The redactor is **destructive by design**: there is no reverse
//! mapping. Once content is redacted and persisted, the secret is
//! gone from disk. This is intentional — keeping a mapping would
//! recreate the leak surface.

use nexo_config::types::transcripts::{RedactionConfig, RedactionPattern};
use anyhow::Context;
use regex::Regex;

/// Compiled redactor. Construct via `from_config` (returns disabled
/// passthrough when `cfg.enabled == false`) or `disabled()`.
#[derive(Debug)]
pub struct Redactor {
    rules: Vec<(Regex, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct RedactionReport {
    pub redacted_text: String,
    /// (label, count) pairs in the order patterns were declared.
    pub hits: Vec<(String, usize)>,
}

impl Redactor {
    /// Disabled passthrough — `apply` returns the input unchanged with
    /// empty hits.
    pub fn disabled() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn from_config(cfg: &RedactionConfig) -> anyhow::Result<Self> {
        if !cfg.enabled {
            return Ok(Self::disabled());
        }
        let mut rules: Vec<(Regex, String)> = Vec::new();
        if cfg.use_builtins {
            for (label, src) in builtin_patterns() {
                let re = Regex::new(src)
                    .with_context(|| format!("invalid built-in pattern `{label}`"))?;
                rules.push((re, label.to_string()));
            }
        }
        for (idx, p) in cfg.extra_patterns.iter().enumerate() {
            rules.push((compile_extra(idx, p)?, p.label.clone()));
        }
        Ok(Self { rules })
    }

    pub fn is_active(&self) -> bool {
        !self.rules.is_empty()
    }

    /// Apply every loaded rule in declared order. Each pattern runs
    /// over the **already-replaced** text, so a later pattern can
    /// match tokens emitted by earlier `[REDACTED:<label>]` markers.
    /// Built-in patterns are designed not to collide with the
    /// `[REDACTED:...]` shape, but operators should keep the same
    /// invariant when shipping `extra_patterns` — a pattern that
    /// matches `REDACTED` will produce nested markers
    /// (`[REDACTED:[REDACTED:foo]]`).
    pub fn apply(&self, input: &str) -> RedactionReport {
        if self.rules.is_empty() || input.is_empty() {
            return RedactionReport {
                redacted_text: input.to_string(),
                hits: Vec::new(),
            };
        }
        let mut text = input.to_string();
        let mut hits: Vec<(String, usize)> = Vec::with_capacity(self.rules.len());
        for (re, label) in &self.rules {
            let count = re.find_iter(&text).count();
            if count > 0 {
                let replacement = format!("[REDACTED:{label}]");
                text = re.replace_all(&text, replacement.as_str()).into_owned();
                hits.push((label.clone(), count));
            }
        }
        RedactionReport {
            redacted_text: text,
            hits,
        }
    }
}

fn compile_extra(idx: usize, p: &RedactionPattern) -> anyhow::Result<Regex> {
    if p.label.trim().is_empty() {
        anyhow::bail!("invalid extra_patterns[{idx}]: label cannot be empty");
    }
    Regex::new(&p.regex)
        .with_context(|| format!("invalid extra_patterns[{idx}] (label `{}`)", p.label))
}

/// Built-in patterns. **Order matters** — more specific shapes go first
/// so they don't get partially shadowed by generic catch-alls below.
///
/// **Mirror invariant:** `extensions/onepassword/src/redact.rs` ships
/// the same pattern set (it can't depend on `nexo-core` because the
/// extension lives in its own workspace). When you edit either list,
/// edit both. CI grep enforces nothing — discipline is on the author.
fn builtin_patterns() -> &'static [(&'static str, &'static str)] {
    &[
        ("bearer_jwt", r"Bearer\s+eyJ[\w-]+\.[\w-]+\.[\w-]+"),
        ("anthropic_key", r"sk-ant-[A-Za-z0-9_\-]{20,}"),
        ("openai_key", r"sk-[A-Za-z0-9]{20,}"),
        ("aws_access_key", r"AKIA[0-9A-Z]{16}"),
        // Note: a generic 40-char base64 pattern (would catch AWS
        // secret keys) was deliberately omitted — too many false
        // positives on legitimate hashes / opaque ids. Operators who
        // need it can add it via `extra_patterns` scoped to their data.
        // Floor at 64 hex chars: 32-char (MD5) and 40-char (SHA-1 /
        // git SHA) hashes are too common as legitimate identifiers
        // to redact by default. SHA-256 (64 chars) is the smallest
        // shape where false positives become unlikely enough.
        ("hex_token_64", r"\b[a-fA-F0-9]{64,}\b"),
        ("home_path", r"/(?:home|Users)/[^\s/]+"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_default() -> RedactionConfig {
        RedactionConfig {
            enabled: true,
            use_builtins: true,
            extra_patterns: Vec::new(),
        }
    }

    #[test]
    fn disabled_passthrough() {
        let r = Redactor::disabled();
        let out = r.apply("token sk-abc123def456ghi789jkl0");
        assert_eq!(out.redacted_text, "token sk-abc123def456ghi789jkl0");
        assert!(out.hits.is_empty());
        assert!(!r.is_active());
    }

    #[test]
    fn from_config_disabled_returns_passthrough() {
        let r = Redactor::from_config(&RedactionConfig::default()).unwrap();
        assert!(!r.is_active());
        let out = r.apply("AKIAABCDEFGHIJKLMNOP");
        assert_eq!(out.redacted_text, "AKIAABCDEFGHIJKLMNOP");
    }

    #[test]
    fn empty_input_no_panic() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        let out = r.apply("");
        assert_eq!(out.redacted_text, "");
        assert!(out.hits.is_empty());
    }

    #[test]
    fn redacts_openai_key() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        let out = r.apply("call openai with sk-abc123def456ghi789jkl0mn for billing");
        assert!(out.redacted_text.contains("[REDACTED:openai_key]"));
        assert!(!out.redacted_text.contains("sk-abc123"));
        assert!(out.hits.iter().any(|(l, c)| l == "openai_key" && *c == 1));
    }

    #[test]
    fn redacts_anthropic_key_before_generic_openai() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        let out = r.apply("key sk-ant-abcdefghijklmnopqrstuvwxyz123456");
        assert!(out.redacted_text.contains("[REDACTED:anthropic_key]"));
        assert!(!out.redacted_text.contains("sk-ant-"));
    }

    #[test]
    fn redacts_aws_access_key() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        let out = r.apply("export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE");
        assert!(out.redacted_text.contains("[REDACTED:aws_access_key]"));
    }

    #[test]
    fn redacts_bearer_jwt() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        let out = r.apply("Authorization: Bearer eyJhbGc.eyJzdWI.dGVzdA");
        assert!(out.redacted_text.contains("[REDACTED:bearer_jwt]"));
    }

    #[test]
    fn redacts_hex_token_at_or_above_64_chars() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        // SHA-256 shape (64 chars) — should redact.
        let out = r.apply("digest: 5d41402abc4b2a76b9719d911017c5925d41402abc4b2a76b9719d911017c592");
        assert!(out.redacted_text.contains("[REDACTED:hex_token_64]"));
    }

    #[test]
    fn does_not_redact_md5_or_sha1_hashes() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        // MD5 (32) and git/SHA-1 (40) are too common as legitimate ids.
        let md5 = "5d41402abc4b2a76b9719d911017c592";
        let sha1 = "356a192b7913b04c54574d18c28d46e6395428ab";
        let out = r.apply(&format!("md5={md5} sha1={sha1}"));
        assert!(out.redacted_text.contains(md5), "md5 should pass through");
        assert!(out.redacted_text.contains(sha1), "sha1 should pass through");
    }

    #[test]
    fn redacts_home_path() {
        let r = Redactor::from_config(&enabled_default()).unwrap();
        let out = r.apply("error in /home/familia/chat/foo and /Users/alice/bar");
        assert!(out.redacted_text.contains("[REDACTED:home_path]/chat/foo"));
        assert!(out.redacted_text.contains("[REDACTED:home_path]/bar"));
        let h = out.hits.iter().find(|(l, _)| l == "home_path").unwrap();
        assert_eq!(h.1, 2);
    }

    #[test]
    fn custom_pattern_applied_after_builtins() {
        let cfg = RedactionConfig {
            enabled: true,
            use_builtins: true,
            extra_patterns: vec![RedactionPattern {
                regex: r"TENANT-\d+".into(),
                label: "tenant_id".into(),
            }],
        };
        let r = Redactor::from_config(&cfg).unwrap();
        let out = r.apply("invoice for TENANT-42 on /home/x/file");
        assert!(out.redacted_text.contains("[REDACTED:tenant_id]"));
        assert!(out.redacted_text.contains("[REDACTED:home_path]"));
    }

    #[test]
    fn invalid_extra_regex_errors_with_index() {
        let cfg = RedactionConfig {
            enabled: true,
            use_builtins: false,
            extra_patterns: vec![
                RedactionPattern {
                    regex: r"ok".into(),
                    label: "ok".into(),
                },
                RedactionPattern {
                    regex: r"[invalid".into(),
                    label: "bad".into(),
                },
            ],
        };
        let err = Redactor::from_config(&cfg).expect_err("invalid regex");
        let msg = err.to_string();
        assert!(msg.contains("extra_patterns[1]"), "msg: {msg}");
        assert!(msg.contains("bad"));
    }

    #[test]
    fn empty_label_rejected() {
        let cfg = RedactionConfig {
            enabled: true,
            use_builtins: false,
            extra_patterns: vec![RedactionPattern {
                regex: r"x".into(),
                label: "  ".into(),
            }],
        };
        let err = Redactor::from_config(&cfg).expect_err("empty label");
        assert!(err.to_string().contains("label cannot be empty"));
    }
}
