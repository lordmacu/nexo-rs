//! Phase 31.1.b — error variants returned by [`crate::extract::extract_verified_tarball`].

use std::path::PathBuf;

/// Errors surfaced by the tarball extraction pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// IO read/write/rename/remove against the local filesystem failed.
    #[error("extract io error: {0}")]
    Io(String),

    /// Tarball file size exceeds the configured `max_tarball_bytes`.
    #[error("tarball at `{path}` is {actual} bytes, exceeds limit {limit}")]
    TarballTooLarge {
        /// Path of the offending tarball.
        path: PathBuf,
        /// Actual size in bytes.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },

    /// Number of entries (files + dirs) exceeds the configured `max_entries`.
    #[error("tarball entry count exceeds limit {limit}")]
    TooManyEntries {
        /// Configured limit.
        limit: u64,
    },

    /// One entry's header-declared size exceeds `max_entry_bytes`.
    #[error("entry `{path}` size {actual} exceeds per-entry limit {limit}")]
    EntryTooLarge {
        /// Entry path inside the tarball.
        path: String,
        /// Header-declared size.
        actual: u64,
        /// Per-entry limit.
        limit: u64,
    },

    /// Sum of all entry sizes exceeds `max_extracted_bytes`.
    #[error("total extracted size exceeds limit {limit}")]
    ExtractedTooLarge {
        /// Configured limit.
        limit: u64,
    },

    /// Entry path was absolute, contained `..`, was Windows-prefixed, or
    /// included NUL bytes.
    #[error("entry path `{path}` rejected: {reason}")]
    UnsafePath {
        /// Offending entry path as it appeared in the tarball.
        path: String,
        /// Why the path was rejected.
        reason: &'static str,
    },

    /// Entry type was not in the allowed set (regular file or directory).
    #[error("entry `{path}` rejected: type `{kind}` not allowed")]
    DisallowedEntryType {
        /// Entry path as it appeared in the tarball.
        path: String,
        /// Disallowed entry kind label (e.g. `"symlink"`, `"hardlink"`).
        kind: &'static str,
    },

    /// Manifest extracted from the tarball does not match the manifest the
    /// resolver advertised. The mismatch implies tarball/registry tampering
    /// or a publish convention violation; staging is cleaned up.
    #[error(
        "extracted manifest mismatch: expected id={expected_id}@{expected_version}, \
         got id={got_id}@{got_version}"
    )]
    ManifestMismatch {
        /// Expected plugin id (from the resolver).
        expected_id: String,
        /// Expected plugin version (from the resolver).
        expected_version: semver::Version,
        /// Plugin id observed in the tarball's `nexo-plugin.toml`.
        got_id: String,
        /// Plugin version observed in the tarball's `nexo-plugin.toml`.
        got_version: semver::Version,
    },

    /// Manifest file at the given path could not be parsed as a valid
    /// `PluginManifest`.
    #[error("extracted manifest at `{path}` invalid: {reason}")]
    ManifestInvalid {
        /// Path on disk where the manifest was expected.
        path: PathBuf,
        /// Parse failure reason.
        reason: String,
    },

    /// `bin/<plugin_id>` was not present after extraction. Indicates a
    /// publish convention violation upstream.
    #[error("expected binary at `{path}` not found after extract")]
    BinaryMissing {
        /// Path on disk where the binary was expected.
        path: PathBuf,
    },

    /// `tokio::task::spawn_blocking` join failure (panic in the sync extract
    /// task, or runtime shutdown mid-extract).
    #[error("extract task panicked: {0}")]
    JoinError(String),
}
