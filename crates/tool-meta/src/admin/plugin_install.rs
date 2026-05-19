//! `nexo/admin/plugins/{scan,install,uninstall}` wire types.
//!
//! Phase 97.1 — runtime plugin install / discovery / removal.
//! Lets the operator add a new plugin from crates.io (or pre-built
//! binary already present on `$PATH`) without a daemon restart.
//! Mirrors the shape of [`crate::admin::plugin_restart`] —
//! one Params/Response pair per verb, no nested unions.

use serde::{Deserialize, Serialize};

// ── scan ────────────────────────────────────────────────────────

/// Params for `nexo/admin/plugins/scan`. Empty for now; the daemon
/// re-runs its configured discovery walker and reports diffs.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsScanParams {}

/// Response for `nexo/admin/plugins/scan`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsScanResponse {
    /// Plugin ids the daemon discovered that were NOT in the live
    /// registry before the scan. Each was hot-spawned and is now
    /// reachable via the dispatcher, pairing trigger registry, and
    /// admin router. Order: discovery walk order.
    pub spawned: Vec<String>,
    /// Plugin ids the daemon found in the live registry that no
    /// longer have a discoverable manifest (binary deleted, search
    /// path removed, etc.). They are NOT auto-uninstalled — the
    /// operator must explicitly call `plugins/uninstall`. This list
    /// is purely informative so the UI can surface "stale" badges.
    pub stale: Vec<String>,
    /// Free-form per-plugin scan diagnostics — manifest validation
    /// failures, duplicate-id collisions, capability denials. Keyed
    /// by plugin id when known, else by manifest file path.
    pub warnings: Vec<String>,
}

// ── install ─────────────────────────────────────────────────────

/// Which delivery channel the daemon should use to fetch the
/// plugin binary. Defaults to [`InstallSource::Release`] — fastest
/// path for SaaS clients without a Rust toolchain.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// **Primary path.** Daemon downloads the prebuilt binary
    /// tarball from the plugin's GitHub release artefact matching
    /// the daemon's own target triple (compile-time `TARGET` env).
    /// Zero dependencies on the client — no rustc, no cargo. URL
    /// follows the cargo-dist convention:
    /// `https://github.com/<repo>/releases/download/v<ver>/<crate>-<triple>.tar.gz`
    /// (`.zip` on Windows). Daemon validates the binary via
    /// `--print-manifest` before committing it to disk.
    Release,
    /// **Developer / sysadmin fallback.** Daemon shells out to
    /// `cargo install <crate>` (compiles from source). Requires
    /// rustc + cargo on `$PATH`. Slower (minutes) but works
    /// without a published release tarball — handy for forks or
    /// pre-release versions still on crates.io.
    Cargo,
}

impl Default for InstallSource {
    fn default() -> Self {
        Self::Release
    }
}

/// Params for `nexo/admin/plugins/install`.
///
/// Two delivery paths governed by [`InstallSource`]:
///   - `Release` (default): download prebuilt binary from GitHub
///     releases following cargo-dist URL convention.
///   - `Cargo`: shell out to `cargo install <crate>` (compiles
///     locally; requires rustc).
///
/// Sealed against arbitrary command injection — the `crate_name` is
/// validated as a crates.io identifier (`[a-z0-9_-]+`), `version`
/// against a semver-ish pattern, and `repo` against a `<org>/<name>`
/// GitHub-slug pattern before any spawn or HTTP request.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsInstallParams {
    /// crates.io package name (e.g. `nexo-plugin-telegram`).
    /// Rejected if non-empty chars outside `[a-z0-9_-]`.
    pub crate_name: String,
    /// Optional pinned version. `None` resolves to the latest
    /// published release. Rejected if non-empty chars outside
    /// `[0-9A-Za-z.+-]`. REQUIRED for `source = Release` (we don't
    /// query GitHub's "latest release" endpoint to keep the daemon
    /// hermetic + offline-friendly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// GitHub repo slug (`org/name`) hosting the release artefacts.
    /// REQUIRED for `source = Release`. Ignored for `Cargo`.
    /// Defaults to `lordmacu/<crate_name>` if not supplied — the
    /// convention for first-party Nexo plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Delivery channel. See [`InstallSource`] docs.
    #[serde(default)]
    pub source: InstallSource,
    /// `force = true` passes `--force` to `cargo install` (Cargo
    /// path) OR overwrites the existing binary (Release path).
    /// Default `false` skips when the version is already current.
    #[serde(default)]
    pub force: bool,
    /// Phase 98.4 — parity with `nexo plugin install --require-signature`.
    /// When `true` the silent installer escalates `[[authors]]` policy
    /// to `Require` regardless of the TOML setting; an unsigned
    /// release rejects with `PolicyRequiresSig`. Mutually exclusive
    /// with `skip_signature_verify` — the admin handler rejects the
    /// combination with `InvalidParams`. Ignored for
    /// [`InstallSource::Cargo`] (cargo path doesn't expose the
    /// release tarball signature surface; response will carry
    /// `trust_enforcement = "cargo_skipped"`).
    #[serde(default)]
    pub require_signature: bool,
    /// Phase 98.4 — parity with `nexo plugin install --skip-signature-verify`.
    /// Downgrades policy to `Ignore` regardless of TOML for this
    /// install. Use case: SaaS operator needs to install an unsigned
    /// pre-release plugin from a known dev. Mutually exclusive with
    /// `require_signature` — admin handler rejects the combo.
    #[serde(default)]
    pub skip_signature_verify: bool,
}

