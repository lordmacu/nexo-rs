//! `ExtensionPoller` — wraps an `agent-extensions::StdioRuntime` and
//! implements `agent_poller::Poller`. The runner treats it just like
//! a built-in module, so operators can ship a `poller` extension
//! written in any language that speaks JSON-RPC over stdio.
//!
//! ## Wire protocol
//!
//! The extension MUST handle one method:
//!
//! ```text
//! method: poll_tick
//! params: {
//!   "kind":    "<the kind string this tick targets>",
//!   "job_id":  "<job id>",
//!   "agent_id":"<agent id>",
//!   "cursor":  null | "<base64 url-safe string>",
//!   "config":  <opaque JSON value — the job's config: block>,
//!   "now":     "<RFC3339 timestamp>"
//! }
//!
//! result: {
//!   "items_seen":       <u32>,
//!   "items_dispatched": <u32>,
//!   "deliver": [
//!     { "channel": "whatsapp"|"telegram"|"google",
//!       "recipient": "<jid|chat_id|email>",
//!       "payload":   <JSON>
//!     },
//!     ...
//!   ],
//!   "next_cursor":         null | "<base64 url-safe string>",
//!   "next_interval_secs":  null | <u64>
//! }
//! ```
//!
//! Errors must use a JSON-RPC error response with `code`:
//! - `-32001` for `Transient` (network blip, 5xx)
//! - `-32002` for `Permanent` (token revoked, scope changed)
//! - `-32602` for `Config` (validation failure)
//! Any other code is treated as `Transient`.

use std::sync::Arc;
use std::time::Duration;

use agent_extensions::StdioRuntime;
use agent_poller::{
    OutboundDelivery, PollContext, Poller, PollerError, TickOutcome,
};
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

const ERR_TRANSIENT: i32 = -32001;
const ERR_PERMANENT: i32 = -32002;
const ERR_CONFIG: i32 = -32602;

pub struct ExtensionPoller {
    /// One stdio subprocess can advertise multiple `kind`s; this
    /// struct is one binding (extension × kind) registered to the
    /// runner.
    kind: &'static str,
    runtime: Arc<StdioRuntime>,
}

impl ExtensionPoller {
    pub fn new(kind: &'static str, runtime: Arc<StdioRuntime>) -> Self {
        Self { kind, runtime }
    }
}

#[async_trait]
impl Poller for ExtensionPoller {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn description(&self) -> &'static str {
        "(extension)"
    }

    fn validate(&self, _config: &Value) -> Result<(), PollerError> {
        // The extension owns its own validation in `poll_tick`. We
        // could add a `poll_validate` round-trip in the future; for
        // V1 errors surface on the first tick.
        Ok(())
    }

    async fn tick(&self, ctx: &PollContext) -> Result<TickOutcome, PollerError> {
        let cursor_b64 = ctx
            .cursor
            .as_deref()
            .map(|b| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b));
        let params = json!({
            "kind":     self.kind,
            "job_id":   ctx.job_id,
            "agent_id": ctx.agent_id,
            "cursor":   cursor_b64,
            "config":   ctx.config,
            "now":      ctx.now.to_rfc3339(),
        });

        let resp = self
            .runtime
            .call("poll_tick", params)
            .await
            .map_err(map_call_error)?;

        let parsed: TickResponse = serde_json::from_value(resp).map_err(|e| {
            PollerError::Transient(anyhow::anyhow!(
                "extension '{}' returned malformed poll_tick response: {e}",
                self.kind
            ))
        })?;

        let next_cursor = parsed
            .next_cursor
            .as_deref()
            .and_then(|s| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(s.trim_end_matches('='))
                    .ok()
            });

        let next_interval_hint = parsed
            .next_interval_secs
            .map(Duration::from_secs);

        let deliver = parsed
            .deliver
            .into_iter()
            .map(|d| {
                Ok::<_, PollerError>(OutboundDelivery {
                    channel: channel_from_str(&d.channel)?,
                    recipient: d.recipient,
                    payload: d.payload,
                })
            })
            .collect::<Result<Vec<_>, PollerError>>()?;

        Ok(TickOutcome {
            items_seen: parsed.items_seen,
            items_dispatched: parsed.items_dispatched,
            deliver,
            next_cursor,
            next_interval_hint,
        })
    }
}

