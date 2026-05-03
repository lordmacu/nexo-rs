//! Phase 31.0 — `ext-registry` index format types + parser +
//! validation.
//!
//! The ext-registry is the catalog of installable nexo plugins
//! (out-of-tree subprocess plugins authored against
//! `nexo-plugin-contract.md`). A single JSON document at a
//! well-known URL (e.g. `https://nexo-rs.dev/ext-index.json`)
//! lists every plugin available for `nexo ext install <id>`.
//!
//! This crate ships ONLY the data types + parser + validation.
//! Phase 31.1 (`nexo ext install`), 31.2 (CI publish workflow),
//! 31.3 (cosign signing + trusted keys) consume these types.
//!
//! # Wire shape (schema_version = 1.0.0)
//!
//! ```json
//! {
//!   "schema_version": "1.0.0",
//!   "generated_at": "2026-05-01T00:00:00Z",
//!   "entries": [
//!     {
//!       "id": "slack",
//!       "version": "0.2.0",
//!       "name": "Slack Channel",
//!       "description": "Slack bot integration",
//!       "homepage": "https://github.com/nexo-rs/plugin-slack",
//!       "tier": "verified",
//!       "min_nexo_version": ">=0.1.0",
//!       "downloads": [
//!         {
//!           "target": "x86_64-unknown-linux-gnu",
//!           "url": "https://.../slack-0.2.0-x86_64-linux.tar.gz",
//!           "sha256": "abcd1234...",
//!           "size_bytes": 1234567
//!         }
//!       ],
//!       "manifest_url": "https://.../nexo-plugin.toml",
//!       "signing": {
//!         "cosign_signature_url": "https://.../slack-0.2.0.sig",
//!         "cosign_certificate_url": "https://.../slack-0.2.0.cert"
//!       },
//!       "authors": ["Cristian Garcia <informacion@cristiangarcia.co>"]
//!     }
//!   ]
//! }
//! ```
//!
//! # IRROMPIBLE refs
//!
//! - Internal precedent: `crates/plugin-manifest/` — per-plugin
//!   manifest schema; the registry index is a CATALOG OF those.
//!   Distinct shape, distinct crate.
//! - OpenClaw absence: `research/extensions/<id>/openclaw.plugin.json`
//!   are individual plugin manifests; OpenClaw has no remote
//!   index, all plugins live in the same repo.
//! - claude-code-leak: no remote plugin index — `src/plugins/`
//!   builtins ship inline with the binary.
//! - Real-world references for the format design: `crates.io`
//!   index (per-crate JSON files in a git repo), npm registry
//!   (single doc per package), brew taps (Ruby formulae).
//!   We chose a single-document layout because nexo's plugin
//!   count starts small (~10s, not millions like crates.io).

#![deny(missing_docs)]

use std::str::FromStr;

use chrono::{DateTime, Utc};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

pub mod error;
pub use error::IndexValidationError;

/// Current schema version of the registry index.
pub const SCHEMA_VERSION: &str = "1.0.0";

/// SHA-256 hex string length (32 bytes × 2 hex chars).
pub const SHA256_HEX_LEN: usize = 64;

/// Top-level shape of `ext-index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtRegistryIndex {
    /// Semver of the registry SCHEMA itself (this crate's
    /// shape). Today: `1.0.0`. Bumped per the contract bump
    /// rules — additive fields = minor; removal/rename = major.
    pub schema_version: Version,
    /// UTC timestamp when this index was last regenerated. CI
    /// pipelines (Phase 31.2) write this on every publish.
    pub generated_at: DateTime<Utc>,
    /// Catalog of plugins available for installation.
    pub entries: Vec<ExtEntry>,
}

