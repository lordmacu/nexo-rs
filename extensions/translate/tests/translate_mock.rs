use serde_json::json;
use serial_test::serial;
use translate_ext::tools;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn set_env(server: &MockServer) {
    std::env::set_var("TRANSLATE_PROVIDER", "libretranslate");
    std::env::set_var("LIBRETRANSLATE_URL", server.uri());
    std::env::remove_var("LIBRETRANSLATE_API_KEY");
    std::env::remove_var("DEEPL_API_KEY");
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
async fn languages_libretranslate_ok() {
    let server = MockServer::start().await;
    set_env(&server);
    Mock::given(method("GET"))
        .and(path("/languages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"code":"en","name":"English"},
            {"code":"es","name":"Spanish"}
        ])))
        .mount(&server)
        .await;

    let out = dispatch("languages", json!({})).await.expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["provider"], "libretranslate");
    assert_eq!(out["languages"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn translate_libretranslate_ok() {
    let server = MockServer::start().await;
    set_env(&server);
    Mock::given(method("POST"))
        .and(path("/translate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "translatedText": "Hola",
            "detectedLanguage": { "language": "en" }
        })))
        .mount(&server)
        .await;

    let out = dispatch(
        "translate",
        json!({"text":"Hello","target":"es","source":"en"}),
    )
    .await
    .expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["provider"], "libretranslate");
    assert_eq!(out["translated"], "Hola");
    assert_eq!(out["detected"], "en");
}
