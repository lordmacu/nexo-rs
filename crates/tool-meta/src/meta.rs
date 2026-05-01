//! Helpers around the `_meta` payload nexo emits on every tool
//! call. The wire shape is dual-write:
//!
//! - flat backward-compat block at the top (`agent_id`,
//!   `session_id`),
//! - nested `nexo.binding` block carrying the full
//!   [`BindingContext`] when an inbound binding matched,
//! - nested `nexo.inbound` block carrying the
//!   [`InboundMessageMeta`] when the runtime knows which message
//!   triggered the turn.
//!
//! ```text
//! {
//!   "agent_id": "ana",
//!   "session_id": "00000000-0000-0000-0000-000000000000",
//!   "nexo": {
//!     "binding": {
//!       "agent_id": "ana",
//!       "channel": "whatsapp",
//!       "account_id": "personal",
//!       "binding_id": "whatsapp:personal"
//!     },
//!     "inbound": {
//!       "kind": "external_user",
//!       "sender_id": "+5491100",
//!       "msg_id": "wa.ABCD1234",
//!       "inbound_ts": "2026-05-01T12:34:56Z"
//!     }
//!   }
//! }
//! ```
//!
//! Functions exposed here are inverses:
//! - [`build_meta_value`] is what the daemon emits.
//! - [`parse_binding_from_meta`] / [`parse_inbound_from_meta`]
//!   are what a microapp consumes.

use serde_json::Value;
use uuid::Uuid;

use crate::binding::BindingContext;
use crate::inbound::InboundMessageMeta;

/// Top-level key the JSON-RPC dispatcher writes the `_meta`
/// payload under.
pub const META_KEY: &str = "_meta";

/// Inner namespace key wrapping nexo-specific metadata.
pub const NEXO_NAMESPACE: &str = "nexo";

/// Key under [`NEXO_NAMESPACE`] that holds the [`BindingContext`].
pub const BINDING_KEY: &str = "binding";

/// Key under [`NEXO_NAMESPACE`] that holds the
/// [`InboundMessageMeta`].
pub const INBOUND_KEY: &str = "inbound";

/// Build the `_meta` payload sent on every tool call.
///
/// Single source of truth used by both Phase 11 stdio extensions
/// and Phase 12 MCP `tools/call`. The shape is dual-write so a
/// pre-Phase-82 consumer reading only `agent_id` keeps working
/// even when the same call carries the full nested binding.
///
/// - `binding == None` (delegation receive, heartbeat bootstrap,
///   tests with no binding match) skips the nested `nexo.binding`
///   block.
/// - `inbound == None` (legacy producers not yet migrated, tests)
///   skips the nested `nexo.inbound` block.
/// - When both are `None`, the payload reduces to the flat
///   backward-compat shape.
///
/// # Example
///
/// ```
/// use nexo_tool_meta::{build_meta_value, BindingContext, InboundMessageMeta};
/// use uuid::Uuid;
///
/// let session = Uuid::nil();
/// let mut binding = BindingContext::agent_only("ana");
/// binding.session_id = Some(session);
/// binding.channel = Some("whatsapp".into());
/// binding.account_id = Some("personal".into());
/// binding.binding_id = Some("whatsapp:personal".into());
/// let inbound = InboundMessageMeta::external_user("+5491100", "wa.ABCD");
///
/// let meta = build_meta_value("ana", Some(session), Some(&binding), Some(&inbound));
/// assert_eq!(meta["agent_id"], "ana");
/// assert_eq!(meta["nexo"]["binding"]["channel"], "whatsapp");
/// assert_eq!(meta["nexo"]["inbound"]["sender_id"], "+5491100");
/// ```
pub fn build_meta_value(
    agent_id: &str,
    session_id: Option<Uuid>,
    binding: Option<&BindingContext>,
    inbound: Option<&InboundMessageMeta>,
) -> Value {
    let mut meta = serde_json::Map::new();
    meta.insert("agent_id".into(), Value::String(agent_id.to_string()));
    meta.insert(
        "session_id".into(),
        session_id
            .map(|u| Value::String(u.to_string()))
            .unwrap_or(Value::Null),
    );
    if binding.is_some() || inbound.is_some() {
        let mut nexo = serde_json::Map::new();
        if let Some(b) = binding {
            nexo.insert(BINDING_KEY.into(), serde_json::to_value(b).unwrap_or(Value::Null));
        }
        if let Some(i) = inbound {
            nexo.insert(INBOUND_KEY.into(), serde_json::to_value(i).unwrap_or(Value::Null));
        }
        meta.insert(NEXO_NAMESPACE.into(), Value::Object(nexo));
    }
    Value::Object(meta)
}

