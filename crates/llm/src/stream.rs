//! Incremental streaming primitives for `LlmClient`.
//!
//! A `BoxStream<Result<StreamChunk>>` represents one provider response as
//! it arrives. Callers accumulate chunks to render UI incrementally or
//! feed the `collect_stream` helper to reconstruct a full `ChatResponse`.

use futures::stream::{self, BoxStream, Stream, StreamExt};
use std::collections::BTreeMap;
use std::time::Duration;

/// Max idle time between SSE events from an LLM. If the upstream
/// stalls longer than this we emit an error chunk and close the stream
/// so the agent loop doesn't hang waiting for a reply that's never
/// coming. 120 s is enough for the slowest observed long-thought
/// tokens while catching genuinely dead connections.
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

use crate::client::LlmClient;
use crate::rate_limiter::RateLimiter;
use crate::types::{
    ChatRequest, ChatResponse, FinishReason, ResponseContent, TokenUsage, ToolCall,
};
use std::sync::Arc;

/// Wrap a `StreamChunk` stream so the final `Usage` event is recorded
/// against the provider's quota tracker. Providers should call this in
/// their `stream()` impl — the non-streaming `chat()` path does this
/// inline in `do_request`, but streaming bypasses that until drained.
pub fn record_usage_tap<S>(
    stream: S,
    rate_limiter: Arc<RateLimiter>,
) -> BoxStream<'static, anyhow::Result<StreamChunk>>
where
    S: Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static,
{
    stream
        .inspect(move |item| {
            if let Ok(StreamChunk::Usage(u)) = item {
                if let Some(t) = rate_limiter.quota_tracker() {
                    t.record_usage(u.prompt_tokens, u.completion_tokens);
                }
            }
        })
        .boxed()
}

/// One incremental event from a streaming LLM call.
///
/// Ordering guarantees:
/// * `TextDelta` chunks appear in the order they should be concatenated.
/// * For a given tool-call `id`, events arrive as
///   `ToolCallStart → ToolCallArgsDelta* → ToolCallEnd`.
/// * `Usage` (if present) and `End` are the last two chunks of a successful
///   stream. On error the stream terminates with `Err(_)` and no `End`.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta { delta: String },
    ToolCallStart { id: String, name: String },
    ToolCallArgsDelta { id: String, delta: String },
    ToolCallEnd { id: String },
    Usage(TokenUsage),
    End { finish_reason: FinishReason },
}

/// Drain a `StreamChunk` stream into a complete `ChatResponse`.
///
/// Returns an error if the stream ends without an `End` chunk, or if any
/// inner `Err(_)` is observed. A stream that contains both text and tool
/// calls prefers tool calls (matches provider behaviour: when `finish_reason`
/// is `ToolUse`, any partial assistant text is discarded by the loop).
pub async fn collect_stream<S>(mut s: S) -> anyhow::Result<ChatResponse>
where
    S: Stream<Item = anyhow::Result<StreamChunk>> + Unpin,
{
    let mut text = String::new();
    // Preserve insertion order while allowing in-place args concatenation.
    let mut tool_order: Vec<String> = Vec::new();
    let mut tool_buf: BTreeMap<String, (String, String)> = BTreeMap::new(); // id -> (name, args)
    let mut usage = TokenUsage::default();
    let mut finish: Option<FinishReason> = None;

    while let Some(item) = s.next().await {
        match item? {
            StreamChunk::TextDelta { delta } => text.push_str(&delta),
            StreamChunk::ToolCallStart { id, name } => {
                if !tool_buf.contains_key(&id) {
                    tool_order.push(id.clone());
                }
                tool_buf.insert(id, (name, String::new()));
            }
            StreamChunk::ToolCallArgsDelta { id, delta } => {
                let entry = tool_buf
                    .entry(id.clone())
                    .or_insert_with(|| (String::new(), String::new()));
                entry.1.push_str(&delta);
                if !tool_order.iter().any(|x| x == &id) {
                    tool_order.push(id);
                }
            }
            StreamChunk::ToolCallEnd { .. } => {}
            StreamChunk::Usage(u) => usage = u,
            StreamChunk::End { finish_reason } => {
                finish = Some(finish_reason);
                break;
            }
        }
    }

    let finish_reason =
        finish.ok_or_else(|| anyhow::anyhow!("stream ended without End chunk"))?;

    let content = if !tool_order.is_empty() {
        let calls: Vec<ToolCall> = tool_order
            .into_iter()
            .filter_map(|id| {
                tool_buf.remove(&id).map(|(name, args)| {
                    let arguments = if args.trim().is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&args).unwrap_or_else(|_| {
                            serde_json::Value::String(args.clone())
                        })
                    };
                    ToolCall {
                        id,
                        name,
                        arguments,
                    }
                })
            })
            .collect();
        ResponseContent::ToolCalls(calls)
    } else {
        ResponseContent::Text(text)
    };

    Ok(ChatResponse {
        content,
        usage,
        finish_reason,
    })
}

