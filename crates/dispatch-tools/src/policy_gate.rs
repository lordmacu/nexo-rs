//! Phase 67.D.2 — admission gate for the `program_phase` family of
//! tools. Pure function with no I/O: feed it the resolved policy,
//! the trust signal, the live cap counters, and the requested
//! phase id; get back `Ok(())` or a structured `DispatchDenied`.
//!
//! Enforcement matrix (denies short-circuit in this order):
//!
//! 1. `dispatch_capability == None` → `CapabilityNone`.
//! 2. `dispatch_capability == ReadOnly` and the request is a write
//!    op → `CapabilityReadOnly`.
//! 3. `require_trusted=true` AND sender is not pairing-trusted →
//!    `SenderNotTrusted`.
//! 4. `forbidden_phase_ids` matches → `PhaseForbidden`.
//! 5. `allowed_phase_ids` is non-empty AND nothing matches →
//!    `PhaseNotAllowed`.
//! 6. Per-dispatcher cap exceeded → `DispatcherCapReached`.
//! 7. Per-sender cap exceeded → `SenderCapReached`.
//! 8. Global cap exceeded AND queue policy is reject →
//!    `GlobalCapReached`.
//!
//! Order matters: capability + trust failures must surface before
//! cap-related ones so an unauthorized caller never learns the
//! current concurrency state.

use nexo_config::{DispatchCapability, DispatchPolicy};
use thiserror::Error;

/// Discriminator for read-only vs write tools so the gate can refuse
/// `program_phase` while allowing `list_agents` under a `ReadOnly`
/// capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchKind {
    /// Read tool (`project_status`, `list_agents`, `agent_status`,
    /// `agent_logs_tail`, `agent_hooks_list`, `followup_detail`,
    /// `git_log_for_phase`).
    Read,
    /// Write / mutating tool (`program_phase`,
    /// `program_phase_chain`, `program_phase_parallel`,
    /// `dispatch_followup`, `cancel_agent`, `pause_agent`,
    /// `resume_agent`, `update_budget`, `add_hook`, `remove_hook`).
    Write,
}

/// Snapshot of cap counters consumed by the gate. Caller is
/// responsible for taking a consistent reading (locking the queue
/// admit path) before invoking `check`.
#[derive(Clone, Copy, Debug, Default)]
pub struct CapSnapshot {
    pub global_running: u32,
    pub global_max: u32,
    pub dispatcher_running: u32,
    pub sender_running: u32,
    pub sender_max: u32,
    /// `true` when the orchestrator will accept new work into a
    /// FIFO queue past the global cap; gate only denies if the
    /// queue is also disabled.
    pub queue_when_full: bool,
}

/// Inputs the caller hands to `DispatchGate::check`.
pub struct DispatchRequest<'a> {
    pub kind: DispatchKind,
    pub phase_id: &'a str,
    pub policy: &'a DispatchPolicy,
    /// Resolved from `program_phase.require_trusted` config.
    pub require_trusted: bool,
    /// `true` when the sender comes from a `pairing.trusted=true`
    /// binding.
    pub sender_trusted: bool,
    pub caps: CapSnapshot,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DispatchDenied {
    #[error("[dispatch] dispatch_capability=none for this binding")]
    CapabilityNone,
    #[error("[dispatch] dispatch_capability=read_only — write tool blocked")]
    CapabilityReadOnly,
    #[error(
        "[dispatch] sender is not pairing.trusted (binding flag, not the intake \
         pairing store; flip `program_phase.require_trusted` or set the binding's \
         `pairing.trusted=true`)"
    )]
    SenderNotTrusted,
    #[error("[dispatch] phase {0} is on forbidden_phase_ids")]
    PhaseForbidden(String),
    #[error("[dispatch] phase {0} is not in allowed_phase_ids")]
    PhaseNotAllowed(String),
    #[error("[dispatch] dispatcher cap reached: {current}/{max}")]
    DispatcherCapReached { current: u32, max: u32 },
    #[error("[dispatch] sender cap reached: {current}/{max}")]
    SenderCapReached { current: u32, max: u32 },
    #[error("[dispatch] global cap reached: {current}/{max}")]
    GlobalCapReached { current: u32, max: u32 },
}

pub struct DispatchGate;

impl DispatchGate {
    /// Pure decision. Caller compares the result and either spawns
    /// the goal or relays the denial to the user.
    pub fn check(req: &DispatchRequest<'_>) -> Result<(), DispatchDenied> {
        // 1 — capability None: nothing flows.
        if matches!(req.policy.mode, DispatchCapability::None) {
            return Err(DispatchDenied::CapabilityNone);
        }
        // 2 — read-only capability: write tools blocked even if
        // everything else passes.
        if matches!(req.policy.mode, DispatchCapability::ReadOnly)
            && req.kind == DispatchKind::Write
        {
            return Err(DispatchDenied::CapabilityReadOnly);
        }
        // 3 — trust: only enforced for write requests; reads stay
        // open even for non-trusted senders so the operator can see
        // status before pairing.
        if req.kind == DispatchKind::Write && req.require_trusted && !req.sender_trusted {
            return Err(DispatchDenied::SenderNotTrusted);
        }

        // Phase-id guards apply to write requests only — reads don't
        // touch a specific phase via dispatch.
        if req.kind == DispatchKind::Write {
            if matches_any(&req.policy.forbidden_phase_ids, req.phase_id) {
                return Err(DispatchDenied::PhaseForbidden(req.phase_id.to_string()));
            }
            if !req.policy.allowed_phase_ids.is_empty()
                && !matches_any(&req.policy.allowed_phase_ids, req.phase_id)
            {
                return Err(DispatchDenied::PhaseNotAllowed(req.phase_id.to_string()));
            }
        }

        // Cap checks — write only.
        if req.kind == DispatchKind::Write {
            if req.policy.max_concurrent_per_dispatcher > 0
                && req.caps.dispatcher_running >= req.policy.max_concurrent_per_dispatcher
            {
                return Err(DispatchDenied::DispatcherCapReached {
                    current: req.caps.dispatcher_running,
                    max: req.policy.max_concurrent_per_dispatcher,
                });
            }
            if req.caps.sender_max > 0 && req.caps.sender_running >= req.caps.sender_max {
                return Err(DispatchDenied::SenderCapReached {
                    current: req.caps.sender_running,
                    max: req.caps.sender_max,
                });
            }
            if req.caps.global_max > 0
                && req.caps.global_running >= req.caps.global_max
                && !req.caps.queue_when_full
            {
                return Err(DispatchDenied::GlobalCapReached {
                    current: req.caps.global_running,
                    max: req.caps.global_max,
                });
            }
        }

        Ok(())
    }
}

