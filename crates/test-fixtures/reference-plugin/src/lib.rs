//! Phase 81.33.b.real reference plugin demo — broker handler
//! sketches.
//!
//! Companion to `nexo-plugin.toml` at the crate root. Each
//! module corresponds to one manifest section + its broker
//! topic family. Real plugin subprocesses subscribe to the
//! topics and route messages into these handlers (or their own
//! equivalents).
//!
//! Handlers here are **pure functions** taking a JSON request
//! payload + returning a JSON response payload. No broker, no
//! tokio runtime, no I/O. This keeps them unit-testable and
//! easy to copy into a real plugin's broker subscriber loop:
//!
//! ```ignore
//! ctx.broker
//!     .subscribe("plugin.<id>.pairing.normalize_sender")
//!     .await?
//!     .for_each(|msg| async {
//!         let reply = reference_demo::pairing::normalize_sender(&msg.payload);
//!         broker.publish(&msg.reply_to.unwrap(), reply).await
//!     });
//! ```

use serde_json::{json, Value};

/// Pairing adapter broker handlers (Phase 81.33.b.real Stage 1).
///
/// Topics the plugin subscribes to:
/// - `<broker_topic_prefix>.pairing.normalize_sender`
/// - `<broker_topic_prefix>.pairing.send_reply`
/// - `<broker_topic_prefix>.pairing.send_qr_image`
pub mod pairing {
    use super::*;

    /// Canonicalise an inbound sender id. The reference impl
    /// strips a hypothetical `@demo.local` suffix + lowercases
    /// the rest; real plugins would do channel-specific
    /// E.164 / handle normalisation (whatsapp `@c.us` strip,
    /// telegram `@user` lowercase, …).
    ///
    /// Request: `{ "raw": "<raw-sender>" }`
    /// Reply:   `{ "normalized": "<canonical>" }` or
    ///          `{ "normalized": null }` to reject.
    pub fn normalize_sender(request: &Value) -> Value {
        let raw = request.get("raw").and_then(|v| v.as_str()).unwrap_or("");
        if raw.is_empty() {
            return json!({ "normalized": null });
        }
        let stripped = raw
            .strip_suffix("@demo.local")
            .unwrap_or(raw)
            .to_lowercase();
        // Reject pathological cases: non-ASCII, length > 64, …
        if !stripped.is_ascii() || stripped.len() > 64 {
            return json!({ "normalized": null });
        }
        json!({ "normalized": stripped })
    }

    /// Deliver a text reply (challenge code) to the sender.
    /// Reference impl logs + acks; a real plugin would issue the
    /// channel-specific send-message call.
    ///
    /// Request: `{ "account": "<inst>", "to": "<sender>", "text": "..." }`
    /// Reply:   `{ "ok": true }` or `{ "ok": false, "error": "..." }`
    pub fn send_reply(request: &Value) -> Value {
        let account = request
            .get("account")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let to = request.get("to").and_then(|v| v.as_str()).unwrap_or("");
        let text = request.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if account.is_empty() || to.is_empty() {
            return json!({ "ok": false, "error": "account and to required" });
        }
        // Real plugins call their channel SDK here. The demo just
        // records a tracing event so the test harness can assert
        // the handler ran with the right fields.
        tracing::info!(
            account,
            to,
            text_len = text.len(),
            "reference_demo pairing send_reply"
        );
        json!({ "ok": true })
    }

    /// Deliver a QR PNG image. Reference impl decodes + acks
    /// success when bytes are well-formed.
    ///
    /// Request: `{ "account": "...", "to": "...", "png_base64": "..." }`
    /// Reply:   `{ "ok": true }` or `{ "ok": false, "error": "..." }`
    pub fn send_qr_image(request: &Value) -> Value {
        use base64::Engine;
        let png_b64 = request
            .get("png_base64")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match base64::engine::general_purpose::STANDARD.decode(png_b64.as_bytes()) {
            Ok(bytes) if !bytes.is_empty() => json!({ "ok": true }),
            Ok(_) => json!({ "ok": false, "error": "png_base64 decoded to empty" }),
            Err(e) => json!({ "ok": false, "error": format!("invalid base64: {e}") }),
        }
    }
}

