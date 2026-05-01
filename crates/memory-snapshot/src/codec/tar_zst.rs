//! Pack and unpack the bundle body as `tar` + zstd.
//!
//! These are sync APIs; orchestration code wraps them in
//! `tokio::task::spawn_blocking` so the async runtime is never blocked
//! by the disk + compress path.

use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Compression level applied to every bundle. Mirrors the
/// `nexo-backup.sh` operator script (Phase 36.1) so backup and snapshot
/// converge on one tradeoff.
pub const ZSTD_LEVEL: i32 = 19;

/// One file going into the archive.
pub struct PackEntry<'a> {
    /// Path used inside the tar (e.g. `sqlite/long_term.sqlite`).
    pub path_in_bundle: &'a str,
    pub source: &'a Path,
}

/// Stream a list of files into a `.tar.zst` at `dst`. The caller
/// guarantees `entries` are unique and that `source` paths exist.
pub fn pack_files<W: Write>(entries: &[PackEntry<'_>], out: W) -> io::Result<W> {
    let mut zst = zstd::stream::Encoder::new(out, ZSTD_LEVEL)?;
    {
        let mut tar = tar::Builder::new(&mut zst);
        // Reproducible-ish ordering: caller-supplied order is preserved
        // so the manifest layout matches what verify reconstructs.
        for entry in entries {
            let mut f = fs::File::open(entry.source)?;
            let metadata = f.metadata()?;
            let mut header = tar::Header::new_gnu();
            header.set_size(metadata.len());
            header.set_mode(0o600);
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, entry.path_in_bundle, &mut f)?;
        }
        tar.finish()?;
    }
    // `finish` flushes the zstd footer and returns the inner writer.
    let inner = zst.finish()?;
    Ok(inner)
}

/// Reject paths that would escape the staging dir. Pulled out as a free
/// function so it can be unit-tested without round-tripping through tar
/// (the crate already refuses these on write, but a hostile bundle can
/// still smuggle them through hand-crafted bytes).
pub fn is_safe_bundle_path(p: &Path) -> bool {
    if p.is_absolute() {
        return false;
    }
    !p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Read every entry in `bundle` and write it into `into_dir`,
/// preserving the in-bundle relative path. Returns the list of paths
/// extracted.
pub fn unpack_into<R: Read + Seek>(reader: R, into_dir: &Path) -> io::Result<Vec<PathBuf>> {
    fs::create_dir_all(into_dir)?;
    let dec = zstd::stream::Decoder::new(reader)?;
    let mut tar = tar::Archive::new(dec);
    let mut written = Vec::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let in_bundle = entry.path()?.into_owned();
        if !is_safe_bundle_path(&in_bundle) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle entry escapes staging dir: {}", in_bundle.display()),
            ));
        }
        let dst = into_dir.join(&in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&dst)?;
        io::copy(&mut entry, &mut out)?;
        written.push(dst);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn write_temp(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn round_trip_three_files() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_temp(tmp.path(), "a.txt", b"alpha");
        let b = write_temp(tmp.path(), "b.txt", b"bravo");
        let c = write_temp(tmp.path(), "c.txt", b"charlie");

        let mut out = Vec::new();
        pack_files(
            &[
                PackEntry {
                    path_in_bundle: "memory_files/a.txt",
                    source: &a,
                },
                PackEntry {
                    path_in_bundle: "memory_files/b.txt",
                    source: &b,
                },
                PackEntry {
                    path_in_bundle: "memory_files/c.txt",
                    source: &c,
                },
            ],
            &mut out,
        )
        .unwrap();

        let dst = tempfile::tempdir().unwrap();
        let written = unpack_into(Cursor::new(&out), dst.path()).unwrap();
        assert_eq!(written.len(), 3);

        let read_back = |sub: &str| fs::read(dst.path().join(sub)).unwrap();
        assert_eq!(read_back("memory_files/a.txt"), b"alpha");
        assert_eq!(read_back("memory_files/b.txt"), b"bravo");
        assert_eq!(read_back("memory_files/c.txt"), b"charlie");
    }

    #[test]
    fn empty_archive_round_trips() {
        let mut out = Vec::new();
        pack_files(&[], &mut out).unwrap();
        let dst = tempfile::tempdir().unwrap();
        let written = unpack_into(Cursor::new(&out), dst.path()).unwrap();
        assert!(written.is_empty());
    }

    #[test]
    fn safe_path_predicate_rejects_absolute() {
        assert!(!is_safe_bundle_path(Path::new("/etc/passwd")));
    }

    #[test]
    fn safe_path_predicate_rejects_parent_traversal() {
        assert!(!is_safe_bundle_path(Path::new("../escape.txt")));
        assert!(!is_safe_bundle_path(Path::new("a/../b")));
    }

    #[test]
    fn safe_path_predicate_accepts_relative() {
        assert!(is_safe_bundle_path(Path::new("memory_files/a.md")));
        assert!(is_safe_bundle_path(Path::new("sqlite/long_term.sqlite")));
        assert!(is_safe_bundle_path(Path::new("manifest.json")));
    }

    #[test]
    fn round_trip_compresses_repetitive_payload_below_input_size() {
        let tmp = tempfile::tempdir().unwrap();
        let body = vec![0xab; 64 * 1024];
        let p = write_temp(tmp.path(), "repeat.bin", &body);
        let mut out = Vec::new();
        pack_files(
            &[PackEntry {
                path_in_bundle: "memory_files/repeat.bin",
                source: &p,
            }],
            &mut out,
        )
        .unwrap();
        // zstd-19 on 64 KiB of identical bytes lands well under 1 KiB.
        assert!(
            out.len() < body.len() / 4,
            "compressed {} vs raw {}",
            out.len(),
            body.len()
        );
    }
}
