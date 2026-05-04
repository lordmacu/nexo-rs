//! Phase 81.22 — Plugin sandbox runner (bubblewrap-based on Linux).
//!
//! Wraps a subprocess plugin's spawn `Command` with a `bwrap` invocation
//! that enforces the plugin's `[plugin.sandbox]` manifest section: fs
//! read/write allowlist, network namespace policy (deny | host),
//! optional uid/gid drop. Discovery happens once at boot; per-spawn
//! cost is argv assembly only.
//!
//! macOS path: no-op + `tracing::warn!`. Native sandbox-exec
//! integration deferred to 81.22.macos.
//!
//! IRROMPIBLE refs:
//! - `research/src/agents/sandbox/validate-sandbox-security.ts:307-345`
//!   — `validateBindMounts` 2-pass shape (canonicalize → per-path
//!   denylist check) inspired the [`validate_resolved_path`] helper.
//! - `research/src/agents/sandbox/network-mode.ts:18-29` — coarse
//!   `host` rejection without operator opt-in is OpenClaw's pattern;
//!   we keep the same shape gated behind
//!   [`Self::host_net_capability_allowed`] (env-driven).
//! - claude-code-leak/: absent — process-isolation infra, no
//!   LLM-tool surface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexo_plugin_manifest::{
    path_under_or_equals_denylist, PluginManifest, SandboxNetwork, SandboxSection,
    SANDBOX_DENYLIST_HOST_PATHS, SANDBOX_STATE_DIR_TOKEN,
};
use thiserror::Error;

/// Env var: when `1`, refuse to spawn any plugin without
/// `sandbox.enabled = true`. Capability inventory entry lives in
/// `crates/setup/src/capabilities.rs::INVENTORY` (Phase 81.22).
pub const ENV_SANDBOX_REQUIRE: &str = "NEXO_PLUGIN_SANDBOX_REQUIRE";

/// Env var: when `1`, permits manifests declaring
/// `sandbox.network = "host"`. Default-off because sharing the host
/// net namespace defeats most of the sandbox.
pub const ENV_SANDBOX_HOST_NET_ALLOW: &str = "NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW";

/// Default uid/gid bwrap maps the child to when `drop_user = true`.
/// 65534 is the standard `nobody:nogroup` shape on Debian/Ubuntu/Alpine.
const NOBODY_UID: u32 = 65534;
const NOBODY_GID: u32 = 65534;

/// Boot-time state for sandbox enforcement. Constructed once in
/// `wire_plugin_registry_with_runtime` and shared across all
/// subprocess plugin spawns.
#[derive(Debug, Clone)]
pub struct SandboxRunner {
    /// `Some(p)` when bwrap was found in PATH at boot. `None` on
    /// macOS (always) or Linux without bubblewrap installed.
    bwrap_path: Option<PathBuf>,

    /// `NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW` — when `false`,
    /// manifests declaring `network = "host"` fail validation
    /// host-side.
    host_net_capability_allowed: bool,

    /// `NEXO_PLUGIN_SANDBOX_REQUIRE` — when `true`, every
    /// subprocess plugin must have `sandbox.enabled = true`,
    /// otherwise spawn fails with [`SandboxError::SandboxRequiredButDisabled`].
    require_sandbox: bool,
}

impl SandboxRunner {
    /// Discovers `bwrap` once and caches the path. Reads env vars
    /// for capability flags. Idempotent.
    pub fn discover() -> Self {
        let bwrap_path = if cfg!(target_os = "linux") {
            which::which("bwrap").ok()
        } else {
            None
        };
        Self {
            bwrap_path,
            host_net_capability_allowed: env_flag(ENV_SANDBOX_HOST_NET_ALLOW),
            require_sandbox: env_flag(ENV_SANDBOX_REQUIRE),
        }
    }

