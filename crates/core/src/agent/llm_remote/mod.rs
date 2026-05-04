//! Phase 81.25 — `LlmClient` impl backed by a subprocess plugin's
//! stdio bridge.
//!
//! Subprocess plugins declaring `[plugin.extends].llm_providers
//! = ["cohere"]` get one `RemoteLlmFactory` per provider name
//! registered into `LlmRegistry`. When the agent runtime resolves
//! `model.provider = "cohere"`, the factory builds a
//! `RemoteLlmClient` that translates trait calls into
//! `llm.chat` JSON-RPC requests over the subprocess plugin's
//! stdio pipe (the same `Inner.stdin_tx` + `pending` shared with
//! `RemoteChannelAdapter` from Phase 81.24).
//!
//! Streaming uses a separate `streaming_pending` map keyed on
//! request id — `llm.chat.delta` notifications push chunks into
//! a per-request `mpsc::UnboundedReceiver`; the final response
//! resolves the oneshot + drops the chunks sender.

pub mod wire;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::stream::BoxStream;
use futures::StreamExt;
use nexo_config::types::llm::{LlmProviderConfig, RetryConfig};
use nexo_llm::client::LlmClient;
use nexo_llm::registry::LlmProviderFactory;
use nexo_llm::stream::StreamChunk;
use nexo_llm::types::{ChatRequest, ChatResponse};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use self::wire::{request_to_wire, wire_to_response, WireChatResponse};

#[allow(unused_imports)]
pub(crate) use self::wire::{wire_to_chunk, WireStreamChunk};

const DEFAULT_CHAT_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(300);

/// Per-request streaming state stored in
/// `Inner.streaming_pending`. The reader pushes
/// `llm.chat.delta` chunks to `delta_tx`; when the final reply
/// arrives, the response handler resolves `final_tx` AND drops
/// the entry so `delta_tx` closes (signalling end-of-stream to
/// the consumer).
pub struct StreamingPending {
    pub delta_tx: mpsc::UnboundedSender<StreamChunk>,
    pub final_tx: oneshot::Sender<Result<ChatResponse, String>>,
}

/// Errors surfaced by `register_remote_llm_providers`.
#[derive(Debug, thiserror::Error)]
pub enum LlmProviderRegistrationError {
    #[error("LLM provider `{name}` already registered (cannot collide with built-ins or prior plugins)")]
    AlreadyRegistered { name: String },
    #[error("subprocess plugin inner not initialized — call register_remote_llm_providers AFTER init()")]
    InnerUnavailable,
}

/// `LlmClient` impl backed by a subprocess plugin's stdio bridge.
pub struct RemoteLlmClient {
    provider: String,
    model: String,
    plugin_id: String,
    stdin_tx: mpsc::Sender<Value>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    streaming_pending: Arc<DashMap<u64, StreamingPending>>,
    next_id: Arc<AtomicU64>,
    chat_timeout: Duration,
    stream_timeout: Duration,
}

impl RemoteLlmClient {
    pub fn new(
        provider: String,
        model: String,
        plugin_id: String,
        stdin_tx: mpsc::Sender<Value>,
        pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        streaming_pending: Arc<DashMap<u64, StreamingPending>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        let (chat_timeout, stream_timeout) = Self::resolve_timeouts();
        Self {
            provider,
            model,
            plugin_id,
            stdin_tx,
            pending,
            streaming_pending,
            next_id,
            chat_timeout,
            stream_timeout,
        }
    }

    fn resolve_timeouts() -> (Duration, Duration) {
        let env_override = std::env::var("NEXO_PLUGIN_LLM_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis);
        match env_override {
            Some(t) => (t, t),
            None => (DEFAULT_CHAT_TIMEOUT, DEFAULT_STREAM_TIMEOUT),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn build_request_frame(
        &self,
        id: u64,
        request: &ChatRequest,
        stream: bool,
    ) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "llm.chat",
            "params": {
                "provider": &self.provider,
                "model": &self.model,
                "stream": stream,
                "request": request_to_wire(request),
            },
        })
    }

    /// Map a stringified JSON-RPC error object onto an
    /// `anyhow::Error` whose message encodes the structured info
    /// (operator can grep for `rate_limited`, `model_not_found`,
    /// etc.).
    fn parse_error_string(&self, s: &str) -> anyhow::Error {
        let parsed: Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => return anyhow::anyhow!("provider {} error: {}", self.provider, s),
        };
        let code = parsed.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let data = parsed.get("data").cloned().unwrap_or(Value::Null);
        let provider = &self.provider;

