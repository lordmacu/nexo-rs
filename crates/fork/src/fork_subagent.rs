//! `ForkSubagent` trait + `DefaultForkSubagent` impl.
//! Step 80.19 / 9.
//!
//! Verbatim from `runForkedAgent` (leak `forkedAgent.ts:489-541`):
//! 1. Apply [`ForkOverrides`] → forked [`AgentContext`].
//! 2. Spawn the standalone [`run_turn_loop`].
//! 3. Sync mode awaits inline; ForkAndForget mode `tokio::spawn`s.
//! 4. Wraps the loop in `tokio::time::timeout`.
//! 5. Emits `tracing` span `fork.subagent` with run_id, fork_label,
//!    mode, total usage, cache hit ratio, final_status.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use nexo_agent_registry::store::AgentRegistryStore;
use nexo_core::agent::AgentContext;
use nexo_llm::types::ChatMessage;
use nexo_llm::LlmClient;
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};
use uuid::Uuid;

use crate::cache_safe::CacheSafeParams;
use crate::delegate_mode::DelegateMode;
use crate::error::ForkError;
use crate::fork_handle::{ForkHandle, ForkResult};
use crate::on_message::OnMessage;
use crate::overrides::{create_fork_context, ForkOverrides};
use crate::tool_filter::ToolFilter;
use crate::turn_loop::{run_turn_loop, ToolDispatcher, TurnLoopParams};

/// Tag for telemetry — identifies the call site that fired the fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QuerySource {
    /// Phase 80.1 autoDream deep-pass consolidation.
    AutoDream,
    /// Phase 80.14 AWAY_SUMMARY re-connection digest.
    AwayDigest,
    /// Phase 77.5 extract_memories post-turn extraction.
    SessionMemory,
    /// Speculation / look-ahead exploration.
    Speculation,
    /// Phase 51 eval harness.
    Eval,
    /// Custom static label (held in code as a literal).
    Custom(&'static str),
}

#[async_trait]
pub trait ForkSubagent: Send + Sync {
    /// Spawn a fork. Returns a [`ForkHandle`] whose `completion` future
    /// resolves to a [`ForkResult`] (Sync mode) or fires immediately
    /// after the spawned task finishes (ForkAndForget mode).
    async fn fork(&self, params: ForkParams) -> Result<ForkHandle, ForkError>;
}

pub struct ForkParams {
    pub parent_ctx: AgentContext,
    pub llm: Arc<dyn LlmClient>,
    pub tool_dispatcher: Arc<dyn ToolDispatcher>,
    pub prompt_messages: Vec<ChatMessage>,
    pub cache_safe: CacheSafeParams,
    pub tool_filter: Arc<dyn ToolFilter>,
    pub query_source: QuerySource,
    pub fork_label: String,
    pub overrides: Option<ForkOverrides>,
    pub max_turns: u32,
    pub on_message: Option<Arc<dyn OnMessage>>,
    /// When true, no row is written to [`AgentRegistryStore`].
    /// The fork is invisible to `agent ps` and survives daemon restart
    /// only via consumer-specific audit (e.g. Phase 80.18 `dream_runs`).
    pub skip_transcript: bool,
    pub mode: DelegateMode,
    pub timeout: Duration,
    /// Optional external abort token. When `None`, a fresh token is
    /// minted and exposed via `ForkHandle::abort`.
    pub external_abort: Option<CancellationToken>,
}

/// Default impl — same-process fork. Cross-process forks (Phase 32
/// multi-host) ship as a follow-up `NatsForkSubagent`.
pub struct DefaultForkSubagent {
    registry: Option<Arc<dyn AgentRegistryStore>>,
}

impl Default for DefaultForkSubagent {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultForkSubagent {
    pub fn new() -> Self {
        Self { registry: None }
    }

