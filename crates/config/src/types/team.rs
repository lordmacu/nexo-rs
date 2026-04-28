//! Phase 79.6 — per-agent team policy.
//!
//! `TeamPolicy` lives on `AgentConfig::team`. Default is
//! `enabled: false` (opt-in per agent). When enabled, the agent's
//! tool registry gains the 5 `Team*` tools.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/tools/TeamCreateTool/TeamCreateTool.ts:88-90`
//!     uses `isAgentSwarmsEnabled()` env-toggle. Our YAML capability
//!     gate is the equivalent — operator opts in per agent, default
//!     off.

use serde::{Deserialize, Serialize};

const TEAM_MAX_MEMBERS: u32 = 8;
const TEAM_MAX_CONCURRENT_DEFAULT: u32 = 4;
const TEAM_IDLE_TIMEOUT_DEFAULT: u64 = 3600;

fn default_enabled() -> bool {
    false
}
fn default_max_members() -> u32 {
    TEAM_MAX_MEMBERS
}
fn default_max_concurrent() -> u32 {
    TEAM_MAX_CONCURRENT_DEFAULT
}
fn default_idle_timeout() -> u64 {
    TEAM_IDLE_TIMEOUT_DEFAULT
}

/// Per-agent team capability + caps.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct TeamPolicy {
    /// Master switch. The 5 `Team*` tools register only when
    /// `true`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Cap on members per team (incl. the lead). Clamped against
    /// the runtime constant `TEAM_MAX_MEMBERS = 8`.
    #[serde(default = "default_max_members")]
    pub max_members: u32,

    /// Cap on concurrent active teams led by this agent. Clamped
    /// against the runtime constant `TEAM_MAX_CONCURRENT = 4`.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,

    /// Stale-team threshold. The reaper marks teams whose
    /// `last_active_at` exceeds this and notifies the lead via
    /// `notify_origin`. Default 3600s (1h).
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,

    /// Whether `TeamCreate` defaults to creating a per-member git
    /// worktree. Per-call argument can override.
    #[serde(default)]
    pub worktree_per_member: bool,
}

impl Default for TeamPolicy {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            max_members: default_max_members(),
            max_concurrent: default_max_concurrent(),
            idle_timeout_secs: default_idle_timeout(),
            worktree_per_member: false,
        }
    }
}

impl TeamPolicy {
    pub fn tool_enabled(&self) -> bool {
        self.enabled
    }

    /// Effective members cap, clamped at `TEAM_MAX_MEMBERS`.
    pub fn effective_max_members(&self) -> u32 {
        self.max_members.min(TEAM_MAX_MEMBERS).max(1)
    }

    /// Effective concurrent cap, clamped at
    /// `TEAM_MAX_CONCURRENT_DEFAULT`.
    pub fn effective_max_concurrent(&self) -> u32 {
        self.max_concurrent.min(TEAM_MAX_CONCURRENT_DEFAULT).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_yaml(y: &str) -> TeamPolicy {
        serde_yaml::from_str(y).unwrap()
    }

    #[test]
    fn team_policy_default_disabled() {
        let p = TeamPolicy::default();
        assert!(!p.enabled);
        assert_eq!(p.max_members, TEAM_MAX_MEMBERS);
        assert_eq!(p.max_concurrent, TEAM_MAX_CONCURRENT_DEFAULT);
        assert_eq!(p.idle_timeout_secs, TEAM_IDLE_TIMEOUT_DEFAULT);
        assert!(!p.worktree_per_member);
    }

    #[test]
    fn team_policy_yaml_roundtrip() {
        let yaml = r#"
            enabled: true
            max_members: 4
            max_concurrent: 2
            idle_timeout_secs: 1200
            worktree_per_member: true
        "#;
        let p = from_yaml(yaml);
        assert!(p.enabled);
        assert_eq!(p.max_members, 4);
        assert_eq!(p.max_concurrent, 2);
        assert_eq!(p.idle_timeout_secs, 1200);
        assert!(p.worktree_per_member);
    }

    #[test]
    fn team_policy_empty_yaml_uses_defaults() {
        let p = from_yaml("{}");
        assert_eq!(p, TeamPolicy::default());
    }

    #[test]
    fn team_policy_unknown_field_rejected() {
        let yaml = r#"
            enabled: true
            mystery: 1
        "#;
        let err = serde_yaml::from_str::<TeamPolicy>(yaml)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mystery"), "got: {err}");
    }

    #[test]
    fn effective_max_members_clamps_to_8() {
        let p = TeamPolicy {
            max_members: 100,
            ..TeamPolicy::default()
        };
        assert_eq!(p.effective_max_members(), 8);
    }

    #[test]
    fn effective_max_concurrent_clamps_to_4() {
        let p = TeamPolicy {
            max_concurrent: 100,
            ..TeamPolicy::default()
        };
        assert_eq!(p.effective_max_concurrent(), 4);
    }

    #[test]
    fn effective_caps_floor_at_one() {
        let p = TeamPolicy {
            max_members: 0,
            max_concurrent: 0,
            ..TeamPolicy::default()
        };
        assert_eq!(p.effective_max_members(), 1);
        assert_eq!(p.effective_max_concurrent(), 1);
    }

    #[test]
    fn tool_enabled_matches_field() {
        let p = TeamPolicy {
            enabled: true,
            ..TeamPolicy::default()
        };
        assert!(p.tool_enabled());
    }
}
