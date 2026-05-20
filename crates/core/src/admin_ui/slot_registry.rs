//! Phase 99.4 — well-known slot vocabulary + trust-tier gating.
//!
//! A `[plugin.admin_ui]` contribution targets either a core slot
//! (`core.sidebar.integrations`, …) or the plugin's own namespace
//! (`plugin.<id>.<seg>`). The manifest validator (Phase 99.1)
//! checks the slot *name* is well-formed; THIS module owns the
//! runtime *trust policy* — which tier may inject into which slot —
//! consulted by the `plugin_ui/list` aggregator (Phase 99.5).
//!
//! Trust tiers come from install provenance
//! ([`TrustTier`], Phase 98), never self-declared:
//!
//! | Slot                              | Official | CommunityIndexed | Unverified |
//! |-----------------------------------|----------|------------------|------------|
//! | `plugin.<id>.*` (own namespace)   | ✅       | ✅               | ✅         |
//! | `core.sidebar.root`               | ✅       | ✅               | ✅ (banner)|
//! | `core.command_palette.actions`    | ✅       | ✅               | ❌         |
//! | `core.sidebar.{channels,…}`       | ✅       | ⚠️ override      | ❌         |
//! | `core.agent_detail.tabs`          | ✅       | ⚠️ override      | ❌         |
//!
//! ⚠️ = allowed for `CommunityIndexed` only when the operator sets
//! `NEXO_PLUGIN_ADMIN_UI_ALLOW_COMMUNITY_CORE_SLOTS=1` (capability
//! inventory entry). `Unverified` is never allowed into core slots.

use nexo_tool_meta::admin::plugin_discovery::TrustTier;

/// Operator opt-in env var that lets `CommunityIndexed` plugins
/// inject into reserved core slots. Mirrors the
/// `crates/setup/src/capabilities.rs::INVENTORY` entry.
pub const ALLOW_COMMUNITY_CORE_SLOTS_ENV: &str = "NEXO_PLUGIN_ADMIN_UI_ALLOW_COMMUNITY_CORE_SLOTS";

/// Slot prefix a plugin owns outright (`plugin.<id>.<seg>`).
pub const PLUGIN_NAMESPACE_PREFIX: &str = "plugin.";

/// Outcome of gating a `(slot, tier)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotDecision {
    /// Contribution is allowed into the slot.
    Allow,
    /// Dropped — slot is reserved for a higher trust tier.
    DenyTrust,
    /// Dropped — slot name is unknown or closed.
    DenyUnknown,
}

impl SlotDecision {
    /// `true` only for [`SlotDecision::Allow`].
    pub fn is_allowed(self) -> bool {
        matches!(self, SlotDecision::Allow)
    }
}

/// Trust policy for a core slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotAccess {
    /// Any installed plugin, including `Unverified` (UI shows a
    /// review banner for unverified entries).
    Open,
    /// `Official` + `CommunityIndexed`; `Unverified` denied.
    NoUnverified,
    /// `Official` always; `CommunityIndexed` only with the operator
    /// override; `Unverified` denied.
    CoreReserved,
}

/// One well-known slot + its trust policy.
#[derive(Debug, Clone, Copy)]
pub struct SlotMeta {
    /// Canonical slot id (`core.sidebar.integrations`).
    pub id: &'static str,
    access: SlotAccess,
    /// Reserved for a future capability; no contributions accepted
    /// while `true`. All v1 slots are open (`false`).
    closed: bool,
}

/// The frozen v1 core-slot table. The aggregator rejects any core
/// slot not listed here. Plugin-namespaced slots (`plugin.<id>.*`)
/// bypass this table (always allowed — the plugin owns them).
const CORE_SLOTS: &[SlotMeta] = &[
    SlotMeta {
        id: "core.sidebar.root",
        access: SlotAccess::Open,
        closed: false,
    },
    SlotMeta {
        id: "core.command_palette.actions",
        access: SlotAccess::NoUnverified,
        closed: false,
    },
    SlotMeta {
        id: "core.sidebar.channels",
        access: SlotAccess::CoreReserved,
        closed: false,
    },
    SlotMeta {
        id: "core.sidebar.integrations",
        access: SlotAccess::CoreReserved,
        closed: false,
    },
    SlotMeta {
        id: "core.sidebar.settings",
        access: SlotAccess::CoreReserved,
        closed: false,
    },
    SlotMeta {
        id: "core.agent_detail.tabs",
        access: SlotAccess::CoreReserved,
        closed: false,
    },
];

