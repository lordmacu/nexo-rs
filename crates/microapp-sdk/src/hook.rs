//! Hook handler trait + outcome enum.

use async_trait::async_trait;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::ctx::HookCtx;
use crate::errors::ToolError;

/// Decision a hook returns to the daemon.
///
/// `Continue` lets the daemon proceed with whatever the hook
/// observed; the other variants are vote-to-block / vote-to-
/// transform decisions (Phase 83.3) — the core remains
/// authoritative and decides whether to apply the vote, with an
/// audit log row written for every applied block / transform.
///
/// `#[non_exhaustive]` so future outcomes (e.g. `Reroute`) land
/// non-major.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Default outcome — the daemon proceeds.
    Continue,
    /// Phase 11 legacy alias of [`HookOutcome::Block`]. Kept so
    /// existing handlers compile unchanged. Wire form serialises
    /// as the legacy `{abort: true, reason: "..."}` shape so a
    /// pre-83.3 daemon parser keeps working; new daemons parse
    /// either shape.
    Abort {
        /// Human-readable reason surfaced in tracing + LLM context.
        reason: String,
    },
    /// Phase 83.3 — vote to block dispatch. Identical wire
    /// semantics to `Abort` but signals an explicit Phase 83.3
    /// decision so the daemon can audit-log the vote separately
    /// from legacy aborts.
    Block {
        /// Operator-visible reason.
        reason: String,
        /// When `true`, the daemon should also suppress any
        /// pending auto-replies for the same conversation
        /// (anti-loop signal). Defaults to `false`.
        do_not_reply_again: bool,
    },
    /// Phase 83.3 — vote to rewrite the inbound body before the
    /// agent sees it. The daemon SHOULD apply the rewrite
    /// (subject to operator policy) and audit-log the diff.
    Transform {
        /// Replacement body the agent will receive in place of
        /// the original inbound.
        transformed_body: String,
        /// Optional reason for the transform. Surfaces in audit
        /// logs and operator UI.
        reason: Option<String>,
        /// When `true`, also suppresses pending auto-replies for
        /// this conversation (same anti-loop signal as `Block`).
        do_not_reply_again: bool,
    },
}

impl HookOutcome {
    /// Convenience: build a `Block` with no anti-loop suppression.
    pub fn block(reason: impl Into<String>) -> Self {
        HookOutcome::Block {
            reason: reason.into(),
            do_not_reply_again: false,
        }
    }

    /// Convenience: build a `Transform` with neither reason nor
    /// anti-loop flag set.
    pub fn transform(body: impl Into<String>) -> Self {
        HookOutcome::Transform {
            transformed_body: body.into(),
            reason: None,
            do_not_reply_again: false,
        }
    }
}

