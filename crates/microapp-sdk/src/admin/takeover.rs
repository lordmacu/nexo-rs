//! Phase 83.8.6 — `HumanTakeover` SDK helper.
//!
//! Three-step pattern for an operator-driven takeover of a paused
//! conversation:
//!
//! 1. [`HumanTakeover::engage`] — pause the
//!    [`ProcessingScope::Conversation`] so the agent skips inbound
//!    messages on it.
//! 2. [`HumanTakeover::send_reply`] — dispatch one (or more) manual
//!    replies through the daemon's
//!    `processing/intervention` handler. Each call goes out via
//!    the [`ChannelOutboundDispatcher`] wired in `nexo-setup`
//!    (Phase 83.8.4).
//! 3. [`HumanTakeover::release`] — resume the agent.
//!
//! The helper is a thin orchestration over the generic
//! [`super::AdminClient`] — every method maps to one
//! `nexo/admin/processing/*` call. Microapps can also call those
//! methods directly; this wrapper is for ergonomics + future
//! synthetic-summary injection.

use serde::Serialize;
use serde_json::{json, Value};

use nexo_tool_meta::admin::processing::ProcessingScope;
use uuid::Uuid;

use super::{AdminClient, AdminError};

/// Arguments for [`HumanTakeover::send_reply`]. Mirrors the
/// `Reply` shape of `InterventionAction` (Phase 82.13) so callers
/// don't have to import the daemon-side enum.
#[derive(Debug, Clone, Serialize)]
pub struct SendReplyArgs {
    /// Body text or template payload.
    pub body: String,
    /// `"text"` / `"template"` / `"media"`. Defaults to `"text"`
    /// when omitted via [`SendReplyArgs::text`].
    pub msg_kind: String,
    /// Optional channel-specific attachments.
    #[serde(default)]
    pub attachments: Vec<Value>,
    /// Optional reply-to message id when the channel supports
    /// threaded replies.
    #[serde(default)]
    pub reply_to_msg_id: Option<String>,
    /// Phase 82.13.b.1 — optional session id. When provided AND
    /// the daemon has a transcript appender wired (production
    /// always does), the operator's reply is appended to the
    /// agent transcript so the agent reanudes coherently after
    /// release. When `None`, the channel send still happens but
    /// the transcript is not modified — operator UIs should
    /// surface the `transcript_stamped: false` ack hint.
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

impl SendReplyArgs {
    /// Convenience constructor for the common `"text"` reply.
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            msg_kind: "text".into(),
            attachments: Vec::new(),
            reply_to_msg_id: None,
            session_id: None,
        }
    }

    /// Phase 82.13.b.1 — fluent setter for the session id used
    /// to stamp the operator reply on the agent transcript.
    pub fn with_session(mut self, session_id: Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }
}

/// Operator takeover orchestrator. Holds the scope it was engaged
/// against and dispatches every subsequent call against the same
/// scope so callers don't repeat the conversation tuple.
#[derive(Debug)]
pub struct HumanTakeover<'a> {
    admin: &'a AdminClient,
    scope: ProcessingScope,
    operator_token_hash: String,
    /// Phase 82.13.b.2 — session id remembered for `release()` so
    /// the operator can pass `summary_for_agent` without
    /// re-supplying the session. Set via [`Self::with_session`]
    /// (idempotent, returns `Self` so chains work).
    session_id: Option<Uuid>,
}

