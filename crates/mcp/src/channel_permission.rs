//! Phase 80.9.b — channel permission relay.
//!
//! When a tool requires user approval, an MCP channel server that
//! opted into [`crate::channel::CHANNEL_PERMISSION_CAPABILITY_KEY`]
//! can be used as a *second* approval surface. The runtime emits
//! a [`PermissionRequestParams`] notification at
//! [`PERMISSION_REQUEST_METHOD`]; the human replies on the
//! underlying platform; the server parses the reply and emits
//! [`PermissionResponseParams`] at [`PERMISSION_RESPONSE_METHOD`].
//! The runtime races the channel reply against the local approval
//! prompt — first one to claim wins.
//!
//! This module ships the *primitives*:
//!
//! - [`short_request_id`] — stable phone-friendly 5-letter ID
//!   derived from the tool-use UUID, with a defensive blocklist
//!   to avoid generating embarrassing strings.
//! - [`truncate_input_preview`] — 200-char JSON preview so phone
//!   notifications carry context without flooding.
//! - [`parse_permission_reply`] — server-side helper that parses
//!   the canonical reply format.
//! - [`PendingPermissionMap`] — process-local rendezvous keyed by
//!   `request_id`; the approval flow registers, the inbound
//!   notification handler resolves.
//! - [`PermissionRelayDispatcher`] — outbound trait the runtime
//!   calls when it wants to ask a channel server to surface a
//!   prompt; default impl reuses `crate::client_trait::McpClient`.
//!
//! The actual approval flow (race the channel reply against the
//! local prompt, claim the first responder, audit the outcome)
//! lives in the higher-level driver-permission crate and gets
//! wired through `ChannelInboundLoop` once 80.9.b.b lands the
//! `CapabilityClaim` change. The MVP today is the protocol
//! surface — every operator-visible knob, every wire frame.

use crate::client_trait::McpClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

// ---------------------------------------------------------------
// Wire constants.
// ---------------------------------------------------------------

/// Outbound: runtime → server. The runtime emits this when a
/// permission prompt opens AND the server has declared the
/// permission capability.
pub const PERMISSION_REQUEST_METHOD: &str =
    "notifications/nexo/channel/permission_request";

/// Inbound: server → runtime. The server emits this *after*
/// parsing the user's reply on its underlying platform.
pub const PERMISSION_RESPONSE_METHOD: &str =
    "notifications/nexo/channel/permission";

/// Schema version embedded in [`PermissionRequestParams`] so
/// servers can react to a future breaking change. Bump when the
/// payload shape gains a non-optional field.
pub const PERMISSION_REQUEST_SCHEMA_VERSION: u32 = 1;

/// Maximum size of the input preview embedded in the outbound
/// permission request. Keeps phone notifications short — full
/// input lives in the local terminal dialog. The server can
/// still display whatever it wants; this is the upper bound the
/// runtime emits.
pub const INPUT_PREVIEW_MAX_CHARS: usize = 200;

// ---------------------------------------------------------------
// Approval decision.
// ---------------------------------------------------------------

/// User's binary decision relayed back through the channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionBehavior {
    Allow,
    Deny,
}

impl PermissionBehavior {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionBehavior::Allow => "allow",
            PermissionBehavior::Deny => "deny",
        }
    }

    /// Parse from the wire string. Accepts `"allow"` or `"deny"`
    /// case-insensitively. Anything else is `None` — the runtime
    /// surfaces malformed responses as a typed error rather than
    /// guessing.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "allow" => Some(PermissionBehavior::Allow),
            "deny" => Some(PermissionBehavior::Deny),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------
// Wire payloads.
// ---------------------------------------------------------------

