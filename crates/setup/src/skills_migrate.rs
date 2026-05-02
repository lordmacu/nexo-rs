//! Phase 83.8.12.6.b — on-disk skills migration helper.
//!
//! Phase 83.8.12.6 changed the skills layout from
//! `<root>/<name>/SKILL.md` to
//! `<root>/{__global__,<tenant_id>}/<name>/SKILL.md`. This helper
//! rewrites a pre-83.8.12.6 skills directory into the new layout
//! by moving every legacy skill (a top-level dir with `SKILL.md`
//! inside) into the `__global__` namespace.
//!
//! The runtime `SkillLoader` (Phase 83.8.12.6.runtime) keeps the
//! legacy path as a fallback, so this migration is OPTIONAL —
//! existing deployments keep working without it. Operators run
//! it once when they want to clean up the layout (and silence
//! the deprecation warning the loader logs on legacy hits).
//!
//! The function is idempotent: if `<root>/__global__/<name>/`
//! already exists for a candidate, the source is left in place
//! and the conflict is reported in the result. Re-running on an
//! already-migrated tree returns `0` moves and no errors.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Outcome summary returned by [`migrate_legacy_skills_to_global`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillsMigrationReport {
    /// Number of legacy `<root>/<name>/` dirs successfully moved
    /// into `<root>/__global__/<name>/`.
    pub moved: usize,
    /// Source dirs that were skipped because the destination
    /// `<root>/__global__/<name>/` already existed. Caller can
    /// inspect / resolve manually.
    pub skipped_conflicts: Vec<String>,
}

/// Move every legacy skill at `<root>/<name>/SKILL.md` into
/// `<root>/__global__/<name>/SKILL.md`. Tenant-scoped layouts
/// (`<root>/<tenant_id>/` dirs that have NO `SKILL.md` directly
/// inside but contain skill subdirs) are detected by absence of
/// the top-level `SKILL.md` and left untouched.
///
/// Returns the migration report. I/O errors propagate via
/// `Err(io::Error)`; partial migration on error is possible —
/// already-moved entries stay in their new location.
pub fn migrate_legacy_skills_to_global(root: &Path) -> io::Result<SkillsMigrationReport> {
    if !root.exists() {
        return Ok(SkillsMigrationReport::default());
    }
    let global_dir = root.join("__global__");
    fs::create_dir_all(&global_dir)?;

    let mut report = SkillsMigrationReport::default();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Skip the destination namespace itself.
        if name == "__global__" {
            continue;
        }
        // A legacy skill dir has SKILL.md directly inside.
        // Anything else (tenant scope dir) we leave alone.
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        let dest = global_dir.join(name);
        if dest.exists() {
            report.skipped_conflicts.push(name.to_string());
            continue;
        }
        rename_or_copy_then_remove(&path, &dest)?;
        report.moved += 1;
    }
    Ok(report)
}

/// Try `fs::rename` first (cheap, atomic on the same filesystem);
/// fall back to copy + remove when rename fails (cross-device
/// links). Both branches end with the source dir gone and the
/// dest dir populated.
fn rename_or_copy_then_remove(src: &Path, dest: &Path) -> io::Result<()> {
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_dir_recursive(src, dest)?;
            fs::remove_dir_all(src)?;
            Ok(())
        }
    }
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else {
            fs::copy(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn make_path<S: AsRef<str>>(parts: &[S]) -> PathBuf {
    let mut p = PathBuf::new();
    for s in parts {
        p.push(s.as_ref());
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, parts: &[&str], body: &str) {
        let dir = parts
            .iter()
            .fold(root.to_path_buf(), |acc, p| acc.join(p));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn moves_legacy_skill_into_global_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), &["weather"], "Legacy weather.");

        let report = migrate_legacy_skills_to_global(tmp.path()).unwrap();
        assert_eq!(report.moved, 1);
        assert!(report.skipped_conflicts.is_empty());

        // Source gone.
        assert!(!tmp.path().join("weather").exists());
        // Destination populated.
        let new = tmp.path().join("__global__").join("weather").join("SKILL.md");
        assert!(new.exists());
        assert_eq!(fs::read_to_string(new).unwrap(), "Legacy weather.");
    }

    #[test]
    fn idempotent_on_already_migrated_tree() {
        let tmp = tempfile::tempdir().unwrap();
        // Already in new layout — pretend admin RPC wrote it.
        write_skill(tmp.path(), &["__global__", "weather"], "Global weather.");

        let report = migrate_legacy_skills_to_global(tmp.path()).unwrap();
        assert_eq!(report.moved, 0);
        assert!(report.skipped_conflicts.is_empty());
        // File untouched.
        let still_there = tmp
            .path()
            .join("__global__")
            .join("weather")
            .join("SKILL.md");
        assert!(still_there.exists());
    }

    #[test]
    fn leaves_tenant_scope_dirs_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        // Tenant-scoped skill: <root>/<tenant_id>/<name>/SKILL.md
        // — the top-level <tenant_id> dir has NO SKILL.md
        // directly inside, so it's not a legacy skill dir.
        write_skill(tmp.path(), &["acme", "weather"], "Acme weather.");

        let report = migrate_legacy_skills_to_global(tmp.path()).unwrap();
        assert_eq!(report.moved, 0);
        assert!(report.skipped_conflicts.is_empty());
        // Still where it was.
        let acme_path = tmp.path().join("acme").join("weather").join("SKILL.md");
        assert!(acme_path.exists());
        // No global dir got created with the wrong content.
        assert!(!tmp
            .path()
            .join("__global__")
            .join("weather")
            .join("SKILL.md")
            .exists());
    }

    #[test]
    fn reports_conflict_when_destination_exists() {
        let tmp = tempfile::tempdir().unwrap();
        // Both layouts — dest already populated (operator did
        // a partial manual move, or two skills with the same
        // name).
        write_skill(tmp.path(), &["__global__", "weather"], "Global weather.");
        write_skill(tmp.path(), &["weather"], "Legacy weather.");

        let report = migrate_legacy_skills_to_global(tmp.path()).unwrap();
        assert_eq!(report.moved, 0);
        assert_eq!(report.skipped_conflicts, vec!["weather".to_string()]);
        // Source untouched (operator must resolve manually).
        assert!(tmp.path().join("weather").join("SKILL.md").exists());
    }

    #[test]
    fn missing_root_returns_empty_report() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("missing");
        let report = migrate_legacy_skills_to_global(&nonexistent).unwrap();
        assert_eq!(report, SkillsMigrationReport::default());
    }

    #[test]
    fn moves_multiple_legacy_skills_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), &["weather"], "w");
        write_skill(tmp.path(), &["finance"], "f");
        write_skill(tmp.path(), &["devops"], "d");

        let report = migrate_legacy_skills_to_global(tmp.path()).unwrap();
        assert_eq!(report.moved, 3);
        for name in ["weather", "finance", "devops"] {
            assert!(tmp.path().join("__global__").join(name).join("SKILL.md").exists());
            assert!(!tmp.path().join(name).exists());
        }
    }
}