/// Response for `nexo/admin/plugins/install`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsInstallResponse {
    /// Echo of the crate the operator asked for.
    pub crate_name: String,
    /// Version actually installed (parsed from `cargo install`
    /// stdout, or echoed from the `version` param when supplied).
    /// `None` when the install succeeded but the version couldn't
    /// be parsed back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Plugin ids hot-spawned in the same call (the rescan that
    /// runs after `cargo install` returns). Usually one entry, but
    /// can be empty if the operator already had the binary on disk
    /// and just wanted the daemon to scan, or multiple if a single
    /// crate ships several `nexo-plugin-*` binaries.
    pub spawned: Vec<String>,
    /// Trimmed `cargo install` stdout for operator forensics. Long
    /// outputs are clipped to ~16 KiB so the JSON reply stays sane.
    pub cargo_stdout: String,
    /// Trimmed `cargo install` stderr — Cargo prints progress here.
    pub cargo_stderr: String,
    /// Phase 98.4 — discriminator for how trust policy was applied:
    ///   - `"policy_applied"` — Release path; `trusted_keys.toml`
    ///     policy resolved + enforced normally.
    ///   - `"user_require"` — Release path; operator forced
    ///     `require_signature = true` in params.
    ///   - `"user_skip"` — Release path; operator forced
    ///     `skip_signature_verify = true` in params.
    ///   - `"cargo_skipped"` — Cargo source; signature surface
    ///     unavailable so trust enforcement deferred.
    #[serde(default)]
    pub trust_enforcement: String,
    /// Phase 98.4 — Gap 3.1 closed. `true` when cosign verified the
    /// release tarball; `false` for Cargo path or unsigned Release.
    #[serde(default)]
    pub signature_verified: bool,
    /// Phase 98.4 — SAN extracted from cosign cert (e.g. GitHub
    /// Actions workflow URL). `None` when verification skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_identity: Option<String>,
    /// Phase 98.4 — OIDC issuer the cert was minted by. `None` when
    /// verification skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_issuer: Option<String>,
    /// Phase 98.4 — effective trust mode that ran for this install
    /// (`"ignore" | "warn" | "require"`). Empty string for Cargo
    /// source.
    #[serde(default)]
    pub trust_mode: String,
    /// Phase 98.4 — `[[authors]]` entry that matched the release
    /// owner (if any). `None` means the owner had no policy entry
    /// and the global default was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_policy_matched: Option<String>,
}

// ── uninstall ───────────────────────────────────────────────────

/// Params for `nexo/admin/plugins/uninstall`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsUninstallParams {
    /// Plugin id from the manifest's `[plugin] id`. The daemon
    /// stops the subprocess, drops the handle from the live
    /// registry, and (when `cargo_uninstall = true`) shells out to
    /// `cargo uninstall <crate_name>` so the binary leaves disk.
    pub plugin_id: String,
    /// When `true`, also runs `cargo uninstall <crate_name>` where
    /// `crate_name` is derived from the plugin's manifest
    /// `[plugin.cargo].crate_name` field, or the operator's
    /// override. `false` (default) keeps the binary on disk so the
    /// operator can re-enable later without a fresh download.
    #[serde(default)]
    pub cargo_uninstall: bool,
    /// Optional explicit crate name when the manifest doesn't
    /// declare `[plugin.cargo].crate_name`. Ignored when
    /// `cargo_uninstall = false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_name: Option<String>,
}

/// Response for `nexo/admin/plugins/uninstall`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsUninstallResponse {
    /// Echo of the plugin id.
    pub plugin_id: String,
    /// `true` when the plugin was stopped and dropped from the
    /// registry. `false` when no such plugin existed (idempotent).
    pub removed: bool,
    /// `true` when `cargo uninstall` also ran successfully. `false`
    /// when not requested OR when cargo uninstall failed (the
    /// in-process removal still succeeded; the operator gets a
    /// warning in `cargo_stderr`).
    pub cargo_uninstalled: bool,
    /// Trimmed cargo stdout when `cargo_uninstall = true`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cargo_stdout: String,
    /// Trimmed cargo stderr when `cargo_uninstall = true`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cargo_stderr: String,
}
