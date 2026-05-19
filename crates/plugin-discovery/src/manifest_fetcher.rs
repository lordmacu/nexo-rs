//! Fetch + parse a plugin's `nexo-plugin.toml` from a raw GitHub
//! URL (or other HTTP-accessible location). Derives the
//! catalogue's `PluginCategory` from the present manifest
//! sections per the Phase 81.33.b.real auto-discovery design.

use std::time::Duration;

use semver::VersionReq;
use tracing::warn;

use nexo_plugin_manifest::PluginManifest;

use crate::types::{ManifestSummary, PluginCategory};

/// Best-effort manifest fetch + parse. Tries `primary` first; on
/// 404 falls through to `fallbacks` in order. Returns `None` when
/// every URL fails (network error or non-2xx).
///
/// The discovery layer treats manifest unavailability as
/// `CompatStatus::Unknown` + `PluginCategory::Unknown` — install
/// is allowed but the UI surfaces a "manifest unavailable" tooltip.
pub async fn fetch_manifest(
    http: &reqwest::Client,
    primary: &str,
    fallbacks: &[String],
    timeout: Duration,
) -> Option<FetchedManifest> {
    let candidates: Vec<&str> = std::iter::once(primary)
        .chain(fallbacks.iter().map(String::as_str))
        .collect();
    for url in candidates.into_iter() {
        let req = http.get(url).timeout(timeout).send();
        let body = match req.await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    warn!(
                        target: "plugin_discovery::manifest_fetcher",
                        url = %url,
                        status = %resp.status(),
                        "manifest fetch non-2xx; trying next fallback"
                    );
                    continue;
                }
                match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!(
                            target: "plugin_discovery::manifest_fetcher",
                            url = %url,
                            error = %e,
                            "manifest body read failed; trying next fallback"
                        );
                        continue;
                    }
                }
            }
            Err(e) => {
                warn!(
                    target: "plugin_discovery::manifest_fetcher",
                    url = %url,
                    error = %e,
                    "manifest fetch transport failure; trying next fallback"
                );
                continue;
            }
        };
        match PluginManifest::from_str(&body) {
            Ok(manifest) => return Some(parse_into_fetched(url.to_string(), manifest)),
            Err(e) => {
                warn!(
                    target: "plugin_discovery::manifest_fetcher",
                    url = %url,
                    error = %e,
                    "manifest parse failed; trying next fallback"
                );
                continue;
            }
        }
    }
    None
}

/// Outcome of a successful manifest fetch — what the merge stage
/// needs to fill the catalogue card's compat + category fields.
#[derive(Debug, Clone)]
pub struct FetchedManifest {
    /// URL the body actually came from (after fallback walk).
    pub source_url: String,
    /// Manifest's declared SDK / daemon-version requirement
    /// (`[plugin] min_nexo_version`). The compat module uses this
    /// to derive `CompatStatus`.
    pub min_nexo_version: VersionReq,
    /// Derived category from the manifest sections.
    pub category: PluginCategory,
    /// Subset of manifest metadata surfaced via
    /// `nexo/admin/plugins/compat_check`.
    pub summary: ManifestSummary,
}

fn parse_into_fetched(source_url: String, manifest: PluginManifest) -> FetchedManifest {
    let category = derive_category(&manifest);
    let summary = ManifestSummary {
        plugin_id: manifest.plugin.id.clone(),
        plugin_version: manifest.plugin.version.to_string(),
        manifest_version: manifest.manifest_version,
        sdk_requires: Some(manifest.plugin.min_nexo_version.to_string()),
        category,
    };
    FetchedManifest {
        source_url,
        min_nexo_version: manifest.plugin.min_nexo_version.clone(),
        category,
        summary,
    }
}

/// Map manifest sections → catalogue category. Pure function so
/// the merge stage + tests can call it without touching HTTP.
///
/// Priority order (first match wins):
///   1. `[plugin.poller]` present → `Poller`.
///   2. `[plugin.pairing]` or `[plugin.dashboard]` present → `Channel`.
///   3. `[plugin.http]` present (no channel signal) → `Webhook`.
///   4. otherwise → `Tool`.
///
/// `Persona` is reserved for `crates/persona-manifest` — discovery
/// doesn't claim that bucket here.
pub fn derive_category(manifest: &PluginManifest) -> PluginCategory {
    if manifest.plugin.poller.is_some() {
        return PluginCategory::Poller;
    }
    let has_pairing = !manifest.plugin.pairing.is_unset();
    let has_dashboard = manifest.plugin.dashboard.is_some();
    if has_pairing || has_dashboard {
        return PluginCategory::Channel;
    }
    if manifest.plugin.http.is_some() {
        return PluginCategory::Webhook;
    }
    PluginCategory::Tool
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_src: &str) -> PluginManifest {
        PluginManifest::from_str(toml_src).expect("manifest parses")
    }

    fn base(id: &str) -> String {
        format!(
            r#"manifest_version = 2

[plugin]
id = "{id}"
version = "0.1.0"
name = "{id} demo"
description = "fixture"
min_nexo_version = ">=0.1"
"#
        )
    }

    #[test]
    fn derives_tool_when_no_special_sections() {
        let m = parse(&base("tool_only"));
        assert_eq!(derive_category(&m), PluginCategory::Tool);
    }

    #[test]
    fn derives_poller_when_poller_section_present() {
        let mut s = base("poller_plug");
        s.push_str(
            r#"
[plugin.poller]
kinds = ["echo"]
broker_topic_prefix = "echo"
"#,
        );
        let m = parse(&s);
        assert_eq!(derive_category(&m), PluginCategory::Poller);
    }

    #[test]
    fn derives_channel_when_pairing_section_present() {
        // Pairing section with `kind = "qr"` — minimal shape that
        // survives `deny_unknown_fields` parsing.
        let mut s = base("chan_plug");
        s.push_str(
            r#"
[plugin.pairing]
kind = "qr"
"#,
        );
        let m = parse(&s);
        assert_eq!(derive_category(&m), PluginCategory::Channel);
    }

    #[test]
    fn derives_channel_when_dashboard_present() {
        let mut s = base("dash_plug");
        s.push_str(
            r#"
[plugin.dashboard.layout]
kind = "single"

[plugin.dashboard.auth_check]
kind = "file_presence"
path = "session.json"
"#,
        );
        let m = parse(&s);
        assert_eq!(derive_category(&m), PluginCategory::Channel);
    }

    #[test]
    fn derives_webhook_when_only_http() {
        let mut s = base("web_plug");
        s.push_str(
            r#"
[plugin.http]
mount_prefix = "/webhook"
"#,
        );
        let m = parse(&s);
        assert_eq!(derive_category(&m), PluginCategory::Webhook);
    }

    #[test]
    fn poller_wins_over_other_sections() {
        // Defensive: a plugin declaring poller + http simultaneously
        // (legal) maps to Poller — the operator-facing category
        // tracks the headline feature.
        let mut s = base("multi");
        s.push_str(
            r#"
[plugin.poller]
kinds = ["echo"]
broker_topic_prefix = "echo"

[plugin.http]
mount_prefix = "/foo"
"#,
        );
        let m = parse(&s);
        assert_eq!(derive_category(&m), PluginCategory::Poller);
    }
}
