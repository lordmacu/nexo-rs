use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use brave_search_ext::tools;

fn setup(server: &MockServer) {
    std::env::set_var("BRAVE_SEARCH_URL", server.uri());
    std::env::set_var("BRAVE_SEARCH_API_KEY", "test-key");
    tools::reset_state();
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await.expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_reports_key_present() {
    let server = MockServer::start().await;
    setup(&server);
    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["api_key_present"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn search_returns_flattened_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/web/search"))
        .and(header("X-Subscription-Token", "test-key"))
        .and(query_param("q", "rust async"))
        .and(query_param("count", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": {
                "results": [
                    {"title":"Rust async","url":"https://rust-lang.org/async","description":"guide","page_age":"2024-01","language":"en"}
                ]
            }
        })))
        .mount(&server).await;
    setup(&server);
    let out = dispatch("brave_search", json!({"query":"rust async","count":5}))
        .await.expect("ok");
    assert_eq!(out["count"], 1);
    assert_eq!(out["results"][0]["url"], "https://rust-lang.org/async");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_key_errors() {
    let server = MockServer::start().await;
    std::env::remove_var("BRAVE_SEARCH_API_KEY");
    std::env::set_var("BRAVE_SEARCH_URL", server.uri());
    tools::reset_state();
    let err = dispatch("brave_search", json!({"query":"x"})).await.unwrap_err();
    assert_eq!(err.code, -32041);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unauthorized_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/web/search"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .mount(&server).await;
    setup(&server);
    let err = dispatch("brave_search", json!({"query":"x"})).await.unwrap_err();
    assert_eq!(err.code, -32011);
}
