//! Per-agent path discovery for the snapshotter.
//!
//! By default `LocalFsSnapshotter` resolves an agent's memdir as
//! `<memdir_root>/<agent_id>` and its SQLite store as
//! `<sqlite_root>/<agent_id>` — the operator-supplied globals from
//! YAML. That is correct for single-tenant deployments where every
//! agent shares the same memory layout.
//!
//! Multi-tenant SaaS (Phase 82) breaks the symmetry: each agent's
//! memdir typically lives under its own workspace
//! (`agent_cfg.workspace/.git/`), and per-agent SQLite databases may
//! sit under tenant-scoped state directories. The trait below lets
//! the boot wire override the default lookup so the snapshotter
//! captures the right files for each agent without requiring a YAML
//! field per agent.
//!
//! Provider-agnostic by construction: the resolver knows nothing
//! about LLM providers, brokers, or session stores. It is a pure
//! `(agent_id, tenant) → paths` function.

use std::path::PathBuf;

/// Strategy for mapping an agent identity to its on-disk memory
/// layout. Implementations must be deterministic (the same input
/// always yields the same output) and safe to call from any thread.
pub trait PathResolver: Send + Sync + 'static {
    /// Where the agent's git-backed memory directory lives. The
    /// snapshotter reads `.git/**` and any non-`.git` regular files
    /// under this path.
    fn memdir(&self, agent_id: &str, tenant: &str) -> PathBuf;

    /// Where the agent's SQLite stores live. The snapshotter expects
    /// `long_term.sqlite`, `vector.sqlite`, `concepts.sqlite`, and
    /// `compactions.sqlite` directly under this path; missing files
    /// are simply skipped.
    fn sqlite_dir(&self, agent_id: &str, tenant: &str) -> PathBuf;
}

/// Default impl: `<memdir_root>/<agent_id>` and
/// `<sqlite_root>/<agent_id>` — the layout the YAML config assumes
/// when no operator override is supplied. Used by the `Builder` when
/// the boot wire does not inject a richer resolver.
#[derive(Debug, Clone)]
pub struct DefaultPathResolver {
    memdir_root: PathBuf,
    sqlite_root: PathBuf,
}

impl DefaultPathResolver {
    pub fn new(memdir_root: PathBuf, sqlite_root: PathBuf) -> Self {
        Self {
            memdir_root,
            sqlite_root,
        }
    }
}

impl PathResolver for DefaultPathResolver {
    fn memdir(&self, agent_id: &str, _tenant: &str) -> PathBuf {
        self.memdir_root.join(agent_id)
    }

    fn sqlite_dir(&self, agent_id: &str, _tenant: &str) -> PathBuf {
        self.sqlite_root.join(agent_id)
    }
}

/// Builder helper: a closure-backed resolver. Useful at the boot
/// wire when the lookup needs to consult an existing struct
/// (e.g. an agent registry) without forcing the caller to define a
/// dedicated type.
pub struct ClosureResolver<F1, F2>
where
    F1: Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
    F2: Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
{
    memdir_fn: F1,
    sqlite_fn: F2,
}

impl<F1, F2> ClosureResolver<F1, F2>
where
    F1: Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
    F2: Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
{
    pub fn new(memdir_fn: F1, sqlite_fn: F2) -> Self {
        Self {
            memdir_fn,
            sqlite_fn,
        }
    }
}

impl<F1, F2> PathResolver for ClosureResolver<F1, F2>
where
    F1: Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
    F2: Fn(&str, &str) -> PathBuf + Send + Sync + 'static,
{
    fn memdir(&self, agent_id: &str, tenant: &str) -> PathBuf {
        (self.memdir_fn)(agent_id, tenant)
    }

    fn sqlite_dir(&self, agent_id: &str, tenant: &str) -> PathBuf {
        (self.sqlite_fn)(agent_id, tenant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn default_resolver_joins_agent_id_under_root() {
        let r = DefaultPathResolver::new(
            PathBuf::from("/var/lib/memdir"),
            PathBuf::from("/var/lib/sqlite"),
        );
        assert_eq!(r.memdir("ana", "default"), PathBuf::from("/var/lib/memdir/ana"));
        assert_eq!(
            r.sqlite_dir("ana", "default"),
            PathBuf::from("/var/lib/sqlite/ana")
        );
    }

    #[test]
    fn default_resolver_ignores_tenant_for_paths() {
        // Single-tenant fallback never branches on tenant — it falls
        // through whatever the operator set in YAML.
        let r = DefaultPathResolver::new(
            PathBuf::from("/x"),
            PathBuf::from("/y"),
        );
        assert_eq!(r.memdir("ana", "acme"), r.memdir("ana", "globex"));
    }

    #[test]
    fn closure_resolver_routes_per_tenant() {
        let r = ClosureResolver::new(
            |agent: &str, tenant: &str| {
                PathBuf::from(format!("/var/{tenant}/memdir/{agent}"))
            },
            |agent: &str, tenant: &str| {
                PathBuf::from(format!("/var/{tenant}/sqlite/{agent}"))
            },
        );
        assert_eq!(
            r.memdir("ana", "acme"),
            PathBuf::from("/var/acme/memdir/ana")
        );
        assert_eq!(
            r.memdir("ana", "globex"),
            PathBuf::from("/var/globex/memdir/ana")
        );
    }

    #[test]
    fn dyn_path_resolver_can_be_held_as_arc() {
        let r: Arc<dyn PathResolver> = Arc::new(DefaultPathResolver::new(
            PathBuf::from("/a"),
            PathBuf::from("/b"),
        ));
        assert_eq!(r.memdir("x", "default"), PathBuf::from("/a/x"));
    }
}
