//! End-to-end wire test: AnthropicClient → wiremock server.
//!
//! Validates the prompt-cache contract that the agent loop relies on:
//!   1. `ChatRequest.system_blocks` becomes a JSON array of text blocks
//!      with `cache_control` placed on the LAST block of each contiguous
//!      same-policy run.
//!   2. `cache_tools=true` puts `cache_control` on the LAST tool of the
//!      sorted tools array.
//!   3. The `anthropic-beta` header carries the prompt-caching beta
//!      (and the extended-TTL beta when an `Ephemeral1h` block is in
//!      play).
//!   4. `usage.cache_creation_input_tokens` / `cache_read_input_tokens`
//!      from the response are parsed into `ChatResponse.cache_usage`,
//!      with `hit_ratio()` returning the expected value.
//!
//! Live providers are exercised by the unit tests in `anthropic.rs`
//! against the same `build_body` / `to_chat_response` paths; this test
//! pins the contract at the HTTP boundary so a future refactor of the
//! request builder cannot silently break cache hits.

use nexo_config::types::llm::{LlmProviderConfig, RateLimitConfig, RetryConfig};
use nexo_llm::types::{ChatMessage, ChatRequest, ToolChoice, ToolDef};
use nexo_llm::{AnthropicClient, CachePolicy, LlmClient, PromptBlock};
use serde_json::{json, Value};
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
        embedding_model: None,
        safety_settings: None,
    }
}

fn req_with_blocks_and_tools() -> ChatRequest {
    ChatRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![ChatMessage::user("hola")],
        tools: vec![
            // Provider sorts alphabetically; declare in non-alphabetical
            // order so we can pin the sort behavior in the assertions.
            ToolDef {
                name: "z_search".into(),
                description: "Search the web".into(),
                parameters: json!({"type": "object"}),
            },
            ToolDef {
                name: "a_send".into(),
                description: "Send a message".into(),
                parameters: json!({"type": "object"}),
            },
        ],
        max_tokens: 256,
        temperature: 0.5,
        system_prompt: None,
        stop_sequences: Vec::new(),
        tool_choice: ToolChoice::Auto,
        system_blocks: vec![
            PromptBlock::cached_long("identity", "You are Ana."),
            PromptBlock::cached_long("skills", "## SKILLS\n- search\n- send"),
            PromptBlock {
                label: "channel_meta",
                text: "Sender: +57…".to_string(),
                cache: CachePolicy::Ephemeral5m,
            },
        ],
        cache_tools: true,
    }
}

#[tokio::test]
async fn anthropic_cache_control_round_trip() {
    let server = MockServer::start().await;

    // Capture the inbound request via a wiremock template that records
    // and replies. The `request_body` method on Mock is not directly
    // available; we use the recording API on MockServer via
    // `received_requests()` after the call.
    let body = json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-5",
        "content": [{"type": "text", "text": "hola"}],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 12,
            "output_tokens": 5,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 8000
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(body.clone()),
        )
        .mount(&server)
        .await;

    let client = AnthropicClient::new(
        &cfg_for(server.uri()),
        "claude-sonnet-4-5",
        RetryConfig::default(),
    )
    .expect("AnthropicClient::new");

    let resp = client
        .chat(req_with_blocks_and_tools())
        .await
        .expect("chat call");

    // ---- Response parsing ----
    let cu = resp.cache_usage.expect("cache_usage populated");
    assert_eq!(cu.cache_read_input_tokens, 8000);
    assert_eq!(cu.cache_creation_input_tokens, 0);
    assert_eq!(cu.input_tokens, 12);
    assert_eq!(cu.output_tokens, 5);
    // 8000 / (8000 + 0 + 12) ≈ 0.998
    assert!(cu.hit_ratio() > 0.99);

    // ---- Request body shape ----
    let recorded = server.received_requests().await.expect("received_requests");
    assert_eq!(recorded.len(), 1, "expected exactly one POST");
    let req = &recorded[0];

    // Beta headers — both basic + extended-TTL must be present because
    // identity + skills are Ephemeral1h.
    let beta = req
        .headers
        .get("anthropic-beta")
        .map(|h| h.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        beta.contains("prompt-caching-2024-07-31"),
        "missing prompt-caching beta: '{beta}'"
    );
    assert!(
        beta.contains("extended-cache-ttl-2025-04-11"),
        "missing extended-ttl beta: '{beta}'"
    );

    let body_val: Value = serde_json::from_slice(&req.body).expect("body parses as json");

    // System should be an array of content blocks (not a flat string)
    // when system_blocks is non-empty.
    let sys = body_val["system"].as_array().expect("system is array");
    assert_eq!(sys.len(), 3);
    assert_eq!(sys[0]["text"], "You are Ana.");
    assert_eq!(sys[1]["text"], "## SKILLS\n- search\n- send");
    assert_eq!(sys[2]["text"], "Sender: +57…");

    // Cache control rule: marker on the LAST block of each contiguous
    // same-policy run. Two consecutive Ephemeral1h → marker on idx 1
    // only. Then Ephemeral5m → its own marker on idx 2.
    assert!(
        sys[0].get("cache_control").is_none(),
        "first 1h block must not carry cache_control (run continues)"
    );
    assert_eq!(sys[1]["cache_control"]["type"], "ephemeral");
    assert_eq!(sys[1]["cache_control"]["ttl"], "1h");
    assert_eq!(sys[2]["cache_control"]["type"], "ephemeral");
    assert!(
        sys[2]["cache_control"].get("ttl").is_none(),
        "5min cache must omit explicit ttl"
    );

    // Tools sorted alphabetically; LAST gets the cache marker.
    let tools = body_val["tools"].as_array().expect("tools is array");
    assert_eq!(tools[0]["name"], "a_send");
    assert_eq!(tools[1]["name"], "z_search");
    assert!(
        tools[0].get("cache_control").is_none(),
        "first tool must not carry cache_control"
    );
    assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    assert_eq!(tools[1]["cache_control"]["ttl"], "1h");
}

#[tokio::test]
async fn anthropic_no_cache_blocks_yields_string_system_and_no_beta() {
    // Negative control: when the caller passes no system_blocks and
    // no cache_tools, the request must look like the legacy shape
    // (system as a flat string, no beta header). Catches regressions
    // where the framework starts emitting cache headers for callers
    // that opted out.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 1}
        })))
        .mount(&server)
        .await;

    let client = AnthropicClient::new(
        &cfg_for(server.uri()),
        "claude-sonnet-4-5",
        RetryConfig::default(),
    )
    .expect("AnthropicClient::new");

    let mut req = ChatRequest::new("claude-sonnet-4-5", vec![ChatMessage::user("hi")]);
    req.system_prompt = Some("be brief".into());
    let resp = client.chat(req).await.expect("chat call");
    assert!(resp.cache_usage.is_none(), "no cache fields → no CacheUsage");

    let recorded = server.received_requests().await.unwrap();
    let r = &recorded[0];
    let beta = r
        .headers
        .get("anthropic-beta")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    assert!(
        beta.is_none() || !beta.as_deref().unwrap_or("").contains("prompt-caching"),
        "no cache beta when caller opted out, got: {beta:?}"
    );
    let body: Value = serde_json::from_slice(&r.body).unwrap();
    assert!(
        body["system"].is_string(),
        "system must be a flat string when no system_blocks"
    );
}
