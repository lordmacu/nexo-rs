use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use proxmox_ext::tools;

fn setup(server: &MockServer) {
    std::env::set_var("PROXMOX_URL", server.uri());
    std::env::set_var("PROXMOX_TOKEN", "user@pam!token=abc123");
    std::env::remove_var("PROXMOX_ALLOW_WRITE");
    std::env::remove_var("PROXMOX_INSECURE_TLS");
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await.expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_reports_endpoint() {
    let server = MockServer::start().await;
    setup(&server);
    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["token_present"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn list_nodes_passes_auth_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes"))
        .and(header("Authorization", "PVEAPIToken=user@pam!token=abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"node":"pve1","status":"online","uptime":12345}]
        })))
        .mount(&server).await;
    setup(&server);
    let out = dispatch("list_nodes", json!({})).await.expect("ok");
    assert_eq!(out["data"][0]["node"], "pve1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn vm_action_requires_write_flag() {
    let server = MockServer::start().await;
    setup(&server);
    let err = dispatch("vm_action", json!({"node":"pve1","vmid":101,"action":"start"}))
        .await.unwrap_err();
    assert_eq!(err.code, -32043);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn vm_status_for_lxc_hits_right_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes/pve1/lxc/200/status/current"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"status":"running","name":"web","uptime":99}
        })))
        .mount(&server).await;
    setup(&server);
    let out = dispatch("vm_status", json!({"node":"pve1","vmid":200,"kind":"lxc"}))
        .await.expect("ok");
    assert_eq!(out["status"]["name"], "web");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn invalid_node_rejected() {
    let server = MockServer::start().await;
    setup(&server);
    let err = dispatch("list_vms", json!({"node":"pve1; rm -rf /"}))
        .await.unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unauthorized_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api2/json/nodes"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .mount(&server).await;
    setup(&server);
    let err = dispatch("list_nodes", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32011);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_url_or_token_errors() {
    std::env::remove_var("PROXMOX_URL");
    std::env::remove_var("PROXMOX_TOKEN");
    let err = dispatch("list_nodes", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32041);
}