/// Parse a `BindingContext` from a `_meta` payload received by a
/// microapp.
///
/// Returns `None` when:
/// - the value is not a JSON object,
/// - the nested `nexo.binding` block is absent (bindingless
///   path — delegation receive, heartbeat, tests),
/// - the nested block is malformed.
///
/// Forward-compatible: extra keys inside the binding object are
/// ignored thanks to serde's default permissive deserialisation
/// and the `#[non_exhaustive]` marker on [`BindingContext`].
///
/// # Example
///
/// ```
/// use nexo_tool_meta::{build_meta_value, parse_binding_from_meta, BindingContext};
/// use uuid::Uuid;
///
/// let mut original = BindingContext::agent_only("ana");
/// original.channel = Some("whatsapp".into());
/// original.account_id = Some("personal".into());
/// original.binding_id = Some("whatsapp:personal".into());
///
/// let meta = build_meta_value("ana", Some(Uuid::nil()), Some(&original), None);
/// let parsed = parse_binding_from_meta(&meta).expect("binding present");
/// assert_eq!(parsed.channel.as_deref(), Some("whatsapp"));
/// ```
pub fn parse_binding_from_meta(meta: &Value) -> Option<BindingContext> {
    let nexo = meta.as_object()?.get(NEXO_NAMESPACE)?;
    let binding = nexo.as_object()?.get(BINDING_KEY)?;
    serde_json::from_value(binding.clone()).ok()
}

