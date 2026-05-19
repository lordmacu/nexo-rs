//! Merge per-source contributions into a single catalogue.
//!
//! Pure logic — no I/O, no HTTP. Takes the union of items returned
//! by each source, dedups by `name` (canonical crate name),
//! aggregates the `sources: Vec<PluginSource>` field on each entry,
//! and promotes the trust tier based on:
//!   - Owner ∈ `official_owners` allowlist → `Official` (wins).
//!   - Otherwise listed in the curated index → `CommunityIndexed`.
//!   - Otherwise → `Unverified` (already the source-layer default).
//!
//! Manifest-derived `category` + `compat` overrides happen in the
//! client orchestrator (`crate::client`, ships 98.9) AFTER the
//! merge — this module trusts whatever the caller passes in.

use std::collections::{BTreeMap, HashSet};

use crate::types::{DiscoveredPlugin, PluginSource, TrustTier};

/// Merge contributions from N sources.
///
/// Inputs:
///   - `items`: flat list of all source contributions (order
///     doesn't matter — entries with the same `name` collapse).
///   - `official_owners`: lowercase-compared allowlist that
///     promotes a card to `TrustTier::Official`.
///   - `indexed_names`: set of crate names present in the curated
///     index. Promotes non-official entries to `CommunityIndexed`.
///
/// Output ordering: BTree → alphabetical by `name` so the UI
/// shows a stable list across refreshes.
pub fn merge(
    items: Vec<DiscoveredPlugin>,
    official_owners: &[String],
    indexed_names: &HashSet<String>,
) -> Vec<DiscoveredPlugin> {
    let official_set: HashSet<String> = official_owners
        .iter()
        .map(|o| o.to_ascii_lowercase())
        .collect();

    let mut by_name: BTreeMap<String, DiscoveredPlugin> = BTreeMap::new();
    for item in items.into_iter() {
        match by_name.get_mut(&item.name) {
            Some(existing) => {
                // Aggregate the source list — dedup variant-wise so
                // double-listing the same source (defensive) doesn't
                // pollute the badges.
                for src in item.sources.iter() {
                    if !existing.sources.iter().any(|s| same_source_kind(s, src)) {
                        existing.sources.push(src.clone());
                    }
                }
                // Fill in metadata that the existing entry didn't
                // have yet — first non-None wins (preserves the
                // earlier source's stance).
                if existing.description.is_none() && item.description.is_some() {
                    existing.description = item.description;
                }
                if existing.homepage.is_none() && item.homepage.is_some() {
                    existing.homepage = item.homepage;
                }
                if existing.repo_url.is_none() && item.repo_url.is_some() {
                    existing.repo_url = item.repo_url;
                }
                if existing.manifest_url.is_none() && item.manifest_url.is_some() {
                    existing.manifest_url = item.manifest_url;
                }
                if existing.version.is_none() && item.version.is_some() {
                    existing.version = item.version.clone();
                    // Keep install_cmd / install_params in sync if
                    // we just picked up a version.
                    if let Some(v) = item.version.as_deref() {
                        existing.install_params.version = Some(v.to_string());
                        existing.install_cmd =
                            format!("cargo install {} --version {v}", existing.name);
                    }
                }
                // Tag merge: union of tags, stable order.
                for tag in item.tags.into_iter() {
                    if !existing.tags.iter().any(|t| t == &tag) {
                        existing.tags.push(tag);
                    }
                }
            }
            None => {
                by_name.insert(item.name.clone(), item);
            }
        }
    }

    // Trust promotion sweep. Owner-allowlist wins; curated index
    // membership promotes the rest from Unverified.
    for plugin in by_name.values_mut() {
        let owner_lower = plugin.owner.to_ascii_lowercase();
        if official_set.contains(&owner_lower) {
            plugin.trust_tier = TrustTier::Official;
        } else if indexed_names.contains(&plugin.name) {
            // Only promote if the source layer left it at
            // Unverified — a CuratedIndex contribution that
            // already set CommunityIndexed survives.
            if matches!(plugin.trust_tier, TrustTier::Unverified) {
                plugin.trust_tier = TrustTier::CommunityIndexed;
            }
        }
    }

    by_name.into_values().collect()
}

