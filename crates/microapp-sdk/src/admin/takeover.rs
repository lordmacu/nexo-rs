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
        })
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
        // provided one. Skip the field on the wire when None so
        // legacy daemons keep deserialising the params.
        if let Some(sid) = args.session_id {
            params["session_id"] = json!(sid);
        }
        self.admin
            .call("nexo/admin/processing/intervention", params)
            .await
    }

    /// Resume the agent. Drops the orchestrator. The
    /// `summary_for_agent` argument is reserved for the synthetic
    /// system-message injection follow-up — v1 ignores it locally
    /// (gap is logged in `FOLLOWUPS.md` as the `synthetic message
    /// inject API` item) and only forwards it once the
    /// daemon-side surface lands.
    pub async fn release(
        self,
        _summary_for_agent: Option<String>,
    ) -> Result<(), AdminError> {
        let params = json!({
            "scope": self.scope,
            "operator_token_hash": self.operator_token_hash,
        });
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