/// Parse an [`InboundMessageMeta`] from a `_meta` payload received
/// by a microapp.
///
/// Returns `None` when:
/// - the value is not a JSON object,
/// - the nested `nexo.inbound` block is absent (legacy producer
///   not yet migrated, or path that does not produce inbound meta),
/// - the nested block is malformed (e.g. unknown `kind` string).
///
/// Forward-compatible: extra keys inside the inbound object are
/// ignored thanks to serde's default permissive deserialisation
/// and the `#[non_exhaustive]` marker on [`InboundMessageMeta`].
///
/// # Example
///
/// ```
/// use nexo_tool_meta::{build_meta_value, parse_inbound_from_meta, InboundMessageMeta};
///
/// let inbound = InboundMessageMeta::external_user("+5491100", "wa.ABCD");
/// let meta = build_meta_value("ana", None, None, Some(&inbound));
/// let parsed = parse_inbound_from_meta(&meta).expect("inbound present");
/// assert_eq!(parsed.sender_id.as_deref(), Some("+5491100"));
/// ```
pub fn parse_inbound_from_meta(meta: &Value) -> Option<InboundMessageMeta> {
    let nexo = meta.as_object()?.get(NEXO_NAMESPACE)?;
    let inbound = nexo.as_object()?.get(INBOUND_KEY)?;
    serde_json::from_value(inbound.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_with_binding_emits_dual_namespaces() {
        let mut b = BindingContext::agent_only("ana");
        b.session_id = Some(Uuid::nil());
        b.channel = Some("whatsapp".into());
        b.account_id = Some("personal".into());
        b.binding_id = Some("whatsapp:personal".into());

        let meta = build_meta_value("ana", Some(Uuid::nil()), Some(&b), None);
        assert_eq!(meta["agent_id"], "ana");
        assert!(meta["session_id"].is_string());
        let nested = &meta["nexo"]["binding"];
        assert_eq!(nested["agent_id"], "ana");
        assert_eq!(nested["channel"], "whatsapp");
        assert_eq!(nested["account_id"], "personal");
        assert_eq!(nested["binding_id"], "whatsapp:personal");
    }

    #[test]
    fn build_without_binding_emits_legacy_block_only() {
        let meta = build_meta_value("delegation", None, None, None);
        assert_eq!(meta["agent_id"], "delegation");
        assert!(meta["session_id"].is_null());
        assert!(meta.get(NEXO_NAMESPACE).is_none());
    }

    #[test]
    fn parse_round_trips_with_build() {
        let mut original = BindingContext::agent_only("carlos");
        original.channel = Some("telegram".into());
        original.account_id = Some("kate_tg".into());
        original.binding_id = Some("telegram:kate_tg".into());
        original.mcp_channel_source = Some("slack".into());

        let meta = build_meta_value("carlos", Some(Uuid::from_u128(7)), Some(&original), None);
        let back = parse_binding_from_meta(&meta).expect("binding parses");
        assert_eq!(back, original);
    }

    #[test]
    fn parse_returns_none_for_legacy_only_payload() {
        let meta = build_meta_value("delegation", None, None, None);
        assert!(parse_binding_from_meta(&meta).is_none());
    }

    #[test]
    fn parse_returns_none_for_non_object_value() {
        let v = serde_json::json!("not-an-object");
        assert!(parse_binding_from_meta(&v).is_none());
    }

    #[test]
    fn parse_returns_none_for_malformed_binding_block() {
        let v = serde_json::json!({
            "nexo": {
                "binding": "not-an-object"
            }
        });
        assert!(parse_binding_from_meta(&v).is_none());
    }

    #[test]
    fn parse_tolerates_extra_keys_in_binding_block() {
        // Forward-compat: a microapp built against v0.1.0 keeps
        // working when the daemon emits an extra field that
        // future v0.2 of the type will define.
        let v = serde_json::json!({
            "nexo": {
                "binding": {
                    "agent_id": "ana",
                    "channel": "whatsapp",
                    "future_field": "ignored"
                }
            }
        });
        let ctx = parse_binding_from_meta(&v).expect("parses with extras");
        assert_eq!(ctx.agent_id, "ana");
        assert_eq!(ctx.channel.as_deref(), Some("whatsapp"));
    }

    #[test]
    fn build_session_id_serialises_as_string() {
        let sid = Uuid::from_u128(0x42);
        let meta = build_meta_value("x", Some(sid), None, None);
        assert_eq!(meta["session_id"], sid.to_string());
    }

    #[test]
    fn build_with_inbound_emits_nested_inbound_block() {
        let inbound = InboundMessageMeta::external_user("+5491100", "wa.ABCD");
        let meta = build_meta_value("ana", None, None, Some(&inbound));
        let nested = &meta["nexo"]["inbound"];
        assert_eq!(nested["kind"], "external_user");
        assert_eq!(nested["sender_id"], "+5491100");
        assert_eq!(nested["msg_id"], "wa.ABCD");
        // binding bucket absent.
        assert!(meta["nexo"].as_object().unwrap().get("binding").is_none());
    }

    #[test]
    fn parse_inbound_returns_none_when_block_absent() {
        let meta = build_meta_value("delegation", None, None, None);
        assert!(parse_inbound_from_meta(&meta).is_none());
    }

    #[test]
    fn build_with_binding_and_inbound_emits_both_peer_buckets() {
        let mut binding = BindingContext::agent_only("ana");
        binding.channel = Some("whatsapp".into());
        binding.account_id = Some("personal".into());
        binding.binding_id = Some("whatsapp:personal".into());
        let inbound = InboundMessageMeta::external_user("+5491100", "wa.ABCD");

        let meta = build_meta_value("ana", Some(Uuid::nil()), Some(&binding), Some(&inbound));
        assert_eq!(meta["nexo"]["binding"]["channel"], "whatsapp");
        assert_eq!(meta["nexo"]["inbound"]["sender_id"], "+5491100");

        let parsed_b = parse_binding_from_meta(&meta).expect("binding parses");
        let parsed_i = parse_inbound_from_meta(&meta).expect("inbound parses");
        assert_eq!(parsed_b.channel.as_deref(), Some("whatsapp"));
        assert_eq!(parsed_i.sender_id.as_deref(), Some("+5491100"));
    }

    #[test]
    fn parse_inbound_tolerates_extra_keys_in_inbound_block() {
        let v = serde_json::json!({
            "nexo": {
                "inbound": {
                    "kind": "external_user",
                    "sender_id": "+5491100",
                    "future_field": "ignored"
                }
            }
        });
        let parsed = parse_inbound_from_meta(&v).expect("parses with extras");
        assert_eq!(parsed.sender_id.as_deref(), Some("+5491100"));
    }
}