/// HTTP route handler (Phase 81.33.b.real Stage 2).
///
/// Topic the plugin subscribes to:
/// - `plugin.<id>.http.request`
///
/// Request shape (daemon → plugin):
/// `{ "method": "GET", "path": "/reference_demo/foo", "query": "...",
///    "headers": [[k,v],…], "body_base64": "..." }`
///
/// Reply shape (plugin → daemon):
/// `{ "status": 200, "headers": [[k,v],…], "body_base64": "..." }`
pub mod http {
    use super::*;
    use base64::Engine;

    /// Demo router: serves `/reference_demo/hello` as plain text,
    /// `/reference_demo/echo` as JSON echo, everything else 404.
    pub fn handle_request(request: &Value) -> Value {
        let path = request.get("path").and_then(|v| v.as_str()).unwrap_or("/");
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET");
        match (method, path) {
            ("GET", "/reference_demo/hello") => respond(
                200,
                "text/plain; charset=utf-8",
                b"hello from reference_demo\n",
            ),
            ("POST", "/reference_demo/echo") => {
                let body_b64 = request
                    .get("body_base64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let body = base64::engine::general_purpose::STANDARD
                    .decode(body_b64.as_bytes())
                    .unwrap_or_default();
                respond(200, "application/octet-stream", &body)
            }
            _ => respond(
                404,
                "application/json; charset=utf-8",
                br#"{"error":"not found"}"#,
            ),
        }
    }

    fn respond(status: u16, content_type: &str, body: &[u8]) -> Value {
        json!({
            "status": status,
            "headers": [["Content-Type", content_type]],
            "body_base64": base64::engine::general_purpose::STANDARD.encode(body),
        })
    }
}

/// Admin RPC handler (Phase 81.33.b.real Stage 4).
///
/// Topic the plugin subscribes to:
/// - `<broker_topic_prefix>.admin.<verb>` (e.g.
///   `plugin.reference_demo.admin.list`)
///
/// Plugin owns internal routing per verb. The daemon forwarded
/// the full method name in the request envelope so plugins can
/// route by either the topic suffix OR the embedded method.
///
/// Request: `{ "method": "<full method>", "params": <json> }`
/// Reply:   `{ "ok": true, "result": <json> }` or
///          `{ "ok": false, "error": "..." }`
pub mod admin {
    use super::*;

    pub fn handle(request: &Value) -> Value {
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "nexo/admin/reference_demo/list" => json!({
                "ok": true,
                "result": { "items": [
                    { "id": "instance-a", "state": "ready" },
                    { "id": "instance-b", "state": "pending" },
                ] },
            }),
            "nexo/admin/reference_demo/ping" => json!({
                "ok": true,
                "result": { "echo": params, "ts_unix": demo_unix_ts() },
            }),
            other => json!({
                "ok": false,
                "error": format!("unknown admin method: {other}"),
            }),
        }
    }

    fn demo_unix_ts() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// Prometheus metrics handler (Phase 81.33.b.real Stage 5).
///
/// Topic the plugin subscribes to:
/// - `<broker_topic_prefix>.metrics.scrape`
///
/// Request: `{}` (empty object)
/// Reply:   `{ "text": "<prometheus text>" }`
pub mod metrics {
    use super::*;

