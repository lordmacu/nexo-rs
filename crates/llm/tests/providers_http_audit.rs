//! HTTP-level audit tests for the Anthropic and Gemini providers.
//!
//! Covers the correctness fixes applied in the 2026-04-24 audit:
//!   - streaming retries on 429 / 5xx
//!   - streaming records usage via `record_usage_tap`
//!   - chat parse errors include the raw body
//!   - Gemini embed() honours circuit breaker + retry
//!   - validate_request rejects zero `max_tokens`

use std::sync::Arc;

use nexo_config::types::llm::{LlmProviderConfig, RateLimitConfig, RetryConfig};
use nexo_llm::types::{ChatMessage, ChatRequest};
use nexo_llm::{
    collect_stream, AnthropicClient, GeminiClient, LlmClient, MiniMaxClient, OpenAiClient,
    ResponseContent,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_for(base_url: String) -> LlmProviderConfig {
    LlmProviderConfig {
        api_key: "test-key".into(),
        base_url,
        group_id: None,
        rate_limit: RateLimitConfig {
            requests_per_second: 1000.0,
            quota_alert_threshold: Some(100_000),
        },
        auth: None,
        api_flavor: None,
        embedding_model: None,
        safety_settings: None,
    }
}

fn tight_retry() -> RetryConfig {
    RetryConfig {
        max_attempts: 3,
        initial_backoff_ms: 10,
        max_backoff_ms: 50,
        backoff_multiplier: 1.0,
    }
}

fn user_req(model: &str) -> ChatRequest {
    ChatRequest::new(model, vec![ChatMessage::user("hi")])
}

// ── Anthropic ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_chat_retries_on_429_and_succeeds() {
    let server = MockServer::start().await;

    // First two requests: 429 with short retry-after.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_string("{\"type\":\"error\"}"),
        )
        .up_to_n_times(2)
        .mount(&server)
        .await;

    // Third request: success.
    let success = serde_json::json!({
        "content": [{"type":"text","text":"pong"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let client = AnthropicClient::new(&cfg_for(server.uri()), "claude-sonnet-4", tight_retry()).unwrap();
    let resp = client.chat(user_req("claude-sonnet-4")).await.unwrap();
    match resp.content {
        ResponseContent::Text(t) => assert_eq!(t, "pong"),
        _ => panic!("expected text"),
    }
    assert_eq!(resp.usage.prompt_tokens, 5);
    assert_eq!(resp.usage.completion_tokens, 3);
}

#[tokio::test]
async fn anthropic_stream_retries_on_500_and_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream flaky"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let sse = "event: message_start\ndata: {\"message\":{\"id\":\"m1\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":4,\"output_tokens\":0}}}\n\n\
event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
event: content_block_stop\ndata: {\"index\":0}\n\n\
event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\ndata: {}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            // `set_body_string` forces Content-Type to `text/plain`
            // regardless of insert_header order. Use `set_body_raw`
            // so the SSE content-type sticks for the validator.
            ResponseTemplate::new(200)
                .set_body_raw(sse.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = AnthropicClient::new(&cfg_for(server.uri()), "claude-sonnet-4", tight_retry()).unwrap();
    let stream = client.stream(user_req("claude-sonnet-4")).await.unwrap();
    let resp = collect_stream(stream).await.unwrap();
    match resp.content {
        ResponseContent::Text(t) => assert_eq!(t, "hi"),
        _ => panic!("expected text"),
    }
}

#[tokio::test]
async fn anthropic_chat_parse_error_includes_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string("<html>maintenance</html>"),
        )
        .mount(&server)
        .await;
    let client = AnthropicClient::new(&cfg_for(server.uri()), "claude-sonnet-4", tight_retry()).unwrap();
    let err = client.chat(user_req("claude-sonnet-4")).await.unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.contains("maintenance"),
        "parse error must surface raw body; got: {s}"
    );
}

#[tokio::test]
async fn anthropic_chat_rejects_zero_max_tokens_before_sending() {
    // No mock: any outbound HTTP would connect-refuse the fake URL. If
    // validation works, we never try.
    let client = AnthropicClient::new(
        &cfg_for("http://127.0.0.1:1".into()),
        "claude-sonnet-4",
        tight_retry(),
    )
    .unwrap();
    let mut r = user_req("claude-sonnet-4");
    r.max_tokens = 0;
    let err = client.chat(r).await.unwrap_err();
    assert!(format!("{err:?}").contains("max_tokens must be > 0"));
}

// ── Gemini ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn gemini_chat_retries_on_429_and_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/models/gemini-test:generateContent"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(2)
        .mount(&server)
        .await;

    let success = serde_json::json!({
        "candidates": [{
            "content": {"parts":[{"text":"pong"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 1}
    });
    Mock::given(method("POST"))
        .and(path("/models/gemini-test:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let client = GeminiClient::new(&cfg_for(server.uri()), "gemini-test", tight_retry());
    let resp = client.chat(user_req("gemini-test")).await.unwrap();
    match resp.content {
        ResponseContent::Text(t) => assert_eq!(t, "pong"),
        _ => panic!("expected text"),
    }
    assert_eq!(resp.usage.prompt_tokens, 4);
}

#[tokio::test]
async fn gemini_stream_retries_on_500() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/models/gemini-test:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(500).set_body_string("blip"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let sse = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"yo\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":1}}\n\n";
    Mock::given(method("POST"))
        .and(path("/models/gemini-test:streamGenerateContent"))
        .respond_with(
            // `set_body_string` forces Content-Type to `text/plain`
            // regardless of insert_header order. Use `set_body_raw`
            // so the SSE content-type sticks for the validator.
            ResponseTemplate::new(200)
                .set_body_raw(sse.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = GeminiClient::new(&cfg_for(server.uri()), "gemini-test", tight_retry());
    let stream = client.stream(user_req("gemini-test")).await.unwrap();
    let resp = collect_stream(stream).await.unwrap();
    match resp.content {
        ResponseContent::Text(t) => assert_eq!(t, "yo"),
        _ => panic!("expected text"),
    }
    assert_eq!(resp.usage.prompt_tokens, 2);
}

#[tokio::test]
async fn gemini_embed_retries_on_500() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/models/text-embedding-004:batchEmbedContents"))
        .respond_with(ResponseTemplate::new(500).set_body_string("flaky"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let success = serde_json::json!({
        "embeddings":[{"values":[0.1, 0.2, 0.3]}]
    });
    Mock::given(method("POST"))
        .and(path("/models/text-embedding-004:batchEmbedContents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let client = GeminiClient::new(&cfg_for(server.uri()), "gemini-test", tight_retry());
    let vecs = client.embed(&["hello".to_string()]).await.unwrap();
    assert_eq!(vecs.len(), 1);
    assert_eq!(vecs[0], vec![0.1_f32, 0.2, 0.3]);
}

#[tokio::test]
async fn gemini_chat_parse_error_includes_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/models/gemini-test:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json-at-all"))
        .mount(&server)
        .await;
    let client = GeminiClient::new(&cfg_for(server.uri()), "gemini-test", tight_retry());
    let err = client.chat(user_req("gemini-test")).await.unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.contains("not-json-at-all"),
        "parse error must surface raw body; got: {s}"
    );
}

// ── Retry config guarded by config — sanity that Arc<T> semantics are fine ──

// ── MiniMax (OpenAI-compat flavour) ────────────────────────────────────────

fn minimax_cfg(base_url: String) -> LlmProviderConfig {
    LlmProviderConfig {
        api_key: "test-key".into(),
        base_url,
        group_id: None,
        rate_limit: RateLimitConfig {
            requests_per_second: 1000.0,
            quota_alert_threshold: Some(100_000),
        },
        auth: None,
        api_flavor: Some("openai_compat".into()),
        embedding_model: None,
        safety_settings: None,
    }
}

#[tokio::test]
async fn minimax_chat_retries_on_429_with_retry_after() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/text/chatcompletion_v2"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_string("{}"),
        )
        .up_to_n_times(2)
        .mount(&server)
        .await;

    let success = serde_json::json!({
        "choices": [{
            "message": {"role":"assistant", "content":"pong"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
    });
    Mock::given(method("POST"))
        .and(path("/text/chatcompletion_v2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let client = MiniMaxClient::new(&minimax_cfg(server.uri()), "MiniMax-M2", tight_retry());
    let resp = client.chat(user_req("MiniMax-M2")).await.unwrap();
    match resp.content {
        ResponseContent::Text(t) => assert_eq!(t, "pong"),
        _ => panic!("expected text"),
    }
}

#[tokio::test]
async fn minimax_rejects_zero_max_tokens_before_sending() {
    let client = MiniMaxClient::new(
        &minimax_cfg("http://127.0.0.1:1".into()),
        "MiniMax-M2",
        tight_retry(),
    );
    let mut r = user_req("MiniMax-M2");
    r.max_tokens = 0;
    let err = client.chat(r).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("max_tokens must be > 0"),
        "{err:?}"
    );
}

#[tokio::test]
async fn minimax_chat_parse_error_includes_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/text/chatcompletion_v2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"))
        .mount(&server)
        .await;
    let client = MiniMaxClient::new(&minimax_cfg(server.uri()), "MiniMax-M2", tight_retry());
    let err = client.chat(user_req("MiniMax-M2")).await.unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.contains("maintenance"),
        "minimax parse error must include raw body; got: {s}"
    );
}

#[tokio::test]
async fn minimax_embed_openai_compat_returns_sorted_vectors() {
    let server = MockServer::start().await;
    let success = serde_json::json!({
        "data": [
            {"embedding": [0.2, 0.3], "index": 1},
            {"embedding": [0.1], "index": 0}
        ]
    });
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let client = MiniMaxClient::new(&minimax_cfg(server.uri()), "MiniMax-M2", tight_retry());
    let vecs = client
        .embed(&["hola".to_string(), "mundo".to_string()])
        .await
        .unwrap();
    assert_eq!(vecs, vec![vec![0.1_f32], vec![0.2_f32, 0.3_f32]]);
}

#[tokio::test]
async fn minimax_embed_rejects_anthropic_flavor() {
    let mut cfg = minimax_cfg("http://127.0.0.1:1".into());
    cfg.api_flavor = Some("anthropic_messages".into());
    let client = MiniMaxClient::new(&cfg, "MiniMax-M2", tight_retry());
    let err = client.embed(&["hola".to_string()]).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("not supported"),
        "expected unsupported error, got: {err:?}"
    );
}

// ── OpenAI-compat ───────────────────────────────────────────────────────────

#[tokio::test]
async fn openai_embed_retries_on_500() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("blip"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let success = serde_json::json!({
        "data": [{"embedding": [0.1, 0.2], "index": 0}]
    });
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let client = OpenAiClient::new(
        &cfg_for(server.uri()),
        "text-embedding-3-small",
        tight_retry(),
    );
    let vecs = client.embed(&["hi".to_string()]).await.unwrap();
    assert_eq!(vecs[0], vec![0.1_f32, 0.2]);
}

#[tokio::test]
async fn openai_rejects_zero_max_tokens_before_sending() {
    let client = OpenAiClient::new(
        &cfg_for("http://127.0.0.1:1".into()),
        "gpt-4o-mini",
        tight_retry(),
    );
    let mut r = user_req("gpt-4o-mini");
    r.max_tokens = 0;
    let err = client.chat(r).await.unwrap_err();
    assert!(format!("{err:?}").contains("max_tokens must be > 0"));
}

#[test]
fn arc_client_is_usable_from_multiple_tasks() {
    let cfg = cfg_for("http://example.invalid".into());
    let client: Arc<dyn LlmClient> =
        Arc::new(AnthropicClient::new(&cfg, "claude-sonnet-4", tight_retry()).unwrap());
    let client2 = client.clone();
    // Just make sure it compiles and clones; no real call.
    assert_eq!(client.model_id(), client2.model_id());
}