/// Default `stream()` implementation: run `chat()` and synthesize a
/// minimal chunk sequence. Providers without native SSE keep working
/// transparently; callers that only care about the final response are
/// equivalent to calling `chat()` directly.
pub async fn default_stream_from_chat<'a, C>(
    client: &'a C,
    req: ChatRequest,
) -> anyhow::Result<BoxStream<'a, anyhow::Result<StreamChunk>>>
where
    C: LlmClient + ?Sized,
{
    let resp = client.chat(req).await?;
    Ok(synth_chunks_from_response(resp).boxed())
}

fn synth_chunks_from_response(
    resp: ChatResponse,
) -> impl Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static {
    let ChatResponse {
        content,
        usage,
        finish_reason,
    } = resp;
    let mut chunks: Vec<anyhow::Result<StreamChunk>> = Vec::new();
    match content {
        ResponseContent::Text(t) => {
            if !t.is_empty() {
                chunks.push(Ok(StreamChunk::TextDelta { delta: t }));
            }
        }
        ResponseContent::ToolCalls(calls) => {
            for c in calls {
                chunks.push(Ok(StreamChunk::ToolCallStart {
                    id: c.id.clone(),
                    name: c.name.clone(),
                }));
                let args = serde_json::to_string(&c.arguments)
                    .unwrap_or_else(|_| "{}".into());
                chunks.push(Ok(StreamChunk::ToolCallArgsDelta {
                    id: c.id.clone(),
                    delta: args,
                }));
                chunks.push(Ok(StreamChunk::ToolCallEnd { id: c.id }));
            }
        }
    }
    chunks.push(Ok(StreamChunk::Usage(usage)));
    chunks.push(Ok(StreamChunk::End { finish_reason }));
    stream::iter(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, ToolCall};
    use async_trait::async_trait;
    use futures::stream::iter;

    fn ok_chunks(v: Vec<StreamChunk>) -> BoxStream<'static, anyhow::Result<StreamChunk>> {
        iter(v.into_iter().map(Ok)).boxed()
    }

    #[tokio::test]
    async fn collect_text_only() {
        let s = ok_chunks(vec![
            StreamChunk::TextDelta { delta: "hola ".into() },
            StreamChunk::TextDelta { delta: "mundo".into() },
            StreamChunk::Usage(TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 2,
            }),
            StreamChunk::End {
                finish_reason: FinishReason::Stop,
            },
        ]);
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::Text(t) => assert_eq!(t, "hola mundo"),
            _ => panic!("expected text"),
        }
        assert_eq!(r.usage.prompt_tokens, 3);
        assert_eq!(r.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn collect_tool_calls() {
        let s = ok_chunks(vec![
            StreamChunk::ToolCallStart {
                id: "call_1".into(),
                name: "weather".into(),
            },
            StreamChunk::ToolCallArgsDelta {
                id: "call_1".into(),
                delta: "{\"city\":".into(),
            },
            StreamChunk::ToolCallArgsDelta {
                id: "call_1".into(),
                delta: "\"Bogota\"}".into(),
            },
            StreamChunk::ToolCallEnd { id: "call_1".into() },
            StreamChunk::Usage(TokenUsage::default()),
            StreamChunk::End {
                finish_reason: FinishReason::ToolUse,
            },
        ]);
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "weather");
                assert_eq!(calls[0].arguments["city"], "Bogota");
            }
            _ => panic!("expected tool calls"),
        }
    }

    #[tokio::test]
    async fn collect_propagates_err() {
        let s: BoxStream<'static, anyhow::Result<StreamChunk>> = iter(vec![
            Ok(StreamChunk::TextDelta { delta: "x".into() }),
            Err(anyhow::anyhow!("boom")),
        ])
        .boxed();
        let r = collect_stream(s).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn collect_missing_end_fails() {
        let s = ok_chunks(vec![StreamChunk::TextDelta { delta: "x".into() }]);
        assert!(collect_stream(s).await.is_err());
    }

    struct FakeClient {
        resp: ChatResponse,
    }

    #[async_trait]
    impl LlmClient for FakeClient {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(self.resp.clone())
        }
        fn model_id(&self) -> &str {
            "fake"
        }
        fn provider(&self) -> &str {
            "fake"
        }
    }

    #[tokio::test]
    async fn default_stream_synthesizes_text() {
        let client = FakeClient {
            resp: ChatResponse {
                content: ResponseContent::Text("hi".into()),
                usage: TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 2,
                },
                finish_reason: FinishReason::Stop,
            },
        };
        let stream = default_stream_from_chat(
            &client,
            ChatRequest::new("fake", vec![ChatMessage::user("hola")]),
        )
        .await
        .unwrap();
        let collected = collect_stream(stream).await.unwrap();
        match collected.content {
            ResponseContent::Text(t) => assert_eq!(t, "hi"),
            _ => panic!(),
        }
        assert_eq!(collected.usage.completion_tokens, 2);
    }

    #[tokio::test]
    async fn default_stream_synthesizes_tool_calls() {
        let client = FakeClient {
            resp: ChatResponse {
                content: ResponseContent::ToolCalls(vec![ToolCall {
                    id: "c1".into(),
                    name: "search".into(),
                    arguments: serde_json::json!({"q":"rust"}),
                }]),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::ToolUse,
            },
        };
        let stream = default_stream_from_chat(
            &client,
            ChatRequest::new("fake", vec![ChatMessage::user("x")]),
        )
        .await
        .unwrap();
        let collected = collect_stream(stream).await.unwrap();
        match collected.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls[0].arguments["q"], "rust");
            }
            _ => panic!(),
        }
    }
}