/// Outbound payload at [`PERMISSION_REQUEST_METHOD`]. The runtime
/// sends one of these per pending tool approval; the server is
/// responsible for rendering it on the underlying platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequestParams {
    /// Schema version. Servers SHOULD reject mismatched versions
    /// loudly so a runtime upgrade doesn't silently flip semantics.
    pub schema: u32,
    /// Short, phone-friendly identifier for the request. The
    /// server echoes this back in the reply so we can match
    /// pending entries without a database lookup.
    pub request_id: String,
    /// Tool name as the model invoked it.
    pub tool_name: String,
    /// One-line description so the user knows what they're
    /// approving without opening the full transcript.
    pub description: String,
    /// JSON-stringified tool input, truncated to
    /// [`INPUT_PREVIEW_MAX_CHARS`] with a `…` suffix when needed.
    pub input_preview: String,
}

/// Inbound payload at [`PERMISSION_RESPONSE_METHOD`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionResponseParams {
    pub request_id: String,
    pub behavior: PermissionBehavior,
}

/// Bundle the runtime stores in [`PendingPermissionMap`] when a
/// channel reply lands. The `from_server` field surfaces *which*
/// channel raced first — useful for audit + structured logs.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionResponse {
    pub request_id: String,
    pub behavior: PermissionBehavior,
    pub from_server: String,
}

// ---------------------------------------------------------------
// ID generation.
// ---------------------------------------------------------------

// 25-letter alphabet: a–z minus `l` (visually confusable with
// 1/I in many fonts). 25^5 ≈ 9.8M IDs.
const ID_ALPHABET: &[u8; 25] = b"abcdefghijkmnopqrstuvwxyz";

/// Substring blocklist. Five random letters can spell things you
/// don't want in a phone notification. Non-exhaustive — covers
/// the obvious tier. If a generated ID contains any of these,
/// the helper re-hashes with a salt suffix.
const ID_AVOID_SUBSTRINGS: &[&str] = &[
    "fuck", "shit", "cunt", "cock", "dick", "twat", "piss", "crap", "bitch", "whore",
    "ass", "tit", "cum", "fag", "dyke", "nig", "kike", "rape", "nazi", "damn",
    "poo", "pee", "wank", "anus",
];

/// FNV-1a 32-bit hash → base-25 5-letter encode. Not crypto.
fn hash_to_id(input: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for b in input.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    let mut s = String::with_capacity(5);
    for _ in 0..5 {
        let idx = (h % 25) as usize;
        s.push(ID_ALPHABET[idx] as char);
        h /= 25;
    }
    s
}

/// Stable 5-letter ID derived from `tool_use_id`. Same input
/// always produces the same ID; the only randomness is the
/// salted re-hash that fires when the result contains a
/// blocklisted substring. 10 retries cap the cost.
pub fn short_request_id(tool_use_id: &str) -> String {
    let mut candidate = hash_to_id(tool_use_id);
    for salt in 0..10 {
        if !ID_AVOID_SUBSTRINGS
            .iter()
            .any(|bad| candidate.contains(bad))
        {
            return candidate;
        }
        candidate = hash_to_id(&format!("{tool_use_id}:{salt}"));
    }
    candidate
}

// ---------------------------------------------------------------
// Input preview.
// ---------------------------------------------------------------

/// Render `input` as JSON and truncate to
/// [`INPUT_PREVIEW_MAX_CHARS`] with a `…` suffix when needed.
/// Anything that fails to serialise becomes the literal
/// `"(unserializable)"`.
pub fn truncate_input_preview(input: &Value) -> String {
    let raw = match serde_json::to_string(input) {
        Ok(s) => s,
        Err(_) => return "(unserializable)".to_string(),
    };
    if raw.chars().count() <= INPUT_PREVIEW_MAX_CHARS {
        return raw;
    }
    let truncated: String = raw.chars().take(INPUT_PREVIEW_MAX_CHARS).collect();
    format!("{truncated}…")
}

// ---------------------------------------------------------------
// Reply parsing.
// ---------------------------------------------------------------