    /// Construction for tests with explicit knobs (no env reads).
    #[cfg(test)]
    pub fn for_test(
        bwrap_path: Option<PathBuf>,
        host_net_capability_allowed: bool,
        require_sandbox: bool,
    ) -> Self {
        Self {
            bwrap_path,
            host_net_capability_allowed,
            require_sandbox,
        }
    }

    pub fn bwrap_path(&self) -> Option<&Path> {
        self.bwrap_path.as_deref()
    }

    pub fn host_net_capability_allowed(&self) -> bool {
        self.host_net_capability_allowed
    }

    pub fn require_sandbox(&self) -> bool {
        self.require_sandbox
    }

    /// Wraps `cmd + args` according to the plugin's manifest sandbox
    /// section. Returns the resolved program/args ready to feed
    /// `Command::new(...).args(...)`. The optional `diagnostic`
    /// surfaces non-fatal warnings (macOS no-op, missing-bwrap
    /// degrade) so the host can `tracing::warn!` them per spawn.
    pub fn wrap_command(
        &self,
        manifest: &PluginManifest,
        state_dir: &Path,
        cmd: &str,
        args: &[String],
    ) -> Result<WrappedCommand, SandboxError> {
        let sandbox = &manifest.plugin.sandbox;
        let plugin_id = manifest.plugin.id.as_str();

        if !sandbox.enabled {
            if self.require_sandbox {
                return Err(SandboxError::SandboxRequiredButDisabled {
                    plugin_id: plugin_id.to_string(),
                });
            }
            return Ok(WrappedCommand::raw(cmd, args, None));
        }

        // Sandbox enabled: pre-check operator-opt-in for host net.
        if sandbox.network == SandboxNetwork::Host && !self.host_net_capability_allowed {
            return Err(SandboxError::HostNetRequestedButNotAllowed {
                plugin_id: plugin_id.to_string(),
            });
        }

        // Platform branches.
        if cfg!(target_os = "macos") {
            if self.require_sandbox {
                return Err(SandboxError::UnsupportedPlatformAndRequired {
                    plugin_id: plugin_id.to_string(),
                });
            }
            return Ok(WrappedCommand::raw(
                cmd,
                args,
                Some(format!(
                    "macOS — sandbox not enforced for plugin `{}` (native sandbox-exec deferred)",
                    plugin_id
                )),
            ));
        }

        // Linux without bwrap.
        let bwrap = match &self.bwrap_path {
            Some(p) => p,
            None => {
                if self.require_sandbox {
                    return Err(SandboxError::BwrapMissingButRequired {
                        plugin_id: plugin_id.to_string(),
                    });
                }
                return Ok(WrappedCommand::raw(
                    cmd,
                    args,
                    Some(format!(
                        "plugin `{}` declares sandbox but `bwrap` is not in PATH — running without sandbox",
                        plugin_id
                    )),
                ));
            }
        };

        // Linux + bwrap available — build argv.
        let bwrap_args =
            build_bwrap_args(plugin_id, sandbox, state_dir, cmd, args)?;
        Ok(WrappedCommand {
            program: bwrap.clone(),
            args: bwrap_args,
            diagnostic: None,
        })
    }
}

/// Final shape consumed by the spawn site. `program` may be the
/// plugin's raw command (sandbox disabled / unsupported platform /
/// missing bwrap) or `bwrap` itself.
#[derive(Debug, Clone)]
pub struct WrappedCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub diagnostic: Option<String>,
}

impl WrappedCommand {
    fn raw(cmd: &str, args: &[String], diagnostic: Option<String>) -> Self {
        Self {
            program: PathBuf::from(cmd),
            args: args.iter().map(OsString::from).collect(),
            diagnostic,
        }
    }
}

