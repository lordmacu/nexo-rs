//! Phase 81.22 — Plugin sandbox manifest schema.
//!
//! Subprocess plugins opt into bubblewrap-based sandboxing via
//! `[plugin.sandbox]`. The host-side runner (`nexo-core`'s
//! `SandboxRunner`) consumes this section to wrap the spawned
//! `Command` with `bwrap` flags. Manifest-side responsibilities
//! are limited to: schema typing, denylist constants, helper
//! predicates. Path-canonicalization-against-denylist + bwrap
//! argv construction live in `nexo-core::agent::plugin_sandbox`.
//!
//! IRROMPIBLE refs:
//! - `research/src/agents/sandbox/validate-sandbox-security.ts:22-48`
//!   — `BLOCKED_HOST_PATHS` + `BLOCKED_HOME_SUBPATHS` denylist
//!   subset reused here. Mirrored 1:1 (with `/etc` narrowed to
//!   the actually-sensitive `/etc/shadow`/`/etc/sudoers*`
//!   subset, since `/etc/ssl/certs` is a common legitimate
//!   read mount and OpenClaw blocked all of `/etc` only because
//!   Docker's bind shape is coarse).
//! - claude-code-leak/: absent — sandbox is OS-process infra,
//!   no LLM-tool surface relevance.

use serde::{Deserialize, Serialize};

/// `[plugin.sandbox]` section. Default = sandbox disabled (every
/// existing manifest parses unchanged, in-tree plugins keep the
/// today behavior of running unsandboxed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SandboxSection {
    /// Master switch. When `false` (default), the host runner
    /// returns the plugin's raw command unchanged — equivalent
    /// to "no sandbox section present".
    #[serde(default)]
    pub enabled: bool,

    /// Network policy. `Deny` adds `--unshare-net` to bwrap; the
    /// child runs in an isolated network namespace with no
    /// interface besides loopback. `Host` skips network isolation
    /// — gated behind operator-side capability
    /// `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW=1`. Granular allowlist
    /// is deferred to 81.22.b.
    #[serde(default)]
    pub network: SandboxNetwork,

    /// Absolute host paths the plugin gets read-only access to
    /// inside the sandbox (`bwrap --ro-bind <p> <p>`). Validator
    /// rejects relative paths and paths equal-or-under the host
    /// denylist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fs_read_paths: Vec<String>,

    /// Absolute host paths the plugin gets read-write access to
    /// (`bwrap --bind <p> <p>`). Supports the literal token
    /// `${state_dir}` which the host expands to the plugin's
    /// per-instance state root (`<state_root>/plugins/<id>`).
    /// Same denylist guards as `fs_read_paths` apply to the
    /// resolved path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fs_write_paths: Vec<String>,

    /// When `true` (default when sandbox enabled), bwrap maps the
    /// child to uid/gid 65534 (`nobody:nogroup`) via
    /// `--unshare-user --uid 65534 --gid 65534`. Plugins doing
    /// `setuid` work get rejected — intentional.
    #[serde(default = "default_drop_user")]
    pub drop_user: bool,
}

fn default_drop_user() -> bool {
    true
}

impl Default for SandboxSection {
    fn default() -> Self {
        Self {
            enabled: false,
            network: SandboxNetwork::default(),
            fs_read_paths: Vec::new(),
            fs_write_paths: Vec::new(),
            drop_user: default_drop_user(),
        }
    }
}

/// Network policy variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxNetwork {
    /// `--unshare-net` — child runs in isolated net namespace.
    /// Default. No outbound traffic possible. Plugin must use
    /// daemon-mediated RPCs (`llm.complete` / `memory.*`) for
    /// any external IO.
    #[default]
    Deny,

    /// Network namespace shared with host. Operator must opt in
    /// via `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW=1` capability.
    Host,
}

/// Path-classification kind threaded into validator violations
/// so error messages can disambiguate `fs_read_paths` from
/// `fs_write_paths` issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxPathKind {
    Read,
    Write,
}

