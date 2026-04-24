use serde_json::json;
use serial_test::serial;
use wikipedia_ext::tools;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn set_env(server: &MockServer) {
    std::env::set_var("WIKIPEDIA_LANG", "en");
    std::env::set_var(
        "WIKIPEDIA_API_BASE_URL",
        format!("{}/w/api.php", server.uri()),
    );
    std::env::set_var(
        "WIKIPEDIA_REST_BASE_URL",
        format!("{}/api/rest_v1", server.uri()),
    );
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
async fn search_ok() {
    let server = MockServer::start().await;
    set_env(&server);
    Mock::given(method("GET"))
        .and(path("/w/api.php"))
        .and(query_param("action", "query"))
        .and(query_param("list", "search"))
        .and(query_param("srsearch", "rust"))
        .and(query_param("srlimit", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "query": {
                "search": [
                    {
                        "title": "Rust",
                        "snippet": "Rust language",
                        "pageid": 324,
                        "size": 1234,
                        "wordcount": 321
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    let out = dispatch("search", json!({"query":"rust","limit":1}))
        .await
        .expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["count"], 1);
    assert_eq!(out["results"][0]["title"], "Rust");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn summary_404_returns_not_found_payload() {
    let server = MockServer::start().await;
    set_env(&server);
    Mock::given(method("GET"))
        .and(path("/api/rest_v1/page/summary/Unknown"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let out = dispatch("summary", json!({"title":"Unknown"}))
        .await
        .expect("ok");
    assert_eq!(out["ok"], false);
    assert_eq!(out["error"], "not_found");
    assert_eq!(out["lang"], "en");
}
