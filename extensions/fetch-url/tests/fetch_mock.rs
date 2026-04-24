use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use fetch_url_ext::{client, tools};

fn reset(server: &MockServer) -> String {
    client::reset_state();
    server.uri()
}

async fn dispatch(
    name: &'static str,
    args: serde_json::Value,
) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn get_returns_body_text_for_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({ "login": "octocat", "id": 1 })),
        )
        .mount(&server)
        .await;
    let base = reset(&server);
    let url = format!("{base}/users/1");

    let out = dispatch(
        "fetch_url",
        json!({ "url": url, "allow_private": true }),
    )
    .await
    .expect("ok");
    assert_eq!(out["status"], 200);
    let text = out["body_text"].as_str().expect("text");
    assert!(text.contains("octocat"));
    assert!(out["body_base64"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn post_sends_body_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/submit"))
        .and(header("content-type", "application/json"))
        .and(header("x-trace", "abc"))
        .respond_with(ResponseTemplate::new(201).set_body_string("{\"created\":true}"))
        .mount(&server)
        .await;
    let base = reset(&server);

    let out = dispatch(
        "fetch_url",
        json!({
            "url": format!("{base}/submit"),
            "method": "POST",
            "headers": {"content-type": "application/json", "x-trace": "abc"},
            "body": "{\"hello\":1}",
            "allow_private": true
        }),
    )
    .await
    .expect("ok");
    assert_eq!(out["status"], 201);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn max_bytes_truncates_large_body() {
    let server = MockServer::start().await;
    let big = "x".repeat(2000);
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string(big),
        )
        .mount(&server)
        .await;
    let base = reset(&server);

    let out = dispatch(
        "fetch_url",
        json!({
            "url": format!("{base}/big"),
            "max_bytes": 100,
            "allow_private": true
        }),
    )
    .await
    .expect("ok");
    assert_eq!(out["status"], 200);
    assert_eq!(out["truncated"], true);
    let text = out["body_text"].as_str().unwrap();
    assert_eq!(text.len(), 100);
    assert!(out["bytes_read"].as_u64().unwrap() > 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_404_returns_structured_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not here"))
        .mount(&server)
        .await;
    let base = reset(&server);

    let err = dispatch(
        "fetch_url",
        json!({ "url": format!("{base}/missing"), "allow_private": true }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32002);
    assert!(err.message.contains("HTTP 404"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn server_5xx_is_retried_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bad"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let base = reset(&server);

    let err = dispatch(
        "fetch_url",
        json!({ "url": format!("{base}/bad"), "allow_private": true }),
    )
    .await
    .unwrap_err();
    // 5xx surface as -32003 after retry exhaustion.
    assert_eq!(err.code, -32003, "got: {:?}", err);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn blocks_private_host_by_default() {
    let server = MockServer::start().await;
    let base = reset(&server);
    // wiremock binds to 127.0.0.1 which is loopback — blocked by guard.
    let err = dispatch(
        "fetch_url",
        json!({ "url": format!("{base}/x") }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32020);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn rejects_non_http_scheme() {
    let err = dispatch(
        "fetch_url",
        json!({ "url": "file:///etc/passwd" }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("scheme"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn rejects_bad_url() {
    let err = dispatch(
        "fetch_url",
        json!({ "url": "not a url" }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_tool_reports_limits() {
    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["policy"]["blocks_private_hosts_by_default"], true);
    assert!(out["limits"]["hard_max_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn binary_content_type_returns_base64() {
    let server = MockServer::start().await;
    let bytes: Vec<u8> = (0u8..=255).collect();
    Mock::given(method("GET"))
        .and(path("/bin"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/octet-stream")
                .set_body_bytes(bytes.clone()),
        )
        .mount(&server)
        .await;
    let base = reset(&server);

    let out = dispatch(
        "fetch_url",
        json!({
            "url": format!("{base}/bin"),
            "allow_private": true
        }),
    )
    .await
    .expect("ok");
    assert_eq!(out["status"], 200);
    assert!(out["body_text"].is_null());
    let b64 = out["body_base64"].as_str().expect("base64");
    assert!(!b64.is_empty());
}
