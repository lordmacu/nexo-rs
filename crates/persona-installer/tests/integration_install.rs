//! Phase F4 — wiremock integration tests for the
//! `nexo-persona-installer` orchestrator + admin layer.
//!
//! Eleven install scenarios cover the resolve → validate →
//! download → verify → extract pipeline end-to-end:
//!
//! 1. Happy-path install
//! 2. Idempotent re-install (same id+version twice)
//! 3. Rejects v1 manifest at parse with migration hint
//! 4. Rejects manifest with invalid id (validator)
//! 5. Rejects when `persona.toml` asset missing from release
//! 6. Rejects when target tarball asset missing
//! 7. Falls back to `noarch` tarball when per-target absent
//! 8. Detects sha256 mismatch + cleans up partial download
//! 9. Rejects relative install_root (loud-fail, not silent
//!    canonicalize-to-cwd)
//! 10. Rejects tarball entry with parent-traversal path
//! 11. Validate runs on the on-disk re-parse too (defense
//!     in depth — caught even if the release-side manifest
//!     somehow got past the resolver's parse)
//!
//! Five admin scenarios round-trip the in-memory admin
//! impl + cover edge cases not exercised by the unit tests
//! (install→register→list, register→remove→list, version
//! upgrade overwrite, unknown-id get returns None, unknown-id
//! remove errors NotFound).

use std::path::Path;