/// Token recognized in `fs_write_paths` entries. Host expands at
/// spawn-time to the plugin's per-instance state root.
pub const SANDBOX_STATE_DIR_TOKEN: &str = "${state_dir}";

/// Hard denylist — host paths that can never be sandbox-mounted,
/// even if the operator's manifest puts them in an allowlist.
/// Compile-time const so operators can't override.
///
/// Tighter than OpenClaw's `BLOCKED_HOST_PATHS` (`research/src/agents/sandbox/validate-sandbox-security.ts:22-37`)
/// in two directions:
/// 1. **Narrower** on `/etc`: `/etc` itself is OK (legitimate
///    plugins read `/etc/ssl/certs`); only the sensitive
///    subset (`/etc/shadow`, `/etc/sudoers*`) is denied.
/// 2. **Broader** on `/proc`: `/proc/sys` denied beyond the
///    `/proc/<pid>/mem` family.
pub const SANDBOX_DENYLIST_HOST_PATHS: &[&str] = &[
    "/etc/shadow",
    "/etc/sudoers",
    "/etc/sudoers.d",
    "/proc/sys",
    "/proc/kcore",
    "/proc/kallsyms",
    "/sys/firmware",
    "/sys/kernel",
    "/dev/mem",
    "/dev/kmem",
    "/dev/port",
    "/var/run/docker.sock",
    "/run/docker.sock",
    "/private/var/run/docker.sock",
    "/root",
    "/boot",
];

/// Hard denylist for `${HOME}`-relative subpaths. Host resolves
/// at validate-time using the daemon's `$HOME` (via `home_dir()`
/// crate or `std::env`). Mirrors OpenClaw's `BLOCKED_HOME_SUBPATHS`
/// (`research/src/agents/sandbox/validate-sandbox-security.ts:39-48`)
/// plus `.cargo/credentials` (cargo registry tokens) and `.kube`
/// (kubeconfig tokens) which OpenClaw didn't list because TS
/// devs aren't usually in those paths.
pub const SANDBOX_DENYLIST_HOME_SUBPATHS: &[&str] = &[
    ".aws",
    ".ssh",
    ".gnupg",
    ".netrc",
    ".docker",
    ".kube",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".npmrc",
    ".config/gh",
    ".config/git",
];

