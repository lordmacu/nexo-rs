use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use endpoint_check_ext::tools;

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
async fn status_returns_info() {
    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert!(out["limits"]["max_timeout_secs"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_probe_ok_reports_latency_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ping"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("pong"),
        )
        .mount(&server)
        .await;
    let url = format!("{}/ping", server.uri());
    let out = dispatch("http_probe", json!({"url": url})).await.expect("ok");
    assert_eq!(out["status"], 200);
    assert_eq!(out["content_type"], "text/plain");
    assert_eq!(out["body_preview"], "pong");
    assert!(out["latency_ms"].as_u64().unwrap() < 5000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_probe_expected_status_match() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let url = format!("{}/x", server.uri());
    let out = dispatch(
        "http_probe",
        json!({ "url": url, "expected_status": 404 }),
    )
    .await
    .expect("ok");
    assert_eq!(out["status"], 404);
    assert_eq!(out["matches_expected"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_probe_expected_status_miss() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let url = format!("{}/x", server.uri());
    let out = dispatch(
        "http_probe",
        json!({ "url": url, "expected_status": 200 }),
    )
    .await
    .expect("ok");
    assert_eq!(out["status"], 500);
    assert_eq!(out["matches_expected"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_probe_rejects_non_http_scheme() {
    let err = dispatch("http_probe", json!({"url": "ftp://example.com"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_probe_bad_url_rejected() {
    let err = dispatch("http_probe", json!({"url": "not a url"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn http_probe_invalid_method_rejected() {
    let err = dispatch(
        "http_probe",
        json!({"url": "http://127.0.0.1:1/x", "method": "DELETE"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn ssl_cert_bad_port_rejected() {
    let err = dispatch(
        "ssl_cert",
        json!({"host": "example.com", "port": 99999}),
    )
    .await
    .unwrap_err();
    // `serde_json` unsigned parse may accept 99999 in as_u64, but port out of
    // u16 range should fail in tool; if the schema slips it through, we get a
    // BadUrl-class failure from ssl::inspect. Either way non-zero code.
    assert!(err.code < 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn ssl_cert_unknown_host_is_resolve_error() {
    let err = dispatch(
        "ssl_cert",
        json!({"host": "definitely-not-a-real-domain-xyz-12345.invalid", "timeout_secs": 2}),
    )
    .await
    .unwrap_err();
    // -32060 Resolve or -32061 Connect are both acceptable.
    assert!(err.code == -32060 || err.code == -32061, "got {}", err.code);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unknown_tool_is_method_not_found() {
    let err = dispatch("nope", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32601);
}