/// Walk the runtime's manifest capabilities and register one
/// `ExtensionPoller` per `kind`. Returns the count of registered
/// pollers so the caller can log it.
pub fn register_for_runtime(
    runner: &agent_poller::PollerRunner,
    runtime: &Arc<StdioRuntime>,
    pollers: &[String],
) -> usize {
    let mut count = 0;
    for kind in pollers {
        // `Poller::kind()` returns &'static str, so we have to leak.
        // Extension pollers are registered once at boot and live for
        // the daemon's lifetime — leaking a few short kind strings is
        // a controlled, bounded cost.
        let leaked: &'static str = Box::leak(kind.clone().into_boxed_str());
        runner.register(Arc::new(ExtensionPoller::new(leaked, Arc::clone(runtime))));
        count += 1;
    }
    count
}

fn channel_from_str(s: &str) -> Result<agent_auth::Channel, PollerError> {
    match s {
        "whatsapp" => Ok(agent_auth::handle::WHATSAPP),
        "telegram" => Ok(agent_auth::handle::TELEGRAM),
        "google" => Ok(agent_auth::handle::GOOGLE),
        other => Err(PollerError::Config {
            job: "<extension>".into(),
            reason: format!("unknown deliver.channel '{other}' from extension"),
        }),
    }
}

fn map_call_error(err: agent_extensions::CallError) -> PollerError {
    use agent_extensions::CallError::*;
    match err {
        Rpc(rpc) => match rpc.code {
            ERR_PERMANENT => PollerError::Permanent(anyhow::anyhow!("ext: {}", rpc.message)),
            ERR_CONFIG => PollerError::Config {
                job: "<extension>".into(),
                reason: rpc.message,
            },
            ERR_TRANSIENT => PollerError::Transient(anyhow::anyhow!("ext: {}", rpc.message)),
            _ => PollerError::Transient(anyhow::anyhow!(
                "ext rpc error code={}: {}",
                rpc.code,
                rpc.message
            )),
        },
        other => PollerError::Transient(anyhow::anyhow!("ext call error: {other}")),
    }
}

#[derive(Debug, Deserialize)]
struct TickResponse {
    #[serde(default)]
    items_seen: u32,
    #[serde(default)]
    items_dispatched: u32,
    #[serde(default)]
    deliver: Vec<DeliveryWire>,
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    next_interval_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DeliveryWire {
    channel: String,
    recipient: String,
    payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_response() {
        let raw = json!({
            "items_seen": 3,
            "items_dispatched": 2,
            "deliver": [
                { "channel": "telegram", "recipient": "1", "payload": { "text": "x" } }
            ],
            "next_cursor": null
        });
        let parsed: TickResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.items_seen, 3);
        assert_eq!(parsed.items_dispatched, 2);
        assert_eq!(parsed.deliver.len(), 1);
    }

    #[test]
    fn channel_mapping() {
        assert!(channel_from_str("whatsapp").is_ok());
        assert!(channel_from_str("telegram").is_ok());
        assert!(channel_from_str("google").is_ok());
        assert!(channel_from_str("xmpp").is_err());
    }

    #[test]
    fn permanent_error_is_classified() {
        let rpc = agent_extensions::RpcError {
            code: ERR_PERMANENT,
            message: "revoked".into(),
            data: None,
        };
        let mapped = map_call_error(agent_extensions::CallError::Rpc(rpc));
        assert!(matches!(mapped, PollerError::Permanent(_)));
    }

    #[test]
    fn transient_error_is_classified() {
        let rpc = agent_extensions::RpcError {
            code: ERR_TRANSIENT,
            message: "503".into(),
            data: None,
        };
        let mapped = map_call_error(agent_extensions::CallError::Rpc(rpc));
        assert!(matches!(mapped, PollerError::Transient(_)));
    }

    #[test]
    fn config_error_is_classified() {
        let rpc = agent_extensions::RpcError {
            code: ERR_CONFIG,
            message: "missing field x".into(),
            data: None,
        };
        let mapped = map_call_error(agent_extensions::CallError::Rpc(rpc));
        assert!(matches!(mapped, PollerError::Config { .. }));
    }
}