/// One installable plugin entry. Maps an `id@version` pair to
/// its prebuilt download artifacts + signing material + manifest
/// URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtEntry {
    /// Plugin id matching the `id` field in the plugin's
    /// `nexo-plugin.toml`. Globally unique across the registry.
    /// Regex `^[a-z][a-z0-9_]{0,31}$`.
    pub id: String,
    /// Plugin version (semver). Multiple entries with the same
    /// `id` but different `version` MAY appear — the install
    /// CLI resolves to the highest matching `min_nexo_version`.
    pub version: Version,
    /// Human-readable plugin name (UI / docs).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Project homepage. Plugin author's public URL — typically
    /// the GitHub repo. Used by `nexo ext info <id>`.
    pub homepage: String,
    /// Trust tier — `Verified` (signed by nexo-rs maintainers)
    /// vs `Community` (third-party signed). Operator's
    /// `config/extensions/trusted_keys.toml` decides which
    /// tier(s) to accept (Phase 31.3).
    pub tier: ExtTier,
    /// Minimum daemon version that supports this plugin.
    /// `nexo ext install` rejects entries whose
    /// `min_nexo_version` doesn't match the running daemon.
    pub min_nexo_version: VersionReq,
    /// Per-target prebuilt download artifacts. Each target
    /// (linux x86_64, macos arm64, etc.) has its own URL +
    /// sha256 + size. Empty list rejected at validate time.
    pub downloads: Vec<ExtDownload>,
    /// URL of the plugin's `nexo-plugin.toml` manifest. Used
    /// by `nexo ext info <id>` to show capabilities + channels
    /// the plugin contributes BEFORE the operator agrees to
    /// install. The CLI verifies this manifest's content matches
    /// the binary at install time.
    pub manifest_url: String,
    /// Cosign signing material. Required when `tier ==
    /// Verified`; optional for `Community` (operator chooses
    /// whether to require signatures via trusted_keys.toml).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<ExtSigning>,
    /// Plugin authors (free-form strings, typically `Name
    /// <email>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
}

/// Trust tier of a plugin entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtTier {
    /// Signed by nexo-rs maintainers' cosign key. The default
    /// trust level — operators run these without extra config.
    Verified,
    /// Signed by a third-party cosign key. Operator must add
    /// the key to `config/extensions/trusted_keys.toml`
    /// (Phase 31.3) before `nexo ext install` accepts.
    Community,
}

/// One target's download artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtDownload {
    /// Rust target triple (e.g. `x86_64-unknown-linux-gnu`,
    /// `aarch64-apple-darwin`). Operator's daemon picks the
    /// matching target at install time.
    pub target: String,
    /// HTTPS URL of the prebuilt tarball / zip. The install
    /// CLI fetches this, verifies sha256, then unpacks.
    pub url: String,
    /// SHA-256 hex digest (64 lowercase hex chars).
    pub sha256: String,
    /// Artifact size in bytes. Used for progress reporting +
    /// sanity check (download larger than this = abort).
    pub size_bytes: u64,
}

/// Cosign signing material. Sigstore keyless flow stores both
/// the signature and the certificate (the operator verifies the
/// signature against the certificate, then verifies the
/// certificate's identity matches an allowlisted source).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtSigning {
    /// HTTPS URL of the cosign signature file (`.sig`).
    pub cosign_signature_url: String,
    /// HTTPS URL of the cosign certificate (`.cert` / `.pem`).
    /// In keyless flow, the certificate's Subject Alternative
    /// Name carries the GitHub Actions workflow identity that
    /// produced the signature.
    pub cosign_certificate_url: String,
}

impl ExtRegistryIndex {
    /// Parse an index from JSON bytes. Returns the parsed
    /// document; does NOT validate semantic constraints — call
    /// [`Self::validate`] for that.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, IndexValidationError> {
        serde_json::from_slice::<Self>(bytes).map_err(|e| {
            IndexValidationError::Parse {
                message: e.to_string(),
            }
        })
    }

    /// Validate the index's semantic constraints:
    /// - `schema_version` matches a known compat range
    /// - every entry's `id` matches the regex
    /// - every `manifest_url` / download `url` / signing url
    ///   parses as a valid URL
    /// - every download's `sha256` is exactly 64 lowercase hex
    ///   chars
    /// - `downloads` is non-empty per entry
    /// - `Verified` tier entries MUST carry signing material
    pub fn validate(&self) -> Result<(), IndexValidationError> {
        let current = Version::parse(SCHEMA_VERSION).expect("constant valid semver");
        if self.schema_version.major != current.major {
            return Err(IndexValidationError::SchemaVersionUnsupported {
                got: self.schema_version.to_string(),
                want_major: current.major,
            });
        }
        for entry in &self.entries {
            validate_entry(entry)?;
        }
        Ok(())
    }
}

