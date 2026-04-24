use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use summarize::{client, tools};

fn set_endpoint(server: &MockServer) {
    std::env::set_var("SUMMARIZE_OPENAI_URL", server.uri());
    std::env::set_var("SUMMARIZE_OPENAI_API_KEY", "sk-test-xxx");
    std::env::set_var("SUMMARIZE_HTTP_TIMEOUT_SECS", "3");
    std::env::set_var("SUMMARIZE_MODEL", "gpt-test");
    client::reset_state();
}

fn completion(content: &str) -> serde_json::Value {
    json!({
        "id": "cmpl-1",
        "model": "gpt-test",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    })
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_summarize_text_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("Authorization", "Bearer sk-test-xxx"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("brief summary here")))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch(
        "summarize_text",
        json!({"text": "Long text that needs summarizing.", "length": "short"}),
    )
    .await
    .expect("ok");
    assert_eq!(res["summary"], "brief summary here");
    assert_eq!(res["length"], "short");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_summarize_file_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(completion("file summary")))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let dir = tempdir();
    let p = dir.join("note.txt");
    std::fs::write(&p, "some local file content").expect("write");

    let res = dispatch("summarize_file", json!({"path": p.to_string_lossy()})).await.expect("ok");
    assert_eq!(res["summary"], "file summary");
    assert_eq!(res["bytes"], 23);
    assert_eq!(res["length"], "medium");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "bad key"})))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("summarize_text", json!({"text": "x"})).await.unwrap_err();
    assert_eq!(err.code, -32011);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_5xx_retried_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("summarize_text", json!({"text": "x"})).await.unwrap_err();
    assert_eq!(err.code, -32003);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_oversized_text_rejected_locally() {
    let server = MockServer::start().await;
    set_endpoint(&server);

    let big = "a".repeat(60_001);
    let err = dispatch("summarize_text", json!({"text": big})).await.unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("60000"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_empty_completion_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": ""}}]
        })))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("summarize_text", json!({"text": "x"})).await.unwrap_err();
    assert_eq!(err.code, -32007);
}

fn tempdir() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("summarize-test-{}", std::process::id()));
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}
