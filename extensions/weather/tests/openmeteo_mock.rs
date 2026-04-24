use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use weather::{client, tools};

fn set_endpoints(server: &MockServer) {
    std::env::set_var("WEATHER_GEOCODING_URL", format!("{}/v1/search", server.uri()));
    std::env::set_var("WEATHER_FORECAST_URL", format!("{}/v1/forecast", server.uri()));
    std::env::set_var("WEATHER_HTTP_TIMEOUT_SECS", "2");
    client::reset_state();
}

fn geocode_hit() -> serde_json::Value {
    json!({
        "results": [{
            "name": "Madrid",
            "country": "Spain",
            "timezone": "Europe/Madrid",
            "latitude": 40.4168,
            "longitude": -3.7038
        }]
    })
}

fn forecast_payload(days: usize) -> serde_json::Value {
    let times: Vec<String> = (0..days)
        .map(|i| format!("2026-04-{:02}", 23 + i as u32))
        .collect();
    let mins = vec![5.0; days];
    let maxs = vec![15.0; days];
    let precs = vec![0.0; days];
    let winds = vec![10.0; days];
    let codes = vec![1u16; days];
    json!({
        "current": {
            "time": "2026-04-23T12:00",
            "temperature_2m": 15.5,
            "apparent_temperature": 14.0,
            "relative_humidity_2m": 60,
            "wind_speed_10m": 10.0,
            "wind_direction_10m": 180,
            "precipitation": 0.0,
            "weather_code": 1,
            "is_day": 1
        },
        "daily": {
            "time": times,
            "temperature_2m_min": mins,
            "temperature_2m_max": maxs,
            "precipitation_sum": precs,
            "wind_speed_10m_max": winds,
            "weather_code": codes
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
async fn test_current_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(geocode_hit()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/forecast"))
        .respond_with(ResponseTemplate::new(200).set_body_json(forecast_payload(1)))
        .mount(&server)
        .await;
    set_endpoints(&server);

    let res = dispatch("current", json!({"location": "Madrid"})).await.expect("ok");
    assert_eq!(res["resolved"]["name"], "Madrid");
    assert_eq!(res["units"], "metric");
    assert_eq!(res["current"]["temperature"], 15.5);
    assert_eq!(res["current"]["weather_desc"], "mainly clear");
    assert_eq!(res["current"]["is_day"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_forecast_seven_days() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(geocode_hit()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/forecast"))
        .and(query_param("forecast_days", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(forecast_payload(7)))
        .mount(&server)
        .await;
    set_endpoints(&server);

    let res = dispatch("forecast", json!({"location": "Madrid", "days": 7})).await.expect("ok");
    assert_eq!(res["days"], 7);
    let arr = res["forecast"].as_array().expect("array");
    assert_eq!(arr.len(), 7);
    assert_eq!(arr[0]["weather_desc"], "mainly clear");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_geocoding_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;
    set_endpoints(&server);

    let err = dispatch("current", json!({"location": "Atlantis"})).await.unwrap_err();
    assert_eq!(err.code, -32001);
    assert!(err.message.contains("not found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_retry_then_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoints(&server);

    let err = dispatch("current", json!({"location": "Madrid"})).await.unwrap_err();
    assert_eq!(err.code, -32003);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_circuit_open_after_threshold() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoints(&server);

    // Threshold = 5; each call already retries 3 times so it bumps fails by 3.
    // Two calls = 6 fails => CB open. Use unique location strings to skip cache.
    let _ = dispatch("current", json!({"location": "City0"})).await;
    let _ = dispatch("current", json!({"location": "City1"})).await;

    let err = dispatch("current", json!({"location": "City2"})).await.unwrap_err();
    assert_eq!(err.code, -32004, "expected circuit-open code, got {} ({})", err.code, err.message);
}
