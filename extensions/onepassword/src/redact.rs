//! Mini-redactor used to scrub stdout/stderr before returning to the
//! LLM. Pattern set is deliberately narrow — this extension lives in
//! its own workspace and must not depend on `nexo-core`. The same
//! built-in shapes the runtime redactor uses are duplicated here.
//!
//! **Mirror invariant:** `crates/core/src/agent/redaction.rs::builtin_patterns`
//! ships the same pattern set. When you edit either list, edit both.

use regex::Regex;
use std::sync::OnceLock;

fn rules() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            (
                Regex::new(r"Bearer\s+eyJ[\w-]+\.[\w-]+\.[\w-]+").unwrap(),
                "bearer_jwt",
            ),
            (
                Regex::new(r"sk-ant-[A-Za-z0-9_\-]{20,}").unwrap(),
                "anthropic_key",
            ),
            (Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(), "openai_key"),
            (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "aws_access_key"),
            // Floor at 64 hex chars: MD5 (32) and git/SHA-1 (40) are
            // too common as legitimate identifiers. Mirrors the
            // floor used in `crates/core/src/agent/redaction.rs`.
            (
                Regex::new(r"\b[a-fA-F0-9]{64,}\b").unwrap(),
                "hex_token_64",
            ),
        ]
    })
}

pub fn redact(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let mut text = input.to_string();
    for (re, label) in rules() {
        let replacement = format!("[REDACTED:{label}]");
        text = re.replace_all(&text, replacement.as_str()).into_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(redact(""), "");
    }

    #[test]
    fn passthrough_when_no_match() {
        assert_eq!(redact("nothing to see"), "nothing to see");
    }

    #[test]
    fn redacts_openai_key() {
        let out = redact("token sk-abcdefghijklmnopqrstuvwxyz0123");
        assert!(out.contains("[REDACTED:openai_key]"));
        assert!(!out.contains("sk-abcdef"));
    }

    #[test]
    fn redacts_anthropic_key() {
        let out = redact("key sk-ant-abcdefghijklmnopqrstuvwxyz123456");
        assert!(out.contains("[REDACTED:anthropic_key]"));
    }

    #[test]
    fn redacts_aws_access_key() {
        assert!(redact("AKIAIOSFODNN7EXAMPLE").contains("[REDACTED:aws_access_key]"));
    }

    #[test]
    fn redacts_bearer_jwt() {
        let out = redact("Authorization: Bearer eyJhbGc.eyJzdWI.dGVzdA");
        assert!(out.contains("[REDACTED:bearer_jwt]"));
    }

    #[test]
    fn redacts_hex_token_at_or_above_64_chars() {
        let sha256 = "5d41402abc4b2a76b9719d911017c5925d41402abc4b2a76b9719d911017c592";
        assert!(redact(&format!("digest: {sha256}")).contains("[REDACTED:hex_token_64]"));
    }

    #[test]
    fn does_not_redact_md5_or_sha1() {
        let md5 = "5d41402abc4b2a76b9719d911017c592";
        let sha1 = "356a192b7913b04c54574d18c28d46e6395428ab";
        let out = redact(&format!("md5={md5} sha1={sha1}"));
        assert!(out.contains(md5));
        assert!(out.contains(sha1));
    }
}
