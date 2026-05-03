//! Phase 31.1 (Option B — decentralized GitHub Releases) —
//! building block for `nexo plugin install <owner>/<repo>@<tag>`.
//!
//! Architecture: there is NO central catalog. Each plugin
//! author publishes their plugin as a GitHub Release on their
//! own repo, following the asset naming convention documented
//! in `nexo-plugin-contract.md`. The install CLI hits the
//! GitHub Releases API directly to resolve a coords string into
//! a verified tarball.
//!
//! What this crate does:
//! 1. Parse `<owner>/<repo>@<tag>` coords
//! 2. Fetch the GitHub release JSON (or `/releases/latest`)
//! 3. Parse the release into an [`nexo_ext_registry::ExtEntry`]
//!    using the asset naming convention
//! 4. Download the tarball matching the daemon's target
//! 5. Stream-verify the sha256 (read from the `.sha256` asset)
//!
//! What it does NOT do (deferred):
//! - **Cosign verification** — Phase 31.3.
//! - **Tarball extraction** — Phase 31.1.b.
//! - **CLI integration** — Phase 31.1.c.
//!
//! # IRROMPIBLE refs
//!
//! - Internal: `crates/ext-registry/` (Phase 31.0) — entry types.
//! - GitHub Releases API:
//!   `https://docs.github.com/en/rest/releases/releases#get-a-release-by-tag-name`
//! - Real-world: `cargo binstall` + `gh extension install` —
//!   per-repo binary install via GitHub Releases.

#![deny(missing_docs)]

use std::path::{Path, PathBuf};

use futures::StreamExt;
use nexo_ext_registry::ExtEntry;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

pub mod error;
pub mod extract;
pub mod extract_error;
pub mod trusted_keys;
pub mod verify;
pub mod verify_error;

pub use error::InstallError;
pub use extract::{
    extract_verified_tarball, ExtractInput, ExtractLimits, ExtractedPlugin, MAX_ENTRIES,
    MAX_ENTRY_BYTES, MAX_EXTRACTED_BYTES, MAX_TARBALL_BYTES,
};
pub use extract_error::ExtractError;
pub use trusted_keys::{AuthorPolicy, TrustMode, TrustedKeysConfig};
pub use verify::{discover_cosign_binary, verify_plugin_signature, VerifiedSignature, VerifyInput};
pub use verify_error::VerifyError;

/// Parsed `<owner>/<repo>@<tag>` coordinates. `tag` defaults to
/// `latest` when the user omits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCoords {
    /// GitHub repo owner (user or org).
    pub owner: String,
    /// GitHub repo name.
    pub repo: String,
    /// Release tag (e.g. `v0.2.0`) or the literal `"latest"`
    /// to resolve to GitHub's `/releases/latest` endpoint.
    pub tag: String,
}

impl PluginCoords {
    /// Parse `<owner>/<repo>` or `<owner>/<repo>@<tag>`. Tag
    /// defaults to `"latest"` when `@<tag>` is absent.
    pub fn parse(s: &str) -> Result<Self, InstallError> {
        let (coords, tag) = match s.split_once('@') {
            Some((c, t)) => (c, t.to_string()),
            None => (s, "latest".to_string()),
        };
        let (owner, repo) =
            coords.split_once('/').ok_or_else(|| InstallError::CoordsInvalid {
                got: s.to_string(),
                reason: "expected <owner>/<repo>[@<tag>]",
            })?;
        if owner.is_empty() || repo.is_empty() || tag.is_empty() {
            return Err(InstallError::CoordsInvalid {
                got: s.to_string(),
                reason: "owner / repo / tag must not be empty",
            });
        }
        // GitHub allows alphanumerics + `-` + `_` + `.` in
        // owner/repo names. We don't replicate the full GitHub
        // validator; reject the obviously bad chars (whitespace,
        // url-meaningful chars) so a typo fails loud.
        for ch in owner.chars().chain(repo.chars()) {
            if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.') {
                return Err(InstallError::CoordsInvalid {
                    got: s.to_string(),
                    reason: "owner/repo may only contain [A-Za-z0-9._-]",
                });
            }
        }
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            tag,
        })
    }

    /// GitHub Releases API URL for this coords. When tag is
    /// `"latest"`, hits `/releases/latest`; otherwise hits
    /// `/releases/tags/<tag>`. The base URL is configurable for
    /// tests (default `https://api.github.com`).
    pub fn release_api_url(&self, api_base: &str) -> String {
        if self.tag == "latest" {
            format!(
                "{}/repos/{}/{}/releases/latest",
                api_base.trim_end_matches('/'),
                self.owner,
                self.repo
            )
        } else {
            format!(
                "{}/repos/{}/{}/releases/tags/{}",
                api_base.trim_end_matches('/'),
                self.owner,
                self.repo,
                self.tag
            )
        }
    }
}

