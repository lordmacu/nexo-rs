//! Phase 100.x.models-probe HTTP smoke: drive `http_fetch_models`
//! against a WireMock `/models` endpoint. Verifies the request
//! shape (bearer auth + attribution headers + path) and the
//! response parse, without touching the live OpenRouter host or the
//! process-wide cache.

use nexo_llm::openrouter_http_fetch_models;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn http_fetch_models_parses_catalogue_over_http() {
    let server = MockServer::start().await;

    let body = r#"{
        "data": [
          { "id": "anthropic/claude-opus-4-7", "name": "Opus 4.7" },
          { "id": "openai/gpt-5", "name": "GPT-5" },
          { "id": "google/gemini-2.5-pro" }
        ]
    }"#;

    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer sk-or-test"))
        .and(header_exists("http-referer"))
        .and(header_exists("x-title"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let slugs = openrouter_http_fetch_models(&server.uri(), "sk-or-test")
        .await
        .expect("models fetch should succeed");
    assert_eq!(
        slugs,
        vec![
            "anthropic/claude-opus-4-7".to_string(),
            "openai/gpt-5".to_string(),
            "google/gemini-2.5-pro".to_string(),
        ]
    );
}

#[tokio::test]
async fn http_fetch_models_propagates_http_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":\"unauthorized\"}"))
        .mount(&server)
        .await;

    let err = openrouter_http_fetch_models(&server.uri(), "bad-key")
        .await
        .expect_err("401 must surface as error");
    assert!(
        err.to_string().contains("/models HTTP"),
        "error should mention the HTTP status, got: {err}"
    );
}

#[tokio::test]
async fn http_fetch_models_trailing_slash_base_url_normalised() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#))
        .mount(&server)
        .await;

    // Base URL with a trailing slash must still hit `/models`, not
    // `//models`.
    let base = format!("{}/", server.uri());
    let slugs = openrouter_http_fetch_models(&base, "k")
        .await
        .expect("trailing-slash base url should normalise");
    assert!(slugs.is_empty());
}