impl<'a> HumanTakeover<'a> {
    /// Pause the scope (idempotent — `processing/pause` returns
    /// `changed = false` when it was already paused). Returns the
    /// orchestrator the caller threads into `send_reply` and
    /// `release`.
    pub async fn engage(
        admin: &'a AdminClient,
        scope: ProcessingScope,
        operator_token_hash: impl Into<String>,
        reason: Option<String>,
    ) -> Result<HumanTakeover<'a>, AdminError> {
        let operator_token_hash = operator_token_hash.into();
        let mut params = json!({
            "scope": scope,
            "operator_token_hash": operator_token_hash.clone(),
        });
        if let Some(r) = reason {
            params["reason"] = json!(r);
        }
        let _ack: Value = admin.call("nexo/admin/processing/pause", params).await?;
        Ok(HumanTakeover {
            admin,
            scope,
            operator_token_hash,
            session_id: None,
        })
    }

    /// Phase 82.13.b.2 — pin the session id used for transcript
    /// stamping (operator reply via [`Self::send_reply`]) and
    /// summary injection (operator narrative via
    /// [`Self::release`]). Both calls fall back to per-call
    /// arguments when this is not set; pinning here lets the
    /// operator UI configure once and forget.
    pub fn with_session(mut self, session_id: Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Dispatch one operator reply through
    /// `processing/intervention`. The daemon returns once the
    /// channel-outbound adapter has handed the message off to the
    /// channel plugin.
    pub async fn send_reply(
        &self,
        channel: impl Into<String>,
        account_id: impl Into<String>,
        to: impl Into<String>,
        args: SendReplyArgs,
    ) -> Result<Value, AdminError> {
        let mut params = json!({
            "scope": self.scope,
            "operator_token_hash": self.operator_token_hash,
            "action": {
                "kind": "reply",
                "channel": channel.into(),
                "account_id": account_id.into(),
                "to": to.into(),
                "body": args.body,
                "msg_kind": args.msg_kind,
                "attachments": args.attachments,
                "reply_to_msg_id": args.reply_to_msg_id,
            },
        });
        // Phase 82.13.b.1 — thread session_id when the caller
        // provided one. Falls back to the orchestrator's pinned
        // session (set via [`Self::with_session`]). Skip the
        // field on the wire when both are None so legacy
        // daemons keep deserialising the params.
        let effective_session = args.session_id.or(self.session_id);
        if let Some(sid) = effective_session {
            params["session_id"] = json!(sid);
        }
        self.admin
            .call("nexo/admin/processing/intervention", params)
            .await
    }

    /// Resume the agent. Drops the orchestrator.
    ///
    /// Phase 82.13.b.2 — when `summary_for_agent` is `Some` AND
    /// the orchestrator has a session pinned (via
    /// [`Self::with_session`]), the daemon stamps a `System`
    /// transcript entry `[operator_summary] <body>` so the
    /// agent reads the operator's narrative on its next turn.
    /// Without a pinned session, passing a summary returns a
    /// `-32602 session_id_required_with_summary` from the
    /// daemon — surface it to the operator UI as "open the
    /// conversation first".
    pub async fn release(
        self,
        summary_for_agent: Option<String>,
    ) -> Result<(), AdminError> {
        let mut params = json!({
            "scope": self.scope,
            "operator_token_hash": self.operator_token_hash,
        });
        if let Some(sid) = self.session_id {
            params["session_id"] = json!(sid);
        }
        if let Some(summary) = summary_for_agent {
            params["summary_for_agent"] = json!(summary);
        }
        let _: Value = self
            .admin
            .call("nexo/admin/processing/resume", params)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::AdminSender;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct ScriptedSender {
        sent: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl AdminSender for ScriptedSender {
        async fn send_line(&self, line: String) -> Result<(), AdminError> {
            let v: Value = serde_json::from_str(&line).unwrap();
            self.sent.lock().unwrap().push(v);
            Ok(())
        }
    }

    fn convo() -> ProcessingScope {
        ProcessingScope::Conversation {
            agent_id: "ana".into(),
            channel: "whatsapp".into(),
            account_id: "wa.0".into(),
            contact_id: "wa.55".into(),
            mcp_channel_source: None,
        }
    }

    /// Drive an `AdminClient` test by fulfilling its outbound
    /// frames in order with synthesised responses.
    async fn run_with_responses(
        admin: AdminClient,
        sender: Arc<ScriptedSender>,
        responder: impl Fn(&Value) -> Value + Send + 'static,
        future: impl std::future::Future<Output = Result<(), AdminError>> + Send + 'static,
    ) -> Result<(), AdminError> {
        let admin_for_pump = admin.clone();
        let pump = tokio::spawn(async move {
            // Drain frames until the future completes — at most we
            // expect ~3 frames per test, so a tight poll loop is
            // fine.
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let to_handle: Vec<Value> = {
                    let mut sent = sender.sent.lock().unwrap();
                    std::mem::take(&mut *sent)
                };
                for frame in to_handle {
                    let id = frame["id"].as_str().unwrap().to_string();
                    let response = responder(&frame);
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": response,
                    });
                    admin_for_pump.on_inbound_response(&id, &resp);
                }
                if admin_for_pump.pending_len() == 0 {
                    // No outstanding requests — but the task may
                    // still be in flight; let it progress before
                    // we exit.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    if admin_for_pump.pending_len() == 0 {
                        return;
                    }
                }
            }
        });
        let result = future.await;
        // Cancel the pump if it didn't drain naturally.
        pump.abort();
        let _ = pump.await;
        result
    }

    #[tokio::test]
    async fn engage_send_release_invokes_pause_intervention_resume_in_order() {
        let sender = Arc::new(ScriptedSender::default());
        let admin = AdminClient::with_timeout(sender.clone(), Duration::from_secs(2));
        let admin_for_calls = admin.clone();

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();

        let task = async move {
            let takeover = HumanTakeover::engage(
                &admin_for_calls,
                convo(),
                "h0",
                Some("operator".into()),
            )
            .await?;
            takeover
                .send_reply(
                    "whatsapp",
                    "wa.0",
                    "wa.55",
                    SendReplyArgs::text("hello"),
                )
                .await?;
            takeover.release(None).await
        };

        let responder = move |frame: &Value| {
            captured_clone
                .lock()
                .unwrap()
                .push(frame["method"].as_str().unwrap().to_string());
            json!({ "changed": true, "correlation_id": "00000000-0000-0000-0000-000000000000" })
        };
        let result = run_with_responses(admin, sender, responder, task).await;
        result.expect("takeover round trip");

        let methods = captured.lock().unwrap().clone();
        assert_eq!(
            methods,
            vec![
                "nexo/admin/processing/pause".to_string(),
                "nexo/admin/processing/intervention".to_string(),
                "nexo/admin/processing/resume".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn send_reply_args_text_helper_defaults_to_text_kind() {
        let a = SendReplyArgs::text("hi");
        assert_eq!(a.msg_kind, "text");
        assert!(a.attachments.is_empty());
        assert!(a.reply_to_msg_id.is_none());
        assert!(a.session_id.is_none());
    }

    /// Phase 82.13.b.1 — `with_session` threads the session id
    /// onto the intervention frame so the daemon stamps the
    /// reply on the agent transcript.
    #[tokio::test]
    async fn send_reply_with_session_threads_session_id_onto_wire() {
        let sender = Arc::new(ScriptedSender::default());
        let admin = AdminClient::with_timeout(sender.clone(), Duration::from_secs(2));
        let admin_for_calls = admin.clone();

        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let session_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();

        let task = async move {
            let takeover =
                HumanTakeover::engage(&admin_for_calls, convo(), "h0", None).await?;
            takeover
                .send_reply(
                    "whatsapp",
                    "wa.0",
                    "wa.55",
                    SendReplyArgs::text("hello").with_session(session_id),
                )
                .await?;
            Ok(())
        };

        let responder = move |frame: &Value| {
            captured_clone.lock().unwrap().push(frame.clone());
            json!({
                "changed": true,
                "correlation_id": "00000000-0000-0000-0000-000000000000",
                "transcript_stamped": true,
            })
        };
        let result = run_with_responses(admin, sender, responder, task).await;
        result.expect("send_reply with session round trip");

        let frames = captured.lock().unwrap().clone();
        // pause + intervention. Pause has no session_id;
        // intervention does.
        let intervention = frames
            .iter()
            .find(|f| f["method"] == "nexo/admin/processing/intervention")
            .expect("intervention frame present");
        assert_eq!(
            intervention["params"]["session_id"],
            json!(session_id.to_string())
        );
    }

    /// Phase 82.13.b.2 — `release(Some("..."))` after
    /// `.with_session(id)` threads both `session_id` and
    /// `summary_for_agent` onto the resume frame.
    #[tokio::test]
    async fn release_with_session_and_summary_threads_both_onto_wire() {
        let sender = Arc::new(ScriptedSender::default());
        let admin = AdminClient::with_timeout(sender.clone(), Duration::from_secs(2));
        let admin_for_calls = admin.clone();

        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let session_id = Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap();

        let task = async move {
            let takeover =
                HumanTakeover::engage(&admin_for_calls, convo(), "h0", None)
                    .await?
                    .with_session(session_id);
            takeover
                .release(Some("cliente confirmó dirección".into()))
                .await
        };

        let responder = move |frame: &Value| {
            captured_clone.lock().unwrap().push(frame.clone());
            json!({
                "changed": true,
                "correlation_id": "00000000-0000-0000-0000-000000000000",
                "transcript_stamped": true,
            })
        };
        run_with_responses(admin, sender, responder, task)
            .await
            .expect("release with summary round trip");

        let frames = captured.lock().unwrap().clone();
        let resume = frames
            .iter()
            .find(|f| f["method"] == "nexo/admin/processing/resume")
            .expect("resume frame present");
        assert_eq!(
            resume["params"]["session_id"],
            json!(session_id.to_string())
        );
        assert_eq!(
            resume["params"]["summary_for_agent"],
            json!("cliente confirmó dirección")
        );
    }

    /// Phase 82.13.b.2 — `release(None)` MUST NOT emit
    /// `summary_for_agent` on the wire even when a session is
    /// pinned. Allows operators to release without a summary.
    #[tokio::test]
    async fn release_without_summary_omits_field_on_wire() {
        let sender = Arc::new(ScriptedSender::default());
        let admin = AdminClient::with_timeout(sender.clone(), Duration::from_secs(2));
        let admin_for_calls = admin.clone();

        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let session_id = Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap();

        let task = async move {
            let takeover =
                HumanTakeover::engage(&admin_for_calls, convo(), "h0", None)
                    .await?
                    .with_session(session_id);
            takeover.release(None).await
        };

        let responder = move |frame: &Value| {
            captured_clone.lock().unwrap().push(frame.clone());
            json!({ "changed": true, "correlation_id": "00000000-0000-0000-0000-000000000000" })
        };
        run_with_responses(admin, sender, responder, task)
            .await
            .expect("release no summary round trip");

        let frames = captured.lock().unwrap().clone();
        let resume = frames
            .iter()
            .find(|f| f["method"] == "nexo/admin/processing/resume")
            .expect("resume frame present");
        // session_id present (pinned), summary absent.
        assert_eq!(
            resume["params"]["session_id"],
            json!(session_id.to_string())
        );
        assert!(resume["params"].get("summary_for_agent").is_none());
    }

    /// Round-trip without `with_session` MUST omit `session_id`
    /// on the wire (graceful absence — pre-Phase 82.13.b daemons
    /// keep accepting these frames).
    #[tokio::test]
    async fn send_reply_without_session_skips_field_on_wire() {
        let sender = Arc::new(ScriptedSender::default());
        let admin = AdminClient::with_timeout(sender.clone(), Duration::from_secs(2));
        let admin_for_calls = admin.clone();

        let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();

        let task = async move {
            let takeover =
                HumanTakeover::engage(&admin_for_calls, convo(), "h0", None).await?;
            takeover
                .send_reply("whatsapp", "wa.0", "wa.55", SendReplyArgs::text("hi"))
                .await?;
            Ok(())
        };

        let responder = move |frame: &Value| {
            captured_clone.lock().unwrap().push(frame.clone());
            json!({ "changed": true, "correlation_id": "00000000-0000-0000-0000-000000000000" })
        };
        run_with_responses(admin, sender, responder, task)
            .await
            .expect("round trip");

        let frames = captured.lock().unwrap().clone();
        let intervention = frames
            .iter()
            .find(|f| f["method"] == "nexo/admin/processing/intervention")
            .expect("intervention frame present");
        assert!(intervention["params"].get("session_id").is_none());
    }
}