/// `true` when `path` (canonicalized absolute) is equal to or a
/// subpath of any entry in `denylist`. Returns the matched
/// denylist entry on hit (for richer error messages).
///
/// Algorithm mirrors `research/src/agents/sandbox/validate-sandbox-security.ts:140-150`:
/// per denylist entry, accept exact-match OR `path.startsWith(blocked + "/")`.
/// Plus a "path covers root" guard: any literal `/` in the
/// allowlist matches every denylist entry, since `/` is the
/// parent of everything.
pub fn path_under_or_equals_denylist<'a>(path: &str, denylist: &'a [&'a str]) -> Option<&'a str> {
    if path == "/" {
        return Some("/");
    }
    for blocked in denylist {
        if path == *blocked {
            return Some(*blocked);
        }
        // exact prefix with slash — `/etc/shadow` matches
        // `/etc/shadow/foo` but NOT `/etc/shadow_backup`.
        let with_slash = format!("{}/", blocked);
        if path.starts_with(&with_slash) {
            return Some(*blocked);
        }
        // covers-root: `/etc/shadow` is under `/`, so an
        // allowlist of `/` covers every denylist entry.
        if *path == *"/" || (*blocked).starts_with(path) && path != *blocked {
            // The allowlist entry IS a parent of the denylist
            // entry. Reject — operator can't carve a wide
            // bind that swallows a sensitive path.
            let parent_with_slash = format!("{}/", path);
            if blocked.starts_with(&parent_with_slash) {
                return Some(*blocked);
            }
        }
    }
    None
}

/// `true` when `path` is the literal `${state_dir}` token, an
/// extension of it (`${state_dir}/cache`), or contains it
/// anywhere. Used to distinguish read-vs-write allowlist
/// validation: `${state_dir}` only meaningful in `fs_write_paths`.
pub fn contains_state_dir_token(path: &str) -> bool {
    path.contains(SANDBOX_STATE_DIR_TOKEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serde_default_section() {
        let section = SandboxSection::default();
        let toml = toml::to_string(&section).unwrap();
        let parsed: SandboxSection = toml::from_str(&toml).unwrap();
        assert_eq!(section, parsed);
        assert!(!parsed.enabled);
        assert_eq!(parsed.network, SandboxNetwork::Deny);
        assert!(parsed.fs_read_paths.is_empty());
        assert!(parsed.fs_write_paths.is_empty());
        assert!(parsed.drop_user);
    }

    #[test]
    fn round_trip_serde_full_section() {
        let section = SandboxSection {
            enabled: true,
            network: SandboxNetwork::Host,
            fs_read_paths: vec!["/etc/ssl/certs".into()],
            fs_write_paths: vec!["${state_dir}".into(), "/tmp/x".into()],
            drop_user: false,
        };
        let toml = toml::to_string(&section).unwrap();
        let parsed: SandboxSection = toml::from_str(&toml).unwrap();
        assert_eq!(section, parsed);
    }

    #[test]
    fn drop_user_default_is_true() {
        let toml = "enabled = true\nnetwork = \"deny\"\n";
        let parsed: SandboxSection = toml::from_str(toml).unwrap();
        assert!(parsed.drop_user);
    }

    #[test]
    fn denylist_path_exact_match() {
        let hit = path_under_or_equals_denylist("/etc/shadow", SANDBOX_DENYLIST_HOST_PATHS);
        assert_eq!(hit, Some("/etc/shadow"));
    }

    #[test]
    fn denylist_path_under_match() {
        let hit =
            path_under_or_equals_denylist("/etc/sudoers.d/myrule", SANDBOX_DENYLIST_HOST_PATHS);
        assert_eq!(hit, Some("/etc/sudoers.d"));
    }

    #[test]
    fn denylist_root_covers_everything() {
        let hit = path_under_or_equals_denylist("/", SANDBOX_DENYLIST_HOST_PATHS);
        assert_eq!(hit, Some("/"));
    }

    #[test]
    fn denylist_allowlist_parent_swallows_blocked() {
        // Operator tries to allowlist `/etc` — every denylist
        // entry under `/etc` (shadow, sudoers, sudoers.d) is
        // a child of the allowlist entry, so reject.
        let hit = path_under_or_equals_denylist("/etc", SANDBOX_DENYLIST_HOST_PATHS);
        assert!(hit.is_some());
        let matched = hit.unwrap();
        assert!(
            matched == "/etc/shadow" || matched == "/etc/sudoers" || matched == "/etc/sudoers.d",
            "expected an /etc/* denylist entry, got {}",
            matched
        );
    }

    #[test]
    fn denylist_path_unrelated_no_match() {
        let hit = path_under_or_equals_denylist(
            "/usr/share/ca-certificates",
            SANDBOX_DENYLIST_HOST_PATHS,
        );
        assert!(hit.is_none());

        // sibling paths don't match — `/etc/shadow` should not
        // hit on `/etc/shadow_backup`.
        let hit = path_under_or_equals_denylist("/etc/shadow_backup", SANDBOX_DENYLIST_HOST_PATHS);
        assert!(hit.is_none());
    }

    #[test]
    fn state_dir_token_detection() {
        assert!(contains_state_dir_token("${state_dir}"));
        assert!(contains_state_dir_token("${state_dir}/cache"));
        assert!(contains_state_dir_token("/prefix/${state_dir}"));
        assert!(!contains_state_dir_token("/etc/state_dir"));
        assert!(!contains_state_dir_token("$state_dir"));
    }
}
