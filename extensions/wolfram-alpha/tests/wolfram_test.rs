use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use wolfram_alpha_ext::tools;

fn setup(server: &MockServer) {
    std::env::set_var("WOLFRAM_BASE_URL", server.uri());
    std::env::set_var("WOLFRAM_APP_ID", "test-appid");
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await.expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_reports_app_id() {
    let server = MockServer::start().await;
    setup(&server);
    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["app_id_present"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn short_returns_plain_answer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/result"))
        .and(query_param("appid", "test-appid"))
        .and(query_param("i", "distance from earth to moon"))
        .respond_with(ResponseTemplate::new(200).set_body_string("about 238855 miles"))
        .mount(&server).await;
    setup(&server);
    let out = dispatch("wolfram_short", json!({"input":"distance from earth to moon"}))
        .await.expect("ok");
    assert!(out["answer"].as_str().unwrap().contains("238855"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn short_501_maps_to_no_result() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/result"))
        .respond_with(ResponseTemplate::new(501))
        .mount(&server).await;
    setup(&server);
    let out = dispatch("wolfram_short", json!({"input":"gibberish"}))
        .await.expect("ok");
    assert_eq!(out["ok"], false);
    assert_eq!(out["error"], "no_result");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn query_pods_summary() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "queryresult": {
                "success": true,
                "pods": [
                    {"id":"Input","title":"Input","primary":false,"subpods":[{"plaintext":"sqrt(2)"}]},
                    {"id":"Result","title":"Result","primary":true,"subpods":[{"plaintext":"1.4142..."}]}
                ]
            }
        })))
        .mount(&server).await;
    setup(&server);
    let out = dispatch("wolfram_query", json!({"input":"sqrt(2)"})).await.expect("ok");
    assert_eq!(out["success"], true);
    assert_eq!(out["pods_count"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_app_id_errors() {
    let server = MockServer::start().await;
    std::env::remove_var("WOLFRAM_APP_ID");
    std::env::set_var("WOLFRAM_BASE_URL", server.uri());
    let err = dispatch("wolfram_short", json!({"input":"x"})).await.unwrap_err();
    assert_eq!(err.code, -32041);
}