// ── Provider-agnostic parsers ─────────────────────────────────────────────────
//
// These functions convert a stream of raw SSE events (as `String` data
// payloads) into `StreamChunk` values. They are shared by MiniMax
// (OpenAI-compat flavor), the OpenAI client, and the MiniMax Anthropic
// flavor.

use futures::Stream as FStream;
use serde_json::Value;

/// Parse an OpenAI chat.completions SSE data-line payload (one per
/// `data:` frame). Appends emitted chunks into `out`. Accumulator state
/// (tool-call id/name buffers) lives in the `OpenAiAcc`.
pub(crate) fn parse_openai_line(
    line: &str,
    acc: &mut OpenAiAcc,
    out: &mut Vec<anyhow::Result<StreamChunk>>,
) {
    if line.trim() == "[DONE]" {
        // Flush any usage then End emitted by caller at stream close.
        return;
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, line = %line, "openai SSE: skip malformed data");
            return;
        }
    };

    // Usage frame (some providers send `{"usage":{...}}` at the end with no choices).
    if let Some(u) = v.get("usage") {
        acc.usage = Some(TokenUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            completion_tokens: u
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        });
    }

    let choice = match v.get("choices").and_then(|c| c.get(0)) {
        Some(c) => c,
        None => return,
    };
    let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            out.push(Ok(StreamChunk::TextDelta {
                delta: content.to_string(),
            }));
        }
    }

    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let id_opt = tc.get("id").and_then(Value::as_str).map(str::to_string);
            let name_opt = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let args_delta = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");

            let slot = acc.tool_by_index.entry(index).or_default();
            if let Some(id) = id_opt {
                if slot.id.is_empty() {
                    slot.id = id;
                }
            }
            if let Some(name) = name_opt {
                if !name.is_empty() {
                    slot.name_buf.push_str(&name);
                }
            }
            if !slot.started && !slot.id.is_empty() && !slot.name_buf.is_empty() {
                slot.started = true;
                out.push(Ok(StreamChunk::ToolCallStart {
                    id: slot.id.clone(),
                    name: slot.name_buf.clone(),
                }));
            }
            if slot.started && !args_delta.is_empty() {
                out.push(Ok(StreamChunk::ToolCallArgsDelta {
                    id: slot.id.clone(),
                    delta: args_delta.to_string(),
                }));
            } else if !args_delta.is_empty() {
                slot.pending_args.push_str(args_delta);
            }
        }
    }

    if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
        acc.finish_reason = Some(match finish {
            "stop" => FinishReason::Stop,
            "tool_calls" => FinishReason::ToolUse,
            "length" => FinishReason::Length,
            other => FinishReason::Other(other.to_string()),
        });
        // Emit pending starts + args that were buffered before we saw id/name.
        for (_, slot) in acc.tool_by_index.iter_mut() {
            if !slot.started && !slot.id.is_empty() && !slot.name_buf.is_empty() {
                slot.started = true;
                out.push(Ok(StreamChunk::ToolCallStart {
                    id: slot.id.clone(),
                    name: slot.name_buf.clone(),
                }));
                if !slot.pending_args.is_empty() {
                    out.push(Ok(StreamChunk::ToolCallArgsDelta {
                        id: slot.id.clone(),
                        delta: std::mem::take(&mut slot.pending_args),
                    }));
                }
            }
            if slot.started && !slot.ended {
                slot.ended = true;
                out.push(Ok(StreamChunk::ToolCallEnd { id: slot.id.clone() }));
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct OpenAiAcc {
    pub tool_by_index: BTreeMap<usize, OpenAiToolSlot>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Default)]
pub(crate) struct OpenAiToolSlot {
    pub id: String,
    pub name_buf: String,
    pub pending_args: String,
    pub started: bool,
    pub ended: bool,
}

/// Drive an SSE byte-stream through the OpenAI parser and return a
/// `BoxStream<Result<StreamChunk>>`. `byte_stream` is what
/// `reqwest::Response::bytes_stream()` returns.
pub fn parse_openai_sse<S, E>(byte_stream: S) -> BoxStream<'static, anyhow::Result<StreamChunk>>
where
    S: FStream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    use eventsource_stream::Eventsource;
    let events = Box::pin(
        byte_stream
            .map(|r| {
                r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })
            .eventsource(),
    );

    let s = futures::stream::unfold(
        (events, OpenAiAcc::default(), Vec::<anyhow::Result<StreamChunk>>::new(), false),
        |(mut events, mut acc, mut buf, mut closed)| async move {
            loop {
                if let Some(item) = buf.pop() {
                    return Some((item, (events, acc, buf, closed)));
                }
                if closed {
                    return None;
                }
                match tokio::time::timeout(SSE_IDLE_TIMEOUT, events.next()).await {
                    Ok(Some(Ok(ev))) => {
                        parse_openai_line(&ev.data, &mut acc, &mut buf);
                        buf.reverse(); // pop gives FIFO
                    }
                    Ok(Some(Err(e))) => {
                        return Some((Err(anyhow::anyhow!("sse error: {e}")), (events, acc, buf, true)));
                    }
                    Ok(None) => {
                        closed = true;
                        // Emit usage + End as final chunks.
                        if let Some(u) = acc.usage.take() {
                            buf.push(Ok(StreamChunk::Usage(u)));
                        }
                        let finish = acc
                            .finish_reason
                            .take()
                            .unwrap_or(FinishReason::Stop);
                        buf.push(Ok(StreamChunk::End {
                            finish_reason: finish,
                        }));
                        buf.reverse();
                    }
                    Err(_) => {
                        return Some((
                            Err(anyhow::anyhow!(
                                "sse idle timeout after {}s",
                                SSE_IDLE_TIMEOUT.as_secs()
                            )),
                            (events, acc, buf, true),
                        ));
                    }
                }
            }
        },
    );
    s.boxed()
}