/// Parse the canonical reply format `^\s*(y|yes|n|no)\s+([a-km-z]{5})\s*$`
/// (case-insensitive, no `l` in the ID alphabet).
///
/// Servers SHOULD do their own parsing and emit the structured
/// [`PermissionResponseParams`] event — the runtime never matches
/// against general-channel text. This helper is exported so
/// servers don't have to hand-roll the regex.
pub fn parse_permission_reply(text: &str) -> Option<(PermissionBehavior, String)> {
    let trimmed = text.trim();
    // Lowercase only the prefix word so phone autocorrect's
    // capitalisation ("Yes abcde") still parses, but the ID
    // itself is matched as-is — IDs are always emitted lowercase
    // by `short_request_id` and accepting uppercase would
    // weaken the alphabet's anti-confusable promise.
    let mut iter = trimmed.splitn(2, char::is_whitespace);
    let prefix = iter.next()?;
    let rest = iter.next().unwrap_or("");
    let behavior = match prefix.to_ascii_lowercase().as_str() {
        "y" | "yes" => PermissionBehavior::Allow,
        "n" | "no" => PermissionBehavior::Deny,
        _ => return None,
    };
    let id_section = rest.trim_start();
    // Read up to the first whitespace; reject if any trailing
    // non-whitespace remains.
    let (id, tail) = match id_section.find(char::is_whitespace) {
        Some(idx) => (&id_section[..idx], &id_section[idx..]),
        None => (id_section, ""),
    };
    if !tail.trim().is_empty() {
        return None;
    }
    if id.len() != 5 {
        return None;
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() && c != 'l')
    {
        return None;
    }
    Some((behavior, id.to_string()))
}

// ---------------------------------------------------------------
// Pending-permission rendezvous map.
// ---------------------------------------------------------------

/// Errors the registration / resolution path can produce. Typed
/// so callers can surface specific reasons in audit logs.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PermissionRelayError {
    #[error("request_id {0} already pending")]
    AlreadyPending(String),
    #[error("request_id {0} unknown — not pending or already resolved")]
    NotPending(String),
}

/// Process-local pending map. The approval flow calls
/// [`register`](Self::register) when it wants to wait for a
/// channel reply; the inbound handler calls
/// [`resolve`](Self::resolve) when a server emits a response
/// notification.
///
/// Two-phase registration prevents the race where the server
/// replies before the registrant subscribes: the receiver is
/// constructed inside `register` and the sender side is only
/// dropped after the response arrives or the receiver is dropped.
#[derive(Default)]
pub struct PendingPermissionMap {
    inner: Mutex<HashMap<String, oneshot::Sender<PermissionResponse>>>,
}

impl PendingPermissionMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request. Returns the receiver the caller awaits
    /// on. `Err(AlreadyPending)` when an identical id is already
    /// in flight — operators should never see this in practice
    /// (`short_request_id` is stable per tool-use UUID, which is
    /// unique).
    pub async fn register(
        &self,
        request_id: impl Into<String>,
    ) -> Result<oneshot::Receiver<PermissionResponse>, PermissionRelayError> {
        let id = request_id.into();
        let mut guard = self.inner.lock().await;
        if guard.contains_key(&id) {
            return Err(PermissionRelayError::AlreadyPending(id));
        }
        let (tx, rx) = oneshot::channel();
        guard.insert(id, tx);
        Ok(rx)
    }

    /// Resolve a pending request with the user's decision. Drops
    /// the sender side regardless of whether the receiver is
    /// still alive — the caller may have already raced against
    /// the local approval prompt and dropped its receiver.
    /// Returns `true` when the id matched a pending entry, even
    /// when the receiver was already dropped (the server still
    /// got the right answer; nobody listens, but that's not an
    /// error). Returns `Err(NotPending)` when the id is unknown.
    pub async fn resolve(
        &self,
        response: PermissionResponse,
    ) -> Result<bool, PermissionRelayError> {
        let mut guard = self.inner.lock().await;
        let Some(tx) = guard.remove(&response.request_id) else {
            return Err(PermissionRelayError::NotPending(response.request_id));
        };
        // `send` returns Err when the receiver was already
        // dropped. That is the "lost the race" case — no error.
        let delivered = tx.send(response).is_ok();
        Ok(delivered)
    }

    /// Drop a pending entry without resolving. Used by the
    /// approval flow when it claims via the local prompt and
    /// wants to release the channel side.
    pub async fn cancel(&self, request_id: &str) -> bool {
        let mut guard = self.inner.lock().await;
        guard.remove(request_id).is_some()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

pub type SharedPendingPermissionMap = Arc<PendingPermissionMap>;

// ---------------------------------------------------------------
// Inbound parsing.
// ---------------------------------------------------------------

/// Errors a malformed permission notification can produce.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PermissionParseError {
    #[error("unexpected method: {0}")]
    UnexpectedMethod(String),
    #[error("missing or non-object params")]
    MissingParams,
    #[error("missing or non-string params.request_id")]
    MissingRequestId,
    #[error("missing or invalid params.behavior — must be 'allow' or 'deny'")]
    InvalidBehavior,
}

