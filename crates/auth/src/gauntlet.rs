//! Boot-time validators. Every function is pure / filesystem-bound
//! and returns accumulated [`BuildError`]s instead of failing fast —
//! the caller collects errors from every check and reports them in
//! one pass so operators fix their YAML in a single edit.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::BuildError;
use crate::handle::Channel;

/// Environment flag to skip the linux permission check in dev / CI
/// where 0o644 credential fixtures are common. Docker secrets under
/// `/run/secrets/*` are skipped automatically regardless.
pub const SKIP_PERM_ENV: &str = "CHAT_AUTH_SKIP_PERM_CHECK";

#[derive(Debug, Clone)]
pub struct PathClaim {
    pub path: PathBuf,
    pub channel: Channel,
    pub instance: String,
}

/// Canonicalize each path, creating it with mode 0o700 first if it is
/// a session_dir that does not yet exist (WhatsApp pairs on first
/// launch). Returns canonicalized copies paired with their claim.
pub fn canonicalize_session_dirs(claims: &[PathClaim]) -> (Vec<PathClaim>, Vec<BuildError>) {
    let mut ok = Vec::with_capacity(claims.len());
    let mut errs = Vec::new();
    for c in claims {
        if !c.path.exists() {
            if let Err(e) = std::fs::create_dir_all(&c.path) {
                errs.push(BuildError::Credential {
                    channel: c.channel,
                    instance: c.instance.clone(),
                    source: crate::error::CredentialError::Unreadable {
                        path: c.path.clone(),
                        source: e,
                    },
                });
                continue;
            }
            // Best-effort mode 0o700; ignore errors on non-unix.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&c.path) {
                    let mut perm = meta.permissions();
                    perm.set_mode(0o700);
                    if let Err(e) = std::fs::set_permissions(&c.path, perm) {
                        tracing::warn!(
                            path = %c.path.display(),
                            error = %e,
                            "could not chmod 0o700 on credentials file; secrets may be world-readable"
                        );
                    }
                }
            }
        }
        match std::fs::canonicalize(&c.path) {
            Ok(p) => ok.push(PathClaim {
                path: p,
                channel: c.channel,
                instance: c.instance.clone(),
            }),
            Err(e) => errs.push(BuildError::Credential {
                channel: c.channel,
                instance: c.instance.clone(),
                source: crate::error::CredentialError::Unreadable {
                    path: c.path.clone(),
                    source: e,
                },
            }),
        }
    }
    (ok, errs)
}

/// No two claims share a canonical path. Run after
/// [`canonicalize_session_dirs`].
pub fn check_duplicate_paths(claims: &[PathClaim]) -> Vec<BuildError> {
    let mut seen: HashMap<PathBuf, (Channel, String)> = HashMap::new();
    let mut out = Vec::new();
    for c in claims {
        if let Some((prev_channel, prev_instance)) = seen.get(&c.path).cloned() {
            out.push(BuildError::DuplicatePath {
                path: c.path.clone(),
                a_channel: prev_channel,
                a_instance: prev_instance,
                b_channel: c.channel,
                b_instance: c.instance.clone(),
            });
        } else {
            seen.insert(c.path.clone(), (c.channel, c.instance.clone()));
        }
    }
    out
}

/// No session_dir is a parent of another — two WA accounts sharing a
/// nested dir would overwrite each other's Signal keystore.
pub fn check_prefix_overlap(claims: &[PathClaim]) -> Vec<BuildError> {
    let mut out = Vec::new();
    for (i, a) in claims.iter().enumerate() {
        for b in claims.iter().skip(i + 1) {
            if a.path == b.path {
                continue; // duplicate path already reported
            }
            if b.path.starts_with(&a.path) {
                out.push(BuildError::PathPrefixOverlap {
                    outer: a.path.clone(),
                    inner: b.path.clone(),
                });
            } else if a.path.starts_with(&b.path) {
                out.push(BuildError::PathPrefixOverlap {
                    outer: b.path.clone(),
                    inner: a.path.clone(),
                });
            }
        }
    }
    out
}