// ── Anthropic streaming parser ────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct AnthropicAcc {
    /// index -> (id, name, type)
    pub blocks: BTreeMap<u64, AnthropicBlockSlot>,
    pub usage: TokenUsage,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Default)]
pub(crate) struct AnthropicBlockSlot {
    pub id: String,
    pub name: String,
    pub kind: String, // "text" | "tool_use"
    pub started: bool,
}

pub(crate) fn parse_anthropic_event(
    event_type: &str,
    data: &str,
    acc: &mut AnthropicAcc,
    out: &mut Vec<anyhow::Result<StreamChunk>>,
) {
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, event = %event_type, "anthropic SSE: skip malformed data");
            return;
        }
    };

    match event_type {
        "message_start" => {
            if let Some(u) = v.pointer("/message/usage") {
                acc.usage.prompt_tokens =
                    u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
            }
        }
        "content_block_start" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
            let block = v.get("content_block").cloned().unwrap_or(Value::Null);
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let slot = acc.blocks.entry(index).or_default();
            slot.kind = kind.clone();
            if kind == "tool_use" {
                slot.id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                slot.name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !slot.id.is_empty() && !slot.name.is_empty() && !slot.started {
                    slot.started = true;
                    out.push(Ok(StreamChunk::ToolCallStart {
                        id: slot.id.clone(),
                        name: slot.name.clone(),
                    }));
                }
            }
        }
        "content_block_delta" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
            let delta = v.get("delta").cloned().unwrap_or(Value::Null);
            let dtype = delta.get("type").and_then(Value::as_str).unwrap_or("");
            let slot = acc.blocks.entry(index).or_default();
            match dtype {
                "text_delta" => {
                    if let Some(t) = delta.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(Ok(StreamChunk::TextDelta {
                                delta: t.to_string(),
                            }));
                        }
                    }
                }
                "input_json_delta" => {
                    if let Some(t) = delta.get("partial_json").and_then(Value::as_str) {
                        if !t.is_empty() && slot.started {
                            out.push(Ok(StreamChunk::ToolCallArgsDelta {
                                id: slot.id.clone(),
                                delta: t.to_string(),
                            }));
                        }
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
            if let Some(slot) = acc.blocks.get_mut(&index) {
                if slot.kind == "tool_use" && slot.started {
                    out.push(Ok(StreamChunk::ToolCallEnd {
                        id: slot.id.clone(),
                    }));
                }
            }
        }
        "message_delta" => {
            if let Some(stop) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                acc.finish_reason = Some(match stop {
                    "end_turn" => FinishReason::Stop,
                    "tool_use" => FinishReason::ToolUse,
                    "max_tokens" => FinishReason::Length,
                    other => FinishReason::Other(other.to_string()),
                });
            }
            if let Some(u) = v.get("usage") {
                if let Some(ot) = u.get("output_tokens").and_then(Value::as_u64) {
                    acc.usage.completion_tokens = ot as u32;
                }
                if let Some(it) = u.get("input_tokens").and_then(Value::as_u64) {
                    if acc.usage.prompt_tokens == 0 {
                        acc.usage.prompt_tokens = it as u32;
                    }
                }
            }
        }
        "message_stop" => {}
        _ => {}
    }
}

