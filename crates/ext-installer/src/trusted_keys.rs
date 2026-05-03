//! Phase 31.3 — operator-side trust policy loaded from
//! `<config_dir>/extensions/trusted_keys.toml`.
//!
//! Three trust modes (`ignore | warn | require`) compose with a
//! per-author allowlist. Each `[[authors]]` entry binds a repo
//! owner to a cosign certificate identity regex; an entry's `mode`
//! field overrides the global default.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::verify_error::VerifyError;

const TRUSTED_KEYS_FILE: &str = "extensions/trusted_keys.toml";

fn default_schema() -> String {
    "1.0".to_string()
}

fn default_oidc_issuer() -> String {
    "https://token.actions.githubusercontent.com".to_string()
}

/// Trust mode applied to a single install. See module docs for
/// semantics.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrustMode {
    /// Skip cosign verification entirely.
    Ignore,
    /// Verify when `.sig` + `.cert` are present; warn + proceed
    /// when absent.
    #[default]
    Warn,
    /// Reject any install whose tarball does not produce a valid
    /// allowlisted signature.
    Require,
}

impl TrustMode {
    /// Stable lowercase string used as the JSON `trust_mode` field
    /// on the install report.
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustMode::Ignore => "ignore",
            TrustMode::Warn => "warn",
            TrustMode::Require => "require",
        }
    }
}

/// Per-author trust entry from the `[[authors]]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorPolicy {
    /// Repo owner (exact match against `coords.owner`).
    pub owner: String,
    /// Regex matching the cosign certificate Subject Alternative
    /// Name. In keyless flow the SAN encodes the GitHub Actions
    /// workflow URL.
    pub identity_regexp: String,
    /// OIDC issuer the cert was minted by. Defaults to the
    /// GitHub Actions issuer.
    #[serde(default = "default_oidc_issuer")]
    pub oidc_issuer: String,
    /// Override the global default trust mode for this owner.
    /// `None` inherits the global default.
    #[serde(default)]
    pub mode: Option<TrustMode>,
}

/// Top-level config shape.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKeysConfig {
    /// Schema version of this file. Currently always `"1.0"`.
    #[serde(default = "default_schema")]
    pub schema_version: String,
    /// Global default applied when no `[[authors]]` entry matches.
    #[serde(default)]
    pub default: TrustMode,
    /// Optional override for the cosign binary path. When unset
    /// the verifier searches `$PATH` + well-known fallbacks.
    #[serde(default)]
    pub cosign_binary: Option<PathBuf>,
    /// Per-author trust entries. Searched in declaration order.
    #[serde(default)]
    pub authors: Vec<AuthorPolicy>,
}