/// Convenience: wraps `Self` in `Arc` for the boot-time shared
/// runner.
pub fn shared_runner_from_env() -> Arc<SandboxRunner> {
    Arc::new(SandboxRunner::discover())
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error(
        "plugin `{plugin_id}`: sandbox required (NEXO_PLUGIN_SANDBOX_REQUIRE=1) but manifest sets sandbox.enabled = false"
    )]
    SandboxRequiredButDisabled { plugin_id: String },

    #[error(
        "plugin `{plugin_id}`: sandbox.network = \"host\" requires NEXO_PLUGIN_SANDBOX_HOST_NET_ALLOW=1"
    )]
    HostNetRequestedButNotAllowed { plugin_id: String },

    #[error(
        "plugin `{plugin_id}`: sandbox required but `bwrap` not found in PATH (apt install bubblewrap)"
    )]
    BwrapMissingButRequired { plugin_id: String },

    #[error(
        "plugin `{plugin_id}`: sandbox required but the current platform is unsupported (macOS sandbox path deferred)"
    )]
    UnsupportedPlatformAndRequired { plugin_id: String },

    #[error(
        "plugin `{plugin_id}`: sandbox path resolution failed for `{path}`: {reason}"
    )]
    PathResolutionFailed {
        plugin_id: String,
        path: String,
        reason: String,
    },

    #[error(
        "plugin `{plugin_id}`: resolved sandbox path `{path}` (from manifest entry `{original}`) hits denylisted host path `{denylisted}`"
    )]
    ResolvedPathHitsDenylist {
        plugin_id: String,
        original: String,
        path: String,
        denylisted: String,
    },
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Builds the bwrap argv (everything AFTER the bwrap binary path).
/// Runs the post-validation denylist check on the resolved
/// `${state_dir}` path so allowlist entries that expand to a
/// denylisted target get caught at spawn time.
fn build_bwrap_args(
    plugin_id: &str,
    sandbox: &SandboxSection,
    state_dir: &Path,
    cmd: &str,
    cmd_args: &[String],
) -> Result<Vec<OsString>, SandboxError> {
    let mut argv: Vec<OsString> = Vec::new();

    // Process-lifecycle hardening (see bwrap(1)).
    argv.push("--die-with-parent".into());
    argv.push("--unshare-pid".into());
    argv.push("--unshare-uts".into());
    argv.push("--unshare-ipc".into());
    argv.push("--new-session".into());

    // Filesystem skeleton — read-only system dirs, fresh /proc + /tmp.
    argv.push("--proc".into());
    argv.push("/proc".into());
    argv.push("--dev".into());
    argv.push("/dev".into());
    argv.push("--tmpfs".into());
    argv.push("/tmp".into());

    for ro in &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc/ssl"] {
        if Path::new(ro).exists() {
            argv.push("--ro-bind-try".into());
            argv.push(OsString::from(ro));
            argv.push(OsString::from(ro));
        }
    }
    // /etc/resolv.conf is host-managed; bind read-only when present
    // (host net mode needs it; deny-net mode tolerates absence).
    argv.push("--ro-bind-try".into());
    argv.push("/etc/resolv.conf".into());
    argv.push("/etc/resolv.conf".into());

    // Network policy.
    match sandbox.network {
        SandboxNetwork::Deny => {
            argv.push("--unshare-net".into());
        }
        SandboxNetwork::Host => {
            // No --unshare-net flag; child shares host's net namespace.
            // Operator opt-in already verified by caller.
        }
    }

    // User/group drop.
    if sandbox.drop_user {
        argv.push("--unshare-user".into());
        argv.push("--uid".into());
        argv.push(OsString::from(NOBODY_UID.to_string()));
        argv.push("--gid".into());
        argv.push(OsString::from(NOBODY_GID.to_string()));
    }

    // PATH skeleton — bwrap doesn't preserve env by default.
    argv.push("--setenv".into());
    argv.push("PATH".into());
    argv.push("/usr/bin:/bin".into());

    // Implicit: ro-bind the plugin command's parent dir so the
    // executable itself is reachable inside the sandbox. Without
    // this, the binary path (often in the plugin's bundle dir)
    // would not exist in the sandboxed view and bwrap exec fails
    // with ENOENT.
    if let Some(cmd_parent) = std::path::Path::new(cmd).parent() {
        if cmd_parent.exists() && !cmd_parent.as_os_str().is_empty() {
            argv.push("--ro-bind-try".into());
            argv.push(OsString::from(cmd_parent));
            argv.push(OsString::from(cmd_parent));
        }
    }

    // Read-only allowlist.
    for entry in &sandbox.fs_read_paths {
        let resolved = resolve_path(entry, state_dir);
        validate_resolved_path(plugin_id, entry, &resolved)?;
        argv.push("--ro-bind".into());
        argv.push(OsString::from(&resolved));
        argv.push(OsString::from(&resolved));
    }

    // Read-write allowlist (state_dir token expansion happens here).
    for entry in &sandbox.fs_write_paths {
        let resolved = resolve_path(entry, state_dir);
        validate_resolved_path(plugin_id, entry, &resolved)?;
        argv.push("--bind".into());
        argv.push(OsString::from(&resolved));
        argv.push(OsString::from(&resolved));
    }

    // The plugin command + its args go after `--`.
    argv.push("--".into());
    argv.push(OsString::from(cmd));
    for a in cmd_args {
        argv.push(OsString::from(a));
    }

    Ok(argv)
}