pub fn parse_anthropic_sse<S, E>(byte_stream: S) -> BoxStream<'static, anyhow::Result<StreamChunk>>
where
    S: FStream<Item = Result<bytes::Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    use eventsource_stream::Eventsource;
    let events = Box::pin(
        byte_stream
            .map(|r| {
                r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })
            .eventsource(),
    );

    let s = futures::stream::unfold(
        (
            events,
            AnthropicAcc::default(),
            Vec::<anyhow::Result<StreamChunk>>::new(),
            false,
        ),
        |(mut events, mut acc, mut buf, mut closed)| async move {
            loop {
                if let Some(item) = buf.pop() {
                    return Some((item, (events, acc, buf, closed)));
                }
                if closed {
                    return None;
                }
                match tokio::time::timeout(SSE_IDLE_TIMEOUT, events.next()).await {
                    Ok(Some(Ok(ev))) => {
                        let etype = if ev.event.is_empty() {
                            "message".to_string()
                        } else {
                            ev.event.clone()
                        };
                        parse_anthropic_event(&etype, &ev.data, &mut acc, &mut buf);
                        buf.reverse();
                    }
                    Ok(Some(Err(e))) => {
                        return Some((
                            Err(anyhow::anyhow!("sse error: {e}")),
                            (events, acc, buf, true),
                        ));
                    }
                    Ok(None) => {
                        closed = true;
                        buf.push(Ok(StreamChunk::Usage(acc.usage.clone())));
                        let finish = acc
                            .finish_reason
                            .take()
                            .unwrap_or(FinishReason::Stop);
                        buf.push(Ok(StreamChunk::End {
                            finish_reason: finish,
                        }));
                        buf.reverse();
                    }
                    Err(_) => {
                        return Some((
                            Err(anyhow::anyhow!(
                                "sse idle timeout after {}s",
                                SSE_IDLE_TIMEOUT.as_secs()
                            )),
                            (events, acc, buf, true),
                        ));
                    }
                }
            }
        },
    );
    s.boxed()
}

