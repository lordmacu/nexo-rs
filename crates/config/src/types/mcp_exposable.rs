//! Phase 79.M — MCP exposure parity sweep.
//!
//! Static catalog of which runtime tools may be advertised by
//! `nexo mcp-server` to external MCP clients (Claude Desktop, Cursor,
//! Zed, etc.). Source-of-truth in code, not YAML — operators only
//! get to pick a subset of this slice via `mcp_server.expose_tools`.
//!
//! Three-bucket policy:
//! - `BootKind::Always` — handles available in mcp-server boot
//!   context; safe to register.
//! - `BootKind::FeatureGated` — needs Cargo feature flag.
//! - `BootKind::DeniedByPolicy` — denied by default (Heartbeat,
//!   delegate, RemoteTrigger, etc.). `run_mcp_server` may still
//!   provide explicit operator overrides for selected entries.
//! - `BootKind::Deferred` — wiring postponed to a follow-up sub-phase.

use serde::Serialize;

/// Per-tool security tier. Used for telemetry labels and per-tier
/// rate-limit defaults. Tier is a *policy* property, not an
/// implementation property — we centralise it here so the catalog
/// stays the single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SecurityTier {
    ReadOnly,
    ReadWrite,
    OutboundSideEffect,
    Dangerous,
}

impl SecurityTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
            Self::OutboundSideEffect => "outbound_side_effect",
            Self::Dangerous => "dangerous",
        }
    }
}

/// Boot disposition for an exposable tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootKind {
    /// Handles already available in `McpServerBootContext`.
    Always,
    /// Requires a Cargo feature gate (`feature_gate` is `Some`).
    FeatureGated,
    /// Hard-coded default denial from the dispatcher. Carries a static
    /// reason surfaced in operator warnings.
    DeniedByPolicy { reason: &'static str },
    /// Wiring deferred to a follow-up sub-phase.
    Deferred {
        phase: &'static str,
        reason: &'static str,
    },
}

/// A single entry in the exposable catalog.
#[derive(Debug, Clone, Copy)]
pub struct ExposableToolEntry {
    pub name: &'static str,
    pub tier: SecurityTier,
    pub boot_kind: BootKind,
    /// When `Some(_)`, the entry is only bootable when the named
    /// Cargo feature is enabled. Always paired with
    /// `BootKind::FeatureGated`.
    pub feature_gate: Option<&'static str>,
}