/// Runtime view over the core-slot table. Cheap to construct
/// (borrows the const); the aggregator builds one per `list` call.
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotRegistry;

impl SlotRegistry {
    /// Construct the canonical core-slot registry.
    pub fn core() -> Self {
        SlotRegistry
    }

    /// All known core slot ids (for diagnostics / cross-checks).
    pub fn core_slot_ids(&self) -> impl Iterator<Item = &'static str> {
        CORE_SLOTS.iter().map(|s| s.id)
    }

    /// `true` when `slot` is the plugin's own namespace.
    fn is_plugin_namespace(slot: &str, plugin_id: &str) -> bool {
        // Manifest validation (Phase 99.1) already proved the
        // `plugin.<id>.` prefix matches the owning plugin, so here
        // we only confirm the namespace shape against this plugin.
        let prefix = format!("{PLUGIN_NAMESPACE_PREFIX}{plugin_id}.");
        slot.starts_with(&prefix)
    }

    /// Gate a contribution targeting `slot` from a plugin with the
    /// given `plugin_id` + `tier`. `allow_community_core` is the
    /// boot-time read of [`ALLOW_COMMUNITY_CORE_SLOTS_ENV`].
    pub fn gate(
        &self,
        slot: &str,
        plugin_id: &str,
        tier: TrustTier,
        allow_community_core: bool,
    ) -> SlotDecision {
        // A plugin's own namespace is always allowed (any tier).
        if Self::is_plugin_namespace(slot, plugin_id) {
            return SlotDecision::Allow;
        }
        let Some(meta) = CORE_SLOTS.iter().find(|s| s.id == slot) else {
            return SlotDecision::DenyUnknown;
        };
        if meta.closed {
            return SlotDecision::DenyUnknown;
        }
        match meta.access {
            SlotAccess::Open => SlotDecision::Allow,
            SlotAccess::NoUnverified => match tier {
                TrustTier::Official | TrustTier::CommunityIndexed => SlotDecision::Allow,
                TrustTier::Unverified => SlotDecision::DenyTrust,
            },
            SlotAccess::CoreReserved => match tier {
                TrustTier::Official => SlotDecision::Allow,
                TrustTier::CommunityIndexed => {
                    if allow_community_core {
                        SlotDecision::Allow
                    } else {
                        SlotDecision::DenyTrust
                    }
                }
                TrustTier::Unverified => SlotDecision::DenyTrust,
            },
        }
    }
}