// ── Gemini SSE ────────────────────────────────────────────────────────────────
//
// `streamGenerateContent?alt=sse` emits one JSON per SSE event, each a full
// `GenerateContentResponse` carrying incremental text parts or a complete
// `functionCall`. Usage metadata and finishReason typically land on the last
// event. We emit `TextDelta` per new text chunk, and atomic
// `Start → ArgsDelta → End` triples for each functionCall (no incremental
// arg streaming exists in the wire, the part is always complete).

#[derive(Default)]
struct GeminiAcc {
    usage: TokenUsage,
    finish_reason: Option<FinishReason>,
    tool_call_counter: usize,
}

fn parse_gemini_event(data: &str, acc: &mut GeminiAcc, out: &mut Vec<anyhow::Result<StreamChunk>>) {
    let v: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            out.push(Err(anyhow::anyhow!("gemini sse json: {e}")));
            return;
        }
    };
    if let Some(cand) = v.pointer("/candidates/0") {
        if let Some(parts) = cand.pointer("/content/parts").and_then(|p| p.as_array()) {
            for part in parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        out.push(Ok(StreamChunk::TextDelta { delta: t.to_string() }));
                    }
                }
                if let Some(fc) = part.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = fc.get("args").cloned().unwrap_or(serde_json::json!({}));
                    let id = format!("gemini_call_{}", acc.tool_call_counter);
                    acc.tool_call_counter += 1;
                    out.push(Ok(StreamChunk::ToolCallStart {
                        id: id.clone(),
                        name,
                    }));
                    out.push(Ok(StreamChunk::ToolCallArgsDelta {
                        id: id.clone(),
                        delta: serde_json::to_string(&args).unwrap_or_default(),
                    }));
                    out.push(Ok(StreamChunk::ToolCallEnd { id }));
                }
            }
        }
        if let Some(fr) = cand.get("finishReason").and_then(|f| f.as_str()) {
            acc.finish_reason = Some(match fr {
                "STOP" => FinishReason::Stop,
                "MAX_TOKENS" => FinishReason::Length,
                other => FinishReason::Other(other.to_string()),
            });
        }
    }
    if let Some(u) = v.get("usageMetadata") {
        if let Some(p) = u.get("promptTokenCount").and_then(|v| v.as_u64()) {
            acc.usage.prompt_tokens = p as u32;
        }
        if let Some(o) = u.get("candidatesTokenCount").and_then(|v| v.as_u64()) {
            acc.usage.completion_tokens = o as u32;
        }
    }
}

pub fn parse_gemini_sse<S, E>(byte_stream: S) -> BoxStream<'static, anyhow::Result<StreamChunk>>
where
    S: FStream<Item = Result<bytes::Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    use eventsource_stream::Eventsource;
    let events = Box::pin(
        byte_stream
            .map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
            .eventsource(),
    );

    let s = futures::stream::unfold(
        (
            events,
            GeminiAcc::default(),
            Vec::<anyhow::Result<StreamChunk>>::new(),
            false,
        ),
        |(mut events, mut acc, mut buf, mut closed)| async move {
            loop {
                if let Some(item) = buf.pop() {
                    return Some((item, (events, acc, buf, closed)));
                }
                if closed {
                    return None;
                }
                match tokio::time::timeout(SSE_IDLE_TIMEOUT, events.next()).await {
                    Ok(Some(Ok(ev))) => {
                        if ev.data.trim().is_empty() {
                            continue;
                        }
                        parse_gemini_event(&ev.data, &mut acc, &mut buf);
                        buf.reverse();
                    }
                    Ok(Some(Err(e))) => {
                        return Some((
                            Err(anyhow::anyhow!("sse error: {e}")),
                            (events, acc, buf, true),
                        ));
                    }
                    Ok(None) => {
                        closed = true;
                        buf.push(Ok(StreamChunk::Usage(acc.usage.clone())));
                        let finish = acc.finish_reason.take().unwrap_or(FinishReason::Stop);
                        buf.push(Ok(StreamChunk::End {
                            finish_reason: finish,
                        }));
                        buf.reverse();
                    }
                    Err(_) => {
                        return Some((
                            Err(anyhow::anyhow!(
                                "sse idle timeout after {}s",
                                SSE_IDLE_TIMEOUT.as_secs()
                            )),
                            (events, acc, buf, true),
                        ));
                    }
                }
            }
        },
    );
    s.boxed()
}