/// Master catalog. Adding a tool here ≠ exposing it — the operator
/// must still list the name in `mcp_server.expose_tools`.
pub static EXPOSABLE_TOOLS: &[ExposableToolEntry] = &[
    // --- Bucket Always (Phase 79.1–.4, .8, .13 — already shipped) ---
    ExposableToolEntry {
        name: "EnterPlanMode",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "ExitPlanMode",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "ToolSearch",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "TodoWrite",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "SyntheticOutput",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "NotebookEdit",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.1 — cron_* ---
    ExposableToolEntry {
        name: "cron_create",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "cron_list",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "cron_delete",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "cron_pause",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "cron_resume",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.2 — mcp_router (matches runtime tool def names) ---
    ExposableToolEntry {
        name: "ListMcpResources",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "ReadMcpResource",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.3 — config_changes_tail ---
    ExposableToolEntry {
        name: "config_changes_tail",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.4 — web_search + web_fetch ---
    // Names match the runtime tool defs (lowercase, snake_case) so the
    // wire-protocol surface stays identical between `nexo run` and
    // `nexo mcp-server`.
    ExposableToolEntry {
        name: "web_search",
        tier: SecurityTier::OutboundSideEffect,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "web_fetch",
        tier: SecurityTier::OutboundSideEffect,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.A — memory checkpoint + history (workspace-git audit) ---
    ExposableToolEntry {
        name: "forge_memory_checkpoint",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "memory_history",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.A — taskflow (cross-session persistent task graph) ---
    ExposableToolEntry {
        name: "taskflow",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // Durable email follow-up orchestration (flow state + retries).
    ExposableToolEntry {
        name: "start_followup",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "check_followup",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "cancel_followup",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- 79.M.A — plan_mode_resolve (operator-side approval resolver) ---
    ExposableToolEntry {
        name: "plan_mode_resolve",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- DeniedByPolicy ---
    ExposableToolEntry {
        name: "Heartbeat",
        tier: SecurityTier::Dangerous,
        boot_kind: BootKind::DeniedByPolicy {
            reason: "internal-timer-only",
        },
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "delegate",
        tier: SecurityTier::Dangerous,
        boot_kind: BootKind::DeniedByPolicy {
            reason: "a2a-only-no-mcp-target",
        },
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "RemoteTrigger",
        tier: SecurityTier::OutboundSideEffect,
        boot_kind: BootKind::DeniedByPolicy {
            reason: "outbound-publisher-needs-binding-context",
        },
        feature_gate: None,
    },
    // --- Deferred (sub-phases 79.M.b/c/d) ---
    ExposableToolEntry {
        name: "TeamCreate",
        tier: SecurityTier::Dangerous,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "TeamDelete",
        tier: SecurityTier::Dangerous,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "TeamSendMessage",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "TeamList",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "TeamStatus",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    ExposableToolEntry {
        name: "Lsp",
        tier: SecurityTier::ReadWrite,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // --- FeatureGated + auth_token-required ---
    // Config self-edit is guarded by THREE locks: (1) the
    // `config-self-edit` Cargo feature must be on at compile time,
    // (2) the operator must add `Config` to `mcp_server.expose_tools`,
    // and (3) `mcp_server.auth_token_env` (or `http.auth.kind`) must
    // be set so a bare client cannot mutate config without a bearer
    // token. The boot dispatcher refuses (3) at server startup
    // when Config is requested without an auth token configured.
    ExposableToolEntry {
        name: "Config",
        tier: SecurityTier::Dangerous,
        boot_kind: BootKind::FeatureGated,
        feature_gate: Some("config-self-edit"),
    },
];

/// Case-sensitive lookup. Returns `None` for names not in the
/// catalog (typo, removed tool, future tool not yet added).
pub fn lookup_exposable(name: &str) -> Option<&'static ExposableToolEntry> {
    EXPOSABLE_TOOLS.iter().find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn lookup_hit() {
        let entry = lookup_exposable("cron_list").expect("cron_list present");
        assert_eq!(entry.tier, SecurityTier::ReadOnly);
        assert!(matches!(entry.boot_kind, BootKind::Always));
    }

    #[test]
    fn lookup_miss() {
        assert!(lookup_exposable("cron_lst").is_none());
        assert!(lookup_exposable("").is_none());
    }

    #[test]
    fn lookup_case_sensitive() {
        // Catalog uses canonical case — lowercase 'cron_list' hits,
        // 'Cron_List' must miss.
        assert!(lookup_exposable("cron_list").is_some());
        assert!(lookup_exposable("Cron_List").is_none());
    }

    #[test]
    fn no_duplicate_names() {
        let mut seen = HashSet::new();
        for entry in EXPOSABLE_TOOLS {
            assert!(
                seen.insert(entry.name),
                "duplicate name in EXPOSABLE_TOOLS: {}",
                entry.name
            );
        }
    }

    #[test]
    fn feature_gated_entries_carry_feature_gate() {
        for entry in EXPOSABLE_TOOLS {
            if matches!(entry.boot_kind, BootKind::FeatureGated) {
                assert!(
                    entry.feature_gate.is_some(),
                    "FeatureGated entry missing feature_gate: {}",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn denied_entries_carry_reason() {
        for entry in EXPOSABLE_TOOLS {
            if let BootKind::DeniedByPolicy { reason } = entry.boot_kind {
                assert!(
                    !reason.is_empty(),
                    "DeniedByPolicy entry has empty reason: {}",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn deferred_entries_cite_phase() {
        for entry in EXPOSABLE_TOOLS {
            if let BootKind::Deferred { phase, reason } = entry.boot_kind {
                assert!(!phase.is_empty(), "deferred phase empty: {}", entry.name);
                assert!(!reason.is_empty(), "deferred reason empty: {}", entry.name);
            }
        }
    }

    #[test]
    fn tier_str_round_trip() {
        assert_eq!(SecurityTier::ReadOnly.as_str(), "read_only");
        assert_eq!(SecurityTier::ReadWrite.as_str(), "read_write");
        assert_eq!(
            SecurityTier::OutboundSideEffect.as_str(),
            "outbound_side_effect"
        );
        assert_eq!(SecurityTier::Dangerous.as_str(), "dangerous");
    }
}
