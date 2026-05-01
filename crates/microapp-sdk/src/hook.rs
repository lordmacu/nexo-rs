//! Hook handler trait + outcome enum.

use async_trait::async_trait;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::ctx::HookCtx;
use crate::errors::ToolError;

/// Decision a hook returns to the daemon.
///
/// `Continue` lets the daemon proceed with whatever the hook
/// observed; `Abort` short-circuits with the operator-supplied
/// reason (rendered to the LLM / logs).
///
/// `#[non_exhaustive]` so future outcomes (e.g. `Reroute`) land
/// non-major.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Default outcome — the daemon proceeds.
    Continue,
    /// Short-circuit with `reason` rendered to the agent / logs.
    Abort {
        /// Human-readable reason surfaced in tracing + LLM context.
        reason: String,
    },
}

/// Wire shape: `{abort: bool, reason?: String}`.
impl Serialize for HookOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            HookOutcome::Continue => {
                map.serialize_entry("abort", &false)?;
            }
            HookOutcome::Abort { reason } => {
                map.serialize_entry("abort", &true)?;
                map.serialize_entry("reason", reason)?;
            }
        }
        map.end()
    }
}

/// Hook handler.
///
/// Implementations branch on the hook name they registered for.
/// Returning `Err(ToolError::...)` flows to the daemon as a
/// JSON-RPC error frame; the daemon falls back to `Continue` so
/// a misbehaving hook never aborts a turn.
#[async_trait]
pub trait HookHandler: Send + Sync {
    /// Invoke the hook with the daemon-supplied `args` (typically
    /// the inbound message body or tool-call arguments).
    async fn call(&self, args: Value, ctx: HookCtx) -> Result<HookOutcome, ToolError>;
}

#[async_trait]
impl<F, Fut> HookHandler for F
where
    F: Fn(Value, HookCtx) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<HookOutcome, ToolError>> + Send,
{
    async fn call(&self, args: Value, ctx: HookCtx) -> Result<HookOutcome, ToolError> {
        (self)(args, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_serialises_to_abort_false() {
        let v = serde_json::to_value(&HookOutcome::Continue).unwrap();
        assert_eq!(v["abort"], false);
        assert!(v.get("reason").is_none());
    }

    #[test]
    fn abort_serialises_with_reason() {
        let v = serde_json::to_value(&HookOutcome::Abort {
            reason: "spam".into(),
        })
        .unwrap();
        assert_eq!(v["abort"], true);
        assert_eq!(v["reason"], "spam");
    }

    #[tokio::test]
    async fn blanket_impl_async_fn() {
        async fn h(_args: Value, _ctx: HookCtx) -> Result<HookOutcome, ToolError> {
            Ok(HookOutcome::Abort {
                reason: "policy".into(),
            })
        }
        let ctx = HookCtx {
            agent_id: "a".into(),
            binding: None,
            inbound: None,
        };
        let out = HookHandler::call(&h, Value::Null, ctx).await.unwrap();
        assert!(matches!(out, HookOutcome::Abort { .. }));
    }

    #[test]
    fn outcome_pattern_match_is_exhaustive_internal_use() {
        // Inside this crate, the enum is exhaustive at compile
        // time. Useful sanity check that we cover every variant
        // here (external crates need a `_ =>` arm).
        let o = HookOutcome::Continue;
        match o {
            HookOutcome::Continue => {}
            HookOutcome::Abort { .. } => {}
            // Within the defining crate `#[non_exhaustive]` does
            // not require a wildcard; this lock-down test fails
            // to compile if a future variant lands without
            // updating this match.
        }
    }
}