/// Default GitHub API base URL. Override via the
/// `NEXO_GITHUB_API_BASE` env or in tests with a wiremock URL.
pub const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";

/// One asset entry from the GitHub Releases API response.
/// Internal-only; the parser maps these into `ExtDownload` /
/// `ExtSigning`.
#[derive(Debug, Clone, serde::Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Top-level shape of a GitHub Releases API response. Only the
/// fields we consume.
#[derive(Debug, Clone, serde::Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// Successful resolution of a release into an installable
/// entry. Carries enough info to call [`download_and_verify`].
#[derive(Debug, Clone)]
pub struct ResolvedInstall {
    /// Plugin entry built from the GitHub release.
    pub entry: ExtEntry,
    /// Index of the matching download in `entry.downloads`.
    pub download_index: usize,
    /// URL of the per-tarball `.sha256` asset (single line of
    /// hex). Used by `download_and_verify` to obtain the
    /// expected digest at install time.
    pub sha256_url: String,
}

/// Successful install — verified tarball on disk.
#[derive(Debug, Clone)]
pub struct InstalledTarball {
    /// On-disk path of the tarball.
    pub tarball_path: PathBuf,
    /// The plugin entry that was installed.
    pub entry: ExtEntry,
    /// Bytes downloaded.
    pub size_bytes: u64,
}

/// Fetch a release JSON from GitHub. Wraps the raw JSON in our
/// `ReleaseResponse` for the resolver to consume.
async fn fetch_release_raw(
    client: &reqwest::Client,
    coords: &PluginCoords,
    api_base: &str,
) -> Result<ReleaseResponse, InstallError> {
    let url = coords.release_api_url(api_base);
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nexo-ext-installer")
        .send()
        .await
        .map_err(|e| InstallError::Http(format!("fetch release: {e}")))?;
    if !response.status().is_success() {
        return Err(InstallError::Http(format!(
            "fetch release: HTTP {} for {}",
            response.status(),
            url
        )));
    }
    let json = response
        .json::<ReleaseResponse>()
        .await
        .map_err(|e| InstallError::Http(format!("decode release: {e}")))?;
    Ok(json)
}