/// Wire shape:
/// - Phase 11 legacy: `{abort: bool, reason?: String}`.
/// - Phase 83.3: `{decision: "allow" | "block" | "transform",
///   reason?: String, transformed_body?: String,
///   do_not_reply_again?: bool}`.
///
/// Both are emitted side-by-side so a pre-83.3 daemon (which
/// reads `abort`) keeps working and a post-83.3 daemon reads the
/// richer `decision` field. Discriminator-on-Continue is
/// `"allow"`.
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
                map.serialize_entry("decision", "allow")?;
            }
            HookOutcome::Abort { reason } => {
                map.serialize_entry("abort", &true)?;
                map.serialize_entry("decision", "block")?;
                map.serialize_entry("reason", reason)?;
            }
            HookOutcome::Block {
                reason,
                do_not_reply_again,
            } => {
                map.serialize_entry("abort", &true)?;
                map.serialize_entry("decision", "block")?;
                map.serialize_entry("reason", reason)?;
                if *do_not_reply_again {
                    map.serialize_entry("do_not_reply_again", &true)?;
                }
            }
            HookOutcome::Transform {
                transformed_body,
                reason,
                do_not_reply_again,
            } => {
                // `abort: false` because the daemon should still
                // dispatch — just with the rewritten body.
                map.serialize_entry("abort", &false)?;
                map.serialize_entry("decision", "transform")?;
                map.serialize_entry("transformed_body", transformed_body)?;
                if let Some(r) = reason {
                    map.serialize_entry("reason", r)?;
                }
                if *do_not_reply_again {
                    map.serialize_entry("do_not_reply_again", &true)?;
                }
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
            #[cfg(feature = "admin")]
            admin: None,
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
            HookOutcome::Block { .. } => {}
            HookOutcome::Transform { .. } => {}
            // Within the defining crate `#[non_exhaustive]` does
            // not require a wildcard; this lock-down test fails
            // to compile if a future variant lands without
            // updating this match.
        }
    }

    // ── Phase 83.3 — vote-to-block / vote-to-transform tests ──

    #[test]
    fn block_serialises_with_decision_field() {
        let out = HookOutcome::Block {
            reason: "anti-loop".into(),
            do_not_reply_again: false,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["decision"], "block");
        assert_eq!(v["reason"], "anti-loop");
        // Legacy field still present so a pre-83.3 daemon parser
        // (which only reads `abort`) keeps working.
        assert_eq!(v["abort"], true);
        // do_not_reply_again omitted when false.
        assert!(v.get("do_not_reply_again").is_none());
    }

    #[test]
    fn block_with_anti_loop_flag_serialises_field() {
        let out = HookOutcome::Block {
            reason: "loop".into(),
            do_not_reply_again: true,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["do_not_reply_again"], true);
    }

    #[test]
    fn transform_serialises_with_body_and_no_abort() {
        let out = HookOutcome::Transform {
            transformed_body: "Hasta luego".into(),
            reason: Some("opt-out keyword".into()),
            do_not_reply_again: true,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["decision"], "transform");
        assert_eq!(v["transformed_body"], "Hasta luego");
        assert_eq!(v["reason"], "opt-out keyword");
        assert_eq!(v["do_not_reply_again"], true);
        // Transform does NOT abort dispatch — body just gets
        // rewritten.
        assert_eq!(v["abort"], false);
    }

    #[test]
    fn transform_without_reason_omits_field() {
        let out = HookOutcome::Transform {
            transformed_body: "redacted".into(),
            reason: None,
            do_not_reply_again: false,
        };
        let v = serde_json::to_value(&out).unwrap();
        assert!(v.get("reason").is_none());
        assert!(v.get("do_not_reply_again").is_none());
    }

    #[test]
    fn continue_serialises_decision_allow() {
        let v = serde_json::to_value(&HookOutcome::Continue).unwrap();
        assert_eq!(v["decision"], "allow");
        assert_eq!(v["abort"], false);
    }

    #[test]
    fn legacy_abort_serialises_decision_block() {
        // Pre-83.3 handlers using `Abort { reason }` get auto-
        // upgraded to a `block` decision on the wire so the new
        // daemon's vote-to-block path triggers, AND the legacy
        // `abort: true` stays for back-compat with old daemons.
        let out = HookOutcome::Abort {
            reason: "spam".into(),
        };
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["decision"], "block");
        assert_eq!(v["abort"], true);
        assert_eq!(v["reason"], "spam");
    }

    #[test]
    fn block_helper_constructor_defaults_to_no_anti_loop() {
        let out = HookOutcome::block("rate-limit");
        assert_eq!(
            out,
            HookOutcome::Block {
                reason: "rate-limit".into(),
                do_not_reply_again: false,
            }
        );
    }

    #[test]
    fn transform_helper_constructor_minimal() {
        let out = HookOutcome::transform("[redacted]");
        assert_eq!(
            out,
            HookOutcome::Transform {
                transformed_body: "[redacted]".into(),
                reason: None,
                do_not_reply_again: false,
            }
        );
    }
}
