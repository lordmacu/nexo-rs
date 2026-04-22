use std::fs;
use anyhow::{anyhow, Result};

pub fn resolve_placeholders(content: &str, source: &str) -> Result<String> {
    let mut result = String::with_capacity(content.len());
    let mut last = 0;

    for (start, end, inner) in find_placeholders(content) {
        result.push_str(&content[last..start]);
        let value = resolve_one(inner, source)?;
        result.push_str(&value);
        last = end;
    }
    result.push_str(&content[last..]);
    Ok(result)
}

fn find_placeholders(content: &str) -> Vec<(usize, usize, &str)> {
    let mut matches = Vec::new();
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 1 < len {
        if bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let inner_start = i + 2;
            let mut depth = 1usize;
            let mut j = inner_start;
            while j < len {
                if bytes[j] == b'{' { depth += 1; }
                if bytes[j] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        matches.push((i, j + 1, &content[inner_start..j]));
                        i = j + 1;
                        break;
                    }
                }
                j += 1;
            }
        } else {
            i += 1;
        }
    }
    matches
}

fn resolve_one(inner: &str, source: &str) -> Result<String> {
    if let Some(path) = inner.strip_prefix("file:") {
        let content = fs::read_to_string(path)
            .map_err(|e| anyhow!("${{{inner}}}: cannot read file '{path}': {e} (in {source})"))?;
        return Ok(content.trim().to_string());
    }

    let var_name = inner.trim();
    std::env::var(var_name)
        .map_err(|_| anyhow!("env var {var_name} not set (referenced in {source})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resolves_env_var() {
        std::env::set_var("TEST_CONFIG_VAR", "hello");
        let out = resolve_placeholders("key: ${TEST_CONFIG_VAR}", "test.yaml").unwrap();
        assert_eq!(out, "key: hello");
        std::env::remove_var("TEST_CONFIG_VAR");
    }

    #[test]
    fn error_on_missing_var() {
        std::env::remove_var("DEFINITELY_NOT_SET_XYZ");
        let err = resolve_placeholders("key: ${DEFINITELY_NOT_SET_XYZ}", "llm.yaml").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("DEFINITELY_NOT_SET_XYZ"), "got: {msg}");
        assert!(msg.contains("llm.yaml"), "got: {msg}");
    }

    #[test]
    fn resolves_file_secret() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "  secret_value  ").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let input = format!("key: ${{file:{path}}}");
        let out = resolve_placeholders(&input, "test.yaml").unwrap();
        assert_eq!(out, "key: secret_value");
    }

    #[test]
    fn passthrough_no_placeholders() {
        let out = resolve_placeholders("key: plain_value", "test.yaml").unwrap();
        assert_eq!(out, "key: plain_value");
    }

    #[test]
    fn multiple_vars_in_one_file() {
        std::env::set_var("TEST_A", "foo");
        std::env::set_var("TEST_B", "bar");
        let out = resolve_placeholders("a: ${TEST_A}\nb: ${TEST_B}", "test.yaml").unwrap();
        assert_eq!(out, "a: foo\nb: bar");
        std::env::remove_var("TEST_A");
        std::env::remove_var("TEST_B");
    }
}
