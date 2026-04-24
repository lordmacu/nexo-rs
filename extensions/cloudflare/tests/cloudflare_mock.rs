use cloudflare_ext::tools;
use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn set_env(server: &MockServer) {
    std::env::set_var(
        "CLOUDFLARE_API_BASE_URL",
        format!("{}/client/v4", server.uri()),
    );
    std::env::set_var("CLOUDFLARE_API_TOKEN", "test-token");
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
async fn list_zones_ok() {
    let server = MockServer::start().await;
    set_env(&server);
    Mock::given(method("GET"))
        .and(path("/client/v4/zones"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "result": [
                {
                    "id": "zone_1",
                    "name": "example.com",
                    "status": "active",
                    "plan": { "name": "Free" },
                    "name_servers": ["ns1.example.com"]
                }
            ]
        })))
        .mount(&server)
        .await;

    let out = dispatch("list_zones", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["count"], 1);
    assert_eq!(out["zones"][0]["name"], "example.com");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn list_zones_403_maps_to_auth_error() {
    let server = MockServer::start().await;
    set_env(&server);
    Mock::given(method("GET"))
        .and(path("/client/v4/zones"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "success": false,
            "errors": [{ "message": "forbidden token" }]
        })))
        .mount(&server)
        .await;

    let err = dispatch("list_zones", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32011);
    assert!(err.message.contains("HTTP 403"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_exposes_write_gate_hint() {
    let server = MockServer::start().await;
    set_env(&server);
    std::env::remove_var("CLOUDFLARE_ALLOW_WRITES");
    std::env::remove_var("CLOUDFLARE_ALLOW_PURGE");

    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["write_gate"]["dns_writes_enabled"], false);
    assert_eq!(out["write_gate"]["purge_enabled"], false);
    assert_eq!(out["write_gate"]["env"][0], "CLOUDFLARE_ALLOW_WRITES");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn create_dns_record_denied_when_gate_is_off() {
    let server = MockServer::start().await;
    set_env(&server);
    std::env::remove_var("CLOUDFLARE_ALLOW_WRITES");

    let err = dispatch(
        "create_dns_record",
        json!({
            "zone_id": "zone_1",
            "type": "A",
            "name": "api.example.com",
            "content": "1.2.3.4"
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32041);
    assert!(err.message.contains("CLOUDFLARE_ALLOW_WRITES"));
}