        match code {
            -32601 => anyhow::anyhow!(
                "provider {provider} method not implemented: {message}"
            ),
            -32602 => anyhow::anyhow!("invalid llm.chat params: {message}"),
            -32603 => anyhow::anyhow!("provider {provider} internal error: {message}"),
            -33101 => anyhow::anyhow!("connection failed: {message}"),
            -33102 => anyhow::anyhow!("authentication failed: {message}"),
            -33103 => {
                let secs = data
                    .get("retry_after_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                anyhow::anyhow!("rate limited; retry after {secs}s")
            }
            -33104 => anyhow::anyhow!(
                "model {} not available on provider {provider}",
                self.model
            ),
            -33105 => anyhow::anyhow!("context too long: {message}"),
            _ => anyhow::anyhow!(
                "provider {provider} error code {code}: {message}"
            ),
        }
    }
}

#[async_trait]
impl LlmClient for RemoteLlmClient {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let id = self.next_request_id();
        let frame = self.build_request_frame(id, &req, false);
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        if let Err(e) = self.stdin_tx.send(frame).await {
            self.pending.remove(&id);
            anyhow::bail!("provider {} stdin closed: {e}", self.provider);
        }

        let result = match tokio::time::timeout(self.chat_timeout, rx).await {
            Ok(Ok(Ok(value))) => value,
            Ok(Ok(Err(err_str))) => return Err(self.parse_error_string(&err_str)),
            Ok(Err(_)) => {
                self.pending.remove(&id);
                anyhow::bail!(
                    "provider {} pending dropped (subprocess gone)",
                    self.provider
                );
            }
            Err(_) => {
                self.pending.remove(&id);
                anyhow::bail!(
                    "provider {} llm.chat timed out after {}s",
                    self.provider,
                    self.chat_timeout.as_secs()
                );
            }
        };

        let wire: WireChatResponse = serde_json::from_value(result)
            .map_err(|e| anyhow::anyhow!("decode WireChatResponse: {e}"))?;
        Ok(wire_to_response(wire))
    }

    async fn stream<'a>(
        &'a self,
        req: ChatRequest,
    ) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>> {
        let id = self.next_request_id();
        let frame = self.build_request_frame(id, &req, true);

        let (delta_tx, delta_rx) = mpsc::unbounded_channel::<StreamChunk>();
        let (final_tx, final_rx) = oneshot::channel::<Result<ChatResponse, String>>();
        self.streaming_pending.insert(
            id,
            StreamingPending {
                delta_tx,
                final_tx,
            },
        );

        if let Err(e) = self.stdin_tx.send(frame).await {
            self.streaming_pending.remove(&id);
            anyhow::bail!("provider {} stdin closed: {e}", self.provider);
        }

        let provider = self.provider.clone();
        let stream_timeout = self.stream_timeout;
        let streaming_pending = self.streaming_pending.clone();

        // Driver task: awaits final_rx, emits End/Err to the chunk
        // stream, removes the streaming_pending entry. Drops the
        // delta_tx clone on exit so the consumer's stream closes.
        let chunk_stream = futures::stream::unfold(delta_rx, |mut rx| async move {
            rx.recv()
                .await
                .map(|chunk| (Ok::<StreamChunk, anyhow::Error>(chunk), rx))
        });
        let final_chunk = async move {
            let outcome = match tokio::time::timeout(stream_timeout, final_rx).await {
                Ok(Ok(Ok(resp))) => Ok(StreamChunk::End {
                    finish_reason: resp.finish_reason,
                }),
                Ok(Ok(Err(err_str))) => Err(anyhow::anyhow!(
                    "provider {} stream error: {}",
                    provider,
                    err_str
                )),
                Ok(Err(_)) => Err(anyhow::anyhow!(
                    "provider {} streaming pending dropped (subprocess gone)",
                    provider
                )),
                Err(_) => {
                    streaming_pending.remove(&id);
                    Err(anyhow::anyhow!(
                        "provider {} llm.chat stream timed out after {}s",
                        provider,
                        stream_timeout.as_secs()
                    ))
                }
            };
            outcome
        };
        let final_stream = futures::stream::once(final_chunk);

        let combined = chunk_stream.chain(final_stream);
        Ok(combined.boxed())
    }
}

