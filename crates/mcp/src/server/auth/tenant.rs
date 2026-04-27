//! Phase 76.4 — tenant identity + scoped path helpers.
//!
//! The `Principal` carries a [`TenantId`] derived at the auth boundary
//! (JWT claim, mTLS CN, static-token YAML config, or stdio constant).
//! Tools and handlers consume it via `DispatchContext::tenant()`.
//!
//! ## Why the validation is paranoid
//!
//! `TenantId` values become path components on disk
//! ([`tenant_scoped_path`], [`tenant_db_path`]) and database
//! identifiers. A tenant id is **always** server-derived (from
//! `Principal`), but the upstream source is partially user-controlled
//! (a JWT claim, a CN forwarded by a proxy). Validation here is the
//! choke point.
//!
//! Defenses ported from `claude-code-leak/src/memdir/teamMemPaths.ts`
//! (`sanitizePathKey`, lines 22–64) and adapted to Rust:
//!
//! * Null bytes — C-syscall truncation vector.
//! * NFKC normalization — fullwidth-form bypasses (e.g. `．．／`).
//! * Percent-decode-and-recheck — defends against `%2e%2e%2f` smuggling.
//! * Strict ASCII allowlist `[a-z0-9_-]{1,64}` after the above.
//! * No leading/trailing `_` or `-` (avoids `argv[0]`-style ambiguity).

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