/// Boot-time read of the community-core-slots override env var.
/// `1|true|yes|on` (case-insensitive) enables; anything else (or
/// unset) disables.
pub fn community_core_slots_allowed_from_env() -> bool {
    std::env::var(ALLOW_COMMUNITY_CORE_SLOTS_ENV)
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PID: &str = "google";

    fn gate(slot: &str, tier: TrustTier, allow: bool) -> SlotDecision {
        SlotRegistry::core().gate(slot, PID, tier, allow)
    }

    #[test]
    fn plugin_namespace_allowed_for_every_tier() {
        for tier in [
            TrustTier::Official,
            TrustTier::CommunityIndexed,
            TrustTier::Unverified,
        ] {
            assert_eq!(gate("plugin.google.root", tier, false), SlotDecision::Allow);
            assert_eq!(
                gate("plugin.google.settings", tier, false),
                SlotDecision::Allow
            );
        }
    }

    #[test]
    fn plugin_namespace_for_other_plugin_is_unknown() {
        // `plugin.telegram.root` is not THIS plugin's namespace and
        // not a core slot → unknown. (Manifest validation also
        // rejects this earlier, defense-in-depth.)
        assert_eq!(
            gate("plugin.telegram.root", TrustTier::Official, false),
            SlotDecision::DenyUnknown
        );
    }

    #[test]
    fn sidebar_root_open_to_all_tiers() {
        for tier in [
            TrustTier::Official,
            TrustTier::CommunityIndexed,
            TrustTier::Unverified,
        ] {
            assert_eq!(gate("core.sidebar.root", tier, false), SlotDecision::Allow);
        }
    }

    #[test]
    fn command_palette_denies_unverified_only() {
        assert_eq!(
            gate("core.command_palette.actions", TrustTier::Official, false),
            SlotDecision::Allow
        );
        assert_eq!(
            gate(
                "core.command_palette.actions",
                TrustTier::CommunityIndexed,
                false
            ),
            SlotDecision::Allow
        );
        assert_eq!(
            gate("core.command_palette.actions", TrustTier::Unverified, false),
            SlotDecision::DenyTrust
        );
    }

    #[test]
    fn reserved_slots_official_always_allowed() {
        for slot in [
            "core.sidebar.channels",
            "core.sidebar.integrations",
            "core.sidebar.settings",
            "core.agent_detail.tabs",
        ] {
            assert_eq!(gate(slot, TrustTier::Official, false), SlotDecision::Allow);
        }
    }

    #[test]
    fn reserved_slots_community_denied_without_override() {
        assert_eq!(
            gate(
                "core.sidebar.integrations",
                TrustTier::CommunityIndexed,
                false
            ),
            SlotDecision::DenyTrust
        );
    }

    #[test]
    fn reserved_slots_community_allowed_with_override() {
        assert_eq!(
            gate(
                "core.sidebar.integrations",
                TrustTier::CommunityIndexed,
                true
            ),
            SlotDecision::Allow
        );
        assert_eq!(
            gate("core.agent_detail.tabs", TrustTier::CommunityIndexed, true),
            SlotDecision::Allow
        );
    }

    #[test]
    fn reserved_slots_unverified_denied_even_with_override() {
        assert_eq!(
            gate("core.sidebar.settings", TrustTier::Unverified, true),
            SlotDecision::DenyTrust
        );
    }

    #[test]
    fn unknown_core_slot_denied() {
        assert_eq!(
            gate("core.sidebar.bogus", TrustTier::Official, true),
            SlotDecision::DenyUnknown
        );
        assert_eq!(
            gate("core.totally.made.up", TrustTier::Official, true),
            SlotDecision::DenyUnknown
        );
    }

    #[test]
    fn decision_is_allowed_helper() {
        assert!(SlotDecision::Allow.is_allowed());
        assert!(!SlotDecision::DenyTrust.is_allowed());
        assert!(!SlotDecision::DenyUnknown.is_allowed());
    }

    #[test]
    fn core_slot_ids_match_manifest_list() {
        // Drift guard: the runtime trust table + the manifest's
        // name-validation list must agree on the slot vocabulary.
        let mut runtime: Vec<&str> = SlotRegistry::core().core_slot_ids().collect();
        let mut manifest: Vec<&str> = nexo_plugin_manifest::CORE_SLOTS.to_vec();
        runtime.sort_unstable();
        manifest.sort_unstable();
        assert_eq!(runtime, manifest);
    }

    #[test]
    fn env_parse_truthy_values() {
        for v in ["1", "true", "TRUE", "Yes", "on"] {
            std::env::set_var(ALLOW_COMMUNITY_CORE_SLOTS_ENV, v);
            assert!(community_core_slots_allowed_from_env(), "value {v}");
        }
        std::env::set_var(ALLOW_COMMUNITY_CORE_SLOTS_ENV, "0");
        assert!(!community_core_slots_allowed_from_env());
        std::env::remove_var(ALLOW_COMMUNITY_CORE_SLOTS_ENV);
        assert!(!community_core_slots_allowed_from_env());
    }

    #[test]
    fn every_core_slot_gated_for_every_tier_no_panic() {
        // Exhaustive matrix smoke: all known slots × all tiers ×
        // override on/off never panics and returns a decision.
        for slot in SlotRegistry::core().core_slot_ids() {
            for tier in [
                TrustTier::Official,
                TrustTier::CommunityIndexed,
                TrustTier::Unverified,
            ] {
                for allow in [true, false] {
                    let _ = gate(slot, tier, allow);
                }
            }
        }
    }
}
