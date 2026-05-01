//! Handler context — what tool / hook handlers see at call time.

use std::sync::Arc;

use nexo_tool_meta::BindingContext;
use uuid::Uuid;

#[cfg(feature = "outbound")]
use crate::outbound::OutboundDispatcher;

/// Context passed to every [`crate::ToolHandler`] call.
///
/// Carries the `(agent_id, session_id, binding)` triple parsed
/// from the inbound JSON-RPC `_meta` block plus optional helpers
/// gated behind cargo features (`outbound`).
#[derive(Debug, Clone)]
pub struct ToolCtx {
    /// Stable agent identifier (`agents.yaml.<id>`).
    pub agent_id: String,
    /// Active session UUID. `None` for delegation receive,
    /// heartbeat bootstrap, tests.
    pub session_id: Option<Uuid>,
    /// Inbound binding when matched. `None` for paths without a
    /// binding match.
    pub binding: Option<BindingContext>,

    /// Outbound dispatcher — only available with the `outbound`
    /// feature on. Compile-time gate (no runtime
    /// `FeatureDisabled` error path).
    #[cfg(feature = "outbound")]
    pub(crate) outbound: Arc<OutboundDispatcher>,

    // Hold even when feature off so future fields can be added
    // semver-minor; the field is private and unused.
    #[cfg(not(feature = "outbound"))]
    #[allow(dead_code)]
    pub(crate) _outbound_marker: std::marker::PhantomData<Arc<()>>,
}

impl ToolCtx {
    /// Borrow the parsed [`BindingContext`] when the inbound
    /// matched a binding. Returns `None` for delegation receive,
    /// heartbeat bootstrap, tests.
    pub fn binding(&self) -> Option<&BindingContext> {
        self.binding.as_ref()
    }

    /// Borrow the outbound dispatcher.
    ///
    /// Only available with the `outbound` cargo feature on. Calling
    /// without the feature is a compile error.
    #[cfg(feature = "outbound")]
    pub fn outbound(&self) -> &OutboundDispatcher {
        &self.outbound
    }
}

/// Context passed to every [`crate::HookHandler`] call.
#[derive(Debug, Clone)]
pub struct HookCtx {
    /// Stable agent identifier.
    pub agent_id: String,
    /// Inbound binding when matched. `None` for paths without a
    /// binding match.
    pub binding: Option<BindingContext>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::BindingContext;

    fn ctx_with_binding(b: Option<BindingContext>) -> ToolCtx {
        ToolCtx {
            agent_id: "ana".into(),
            session_id: None,
            binding: b,
            #[cfg(not(feature = "outbound"))]
            _outbound_marker: std::marker::PhantomData,
            #[cfg(feature = "outbound")]
            outbound: Arc::new(OutboundDispatcher::new_stub()),
        }
    }

    #[test]
    fn binding_accessor_returns_some_when_present() {
        let b = BindingContext::agent_only("ana");
        let ctx = ctx_with_binding(Some(b.clone()));
        assert_eq!(ctx.binding(), Some(&b));
    }

    #[test]
    fn binding_accessor_returns_none_when_absent() {
        let ctx = ctx_with_binding(None);
        assert!(ctx.binding().is_none());
    }

    #[test]
    fn hook_ctx_round_trip() {
        let b = BindingContext::agent_only("ana");
        let h = HookCtx {
            agent_id: "ana".into(),
            binding: Some(b.clone()),
        };
        assert_eq!(h.binding, Some(b));
    }
}
