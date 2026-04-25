use std::sync::Arc;

use nexo_core::agent::plugin::Response;
use nexo_plugin_telegram::bot::BotClient;
use nexo_plugin_telegram::plugin::dispatch_custom;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "test-token";

fn bot(server: &MockServer) -> Arc<BotClient> {
    Arc::new(BotClient::new(TOKEN, Some(&server.uri())))
}

fn telegram_path(endpoint: &str) -> String {
    format!("/bot{TOKEN}/{endpoint}")
}

fn message_ok(message_id: i64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "ok": true,
        "result": {
            "message_id": message_id,
            "chat": { "id": 123, "type": "private" }
        }
    }))
}

fn bool_ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "ok": true,
        "result": true
    }))
}

fn assert_message_sent(resp: Response, expected_id: &str) {
    match resp {
        Response::MessageSent { message_id } => assert_eq!(message_id, expected_id),
        other => panic!("expected MessageSent, got {other:?}"),
    }
}

#[tokio::test]
async fn custom_chat_action_hits_send_chat_action() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendChatAction")))
        .and(body_partial_json(
            json!({"chat_id": 123, "action": "typing"}),
        ))
        .respond_with(bool_ok())
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "chat_action",
        json!({"chat_id": 123, "action": "typing"}),
    )
    .await
    .unwrap();
    assert!(matches!(resp, Response::Ok));
}

#[tokio::test]
async fn custom_reply_hits_send_message_with_reply_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendMessage")))
        .and(body_partial_json(json!({
            "chat_id": 777,
            "text": "hola",
            "reply_to_message_id": 99,
            "parse_mode": "Markdown"
        })))
        .respond_with(message_ok(7001))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "reply",
        json!({
            "chat_id": 777,
            "msg_id": 99,
            "text": "hola",
            "parse_mode": "markdown"
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "7001");
}

#[tokio::test]
async fn custom_send_with_format_normalizes_parse_mode() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendMessage")))
        .and(body_partial_json(json!({
            "chat_id": 10,
            "text": "bold",
            "parse_mode": "MarkdownV2",
            "reply_markup": {"inline_keyboard":[[{"text":"A","callback_data":"a"}]]}
        })))
        .respond_with(message_ok(7002))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_with_format",
        json!({
            "chat_id": 10,
            "text": "bold",
            "parse_mode": "mdv2",
            "reply_markup": {"inline_keyboard":[[{"text":"A","callback_data":"a"}]]}
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "7002");
}

#[tokio::test]
async fn custom_edit_message_hits_edit_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("editMessageText")))
        .and(body_partial_json(json!({
            "chat_id": 11,
            "message_id": 44,
            "text": "editado",
            "parse_mode": "HTML"
        })))
        .respond_with(message_ok(44))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "edit_message",
        json!({
            "chat_id": 11,
            "message_id": 44,
            "text": "editado",
            "parse_mode": "html"
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "44");
}

#[tokio::test]
async fn custom_reaction_hits_set_message_reaction() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("setMessageReaction")))
        .and(body_partial_json(json!({
            "chat_id": 11,
            "message_id": 45,
            "reaction": [{"type":"emoji","emoji":"🔥"}]
        })))
        .respond_with(bool_ok())
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "reaction",
        json!({"chat_id": 11, "message_id": 45, "emoji": "🔥"}),
    )
    .await
    .unwrap();
    assert!(matches!(resp, Response::Ok));
}

#[tokio::test]
async fn custom_send_location_hits_send_location() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendLocation")))
        .and(body_partial_json(json!({
            "chat_id": 123,
            "latitude": 4.7110,
            "longitude": -74.0721
        })))
        .respond_with(message_ok(7003))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_location",
        json!({"chat_id": 123, "latitude": 4.7110, "longitude": -74.0721}),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "7003");
}

#[tokio::test]
async fn custom_send_photo_hits_send_photo() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendPhoto")))
        .and(body_partial_json(json!({
            "chat_id": 9,
            "photo": "FILE_PHOTO",
            "caption": "cap",
            "parse_mode": "Markdown"
        })))
        .respond_with(message_ok(8001))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_photo",
        json!({
            "chat_id": 9,
            "source": {"source":"file_id", "value":"FILE_PHOTO"},
            "caption": "cap",
            "parse_mode": "markdown"
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "8001");
}

#[tokio::test]
async fn custom_send_audio_hits_send_audio_with_extras() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendAudio")))
        .and(body_partial_json(json!({
            "chat_id": 9,
            "audio": "FILE_AUDIO",
            "caption": "audio cap",
            "parse_mode": "Markdown",
            "title": "track",
            "performer": "artist",
            "duration": "31"
        })))
        .respond_with(message_ok(8002))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_audio",
        json!({
            "chat_id": 9,
            "source": {"source":"file_id", "value":"FILE_AUDIO"},
            "caption": "audio cap",
            "parse_mode": "markdown",
            "title": "track",
            "performer": "artist",
            "duration": 31
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "8002");
}

#[tokio::test]
async fn custom_send_voice_hits_send_voice() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendVoice")))
        .and(body_partial_json(json!({
            "chat_id": 9,
            "voice": "FILE_VOICE",
            "caption": "voice cap",
            "parse_mode": "Markdown",
            "duration": "12"
        })))
        .respond_with(message_ok(8003))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_voice",
        json!({
            "chat_id": 9,
            "source": {"source":"file_id", "value":"FILE_VOICE"},
            "caption": "voice cap",
            "parse_mode": "markdown",
            "duration": 12
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "8003");
}

#[tokio::test]
async fn custom_send_video_hits_send_video() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendVideo")))
        .and(body_partial_json(json!({
            "chat_id": 9,
            "video": "FILE_VIDEO",
            "caption": "video cap",
            "parse_mode": "Markdown",
            "duration": "42"
        })))
        .respond_with(message_ok(8004))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_video",
        json!({
            "chat_id": 9,
            "source": {"source":"file_id", "value":"FILE_VIDEO"},
            "caption": "video cap",
            "parse_mode": "markdown",
            "duration": 42
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "8004");
}

#[tokio::test]
async fn custom_send_document_hits_send_document() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendDocument")))
        .and(body_partial_json(json!({
            "chat_id": 9,
            "document": "FILE_DOC",
            "caption": "doc cap",
            "parse_mode": "HTML"
        })))
        .respond_with(message_ok(8005))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_document",
        json!({
            "chat_id": 9,
            "source": {"source":"file_id", "value":"FILE_DOC"},
            "caption": "doc cap",
            "parse_mode": "html"
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "8005");
}

#[tokio::test]
async fn custom_send_animation_hits_send_animation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(telegram_path("sendAnimation")))
        .and(body_partial_json(json!({
            "chat_id": 9,
            "animation": "FILE_GIF",
            "caption": "gif cap",
            "parse_mode": "HTML"
        })))
        .respond_with(message_ok(8006))
        .expect(1)
        .mount(&server)
        .await;

    let resp = dispatch_custom(
        &bot(&server),
        "send_animation",
        json!({
            "chat_id": 9,
            "source": {"source":"file_id", "value":"FILE_GIF"},
            "caption": "gif cap",
            "parse_mode": "html"
        }),
    )
    .await
    .unwrap();
    assert_message_sent(resp, "8006");
}

#[tokio::test]
async fn custom_unknown_command_returns_error() {
    let server = MockServer::start().await;
    let resp = dispatch_custom(&bot(&server), "does_not_exist", json!({}))
        .await
        .unwrap();
    match resp {
        Response::Error { message } => assert!(message.contains("unknown custom command")),
        other => panic!("expected Error, got {other:?}"),
    }
}
