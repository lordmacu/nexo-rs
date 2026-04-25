//! Path-traversal-guarded file regex matcher.

use std::path::Path;

use regex::Regex;

use crate::error::DriverError;

/// Returns `Ok(None)` on pass; `Ok(Some(message))` on fail; `Err`
/// only for genuine errors (bad regex, path escapes workspace).
pub(crate) async fn check_file(
    path: &Path,
    regex_str: &str,
    required: bool,
    workspace: &Path,
) -> Result<Option<String>, DriverError> {
    let target = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };

    let workspace_canonical = tokio::fs::canonicalize(workspace).await?;

    let canonical = match tokio::fs::canonicalize(&target).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if required {
                return Ok(Some(format!("file not found: {}", target.display())));
            } else {
                return Ok(None);
            }
        }
        Err(e) => {
            return Err(DriverError::Acceptance(format!(
                "canonicalize {}: {e}",
                target.display()
            )));
        }
    };

    if !canonical.starts_with(&workspace_canonical) {
        return Err(DriverError::Acceptance(format!(
            "file_matches path escapes workspace: {}",
            canonical.display()
        )));
    }

    let raw = tokio::fs::read_to_string(&canonical)
        .await
        .map_err(|e| DriverError::Acceptance(format!("read {}: {e}", canonical.display())))?;

    let re = Regex::new(regex_str)
        .map_err(|e| DriverError::Acceptance(format!("bad regex {regex_str:?}: {e}")))?;

    if re.is_match(&raw) {
        Ok(None)
    } else {
        Ok(Some(format!(
            "file_matches({}): regex did not match: {regex_str}",
            canonical.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        tokio::fs::write(&p, body).await.unwrap();
        p
    }

    #[tokio::test]
    async fn matches_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.md", "hello world").await;
        let r = check_file(Path::new("x.md"), "hello", true, dir.path())
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn no_match_returns_failure() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.md", "hello world").await;
        let r = check_file(Path::new("x.md"), "byebye", true, dir.path())
            .await
            .unwrap();
        assert!(r.is_some());
    }

    #[tokio::test]
    async fn missing_required_fails() {
        let dir = tempfile::tempdir().unwrap();
        let r = check_file(Path::new("missing.md"), "x", true, dir.path())
            .await
            .unwrap();
        assert!(r.is_some());
    }

    #[tokio::test]
    async fn missing_optional_passes() {
        let dir = tempfile::tempdir().unwrap();
        let r = check_file(Path::new("missing.md"), "x", false, dir.path())
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn traversal_escape_errors() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("y.md");
        tokio::fs::write(&outside_file, "x").await.unwrap();
        let err = check_file(&outside_file, "x", true, workspace.path())
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Acceptance(_)));
    }

    #[tokio::test]
    async fn bad_regex_errors() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "x.md", "hi").await;
        let err = check_file(Path::new("x.md"), "(((", true, dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Acceptance(_)));
    }
}
