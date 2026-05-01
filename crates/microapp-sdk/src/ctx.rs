//! Handler context — what tool / hook handlers see at call time.

use std::sync::Arc;

use nexo_tool_meta::{BindingContext, InboundMessageMeta};
use uuid::Uuid;

#[cfg(feature = "outbound")]
use crate::outbound::OutboundDispatcher;

/// Context passed to every [`crate::ToolHandler`] call.
///
/// Carries the `(agent_id, session_id, binding, inbound)` tuple
/// parsed from the inbound JSON-RPC `_meta` block plus optional
/// helpers gated behind cargo features (`outbound`).
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
    /// Per-turn inbound message metadata (sender id, msg id,
    /// timestamp, …) when the producer populated it. `None` for
    /// legacy producers and for tests that don't inject one.
    pub inbound: Option<InboundMessageMeta>,

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

    /// Borrow the parsed [`InboundMessageMeta`] when the producer
    /// populated it. Returns `None` for legacy producers not yet
    /// migrated to Phase 82.5 and for tests that didn't inject
    /// one.
    pub fn inbound(&self) -> Option<&InboundMessageMeta> {
        self.inbound.as_ref()
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
    /// Per-turn inbound message metadata when populated by the
    /// producer. Hooks (e.g. `before_message`) read this for
    /// anti-loop / sender-aware decisions before tool dispatch.
    pub inbound: Option<InboundMessageMeta>,
}

impl HookCtx {
    /// Borrow the parsed [`InboundMessageMeta`] when the producer
    /// populated it.
    pub fn inbound(&self) -> Option<&InboundMessageMeta> {
        self.inbound.as_ref()
    }
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
            inbound: None,
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
    fn inbound_accessor_returns_none_when_absent() {
        let ctx = ctx_with_binding(None);
        assert!(ctx.inbound().is_none());
    }

    #[test]
    fn inbound_accessor_returns_some_when_present() {
        let inbound = nexo_tool_meta::InboundMessageMeta::external_user("+5491100", "wa.X");
        let mut ctx = ctx_with_binding(None);
        ctx.inbound = Some(inbound.clone());
        assert_eq!(ctx.inbound(), Some(&inbound));
    }

    #[test]
    fn hook_ctx_round_trip() {
        let b = BindingContext::agent_only("ana");
        let h = HookCtx {
            agent_id: "ana".into(),
            binding: Some(b.clone()),
            inbound: None,
        };
        assert_eq!(h.binding, Some(b));
        assert!(h.inbound().is_none());
    }
}