use nexo_ext_installer::RepoCoords;
use nexo_persona_installer::{
    install_persona, InMemoryPersonaAdmin, InstallInputs, PersonaAdmin, PersonaInstallError,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ──────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────

/// Build a valid v2 persona.toml string with the given
/// id/version. Used both as the release-side asset body and
/// to seed the in-tarball `persona.toml`.
fn persona_toml(id: &str, version: &str) -> String {
    format!(
        r#"manifest_version = 2
[persona]
id = "{id}"
version = "{version}"
description = "Integration-test persona"
min_nexo_version = ">=0.1.0"
"#
    )
}

/// Build a tar.gz with `persona.toml` at the root + an
/// optional second entry (e.g. an agents.d/ file). Returns
/// the bytes.
fn build_persona_tarball(id: &str, version: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let manifest_body = persona_toml(id, version);
    let mut buf: Vec<u8> = Vec::new();
    {
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut builder = tar::Builder::new(gz);

        // persona.toml at root
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(&mut header, "persona.toml", manifest_body.as_bytes())
            .unwrap();

        for (name, data) in extra {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            builder.append_data(&mut h, *name, *data).unwrap();
        }

        builder.into_inner().unwrap().finish().unwrap();
    }
    buf
}

/// Build a malicious tar.gz containing one entry whose name
/// is `../escape.txt` (writer-side guard bypassed by raw
/// header manipulation). Used by scenario 10.
fn build_malicious_tarball() -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let payload = b"malicious";
    let mut buf: Vec<u8> = Vec::new();
    {
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut builder = tar::Builder::new(gz);
        let mut header = tar::Header::new_old();
        let bad_name = b"../escape.txt";
        header.as_old_mut().name[..bad_name.len()].copy_from_slice(bad_name);
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append(&header, &payload[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }
    buf
}

/// Mount a complete v2 persona release on the MockServer:
/// release JSON + persona.toml asset + tarball + sha256.
/// Returns the manifest body so callers can override it (for
/// the "rejects v1" / "rejects invalid id" scenarios).
struct ReleaseFixture {
    server: MockServer,
    coords: RepoCoords,
    target: String,
}

async fn mount_happy_release(
    id: &str,
    version: &str,
    target: &str,
    asset_target_segment: &str,
    extra_in_tarball: &[(&str, &[u8])],
) -> ReleaseFixture {
    let server = MockServer::start().await;

    let manifest_body = persona_toml(id, version);
    let tarball_bytes = build_persona_tarball(id, version, extra_in_tarball);
    let mut hasher = Sha256::new();
    hasher.update(&tarball_bytes);
    let sha_body = format!("{}\n", hex::encode(hasher.finalize()));

    let manifest_url = format!("{}/persona-toml", server.uri());
    let tarball_url = format!("{}/persona-tar", server.uri());
    let sha_url = format!("{}/persona-sha", server.uri());

    let tarball_asset_name = format!("{id}-{version}-{asset_target_segment}.tar.gz");
    let sha_asset_name = format!("{tarball_asset_name}.sha256");

    let release = json!({
        "tag_name": format!("v{version}"),
        "assets": [
            {"name": "persona.toml", "browser_download_url": manifest_url, "size": manifest_body.len()},
            {"name": tarball_asset_name, "browser_download_url": tarball_url, "size": tarball_bytes.len()},
            {"name": sha_asset_name, "browser_download_url": sha_url, "size": sha_body.len()}
        ]
    });

    Mock::given(method("GET"))
        .and(path(format!(
            "/repos/alice/nexo-persona-{id}/releases/tags/v{version}"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/persona-toml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/persona-sha"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sha_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/persona-tar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball_bytes.as_slice()))
        .mount(&server)
        .await;

    ReleaseFixture {
        server,
        coords: RepoCoords::parse(&format!("alice/nexo-persona-{id}@v{version}")).unwrap(),
        target: target.to_string(),
    }
}

async fn run_install(
    fix: &ReleaseFixture,
    install_root: &Path,
) -> Result<nexo_persona_installer::InstalledPersona, PersonaInstallError> {
    let client = reqwest::Client::new();
    install_persona(InstallInputs {
        client: &client,
        coords: &fix.coords,
        target: &fix.target,
        install_root,
        api_base: &fix.server.uri(),
    })
    .await
}

// ──────────────────────────────────────────────────────────
// Install scenarios (1-11)
// ──────────────────────────────────────────────────────────

/// Scenario 1: happy-path install lays down the expected
/// directory + persona.toml + carries through manifest fields.
#[tokio::test]
async fn install_happy_path_lays_down_install_dir_and_returns_manifest() {
    let fix = mount_happy_release(
        "cody",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        &[("agents.d/cody.yaml", b"id: cody\nname: Cody\n")],
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let installed = run_install(&fix, tmp.path()).await.expect("install");

    assert_eq!(installed.id, "cody");
    assert_eq!(installed.version.to_string(), "0.2.0");
    assert!(installed.install_root.starts_with(tmp.path()));
    assert!(installed.install_root.ends_with("cody-0.2.0"));
    assert!(installed.install_root.join("persona.toml").exists());
    assert!(installed.install_root.join("agents.d/cody.yaml").exists());
    assert!(!installed.was_already_present);
    assert!(installed.tarball_bytes > 0);
}

/// Scenario 2: a second install at the same version short-
/// circuits via the idempotency check (no re-download).
#[tokio::test]
async fn install_is_idempotent_when_dir_already_holds_same_version() {
    let fix = mount_happy_release(
        "cody",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        &[],
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    run_install(&fix, tmp.path()).await.expect("first install");
    let second = run_install(&fix, tmp.path()).await.expect("second install");
    assert!(
        second.was_already_present,
        "second install must short-circuit via idempotency check"
    );
    assert_eq!(
        second.tarball_bytes, 0,
        "no bytes downloaded on idempotent re-install"
    );
}

/// Scenario 3: a v1 manifest in the release (operator
/// published a stale pack) errors at the resolver's
/// parse_manifest step with the migration hint.
#[tokio::test]
async fn install_rejects_v1_manifest_with_migration_hint() {
    let server = MockServer::start().await;
    let v1_body = r#"manifest_version = 1
[persona]
id = "cody"
version = "0.2.0"
description = "v1 pack"
min_nexo_version = ">=0.1.0"
"#;
    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "persona.toml", "browser_download_url": format!("{}/m", server.uri()), "size": v1_body.len()}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_string(v1_body))
        .mount(&server)
        .await;

    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    let result = install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await;
    match result {
        Err(PersonaInstallError::Ext(nexo_ext_installer::InstallError::ReleaseShape {
            reason,
            ..
        })) => {
            assert!(
                reason.contains("install.sh"),
                "v1 rejection must surface install.sh hint, got: {reason}"
            );
        }
        other => panic!("expected ReleaseShape v1 error, got {other:?}"),
    }
}

/// Scenario 4: manifest in the release parses but fails
/// validation (uppercase id). Caught by the orchestrator's
/// validate() pass right after resolve.
#[tokio::test]
async fn install_rejects_manifest_with_invalid_id() {
    let server = MockServer::start().await;
    let bad_body = r#"manifest_version = 2
[persona]
id = "BadCaseID"
version = "0.2.0"
description = "x"
min_nexo_version = ">=0.1.0"
"#;
    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "persona.toml", "browser_download_url": format!("{}/m", server.uri()), "size": bad_body.len()},
            {"name": "BadCaseID-0.2.0-noarch.tar.gz", "browser_download_url": format!("{}/t", server.uri()), "size": 10},
            {"name": "BadCaseID-0.2.0-noarch.tar.gz.sha256", "browser_download_url": format!("{}/s", server.uri()), "size": 64}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_string(bad_body))
        .mount(&server)
        .await;

    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    let result = install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await;
    match result {
        Err(PersonaInstallError::Manifest(
            nexo_persona_manifest::PersonaManifestError::InvalidId { got },
        )) => assert_eq!(got, "BadCaseID"),
        other => panic!("expected InvalidId, got {other:?}"),
    }
}

/// Scenario 5: release with no `persona.toml` asset → resolver
/// errors at the contract-driven manifest lookup (echoing the
/// contract's filename, not the plugin's).
#[tokio::test]
async fn install_rejects_release_missing_persona_toml_asset() {
    let server = MockServer::start().await;
    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            // ONLY the tarball, no manifest asset.
            {"name": "cody-0.2.0-noarch.tar.gz", "browser_download_url": "https://example.com/t", "size": 10}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    match install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await
    {
        Err(PersonaInstallError::Ext(nexo_ext_installer::InstallError::ReleaseShape {
            reason,
            ..
        })) => {
            assert!(reason.contains("persona.toml"), "got: {reason}");
        }
        other => panic!("expected ReleaseShape missing-asset, got {other:?}"),
    }
}

/// Scenario 6: release has the manifest but no tarball for
/// the requested target AND no noarch fallback.
#[tokio::test]
async fn install_rejects_when_no_tarball_matches_target_and_no_noarch() {
    let server = MockServer::start().await;
    let manifest_body = persona_toml("cody", "0.2.0");
    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "persona.toml", "browser_download_url": format!("{}/m", server.uri()), "size": manifest_body.len()},
            // ONLY aarch64-darwin — operator asked for x86_64-linux.
            {"name": "cody-0.2.0-aarch64-apple-darwin.tar.gz", "browser_download_url": "https://example.com/t", "size": 10}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
        .mount(&server)
        .await;
    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    match install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await
    {
        Err(PersonaInstallError::Ext(nexo_ext_installer::InstallError::TargetNotFound {
            available,
            ..
        })) => {
            assert_eq!(
                available,
                vec!["cody-0.2.0-aarch64-apple-darwin.tar.gz".to_string()]
            );
        }
        other => panic!("expected TargetNotFound, got {other:?}"),
    }
}

/// Scenario 7: per-target tarball absent but noarch present
/// → install succeeds via noarch fallback.
#[tokio::test]
async fn install_falls_back_to_noarch_when_per_target_absent() {
    let fix = mount_happy_release(
        "cody",
        "0.2.0",
        "x86_64-unknown-linux-gnu",
        // Asset target segment != caller's target → noarch
        // fallback path exercised.
        "noarch",
        &[],
    )
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let installed = run_install(&fix, tmp.path())
        .await
        .expect("noarch fallback");
    assert!(installed.install_root.join("persona.toml").exists());
}

/// Scenario 8: sha256 advertised by `.sha256` asset doesn't
/// match the actual tarball bytes → install errors +
/// staging tarball is removed.
#[tokio::test]
async fn install_rejects_sha256_mismatch_and_cleans_partial() {
    let server = MockServer::start().await;
    let manifest_body = persona_toml("cody", "0.2.0");
    let real_tarball = build_persona_tarball("cody", "0.2.0", &[]);
    // Advertise a wrong sha — expect Sha256Mismatch.
    let advertised_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n";

    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "persona.toml", "browser_download_url": format!("{}/m", server.uri()), "size": manifest_body.len()},
            {"name": "cody-0.2.0-noarch.tar.gz", "browser_download_url": format!("{}/t", server.uri()), "size": real_tarball.len()},
            {"name": "cody-0.2.0-noarch.tar.gz.sha256", "browser_download_url": format!("{}/s", server.uri()), "size": advertised_sha.len()}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s"))
        .respond_with(ResponseTemplate::new(200).set_body_string(advertised_sha))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/t"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(real_tarball.as_slice()))
        .mount(&server)
        .await;

    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    match install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await
    {
        Err(PersonaInstallError::Ext(nexo_ext_installer::InstallError::Sha256Mismatch {
            id,
            ..
        })) => assert_eq!(id, "cody"),
        other => panic!("expected Sha256Mismatch, got {other:?}"),
    }
    // The staging .partial file under install_root should be
    // gone (download_and_verify_url removes on mismatch).
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.iter().all(|n| !n.ends_with(".partial")),
        "no .partial staging files allowed after sha mismatch, found: {leftovers:?}"
    );
}

