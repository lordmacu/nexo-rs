//! Outbound reply transform pipeline.
//!
//! Every agent reply that the framework is about to publish on a
//! channel topic first runs through a chain of
//! [`OutboundReplyTransformer`]s. Each transformer receives the
//! current `OutboundReplyKind` + read-only context, and returns
//! either a replaced kind or passes through unchanged. The chain
//! is registered in order at boot — typical placement:
//!
//! 1. content-filter / DLP transformer (microapp).
//! 2. voice-mode TTS transformer (microapp).
//! 3. attachment / sticker decorators (microapp).
//!
//! The trait stays channel-agnostic: transformers don't know whether
//! the reply will end on whatsapp or telegram, only what the agent
//! produced. Channel plugins consume the final
//! `OutboundReplyKind` and map it to their native primitives.
//!
//! Errors short-circuit the chain. The reply is dropped (with a
//! warn log) and the operator dashboard surfaces a
//! `transform_failed` event so the lost reply is observable.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_tool_meta::reply_kind::{OutboundReplyContext, OutboundReplyKind};

/// Errors a transformer can surface. Boxed so impls can wrap
/// their own error types without forcing a concrete std error
/// dependency in the trait.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    /// The transformer rejected the reply outright (rate-limit,
    /// policy violation, etc.). Callers MUST drop the reply and
    /// surface the reason.
    #[error("rejected by `{transformer}`: {reason}")]
    Rejected {
        /// Identifier of the transformer that rejected.
        transformer: String,
        /// Operator-facing reason string.
        reason: String,
    },
    /// Backend call (e.g. TTS provider) failed transiently. Caller
    /// MUST drop this reply but the transformer chain itself
    /// remains usable for the next reply.
    #[error("transient failure in `{transformer}`: {source}")]
    Transient {
        /// Identifier of the transformer that failed.
        transformer: String,
        /// Underlying error.
        #[source]
        source: anyhow::Error,
    },
    /// Configuration / programming bug. Callers MUST log loud and
    /// drop.
    #[error("internal error in `{transformer}`: {source}")]
    Internal {
        /// Identifier of the transformer that errored.
        transformer: String,
        /// Underlying error.
        #[source]
        source: anyhow::Error,
    },
}

/// Channel-agnostic hook the framework runs before dispatching the
/// agent's outbound reply. Implementations live in microapps,
/// extensions, or the daemon itself.
///
/// Implementations MUST be cheap on the no-op path because every
/// reply pays the chain cost — short-circuit early when the
/// transformer doesn't apply (e.g. voice-mode disabled for this
/// conversation).
#[async_trait]
pub trait OutboundReplyTransformer: Send + Sync + 'static {
    /// Stable identifier surfaced in tracing + error wrapping.
    /// Should be slug-shaped (`voice_mode`, `pii_redactor`).
    fn id(&self) -> &str;

    /// Inspect or rewrite the reply. Returning the input unchanged
    /// is the no-op path. The framework hands ownership so impls
    /// can mutate-and-return without cloning.
    async fn transform(
        &self,
        ctx: &OutboundReplyContext,
        reply: OutboundReplyKind,
    ) -> Result<OutboundReplyKind, TransformError>;
}

/// Ordered chain of transformers. Cheap to clone (`Arc`), cheap to
/// call (returns the input directly when empty). Wire it onto the
/// agent runtime via the boot-time registration helper exposed in
/// the daemon's setup code.
#[derive(Clone, Default)]
pub struct OutboundReplyTransformChain {
    transformers: Arc<Vec<Arc<dyn OutboundReplyTransformer>>>,
}

impl std::fmt::Debug for OutboundReplyTransformChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundReplyTransformChain")
            .field(
                "ids",
                &self
                    .transformers
                    .iter()
                    .map(|t| t.id().to_string())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl OutboundReplyTransformChain {
    /// Empty chain — every reply passes through unchanged.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from a pre-ordered list of transformers.
    pub fn from_vec(transformers: Vec<Arc<dyn OutboundReplyTransformer>>) -> Self {
        Self {
            transformers: Arc::new(transformers),
        }
    }

    /// `true` when no transformers are registered. Callers can use
    /// this to skip the cloning of `OutboundReplyContext` on the
    /// hot path.
    pub fn is_empty(&self) -> bool {
        self.transformers.is_empty()
    }

    /// Stable transformer ids in registration order — surfaced by
    /// admin diagnostics so operators can see what's wired.
    pub fn ids(&self) -> Vec<String> {
        self.transformers.iter().map(|t| t.id().to_string()).collect()
    }

    /// Run the reply through each transformer in order. The first
    /// `Err` short-circuits the chain.
    pub async fn run(
        &self,
        ctx: &OutboundReplyContext,
        mut reply: OutboundReplyKind,
    ) -> Result<OutboundReplyKind, TransformError> {
        for t in self.transformers.iter() {
            reply = t.transform(ctx, reply).await?;
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UpperCaseText;

    #[async_trait]
    impl OutboundReplyTransformer for UpperCaseText {
        fn id(&self) -> &str {
            "uppercase_text"
        }
        async fn transform(
            &self,
            _ctx: &OutboundReplyContext,
            reply: OutboundReplyKind,
        ) -> Result<OutboundReplyKind, TransformError> {
            match reply {
                OutboundReplyKind::Text { body } => Ok(OutboundReplyKind::Text {
                    body: body.to_uppercase(),
                }),
                other => Ok(other),
            }
        }
    }

    fn ctx() -> OutboundReplyContext {
        OutboundReplyContext {
            agent_id: "ana".into(),
            session_id: "00000000-0000-0000-0000-000000000000".into(),
            channel: "whatsapp".into(),
            instance: Some("smoketest".into()),
            recipient: Some("57300@s.whatsapp.net".into()),
            tenant_id: None,
            conversation_key: "ana:session:00000000-0000-0000-0000-000000000000".into(),
            language: None,
        }
    }

    #[tokio::test]
    async fn empty_chain_passes_through() {
        let chain = OutboundReplyTransformChain::empty();
        let out = chain
            .run(&ctx(), OutboundReplyKind::text("hola"))
            .await
            .unwrap();
        assert_eq!(out, OutboundReplyKind::text("hola"));
    }

    #[tokio::test]
    async fn chain_runs_in_order() {
        let chain = OutboundReplyTransformChain::from_vec(vec![Arc::new(UpperCaseText)]);
        let out = chain
            .run(&ctx(), OutboundReplyKind::text("hola"))
            .await
            .unwrap();
        assert_eq!(out, OutboundReplyKind::text("HOLA"));
    }
}