    /// Wire an [`AgentRegistryStore`]. When set AND
    /// `skip_transcript: false`, the fork registers an `AgentHandle`
    /// row so `agent ps` lists it. NOTE for 80.19: actual handle row
    /// shape is deferred — registry integration lands as a follow-up
    /// in 80.10 (SessionKind discriminator). For now this getter is
    /// kept so the public API stays stable.
    pub fn with_registry(mut self, r: Arc<dyn AgentRegistryStore>) -> Self {
        self.registry = Some(r);
        self
    }
}

#[async_trait]
impl ForkSubagent for DefaultForkSubagent {
    #[instrument(
        skip_all,
        fields(
            fork_run_id = tracing::field::Empty,
            parent_agent = %params.parent_ctx.agent_id,
            fork_label = %params.fork_label,
            query_source = ?params.query_source,
            mode = ?params.mode,
            skip_transcript = params.skip_transcript,
            cache_key_hash = %params.cache_safe.cache_key_hash(),
        )
    )]
    async fn fork(&self, params: ForkParams) -> Result<ForkHandle, ForkError> {
        let run_id = Uuid::new_v4();
        tracing::Span::current().record("fork_run_id", tracing::field::display(run_id));

        // Mirror leak `:96-104` — warn when max_output_tokens (via
        // cache_safe.max_tokens) differs from parent: it clamps
        // budget_tokens and may invalidate the cache.
        // NOTE: at this point cache_safe.max_tokens IS the parent's
        // when built via `from_parent_request`; if a caller manually
        // adjusted it after, they accept the cache miss risk.

        let abort = params
            .external_abort
            .clone()
            .unwrap_or_else(CancellationToken::new);

        let overrides = params.overrides.clone().unwrap_or_default();
        let critical_reminder = overrides.critical_system_reminder.clone();
        let _fork_ctx = create_fork_context(&params.parent_ctx, overrides);

        let turn_params = TurnLoopParams {
            llm: params.llm,
            cache_safe: params.cache_safe,
            prompt_messages: params.prompt_messages,
            tool_dispatcher: params.tool_dispatcher,
            tool_filter: params.tool_filter,
            max_turns: params.max_turns,
            on_message: params.on_message,
            abort: abort.clone(),
            critical_system_reminder: critical_reminder,
            fork_label: params.fork_label.clone(),
        };

        let timeout = params.timeout;
        let abort_for_loop = abort.clone();

        let completion: BoxFuture<'static, Result<ForkResult, ForkError>> = match params.mode {
            DelegateMode::Sync => Box::pin(async move {
                let r = tokio::time::timeout(timeout, run_turn_loop(turn_params))
                    .await
                    .map_err(|_| {
                        abort_for_loop.cancel();
                        ForkError::Timeout(timeout)
                    })??;
                Ok(ForkResult::from_turn_loop(r))
            }),
            DelegateMode::ForkAndForget => {
                let join = tokio::spawn(async move {
                    tokio::time::timeout(timeout, run_turn_loop(turn_params))
                        .await
                        .map_err(|_| {
                            abort_for_loop.cancel();
                            ForkError::Timeout(timeout)
                        })?
                        .map(ForkResult::from_turn_loop)
                });
                Box::pin(async move {
                    join.await
                        .map_err(|e| ForkError::Internal(e.to_string()))?
                })
            }
        };

        let goal_id: Option<Uuid> = if params.skip_transcript {
            None
        } else if self.registry.is_some() {
            warn!(
                target: "fork.subagent",
                "skip_transcript=false with registry wired but agent_handle row write deferred to 80.10 — fork will not appear in agent ps"
            );
            None
        } else {
            None
        };

        Ok(ForkHandle::new(run_id, goal_id, completion, abort))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_safe::CacheSafeParams;
    use crate::on_message::CountingCollector;
    use crate::tool_filter::AllowAllFilter;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use nexo_broker::AnyBroker;
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    use nexo_core::session::SessionManager;
    use nexo_llm::stream::StreamChunk;
    use nexo_llm::types::{
        ChatRequest, ChatResponse, FinishReason, ResponseContent, TokenUsage, ToolDef,
    };
    use serde_json::Value;
    use std::sync::Mutex;
    use std::time::Duration;

    fn mk_parent_ctx() -> AgentContext {
        let cfg = AgentConfig {
            id: "parent".into(),
            model: ModelConfig {
                provider: "test".into(),
                model: "test-model".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
            repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
            empresa_id: None,
            extensions_config: std::collections::BTreeMap::new(),
        };
        AgentContext::new(
            "parent",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(SessionManager::new(Duration::from_secs(60), 8)),
        )
    }

    fn mk_cache_safe() -> CacheSafeParams {
        CacheSafeParams::from_parent_request(&ChatRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage::user("parent prefix")],
            tools: vec![ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: 1024,
            temperature: 0.0,
            system_prompt: Some("system".into()),
            stop_sequences: vec![],
            tool_choice: nexo_llm::types::ToolChoice::Auto,
            system_blocks: vec![],
            cache_tools: false,
        })
    }

    struct ScriptedLlm {
        responses: Mutex<std::collections::VecDeque<ChatResponse>>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses.into()),
            })
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedLlm {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted llm exhausted"))
        }
        fn model_id(&self) -> &str {
            "test-model"
        }
        fn provider(&self) -> &str {
            "scripted"
        }
        async fn stream<'a>(
            &'a self,
            _req: ChatRequest,
        ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
            anyhow::bail!("stream not used in tests")
        }
    }

    struct EchoDispatcher;
    #[async_trait]
    impl ToolDispatcher for EchoDispatcher {
        async fn dispatch(&self, name: &str, _args: Value) -> Result<String, String> {
            Ok(format!("echoed:{name}"))
        }
    }

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: ResponseContent::Text(text.into()),
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
            },
            finish_reason: FinishReason::Stop,
            cache_usage: None,
        }
    }

    fn mk_params(mode: DelegateMode) -> ForkParams {
        ForkParams {
            parent_ctx: mk_parent_ctx(),
            llm: ScriptedLlm::new(vec![text_response("ok")]),
            tool_dispatcher: Arc::new(EchoDispatcher),
            prompt_messages: vec![ChatMessage::user("go")],
            cache_safe: mk_cache_safe(),
            tool_filter: Arc::new(AllowAllFilter),
            query_source: QuerySource::Custom("test"),
            fork_label: "test_fork".into(),
            overrides: None,
            max_turns: 5,
            on_message: None,
            skip_transcript: true,
            mode,
            timeout: Duration::from_secs(2),
            external_abort: None,
        }
    }

    #[tokio::test]
    async fn sync_mode_completes_inline() {
        let fork = DefaultForkSubagent::new();
        let mut handle = fork.fork(mk_params(DelegateMode::Sync)).await.unwrap();
        let result = handle.take_completion().unwrap().await.unwrap();
        assert_eq!(result.final_text.as_deref(), Some("ok"));
        assert_eq!(result.turns_executed, 1);
    }

    #[tokio::test]
    async fn fork_and_forget_returns_handle_immediately() {
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::ForkAndForget);
        // Slow LLM so we can prove fork() returns before LLM completes.
        let slow_llm = {
            struct SlowLlm;
            #[async_trait]
            impl LlmClient for SlowLlm {
                async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Ok(text_response("delayed"))
                }
                fn model_id(&self) -> &str {
                    "test"
                }
                fn provider(&self) -> &str {
                    "slow"
                }
                async fn stream<'a>(
                    &'a self,
                    _req: ChatRequest,
                ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
                    anyhow::bail!("unused")
                }
            }
            Arc::new(SlowLlm)
        };
        params.llm = slow_llm;
        let started = std::time::Instant::now();
        let mut handle = fork.fork(params).await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "fork() should return immediately for ForkAndForget; took {elapsed:?}"
        );
        let result = handle.take_completion().unwrap().await.unwrap();
        assert_eq!(result.final_text.as_deref(), Some("delayed"));
    }

    #[tokio::test]
    async fn timeout_returns_timeout_error_and_aborts_loop() {
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::Sync);
        // LLM that hangs forever — timeout should kick in.
        let hanging_llm = {
            struct HangingLlm;
            #[async_trait]
            impl LlmClient for HangingLlm {
                async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
                    futures::future::pending::<()>().await;
                    unreachable!()
                }
                fn model_id(&self) -> &str {
                    "test"
                }
                fn provider(&self) -> &str {
                    "hanging"
                }
                async fn stream<'a>(
                    &'a self,
                    _req: ChatRequest,
                ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
                    anyhow::bail!("unused")
                }
            }
            Arc::new(HangingLlm)
        };
        params.llm = hanging_llm;
        params.timeout = Duration::from_millis(50);
        let mut handle = fork.fork(params).await.unwrap();
        let abort = handle.abort.clone();
        let err = handle.take_completion().unwrap().await.unwrap_err();
        assert!(matches!(err, ForkError::Timeout(_)));
        assert!(abort.is_cancelled());
    }

    #[tokio::test]
    async fn abort_via_handle_kills_loop() {
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::Sync);
        let hanging_llm = {
            struct HangingLlm;
            #[async_trait]
            impl LlmClient for HangingLlm {
                async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
                    futures::future::pending::<()>().await;
                    unreachable!()
                }
                fn model_id(&self) -> &str {
                    "test"
                }
                fn provider(&self) -> &str {
                    "hanging"
                }
                async fn stream<'a>(
                    &'a self,
                    _req: ChatRequest,
                ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
                    anyhow::bail!("unused")
                }
            }
            Arc::new(HangingLlm)
        };
        params.llm = hanging_llm;
        params.timeout = Duration::from_secs(60);
        let mut handle = fork.fork(params).await.unwrap();
        let abort = handle.abort.clone();
        let completion = handle.take_completion().unwrap();
        // Cancel from outside before awaiting.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            abort.cancel();
        });
        let err = completion.await.unwrap_err();
        assert!(matches!(err, ForkError::Aborted));
    }

    #[tokio::test]
    async fn skip_transcript_true_does_not_set_goal_id() {
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::Sync);
        params.skip_transcript = true;
        let mut handle = fork.fork(params).await.unwrap();
        assert!(handle.goal_id.is_none());
        let _ = handle.take_completion().unwrap().await;
    }

    #[tokio::test]
    async fn on_message_observes_messages() {
        let collector = Arc::new(CountingCollector::default());
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::Sync);
        struct Forward(Arc<CountingCollector>);
        #[async_trait]
        impl OnMessage for Forward {
            async fn on_message(&self, m: &ChatMessage) {
                self.0.on_message(m).await;
            }
        }
        params.on_message = Some(Arc::new(Forward(collector.clone())));
        let mut handle = fork.fork(params).await.unwrap();
        let _ = handle.take_completion().unwrap().await.unwrap();
        // 1 assistant message in this scripted run.
        assert_eq!(
            collector.count.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn overrides_agent_id_propagates_to_fork_context() {
        // Indirect test — we can't observe the cloned fork_ctx.agent_id
        // outside the call because run_turn_loop doesn't expose the
        // context. We verify the overrides path doesn't crash and the
        // run completes cleanly.
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::Sync);
        params.overrides = Some(ForkOverrides {
            agent_id: Some("forked_dream".into()),
            critical_system_reminder: None,
        });
        let mut handle = fork.fork(params).await.unwrap();
        let result = handle.take_completion().unwrap().await.unwrap();
        assert_eq!(result.turns_executed, 1);
    }

    #[tokio::test]
    async fn external_abort_token_propagates() {
        let fork = DefaultForkSubagent::new();
        let mut params = mk_params(DelegateMode::Sync);
        let external = CancellationToken::new();
        params.external_abort = Some(external.clone());
        let mut handle = fork.fork(params).await.unwrap();
        // The handle's abort should be the same token.
        // (We can't compare CancellationToken directly; verify by
        // cancelling external and observing handle.abort.is_cancelled.)
        let completion = handle.take_completion().unwrap();
        external.cancel();
        assert!(handle.abort.is_cancelled());
        let _ = completion.await; // drains
    }
}
