//! `nexo/admin/plugins/{search,compat_check,refresh_index}` wire types.
//!
//! Phase 98.5 — additive layer on top of Phase 97.1's
//! `plugin_install` machinery. Operators get a public catalogue of
//! installable plugins fetched from three sources:
//!   1. crates.io (`q=nexo-plugin` + `q=nexo-poller`).
//!   2. GitHub public repos tagged `topic:nexo-plugin`.
//!   3. Curated index repo `lordmacu/nexo-plugin-index`.
//!
//! Each `DiscoveredPlugin` carries a pre-filled
//! [`crate::admin::plugin_install::PluginsInstallParams`] so a
//! "Install" button on the admin UI feeds the existing
//! `nexo/admin/plugins/install` endpoint directly — no wire
//! duplication, no parallel install flow.

use serde::{Deserialize, Serialize};

use crate::admin::plugin_install::PluginsInstallParams;

// ── DiscoveredPlugin ─────────────────────────────────────────────

/// Single catalogue entry. Merged from one or more sources; UI
/// renders this as a card.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredPlugin {
    /// Canonical crate name (`nexo-plugin-telegram`). Unique key
    /// across sources — merge-by-name dedups duplicates.
    pub name: String,
    /// Latest non-yanked semver. `None` when the source doesn't
    /// publish version info (rare; GitHub-topic-only entries
    /// without crates.io publication).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// One-line summary. Pulled from crates.io description /
    /// repo description / curated index entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// GitHub org or crates.io owner login. Drives the
    /// `trust_tier` derivation against `TrustedKeysConfig.authors`.
    pub owner: String,
    /// Where the daemon found this entry. Multiple sources merge
    /// into one entry with the union of sources here — operators
    /// see "available on crates.io + indexed" badges.
    pub sources: Vec<PluginSource>,
    /// Canonical repo URL (`https://github.com/org/name`). Used as
    /// the fallback `repo` slug when constructing `install_params`
    /// for the Release path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    /// Plugin homepage / docs URL. Optional; surfaced as a "Learn
    /// more" link on the UI card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Free-form tags from crates.io keywords + curated index
    /// metadata. Drives the search-by-tag filter.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Functional category derived from `nexo-plugin.toml` manifest
    /// sections at fetch time. Lets the UI bucket plugins by what
    /// they do (channel / poller / tool / webhook / persona).
    pub category: PluginCategory,
    /// Trust tier resolved against the daemon's `TrustedKeysConfig`
    /// authors allowlist + curated index membership. Shipped as a
    /// badge on the UI card; never enforced here — installation
    /// still routes through Phase 97.1's signature pipeline.
    pub trust_tier: TrustTier,
    /// Result of comparing the manifest's `[plugin.requires]
    /// nexo-sdk = ">=…"` range against the running daemon's
    /// `nexo_microapp_sdk::VERSION`. `Unknown` when no manifest
    /// was fetched (404 / parse error / source-only entry).
    pub compat: CompatStatus,
    /// Raw URL to the plugin's `nexo-plugin.toml` (or fallback
    /// paths). Empty when the source doesn't expose one; UI hides
    /// the "manifest" link when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
    /// Human-readable install command (`cargo install
    /// nexo-plugin-X --version 0.3.0`). Convenient for the
    /// copy-button on the UI card; the actual install is driven
    /// by `install_params` below.
    pub install_cmd: String,
    /// Pre-filled params for `nexo/admin/plugins/install`. A click
    /// on the UI "Install" button opens the existing
    /// [`crate::admin::plugin_install::PluginsInstallParams`]
    /// modal with these values populated — operator can tweak
    /// trust flags / source before submitting.
    pub install_params: PluginsInstallParams,
}

// ── PluginSource ─────────────────────────────────────────────────

/// Where a `DiscoveredPlugin` was found. A single plugin may
/// appear in multiple sources; the catalogue merges them so the
/// operator sees "available on crates.io + indexed" badges.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginSource {
    /// crates.io search hit. Version comes from
    /// `latest_non_yanked`; yanked-only crates filter out.
    CratesIo,
    /// GitHub public repo with `topic:nexo-plugin`.
    GithubTopic {
        /// `<org>/<name>` slug — used to build the manifest URL.
        repo: String,
    },
    /// Curated index entry from `lordmacu/nexo-plugin-index/index.json`.
    CuratedIndex,
}

// ── PluginCategory ───────────────────────────────────────────────

/// Functional bucket auto-derived from `nexo-plugin.toml` sections
/// at fetch time. Manifest sections are introspected per Phase
/// 81.33.b.real auto-discovery design — discovery never asks
/// authors to tag manually.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginCategory {
    /// `[plugin.pairing]` or `[plugin.dashboard]` present → user
    /// channel (telegram / whatsapp / email).
    Channel,
    /// `[plugin.poller]` present → scheduled tick worker (rss /
    /// gmail / google_calendar).
    Poller,
    /// `[plugin.http]` only → webhook receiver / outbound HTTP
    /// service.
    Webhook,
    /// `kind = "persona"` in `[plugin]` → persona overlay (cody /
    /// custom marketing copy).
    Persona,
    /// Default fallback. Tool plugins expose `[plugin.tools]` to
    /// the agent loop without channel / poller / webhook surfaces.
    Tool,
    /// Manifest wasn't fetched or parsed. UI hides the category
    /// chip + flags the entry as "Manifest unavailable".
    Unknown,
}