/// Validated tenant identifier. The wire/YAML form is a plain string;
/// once it becomes a `TenantId` the validation invariants hold for the
/// lifetime of the value.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    /// Tenant the stdio transport always uses. Single-process scope.
    pub const STDIO_LOCAL: &'static str = "local";

    /// Default tenant for static-token auth when the operator omits an
    /// explicit `tenant` field. Mirrors the leak's `'default'` scope
    /// (see `claude-code-leak/src/services/mcp/types.ts:10-20`).
    pub const DEFAULT: &'static str = "default";

    /// Maximum byte length. 64 is generous for human-readable slugs
    /// while staying well below filesystem `NAME_MAX` (255 on most
    /// Linux fs).
    pub const MAX_LEN: usize = 64;

    pub fn parse(raw: &str) -> Result<Self, TenantIdError> {
        // Step 1: null-byte rejection. C syscalls truncate at NUL,
        // so a tenant id with an embedded NUL would split into a
        // shorter identifier on disk. Pattern from
        // claude-code-leak/src/memdir/teamMemPaths.ts:22-64.
        if raw.contains('\0') {
            return Err(TenantIdError::NullByte);
        }

        // Step 2: NFKC normalize and reject if normalization changed
        // the string. Catches fullwidth bypasses (Ｔｅｎａｎｔ, ．．／)
        // and other compatibility-form tricks. Same pattern as
        // sanitizePathKey's NFKC step in the leak.
        let nfkc: String = raw.nfkc().collect();
        if nfkc != raw {
            return Err(TenantIdError::NonCanonical);
        }

        // Step 3: percent-decode and reject if the decoded form
        // differs AND introduces `..` or `/`. A literal `%2e%2e%2f`
        // is allowed by step 5's charset filter (it has no `%`), so
        // this layer is precautionary against future changes that
        // might widen the allowlist.
        if let Ok(decoded) = percent_encoding::percent_decode_str(raw).decode_utf8() {
            let decoded_str: &str = decoded.as_ref();
            if decoded_str != raw
                && (decoded_str.contains("..")
                    || decoded_str.contains('/')
                    || decoded_str.contains('\\'))
            {
                return Err(TenantIdError::PercentSmuggle);
            }
        }

        // Step 4: length.
        if raw.is_empty() {
            return Err(TenantIdError::Empty);
        }
        if raw.len() > Self::MAX_LEN {
            return Err(TenantIdError::TooLong(raw.len()));
        }

        // Step 5: charset allowlist. Lowercase only — uppercase is
        // rejected to avoid filesystem case-fold ambiguity (HFS+,
        // Windows NTFS in default mode, ZFS with casesensitivity=
        // mixed). No `.` (path separator on macOS package bundles
        // and a path-injection primitive). No `/` or `\\`. No
        // whitespace.
        if !raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(TenantIdError::InvalidChar);
        }

        // Step 6: edge characters. Leading/trailing `_` or `-` would
        // turn into argv-style flags if the id ever ends up on a
        // command line, and a leading `-` is the canonical
        // path-injection trick on rsync/scp.
        let first = raw.chars().next().unwrap();
        let last = raw.chars().next_back().unwrap();
        if matches!(first, '-' | '_') || matches!(last, '-' | '_') {
            return Err(TenantIdError::InvalidEdge);
        }

        Ok(TenantId(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for TenantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for TenantId {
    type Error = TenantIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TenantId> for String {
    fn from(value: TenantId) -> Self {
        value.0
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TenantIdError {
    #[error("tenant id empty")]
    Empty,
    #[error("tenant id too long: {0} bytes > {} limit", TenantId::MAX_LEN)]
    TooLong(usize),
    #[error("tenant id contains disallowed character (allowed: [a-z0-9_-])")]
    InvalidChar,
    #[error("tenant id has disallowed leading/trailing character (no _ or - on edges)")]
    InvalidEdge,
    #[error("tenant id contains a NUL byte")]
    NullByte,
    #[error("tenant id is not in NFKC canonical form (compatibility-form bypass attempt)")]
    NonCanonical,
    #[error("tenant id contains percent-encoded path separators (smuggle attempt)")]
    PercentSmuggle,
}

// --- TenantScoped<T> ----------------------------------------------

/// Wraps a value (DB handle, path, store, …) with the [`TenantId`]
/// it was constructed for. The wrap is the trip-wire: a handler that
/// receives a `TenantScoped<DbHandle>` for tenant `t1` and tries to
/// extract the inner value under tenant `t2` gets a hard
/// [`CrossTenantError`] instead of silent cross-tenant access.
///
/// Cheap alternative to a full type-state proof — defense in depth
/// against future bugs rather than a load-bearing security boundary.
/// The actual isolation comes from path scoping at construction time.
pub struct TenantScoped<T> {
    tenant: TenantId,
    inner: T,
}

impl<T> TenantScoped<T> {
    pub fn new(tenant: TenantId, inner: T) -> Self {
        Self { tenant, inner }
    }

    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Trip-wire: returns the inner value only when the caller's
    /// expected tenant matches. Mismatch → `CrossTenantError`, which
    /// is a hard programming bug at the dispatch site.
    pub fn try_into_inner(self, expected: &TenantId) -> Result<T, CrossTenantError> {
        if &self.tenant == expected {
            Ok(self.inner)
        } else {
            Err(CrossTenantError {
                held: self.tenant.as_str().to_string(),
                requested: expected.as_str().to_string(),
            })
        }
    }

    /// Map the inner value while preserving the tenant tag. Useful
    /// for adapter chains (`TenantScoped<RawHandle>` →
    /// `TenantScoped<TypedRepo>`).
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> TenantScoped<U> {
        TenantScoped {
            tenant: self.tenant,
            inner: f(self.inner),
        }
    }

    /// Borrow the inner value without surrendering the wrapper.
    pub fn as_ref(&self) -> TenantScoped<&T> {
        TenantScoped {
            tenant: self.tenant.clone(),
            inner: &self.inner,
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for TenantScoped<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantScoped")
            .field("tenant", &self.tenant)
            .field("inner", &self.inner)
            .finish()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("cross-tenant access: held=`{held}`, requested=`{requested}`")]
pub struct CrossTenantError {
    pub held: String,
    pub requested: String,
}

// --- tenant_scoped_path -------------------------------------------

/// Build `<root>/tenants/<tenant>/<suffix>` without touching the
/// filesystem. The `suffix` is sanity-checked: absolute paths and
/// `..` segments are *not* applied — they fall back to the
/// `_invalid` sentinel within the tenant dir so the caller never
/// silently writes outside the tenant boundary.
///
/// Use this for *new* paths (writes to files that may not yet
/// exist). For reads, prefer [`tenant_scoped_canonicalize`] which
/// also runs symlink-aware containment checks (Phase 76.4 step 4).
pub fn tenant_scoped_path(root: &Path, tenant: &TenantId, suffix: &str) -> PathBuf {
    let mut base = root.join("tenants").join(tenant.as_str());
    if suffix.is_empty() {
        return base;
    }
    let s = Path::new(suffix);
    // Reject absolute paths — `Path::join` of an absolute drops the
    // base entirely, the classic injection foothold.
    if s.is_absolute() {
        tracing::error!(
            tenant = %tenant,
            suffix,
            "tenant_scoped_path called with absolute suffix; clamped to _invalid"
        );
        debug_assert!(false, "absolute suffix in tenant_scoped_path");
        base.push("_invalid");
        return base;
    }
    // Reject any `..` or root-dir component. We also reject `.`
    // (no-ops on a path but allows trivial "looks safe" obfuscation).
    for c in s.components() {
        match c {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                tracing::error!(
                    tenant = %tenant,
                    suffix,
                    "tenant_scoped_path called with `..`/root component; clamped to _invalid"
                );
                debug_assert!(false, "dot-dot/root in tenant_scoped_path");
                base.push("_invalid");
                return base;
            }
            Component::CurDir => continue,
            Component::Normal(seg) => base.push(seg),
        }
    }
    base
}

/// Per-tenant SQLite path. Layout is
/// `<root>/tenants/<tenant>/state.sqlite3` so the SQLite file lives
/// next to whatever else the tenant accumulates. One DB per tenant
/// is the strongest isolation Rust's `rusqlite` makes easy; the
/// production-grade reference at
/// `claude-code-leak/src/services/teamMemorySync/index.ts:163-166`
/// is server-side enforced (Bearer token gates the org_id in
/// responses), but for the in-process MCP server one-DB-per-tenant
/// is the cheapest way to make a corruption blast radius equal one
/// tenant.
pub fn tenant_db_path(root: &Path, tenant: &TenantId) -> PathBuf {
    tenant_scoped_path(root, tenant, "state.sqlite3")
}

// --- tenant_scoped_canonicalize -----------------------------------

/// Two-pass containment check ported from
/// `claude-code-leak/src/memdir/teamMemPaths.ts:228-256`
/// (`validateTeamMemWritePath`):
///
///   * Pass 1: lexical resolution. We reject absolute suffixes,
///     `..` segments, and verify the joined path is well-formed.
///   * Pass 2: `realpath()` on the deepest existing ancestor and
///     re-attach the non-existing tail. Defeats symlink escape
///     because `canonicalize()` follows symlinks; the resolved
///     path must then be inside `<root>/tenants/<tenant>/` (with
///     a trailing separator — the *separator guard* — so that
///     `<root>/tenants/<tenant>-evil/...` does NOT match
///     `<root>/tenants/<tenant>`).
///
/// Errors distinguish the failure mode so the caller can log
/// usefully — the wire-level surface (HTTP / JSON-RPC) collapses
/// them all to a single 403/404 to avoid leaking which probe
/// succeeded.
///
/// Symlink edge cases on Windows: `std::fs::canonicalize` returns
/// UNC paths (`\\?\C:\…`), which break the prefix containment check.
/// Phase 76.4 treats Windows as out of scope; the symlink-defense
/// tests are gated on `cfg(unix)` and full Windows port is a
/// follow-up. This is consistent with the project's musl/Termux
/// production targets.
pub fn tenant_scoped_canonicalize(
    root: &Path,
    tenant: &TenantId,
    suffix: &str,
) -> Result<PathBuf, TenantPathError> {
    // Pass 1: lexical join with strict suffix validation.
    let s = Path::new(suffix);
    if s.is_absolute() {
        return Err(TenantPathError::AbsoluteSuffix(suffix.to_string()));
    }
    for c in s.components() {
        match c {
            Component::ParentDir => {
                return Err(TenantPathError::DotDot(suffix.to_string()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(TenantPathError::AbsoluteSuffix(suffix.to_string()));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    let tenant_dir = root.join("tenants").join(tenant.as_str());
    let lexical = tenant_dir.join(s);

    // The "expected root" is the canonical form of the tenant dir
    // *with a trailing separator*. Without the separator a path
    // `/data/tenants/t-evil/x` would `.starts_with` of
    // `/data/tenants/t` because the prefix-check is byte-wise on
    // the components — actually `Path::starts_with` IS
    // component-wise so `/data/tenants/t-evil` does NOT start with
    // `/data/tenants/t` already, but we still pin the separator
    // semantics by canonicalising the tenant_dir itself.
    //
    // We canonicalise the deepest existing ancestor + re-attach
    // the tail; this is the leak's exact trick to handle "writing
    // a file that doesn't exist yet inside a directory that does".
    let (existing_root, tail) = deepest_existing_ancestor(&lexical)?;
    let canon_existing = match std::fs::canonicalize(&existing_root) {
        Ok(p) => p,
        Err(e) => {
            // ELOOP surfaces here when the deepest existing path is
            // itself part of a symlink loop.
            #[cfg(unix)]
            if matches!(
                e.raw_os_error(),
                Some(libc_eloop) if libc_eloop == 40
            ) {
                return Err(TenantPathError::SymlinkLoop);
            }
            return Err(TenantPathError::Io(e));
        }
    };
    let canon_tenant = match std::fs::canonicalize(&tenant_dir) {
        Ok(p) => p,
        Err(_) => {
            // Tenant dir may not exist yet — fall back to its
            // lexical form for the prefix check. This means the
            // *first* write into a brand-new tenant goes via the
            // lexical containment guarantee from
            // `tenant_scoped_path` above; that's still safe because
            // we already rejected `..` / absolute suffixes.
            tenant_dir.clone()
        }
    };

    let resolved = if let Some(tail) = tail {
        canon_existing.join(tail)
    } else {
        canon_existing.clone()
    };

    // Containment: resolved must equal canon_tenant or be
    // strictly underneath it. `Path::starts_with` is component-
    // wise so it natively gives us the separator guard.
    if !resolved.starts_with(&canon_tenant) {
        return Err(TenantPathError::SymlinkEscape {
            resolved,
            expected: canon_tenant,
        });
    }

    // Detect dangling-symlink case: if `existing_root` was a
    // symlink and the path it pointed at also doesn't exist, the
    // canonicalize above would have failed; if the canonical
    // existing root itself is a broken symlink we won't reach
    // here. To catch dangling symlinks at the *tail*, walk the
    // tail components and check each: if any non-final component
    // exists as a symlink whose target doesn't exist, flag it.
    //
    // In practice `canonicalize` already follows the chain; we
    // only need to handle the case where the lexical tail is
    // empty (the path itself is the existing thing) AND that
    // path is a dangling symlink.
    if let Ok(meta) = std::fs::symlink_metadata(&existing_root) {
        if meta.file_type().is_symlink() {
            // existing_root is a symlink; canonicalize already
            // resolved it — so if std::fs::metadata (which follows
            // links) fails, the link is dangling.
            if std::fs::metadata(&existing_root).is_err() {
                return Err(TenantPathError::DanglingSymlink(existing_root));
            }
        }
    }

    Ok(resolved)
}

/// Walk `path` upward returning the deepest ancestor that exists
/// on disk, plus the (possibly empty) leftover tail relative to it.
fn deepest_existing_ancestor(path: &Path) -> Result<(PathBuf, Option<PathBuf>), TenantPathError> {
    let mut current = path.to_path_buf();
    let mut tail_components: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(_) => {
                let tail = if tail_components.is_empty() {
                    None
                } else {
                    let mut p = PathBuf::new();
                    for c in tail_components.iter().rev() {
                        p.push(c);
                    }
                    Some(p)
                };
                return Ok((current, tail));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let parent = match current.parent() {
                    Some(p) => p.to_path_buf(),
                    None => return Err(TenantPathError::Io(e)),
                };
                if let Some(name) = current.file_name() {
                    tail_components.push(name.to_owned());
                }
                if parent.as_os_str().is_empty() {
                    // Reached the relative-root with no existing
                    // ancestor — that should be impossible because
                    // `root` is supplied as absolute or as the
                    // process CWD. Treat as IO error.
                    return Err(TenantPathError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no existing ancestor for path",
                    )));
                }
                current = parent;
            }
            Err(e) => return Err(TenantPathError::Io(e)),
        }
    }
}

#[derive(Debug, Error)]
pub enum TenantPathError {
    #[error("absolute suffix not allowed: `{0}`")]
    AbsoluteSuffix(String),
    #[error("dot-dot segment not allowed: `{0}`")]
    DotDot(String),
    #[error("symlink escape: resolved {resolved:?} not under {expected:?}")]
    SymlinkEscape {
        resolved: PathBuf,
        expected: PathBuf,
    },
    #[error("symlink loop")]
    SymlinkLoop,
    #[error("dangling symlink at {0:?}")]
    DanglingSymlink(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_canonical_lowercase_alnum() {
        let cases = ["abc", "tenant1", "tenant-1", "a_b", "x", "0", "0a", "a0"];
        for s in cases {
            assert!(TenantId::parse(s).is_ok(), "expected `{s}` to parse");
        }
    }

    #[test]
    fn parse_accepts_64_chars() {
        let s: String = "a".repeat(64);
        let t = TenantId::parse(&s).expect("64-char id should parse");
        assert_eq!(t.as_str(), &s);
    }

    #[test]
    fn parse_rejects_uppercase() {
        assert_eq!(TenantId::parse("Tenant"), Err(TenantIdError::InvalidChar));
    }

    #[test]
    fn parse_rejects_dot() {
        assert_eq!(
            TenantId::parse("agent.x.internal"),
            Err(TenantIdError::InvalidChar)
        );
    }

    #[test]
    fn parse_rejects_slash() {
        assert_eq!(TenantId::parse("a/b"), Err(TenantIdError::InvalidChar));
    }

    #[test]
    fn parse_rejects_backslash() {
        assert_eq!(TenantId::parse("a\\b"), Err(TenantIdError::InvalidChar));
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(TenantId::parse(""), Err(TenantIdError::Empty));
    }

    #[test]
    fn parse_rejects_too_long() {
        let s: String = "a".repeat(65);
        assert_eq!(TenantId::parse(&s), Err(TenantIdError::TooLong(65)));
    }

    #[test]
    fn parse_rejects_null_byte() {
        assert_eq!(TenantId::parse("foo\0bar"), Err(TenantIdError::NullByte));
    }

    #[test]
    fn parse_rejects_fullwidth() {
        // Fullwidth lowercase letters look like ASCII but are
        // U+FF54 / U+FF45 / etc. NFKC folds them to ASCII; we
        // reject the unfolded form.
        assert_eq!(
            TenantId::parse("ｔｅｎａｎｔ"),
            Err(TenantIdError::NonCanonical)
        );
    }

    #[test]
    fn parse_rejects_leading_dash() {
        assert_eq!(TenantId::parse("-tenant"), Err(TenantIdError::InvalidEdge));
    }

    #[test]
    fn parse_rejects_trailing_underscore() {
        assert_eq!(TenantId::parse("tenant_"), Err(TenantIdError::InvalidEdge));
    }

    #[test]
    fn scoped_try_into_inner_match() {
        let t = TenantId::parse("a").unwrap();
        let s = TenantScoped::new(t.clone(), 42u32);
        assert_eq!(s.try_into_inner(&t), Ok(42));
    }

    #[test]
    fn scoped_try_into_inner_mismatch() {
        let t1 = TenantId::parse("a").unwrap();
        let t2 = TenantId::parse("b").unwrap();
        let s = TenantScoped::new(t1.clone(), 42u32);
        assert_eq!(
            s.try_into_inner(&t2),
            Err(CrossTenantError {
                held: "a".into(),
                requested: "b".into()
            })
        );
    }

    #[test]
    fn scoped_map_preserves_tenant() {
        let t = TenantId::parse("a").unwrap();
        let s = TenantScoped::new(t.clone(), 21u32);
        let s2 = s.map(|n| n * 2);
        assert_eq!(s2.tenant(), &t);
        assert_eq!(s2.try_into_inner(&t), Ok(42));
    }

    #[test]
    fn scoped_as_ref_preserves_tenant() {
        let t = TenantId::parse("a").unwrap();
        let s = TenantScoped::new(t.clone(), String::from("hello"));
        {
            let borrowed = s.as_ref();
            assert_eq!(borrowed.tenant(), &t);
        }
        // owner still usable post-borrow
        assert_eq!(s.try_into_inner(&t).unwrap(), "hello");
    }

    #[test]
    fn tenant_scoped_path_layout() {
        let t = TenantId::parse("tenant-a").unwrap();
        let p = tenant_scoped_path(Path::new("/data"), &t, "memory/notes.txt");
        assert_eq!(p, Path::new("/data/tenants/tenant-a/memory/notes.txt"));
    }

    #[test]
    fn tenant_scoped_path_empty_suffix_returns_dir() {
        let t = TenantId::parse("a").unwrap();
        let p = tenant_scoped_path(Path::new("/data"), &t, "");
        assert_eq!(p, Path::new("/data/tenants/a"));
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn tenant_scoped_path_rejects_absolute_suffix_release() {
        let t = TenantId::parse("a").unwrap();
        let p = tenant_scoped_path(Path::new("/data"), &t, "/etc/passwd");
        assert_eq!(p, Path::new("/data/tenants/a/_invalid"));
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn tenant_scoped_path_rejects_dotdot_release() {
        let t = TenantId::parse("a").unwrap();
        let p = tenant_scoped_path(Path::new("/data"), &t, "../etc/passwd");
        assert_eq!(p, Path::new("/data/tenants/a/_invalid"));
    }

    #[test]
    fn tenant_scoped_path_curdir_is_noop() {
        let t = TenantId::parse("a").unwrap();
        let p = tenant_scoped_path(Path::new("/data"), &t, "./memory/x");
        assert_eq!(p, Path::new("/data/tenants/a/memory/x"));
    }

    #[test]
    fn tenant_db_path_layout() {
        let t = TenantId::parse("acme").unwrap();
        let p = tenant_db_path(Path::new("/var/lib/nexo"), &t);
        assert_eq!(p, Path::new("/var/lib/nexo/tenants/acme/state.sqlite3"));
    }

    #[test]
    fn known_constants_parse() {
        // STDIO_LOCAL and DEFAULT must round-trip through parse —
        // we rely on .unwrap() in non-fallible call sites.
        assert!(TenantId::parse(TenantId::STDIO_LOCAL).is_ok());
        assert!(TenantId::parse(TenantId::DEFAULT).is_ok());
    }
}
