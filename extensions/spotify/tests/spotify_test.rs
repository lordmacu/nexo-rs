use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use spotify_ext::{client, tools};

fn set_endpoint(server: &MockServer) {
    std::env::set_var("SPOTIFY_API_URL", server.uri());
    std::env::set_var("SPOTIFY_ACCESS_TOKEN", "fake-access-token");
    std::env::set_var("SPOTIFY_HTTP_TIMEOUT_SECS", "3");
    client::reset_state();
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn status_when_token_set() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let out = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["token_present"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn now_playing_flattens_track_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/player"))
        .and(header("Authorization", "Bearer fake-access-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "is_playing": true,
            "progress_ms": 42000,
            "item": {
                "name": "Bohemian Rhapsody",
                "uri": "spotify:track:abc",
                "artists": [{"name":"Queen"}],
                "album": {"name":"A Night at the Opera"}
            },
            "device": {"name":"Kitchen Sonos","id":"d1","is_active":true}
        })))
        .mount(&server).await;
    set_endpoint(&server);
    let out = dispatch("now_playing", json!({})).await.expect("ok");
    assert_eq!(out["is_playing"], true);
    assert_eq!(out["track"], "Bohemian Rhapsody");
    assert_eq!(out["artists"][0], "Queen");
    assert_eq!(out["album"], "A Night at the Opera");
    assert_eq!(out["device"]["name"], "Kitchen Sonos");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn now_playing_204_is_no_device() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/player"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server).await;
    set_endpoint(&server);
    let out = dispatch("now_playing", json!({})).await.expect("ok");
    assert_eq!(out["is_playing"], false);
    assert!(out["reason"].as_str().unwrap().contains("no active"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn search_passes_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tracks": {"items": [{"name":"Yellow Submarine"}]}
        })))
        .mount(&server).await;
    set_endpoint(&server);
    let out = dispatch(
        "search",
        json!({"query":"Yellow","types":"track","limit":5}),
    ).await.expect("ok");
    assert_eq!(out["results"]["tracks"]["items"][0]["name"], "Yellow Submarine");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unauthorized_maps_to_code() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/player"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .mount(&server).await;
    set_endpoint(&server);
    let err = dispatch("now_playing", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32011);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn rate_limit_maps_to_code_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "10")
                .set_body_string("slow down"),
        )
        .mount(&server).await;
    set_endpoint(&server);
    let err = dispatch("search", json!({"query":"x"})).await.unwrap_err();
    assert_eq!(err.code, -32013);
    assert!(err.message.contains("10"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn no_active_device_detected_from_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/me/player/play"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"status":404,"reason":"NO_ACTIVE_DEVICE","message":"Player command failed"}
        })))
        .mount(&server).await;
    set_endpoint(&server);
    let err = dispatch(
        "play",
        json!({"uri":"spotify:track:abc"}),
    ).await.unwrap_err();
    assert_eq!(err.code, -32070);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn play_without_uri_sends_no_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/me/player/play"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server).await;
    set_endpoint(&server);
    let out = dispatch("play", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn play_rejects_non_spotify_uri() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let err = dispatch("play", json!({"uri":"https://evil"})).await.unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn search_rejects_bad_type() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let err = dispatch("search", json!({"query":"a","types":"pizza"})).await.unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn missing_token_surfaces() {
    let server = MockServer::start().await;
    std::env::set_var("SPOTIFY_API_URL", server.uri());
    std::env::remove_var("SPOTIFY_ACCESS_TOKEN");
    client::reset_state();
    let err = dispatch("now_playing", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32041);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn unknown_tool_is_method_not_found() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let err = dispatch("nope", json!({})).await.unwrap_err();
    assert_eq!(err.code, -32601);
}
