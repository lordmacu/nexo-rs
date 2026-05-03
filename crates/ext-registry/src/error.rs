//! Phase 31.0 — error variants returned by index parse +
//! validation. Each variant names the entry id (when known) +
//! the field at fault so operator-facing errors point at the
//! exact line in the offending registry submission.

/// Errors surfaced by [`crate::ExtRegistryIndex::from_json_slice`]
/// and [`crate::ExtRegistryIndex::validate`].
#[derive(Debug, thiserror::Error)]
pub enum IndexValidationError {
    /// JSON parsing failed (`serde_json::Error`). Surfaced when
    /// the document is not valid JSON, or has unknown fields
    /// (the schema uses `#[serde(deny_unknown_fields)]` so a
    /// typo in any field name surfaces here).
    #[error("ext-registry parse error: {message}")]
    Parse {
        /// Human-readable parser error including the location.
        message: String,
    },

    /// The schema's major version doesn't match what this crate
    /// supports. CLI consumers MUST refuse to install from an
    /// index they don't understand — silently downgrading would
    /// install plugins with unknown invariants.
    #[error(
        "ext-registry schema_version `{got}` not supported (this crate handles \
         major version {want_major}). Upgrade nexo or downgrade the index."
    )]
    SchemaVersionUnsupported {
        /// The schema version present in the document.
        got: String,
        /// The major version this crate supports.
        want_major: u64,
    },

    /// Plugin id failed the regex `^[a-z][a-z0-9_]{0,31}$`.
    #[error(
        "ext-registry entry `id` `{id}` invalid: must match \
         `^[a-z][a-z0-9_]{{0,31}}$`"
    )]
    IdInvalid {
        /// The offending id.
        id: String,
    },

    /// A required string field is empty after trim.
    #[error("ext-registry entry `{id}` field `{field}` must not be empty")]
    FieldEmpty {
        /// Plugin id of the offending entry.
        id: String,
        /// Field name (e.g. `name`, `description`).
        field: &'static str,
    },

    /// A URL field couldn't be parsed.
    #[error("ext-registry entry `{id}` field `{field}` `{got}` is not a valid URL")]
    UrlInvalid {
        /// Plugin id.
        id: String,
        /// Field name.
        field: &'static str,
        /// The offending URL string.
        got: String,
    },

    /// A URL field used a non-HTTPS scheme. Registry artifacts
    /// MUST be served over HTTPS — operator's install CLI
    /// refuses HTTP downloads.
    #[error(
        "ext-registry entry `{id}` field `{field}` `{got}` must use HTTPS"
    )]
    UrlNotHttps {
        /// Plugin id.
        id: String,
        /// Field name.
        field: &'static str,
        /// The offending URL string.
        got: String,
    },

    /// `downloads` array was empty. An entry must have at least
    /// one prebuilt artifact for at least one target.
    #[error(
        "ext-registry entry `{id}` `downloads` is empty — must list at least \
         one target's prebuilt artifact"
    )]
    DownloadsEmpty {
        /// Plugin id.
        id: String,
    },

    /// `sha256` is not exactly 64 lowercase hex chars.
    #[error(
        "ext-registry entry `{id}` download for target `{target}` `sha256` \
         `{got}` invalid: must be exactly 64 lowercase hex characters"
    )]
    Sha256Invalid {
        /// Plugin id.
        id: String,
        /// The download's target triple.
        target: String,
        /// The offending sha256 string.
        got: String,
    },

    /// A `verified` tier entry omitted the `signing` block.
    /// Verified plugins MUST carry cosign signature material so
    /// the install CLI can prove the artifact came from the
    /// nexo-rs maintainers' workflow.
    #[error(
        "ext-registry entry `{id}` is `tier = verified` but has no `signing` \
         block — verified plugins must include cosign signature + certificate \
         URLs"
    )]
    VerifiedRequiresSigning {
        /// Plugin id.
        id: String,
    },
}