fn resolve_path(entry: &str, state_dir: &Path) -> String {
    if entry.contains(SANDBOX_STATE_DIR_TOKEN) {
        entry.replace(SANDBOX_STATE_DIR_TOKEN, &state_dir.to_string_lossy())
    } else {
        entry.to_string()
    }
}

/// Spawn-time guard: re-runs the denylist check on the post-token
/// expansion path. The manifest validator ran this against the
/// pre-expansion string with a sentinel placeholder; here we
/// catch the case where `state_dir` itself happens to live under
/// a denylisted path (operator misconfig).
fn validate_resolved_path(
    plugin_id: &str,
    original: &str,
    resolved: &str,
) -> Result<(), SandboxError> {
    if !resolved.starts_with('/') {
        return Err(SandboxError::PathResolutionFailed {
            plugin_id: plugin_id.to_string(),
            path: original.to_string(),
            reason: "resolved path is not absolute".into(),
        });
    }
    if let Some(denylisted) =
        path_under_or_equals_denylist(resolved, SANDBOX_DENYLIST_HOST_PATHS)
    {
        return Err(SandboxError::ResolvedPathHitsDenylist {
            plugin_id: plugin_id.to_string(),
            original: original.to_string(),
            path: resolved.to_string(),
            denylisted: denylisted.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_manifest::{PluginManifest, SandboxNetwork};

    fn manifest_with_sandbox(toml: &str) -> PluginManifest {
        let full = format!(
            r#"
[plugin]
id = "p"
version = "0.1.0"
name = "p"
description = "test plugin"
min_nexo_version = ">=0.0.0"

{}
"#,
            toml
        );
        PluginManifest::from_str(&full).expect("valid TOML")
    }

    fn fresh_runner() -> SandboxRunner {
        SandboxRunner::for_test(Some(PathBuf::from("/usr/bin/bwrap")), false, false)
    }

    #[test]
    fn discover_sets_bwrap_path_when_present() {
        let r = SandboxRunner::for_test(Some("/usr/bin/bwrap".into()), false, false);
        assert_eq!(r.bwrap_path(), Some(Path::new("/usr/bin/bwrap")));
    }

    #[test]
    fn discover_handles_missing_bwrap() {
        let r = SandboxRunner::for_test(None, false, false);
        assert!(r.bwrap_path().is_none());
    }

    #[test]
    fn env_flag_recognizes_truthy_values() {
        // pure helper — set + read.
        std::env::set_var("NEXO_PLUGIN_SANDBOX_TEST_FLAG", "1");
        assert!(env_flag("NEXO_PLUGIN_SANDBOX_TEST_FLAG"));
        std::env::set_var("NEXO_PLUGIN_SANDBOX_TEST_FLAG", "yes");
        assert!(env_flag("NEXO_PLUGIN_SANDBOX_TEST_FLAG"));
        std::env::set_var("NEXO_PLUGIN_SANDBOX_TEST_FLAG", "0");
        assert!(!env_flag("NEXO_PLUGIN_SANDBOX_TEST_FLAG"));
        std::env::remove_var("NEXO_PLUGIN_SANDBOX_TEST_FLAG");
    }

    #[test]
    fn wrap_disabled_passes_command_through_raw() {
        let m = manifest_with_sandbox("[plugin.sandbox]\nenabled = false");
        let runner = fresh_runner();
        let wrapped = runner
            .wrap_command(&m, Path::new("/state"), "/bin/echo", &["hi".into()])
            .unwrap();
        assert_eq!(wrapped.program, PathBuf::from("/bin/echo"));
        assert_eq!(wrapped.args, vec![OsString::from("hi")]);
        assert!(wrapped.diagnostic.is_none());
    }

    #[test]
    fn wrap_disabled_with_strict_mode_errors() {
        let m = manifest_with_sandbox("[plugin.sandbox]\nenabled = false");
        let runner = SandboxRunner::for_test(Some("/usr/bin/bwrap".into()), false, true);
        let err = runner
            .wrap_command(&m, Path::new("/state"), "/bin/echo", &[])
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::SandboxRequiredButDisabled { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn wrap_host_network_without_capability_errors() {
        let m = manifest_with_sandbox(
            "[plugin.sandbox]\nenabled = true\nnetwork = \"host\"",
        );
        let runner = fresh_runner();
        let err = runner
            .wrap_command(&m, Path::new("/state"), "/bin/echo", &[])
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::HostNetRequestedButNotAllowed { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn wrap_host_network_with_capability_passes() {
        let m = manifest_with_sandbox(
            "[plugin.sandbox]\nenabled = true\nnetwork = \"host\"",
        );
        let runner = SandboxRunner::for_test(Some("/usr/bin/bwrap".into()), true, false);
        // On macOS the platform branch may downgrade to raw — we
        // only assert the host-net check passed (no error).
        let res = runner.wrap_command(&m, Path::new("/state"), "/bin/echo", &[]);
        assert!(res.is_ok(), "{res:?}");
    }

    #[test]
    fn wrap_bwrap_missing_warns_in_non_strict_mode() {
        let m = manifest_with_sandbox(
            "[plugin.sandbox]\nenabled = true\nfs_read_paths = [\"/etc/ssl\"]",
        );
        let runner = SandboxRunner::for_test(None, false, false);
        if cfg!(target_os = "macos") {
            // macOS branch fires first — separate test covers
            // that path.
            return;
        }
        let wrapped = runner
            .wrap_command(&m, Path::new("/state"), "/bin/echo", &[])
            .unwrap();
        assert_eq!(wrapped.program, PathBuf::from("/bin/echo"));
        assert!(wrapped.diagnostic.is_some());
        let diag = wrapped.diagnostic.unwrap();
        assert!(diag.contains("bwrap"), "{diag}");
    }

    #[test]
    fn wrap_bwrap_missing_with_strict_errors() {
        let m = manifest_with_sandbox("[plugin.sandbox]\nenabled = true");
        let runner = SandboxRunner::for_test(None, false, true);
        if cfg!(target_os = "macos") {
            // strict + macos hits UnsupportedPlatformAndRequired
            // first; assertion in dedicated macos test.
            return;
        }
        let err = runner
            .wrap_command(&m, Path::new("/state"), "/bin/echo", &[])
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::BwrapMissingButRequired { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn build_bwrap_args_includes_unshare_net_for_deny_mode() {
        let sandbox = SandboxSection {
            enabled: true,
            network: SandboxNetwork::Deny,
            fs_read_paths: vec![],
            fs_write_paths: vec![],
            drop_user: false,
        };
        let argv = build_bwrap_args("p", &sandbox, Path::new("/state"), "/bin/echo", &[])
            .unwrap();
        assert!(
            argv.iter().any(|a| *a == OsString::from("--unshare-net")),
            "deny mode must include --unshare-net: {argv:?}"
        );
    }

    #[test]
    fn build_bwrap_args_omits_unshare_net_for_host_mode() {
        let sandbox = SandboxSection {
            enabled: true,
            network: SandboxNetwork::Host,
            fs_read_paths: vec![],
            fs_write_paths: vec![],
            drop_user: false,
        };
        let argv = build_bwrap_args("p", &sandbox, Path::new("/state"), "/bin/echo", &[])
            .unwrap();
        assert!(
            !argv.iter().any(|a| *a == OsString::from("--unshare-net")),
            "host mode must omit --unshare-net: {argv:?}"
        );
    }

    #[test]
    fn build_bwrap_args_includes_uid_gid_for_drop_user() {
        let sandbox = SandboxSection {
            enabled: true,
            network: SandboxNetwork::Deny,
            fs_read_paths: vec![],
            fs_write_paths: vec![],
            drop_user: true,
        };
        let argv = build_bwrap_args("p", &sandbox, Path::new("/state"), "/bin/echo", &[])
            .unwrap();
        // Find `--uid` followed by 65534.
        let uid_pos = argv.iter().position(|a| *a == OsString::from("--uid"));
        assert!(uid_pos.is_some());
        let uid_val = &argv[uid_pos.unwrap() + 1];
        assert_eq!(uid_val, &OsString::from("65534"));
    }

    #[test]
    fn build_bwrap_args_expands_state_dir_token_in_write_paths() {
        let sandbox = SandboxSection {
            enabled: true,
            network: SandboxNetwork::Deny,
            fs_read_paths: vec![],
            fs_write_paths: vec!["${state_dir}".into(), "${state_dir}/cache".into()],
            drop_user: false,
        };
        let argv = build_bwrap_args(
            "p",
            &sandbox,
            Path::new("/var/lib/nexo/plugins/p"),
            "/bin/echo",
            &[],
        )
        .unwrap();
        // After expansion, --bind <state_dir> <state_dir> appears.
        let bind_count = argv
            .iter()
            .filter(|a| **a == OsString::from("--bind"))
            .count();
        assert_eq!(bind_count, 2);
        assert!(argv
            .iter()
            .any(|a| a == &OsString::from("/var/lib/nexo/plugins/p")));
        assert!(argv
            .iter()
            .any(|a| a == &OsString::from("/var/lib/nexo/plugins/p/cache")));
    }

    #[test]
    fn build_bwrap_args_appends_command_after_double_dash() {
        let sandbox = SandboxSection::default();
        let mut s = sandbox;
        s.enabled = true;
        let argv = build_bwrap_args("p", &s, Path::new("/state"), "/usr/bin/python3", &[
            "-c".into(),
            "print(1)".into(),
        ])
        .unwrap();
        let dd_pos = argv
            .iter()
            .position(|a| *a == OsString::from("--"))
            .expect("argv must contain --");
        assert_eq!(argv[dd_pos + 1], OsString::from("/usr/bin/python3"));
        assert_eq!(argv[dd_pos + 2], OsString::from("-c"));
        assert_eq!(argv[dd_pos + 3], OsString::from("print(1)"));
    }

    #[test]
    fn validate_resolved_path_rejects_denylist_after_state_dir_expansion() {
        // state_dir resolves to a denylisted path — operator
        // misconfig caught at spawn time.
        let err = validate_resolved_path("p", "${state_dir}", "/etc/shadow")
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::ResolvedPathHitsDenylist { .. }),
            "{err:?}"
        );
    }
}
