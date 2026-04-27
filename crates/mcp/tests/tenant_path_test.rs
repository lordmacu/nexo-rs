//! Phase 76.4 — symlink-aware containment tests for
//! `tenant_scoped_canonicalize`. Unix-only because Windows
//! `std::fs::canonicalize` returns UNC paths that break the prefix
//! check; the production targets are Linux musl + Termux.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::Path;

use nexo_mcp::server::auth::tenant::{tenant_scoped_canonicalize, TenantId, TenantPathError};
use tempfile::TempDir;

fn tid(s: &str) -> TenantId {
    TenantId::parse(s).unwrap()
}

#[test]
fn canonicalize_accepts_simple_existing_path() {
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    // Pre-create the tenant dir + a file.
    let dir = tmp.path().join("tenants").join("a");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), b"hi").unwrap();
    let p = tenant_scoped_canonicalize(tmp.path(), &t, "notes.txt").unwrap();
    assert!(p.ends_with("tenants/a/notes.txt"));
}

#[test]
fn canonicalize_accepts_nonexisting_file_in_existing_dir() {
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    std::fs::create_dir_all(tmp.path().join("tenants").join("a")).unwrap();
    let p = tenant_scoped_canonicalize(tmp.path(), &t, "future.txt").unwrap();
    assert!(p.ends_with("tenants/a/future.txt"));
}

#[test]
fn canonicalize_rejects_dotdot_lexically() {
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    let err = tenant_scoped_canonicalize(tmp.path(), &t, "../escape.txt").unwrap_err();
    assert!(matches!(err, TenantPathError::DotDot(_)), "got {err:?}");
}

#[test]
fn canonicalize_rejects_absolute_suffix() {
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    let err = tenant_scoped_canonicalize(tmp.path(), &t, "/etc/passwd").unwrap_err();
    assert!(
        matches!(err, TenantPathError::AbsoluteSuffix(_)),
        "got {err:?}"
    );
}

#[test]
fn canonicalize_detects_symlink_escape() {
    // Create a symlink inside tenant-a that points outside the
    // tenant root. canonicalize() must follow it and the resulting
    // resolved path must fail the containment check.
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    let tenant_dir = tmp.path().join("tenants").join("a");
    std::fs::create_dir_all(&tenant_dir).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"sekrit").unwrap();
    symlink(&outside, tenant_dir.join("escape")).unwrap();

    let err = tenant_scoped_canonicalize(tmp.path(), &t, "escape/secret.txt").unwrap_err();
    match err {
        TenantPathError::SymlinkEscape { resolved, expected } => {
            assert!(
                !resolved.starts_with(&expected),
                "resolved={resolved:?} expected_prefix={expected:?}"
            );
        }
        other => panic!("expected SymlinkEscape, got {other:?}"),
    }
}

#[test]
fn canonicalize_detects_symlink_loop() {
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    let tenant_dir = tmp.path().join("tenants").join("a");
    std::fs::create_dir_all(&tenant_dir).unwrap();
    // a -> b -> a (loop)
    let a = tenant_dir.join("a");
    let b = tenant_dir.join("b");
    symlink(&b, &a).unwrap();
    symlink(&a, &b).unwrap();
    let err = tenant_scoped_canonicalize(tmp.path(), &t, "a/inside.txt").unwrap_err();
    assert!(
        matches!(err, TenantPathError::SymlinkLoop | TenantPathError::Io(_)),
        "got {err:?}"
    );
}

#[test]
fn canonicalize_detects_dangling_symlink() {
    let tmp = TempDir::new().unwrap();
    let t = tid("a");
    let tenant_dir = tmp.path().join("tenants").join("a");
    std::fs::create_dir_all(&tenant_dir).unwrap();
    // dangling symlink: target doesn't exist.
    symlink(
        Path::new("/nonexistent-nexo-test-target-9821"),
        tenant_dir.join("dangling"),
    )
    .unwrap();
    let err = tenant_scoped_canonicalize(tmp.path(), &t, "dangling").unwrap_err();
    // Either DanglingSymlink (preferred) or SymlinkEscape (because
    // canonicalize fell back).
    assert!(
        matches!(
            err,
            TenantPathError::DanglingSymlink(_) | TenantPathError::Io(_)
        ),
        "got {err:?}"
    );
}

#[test]
fn canonicalize_separator_guard_blocks_sibling_tenant() {
    // Tenant `t` exists and tenant `t-evil` also exists. A path
    // calculation for tenant `t` that somehow tried to point at
    // `t-evil` (e.g. via symlink) must be rejected — the separator
    // guard ensures we don't accept `tenants/t-evil/...` as
    // "starts with tenants/t/".
    let tmp = TempDir::new().unwrap();
    let t = tid("t");
    std::fs::create_dir_all(tmp.path().join("tenants").join("t")).unwrap();
    let evil_dir = tmp.path().join("tenants").join("t-evil");
    std::fs::create_dir_all(&evil_dir).unwrap();
    std::fs::write(evil_dir.join("loot.txt"), b"loot").unwrap();
    // Symlink inside tenant t pointing at the sibling tenant's dir.
    symlink(
        &evil_dir,
        tmp.path().join("tenants").join("t").join("sibling"),
    )
    .unwrap();
    let err = tenant_scoped_canonicalize(tmp.path(), &t, "sibling/loot.txt").unwrap_err();
    assert!(
        matches!(err, TenantPathError::SymlinkEscape { .. }),
        "got {err:?}"
    );
}
