//! Mini-redactor used to scrub stdout/stderr before returning to the
//! LLM. Pattern set is deliberately narrow — this extension lives in
//! its own workspace and must not depend on `agent-core`. The same
//! built-in shapes the runtime redactor uses are duplicated here.

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
            (
                Regex::new(r"\b[a-fA-F0-9]{32,}\b").unwrap(),
                "hex_token_32",
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
    fn redacts_hex_token() {
        assert!(redact("ETag: 5d41402abc4b2a76b9719d911017c592").contains("[REDACTED:hex_token_32]"));
    }
}