impl TrustedKeysConfig {
    /// Load from `<config_dir>/extensions/trusted_keys.toml`.
    /// Returns the global-default config (`Warn` mode, zero
    /// authors) when the file is absent.
    pub fn load(config_dir: &Path) -> Result<Self, VerifyError> {
        let path = config_dir.join(TRUSTED_KEYS_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(&path).map_err(|e| VerifyError::TrustedKeysParse {
            path: path.clone(),
            reason: format!("read failed: {e}"),
        })?;
        let cfg: Self = toml::from_str(&body).map_err(|e| VerifyError::TrustedKeysParse {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        // Validate every identity_regexp compiles.
        for author in &cfg.authors {
            regex::Regex::new(&author.identity_regexp).map_err(|e| {
                VerifyError::IdentityRegexpInvalid {
                    got: author.identity_regexp.clone(),
                    reason: e.to_string(),
                }
            })?;
        }
        Ok(cfg)
    }

    /// Resolve the effective policy for a given repo owner.
    /// Returns `(mode, Option<author>)`. `None` means no
    /// per-author entry matched; mode is the global default.
    pub fn resolve(&self, owner: &str) -> (TrustMode, Option<&AuthorPolicy>) {
        for author in &self.authors {
            if author.owner == owner {
                return (author.mode.unwrap_or(self.default), Some(author));
            }
        }
        (self.default, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_config(dir: &Path, body: &str) -> PathBuf {
        let ext_dir = dir.join("extensions");
        fs::create_dir_all(&ext_dir).unwrap();
        let path = ext_dir.join("trusted_keys.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn parses_minimal_default() {
        let tmp = TempDir::new().unwrap();
        write_config(tmp.path(), r#"schema_version = "1.0""#);
        let cfg = TrustedKeysConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.schema_version, "1.0");
        assert_eq!(cfg.default, TrustMode::Warn);
        assert!(cfg.authors.is_empty());
    }

    #[test]
    fn parses_global_default_and_authors() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
schema_version = "1.0"
default = "require"

[[authors]]
owner = "lordmacu"
identity_regexp = "^https://github.com/lordmacu/.*$"

[[authors]]
owner = "other"
identity_regexp = "^https://github.com/other/.*$"
oidc_issuer = "https://example.com"
mode = "ignore"
"#,
        );
        let cfg = TrustedKeysConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.default, TrustMode::Require);
        assert_eq!(cfg.authors.len(), 2);
        assert_eq!(cfg.authors[0].owner, "lordmacu");
        assert_eq!(cfg.authors[0].oidc_issuer, default_oidc_issuer());
        assert!(cfg.authors[0].mode.is_none());
        assert_eq!(cfg.authors[1].mode, Some(TrustMode::Ignore));
        assert_eq!(cfg.authors[1].oidc_issuer, "https://example.com");
    }

    #[test]
    fn rejects_unknown_fields() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
schema_version = "1.0"
unknown_field = "x"
"#,
        );
        let err = TrustedKeysConfig::load(tmp.path()).unwrap_err();
        assert!(matches!(err, VerifyError::TrustedKeysParse { .. }));
    }

    #[test]
    fn resolve_returns_global_default_when_no_match() {
        let cfg = TrustedKeysConfig {
            default: TrustMode::Require,
            ..Default::default()
        };
        let (mode, hit) = cfg.resolve("nobody");
        assert_eq!(mode, TrustMode::Require);
        assert!(hit.is_none());
    }

    #[test]
    fn resolve_returns_author_override_when_present() {
        let cfg = TrustedKeysConfig {
            default: TrustMode::Warn,
            authors: vec![AuthorPolicy {
                owner: "alice".into(),
                identity_regexp: "^.*$".into(),
                oidc_issuer: default_oidc_issuer(),
                mode: Some(TrustMode::Require),
            }],
            ..Default::default()
        };
        let (mode, hit) = cfg.resolve("alice");
        assert_eq!(mode, TrustMode::Require);
        assert!(hit.is_some());
    }

    #[test]
    fn resolve_inherits_global_when_author_mode_unset() {
        let cfg = TrustedKeysConfig {
            default: TrustMode::Require,
            authors: vec![AuthorPolicy {
                owner: "alice".into(),
                identity_regexp: "^.*$".into(),
                oidc_issuer: default_oidc_issuer(),
                mode: None,
            }],
            ..Default::default()
        };
        let (mode, hit) = cfg.resolve("alice");
        assert_eq!(mode, TrustMode::Require);
        assert!(hit.is_some());
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let cfg = TrustedKeysConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.default, TrustMode::Warn);
        assert!(cfg.authors.is_empty());
    }

    #[test]
    fn rejects_invalid_identity_regexp() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
[[authors]]
owner = "alice"
identity_regexp = "[unterminated"
"#,
        );
        let err = TrustedKeysConfig::load(tmp.path()).unwrap_err();
        assert!(matches!(err, VerifyError::IdentityRegexpInvalid { .. }));
    }

    #[test]
    fn rejects_invalid_trust_mode() {
        let tmp = TempDir::new().unwrap();
        write_config(
            tmp.path(),
            r#"
default = "yolo"
"#,
        );
        let err = TrustedKeysConfig::load(tmp.path()).unwrap_err();
        assert!(matches!(err, VerifyError::TrustedKeysParse { .. }));
    }

    #[test]
    fn trust_mode_as_str_returns_canonical_lowercase() {
        assert_eq!(TrustMode::Ignore.as_str(), "ignore");
        assert_eq!(TrustMode::Warn.as_str(), "warn");
        assert_eq!(TrustMode::Require.as_str(), "require");
    }
}