/// Linux-only 0o600/0o700 check. Docker secret mounts and paths under
/// `/run/secrets/` are skipped (orchestrator owns perms). Returns
/// [`CredentialError::InsecurePermissions`] wrapped in
/// [`BuildError::Credential`] for each offender.
pub fn check_permissions(paths: &[(Channel, String, PathBuf)]) -> Vec<BuildError> {
    if std::env::var(SKIP_PERM_ENV).ok().as_deref() == Some("1") {
        return Vec::new();
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        return Vec::new();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut out = Vec::new();
        for (channel, instance, path) in paths {
            if path.starts_with("/run/secrets/") {
                continue;
            }
            let meta = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    out.push(BuildError::Credential {
                        channel,
                        instance: instance.clone(),
                        source: crate::error::CredentialError::FileMissing { path: path.clone() },
                    });
                    continue;
                }
                Err(e) => {
                    out.push(BuildError::Credential {
                        channel,
                        instance: instance.clone(),
                        source: crate::error::CredentialError::Unreadable {
                            path: path.clone(),
                            source: e,
                        },
                    });
                    continue;
                }
            };
            let mode = meta.permissions().mode();
            // Fail if group or others have any bits set. 0o077 mask
            // catches every lax case (0o644, 0o755, 0o777, …).
            if mode & 0o077 != 0 {
                out.push(BuildError::Credential {
                    channel,
                    instance: instance.clone(),
                    source: crate::error::CredentialError::InsecurePermissions {
                        path: path.clone(),
                        mode,
                    },
                });
            }
        }
        out
    }
}

/// Pretty-print every error on its own line. Used by `--check-config`
/// and by `AgentCredentialResolver::build` panic-in-production paths.
pub fn format_errors(errs: &[BuildError]) -> String {
    let mut s = String::new();
    for (i, e) in errs.iter().enumerate() {
        s.push_str(&format!("  {:>2}. {e}\n", i + 1));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn claim(channel: Channel, instance: &str, path: &std::path::Path) -> PathClaim {
        PathClaim {
            path: path.to_path_buf(),
            channel,
            instance: instance.to_string(),
        }
    }

    #[test]
    fn duplicate_paths_are_reported() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("shared");
        std::fs::create_dir(&p).unwrap();
        let cs = vec![
            claim(crate::handle::WHATSAPP, "a", &p),
            claim(crate::handle::WHATSAPP, "b", &p),
        ];
        let errs = check_duplicate_paths(&cs);
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            BuildError::DuplicatePath { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn prefix_overlap_is_reported() {
        let dir = TempDir::new().unwrap();
        let outer = dir.path().join("wa");
        let inner = outer.join("personal");
        std::fs::create_dir_all(&inner).unwrap();
        let cs = vec![
            claim(crate::handle::WHATSAPP, "outer", &outer),
            claim(crate::handle::WHATSAPP, "inner", &inner),
        ];
        let errs = check_prefix_overlap(&cs);
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            BuildError::PathPrefixOverlap { outer: o, inner: i } => {
                assert_eq!(o, &outer);
                assert_eq!(i, &inner);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn disjoint_paths_pass() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let cs = vec![
            claim(crate::handle::WHATSAPP, "a", &a),
            claim(crate::handle::WHATSAPP, "b", &b),
        ];
        assert!(check_duplicate_paths(&cs).is_empty());
        assert!(check_prefix_overlap(&cs).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn insecure_permissions_are_reported() {
        use std::os::unix::fs::PermissionsExt;
        // Skip when the env override is set (CI lane).
        if std::env::var(SKIP_PERM_ENV).ok().as_deref() == Some("1") {
            return;
        }
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("token.json");
        std::fs::write(&p, "{}").unwrap();
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o644);
        std::fs::set_permissions(&p, perm).unwrap();
        let errs = check_permissions(&[(crate::handle::GOOGLE, "ana".into(), p)]);
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            BuildError::Credential {
                source: crate::error::CredentialError::InsecurePermissions { mode, .. },
                ..
            } => {
                assert_eq!(mode & 0o077, 0o044);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn tight_permissions_pass() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("token.json");
        std::fs::write(&p, "{}").unwrap();
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(&p, perm).unwrap();
        let errs = check_permissions(&[(crate::handle::GOOGLE, "ana".into(), p)]);
        assert!(errs.is_empty(), "got: {errs:#?}");
    }

    #[test]
    fn canonicalize_creates_missing_dirs() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not_yet");
        let (ok, errs) =
            canonicalize_session_dirs(&[claim(crate::handle::WHATSAPP, "x", &missing)]);
        assert!(errs.is_empty(), "got: {errs:#?}");
        assert_eq!(ok.len(), 1);
        assert!(ok[0].path.exists());
    }
}
