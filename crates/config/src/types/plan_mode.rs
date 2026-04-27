//! Phase 79.1 — per-binding plan-mode policy.
//!
//! This module owns the YAML surface that an operator uses to opt into
//! (or opt out of) plan-mode gating per binding. The runtime loads the
//! resolved policy via Phase 16 `EffectiveBindingPolicy` and consults
//! [`PlanModePolicy::compute_default_active`] to decide whether plan
//! mode should be ARMED for a given goal — that is, whether new goals
//! enter plan mode automatically. Tool registration is gated by the
//! independent [`PlanModePolicy::enabled`] knob so an operator can keep
//! the tools registered (so the model sees them in the deferred-schema
//! catalogue) without arming the auto-on default.

use serde::Deserialize;

/// Roles the YAML may set on a binding. Mirror of `binding.role` —
/// kept here to break a config <-> core dep cycle. The string values
/// match `crates/config/src/types/agents.rs::InboundBinding::role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRole {
    Coordinator,
    Worker,
    Proactive,
    /// No `role` key set in YAML — the safest default is OFF for
    /// auto-arming because any new role would otherwise be silently
    /// opted in.
    Unset,
}

impl BindingRole {
    /// Parse the textual role used in YAML.
    pub fn from_role_str(role: Option<&str>) -> Self {
        match role.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("coordinator") => BindingRole::Coordinator,
            Some("worker") => BindingRole::Worker,
            Some("proactive") => BindingRole::Proactive,
            _ => BindingRole::Unset,
        }
    }
}

const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 86_400; // 24 h

/// YAML schema for `plan_mode:` per binding.
///
/// Defaults below are the values applied when the operator omits the
/// block entirely:
///   * `enabled: true` — tools are registered; the model sees them.
///   * `auto_enter_on_destructive: false` — opt-in pairing with 77.8.
///   * `default_active: None` — role-aware computed default; see
///     [`PlanModePolicy::compute_default_active`].
///   * `approval_timeout_secs: 86_400` (24 h).
///
/// Cross-reference: `claude-code-leak/src/tools/EnterPlanModeTool/EnterPlanModeTool.ts:60-66`
/// uses `feature('KAIROS_CHANNELS')` to disable the entire tool when
/// channels are active. Nexo intentionally diverges (pairing IS the
/// channel) — `enabled: true` is a sensible default for chat-rooted
/// bindings, and `compute_default_active` keeps non-coordinator roles
/// from suffering surprise auto-arm.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct PlanModePolicy {
    /// When `false`, the `EnterPlanMode` and `ExitPlanMode` tools are
    /// not registered for goals on this binding. The model never sees
    /// them in the catalogue. Useful for bindings whose channel cannot
    /// deliver an approval message (e.g. one-shot HTTP webhooks).
    pub enabled: bool,
    /// Soft dep on Phase 77.8: when the destructive-command warning
    /// classifies the next Bash call as destructive, the dispatcher
    /// pre-empts the call and auto-enters plan mode with the
    /// classifier verdict embedded in the refusal. While 77.8 is not
    /// shipped this knob no-ops and emits a `tracing::warn!` once at
    /// boot.
    pub auto_enter_on_destructive: bool,
    /// Whether plan mode is ARMED by default for new goals on this
    /// binding. `None` (the YAML default) defers to
    /// [`PlanModePolicy::compute_default_active`] — `true` for
    /// coordinator role, `false` for everything else. Operators can
    /// pin the value with `default_active: true` / `false` to override
    /// the role-aware default.
    pub default_active: Option<bool>,
    /// How long an `ExitPlanMode` call may wait for operator
    /// approval before the goal resolves to `LostOnApproval`. Default
    /// 24 h is enough for an operator to come back to a paused chat
    /// the next morning.
    pub approval_timeout_secs: u64,
    /// Phase 79.1 — when `true`, `ExitPlanMode` blocks until an
    /// operator-side `plan_mode_resolve` arrives via the pairing
    /// channel. When `false` (the default for safe rollout), the
    /// model self-approves: the unlock is immediate. Production
    /// deployments SHOULD set this to `true` once the pairing parser
    /// wiring is verified end-to-end.
    pub require_approval: bool,
}