    pub fn scrape(_request: &Value) -> Value {
        // Real plugins format their counters / gauges /
        // histograms here. Plugins SHOULD namespace metric names
        // with the plugin id prefix (`reference_demo_<name>`) so
        // they don't collide with daemon-internal series.
        let text = "\
# HELP reference_demo_handler_calls_total Number of broker handler invocations.\n\
# TYPE reference_demo_handler_calls_total counter\n\
reference_demo_handler_calls_total{handler=\"normalize_sender\"} 0\n\
reference_demo_handler_calls_total{handler=\"send_reply\"} 0\n\
reference_demo_handler_calls_total{handler=\"send_qr_image\"} 0\n\
# HELP reference_demo_ready Whether the demo plugin is ready to handle requests.\n\
# TYPE reference_demo_ready gauge\n\
reference_demo_ready 1\n";
        json!({ "text": text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pairing ────────────────────────────────────────────────

    #[test]
    fn pairing_normalize_sender_strips_demo_suffix() {
        let r = pairing::normalize_sender(&json!({ "raw": "User.A@demo.local" }));
        assert_eq!(r.get("normalized").and_then(|v| v.as_str()), Some("user.a"));
    }

    #[test]
    fn pairing_normalize_sender_rejects_empty() {
        let r = pairing::normalize_sender(&json!({ "raw": "" }));
        assert!(r.get("normalized").unwrap().is_null());
    }

    #[test]
    fn pairing_normalize_sender_rejects_overlong() {
        let huge = "a".repeat(100);
        let r = pairing::normalize_sender(&json!({ "raw": huge }));
        assert!(r.get("normalized").unwrap().is_null());
    }

    #[test]
    fn pairing_send_reply_acks_valid_request() {
        let r = pairing::send_reply(&json!({
            "account": "default",
            "to": "user.a",
            "text": "code: 1234",
        }));
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn pairing_send_reply_rejects_missing_to() {
        let r = pairing::send_reply(&json!({
            "account": "default",
            "to": "",
            "text": "code",
        }));
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn pairing_send_qr_image_validates_base64() {
        use base64::Engine;
        let r = pairing::send_qr_image(&json!({
            "account": "default",
            "to": "user",
            "png_base64": base64::engine::general_purpose::STANDARD.encode(b"PNG-bytes"),
        }));
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn pairing_send_qr_image_rejects_invalid_base64() {
        let r = pairing::send_qr_image(&json!({
            "account": "default",
            "to": "user",
            "png_base64": "!!!not base64!!!",
        }));
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(false));
    }

    // ── http ───────────────────────────────────────────────────

    #[test]
    fn http_get_hello_serves_text_plain_200() {
        use base64::Engine;
        let r = http::handle_request(&json!({
            "method": "GET",
            "path": "/reference_demo/hello",
        }));
        assert_eq!(r.get("status").and_then(|v| v.as_u64()), Some(200));
        let body = base64::engine::general_purpose::STANDARD
            .decode(r.get("body_base64").and_then(|v| v.as_str()).unwrap())
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("hello from reference_demo"));
    }

    #[test]
    fn http_post_echo_round_trips_body() {
        use base64::Engine;
        let payload = b"binary payload here";
        let r = http::handle_request(&json!({
            "method": "POST",
            "path": "/reference_demo/echo",
            "body_base64": base64::engine::general_purpose::STANDARD.encode(payload),
        }));
        let echoed = base64::engine::general_purpose::STANDARD
            .decode(r.get("body_base64").and_then(|v| v.as_str()).unwrap())
            .unwrap();
        assert_eq!(echoed, payload);
    }

    #[test]
    fn http_unknown_path_returns_404() {
        let r = http::handle_request(&json!({
            "method": "GET",
            "path": "/reference_demo/missing",
        }));
        assert_eq!(r.get("status").and_then(|v| v.as_u64()), Some(404));
    }

    // ── admin ──────────────────────────────────────────────────

    #[test]
    fn admin_list_returns_known_instances() {
        let r = admin::handle(&json!({
            "method": "nexo/admin/reference_demo/list",
            "params": {},
        }));
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(true));
        let items = r
            .get("result")
            .and_then(|v| v.get("items"))
            .and_then(|v| v.as_array())
            .expect("result.items");
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn admin_ping_echoes_params() {
        let r = admin::handle(&json!({
            "method": "nexo/admin/reference_demo/ping",
            "params": { "msg": "hello" },
        }));
        assert_eq!(
            r.get("result")
                .and_then(|v| v.get("echo"))
                .and_then(|v| v.get("msg"))
                .and_then(|v| v.as_str()),
            Some("hello"),
        );
    }

    #[test]
    fn admin_unknown_method_returns_err() {
        let r = admin::handle(&json!({
            "method": "nexo/admin/reference_demo/nonexistent",
            "params": {},
        }));
        assert_eq!(r.get("ok").and_then(|v| v.as_bool()), Some(false));
    }

    // ── metrics ────────────────────────────────────────────────

    #[test]
    fn metrics_scrape_returns_prometheus_text() {
        let r = metrics::scrape(&json!({}));
        let text = r.get("text").and_then(|v| v.as_str()).expect("text");
        assert!(text.contains("reference_demo_handler_calls_total"));
        assert!(text.contains("# TYPE reference_demo_handler_calls_total counter"));
        assert!(text.contains("reference_demo_ready 1"));
    }
}