/// Resolve a plugin's release into a downloadable entry.
///
/// Steps:
/// 1. Fetch the release JSON.
/// 2. Locate the `nexo-plugin.toml` asset.
/// 3. Download + parse the manifest to learn `plugin.id`.
/// 4. Find the tarball asset matching the requested `target`
///    using the naming convention
///    `<id>-<version>-<target>.tar.gz`.
/// 5. Find the matching `.sha256` asset.
/// 6. Build an `ExtEntry` with one download.
pub async fn resolve_release(
    client: &reqwest::Client,
    coords: &PluginCoords,
    target: &str,
    api_base: &str,
) -> Result<ResolvedInstall, InstallError> {
    let release = fetch_release_raw(client, coords, api_base).await?;
    let version_str = release.tag_name.trim_start_matches('v').to_string();
    let version = semver::Version::parse(&version_str).map_err(|e| {
        InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!(
                "release tag `{}` does not parse as semver `vX.Y.Z`: {e}",
                release.tag_name
            ),
        }
    })?;

    // Locate the manifest asset.
    let manifest_asset = release
        .assets
        .iter()
        .find(|a| a.name == "nexo-plugin.toml")
        .ok_or_else(|| InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!(
                "release `{}` is missing required asset `nexo-plugin.toml`",
                release.tag_name
            ),
        })?;

    // Fetch manifest, parse to extract plugin.id.
    let manifest_bytes = client
        .get(&manifest_asset.browser_download_url)
        .header("User-Agent", "nexo-ext-installer")
        .send()
        .await
        .map_err(|e| InstallError::Http(format!("fetch manifest: {e}")))?
        .bytes()
        .await
        .map_err(|e| InstallError::Http(format!("read manifest body: {e}")))?;
    let manifest: nexo_plugin_manifest::PluginManifest =
        toml::from_str(std::str::from_utf8(&manifest_bytes).map_err(|e| {
            InstallError::ReleaseShape {
                owner: coords.owner.clone(),
                repo: coords.repo.clone(),
                reason: format!("manifest is not valid UTF-8: {e}"),
            }
        })?)
        .map_err(|e| InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!("manifest parse failed: {e}"),
        })?;
    let plugin_id = manifest.plugin.id.clone();

    // Find the tarball asset for the requested target. Phase 31.4
    // adds `noarch` as a fallback target name so portable plugins
    // (Python, TypeScript) can publish a single asset that all
    // daemons accept.
    let per_target_name = format!("{plugin_id}-{version_str}-{target}.tar.gz");
    let noarch_name = format!("{plugin_id}-{version_str}-noarch.tar.gz");
    let (tarball_asset, tarball_name) = match release.assets.iter().find(|a| a.name == per_target_name) {
        Some(a) => (a, per_target_name),
        None => match release.assets.iter().find(|a| a.name == noarch_name) {
            Some(a) => (a, noarch_name),
            None => {
                let available: Vec<String> = release
                    .assets
                    .iter()
                    .filter(|a| a.name.ends_with(".tar.gz"))
                    .map(|a| a.name.clone())
                    .collect();
                return Err(InstallError::TargetNotFound {
                    id: plugin_id.clone(),
                    version: version.clone(),
                    target: target.to_string(),
                    available,
                });
            }
        },
    };

    // Find the matching .sha256 asset.
    let sha256_name = format!("{tarball_name}.sha256");
    let sha256_asset = release
        .assets
        .iter()
        .find(|a| a.name == sha256_name)
        .ok_or_else(|| InstallError::ReleaseShape {
            owner: coords.owner.clone(),
            repo: coords.repo.clone(),
            reason: format!(
                "release `{}` is missing required asset `{sha256_name}` for tarball `{tarball_name}`",
                release.tag_name
            ),
        })?;

    // Locate optional cosign material (Phase 31.3 enforces).
    let sig_name = format!("{tarball_name}.sig");
    let cert_name = format!("{tarball_name}.cert");
    let signing = match (
        release.assets.iter().find(|a| a.name == sig_name),
        release.assets.iter().find(|a| a.name == cert_name),
    ) {
        (Some(sig), Some(cert)) => Some(nexo_ext_registry::ExtSigning {
            cosign_signature_url: sig.browser_download_url.clone(),
            cosign_certificate_url: cert.browser_download_url.clone(),
        }),
        _ => None,
    };

    // Per Option B's decentralized model, every release defaults
    // to `tier = community`. Operator's trusted_keys.toml decides
    // which authors' cosign keys count as "verified" at install
    // time (Phase 31.3).
    let entry = ExtEntry {
        id: plugin_id,
        version,
        name: manifest.plugin.name.clone(),
        description: manifest.plugin.description.clone(),
        homepage: format!("https://github.com/{}/{}", coords.owner, coords.repo),
        tier: nexo_ext_registry::ExtTier::Community,
        min_nexo_version: manifest.plugin.min_nexo_version.clone(),
        downloads: vec![nexo_ext_registry::ExtDownload {
            target: target.to_string(),
            url: tarball_asset.browser_download_url.clone(),
            // Placeholder: actual sha256 hex is read from the
            // `.sha256` asset at download time. Putting the
            // GitHub asset URL here would be wrong (URLs aren't
            // hex). Use a well-known sentinel + the downloader
            // overrides with the fetched value before the
            // expected/got compare.
            sha256: "from_sha256_asset_at_download".to_string(),
            size_bytes: tarball_asset.size,
        }],
        manifest_url: manifest_asset.browser_download_url.clone(),
        signing,
        authors: Vec::new(),
    };

    Ok(ResolvedInstall {
        entry,
        download_index: 0,
        sha256_url: sha256_asset.browser_download_url.clone(),
    })
}