fn validate_entry(entry: &ExtEntry) -> Result<(), IndexValidationError> {
    if !is_valid_id(&entry.id) {
        return Err(IndexValidationError::IdInvalid {
            id: entry.id.clone(),
        });
    }
    if entry.name.trim().is_empty() {
        return Err(IndexValidationError::FieldEmpty {
            id: entry.id.clone(),
            field: "name",
        });
    }
    if entry.description.trim().is_empty() {
        return Err(IndexValidationError::FieldEmpty {
            id: entry.id.clone(),
            field: "description",
        });
    }
    validate_https_url(&entry.id, "homepage", &entry.homepage)?;
    validate_https_url(&entry.id, "manifest_url", &entry.manifest_url)?;
    if entry.downloads.is_empty() {
        return Err(IndexValidationError::DownloadsEmpty {
            id: entry.id.clone(),
        });
    }
    for d in &entry.downloads {
        if d.target.trim().is_empty() {
            return Err(IndexValidationError::FieldEmpty {
                id: entry.id.clone(),
                field: "downloads.target",
            });
        }
        validate_https_url(&entry.id, "downloads.url", &d.url)?;
        if d.sha256.len() != SHA256_HEX_LEN
            || !d.sha256.chars().all(|c| c.is_ascii_hexdigit())
            || d.sha256.chars().any(|c| c.is_ascii_uppercase())
        {
            return Err(IndexValidationError::Sha256Invalid {
                id: entry.id.clone(),
                target: d.target.clone(),
                got: d.sha256.clone(),
            });
        }
    }
    if let Some(s) = &entry.signing {
        validate_https_url(&entry.id, "signing.cosign_signature_url", &s.cosign_signature_url)?;
        validate_https_url(
            &entry.id,
            "signing.cosign_certificate_url",
            &s.cosign_certificate_url,
        )?;
    } else if matches!(entry.tier, ExtTier::Verified) {
        return Err(IndexValidationError::VerifiedRequiresSigning {
            id: entry.id.clone(),
        });
    }
    Ok(())
}

fn validate_https_url(
    entry_id: &str,
    field: &'static str,
    raw: &str,
) -> Result<(), IndexValidationError> {
    let url = url::Url::from_str(raw).map_err(|_| IndexValidationError::UrlInvalid {
        id: entry_id.to_string(),
        field,
        got: raw.to_string(),
    })?;
    if url.scheme() != "https" {
        return Err(IndexValidationError::UrlNotHttps {
            id: entry_id.to_string(),
            field,
            got: raw.to_string(),
        });
    }
    Ok(())
}

