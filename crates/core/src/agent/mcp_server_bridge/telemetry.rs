//! Phase 79.M — telemetry counters for the boot dispatcher.
//!
//! Three labels carry the operator-facing signal:
//! - `name` — exposable tool name (high cardinality, ≤ ~30).
//! - `tier` — `SecurityTier` (4 values; useful for "any
//!   `dangerous` tool registered" alerts).
//! - `reason` — skip reason (`denied_by_policy`, `deferred`,
//!   `feature_gate_off`, `infra_missing`, `unknown_name`).

use nexo_config::types::mcp_exposable::SecurityTier;

use crate::telemetry::{inc_mcp_server_tool_registered, inc_mcp_server_tool_skipped};

/// Increment `mcp_server_tool_registered_total{name, tier}`.
pub fn record_registered(name: &str, tier: SecurityTier) {
    inc_mcp_server_tool_registered(name, tier.as_str());
}

/// Increment `mcp_server_tool_skipped_total{name, reason}`.
pub fn record_skipped(name: &str, reason: &'static str) {
    inc_mcp_server_tool_skipped(name, reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{render_prometheus, reset_for_test};

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn metric_names_present_in_prometheus_render() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        let body = render_prometheus(false);
        assert!(body.contains("mcp_server_tool_registered_total"));
        assert!(body.contains("mcp_server_tool_skipped_total"));
    }

    #[test]
    fn registered_increments_with_labels() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        record_registered("cron_list", SecurityTier::ReadOnly);
        record_registered("cron_list", SecurityTier::ReadOnly);
        record_registered("WebSearch", SecurityTier::OutboundSideEffect);
        let body = render_prometheus(false);
        assert!(body
            .contains("mcp_server_tool_registered_total{name=\"cron_list\",tier=\"read_only\"} 2"));
        assert!(body.contains(
            "mcp_server_tool_registered_total{name=\"WebSearch\",tier=\"outbound_side_effect\"} 1"
        ));
    }

    #[test]
    fn skipped_increments_with_reason_label() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_for_test();
        record_skipped("Lsp", "deferred");
        record_skipped("delegate", "denied_by_policy");
        let body = render_prometheus(false);
        assert!(body.contains("mcp_server_tool_skipped_total{name=\"Lsp\",reason=\"deferred\"} 1"));
        assert!(body.contains(
            "mcp_server_tool_skipped_total{name=\"delegate\",reason=\"denied_by_policy\"} 1"
        ));
    }
}