/// Scenario 9: relative install_root rejected loud (no
/// silent canonicalization to cwd).
#[tokio::test]
async fn install_rejects_relative_install_root() {
    let fix = mount_happy_release("cody", "0.2.0", "x86_64-unknown-linux-gnu", "noarch", &[]).await;
    let client = reqwest::Client::new();
    match install_persona(InstallInputs {
        client: &client,
        coords: &fix.coords,
        target: &fix.target,
        install_root: Path::new("relative/path"),
        api_base: &fix.server.uri(),
    })
    .await
    {
        Err(PersonaInstallError::InstallRootNotAbsolute { got }) => {
            assert_eq!(got, "relative/path")
        }
        other => panic!("expected InstallRootNotAbsolute, got {other:?}"),
    }
}

/// Scenario 10: malicious tarball with `..` traversal entry
/// rejected at extraction; final dir is NOT created.
#[tokio::test]
async fn install_rejects_malicious_tarball_with_parent_traversal() {
    let server = MockServer::start().await;
    let manifest_body = persona_toml("cody", "0.2.0");
    let tarball = build_malicious_tarball();
    let mut hasher = Sha256::new();
    hasher.update(&tarball);
    let sha_body = format!("{}\n", hex::encode(hasher.finalize()));

    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "persona.toml", "browser_download_url": format!("{}/m", server.uri()), "size": manifest_body.len()},
            {"name": "cody-0.2.0-noarch.tar.gz", "browser_download_url": format!("{}/t", server.uri()), "size": tarball.len()},
            {"name": "cody-0.2.0-noarch.tar.gz.sha256", "browser_download_url": format!("{}/s", server.uri()), "size": sha_body.len()}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sha_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/t"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball.as_slice()))
        .mount(&server)
        .await;

    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    match install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await
    {
        Err(PersonaInstallError::Extract { reason, .. }) => {
            assert!(reason.contains(".."), "got: {reason}");
        }
        other => panic!("expected Extract parent-traversal, got {other:?}"),
    }
    // Final dir must NOT have been created.
    assert!(!tmp.path().join("cody-0.2.0").exists());
}

