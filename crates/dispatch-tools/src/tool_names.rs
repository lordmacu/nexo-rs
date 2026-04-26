//! Canonical names of every tool registered by the dispatch
//! subsystem, grouped by `DispatchKind` so the registry can prune the
//! set as a function of `DispatchCapability`.
//!
//! Lists live here (not in `nexo-core`) so the `register_dispatch_tools`
//! wiring in core depends on the same single source of truth that the
//! per-tool implementations (67.E.x) will pull in. A YAML registry
//! would diverge silently the moment somebody renamed a constant.

use nexo_config::{DispatchCapability, DispatchPolicy};

/// Tools that only read state. Always registered when capability is
/// `ReadOnly` or `Full`; never when capability is `None`.
pub const READ_TOOL_NAMES: &[&str] = &[
    "project_status",
    "project_phases_list",
    "followup_detail",
    "git_log_for_phase",
    "list_agents",
    "agent_status",
    "agent_logs_tail",
    "agent_turns_tail",
    "agent_hooks_list",
];

/// Tools that mutate state. Registered only when capability is `Full`.
/// The `DispatchGate` (67.D.2) is the second line of defense at
/// invocation time.
pub const WRITE_TOOL_NAMES: &[&str] = &[
    "program_phase",
    "program_phase_chain",
    "program_phase_parallel",
    "dispatch_followup",
    "cancel_agent",
    "pause_agent",
    "resume_agent",
    "update_budget",
    "add_hook",
    "remove_hook",
];

/// Operator-only tools. Registered behind a separate flag (admin)
/// rather than the per-binding `DispatchPolicy` because they affect
/// the entire orchestrator, not just the calling agent's surface.
/// 67.G.4 will land the actual registration; the names are listed
/// here so the registry can prune them out of non-admin sessions.
pub const ADMIN_TOOL_NAMES: &[&str] = &[
    "set_concurrency_cap",
    "flush_agent_queue",
    "evict_completed",
];

/// Group of tools — the dimension along which `should_register`
/// decides.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolGroup {
    Read,
    Write,
    Admin,
}

/// Should a tool of `group` be registered for a binding whose
/// dispatch policy is `policy`? `is_admin` is the operator-bit; only
/// admin sessions get the admin tools.
pub fn should_register(policy: &DispatchPolicy, group: ToolGroup, is_admin: bool) -> bool {
    match (policy.mode, group) {
        (DispatchCapability::None, _) => false,
        (_, ToolGroup::Admin) => is_admin,
        (DispatchCapability::ReadOnly, ToolGroup::Read) => true,
        (DispatchCapability::ReadOnly, ToolGroup::Write) => false,
        (DispatchCapability::Full, _) => true,
    }
}

/// Convenience: collect every tool name that survives the policy
/// filter. Used by `nexo-core::ToolRegistry::register_dispatch_tools`
/// to know which entries to keep / drop in one place.
pub fn allowed_tool_names(policy: &DispatchPolicy, is_admin: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if should_register(policy, ToolGroup::Read, is_admin) {
        out.extend_from_slice(READ_TOOL_NAMES);
    }
    if should_register(policy, ToolGroup::Write, is_admin) {
        out.extend_from_slice(WRITE_TOOL_NAMES);
    }
    if should_register(policy, ToolGroup::Admin, is_admin) {
        out.extend_from_slice(ADMIN_TOOL_NAMES);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol(mode: DispatchCapability) -> DispatchPolicy {
        DispatchPolicy {
            mode,
            max_concurrent_per_dispatcher: 0,
            allowed_phase_ids: Vec::new(),
            forbidden_phase_ids: Vec::new(),
        }
    }

    #[test]
    fn none_capability_drops_every_tool() {
        let p = pol(DispatchCapability::None);
        assert!(!should_register(&p, ToolGroup::Read, false));
        assert!(!should_register(&p, ToolGroup::Read, true));
        assert!(!should_register(&p, ToolGroup::Write, true));
        assert!(allowed_tool_names(&p, true).is_empty());
    }

    #[test]
    fn read_only_keeps_reads_drops_writes_drops_admin() {
        let p = pol(DispatchCapability::ReadOnly);
        assert!(should_register(&p, ToolGroup::Read, false));
        assert!(!should_register(&p, ToolGroup::Write, false));
        assert!(!should_register(&p, ToolGroup::Admin, false));
        // is_admin=true raises admin tools but does not unlock writes.
        assert!(!should_register(&p, ToolGroup::Write, true));
        assert!(should_register(&p, ToolGroup::Admin, true));

        let names = allowed_tool_names(&p, false);
        for n in READ_TOOL_NAMES {
            assert!(names.contains(n), "missing read tool {n}");
        }
        assert!(!names.contains(&"program_phase"));
    }

    #[test]
    fn full_unlocks_reads_and_writes_admin_only_with_admin_flag() {
        let p = pol(DispatchCapability::Full);
        assert!(should_register(&p, ToolGroup::Read, false));
        assert!(should_register(&p, ToolGroup::Write, false));
        assert!(!should_register(&p, ToolGroup::Admin, false));
        assert!(should_register(&p, ToolGroup::Admin, true));

        let names_user = allowed_tool_names(&p, false);
        assert!(names_user.contains(&"program_phase"));
        assert!(!names_user.contains(&"set_concurrency_cap"));
        let names_admin = allowed_tool_names(&p, true);
        assert!(names_admin.contains(&"set_concurrency_cap"));
    }

    #[test]
    fn name_lists_have_no_overlap() {
        for n in READ_TOOL_NAMES {
            assert!(!WRITE_TOOL_NAMES.contains(n), "{n} in both read+write");
            assert!(!ADMIN_TOOL_NAMES.contains(n), "{n} in both read+admin");
        }
        for n in WRITE_TOOL_NAMES {
            assert!(!ADMIN_TOOL_NAMES.contains(n), "{n} in both write+admin");
        }
    }
}
