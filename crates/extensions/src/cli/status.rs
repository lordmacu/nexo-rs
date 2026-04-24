//! Status classification for CLI output.

use std::path::Path;

use crate::discovery::{DiagnosticLevel, DiscoveryDiagnostic};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliStatus {
    Enabled,
    Disabled,
    ManifestError(String),
}

impl CliStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::ManifestError(_) => "error",
        }
    }
}

/// Resolve the display status for a discovered manifest id.
/// `root_dir` is only used when no manifest is present (error rows).
pub fn resolve_status_for_candidate(id: &str, disabled: &[String]) -> CliStatus {
    if disabled.iter().any(|d| d == id) {
        CliStatus::Disabled
    } else {
        CliStatus::Enabled
    }
}

/// Pull the first Error-level diagnostic that points at the given root dir.
pub fn error_for_path<'a>(
    diags: &'a [DiscoveryDiagnostic],
    path: &Path,
) -> Option<&'a DiscoveryDiagnostic> {
    diags
        .iter()
        .find(|d| d.level == DiagnosticLevel::Error && d.path.starts_with(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn enabled_when_not_in_disabled_list() {
        let s = resolve_status_for_candidate("weather", &[]);
        assert_eq!(s, CliStatus::Enabled);
    }

    #[test]
    fn disabled_when_id_present() {
        let s = resolve_status_for_candidate("weather", &["weather".into()]);
        assert_eq!(s, CliStatus::Disabled);
    }

    #[test]
    fn error_lookup_matches_path_prefix() {
        let diags = vec![DiscoveryDiagnostic {
            level: DiagnosticLevel::Error,
            path: PathBuf::from("/tmp/ext/broken/plugin.toml"),
            message: "bad".into(),
        }];
        let hit = error_for_path(&diags, &PathBuf::from("/tmp/ext/broken"));
        assert!(hit.is_some());
        let miss = error_for_path(&diags, &PathBuf::from("/tmp/ext/ok"));
        assert!(miss.is_none());
    }
}