fn matches_any(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| {
        if p == "*" {
            return true;
        }
        match p.strip_suffix('*') {
            Some(stem) => name.starts_with(stem),
            None => p == name,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: DispatchCapability) -> DispatchPolicy {
        DispatchPolicy {
            mode,
            max_concurrent_per_dispatcher: 0,
            allowed_phase_ids: Vec::new(),
            forbidden_phase_ids: Vec::new(),
        }
    }

    fn write_request<'a>(p: &'a DispatchPolicy, phase: &'a str) -> DispatchRequest<'a> {
        DispatchRequest {
            kind: DispatchKind::Write,
            phase_id: phase,
            policy: p,
            require_trusted: true,
            sender_trusted: true,
            caps: CapSnapshot::default(),
        }
    }

    #[test]
    fn capability_none_blocks_everything() {
        let p = policy(DispatchCapability::None);
        let req = write_request(&p, "67.10");
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::CapabilityNone
        );

        // Even reads are blocked when capability=None.
        let mut req_r = write_request(&p, "67.10");
        req_r.kind = DispatchKind::Read;
        assert_eq!(
            DispatchGate::check(&req_r).unwrap_err(),
            DispatchDenied::CapabilityNone
        );
    }

    #[test]
    fn read_only_allows_reads_blocks_writes() {
        let p = policy(DispatchCapability::ReadOnly);
        let mut req = write_request(&p, "67.10");
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::CapabilityReadOnly
        );
        req.kind = DispatchKind::Read;
        DispatchGate::check(&req).unwrap();
    }

    #[test]
    fn untrusted_sender_blocked_when_required() {
        let p = policy(DispatchCapability::Full);
        let mut req = write_request(&p, "67.10");
        req.sender_trusted = false;
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::SenderNotTrusted
        );

        // Reads bypass the trust gate so list_agents stays open.
        req.kind = DispatchKind::Read;
        DispatchGate::check(&req).unwrap();
    }

    #[test]
    fn untrusted_sender_passes_when_trust_not_required() {
        let p = policy(DispatchCapability::Full);
        let mut req = write_request(&p, "67.10");
        req.require_trusted = false;
        req.sender_trusted = false;
        DispatchGate::check(&req).unwrap();
    }

    #[test]
    fn forbidden_phase_wins_over_allowed() {
        let mut p = policy(DispatchCapability::Full);
        p.allowed_phase_ids = vec!["*".into()];
        p.forbidden_phase_ids = vec!["67.13".into()];
        let req = write_request(&p, "67.13");
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::PhaseForbidden("67.13".into())
        );
    }

    #[test]
    fn phase_not_in_allowlist_blocked() {
        let mut p = policy(DispatchCapability::Full);
        p.allowed_phase_ids = vec!["67.*".into()];
        let req = write_request(&p, "5.4");
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::PhaseNotAllowed("5.4".into())
        );

        // Same agent dispatching to 67.x is allowed.
        let req_ok = write_request(&p, "67.10");
        DispatchGate::check(&req_ok).unwrap();
    }

    #[test]
    fn empty_allowlist_means_any_phase() {
        let p = policy(DispatchCapability::Full);
        let req = write_request(&p, "anything.id");
        DispatchGate::check(&req).unwrap();
    }

    #[test]
    fn dispatcher_cap_blocks_when_reached() {
        let mut p = policy(DispatchCapability::Full);
        p.max_concurrent_per_dispatcher = 2;
        let mut req = write_request(&p, "67.10");
        req.caps.dispatcher_running = 2;
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::DispatcherCapReached { current: 2, max: 2 }
        );
    }

    #[test]
    fn sender_cap_blocks_when_reached() {
        let p = policy(DispatchCapability::Full);
        let mut req = write_request(&p, "67.10");
        req.caps.sender_running = 2;
        req.caps.sender_max = 2;
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::SenderCapReached { current: 2, max: 2 }
        );
    }

    #[test]
    fn global_cap_passes_when_queue_when_full() {
        let p = policy(DispatchCapability::Full);
        let mut req = write_request(&p, "67.10");
        req.caps.global_running = 4;
        req.caps.global_max = 4;
        req.caps.queue_when_full = true;
        DispatchGate::check(&req).unwrap();

        // Queue disabled → reject.
        req.caps.queue_when_full = false;
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::GlobalCapReached { current: 4, max: 4 }
        );
    }

    #[test]
    fn capability_check_runs_before_cap_check() {
        // Even with caps maxed, a None-capability denial wins so an
        // unauthorized caller does not learn current state.
        let p = policy(DispatchCapability::None);
        let mut req = write_request(&p, "67.10");
        req.caps.global_running = 99;
        req.caps.global_max = 1;
        assert_eq!(
            DispatchGate::check(&req).unwrap_err(),
            DispatchDenied::CapabilityNone
        );
    }
}
