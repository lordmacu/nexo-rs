//! HTTP-level smoke test: a wiremock server returns SSE bytes,
//! the OpenAI-compat client's `stream()` produces the expected chunks.

use agent_config::types::llm::{LlmProviderConfig, RateLimitConfig, RetryConfig};
use agent_llm::{collect_stream, LlmClient, OpenAiClient, ResponseContent};
use agent_llm::types::{ChatMessage, ChatRequest};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg_for(base_url: String) -> LlmProviderConfig {
    LlmProviderConfig {
        api_key: "test-key".into(),
        base_url,
        group_id: None,
        rate_limit: RateLimitConfig {
            requests_per_second: 1000.0,
            quota_alert_threshold: None,
        },
        auth: None,
        api_flavor: None,
            embedding_model: None, safety_settings: None,
    }
}

#[tokio::test]
async fn openai_stream_parses_sse_over_http() {
    let server = MockServer::start().await;

    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi \"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"world\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body.to_string(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = OpenAiClient::new(
        &cfg_for(server.uri()),
        "gpt-test",
        RetryConfig::default(),
    );
    let stream = client
        .stream(ChatRequest::new(
            "gpt-test",
            vec![ChatMessage::user("hola")],
        ))
        .await
        .unwrap();
    let resp = collect_stream(stream).await.unwrap();
    match resp.content {
        ResponseContent::Text(t) => assert_eq!(t, "hi world"),
        _ => panic!("expected text"),
    }
    assert_eq!(resp.usage.completion_tokens, 2);
}

#[tokio::test]
async fn openai_stream_opens_returns_error_on_400() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
        .mount(&server)
        .await;

    let client = OpenAiClient::new(
        &cfg_for(server.uri()),
        "gpt-test",
        RetryConfig::default(),
    );
    let result = client
        .stream(ChatRequest::new(
            "gpt-test",
            vec![ChatMessage::user("x")],
        ))
        .await;
    match result {
        Ok(_) => panic!("expected error on HTTP 400"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(msg.contains("400"), "error should mention 400: {msg}");
        }
    }
}