// ── TrustTier ────────────────────────────────────────────────────

/// Operator-facing trust signal derived from owner allowlist +
/// curated index membership. Never enforced here — install
/// continues to route through Phase 97.1 signature verification.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// Owner appears in `TrustedKeysConfig.authors` allowlist.
    /// "Maintained by the Nexo team" badge.
    Official,
    /// Owner not in allowlist but listed in
    /// `lordmacu/nexo-plugin-index`. "Community-verified" badge.
    CommunityIndexed,
    /// Neither allowlist nor curated index. "Unverified — review
    /// manually before installing" badge.
    Unverified,
}

// ── CompatStatus ─────────────────────────────────────────────────

/// SDK semver compat result. Drives the install button's enabled
/// state on the UI card.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompatStatus {
    /// Manifest's SDK range includes the running daemon's
    /// version. Install button enabled.
    Compatible,
    /// Manifest requires a newer SDK than the daemon ships.
    /// Install button shows tooltip "Upgrade daemon to ≥X.Y.Z".
    NeedsUpgrade {
        /// `semver::VersionReq` display of the required range.
        required: String,
        /// `semver::Version` display of the running daemon's SDK.
        current: String,
    },
    /// Manifest requires an older SDK / pre-1.0 break. Install
    /// button disabled with red badge.
    Incompatible {
        /// Human-readable explanation surfaced as the badge tooltip
        /// (e.g. "Plugin pins nexo-sdk = 0.0.x; daemon ships 0.1+").
        reason: String,
    },
    /// Manifest fetch failed or didn't declare a range. Install
    /// button allowed but with "Compat unknown — install at your
    /// own risk" warning.
    Unknown,
}

// ── ManifestSummary ──────────────────────────────────────────────

/// Subset of `nexo-plugin.toml` surfaced via
/// `nexo/admin/plugins/compat_check`. Lets the UI render a
/// pre-install dialog with manifest details without exposing the
/// full TOML.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestSummary {
    /// Manifest's `[plugin] id` field — canonical plugin identifier.
    pub plugin_id: String,
    /// Manifest's `[plugin] version` field — declared semver.
    pub plugin_version: String,
    /// `manifest_version` schema discriminator (currently `2`).
    pub manifest_version: u32,
    /// Raw `[plugin.requires] nexo-sdk` value (semver range string),
    /// `None` when not declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_requires: Option<String>,
    /// Functional category derived from which manifest sections are
    /// present (same logic that powers `DiscoveredPlugin.category`).
    pub category: PluginCategory,
}

// ── SourceError ──────────────────────────────────────────────────

/// Per-source failure surfaced to the UI's
/// `<PartialFailureBanner>`. `search` returns whatever items the
/// healthy sources produced + this list of failures, so a
/// rate-limited GitHub doesn't blank the page.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceError {
    /// Source name (`crates_io`, `github_topic`, `curated_index`).
    pub source: String,
    /// Human-readable failure reason. Already escaped for label
    /// rendering; ≤ 256 chars.
    pub message: String,
}

// ── search ───────────────────────────────────────────────────────

/// Params for `nexo/admin/plugins/search`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsSearchParams {
    /// Optional substring filter applied client-side post-merge
    /// against `name + description + tags`. `None` → return the
    /// full catalogue (paginated naturally by source caps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// When `true`, drop entries where `compat` is `NeedsUpgrade` /
    /// `Incompatible`. Keeps `Unknown` (manifest fetch failed —
    /// not necessarily incompatible).
    #[serde(default)]
    pub compat_only: bool,
    /// Optional category filter (`channel`, `poller`, `tool`,
    /// `webhook`, `persona`). `None` → all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional source filter (`crates_io`, `github_topic`,
    /// `curated_index`). `None` → all sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Response for `nexo/admin/plugins/search`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsSearchResponse {
    /// Merged + filtered catalogue. Empty when every source is
    /// down or rate-limited — `partial_failures` then carries the
    /// per-source errors.
    pub items: Vec<DiscoveredPlugin>,
    /// Unix milliseconds when the underlying catalogue was last
    /// refreshed (cache hit = older value; cold fetch = "now").
    /// Drives the "Updated <X> ago" footer on the UI.
    pub fetched_at_ms: u64,
    /// One entry per source that failed; healthy sources contribute
    /// to `items` regardless. Empty when every source succeeded.
    #[serde(default)]
    pub partial_failures: Vec<SourceError>,
}

// ── compat_check ─────────────────────────────────────────────────