/// Download the resolved tarball, fetch the expected sha256
/// from its `.sha256` sibling, stream-verify the downloaded
/// bytes' digest matches. Aborts and removes the partial file
/// if the digest doesn't match.
pub async fn download_and_verify(
    client: &reqwest::Client,
    resolved: &ResolvedInstall,
    dest_path: &Path,
) -> Result<InstalledTarball, InstallError> {
    let download = &resolved.entry.downloads[resolved.download_index];
    if let Some(parent) = dest_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| InstallError::Io(format!("mkdir parent: {e}")))?;
        }
    }

    // Fetch expected sha256 from the .sha256 asset. Convention:
    // single line of lowercase hex (64 chars) with optional
    // trailing whitespace.
    let expected_sha = client
        .get(&resolved.sha256_url)
        .header("User-Agent", "nexo-ext-installer")
        .send()
        .await
        .map_err(|e| InstallError::Http(format!("fetch sha256: {e}")))?
        .text()
        .await
        .map_err(|e| InstallError::Http(format!("read sha256 body: {e}")))?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if expected_sha.len() != 64 || !expected_sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(InstallError::Sha256Invalid {
            id: resolved.entry.id.clone(),
            got: expected_sha,
        });
    }

    let response = client
        .get(&download.url)
        .header("User-Agent", "nexo-ext-installer")
        .send()
        .await
        .map_err(|e| InstallError::Http(format!("fetch tarball: {e}")))?;
    if !response.status().is_success() {
        return Err(InstallError::Http(format!(
            "fetch tarball: HTTP {}",
            response.status()
        )));
    }

    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut file = tokio::fs::File::create(dest_path)
        .await
        .map_err(|e| InstallError::Io(format!("create dest: {e}")))?;
    let mut stream = response.bytes_stream();
    while let Some(chunk_res) = stream.next().await {
        let chunk = match chunk_res {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(dest_path).await;
                return Err(InstallError::Http(format!("download chunk: {e}")));
            }
        };
        hasher.update(&chunk);
        size += chunk.len() as u64;
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(dest_path).await;
            return Err(InstallError::Io(format!("write tarball: {e}")));
        }
    }
    file.flush()
        .await
        .map_err(|e| InstallError::Io(format!("flush tarball: {e}")))?;
    drop(file);

    let computed = hex::encode(hasher.finalize());
    if computed != expected_sha {
        let _ = tokio::fs::remove_file(dest_path).await;
        return Err(InstallError::Sha256Mismatch {
            id: resolved.entry.id.clone(),
            expected: expected_sha,
            got: computed,
        });
    }
    Ok(InstalledTarball {
        tarball_path: dest_path.to_path_buf(),
        entry: resolved.entry.clone(),
        size_bytes: size,
    })
}

/// One-shot helper: parse coords, fetch release, resolve,
/// download, verify. Equivalent to chaining the lower-level
/// functions but matches typical CLI usage.
pub async fn install_plugin(
    client: &reqwest::Client,
    coords: &str,
    target: &str,
    dest_path: &Path,
    api_base: &str,
) -> Result<InstalledTarball, InstallError> {
    let coords = PluginCoords::parse(coords)?;
    let resolved = resolve_release(client, &coords, target, api_base).await?;
    download_and_verify(client, &resolved, dest_path).await
}

