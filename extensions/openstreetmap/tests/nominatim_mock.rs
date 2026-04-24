use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use openstreetmap::{client, tools};

fn set_endpoint(server: &MockServer) {
    std::env::set_var("OSM_NOMINATIM_URL", server.uri());
    std::env::set_var("OSM_HTTP_TIMEOUT_SECS", "2");
    client::reset_state();
}

fn search_hits() -> serde_json::Value {
    json!([
        {
            "display_name": "Madrid, Comunidad de Madrid, España",
            "lat": "40.4167",
            "lon": "-3.7033",
            "class": "boundary",
            "type": "administrative",
            "importance": 0.85,
            "boundingbox": ["40.31", "40.56", "-3.83", "-3.52"]
        }
    ])
}

fn reverse_payload() -> serde_json::Value {
    json!({
        "display_name": "Calle Gran Vía, Madrid, España",
        "lat": "40.4203",
        "lon": "-3.7058",
        "address": {
            "road": "Calle Gran Vía",
            "city": "Madrid",
            "state": "Comunidad de Madrid",
            "country": "España",
            "country_code": "es",
            "postcode": "28013"
        }
    })
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_search_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("format", "jsonv2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_hits()))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("search", json!({"query": "Madrid"})).await.expect("ok");
    let arr = res["results"].as_array().expect("results");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["display_name"], "Madrid, Comunidad de Madrid, España");
    assert_eq!(arr[0]["type"], "administrative");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_reverse_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/reverse"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reverse_payload()))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let res = dispatch("reverse", json!({"lat": 40.4203, "lon": -3.7058}))
        .await
        .expect("ok");
    assert_eq!(res["display_name"], "Calle Gran Vía, Madrid, España");
    assert_eq!(res["address"]["country_code"], "es");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_search_empty_results_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("search", json!({"query": "Atlantis"})).await.unwrap_err();
    assert_eq!(err.code, -32001);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_retry_on_5xx_then_fail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let err = dispatch("search", json!({"query": "Madrid"})).await.unwrap_err();
    assert_eq!(err.code, -32003);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_circuit_open_after_threshold() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoint(&server);

    // 2 calls × 3 retries = 6 fails, threshold=5 → CB open on 3rd call.
    let _ = dispatch("search", json!({"query": "City0"})).await;
    let _ = dispatch("search", json!({"query": "City1"})).await;
    let err = dispatch("search", json!({"query": "City2"})).await.unwrap_err();
    assert_eq!(err.code, -32004, "expected circuit-open, got {} ({})", err.code, err.message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_invalid_lat_rejected() {
    let server = MockServer::start().await;
    set_endpoint(&server);

    let err = dispatch("reverse", json!({"lat": 200, "lon": 0})).await.unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("lat"));
}