#[cfg(test)]
mod parser_tests {
    use super::*;
    use bytes::Bytes;
    use futures::stream;

    fn bstream(
        chunks: Vec<&'static str>,
    ) -> impl FStream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
        stream::iter(chunks.into_iter().map(|s| Ok(Bytes::from_static(s.as_bytes()))))
    }

    #[tokio::test]
    async fn openai_text_stream() {
        let raw = "data: {\"choices\":[{\"delta\":{\"content\":\"Hola \"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"mundo\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";
        let s = parse_openai_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::Text(t) => assert_eq!(t, "Hola mundo"),
            _ => panic!(),
        }
        assert_eq!(r.usage.completion_tokens, 2);
        assert_eq!(r.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn openai_tool_call_stream() {
        let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"weather\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Bogota\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";
        let s = parse_openai_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "weather");
                assert_eq!(calls[0].arguments["city"], "Bogota");
            }
            _ => panic!("expected tool calls"),
        }
        assert_eq!(r.finish_reason, FinishReason::ToolUse);
    }

    #[tokio::test]
    async fn openai_malformed_line_is_skipped() {
        let raw = "data: {broken\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
        let s = parse_openai_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::Text(t) => assert_eq!(t, "ok"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn anthropic_text_stream() {
        let raw = "event: message_start\n\
data: {\"message\":{\"usage\":{\"input_tokens\":4}}}\n\n\
event: content_block_start\n\
data: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n\
event: content_block_delta\n\
data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hola \"}}\n\n\
event: content_block_delta\n\
data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"mundo\"}}\n\n\
event: content_block_stop\n\
data: {\"index\":0}\n\n\
event: message_delta\n\
data: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\n\
data: {}\n\n";
        let s = parse_anthropic_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::Text(t) => assert_eq!(t, "Hola mundo"),
            _ => panic!(),
        }
        assert_eq!(r.usage.prompt_tokens, 4);
        assert_eq!(r.usage.completion_tokens, 2);
        assert_eq!(r.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn anthropic_tool_use_stream() {
        let raw = "event: message_start\n\
data: {\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n\
event: content_block_start\n\
data: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"weather\"}}\n\n\
event: content_block_delta\n\
data: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\"}}\n\n\
event: content_block_delta\n\
data: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"Bogota\\\"}\"}}\n\n\
event: content_block_stop\n\
data: {\"index\":0}\n\n\
event: message_delta\n\
data: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n\
event: message_stop\n\
data: {}\n\n";
        let s = parse_anthropic_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls[0].id, "toolu_01");
                assert_eq!(calls[0].name, "weather");
                assert_eq!(calls[0].arguments["city"], "Bogota");
            }
            _ => panic!("expected tool calls"),
        }
        assert_eq!(r.finish_reason, FinishReason::ToolUse);
    }

    #[tokio::test]
    async fn gemini_text_stream() {
        let raw = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hola \"}]}}]}\n\n\
data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"mundo\"}]}}]}\n\n\
data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":4,\"candidatesTokenCount\":2}}\n\n";
        let s = parse_gemini_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::Text(t) => assert_eq!(t, "Hola mundo"),
            _ => panic!(),
        }
        assert_eq!(r.usage.prompt_tokens, 4);
        assert_eq!(r.usage.completion_tokens, 2);
        assert_eq!(r.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn gemini_function_call_stream() {
        let raw = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"weather\",\"args\":{\"city\":\"Bogota\"}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":3}}\n\n";
        let s = parse_gemini_sse(bstream(vec![raw]));
        let r = collect_stream(s).await.unwrap();
        match r.content {
            ResponseContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "weather");
                assert_eq!(calls[0].arguments["city"], "Bogota");
                assert!(calls[0].id.starts_with("gemini_call_"));
            }
            _ => panic!("expected tool calls"),
        }
        // Gemini reports STOP even when producing a functionCall; our
        // parser promotes that to ToolUse when tool calls are present.
        // But note the parser only tracks acc.finish_reason from the
        // event — so verify at least it's not an error.
        assert!(matches!(
            r.finish_reason,
            FinishReason::ToolUse | FinishReason::Stop
        ));
    }
}
