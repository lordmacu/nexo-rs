//! Tenant + agent id validation and bundle path resolution.
//!
//! The validators are a tightened subset of the multi-tenant path
//! canonicalization shipped in Phase 76.4 (`crates/mcp/src/server/auth/
//! tenant.rs`). We keep the rules local to avoid pulling `nexo-mcp` in
//! as a dependency — bundle paths never originate from network input,
//! so the lighter validator is sufficient. The variant set
//! intentionally mirrors the production rule (`[a-z0-9_-]{1,64}`,
//! no leading/trailing `_`/`-`, no NUL).

use std::path::{Path, PathBuf};

use crate::error::SnapshotError;
use crate::id::SnapshotId;

const MAX_LEN: usize = 64;

/// Validate a tenant identifier. Allowed: lowercase ASCII letters,
/// digits, `_`, `-`. 1..=64 chars. No leading or trailing punctuation.
pub fn validate_tenant(s: &str) -> Result<&str, SnapshotError> {
    validate_path_segment(s, "tenant")
}

/// Validate an agent id with the same rules as a tenant — agents.yaml
/// already enforces this charset upstream, but we re-check defensively
/// before the value joins a filesystem path.
pub fn validate_agent_id(s: &str) -> Result<&str, SnapshotError> {
    validate_path_segment(s, "agent_id")
}

fn validate_path_segment<'a>(s: &'a str, what: &'static str) -> Result<&'a str, SnapshotError> {
    if s.is_empty() || s.len() > MAX_LEN {
        return Err(SnapshotError::RestoreRefused(format!(
            "{what} length {} not in 1..={}",
            s.len(),
            MAX_LEN
        )));
    }
    if s.contains('\0') {
        return Err(SnapshotError::RestoreRefused(format!(
            "{what} contains NUL byte"
        )));
    }
    let bytes = s.as_bytes();
    if matches!(bytes[0], b'_' | b'-') || matches!(bytes[bytes.len() - 1], b'_' | b'-') {
        return Err(SnapshotError::RestoreRefused(format!(
            "{what} cannot start or end with `_` or `-`"
        )));
    }
    let charset_ok = bytes.iter().all(|b| {
        matches!(b,
            b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'
        )
    });
    if !charset_ok {
        return Err(SnapshotError::RestoreRefused(format!(
            "{what} must match [a-z0-9_-]"
        )));
    }
    Ok(s)
}

/// Resolve the tenant-scoped directory that holds `<tenant>`'s agent
/// state under `state_root`. Lexical: never follows symlinks, never
/// canonicalizes through user-controlled segments.
pub fn tenant_root(state_root: &Path, tenant: &str) -> Result<PathBuf, SnapshotError> {
    let tenant = validate_tenant(tenant)?;
    let path = state_root.join("tenants").join(tenant);
    if !path.starts_with(state_root) {
        return Err(SnapshotError::CrossTenant);
    }
    Ok(path)
}

/// Resolve the directory that holds every snapshot bundle for the
/// `(tenant, agent_id)` pair: `<state_root>/tenants/<tenant>/snapshots/<agent_id>/`.
pub fn snapshots_dir(
    state_root: &Path,
    tenant: &str,
    agent_id: &str,
) -> Result<PathBuf, SnapshotError> {
    let agent_id = validate_agent_id(agent_id)?;
    let dir = tenant_root(state_root, tenant)?
        .join("snapshots")
        .join(agent_id);
    if !dir.starts_with(state_root) {
        return Err(SnapshotError::CrossTenant);
    }
    Ok(dir)
}

/// Resolve the on-disk bundle path for a snapshot id. The `encrypted`
/// flag picks the trailing `.age` extension; both forms live in the
/// same directory so list/verify can detect either.
pub fn snapshot_bundle_path(
    state_root: &Path,
    tenant: &str,
    agent_id: &str,
    id: SnapshotId,
    encrypted: bool,
) -> Result<PathBuf, SnapshotError> {
    let dir = snapshots_dir(state_root, tenant, agent_id)?;
    let suffix = if encrypted { ".tar.zst.age" } else { ".tar.zst" };
    Ok(dir.join(format!("{}{}", id.as_filename(), suffix)))
}

/// Sibling whole-bundle SHA-256 path that lives next to a bundle. The
/// manifest's per-artifact hashes seal the contents; this file seals
/// the bundle byte stream itself, including any encryption layer.
pub fn bundle_sha256_sibling(bundle: &Path) -> PathBuf {
    let mut p = bundle.as_os_str().to_owned();
    p.push(".sha256");
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_typical_ids() {
        validate_tenant("acme").unwrap();
        validate_tenant("acme-corp").unwrap();
        validate_tenant("acme_corp").unwrap();
        validate_tenant("a1").unwrap();
        validate_agent_id("ana").unwrap();
        validate_agent_id("ventas-etb").unwrap();
    }

    #[test]
    fn rejects_uppercase() {
        let err = validate_tenant("Acme").unwrap_err();
        assert!(format!("{err}").contains("[a-z0-9_-]"));
    }

    #[test]
    fn rejects_leading_or_trailing_punct() {
        assert!(validate_tenant("-acme").is_err());
        assert!(validate_tenant("acme-").is_err());
        assert!(validate_tenant("_acme").is_err());
        assert!(validate_tenant("acme_").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let too_long = "a".repeat(65);
        assert!(validate_tenant(&too_long).is_err());
    }

    #[test]
    fn rejects_empty_and_nul() {
        assert!(validate_tenant("").is_err());
        assert!(validate_tenant("ac\0me").is_err());
    }

    #[test]
    fn rejects_path_components() {
        assert!(validate_tenant("..").is_err());
        assert!(validate_tenant("../etc").is_err());
        assert!(validate_tenant("a/b").is_err());
    }

    #[test]
    fn tenant_root_lives_under_state_root() {
        let root = Path::new("/var/lib/nexo");
        let path = tenant_root(root, "acme").unwrap();
        assert_eq!(path, Path::new("/var/lib/nexo/tenants/acme"));
        assert!(path.starts_with(root));
    }

    #[test]
    fn snapshots_dir_includes_agent_id() {
        let root = Path::new("/var/lib/nexo");
        let path = snapshots_dir(root, "acme", "ana").unwrap();
        assert_eq!(path, Path::new("/var/lib/nexo/tenants/acme/snapshots/ana"));
    }

    #[test]
    fn bundle_path_uses_extension_for_encryption_flag() {
        let root = Path::new("/var/lib/nexo");
        let id = SnapshotId::new();
        let plain = snapshot_bundle_path(root, "acme", "ana", id, false).unwrap();
        assert!(plain.to_string_lossy().ends_with(".tar.zst"));
        let enc = snapshot_bundle_path(root, "acme", "ana", id, true).unwrap();
        assert!(enc.to_string_lossy().ends_with(".tar.zst.age"));
    }

    #[test]
    fn bundle_sha256_sibling_appends_extension() {
        let p = Path::new("/x/y.tar.zst");
        assert_eq!(bundle_sha256_sibling(p), Path::new("/x/y.tar.zst.sha256"));
    }
}