/// Two `PluginSource` values count as the same "kind" for dedup
/// purposes when their tag matches; the `GithubTopic` payload's
/// `repo` field is informational and shouldn't fork a new badge.
fn same_source_kind(a: &PluginSource, b: &PluginSource) -> bool {
    matches!(
        (a, b),
        (PluginSource::CratesIo, PluginSource::CratesIo)
            | (PluginSource::CuratedIndex, PluginSource::CuratedIndex)
            | (
                PluginSource::GithubTopic { .. },
                PluginSource::GithubTopic { .. }
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CompatStatus, PluginCategory};
    use nexo_tool_meta::admin::plugin_install::{InstallSource, PluginsInstallParams};

    fn stub(name: &str, owner: &str, source: PluginSource, trust: TrustTier) -> DiscoveredPlugin {
        DiscoveredPlugin {
            name: name.into(),
            version: Some("0.1.0".into()),
            description: None,
            owner: owner.into(),
            sources: vec![source],
            repo_url: None,
            homepage: None,
            tags: vec![],
            category: PluginCategory::Unknown,
            trust_tier: trust,
            compat: CompatStatus::Unknown,
            manifest_url: None,
            install_cmd: format!("cargo install {name} --version 0.1.0"),
            install_params: PluginsInstallParams {
                crate_name: name.into(),
                version: Some("0.1.0".into()),
                repo: None,
                source: InstallSource::Release,
                force: false,
                require_signature: false,
                skip_signature_verify: false,
            },
        }
    }

    fn owners() -> Vec<String> {
        vec!["lordmacu".to_string(), "nexo-rs".to_string()]
    }

    fn no_index() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn same_crate_two_sources_dedup_with_combined_badges() {
        let a = stub(
            "nexo-plugin-foo",
            "lordmacu",
            PluginSource::CratesIo,
            TrustTier::Unverified,
        );
        let b = stub(
            "nexo-plugin-foo",
            "lordmacu",
            PluginSource::GithubTopic {
                repo: "lordmacu/nexo-rs-plugin-foo".into(),
            },
            TrustTier::Unverified,
        );
        let merged = merge(vec![a, b], &owners(), &no_index());
        assert_eq!(merged.len(), 1);
        let entry = &merged[0];
        // Both source kinds present in the merged badges.
        assert!(entry
            .sources
            .iter()
            .any(|s| matches!(s, PluginSource::CratesIo)));
        assert!(entry
            .sources
            .iter()
            .any(|s| matches!(s, PluginSource::GithubTopic { .. })));
    }

    #[test]
    fn owner_in_allowlist_promotes_to_official() {
        let a = stub(
            "nexo-plugin-foo",
            "lordmacu",
            PluginSource::CratesIo,
            TrustTier::Unverified,
        );
        let merged = merge(vec![a], &owners(), &no_index());
        assert_eq!(merged[0].trust_tier, TrustTier::Official);
    }

    #[test]
    fn curated_index_entry_promotes_to_community_indexed() {
        // Owner NOT in allowlist; name IS in curated index → bump
        // from Unverified to CommunityIndexed.
        let a = stub(
            "nexo-plugin-foo",
            "communitydev",
            PluginSource::CratesIo,
            TrustTier::Unverified,
        );
        let mut indexed = HashSet::new();
        indexed.insert("nexo-plugin-foo".to_string());
        let merged = merge(vec![a], &owners(), &indexed);
        assert_eq!(merged[0].trust_tier, TrustTier::CommunityIndexed);
    }

    #[test]
    fn curated_only_owner_unknown_stays_community_indexed() {
        // CuratedIndexSource already sets CommunityIndexed at the
        // source layer; merge must NOT downgrade.
        let a = stub(
            "nexo-plugin-foo",
            "communitydev",
            PluginSource::CuratedIndex,
            TrustTier::CommunityIndexed,
        );
        let merged = merge(vec![a], &owners(), &no_index());
        assert_eq!(merged[0].trust_tier, TrustTier::CommunityIndexed);
    }

    #[test]
    fn allowlist_wins_over_curated_index() {
        // Same plugin in both: owner in allowlist + name in index.
        // Result must be Official (allowlist trumps index).
        let a = stub(
            "nexo-plugin-foo",
            "lordmacu",
            PluginSource::CuratedIndex,
            TrustTier::CommunityIndexed,
        );
        let mut indexed = HashSet::new();
        indexed.insert("nexo-plugin-foo".to_string());
        let merged = merge(vec![a], &owners(), &indexed);
        assert_eq!(merged[0].trust_tier, TrustTier::Official);
    }

    #[test]
    fn unknown_crate_no_allowlist_stays_unverified() {
        let a = stub(
            "stranger-plugin",
            "stranger",
            PluginSource::CratesIo,
            TrustTier::Unverified,
        );
        let merged = merge(vec![a], &owners(), &no_index());
        assert_eq!(merged[0].trust_tier, TrustTier::Unverified);
    }

    #[test]
    fn merge_fills_missing_version_from_second_source() {
        // GithubTopic source emits `version: None`; CratesIo source
        // emits the published version. After merge the resulting
        // card carries the version (UI install button enables).
        let github = DiscoveredPlugin {
            version: None,
            ..stub(
                "nexo-plugin-foo",
                "lordmacu",
                PluginSource::GithubTopic {
                    repo: "lordmacu/nexo-rs-plugin-foo".into(),
                },
                TrustTier::Unverified,
            )
        };
        let crates_io = stub(
            "nexo-plugin-foo",
            "lordmacu",
            PluginSource::CratesIo,
            TrustTier::Unverified,
        );
        let merged = merge(vec![github, crates_io], &owners(), &no_index());
        assert_eq!(merged[0].version.as_deref(), Some("0.1.0"));
        // install_cmd must reflect the now-known version.
        assert!(merged[0].install_cmd.contains("0.1.0"));
        assert_eq!(merged[0].install_params.version.as_deref(), Some("0.1.0"));
    }
}