impl Default for PlanModePolicy {
    fn default() -> Self {
        PlanModePolicy {
            enabled: true,
            auto_enter_on_destructive: false,
            default_active: None,
            approval_timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
            require_approval: false,
        }
    }
}

impl PlanModePolicy {
    /// Resolve `default_active` honouring the role-aware fallback.
    ///
    /// * Explicit YAML wins (`default_active: true|false`).
    /// * `coordinator` role → `true` (these bindings drive
    ///   non-trivial work and benefit from plan-mode gating).
    /// * `worker` role → `false` (workers receive sub-goals from a
    ///   coordinator that already planned).
    /// * `proactive` role → `false` (Phase 77.20 ticks would be
    ///   disrupted by a blocking approval flow).
    /// * Unset role → `false` (safest opt-out — any new role added
    ///   later does not silently auto-arm).
    pub fn compute_default_active(&self, role: BindingRole) -> bool {
        if let Some(explicit) = self.default_active {
            return explicit;
        }
        matches!(role, BindingRole::Coordinator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_yaml(yaml: &str) -> PlanModePolicy {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn default_yields_safe_values() {
        let p = PlanModePolicy::default();
        assert!(p.enabled);
        assert!(!p.auto_enter_on_destructive);
        assert_eq!(p.default_active, None);
        assert_eq!(p.approval_timeout_secs, 86_400);
    }

    #[test]
    fn empty_yaml_uses_defaults() {
        let yaml = "{}";
        let p = from_yaml(yaml);
        assert_eq!(p, PlanModePolicy::default());
    }

    #[test]
    fn full_yaml_deserialises() {
        let yaml = r#"
            enabled: false
            auto_enter_on_destructive: true
            default_active: true
            approval_timeout_secs: 600
            require_approval: true
        "#;
        let p = from_yaml(yaml);
        assert!(!p.enabled);
        assert!(p.auto_enter_on_destructive);
        assert_eq!(p.default_active, Some(true));
        assert_eq!(p.approval_timeout_secs, 600);
        assert!(p.require_approval);
    }

    #[test]
    fn require_approval_defaults_off() {
        let p = PlanModePolicy::default();
        assert!(!p.require_approval, "back-compat: opt-in approval");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let yaml = r#"
            enabled: true
            mystery: 1
        "#;
        let err = serde_yaml::from_str::<PlanModePolicy>(yaml)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mystery"), "error was: {err}");
    }

    #[test]
    fn role_aware_default_coordinator_on() {
        let p = PlanModePolicy::default();
        assert!(p.compute_default_active(BindingRole::Coordinator));
    }

    #[test]
    fn role_aware_default_worker_off() {
        let p = PlanModePolicy::default();
        assert!(!p.compute_default_active(BindingRole::Worker));
    }

    #[test]
    fn role_aware_default_proactive_off() {
        let p = PlanModePolicy::default();
        assert!(!p.compute_default_active(BindingRole::Proactive));
    }

    #[test]
    fn role_aware_default_unset_off() {
        let p = PlanModePolicy::default();
        assert!(!p.compute_default_active(BindingRole::Unset));
    }

    #[test]
    fn explicit_default_active_wins_over_role() {
        let p = PlanModePolicy {
            default_active: Some(false),
            ..PlanModePolicy::default()
        };
        // Even coordinator gets opt-out when explicit.
        assert!(!p.compute_default_active(BindingRole::Coordinator));

        let p_on = PlanModePolicy {
            default_active: Some(true),
            ..PlanModePolicy::default()
        };
        // And worker gets opt-in when explicit.
        assert!(p_on.compute_default_active(BindingRole::Worker));
    }

    #[test]
    fn binding_role_parses_known_strings() {
        assert_eq!(BindingRole::from_role_str(None), BindingRole::Unset);
        assert_eq!(
            BindingRole::from_role_str(Some("coordinator")),
            BindingRole::Coordinator
        );
        assert_eq!(
            BindingRole::from_role_str(Some("  WORKER  ")),
            BindingRole::Worker
        );
        assert_eq!(
            BindingRole::from_role_str(Some("proactive")),
            BindingRole::Proactive
        );
        assert_eq!(
            BindingRole::from_role_str(Some("anything-else")),
            BindingRole::Unset
        );
    }
}