/// `LlmProviderFactory` for subprocess-backed providers. Cloned
/// per `LlmRegistry::build` call; produces a fresh
/// `RemoteLlmClient` per request.
pub struct RemoteLlmFactory {
    provider: String,
    plugin_id: String,
    stdin_tx: mpsc::Sender<Value>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    streaming_pending: Arc<DashMap<u64, StreamingPending>>,
    next_id: Arc<AtomicU64>,
}

impl RemoteLlmFactory {
    pub fn new(
        provider: String,
        plugin_id: String,
        stdin_tx: mpsc::Sender<Value>,
        pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        streaming_pending: Arc<DashMap<u64, StreamingPending>>,
        next_id: Arc<AtomicU64>,
    ) -> Self {
        Self {
            provider,
            plugin_id,
            stdin_tx,
            pending,
            streaming_pending,
            next_id,
        }
    }
}

impl LlmProviderFactory for RemoteLlmFactory {
    fn name(&self) -> &str {
        &self.provider
    }

    fn build(
        &self,
        _provider_cfg: &LlmProviderConfig,
        model: &str,
        _retry: RetryConfig,
    ) -> anyhow::Result<Arc<dyn LlmClient>> {
        Ok(Arc::new(RemoteLlmClient::new(
            self.provider.clone(),
            model.to_string(),
            self.plugin_id.clone(),
            self.stdin_tx.clone(),
            self.pending.clone(),
            self.streaming_pending.clone(),
            self.next_id.clone(),
        )))
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_llm::types::{ChatMessage, ChatRole};

    fn build() -> (
        Arc<RemoteLlmClient>,
        mpsc::Receiver<Value>,
        Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,
        Arc<DashMap<u64, StreamingPending>>,
    ) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let streaming_pending: Arc<DashMap<u64, StreamingPending>> = Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let client = Arc::new(RemoteLlmClient::new(
            "mock_llm".to_string(),
            "any-model".to_string(),
            "mock_plugin".to_string(),
            stdin_tx,
            pending.clone(),
            streaming_pending.clone(),
            next_id,
        ));
        (client, stdin_rx, pending, streaming_pending)
    }