/// Params for `nexo/admin/plugins/compat_check`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsCompatCheckParams {
    /// crates.io crate name (e.g. `nexo-plugin-telegram`).
    /// Validated against the same `[a-z0-9_-]` char class
    /// `plugin_install` enforces.
    pub crate_name: String,
    /// Optional pinned version. `None` → latest published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Response for `nexo/admin/plugins/compat_check`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsCompatCheckResponse {
    /// Compat result against the running daemon's SDK version.
    pub compat: CompatStatus,
    /// Parsed manifest summary when fetch succeeded. `None` when
    /// the manifest URL returned 404 / parse error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_summary: Option<ManifestSummary>,
}

// ── refresh_index ────────────────────────────────────────────────

/// Params for `nexo/admin/plugins/refresh_index`. Empty — the
/// daemon invalidates its disk cache and re-fetches every source.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsRefreshIndexParams {}

/// Response for `nexo/admin/plugins/refresh_index`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsRefreshIndexResponse {
    /// Number of unique `DiscoveredPlugin` entries after merge.
    pub items_count: usize,
    /// Source names that succeeded.
    pub sources_ok: Vec<String>,
    /// Source names that failed + reasons.
    pub sources_err: Vec<SourceError>,
}

// ── tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::plugin_install::InstallSource;

    fn sample_install_params() -> PluginsInstallParams {
        PluginsInstallParams {
            crate_name: "nexo-plugin-telegram".into(),
            version: Some("0.3.0".into()),
            repo: Some("lordmacu/nexo-rs-plugin-telegram".into()),
            source: InstallSource::Release,
            force: false,
            require_signature: false,
            skip_signature_verify: false,
        }
    }

    #[test]
    fn discovered_plugin_roundtrip_json() {
        let plugin = DiscoveredPlugin {
            name: "nexo-plugin-telegram".into(),
            version: Some("0.3.0".into()),
            description: Some("Telegram bot channel".into()),
            owner: "lordmacu".into(),
            sources: vec![
                PluginSource::CratesIo,
                PluginSource::GithubTopic {
                    repo: "lordmacu/nexo-rs-plugin-telegram".into(),
                },
                PluginSource::CuratedIndex,
            ],
            repo_url: Some("https://github.com/lordmacu/nexo-rs-plugin-telegram".into()),
            homepage: None,
            tags: vec!["messaging".into(), "telegram".into()],
            category: PluginCategory::Channel,
            trust_tier: TrustTier::Official,
            compat: CompatStatus::Compatible,
            manifest_url: Some(
                "https://raw.githubusercontent.com/lordmacu/nexo-rs-plugin-telegram/main/nexo-plugin.toml"
                    .into(),
            ),
            install_cmd: "cargo install nexo-plugin-telegram --version 0.3.0".into(),
            install_params: sample_install_params(),
        };

        let json = serde_json::to_string(&plugin).unwrap();
        let parsed: DiscoveredPlugin = serde_json::from_str(&json).unwrap();
        assert_eq!(plugin, parsed);
    }

    #[test]
    fn compat_status_needs_upgrade_serializes_with_kind_tag() {
        let compat = CompatStatus::NeedsUpgrade {
            required: ">=0.2".into(),
            current: "0.1.19".into(),
        };
        let json = serde_json::to_value(&compat).unwrap();
        assert_eq!(json["kind"], "needs_upgrade");
        assert_eq!(json["required"], ">=0.2");
        assert_eq!(json["current"], "0.1.19");
    }

    #[test]
    fn search_params_default_is_unfiltered() {
        let p = PluginsSearchParams::default();
        assert!(p.query.is_none());
        assert!(!p.compat_only);
        assert!(p.category.is_none());
        assert!(p.source.is_none());
    }

    #[test]
    fn search_response_default_is_empty_catalogue() {
        let r = PluginsSearchResponse::default();
        assert!(r.items.is_empty());
        assert!(r.partial_failures.is_empty());
        assert_eq!(r.fetched_at_ms, 0);
    }

    #[test]
    fn search_params_accepts_partial_json() {
        // All fields optional — empty object must parse.
        let p: PluginsSearchParams = serde_json::from_str("{}").unwrap();
        assert!(p.query.is_none());
        // Only `query` populated.
        let p: PluginsSearchParams = serde_json::from_str(r#"{"query":"telegram"}"#).unwrap();
        assert_eq!(p.query.as_deref(), Some("telegram"));
    }

    #[test]
    fn compat_check_response_omits_manifest_when_none() {
        let r = PluginsCompatCheckResponse {
            compat: CompatStatus::Unknown,
            manifest_summary: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("manifest_summary"),
            "manifest_summary must be skipped when None: {json}"
        );
    }

    #[test]
    fn plugin_source_github_topic_roundtrip() {
        let s = PluginSource::GithubTopic {
            repo: "lordmacu/nexo-rs-plugin-foo".into(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["kind"], "github_topic");
        assert_eq!(json["repo"], "lordmacu/nexo-rs-plugin-foo");
        let parsed: PluginSource = serde_json::from_value(json).unwrap();
        assert_eq!(s, parsed);
    }
}
