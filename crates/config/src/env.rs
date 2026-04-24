use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::{Component, Path, PathBuf};

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
    // YAML comment state: once we see `#` (not inside quotes) we skip
    // the rest of the line. Avoids resolving placeholders that only
    // exist in documentation/examples.
    let mut in_comment = false;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i + 1 < len {
        let c = bytes[i];
        if c == b'\n' {
            in_comment = false;
            in_single_quote = false;
            in_double_quote = false;
            i += 1;
            continue;
        }
        if !in_comment && !in_single_quote && !in_double_quote && c == b'#' {
            in_comment = true;
            i += 1;
            continue;
        }
        if in_comment {
            i += 1;
            continue;
        }
        if !in_double_quote && c == b'\'' {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if !in_single_quote && c == b'"' {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }
        if c == b'$' && bytes[i + 1] == b'{' {
            let inner_start = i + 2;
            let mut depth = 1usize;
            let mut j = inner_start;
            while j < len {
                if bytes[j] == b'{' {
                    depth += 1;
                }
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
        let safe = validate_file_ref(path)
            .map_err(|e| anyhow!("${{{inner}}}: refusing to read '{path}': {e} (in {source})"))?;
        let content = fs::read_to_string(&safe).map_err(|e| {
            anyhow!(
                "${{{inner}}}: cannot read file '{}': {e} (in {source})",
                safe.display()
            )
        })?;
        return Ok(content.trim().to_string());
    }

    // Shell-style default: `${VAR:-fallback}` and `${VAR-fallback}`.
    // Both mean "use VAR if set (and non-empty for `:-`), else fallback".
    // Lets YAML leave optional secrets blank without crashing the
    // config loader.
    if let Some((name, default)) = split_default(inner) {
        let colon_minus = inner.contains(":-");
        return Ok(match std::env::var(name.trim()) {
            Ok(v) if colon_minus && v.is_empty() => default.to_string(),
            Ok(v) => v,
            Err(_) => default.to_string(),
        });
    }

    let var_name = inner.trim();
    std::env::var(var_name)
        .map_err(|_| anyhow!("env var {var_name} not set (referenced in {source})"))
}

/// Only allow `${file:...}` to read from documented secret locations.
/// Absolute paths must sit under a whitelisted root (Docker's
/// `/run/secrets/`, the project's `./secrets/`, or an operator-chosen
/// `CONFIG_SECRETS_DIR`). Relative paths are resolved against the
/// current directory and must not contain `..` segments. This blocks a
/// malicious YAML from exfiltrating arbitrary files via the loader.
fn validate_file_ref(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);

    for c in path.components() {
        if matches!(c, Component::ParentDir) {
            bail!("`..` not allowed in file reference");
        }
    }

    if path.is_absolute() {
        let mut roots: Vec<PathBuf> = vec![
            PathBuf::from("/run/secrets"),
            PathBuf::from("/var/run/secrets"),
        ];
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd.join("secrets"));
        }
        if let Ok(custom) = std::env::var("CONFIG_SECRETS_DIR") {
            roots.push(PathBuf::from(custom));
        }
        let abs = path.to_path_buf();
        let inside_root = roots.iter().any(|r| {
            // `starts_with` compares on Path components, so it can't be
            // fooled by `/run/secretsXYZ` looking like a prefix.
            abs.starts_with(r)
        });
        if !inside_root {
            bail!(
                "absolute paths must be under {} or CONFIG_SECRETS_DIR",
                roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(abs)
    } else {
        Ok(path.to_path_buf())
    }
}

/// Split `${VAR:-default}` / `${VAR-default}` into `(VAR, default)`.
/// Returns `None` for plain `${VAR}` (no default separator).
fn split_default(inner: &str) -> Option<(&str, &str)> {
    if let Some(idx) = inner.find(":-") {
        Some((&inner[..idx], &inner[idx + 2..]))
    } else if let Some(idx) = inner.find('-') {
        if idx > 0 {
            Some((&inner[..idx], &inner[idx + 1..]))
        } else {
            None
        }
    } else {
        None
    }
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
        let parent = tmp.path().parent().unwrap().to_path_buf();
        // Whitelist the tempfile's dir so validate_file_ref accepts it.
        std::env::set_var("CONFIG_SECRETS_DIR", &parent);
        let input = format!("key: ${{file:{path}}}");
        let out = resolve_placeholders(&input, "test.yaml").unwrap();
        assert_eq!(out, "key: secret_value");
        std::env::remove_var("CONFIG_SECRETS_DIR");
    }

    #[test]
    fn rejects_file_traversal() {
        let err = resolve_placeholders("key: ${file:../etc/passwd}", "t.yaml").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`..` not allowed"), "got: {msg}");
    }

    #[test]
    fn rejects_absolute_outside_whitelist() {
        std::env::remove_var("CONFIG_SECRETS_DIR");
        let err = resolve_placeholders("key: ${file:/etc/passwd}", "t.yaml").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("absolute paths must be under"), "got: {msg}");
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