/// Detect the running daemon's target triple. Override via
/// `NEXO_INSTALL_TARGET` env.
pub fn current_target_triple() -> String {
    if let Ok(t) = std::env::var("NEXO_INSTALL_TARGET") {
        if !t.is_empty() {
            return t;
        }
    }
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        "x86_64-unknown-linux-gnu".to_string()
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux")) {
        "aarch64-unknown-linux-gnu".to_string()
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin".to_string()
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin".to_string()
    } else {
        "unknown-target".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parse_coords_default_tag_latest() {
        let c = PluginCoords::parse("alice/plugin-x").unwrap();
        assert_eq!(c.owner, "alice");
        assert_eq!(c.repo, "plugin-x");
        assert_eq!(c.tag, "latest");
    }

    #[test]
    fn parse_coords_with_tag() {
        let c = PluginCoords::parse("alice/plugin-x@v0.2.0").unwrap();
        assert_eq!(c.owner, "alice");
        assert_eq!(c.repo, "plugin-x");
        assert_eq!(c.tag, "v0.2.0");
    }

    #[test]
    fn parse_coords_rejects_bad_shapes() {
        assert!(PluginCoords::parse("no-slash").is_err());
        assert!(PluginCoords::parse("/empty-owner").is_err());
        assert!(PluginCoords::parse("alice/").is_err());
        assert!(PluginCoords::parse("alice/plugin@").is_err());
        assert!(PluginCoords::parse("alice/plugin space@v1").is_err());
    }

    #[test]
    fn release_api_url_branches_on_tag() {
        let c = PluginCoords::parse("alice/x@v0.2.0").unwrap();
        assert_eq!(
            c.release_api_url("https://api.github.com"),
            "https://api.github.com/repos/alice/x/releases/tags/v0.2.0"
        );
        let c2 = PluginCoords::parse("alice/x").unwrap();
        assert_eq!(
            c2.release_api_url("https://api.github.com"),
            "https://api.github.com/repos/alice/x/releases/latest"
        );
    }

    fn manifest_toml(id: &str, version: &str) -> String {
        format!(
            r#"[plugin]
id = "{id}"
version = "{version}"
name = "Slack Channel"
description = "Slack bot integration"
min_nexo_version = ">=0.0.1"

[plugin.requires]
nexo_capabilities = ["broker"]
"#
        )
    }

    /// Round-trip: fetch release, parse manifest from asset,
    /// resolve a target's tarball + sha256, download tarball,
    /// verify sha256 matches.
    #[tokio::test]
    async fn install_round_trip_with_real_sha() {
        let server = MockServer::start().await;

        let manifest_body = manifest_toml("slack", "0.2.0");
        let tarball_payload = b"fake plugin tarball bytes";
        let mut hasher = Sha256::new();
        hasher.update(tarball_payload);
        let tarball_sha = hex::encode(hasher.finalize());
        let sha_body = format!("{tarball_sha}\n");

        let manifest_url = format!("{}/manifest", server.uri());
        let tarball_url = format!("{}/tarball", server.uri());
        let sha_url = format!("{}/sha256", server.uri());

        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "nexo-plugin.toml",
                    "browser_download_url": manifest_url,
                    "size": manifest_body.len()
                },
                {
                    "name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": tarball_url,
                    "size": tarball_payload.len()
                },
                {
                    "name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256",
                    "browser_download_url": sha_url,
                    "size": sha_body.len()
                }
            ]
        });

        Mock::given(method("GET"))
            .and(path("/repos/alice/slack-plugin/releases/tags/v0.2.0"))
            .and(header("Accept", "application/vnd.github+json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/sha256"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sha_body))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/tarball"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(tarball_payload.as_slice()),
            )
            .mount(&server)
            .await;

        let coords = PluginCoords::parse("alice/slack-plugin@v0.2.0").unwrap();
        let client = reqwest::Client::new();
        let resolved =
            resolve_release(&client, &coords, "x86_64-unknown-linux-gnu", &server.uri())
                .await
                .expect("resolve");
        assert_eq!(resolved.entry.id, "slack");
        assert_eq!(resolved.entry.version.to_string(), "0.2.0");
        assert_eq!(
            resolved.entry.tier,
            nexo_ext_registry::ExtTier::Community
        );

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("slack-0.2.0.tar.gz");
        let installed = download_and_verify(&client, &resolved, &dest)
            .await
            .expect("download");
        assert_eq!(installed.tarball_path, dest);
        assert_eq!(installed.size_bytes as usize, tarball_payload.len());
        assert!(dest.exists());
    }

    #[tokio::test]
    async fn rejects_release_missing_manifest_asset() {
        let server = MockServer::start().await;
        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/tar",
                    "size": 100
                }
                // no nexo-plugin.toml asset
            ]
        });
        Mock::given(method("GET"))
            .and(path("/repos/alice/x/releases/tags/v0.2.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .mount(&server)
            .await;

        let coords = PluginCoords::parse("alice/x@v0.2.0").unwrap();
        let client = reqwest::Client::new();
        match resolve_release(&client, &coords, "x86_64-unknown-linux-gnu", &server.uri())
            .await
        {
            Err(InstallError::ReleaseShape { reason, .. }) => {
                assert!(reason.contains("nexo-plugin.toml"));
            }
            other => panic!("expected ReleaseShape error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_release_missing_target_tarball() {
        let server = MockServer::start().await;
        let manifest_body = manifest_toml("slack", "0.2.0");
        let manifest_url = format!("{}/manifest", server.uri());
        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "nexo-plugin.toml",
                    "browser_download_url": manifest_url,
                    "size": manifest_body.len()
                },
                {
                    "name": "slack-0.2.0-aarch64-apple-darwin.tar.gz",
                    "browser_download_url": "https://example.com/tar",
                    "size": 100
                }
                // no x86_64 linux tarball
            ]
        });
        Mock::given(method("GET"))
            .and(path("/repos/alice/x/releases/tags/v0.2.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
            .mount(&server)
            .await;

        let coords = PluginCoords::parse("alice/x@v0.2.0").unwrap();
        let client = reqwest::Client::new();
        match resolve_release(&client, &coords, "x86_64-unknown-linux-gnu", &server.uri())
            .await
        {
            Err(InstallError::TargetNotFound { available, .. }) => {
                assert_eq!(
                    available,
                    vec!["slack-0.2.0-aarch64-apple-darwin.tar.gz".to_string()]
                );
            }
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_release_falls_back_to_noarch_when_per_target_absent() {
        let server = MockServer::start().await;
        let manifest_body = manifest_toml("slack", "0.2.0");
        let manifest_url = format!("{}/manifest", server.uri());
        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "nexo-plugin.toml", "browser_download_url": manifest_url, "size": manifest_body.len()},
                // ONLY noarch — no per-target tarball.
                {"name": "slack-0.2.0-noarch.tar.gz", "browser_download_url": "https://example.com/tar", "size": 100},
                {"name": "slack-0.2.0-noarch.tar.gz.sha256", "browser_download_url": "https://example.com/sha", "size": 64}
            ]
        });
        Mock::given(method("GET"))
            .and(path("/repos/alice/x/releases/tags/v0.2.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
            .mount(&server)
            .await;

        let coords = PluginCoords::parse("alice/x@v0.2.0").unwrap();
        let client = reqwest::Client::new();
        let resolved = resolve_release(&client, &coords, "x86_64-unknown-linux-gnu", &server.uri())
            .await
            .expect("noarch fallback");
        assert_eq!(resolved.entry.downloads[0].url.as_str(), "https://example.com/tar");
        assert!(resolved.sha256_url.contains("/sha"));
    }

    #[tokio::test]
    async fn resolve_release_prefers_per_target_over_noarch() {
        let server = MockServer::start().await;
        let manifest_body = manifest_toml("slack", "0.2.0");
        let manifest_url = format!("{}/manifest", server.uri());
        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "nexo-plugin.toml", "browser_download_url": manifest_url, "size": manifest_body.len()},
                {"name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz", "browser_download_url": "https://example.com/per-target", "size": 100},
                {"name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256", "browser_download_url": "https://example.com/per-sha", "size": 64},
                {"name": "slack-0.2.0-noarch.tar.gz", "browser_download_url": "https://example.com/noarch", "size": 100},
                {"name": "slack-0.2.0-noarch.tar.gz.sha256", "browser_download_url": "https://example.com/noarch-sha", "size": 64}
            ]
        });
        Mock::given(method("GET"))
            .and(path("/repos/alice/x/releases/tags/v0.2.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
            .mount(&server)
            .await;

        let coords = PluginCoords::parse("alice/x@v0.2.0").unwrap();
        let client = reqwest::Client::new();
        let resolved = resolve_release(&client, &coords, "x86_64-unknown-linux-gnu", &server.uri())
            .await
            .expect("per-target preferred");
        assert_eq!(
            resolved.entry.downloads[0].url.as_str(),
            "https://example.com/per-target",
            "per-target tarball must win when both present"
        );
    }

    #[tokio::test]
    async fn detects_sha256_mismatch_and_cleans_up() {
        let server = MockServer::start().await;
        let manifest_body = manifest_toml("slack", "0.2.0");
        let tarball_payload = b"actual bytes here";
        // ADVERTISE A WRONG sha256 — install must reject + remove.
        let advertised_sha =
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n";

        let manifest_url = format!("{}/manifest", server.uri());
        let tarball_url = format!("{}/tarball", server.uri());
        let sha_url = format!("{}/sha256", server.uri());

        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "nexo-plugin.toml", "browser_download_url": manifest_url, "size": manifest_body.len()},
                {"name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz", "browser_download_url": tarball_url, "size": tarball_payload.len()},
                {"name": "slack-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256", "browser_download_url": sha_url, "size": advertised_sha.len()}
            ]
        });
        Mock::given(method("GET"))
            .and(path("/repos/alice/x/releases/tags/v0.2.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/manifest"))
            .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/sha256"))
            .respond_with(ResponseTemplate::new(200).set_body_string(advertised_sha))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/tarball"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(tarball_payload.as_slice()),
            )
            .mount(&server)
            .await;

        let coords = PluginCoords::parse("alice/x@v0.2.0").unwrap();
        let client = reqwest::Client::new();
        let resolved =
            resolve_release(&client, &coords, "x86_64-unknown-linux-gnu", &server.uri())
                .await
                .expect("resolve");

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("tampered.tar.gz");
        match download_and_verify(&client, &resolved, &dest).await {
            Err(InstallError::Sha256Mismatch { id, .. }) => assert_eq!(id, "slack"),
            other => panic!("expected Sha256Mismatch, got {other:?}"),
        }
        assert!(!dest.exists(), "partial file must be removed on mismatch");
    }
}