/// Parse a `notifications/nexo/channel/permission` payload into
/// a typed [`PermissionResponseParams`]. Caller passes in the
/// emitting `from_server` so audit logs know which channel
/// raced first.
pub fn parse_permission_response(
    method: &str,
    params: &Value,
) -> Result<PermissionResponseParams, PermissionParseError> {
    if method != PERMISSION_RESPONSE_METHOD {
        return Err(PermissionParseError::UnexpectedMethod(method.to_string()));
    }
    let map = params
        .as_object()
        .ok_or(PermissionParseError::MissingParams)?;
    let request_id = map
        .get("request_id")
        .and_then(|v| v.as_str())
        .ok_or(PermissionParseError::MissingRequestId)?;
    let behavior_raw = map
        .get("behavior")
        .and_then(|v| v.as_str())
        .ok_or(PermissionParseError::InvalidBehavior)?;
    let behavior = PermissionBehavior::parse(behavior_raw)
        .ok_or(PermissionParseError::InvalidBehavior)?;
    Ok(PermissionResponseParams {
        request_id: request_id.to_string(),
        behavior,
    })
}

// ---------------------------------------------------------------
// Outbound dispatcher.
// ---------------------------------------------------------------

/// Trait the runtime calls to ask a server to surface a permission
/// prompt. Trait so tests can stub it without spinning an MCP
/// connection.
#[async_trait::async_trait]
pub trait PermissionRelayDispatcher: Send + Sync {
    /// Emit the outbound notification. Implementations are
    /// expected to be idempotent — a duplicate emission MUST NOT
    /// surface a duplicate prompt to the user (the server is
    /// responsible for de-duping by `request_id`).
    async fn emit_request(
        &self,
        server_name: &str,
        params: &PermissionRequestParams,
    ) -> Result<(), DispatchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("client error: {0}")]
    Client(String),
    #[error("server {0} unknown / not connected")]
    UnknownServer(String),
    #[error("serialise params: {0}")]
    Serialise(String),
}

/// `PermissionRelayDispatcher` impl that uses an [`McpClient`] to
/// emit the notification. Slim adapter — the real work is in
/// `client.send_notification`.
pub struct McpPermissionRelayDispatcher<C: McpClient> {
    client: Arc<C>,
}