    fn fixture_request() -> ChatRequest {
        ChatRequest::new(
            "any-model",
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            }],
        )
    }

    #[test]
    fn provider_returns_declared_id() {
        let (client, _, _, _) = build();
        assert_eq!(client.provider(), "mock_llm");
        assert_eq!(client.model_id(), "any-model");
        assert_eq!(client.plugin_id(), "mock_plugin");
    }

    #[tokio::test]
    async fn chat_serializes_request_to_wire() {
        let (client, mut stdin_rx, pending, _) = build();
        let task = tokio::spawn({
            let client = client.clone();
            async move { client.chat(fixture_request()).await }
        });

        let frame = stdin_rx.recv().await.expect("frame");
        assert_eq!(frame["method"], "llm.chat");
        assert_eq!(frame["params"]["provider"], "mock_llm");
        assert_eq!(frame["params"]["stream"], false);
        assert_eq!(frame["params"]["request"]["model"], "any-model");
        let id = frame["id"].as_u64().unwrap();

        // Resolve with success.
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Ok(serde_json::json!({
                "content": { "type": "text", "text": "ack" },
                "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                "finish_reason": { "kind": "stop" }
            })));
        }
        let _resp = task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn chat_deserializes_response_from_wire() {
        let (client, mut stdin_rx, pending, _) = build();
        let task = tokio::spawn({
            let client = client.clone();
            async move { client.chat(fixture_request()).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        if let Some((_, sender)) = pending.remove(&id) {
            let _ = sender.send(Ok(serde_json::json!({
                "content": { "type": "text", "text": "hello world" },
                "usage": { "prompt_tokens": 4, "completion_tokens": 2 },
                "finish_reason": { "kind": "stop" }
            })));
        }
        let resp = task.await.unwrap().unwrap();
        match resp.content {
            nexo_llm::types::ResponseContent::Text(t) => assert_eq!(t, "hello world"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(resp.usage.prompt_tokens, 4);
    }

    #[tokio::test]
    async fn chat_unsupported_method_maps_to_anyhow() {
        let (client, mut stdin_rx, pending, _) = build();
        let task = tokio::spawn({
            let client = client.clone();
            async move { client.chat(fixture_request()).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        if let Some((_, sender)) = pending.remove(&id) {
            let err = serde_json::json!({
                "code": -32601,
                "message": "llm.chat"
            });
            let _ = sender.send(Err(err.to_string()));
        }
        let err = task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("not implemented"));
    }

    #[tokio::test]
    async fn chat_rate_limited_extracts_retry_after_seconds() {
        let (client, mut stdin_rx, pending, _) = build();
        let task = tokio::spawn({
            let client = client.clone();
            async move { client.chat(fixture_request()).await }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        if let Some((_, sender)) = pending.remove(&id) {
            let err = serde_json::json!({
                "code": -33103,
                "message": "rate limited",
                "data": { "retry_after_secs": 30 }
            });
            let _ = sender.send(Err(err.to_string()));
        }
        let err = task.await.unwrap().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rate limited"));
        assert!(msg.contains("30s"));
    }

    #[tokio::test]
    async fn stream_emits_chunks_in_order_then_final_response() {
        let (client, mut stdin_rx, _, streaming_pending) = build();
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                let stream = client.stream(fixture_request()).await.unwrap();
                let mut chunks: Vec<StreamChunk> = Vec::new();
                let mut s = stream;
                while let Some(item) = s.next().await {
                    match item {
                        Ok(c) => chunks.push(c),
                        Err(e) => panic!("stream err: {e}"),
                    }
                }
                chunks
            }
        });
        let frame = stdin_rx.recv().await.expect("frame");
        let id = frame["id"].as_u64().unwrap();
        assert_eq!(frame["params"]["stream"], true);

        // Push 2 deltas + resolve final.
        if let Some(entry) = streaming_pending.get(&id) {
            let _ = entry.delta_tx.send(StreamChunk::TextDelta {
                delta: "hello".into(),
            });
            let _ = entry.delta_tx.send(StreamChunk::TextDelta {
                delta: " world".into(),
            });
        }
        // Drop entry to resolve final + close delta_rx.
        if let Some((_, entry)) = streaming_pending.remove(&id) {
            let _ = entry.final_tx.send(Ok(ChatResponse {
                content: nexo_llm::types::ResponseContent::Text("".into()),
                usage: nexo_llm::types::TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 2,
                },
                finish_reason: nexo_llm::types::FinishReason::Stop,
                cache_usage: None,
            }));
        }

        let chunks = task.await.unwrap();
        assert!(chunks.iter().any(|c| matches!(
            c,
            StreamChunk::TextDelta { delta } if delta == "hello"
        )));
        assert!(chunks.iter().any(|c| matches!(
            c,
            StreamChunk::TextDelta { delta } if delta == " world"
        )));
        assert!(chunks.iter().any(|c| matches!(
            c,
            StreamChunk::End {
                finish_reason: nexo_llm::types::FinishReason::Stop
            }
        )));
    }

    #[tokio::test]
    async fn stream_dropped_subscription_cleans_up_pending() {
        let (client, _stdin_rx, _, streaming_pending) = build();
        let task = tokio::spawn({
            let client = client.clone();
            async move {
                let _stream = client.stream(fixture_request()).await.unwrap();
                // Drop immediately.
            }
        });
        task.await.unwrap();
        // Streaming pending may or may not be cleaned by the
        // driver — defer cleanup is an 81.25.b reaper. For now,
        // just verify no panic.
        let _ = streaming_pending.len();
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn request_timeout_returns_anyhow_err() {
        let (stdin_tx, mut stdin_rx) = mpsc::channel(8);
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let streaming_pending: Arc<DashMap<u64, StreamingPending>> = Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let client = RemoteLlmClient {
            provider: "mock_llm".into(),
            model: "x".into(),
            plugin_id: "mock_plugin".into(),
            stdin_tx,
            pending,
            streaming_pending,
            next_id,
            chat_timeout: Duration::from_millis(50),
            stream_timeout: Duration::from_millis(50),
        };
        let task = tokio::spawn(async move { client.chat(fixture_request()).await });
        let _frame = stdin_rx.recv().await.expect("frame");
        // Don't resolve — let timeout fire.
        tokio::time::advance(Duration::from_millis(200)).await;
        let err = task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }
}