fn is_valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 32 {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid index — one verified plugin with all
    /// required fields. Used as the baseline; negative tests
    /// mutate one field at a time.
    fn sample_index_json() -> String {
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": "1.0.0",
            "generated_at": "2026-05-01T00:00:00Z",
            "entries": [{
                "id": "slack",
                "version": "0.2.0",
                "name": "Slack Channel",
                "description": "Slack bot integration",
                "homepage": "https://github.com/nexo-rs/plugin-slack",
                "tier": "verified",
                "min_nexo_version": ">=0.1.0",
                "downloads": [{
                    "target": "x86_64-unknown-linux-gnu",
                    "url": "https://example.com/slack-0.2.0-x86_64-linux.tar.gz",
                    "sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
                    "size_bytes": 1234567
                }],
                "manifest_url": "https://example.com/slack-0.2.0/nexo-plugin.toml",
                "signing": {
                    "cosign_signature_url": "https://example.com/slack-0.2.0.sig",
                    "cosign_certificate_url": "https://example.com/slack-0.2.0.cert"
                },
                "authors": ["Test Author <test@example.com>"]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn parse_and_validate_minimal_verified_entry() {
        let json = sample_index_json();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes())
            .expect("parses cleanly");
        assert_eq!(index.schema_version.to_string(), "1.0.0");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].id, "slack");
        assert_eq!(index.entries[0].tier, ExtTier::Verified);
        index.validate().expect("validates cleanly");
    }

    #[test]
    fn rejects_invalid_id() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v["entries"][0]["id"] = serde_json::json!("UPPERCASE");
        let json = serde_json::to_string(&v).unwrap();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes()).unwrap();
        match index.validate() {
            Err(IndexValidationError::IdInvalid { id }) => {
                assert_eq!(id, "UPPERCASE");
            }
            other => panic!("expected IdInvalid, got {other:?}"),
        }
    }

    #[test]
    fn rejects_non_https_url() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v["entries"][0]["homepage"] = serde_json::json!("http://insecure.example.com");
        let json = serde_json::to_string(&v).unwrap();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes()).unwrap();
        match index.validate() {
            Err(IndexValidationError::UrlNotHttps { field, .. }) => {
                assert_eq!(field, "homepage");
            }
            other => panic!("expected UrlNotHttps, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_sha256() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        // Wrong length.
        v["entries"][0]["downloads"][0]["sha256"] = serde_json::json!("tooshort");
        let json = serde_json::to_string(&v).unwrap();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes()).unwrap();
        match index.validate() {
            Err(IndexValidationError::Sha256Invalid { got, .. }) => {
                assert_eq!(got, "tooshort");
            }
            other => panic!("expected Sha256Invalid (length), got {other:?}"),
        }

        // Uppercase hex.
        let mut v2: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v2["entries"][0]["downloads"][0]["sha256"] = serde_json::json!(
            "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789"
        );
        let json2 = serde_json::to_string(&v2).unwrap();
        let index2 = ExtRegistryIndex::from_json_slice(json2.as_bytes()).unwrap();
        match index2.validate() {
            Err(IndexValidationError::Sha256Invalid { .. }) => {}
            other => panic!("expected Sha256Invalid (uppercase), got {other:?}"),
        }
    }

    #[test]
    fn rejects_verified_without_signing() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("signing");
        let json = serde_json::to_string(&v).unwrap();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes()).unwrap();
        match index.validate() {
            Err(IndexValidationError::VerifiedRequiresSigning { id }) => {
                assert_eq!(id, "slack");
            }
            other => panic!("expected VerifiedRequiresSigning, got {other:?}"),
        }
    }

    #[test]
    fn community_tier_can_omit_signing() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v["entries"][0]["tier"] = serde_json::json!("community");
        v["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("signing");
        let json = serde_json::to_string(&v).unwrap();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes()).unwrap();
        index
            .validate()
            .expect("community tier without signing must validate");
    }

    #[test]
    fn rejects_empty_downloads() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v["entries"][0]["downloads"] = serde_json::json!([]);
        let json = serde_json::to_string(&v).unwrap();
        let index = ExtRegistryIndex::from_json_slice(json.as_bytes()).unwrap();
        match index.validate() {
            Err(IndexValidationError::DownloadsEmpty { id }) => {
                assert_eq!(id, "slack");
            }
            other => panic!("expected DownloadsEmpty, got {other:?}"),
        }
    }

    /// Sanity check: the bundled `examples/sample-ext-index.json`
    /// parses + validates cleanly. If a future schema change
    /// breaks this, both must update together — the sample is
    /// the operator-facing reference for what a valid index
    /// looks like.
    #[test]
    fn bundled_sample_index_parses_and_validates() {
        let bytes = include_bytes!("../examples/sample-ext-index.json");
        let index = ExtRegistryIndex::from_json_slice(bytes)
            .expect("sample parses");
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].id, "slack");
        assert_eq!(index.entries[0].tier, ExtTier::Verified);
        assert_eq!(index.entries[1].id, "discord");
        assert_eq!(index.entries[1].tier, ExtTier::Community);
        index.validate().expect("sample validates");
    }

    #[test]
    fn rejects_unknown_field_in_entry() {
        let mut v: serde_json::Value =
            serde_json::from_str(&sample_index_json()).unwrap();
        v["entries"][0]["mystery_field"] = serde_json::json!("oops");
        let json = serde_json::to_string(&v).unwrap();
        match ExtRegistryIndex::from_json_slice(json.as_bytes()) {
            Err(IndexValidationError::Parse { message }) => {
                assert!(message.contains("mystery_field"));
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }
}
