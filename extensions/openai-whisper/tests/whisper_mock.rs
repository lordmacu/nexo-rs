use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use openai_whisper::{client, tools};

fn set_endpoint(server: &MockServer) {
    std::env::set_var("WHISPER_OPENAI_URL", server.uri());
    std::env::set_var("WHISPER_OPENAI_API_KEY", "sk-whisper-test");
    std::env::set_var("WHISPER_HTTP_TIMEOUT_SECS", "5");
    std::env::set_var("WHISPER_MODEL", "whisper-test");
    client::reset_state();
}

fn write_fake_audio(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("whisper-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join(name);
    // Minimal fake — Whisper API server is mocked anyway.
    std::fs::write(&p, b"FAKE\xff\xfe\x00\x01audio bytes").expect("write");
    p
}

async fn dispatch(name: &'static str, args: serde_json::Value) -> Result<serde_json::Value, tools::ToolError> {
    tokio::task::spawn_blocking(move || tools::dispatch(name, &args))
        .await
        .expect("join")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_status_ok() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let res = dispatch("status", json!({})).await.expect("ok");
    assert_eq!(res["token_present"], true);
    assert_eq!(res["default_model"], "whisper-test");
    assert_eq!(res["limits"]["max_file_bytes"], 25 * 1024 * 1024);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_transcribe_text_format_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .and(header("Authorization", "Bearer sk-whisper-test"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Hola mundo desde whisper.\n"))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("a.wav");
    let res = dispatch(
        "transcribe_file",
        json!({"file_path": p.to_string_lossy(), "language": "es"}),
    )
    .await
    .expect("ok");
    assert_eq!(res["transcript"]["text"], "Hola mundo desde whisper.");
    assert_eq!(res["response_format"], "text");
    assert_eq!(res["language"], "es");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_transcribe_verbose_json_ok() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "hello world",
            "language": "english",
            "duration": 1.42,
            "segments": [{ "id": 0, "start": 0.0, "end": 1.42, "text": "hello world" }]
        })))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("b.wav");
    let res = dispatch(
        "transcribe_file",
        json!({"file_path": p.to_string_lossy(), "response_format": "verbose_json"}),
    )
    .await
    .expect("ok");
    assert_eq!(res["transcript"]["text"], "hello world");
    assert_eq!(res["transcript"]["segments"][0]["end"], 1.42);
    assert_eq!(res["response_format"], "verbose_json");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "bad key"})))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("c.wav");
    let err = dispatch("transcribe_file", json!({"file_path": p.to_string_lossy()}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32011);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_payload_too_large_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(413).set_body_string("file too big"))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("d.wav");
    let err = dispatch("transcribe_file", json!({"file_path": p.to_string_lossy()}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32014);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_unsupported_media_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(415).set_body_string("bad mime"))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("e.txt");
    let err = dispatch("transcribe_file", json!({"file_path": p.to_string_lossy()}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32015);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_5xx_retried_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("f.wav");
    let err = dispatch("transcribe_file", json!({"file_path": p.to_string_lossy()}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32003);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_empty_text_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("   \n  "))
        .mount(&server)
        .await;
    set_endpoint(&server);

    let p = write_fake_audio("g.wav");
    let err = dispatch("transcribe_file", json!({"file_path": p.to_string_lossy()}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_invalid_format_rejected() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let p = write_fake_audio("h.wav");
    let err = dispatch(
        "transcribe_file",
        json!({"file_path": p.to_string_lossy(), "response_format": "binary"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn test_missing_file_rejected() {
    let server = MockServer::start().await;
    set_endpoint(&server);
    let err = dispatch(
        "transcribe_file",
        json!({"file_path": "/nonexistent/path/audio.wav"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("cannot stat"));
}