/// Scenario 11: tarball is missing the in-archive
/// `persona.toml` → on-disk re-parse fails after extraction
/// (defense in depth — even a release that publishes a valid
/// manifest as an asset can ship a broken tarball).
#[tokio::test]
async fn install_rejects_when_tarball_missing_in_archive_persona_toml() {
    let server = MockServer::start().await;
    let manifest_body = persona_toml("cody", "0.2.0");

    // Build a tarball with ONLY a placeholder file — no
    // persona.toml at root.
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let tarball = {
        let mut buf: Vec<u8> = Vec::new();
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut builder = tar::Builder::new(gz);
        let payload = b"placeholder";
        let mut h = tar::Header::new_gnu();
        h.set_size(payload.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        builder
            .append_data(&mut h, "README.md", &payload[..])
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        buf
    };
    let mut hasher = Sha256::new();
    hasher.update(&tarball);
    let sha_body = format!("{}\n", hex::encode(hasher.finalize()));

    let release = json!({
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "persona.toml", "browser_download_url": format!("{}/m", server.uri()), "size": manifest_body.len()},
            {"name": "cody-0.2.0-noarch.tar.gz", "browser_download_url": format!("{}/t", server.uri()), "size": tarball.len()},
            {"name": "cody-0.2.0-noarch.tar.gz.sha256", "browser_download_url": format!("{}/s", server.uri()), "size": sha_body.len()}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/repos/alice/x/releases/tags/v0.2.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/m"))
        .respond_with(ResponseTemplate::new(200).set_body_string(manifest_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sha_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/t"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tarball.as_slice()))
        .mount(&server)
        .await;

    let coords = RepoCoords::parse("alice/x@v0.2.0").unwrap();
    let client = reqwest::Client::new();
    let tmp = tempfile::tempdir().unwrap();
    match install_persona(InstallInputs {
        client: &client,
        coords: &coords,
        target: "x86_64-unknown-linux-gnu",
        install_root: tmp.path(),
        api_base: &server.uri(),
    })
    .await
    {
        Err(PersonaInstallError::Io { op, .. }) => assert_eq!(op, "read"),
        other => panic!("expected on-disk persona.toml read failure, got {other:?}"),
    }
}

// ──────────────────────────────────────────────────────────
// Admin scenarios (1-5)
// ──────────────────────────────────────────────────────────

/// Admin 1: install + register + list shows the entry with
/// correct source_repo + version.
#[tokio::test]
async fn admin_install_register_list_round_trip() {
    let fix = mount_happy_release("cody", "0.2.0", "x86_64-unknown-linux-gnu", "noarch", &[]).await;
    let tmp = tempfile::tempdir().unwrap();
    let installed = run_install(&fix, tmp.path()).await.expect("install");
    let admin = InMemoryPersonaAdmin::new();
    admin.register(installed).await.unwrap();
    let listed = admin.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "cody");
    assert_eq!(listed[0].source_repo, "alice/nexo-persona-cody");
    assert_eq!(listed[0].version.to_string(), "0.2.0");
}

/// Admin 2: register + remove + list returns empty +
/// install_root path on the removed entry matches the
/// install dir.
#[tokio::test]
async fn admin_register_remove_then_list_is_empty() {
    let fix = mount_happy_release("cody", "0.2.0", "x86_64-unknown-linux-gnu", "noarch", &[]).await;
    let tmp = tempfile::tempdir().unwrap();
    let installed = run_install(&fix, tmp.path()).await.expect("install");
    let install_root = installed.install_root.clone();
    let admin = InMemoryPersonaAdmin::new();
    admin.register(installed).await.unwrap();
    let removed = admin.remove("cody").await.expect("remove");
    assert_eq!(removed.install_root, install_root);
    assert!(admin.list().await.unwrap().is_empty());
}

/// Admin 3: register same id at two versions — second
/// register overwrites first, list size stays at 1 with
/// the newer version.
#[tokio::test]
async fn admin_re_register_at_higher_version_overwrites() {
    let fix_v1 =
        mount_happy_release("cody", "0.1.0", "x86_64-unknown-linux-gnu", "noarch", &[]).await;
    let fix_v2 =
        mount_happy_release("cody", "0.2.0", "x86_64-unknown-linux-gnu", "noarch", &[]).await;
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    let i1 = run_install(&fix_v1, tmp1.path()).await.expect("v1");
    let i2 = run_install(&fix_v2, tmp2.path()).await.expect("v2");
    let admin = InMemoryPersonaAdmin::new();
    admin.register(i1).await.unwrap();
    admin.register(i2).await.unwrap();
    let listed = admin.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version.to_string(), "0.2.0");
}

/// Admin 4: get on an unknown id returns Ok(None) (not an
/// error variant) so CLI callers can render "not found"
/// without matching on errors.
#[tokio::test]
async fn admin_get_unknown_id_returns_none() {
    let admin = InMemoryPersonaAdmin::new();
    assert!(admin.get("nonexistent").await.unwrap().is_none());
}

/// Admin 5: remove on an unknown id surfaces NotFound with
/// the requested id echoed back.
#[tokio::test]
async fn admin_remove_unknown_id_errors_not_found() {
    let admin = InMemoryPersonaAdmin::new();
    match admin.remove("ghost").await {
        Err(PersonaInstallError::NotFound { id, .. }) => assert_eq!(id, "ghost"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}
