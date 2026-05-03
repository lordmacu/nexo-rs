//! Phase 31.2 — End-to-end test of `scripts/pack-tarball.sh`.
//!
//! Asserts that the bash pipeline (extract-plugin-meta.sh +
//! pack-tarball.sh) produces a tarball whose name + layout +
//! sha256 sidecar exactly match the convention `nexo-ext-installer`
//! consumes (validated against 31.1.b's `extract_verified_tarball`
//! expectations).
//!
//! Synthetic binary: a 1-byte file at
//! `<tempdir>/target/<host>/release/<plugin_id>`. We don't rebuild
//! the real binary — pack-tarball.sh treats it as opaque bytes,
//! so any non-empty file with the right path is enough.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const PLUGIN_ID: &str = "template_plugin_rust";
const PLUGIN_VERSION: &str = "0.1.0";

fn host_target_triple() -> String {
    let out = Command::new("rustc")
        .args(["-vV"])
        .output()
        .expect("rustc -vV");
    let text = String::from_utf8(out.stdout).expect("utf8");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("host: ") {
            return rest.trim().to_string();
        }
    }
    panic!("could not parse host target from rustc -vV");
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let dst_child = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_child);
        } else {
            fs::copy(entry.path(), &dst_child).unwrap();
        }
    }
}

fn template_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn pack_tarball_produces_canonical_asset_layout() {
    let target = host_target_triple();
    let work = TempDir::new().unwrap();

    // 1. Copy nexo-plugin.toml + scripts/ into a clean tempdir.
    fs::copy(
        template_root().join("nexo-plugin.toml"),
        work.path().join("nexo-plugin.toml"),
    )
    .unwrap();
    copy_dir_recursive(&template_root().join("scripts"), &work.path().join("scripts"));

    // 2. Drop a synthetic binary mirroring cargo's release layout.
    let bin_dir = work.path().join("target").join(&target).join("release");
    fs::create_dir_all(&bin_dir).unwrap();
    let bin_path = bin_dir.join(PLUGIN_ID);
    fs::write(&bin_path, b"#!fake\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();

    // 3. Run pack-tarball.sh from inside the tempdir.
    let status = Command::new("bash")
        .current_dir(work.path())
        .arg("scripts/pack-tarball.sh")
        .arg(&target)
        .status()
        .expect("invoke bash");
    assert!(status.success(), "pack-tarball.sh failed: {status:?}");

    // 4. Asset present at the expected path.
    let asset_name = format!("{}-{}-{}.tar.gz", PLUGIN_ID, PLUGIN_VERSION, target);
    let asset = work.path().join("dist").join(&asset_name);
    let sidecar = work.path().join("dist").join(format!("{}.sha256", asset_name));
    assert!(asset.is_file(), "asset missing: {}", asset.display());
    assert!(sidecar.is_file(), "sha256 sidecar missing: {}", sidecar.display());

    // 5. Sidecar is exactly 64 lowercase hex chars (+ optional newline).
    let sidecar_body = fs::read_to_string(&sidecar).unwrap();
    let sidecar_hex = sidecar_body.trim();
    assert_eq!(
        sidecar_hex.len(),
        64,
        "sidecar body must be 64 hex chars, got {} ({:?})",
        sidecar_hex.len(),
        sidecar_hex
    );
    assert!(
        sidecar_hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "sidecar must be lowercase hex"
    );

    // 6. Recompute sha256 of the tarball; must match sidecar.
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(&asset).unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let computed = hex::encode(hasher.finalize());
    assert_eq!(computed, sidecar_hex, "sha256 mismatch vs sidecar");

    // 7. Re-extract and verify layout: bin/<id> + nexo-plugin.toml at root.
    let extract_dir = TempDir::new().unwrap();
    let tar_gz = fs::File::open(&asset).unwrap();
    let decoder = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(extract_dir.path()).unwrap();

    let extracted_bin = extract_dir.path().join("bin").join(PLUGIN_ID);
    let extracted_manifest = extract_dir.path().join("nexo-plugin.toml");
    assert!(
        extracted_bin.is_file(),
        "bin/{} missing after extract",
        PLUGIN_ID
    );
    assert!(
        extracted_manifest.is_file(),
        "nexo-plugin.toml missing after extract"
    );

    // No top-level wrapping dir: every top-level entry in the
    // tarball must be either `bin` or `nexo-plugin.toml`.
    for entry in fs::read_dir(extract_dir.path()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        assert!(
            name == "bin" || name == "nexo-plugin.toml",
            "unexpected top-level entry: {}",
            name
        );
    }

    // Binary executable bit preserved on Unix (chmod 0755 at pack time).
    let mode = fs::metadata(&extracted_bin).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "binary mode should be 0755, got {:o}", mode);
}
