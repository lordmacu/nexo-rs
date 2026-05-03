//! Phase 82.10.m — operator identity stamping registry.
//!
//! Canonical list of admin RPC methods whose `params` carry an
//! `operator_token_hash: String` field (defined by the
//! [`processing`](super::processing) and
//! [`escalations`](super::escalations) wire shapes).
//!
//! This list is the single source of truth: SDK clients consult
//! it to transparently stamp identity onto outbound calls so
//! microapps no longer duplicate the list locally. Adding a new
//! method that takes `operator_token_hash` requires:
//!   1. Adding the field to its wire-shape struct.
//!   2. Adding its method literal to [`OPERATOR_STAMPED_METHODS`].
//!   3. Rebuilding the SDK + every consuming microapp.
//!
//! The override is unconditional (defense-in-depth): the SDK
//! replaces any caller-supplied value with the authenticated
//! server-computed value.

use super::escalations::ESCALATIONS_RESOLVE_METHOD;
use super::processing::{
    PROCESSING_INTERVENTION_METHOD, PROCESSING_PAUSE_METHOD, PROCESSING_RESUME_METHOD,
};

/// Methods whose `params` object MUST contain an
/// `operator_token_hash: String` field.
pub const OPERATOR_STAMPED_METHODS: &[&str] = &[
    PROCESSING_PAUSE_METHOD,
    PROCESSING_RESUME_METHOD,
    PROCESSING_INTERVENTION_METHOD,
    ESCALATIONS_RESOLVE_METHOD,
];

/// True if `method` is in [`OPERATOR_STAMPED_METHODS`].
pub fn is_operator_stamped(method: &str) -> bool {
    OPERATOR_STAMPED_METHODS.contains(&method)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn list_is_non_empty_and_has_no_duplicates() {
        assert!(!OPERATOR_STAMPED_METHODS.is_empty());
        let unique: HashSet<&&str> = OPERATOR_STAMPED_METHODS.iter().collect();
        assert_eq!(
            unique.len(),
            OPERATOR_STAMPED_METHODS.len(),
            "duplicate method literal in OPERATOR_STAMPED_METHODS"
        );
    }

    #[test]
    fn each_method_starts_with_nexo_admin_prefix() {
        for m in OPERATOR_STAMPED_METHODS {
            assert!(
                m.starts_with("nexo/admin/"),
                "method literal {m:?} does not match nexo/admin/* convention"
            );
            assert!(!m.contains(' '), "method literal {m:?} contains whitespace");
        }
    }

    #[test]
    fn is_operator_stamped_returns_true_for_known_methods_false_otherwise() {
        for m in OPERATOR_STAMPED_METHODS {
            assert!(is_operator_stamped(m));
        }
        assert!(!is_operator_stamped("nexo/admin/agents/list"));
        assert!(!is_operator_stamped("nexo/admin/escalations/list"));
        assert!(!is_operator_stamped(""));
    }
}