impl<C: McpClient> McpPermissionRelayDispatcher<C> {
    pub fn new(client: Arc<C>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl<C: McpClient + 'static> PermissionRelayDispatcher for McpPermissionRelayDispatcher<C> {
    async fn emit_request(
        &self,
        _server_name: &str,
        params: &PermissionRequestParams,
    ) -> Result<(), DispatchError> {
        let payload = serde_json::to_value(params)
            .map_err(|e| DispatchError::Serialise(e.to_string()))?;
        self.client
            .send_notification(PERMISSION_REQUEST_METHOD, payload)
            .await
            .map_err(|e| DispatchError::Client(e.to_string()))
    }
}

/// Phase 80.9.b.b — dispatcher that fans out to every connected
/// MCP client by name. The runtime exposes a `SessionMcpRuntime`
/// snapshot of `(name, Arc<dyn McpClient>)` pairs; this dispatcher
/// looks up `server_name` against that snapshot and forwards via
/// the client's `send_notification`. Snapshot is captured by a
/// caller-supplied closure so the dispatcher stays agnostic about
/// where the client list lives.
pub struct ClientResolverDispatcher {
    resolver: Arc<dyn Fn(&str) -> Option<Arc<dyn McpClient>> + Send + Sync>,
}

impl ClientResolverDispatcher {
    pub fn new(
        resolver: Arc<dyn Fn(&str) -> Option<Arc<dyn McpClient>> + Send + Sync>,
    ) -> Self {
        Self { resolver }
    }
}

/// Phase 80.9.b.b — spawn a task that subscribes to the supplied
/// `McpClient`'s event broadcast, parses every
/// `ChannelPermissionResponse` event, and resolves the matching
/// pending entry in [`PendingPermissionMap`]. Returns the spawned
/// task handle so the caller controls shutdown via the cancel
/// token.
///
/// The task tolerates lag (broadcast `RecvError::Lagged`) and
/// continues; closes only when the broadcast channel terminates
/// or the cancel token fires. Malformed payloads warn-log and
/// drop without affecting other in-flight requests.
pub fn spawn_permission_response_pump(
    client: Arc<dyn McpClient>,
    server_name: String,
    pending: Arc<PendingPermissionMap>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let mut events = client.subscribe_events();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                ev = events.recv() => {
                    use crate::events::ClientEvent;
                    match ev {
                        Ok(ClientEvent::ChannelPermissionResponse { params }) => {
                            let parsed = parse_permission_response(
                                PERMISSION_RESPONSE_METHOD,
                                &params,
                            );
                            match parsed {
                                Ok(payload) => {
                                    let resp = PermissionResponse {
                                        request_id: payload.request_id,
                                        behavior: payload.behavior,
                                        from_server: server_name.clone(),
                                    };
                                    if let Err(e) = pending.resolve(resp).await {
                                        tracing::debug!(
                                            server = %server_name,
                                            error = %e,
                                            "permission response: no matching pending entry (race lost or already resolved)"
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        server = %server_name,
                                        error = %e,
                                        "permission response: malformed payload"
                                    );
                                }
                            }
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        }
    })
}

#[async_trait::async_trait]
impl PermissionRelayDispatcher for ClientResolverDispatcher {
    async fn emit_request(
        &self,
        server_name: &str,
        params: &PermissionRequestParams,
    ) -> Result<(), DispatchError> {
        let client = (self.resolver)(server_name)
            .ok_or_else(|| DispatchError::UnknownServer(server_name.to_string()))?;
        let payload = serde_json::to_value(params)
            .map_err(|e| DispatchError::Serialise(e.to_string()))?;
        client
            .send_notification(PERMISSION_REQUEST_METHOD, payload)
            .await
            .map_err(|e| DispatchError::Client(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- short_request_id ----

    #[test]
    fn short_request_id_is_deterministic() {
        let a = short_request_id("toolu_abc123");
        let b = short_request_id("toolu_abc123");
        assert_eq!(a, b);
        assert_eq!(a.len(), 5);
    }

    #[test]
    fn short_request_id_distinct_inputs_distinct_ids_usually() {
        // Two random toolu IDs should rarely collide. Non-load
        // bearing — assert lengths are right rather than
        // inequality.
        let a = short_request_id("toolu_aaaaaa");
        let b = short_request_id("toolu_bbbbbb");
        assert_eq!(a.len(), 5);
        assert_eq!(b.len(), 5);
    }

    #[test]
    fn short_request_id_only_uses_alphabet() {
        let id = short_request_id("toolu_xyz");
        for c in id.chars() {
            assert!(c.is_ascii_lowercase());
            assert_ne!(c, 'l', "alphabet excludes l");
        }
    }

    #[test]
    fn short_request_id_avoids_blocklisted_substrings() {
        // We can't easily force a blocklist hit without knowing
        // the seed → output mapping, but we can verify the
        // returned IDs never carry any blocklisted substring for
        // a few thousand inputs.
        for i in 0..2000 {
            let input = format!("toolu_{i:020}");
            let id = short_request_id(&input);
            for bad in ID_AVOID_SUBSTRINGS {
                assert!(
                    !id.contains(bad),
                    "id {id} from {input} contains blocklisted {bad}"
                );
            }
        }
    }

    // ---- truncate_input_preview ----

    #[test]
    fn truncate_input_preview_short_passes_through() {
        let v = serde_json::json!({"x": 1});
        let s = truncate_input_preview(&v);
        assert_eq!(s, r#"{"x":1}"#);
    }

    #[test]
    fn truncate_input_preview_long_is_capped_with_ellipsis() {
        let big: String = "x".repeat(500);
        let v = serde_json::json!({"k": big});
        let s = truncate_input_preview(&v);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), INPUT_PREVIEW_MAX_CHARS + 1);
    }

    // ---- parse_permission_reply ----

    #[test]
    fn parse_reply_accepts_yes_id() {
        assert_eq!(
            parse_permission_reply("yes abcde"),
            Some((PermissionBehavior::Allow, "abcde".to_string()))
        );
        assert_eq!(
            parse_permission_reply("y abcde"),
            Some((PermissionBehavior::Allow, "abcde".to_string()))
        );
    }

    #[test]
    fn parse_reply_accepts_no_id() {
        assert_eq!(
            parse_permission_reply("no abcde"),
            Some((PermissionBehavior::Deny, "abcde".to_string()))
        );
        assert_eq!(
            parse_permission_reply("n abcde"),
            Some((PermissionBehavior::Deny, "abcde".to_string()))
        );
    }

    #[test]
    fn parse_reply_case_insensitive() {
        assert_eq!(
            parse_permission_reply("YES abcde"),
            Some((PermissionBehavior::Allow, "abcde".to_string()))
        );
    }

    #[test]
    fn parse_reply_rejects_no_id() {
        assert_eq!(parse_permission_reply("yes"), None);
        assert_eq!(parse_permission_reply("no"), None);
    }

    #[test]
    fn parse_reply_rejects_id_with_l() {
        // 'l' is excluded from the alphabet
        assert_eq!(parse_permission_reply("yes alice"), None);
    }

    #[test]
    fn parse_reply_rejects_uppercase_id() {
        assert_eq!(parse_permission_reply("yes ABCDE"), None);
    }

    #[test]
    fn parse_reply_rejects_short_or_long_id() {
        assert_eq!(parse_permission_reply("yes abcd"), None);
        assert_eq!(parse_permission_reply("yes abcdef"), None);
    }

    #[test]
    fn parse_reply_rejects_trailing_chatter() {
        assert_eq!(parse_permission_reply("yes abcde sure"), None);
    }

    #[test]
    fn parse_reply_strips_surrounding_whitespace() {
        assert_eq!(
            parse_permission_reply("  yes abcde  "),
            Some((PermissionBehavior::Allow, "abcde".to_string()))
        );
    }

    // ---- PendingPermissionMap ----

    #[tokio::test]
    async fn pending_register_then_resolve_delivers() {
        let map = PendingPermissionMap::new();
        let rx = map.register("xyzab".to_string()).await.unwrap();
        let resp = PermissionResponse {
            request_id: "xyzab".into(),
            behavior: PermissionBehavior::Allow,
            from_server: "slack".into(),
        };
        let delivered = map.resolve(resp.clone()).await.unwrap();
        assert!(delivered);
        let got = rx.await.unwrap();
        assert_eq!(got, resp);
        assert_eq!(map.len().await, 0);
    }

    #[tokio::test]
    async fn pending_resolve_unknown_errors() {
        let map = PendingPermissionMap::new();
        let resp = PermissionResponse {
            request_id: "nope".into(),
            behavior: PermissionBehavior::Deny,
            from_server: "slack".into(),
        };
        let err = map.resolve(resp).await.unwrap_err();
        assert!(matches!(err, PermissionRelayError::NotPending(_)));
    }

    #[tokio::test]
    async fn pending_register_duplicate_errors() {
        let map = PendingPermissionMap::new();
        let _rx = map.register("dup12").await.unwrap();
        let err = map.register("dup12").await.unwrap_err();
        assert!(matches!(err, PermissionRelayError::AlreadyPending(_)));
    }

    #[tokio::test]
    async fn pending_resolve_after_dropped_receiver_still_returns_match() {
        // Race won by the local prompt: receiver dropped before
        // the channel reply lands. The map should still report
        // the id matched (server got the right answer; nobody
        // listened, but that's expected).
        let map = PendingPermissionMap::new();
        let rx = map.register("race1").await.unwrap();
        drop(rx);
        let resp = PermissionResponse {
            request_id: "race1".into(),
            behavior: PermissionBehavior::Deny,
            from_server: "telegram".into(),
        };
        let delivered = map.resolve(resp).await.unwrap();
        assert!(!delivered, "receiver was dropped");
        assert_eq!(map.len().await, 0);
    }

    #[tokio::test]
    async fn pending_cancel_drops_entry() {
        let map = PendingPermissionMap::new();
        let _rx = map.register("kill1").await.unwrap();
        assert!(map.cancel("kill1").await);
        assert_eq!(map.len().await, 0);
        assert!(!map.cancel("kill1").await);
    }

    // ---- parse_permission_response ----

    #[test]
    fn parse_response_happy_path() {
        let params = serde_json::json!({
            "request_id": "abcde",
            "behavior": "allow"
        });
        let got =
            parse_permission_response(PERMISSION_RESPONSE_METHOD, &params).unwrap();
        assert_eq!(got.request_id, "abcde");
        assert_eq!(got.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn parse_response_rejects_unknown_method() {
        let params = serde_json::json!({});
        let err =
            parse_permission_response("notifications/something", &params).unwrap_err();
        assert!(matches!(err, PermissionParseError::UnexpectedMethod(_)));
    }

    #[test]
    fn parse_response_rejects_missing_request_id() {
        let params = serde_json::json!({"behavior": "allow"});
        let err = parse_permission_response(PERMISSION_RESPONSE_METHOD, &params)
            .unwrap_err();
        assert_eq!(err, PermissionParseError::MissingRequestId);
    }

    #[test]
    fn parse_response_rejects_unknown_behavior() {
        let params = serde_json::json!({"request_id": "x", "behavior": "maybe"});
        let err = parse_permission_response(PERMISSION_RESPONSE_METHOD, &params)
            .unwrap_err();
        assert_eq!(err, PermissionParseError::InvalidBehavior);
    }

    // ---- behavior parsing + serde ----

    #[test]
    fn behavior_serde_round_trip() {
        for b in [PermissionBehavior::Allow, PermissionBehavior::Deny] {
            let s = serde_json::to_string(&b).unwrap();
            let back: PermissionBehavior = serde_json::from_str(&s).unwrap();
            assert_eq!(b, back);
        }
    }

    #[test]
    fn behavior_parse_string() {
        assert_eq!(
            PermissionBehavior::parse("allow"),
            Some(PermissionBehavior::Allow)
        );
        assert_eq!(
            PermissionBehavior::parse("DENY"),
            Some(PermissionBehavior::Deny)
        );
        assert_eq!(PermissionBehavior::parse("maybe"), None);
    }

    // ---- request params serialization ----

    #[test]
    fn request_params_round_trip() {
        let p = PermissionRequestParams {
            schema: PERMISSION_REQUEST_SCHEMA_VERSION,
            request_id: "abcde".into(),
            tool_name: "Bash".into(),
            description: "rm -rf /tmp/foo".into(),
            input_preview: r#"{"command":"rm -rf /tmp/foo"}"#.into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: PermissionRequestParams = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
