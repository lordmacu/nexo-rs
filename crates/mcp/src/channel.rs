//! Phase 80.9 — MCP channel routing gate + XML wrap helper.
//!
//! Channels turn an MCP server into an *inbound* surface. The
//! server declares the capability and emits
//! `notifications/nexo/channel` with `{content, meta?}` whenever a
//! human sends a message on the underlying platform (Slack,
//! Telegram, iMessage, …). The runtime wraps the content in
//! `<channel source="..."> ... </channel>` and forwards it as a
//! user message into the agent's conversation.
//!
//! Because a channel server is a *trusted* inbound, registration
//! is gated by a 5-step filter — see [`gate_channel_server`] and
//! [`ChannelGateOutcome`]. The pure-fn shape lets callers exercise
//! every gate from tests without spinning up an MCP transport.

use crate::client_trait::McpClient;
use crate::events::ClientEvent;
use nexo_broker::{AnyBroker, Event as BrokerEvent};
use nexo_config::types::channels::ChannelsConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Notification method emitted by channel servers.
pub const CHANNEL_NOTIFICATION_METHOD: &str = "notifications/nexo/channel";

/// Notification method servers MAY emit when they want to relay
/// permission decisions structurally instead of as free text.
/// Reserved here for the 80.9.b follow-up; the MVP gate ignores it.
pub const CHANNEL_PERMISSION_NOTIFICATION_METHOD: &str =
    "notifications/nexo/channel/permission";

/// Outbound method the runtime emits to ask a channel server to
/// surface a permission prompt (deferred to 80.9.b).
pub const CHANNEL_PERMISSION_REQUEST_METHOD: &str =
    "notifications/nexo/channel/permission_request";

/// Capability key in `experimental[...]` that signals the server is
/// a channel surface. Mirrors the namespace prefix we already use
/// for MCP extensions (`nexo/...`) so we never conflict with
/// upstream / community capabilities.
pub const CHANNEL_CAPABILITY_KEY: &str = "nexo/channel";

/// Same idea for the per-server permission-relay opt-in. Reserved
/// for 80.9.b but checked here so the gate output is forward
/// compatible.
pub const CHANNEL_PERMISSION_CAPABILITY_KEY: &str = "nexo/channel/permission";

/// XML tag name used to wrap inbound channel content for the
/// agent. Stable identifier so prompt fragments can match against
/// it without rebuilding the wrapper.
pub const CHANNEL_TAG: &str = "channel";

/// Outcome of the 5-step gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelGateOutcome {
    /// Server passed every gate — the runtime should subscribe to
    /// `notifications/nexo/channel` and forward inbound messages.
    Register,
    /// Server failed a gate. Connection stays up; the channel
    /// handler is simply not registered. Each variant is a typed
    /// reason so log lines + `setup doctor` outputs can route on it.
    Skip {
        kind: SkipKind,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    /// Gate 1 — server did not declare the capability.
    Capability,
    /// Gate 2 — `channels.enabled = false` (operator killswitch).
    Disabled,
    /// Gate 3 — server name not in this binding's
    /// `allowed_channel_servers`.
    Session,
    /// Gate 4 — runtime plugin source does not match the operator's
    /// declared `plugin_source` for this approved entry.
    Marketplace,
    /// Gate 5 — server does not appear in `channels.approved`.
    Allowlist,
}

impl SkipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipKind::Capability => "capability",
            SkipKind::Disabled => "disabled",
            SkipKind::Session => "session",
            SkipKind::Marketplace => "marketplace",
            SkipKind::Allowlist => "allowlist",
        }
    }
}

/// Inputs for the 5-step gate. Pure-fn — all state the gate cares
/// about flows in here.
#[derive(Debug, Clone)]
pub struct ChannelGateInputs<'a> {
    /// Server name as advertised by the MCP server at connect time.
    pub server_name: &'a str,
    /// Whether the server declared the `nexo/channel` capability.
    /// Caller computes this from the raw JSON
    /// (`capabilities.experimental.nexo/channel` truthy).
    pub capability_declared: bool,
    /// Plugin source metadata stamped onto the connection at
    /// register time (when the server is loaded via a plugin).
    /// `None` for in-process or operator-defined servers.
    pub plugin_source: Option<&'a str>,
    /// Operator-level channels config (`agents.channels`).
    pub cfg: &'a ChannelsConfig,
    /// Per-binding session allowlist
    /// (`InboundBinding.allowed_channel_servers`). The gate reads
    /// this slice — the binding is the trust boundary, the agent's
    /// global config is just the operator's vetted catalogue.
    pub binding_allowlist: &'a [String],
}

/// Run the 5-step gate.
pub fn gate_channel_server(input: &ChannelGateInputs<'_>) -> ChannelGateOutcome {
    // Gate 1 — capability presence.
    if !input.capability_declared {
        return ChannelGateOutcome::Skip {
            kind: SkipKind::Capability,
            reason: format!(
                "server {} did not declare {} capability",
                input.server_name, CHANNEL_CAPABILITY_KEY
            ),
        };
    }

    // Gate 2 — operator killswitch.
    if !input.cfg.enabled {
        return ChannelGateOutcome::Skip {
            kind: SkipKind::Disabled,
            reason: "channels.enabled is false (operator killswitch)".into(),
        };
    }

    // Gate 3 — per-binding session allowlist.
    if !input
        .binding_allowlist
        .iter()
        .any(|s| s == input.server_name)
    {
        return ChannelGateOutcome::Skip {
            kind: SkipKind::Session,
            reason: format!(
                "server {} is not in this binding's allowed_channel_servers",
                input.server_name
            ),
        };
    }

    // Gate 4 — plugin source verification (when an `approved` entry
    // declares one). Done before gate 5 so a `plugin_source`
    // mismatch surfaces specifically — not as a generic "not
    // approved".
    let approved = input.cfg.lookup_approved(input.server_name);
    if let Some(entry) = approved {
        if let Some(declared) = entry.plugin_source.as_deref() {
            match input.plugin_source {
                Some(actual) if actual == declared => {}
                Some(actual) => {
                    return ChannelGateOutcome::Skip {
                        kind: SkipKind::Marketplace,
                        reason: format!(
                            "plugin_source mismatch: approved declares {} but the installed source is {}",
                            declared, actual
                        ),
                    };
                }
                None => {
                    return ChannelGateOutcome::Skip {
                        kind: SkipKind::Marketplace,
                        reason: format!(
                            "approved declares plugin_source {} but the runtime did not stamp one (non-plugin server?)",
                            declared
                        ),
                    };
                }
            }
        }
    }

    // Gate 5 — operator-approved allowlist.
    if approved.is_none() {
        return ChannelGateOutcome::Skip {
            kind: SkipKind::Allowlist,
            reason: format!(
                "server {} is not in channels.approved (add it to vet the source)",
                input.server_name
            ),
        };
    }

    ChannelGateOutcome::Register
}

// ---------------------------------------------------------------
// XML wrap helper.
// ---------------------------------------------------------------

/// Meta keys flow into XML attribute *names*. A crafted key like
/// `x="" injected="y` would break out of the attribute structure,
/// so we only accept identifier-shaped keys. Stricter than the
/// XML spec on purpose.
fn is_safe_meta_key(k: &str) -> bool {
    if k.is_empty() {
        return false;
    }
    let mut chars = k.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Escape an XML attribute value. We escape the tight set
/// `& < > " '` plus control chars + the line breaks so a multi-line
/// meta value cannot break the single-line attribute syntax.
fn escape_xml_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' | '\r' | '\t' => {
                // numeric char ref keeps the value single-line and
                // prevents tab/newline-based attribute injection.
                out.push_str(&format!("&#{};", c as u32));
            }
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("&#{};", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Wrap inbound channel content in `<channel source="..."> ... </channel>`.
/// Meta key/value pairs become extra attributes on the open tag;
/// any unsafe key is silently dropped (never leaked into the
/// markup). The inner content is left untouched — this is by
/// design, the model has to be able to read what the user wrote
/// verbatim. Closing tag is on its own line so multi-line content
/// stays readable.
pub fn wrap_channel_message(
    server_name: &str,
    content: &str,
    meta: Option<&[(String, String)]>,
) -> String {
    let mut attrs = String::new();
    attrs.push_str(" source=\"");
    attrs.push_str(&escape_xml_attr(server_name));
    attrs.push('"');
    if let Some(pairs) = meta {
        for (k, v) in pairs {
            if !is_safe_meta_key(k) {
                continue;
            }
            attrs.push(' ');
            attrs.push_str(k);
            attrs.push_str("=\"");
            attrs.push_str(&escape_xml_attr(v));
            attrs.push('"');
        }
    }
    format!("<{tag}{attrs}>\n{content}\n</{tag}>", tag = CHANNEL_TAG)
}

// ---------------------------------------------------------------
// Capability detection.
// ---------------------------------------------------------------

/// Inspect a parsed `experimental` block (the raw JSON value the
/// MCP server returned at `initialize`) and decide whether the
/// channel capability is declared.
///
/// MCP's idiom is presence-based: `experimental: { "nexo/channel": {} }`
/// signals the feature, anything else (absent / `null` / `false`)
/// means "no". We accept any truthy value (object, `true`, non-empty
/// string) as "declared" so future server implementations have
/// some leeway.
pub fn has_channel_capability(experimental: Option<&Value>) -> bool {
    has_capability_key(experimental, CHANNEL_CAPABILITY_KEY)
}

/// Same shape for the permission-relay opt-in (deferred 80.9.b).
pub fn has_channel_permission_capability(experimental: Option<&Value>) -> bool {
    has_capability_key(experimental, CHANNEL_PERMISSION_CAPABILITY_KEY)
}

fn has_capability_key(experimental: Option<&Value>, key: &str) -> bool {
    let Some(map) = experimental.and_then(|v| v.as_object()) else {
        return false;
    };
    let Some(v) = map.get(key) else {
        return false;
    };
    match v {
        Value::Object(_) => true,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::Array(a) => !a.is_empty(),
        Value::Null => false,
    }
}

// ---------------------------------------------------------------
// Notification parsing.
// ---------------------------------------------------------------

/// Typed inbound event materialised from a channel notification
/// after parsing + optional content cap from `ChannelsConfig`.
/// Cheap clone — `Arc<...>` over the heaviest fields would only
/// matter at very high message volume; channels are interactive,
/// not bulk-data.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInbound {
    pub server_name: String,
    pub content: String,
    /// Sorted for stable session-key derivation. Keys that fail
    /// the safe-attribute regex are dropped here too — the same
    /// rule applies to the wrapper, so meta that survives parsing
    /// can also survive serialisation back into XML.
    pub meta: BTreeMap<String, String>,
    /// Session correlation key — see [`ChannelSessionKey::derive`].
    pub session_key: ChannelSessionKey,
}

/// Errors a malformed notification can produce. Typed so callers
/// can pattern-match — the gate's outcome is *info* (skip vs
/// register), but a parse error is *malformed input* and worth
/// surfacing in setup-doctor / structured logs.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChannelParseError {
    #[error("unexpected method: {0}")]
    UnexpectedMethod(String),
    #[error("missing or non-object params")]
    MissingParams,
    #[error("missing or non-string params.content")]
    MissingContent,
    #[error("params.meta must be an object of string→string")]
    InvalidMeta,
    #[error("server_name must be non-empty")]
    EmptyServerName,
}

/// Parse a `notifications/nexo/channel` payload. Caller passes in
/// the server name (already validated by the gate) and the raw
/// JSON-RPC method + params. Returns a typed [`ChannelInbound`].
///
/// `cfg` is only used for `truncate_content`; passing `None` keeps
/// the content unbounded (useful in tests).
pub fn parse_channel_notification(
    server_name: &str,
    method: &str,
    params: &Value,
    cfg: Option<&ChannelsConfig>,
) -> Result<ChannelInbound, ChannelParseError> {
    if server_name.is_empty() {
        return Err(ChannelParseError::EmptyServerName);
    }
    if method != CHANNEL_NOTIFICATION_METHOD {
        return Err(ChannelParseError::UnexpectedMethod(method.to_string()));
    }
    let map = params
        .as_object()
        .ok_or(ChannelParseError::MissingParams)?;
    let content = map
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or(ChannelParseError::MissingContent)?;
    let mut meta = BTreeMap::new();
    if let Some(raw_meta) = map.get("meta") {
        let obj = raw_meta
            .as_object()
            .ok_or(ChannelParseError::InvalidMeta)?;
        for (k, v) in obj {
            let s = v.as_str().ok_or(ChannelParseError::InvalidMeta)?;
            if !is_safe_meta_key(k) {
                continue;
            }
            meta.insert(k.clone(), s.to_string());
        }
    }
    let trimmed = match cfg {
        Some(c) => c.truncate_content(content),
        None => content.to_string(),
    };
    let session_key = ChannelSessionKey::derive(server_name, &meta);
    Ok(ChannelInbound {
        server_name: server_name.to_string(),
        content: trimmed,
        meta,
        session_key,
    })
}

// ---------------------------------------------------------------
// Session correlation.
// ---------------------------------------------------------------

/// Stable identifier derived from `(server_name, meta)` so that
/// inbound notifications from the same Slack thread (or Telegram
/// chat, or iMessage conversation) route to the same agent session.
/// Threading is the difference between an agent that "remembers"
/// the conversation and one that re-introduces itself every turn.
///
/// Derivation is deterministic and independent of process — the
/// dispatcher in process A and the registry in process B agree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChannelSessionKey(pub String);

/// Names of meta keys that act as the conversation id when
/// present. Order matters — we pick the first match, so common
/// platform conventions win. Anything outside this list does not
/// influence threading (an `author_name` change must NOT split
/// the session, otherwise rename storms break threading).
pub const SESSION_META_KEYS: &[&str] = &[
    "thread_ts",  // Slack threads
    "thread_id",
    "chat_id",    // Telegram, Discord
    "conversation_id",
    "room_id",    // Matrix
    "channel_id", // Discord channels
    "to",         // iMessage / SMS recipient
];

impl ChannelSessionKey {
    /// Compute the key from `server_name` and a meta map. When
    /// none of the `SESSION_META_KEYS` are present, we fall back
    /// to `server_name` alone — every inbound from the same server
    /// hits one session. Operators with a custom keying strategy
    /// can compose their own from `meta` directly.
    pub fn derive(server_name: &str, meta: &BTreeMap<String, String>) -> Self {
        for k in SESSION_META_KEYS {
            if let Some(v) = meta.get(*k) {
                if !v.is_empty() {
                    return Self(format!("{server_name}|{k}={v}"));
                }
            }
        }
        Self(server_name.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------
// Dispatcher trait + NATS subject convention.
// ---------------------------------------------------------------

/// Subject template for cross-process channel delivery. The runtime
/// publishes one message per inbound at
/// `mcp.channel.<binding_id>.<server_name>`; subscribers (the
/// agent runtime, observability, replay) match on the exact
/// subject or use a wildcard to fan in.
pub fn channel_inbox_subject(binding_id: &str, server_name: &str) -> String {
    // Periods would split into NATS tokens; replace defensively.
    let safe_binding = binding_id.replace('.', "_");
    let safe_server = server_name.replace('.', "_");
    format!("mcp.channel.{safe_binding}.{safe_server}")
}

/// Wildcard subject covering every channel inbound across
/// bindings + servers. Useful for observability + replay.
pub const CHANNEL_INBOX_WILDCARD: &str = "mcp.channel.>";

/// What a dispatcher does with a parsed [`ChannelInbound`].
#[async_trait::async_trait]
pub trait ChannelDispatcher: Send + Sync {
    /// Publish the inbound for the agent runtime to consume. The
    /// dispatcher MUST be idempotent enough that a duplicate
    /// delivery does not double-bill the user — the runtime
    /// uses [`ChannelInbound::session_key`] to key dedup.
    async fn dispatch(&self, binding_id: &str, msg: ChannelInbound) -> Result<(), DispatchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("broker publish failed: {0}")]
    Broker(String),
    #[error("dispatcher rejected: {0}")]
    Rejected(String),
}

/// Wrapper payload published on the NATS subject. Consumers
/// deserialize this; dispatcher implementations serialize it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelEnvelope {
    /// 1 — current schema. Bump when payload shape changes so
    /// older subscribers can fall through cleanly.
    pub schema: u32,
    pub binding_id: String,
    pub server_name: String,
    pub content: String,
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
    pub session_key: ChannelSessionKey,
    /// Pre-rendered XML wrapper so subscribers don't have to
    /// re-derive it. Helps observability tools display the
    /// model-facing form without depending on this crate.
    pub rendered: String,
    /// Unix ms at publish time.
    pub sent_at_ms: i64,
    /// Unique id per envelope — dedup key + replay anchor.
    pub envelope_id: Uuid,
}

impl ChannelEnvelope {
    pub fn from_inbound(binding_id: &str, msg: ChannelInbound) -> Self {
        let pairs: Vec<(String, String)> = msg
            .meta
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let rendered = wrap_channel_message(&msg.server_name, &msg.content, Some(&pairs));
        Self {
            schema: 1,
            binding_id: binding_id.to_string(),
            server_name: msg.server_name,
            content: msg.content,
            meta: msg.meta,
            session_key: msg.session_key,
            rendered,
            sent_at_ms: chrono::Utc::now().timestamp_millis(),
            envelope_id: Uuid::new_v4(),
        }
    }
}

// ---------------------------------------------------------------
// Channel registry — per-process state of active handlers.
// ---------------------------------------------------------------

/// A single registered channel — survived the gate, the runtime
/// is actively forwarding inbound from it. Held by
/// [`ChannelRegistry`] keyed by `(binding_id, server_name)`.
#[derive(Debug, Clone)]
pub struct RegisteredChannel {
    pub binding_id: String,
    pub server_name: String,
    pub plugin_source: Option<String>,
    /// Resolved outbound tool name from the operator's
    /// `ApprovedChannel.outbound_tool_name`, falling back to
    /// [`nexo_config::types::channels::DEFAULT_OUTBOUND_TOOL_NAME`].
    /// `channel_send` reads this when invoking an outbound reply
    /// against the underlying MCP server. Stored at register-time
    /// so a config reload mid-session doesn't surprise an in-flight
    /// reply.
    pub outbound_tool_name: Option<String>,
    pub permission_relay: bool,
    /// Wallclock seconds at registration. Useful in `setup doctor`
    /// to surface stale entries.
    pub registered_at_ms: i64,
}

/// Per-process registry of active channel registrations. Backed
/// by an `RwLock` so the hot path (lookup) is cheap and
/// register/unregister stay infrequent.
#[derive(Default, Debug)]
pub struct ChannelRegistry {
    inner: RwLock<BTreeMap<(String, String), RegisteredChannel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, ch: RegisteredChannel) {
        let key = (ch.binding_id.clone(), ch.server_name.clone());
        self.inner.write().await.insert(key, ch);
    }

    pub async fn unregister(&self, binding_id: &str, server_name: &str) -> bool {
        self.inner
            .write()
            .await
            .remove(&(binding_id.to_string(), server_name.to_string()))
            .is_some()
    }

    pub async fn get(
        &self,
        binding_id: &str,
        server_name: &str,
    ) -> Option<RegisteredChannel> {
        self.inner
            .read()
            .await
            .get(&(binding_id.to_string(), server_name.to_string()))
            .cloned()
    }

    pub async fn list_for_binding(&self, binding_id: &str) -> Vec<RegisteredChannel> {
        self.inner
            .read()
            .await
            .iter()
            .filter(|((b, _), _)| b == binding_id)
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub async fn list_all(&self) -> Vec<RegisteredChannel> {
        self.inner
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    pub async fn count(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Phase 80.9.f — hot-reload re-evaluation.
    ///
    /// Walk every active registration and re-run the static
    /// half of the gate against the operator's *current* config.
    /// Entries that no longer pass — killswitch flipped off,
    /// server removed from `approved`, plugin_source pinned to a
    /// new value, binding deleted — get unregistered. The cap is
    /// kept (the live MCP server still declares the capability
    /// or the entry wouldn't have registered in the first place).
    ///
    /// Caller wires this to the Phase 18 reload coordinator's
    /// post-hook so a YAML edit takes effect on the next tick
    /// without restarting the daemon. Returns a typed report so
    /// observability + setup-doctor can surface what changed.
    pub async fn reevaluate(&self, inputs: &ReevaluateInputs) -> ReevaluateReport {
        let mut report = ReevaluateReport::default();
        let snapshot: Vec<((String, String), RegisteredChannel)> = {
            let guard = self.inner.read().await;
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        for ((binding_id, server_name), reg) in snapshot {
            let outcome = match inputs.by_binding.get(&binding_id) {
                Some(b) => {
                    let allow_slice: &[String] = &b.allowed_servers;
                    let g = ChannelGateInputs {
                        server_name: &server_name,
                        capability_declared: true,
                        plugin_source: reg.plugin_source.as_deref(),
                        cfg: &b.cfg,
                        binding_allowlist: allow_slice,
                    };
                    gate_channel_server(&g)
                }
                None => ChannelGateOutcome::Skip {
                    kind: SkipKind::Session,
                    reason: format!(
                        "binding {binding_id} no longer present in config"
                    ),
                },
            };
            match outcome {
                ChannelGateOutcome::Register => {
                    report.kept.push(ReevaluateKept {
                        binding_id,
                        server_name,
                    });
                }
                ChannelGateOutcome::Skip { kind, reason } => {
                    self.unregister(&binding_id, &server_name).await;
                    report.evicted.push(ReevaluateEvicted {
                        binding_id,
                        server_name,
                        kind,
                        reason,
                    });
                }
            }
        }
        report
    }
}

/// Convenience type alias for the wired registry.
pub type SharedChannelRegistry = Arc<ChannelRegistry>;

/// Inputs for [`ChannelRegistry::reevaluate`]. Caller builds one
/// of these from the freshly-loaded YAML; missing bindings are
/// treated as deletions.
#[derive(Debug, Default)]
pub struct ReevaluateInputs {
    pub by_binding: std::collections::HashMap<String, ReevaluateBinding>,
}

/// Per-binding snapshot used during re-evaluation.
#[derive(Debug, Clone)]
pub struct ReevaluateBinding {
    pub cfg: Arc<ChannelsConfig>,
    pub allowed_servers: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReevaluateReport {
    pub kept: Vec<ReevaluateKept>,
    pub evicted: Vec<ReevaluateEvicted>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReevaluateKept {
    pub binding_id: String,
    pub server_name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReevaluateEvicted {
    pub binding_id: String,
    pub server_name: String,
    pub kind: SkipKind,
    pub reason: String,
}

// ---------------------------------------------------------------
// LLM-side introspection.
// ---------------------------------------------------------------

/// Compact summary returned by a `channel_list` agent tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSummary {
    pub server_name: String,
    pub plugin_source: Option<String>,
    pub permission_relay: bool,
}

impl From<&RegisteredChannel> for ChannelSummary {
    fn from(r: &RegisteredChannel) -> Self {
        Self {
            server_name: r.server_name.clone(),
            plugin_source: r.plugin_source.clone(),
            permission_relay: r.permission_relay,
        }
    }
}

// ---------------------------------------------------------------
// Broker-backed dispatcher. Publishes envelopes on the
// mcp.channel.<binding>.<server> topic via AnyBroker (NATS or
// in-process local). Phase 80.9.c.
// ---------------------------------------------------------------

/// `ChannelDispatcher` impl that serialises [`ChannelEnvelope`] and
/// publishes it through an [`AnyBroker`]. Subscribers (the agent
/// runtime, observability) consume from
/// `mcp.channel.<binding>.<server>`.
pub struct BrokerChannelDispatcher {
    broker: AnyBroker,
    /// Tag stamped on the broker `Event.source` so the audit
    /// log distinguishes channel-mediated inbound from other
    /// publishers on the same subject namespace.
    source_tag: String,
}

impl BrokerChannelDispatcher {
    pub fn new(broker: AnyBroker) -> Self {
        Self {
            broker,
            source_tag: "mcp.channel".into(),
        }
    }

    pub fn with_source_tag(mut self, tag: impl Into<String>) -> Self {
        self.source_tag = tag.into();
        self
    }
}

#[async_trait::async_trait]
impl ChannelDispatcher for BrokerChannelDispatcher {
    async fn dispatch(&self, binding_id: &str, msg: ChannelInbound) -> Result<(), DispatchError> {
        let server_name = msg.server_name.clone();
        let envelope = ChannelEnvelope::from_inbound(binding_id, msg);
        let topic = channel_inbox_subject(binding_id, &server_name);
        let payload = serde_json::to_value(&envelope)
            .map_err(|e| DispatchError::Rejected(format!("serialise envelope: {e}")))?;
        let event = BrokerEvent::new(topic.clone(), self.source_tag.clone(), payload);
        nexo_broker::handle::BrokerHandle::publish(&self.broker, &topic, event)
            .await
            .map_err(|e| DispatchError::Broker(e.to_string()))
    }
}

// ---------------------------------------------------------------
// Inbound loop — subscribes to a live MCP client's event stream,
// runs the gate once, and pumps `ChannelMessage` events through
// the parser + dispatcher. Phase 80.9.c.
// ---------------------------------------------------------------

/// Configuration for [`ChannelInboundLoop::spawn`]. Passed by
/// value because every field is small/cheap to clone.
#[derive(Clone)]
pub struct ChannelInboundLoopConfig {
    pub server_name: String,
    pub binding_id: String,
    pub plugin_source: Option<String>,
    pub cfg: Arc<ChannelsConfig>,
    pub binding_allowlist: Arc<Vec<String>>,
    /// Did the server's `experimental` block declare the channel
    /// capability at `initialize` time? Captured by the caller so
    /// the loop never has to re-fetch the JSON-RPC frame.
    pub capability_declared: bool,
    /// Whether the same server also declared the permission-relay
    /// capability (80.9.b reservation).
    pub permission_capability: bool,
    pub registry: SharedChannelRegistry,
    pub dispatcher: Arc<dyn ChannelDispatcher>,
}

/// Outcome returned by [`ChannelInboundLoop::spawn`] so callers
/// know whether the loop is actually running or was vetoed by the
/// gate at start-up.
#[derive(Debug)]
pub enum ChannelInboundLoopHandle {
    /// Loop is live. The handle joins when the source events
    /// channel is closed or the cancellation token fires.
    Running { join: JoinHandle<()> },
    /// Gate vetoed the loop at startup; no task is spawned, so
    /// callers don't have to manage lifetime for inactive servers.
    Skipped {
        kind: SkipKind,
        reason: String,
    },
}

/// Drives a single MCP server's channel notifications: gate at
/// startup → register → stream → parse → dispatch.
pub struct ChannelInboundLoop {
    cfg: ChannelInboundLoopConfig,
}

impl ChannelInboundLoop {
    pub fn new(cfg: ChannelInboundLoopConfig) -> Self {
        Self { cfg }
    }

    /// Convenience: spawn against a live MCP client. Subscribes to
    /// the client's event stream and consumes until the channel
    /// closes or `cancel` fires.
    pub fn spawn_against_client(
        self,
        client: &dyn McpClient,
        cancel: CancellationToken,
    ) -> ChannelInboundLoopHandle {
        let events = client.subscribe_events();
        self.spawn_with_events(events, cancel)
    }

    /// Spawn against a pre-acquired broadcast receiver. Used by
    /// tests + by callers that already have the receiver.
    pub fn spawn_with_events(
        self,
        events: broadcast::Receiver<ClientEvent>,
        cancel: CancellationToken,
    ) -> ChannelInboundLoopHandle {
        let cfg = self.cfg;
        let allow_slice: Vec<String> = (*cfg.binding_allowlist).clone();
        let inputs = ChannelGateInputs {
            server_name: &cfg.server_name,
            capability_declared: cfg.capability_declared,
            plugin_source: cfg.plugin_source.as_deref(),
            cfg: &cfg.cfg,
            binding_allowlist: &allow_slice,
        };
        match gate_channel_server(&inputs) {
            ChannelGateOutcome::Skip { kind, reason } => {
                tracing::info!(
                    server = %cfg.server_name,
                    binding = %cfg.binding_id,
                    kind = kind.as_str(),
                    reason = %reason,
                    "channel gate skipped"
                );
                return ChannelInboundLoopHandle::Skipped { kind, reason };
            }
            ChannelGateOutcome::Register => {}
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        // Snapshot the outbound-tool name from the approved entry
        // at register-time so a config reload mid-session doesn't
        // change which tool an in-flight `channel_send` reaches.
        let outbound_tool_name = cfg
            .cfg
            .lookup_approved(&cfg.server_name)
            .map(|e| e.resolved_outbound_tool_name().to_string());
        let registered = RegisteredChannel {
            binding_id: cfg.binding_id.clone(),
            server_name: cfg.server_name.clone(),
            plugin_source: cfg.plugin_source.clone(),
            outbound_tool_name,
            permission_relay: cfg.permission_capability,
            registered_at_ms: now_ms,
        };

        let registry = cfg.registry.clone();
        let dispatcher = cfg.dispatcher.clone();
        let cfg_for_task = cfg.cfg.clone();
        let server_name = cfg.server_name.clone();
        let binding_id = cfg.binding_id.clone();

        let join = tokio::spawn(async move {
            registry.register(registered).await;
            tracing::info!(
                server = %server_name,
                binding = %binding_id,
                "channel inbound loop running"
            );

            let mut events = events;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!(
                            server = %server_name,
                            binding = %binding_id,
                            "channel inbound loop cancelled"
                        );
                        registry.unregister(&binding_id, &server_name).await;
                        return;
                    }
                    ev = events.recv() => {
                        match ev {
                            Ok(ClientEvent::ChannelMessage { params }) => {
                                Self::handle_message(
                                    &server_name,
                                    &binding_id,
                                    &cfg_for_task,
                                    dispatcher.as_ref(),
                                    params,
                                ).await;
                            }
                            Ok(_) => continue,
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(
                                    server = %server_name,
                                    dropped = n,
                                    "channel events lagged — slow consumer"
                                );
                                continue;
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                tracing::info!(
                                    server = %server_name,
                                    binding = %binding_id,
                                    "channel events closed"
                                );
                                registry.unregister(&binding_id, &server_name).await;
                                return;
                            }
                        }
                    }
                }
            }
        });

        ChannelInboundLoopHandle::Running { join }
    }

    async fn handle_message(
        server_name: &str,
        binding_id: &str,
        cfg: &ChannelsConfig,
        dispatcher: &dyn ChannelDispatcher,
        params: Value,
    ) {
        match parse_channel_notification(
            server_name,
            CHANNEL_NOTIFICATION_METHOD,
            &params,
            Some(cfg),
        ) {
            Ok(inbound) => {
                if let Err(e) = dispatcher.dispatch(binding_id, inbound).await {
                    tracing::warn!(
                        server = %server_name,
                        binding = %binding_id,
                        error = %e,
                        "channel dispatch failed"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    server = %server_name,
                    binding = %binding_id,
                    error = %e,
                    "channel notification parse error"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_config::types::channels::ApprovedChannel;

    fn cfg_off() -> ChannelsConfig {
        ChannelsConfig::default()
    }

    fn cfg_on(approved: Vec<ApprovedChannel>) -> ChannelsConfig {
        ChannelsConfig {
            enabled: true,
            approved,
            ..Default::default()
        }
    }

    fn binding(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ---- gate ----

    #[test]
    fn gate1_capability_missing() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
        }]);
        let allow = binding(&["slack"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: false,
            plugin_source: None,
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        match out {
            ChannelGateOutcome::Skip { kind, .. } => assert_eq!(kind, SkipKind::Capability),
            other => panic!("expected Skip(Capability), got {other:?}"),
        }
    }

    #[test]
    fn gate2_killswitch_off() {
        let cfg = cfg_off();
        let allow = binding(&["slack"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: None,
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        match out {
            ChannelGateOutcome::Skip { kind, .. } => assert_eq!(kind, SkipKind::Disabled),
            other => panic!("expected Skip(Disabled), got {other:?}"),
        }
    }

    #[test]
    fn gate3_session_allowlist() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
        }]);
        // binding does NOT include slack
        let allow = binding(&["telegram"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: None,
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        match out {
            ChannelGateOutcome::Skip { kind, .. } => assert_eq!(kind, SkipKind::Session),
            other => panic!("expected Skip(Session), got {other:?}"),
        }
    }

    #[test]
    fn gate4_marketplace_mismatch_when_actual_differs() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "slack".into(),
            plugin_source: Some("slack@anthropic".into()),
            outbound_tool_name: None,        }]);
        let allow = binding(&["slack"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: Some("slack@evil"),
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        match out {
            ChannelGateOutcome::Skip { kind, reason } => {
                assert_eq!(kind, SkipKind::Marketplace);
                assert!(reason.contains("slack@anthropic"));
                assert!(reason.contains("slack@evil"));
            }
            other => panic!("expected Skip(Marketplace), got {other:?}"),
        }
    }

    #[test]
    fn gate4_marketplace_missing_runtime_source() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "slack".into(),
            plugin_source: Some("slack@anthropic".into()),
            outbound_tool_name: None,        }]);
        let allow = binding(&["slack"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: None, // not stamped
            cfg: &cfg,
            binding_allowlist: &allow,
        });
        match out {
            ChannelGateOutcome::Skip { kind, .. } => assert_eq!(kind, SkipKind::Marketplace),
            other => panic!("expected Skip(Marketplace), got {other:?}"),
        }
    }

    #[test]
    fn gate5_allowlist_when_not_approved() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "telegram".into(),
            plugin_source: None,
            outbound_tool_name: None,
        }]);
        let allow = binding(&["slack"]); // session lets it through, but cfg.approved doesn't
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: None,
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        match out {
            ChannelGateOutcome::Skip { kind, .. } => assert_eq!(kind, SkipKind::Allowlist),
            other => panic!("expected Skip(Allowlist), got {other:?}"),
        }
    }

    #[test]
    fn gate_passes_when_every_step_satisfied() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
        }]);
        let allow = binding(&["slack"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: None,
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        assert_eq!(out, ChannelGateOutcome::Register);
    }

    #[test]
    fn gate_passes_with_matching_plugin_source() {
        let cfg = cfg_on(vec![ApprovedChannel {
            server: "slack".into(),
            plugin_source: Some("slack@anthropic".into()),
            outbound_tool_name: None,        }]);
        let allow = binding(&["slack"]);
        let out = gate_channel_server(&ChannelGateInputs {
            server_name: "slack",
            capability_declared: true,
            plugin_source: Some("slack@anthropic"),
                        cfg: &cfg,
            binding_allowlist: &allow,
        });
        assert_eq!(out, ChannelGateOutcome::Register);
    }

    #[test]
    fn gate_skipkind_str_renders_stable_strings() {
        for k in [
            SkipKind::Capability,
            SkipKind::Disabled,
            SkipKind::Session,
            SkipKind::Marketplace,
            SkipKind::Allowlist,
        ] {
            assert!(!k.as_str().is_empty());
        }
    }

    // ---- wrap helper ----

    #[test]
    fn wrap_emits_open_close_tags_with_source() {
        let s = wrap_channel_message("slack", "hello", None);
        assert!(s.starts_with("<channel source=\"slack\">"));
        assert!(s.ends_with("</channel>"));
        assert!(s.contains("\nhello\n"));
    }

    #[test]
    fn wrap_escapes_source_attr() {
        let s = wrap_channel_message("a\"b<c>", "x", None);
        assert!(s.contains("source=\"a&quot;b&lt;c&gt;\""));
    }

    #[test]
    fn wrap_drops_unsafe_meta_keys() {
        let meta = vec![
            ("chat_id".to_string(), "C123".to_string()),
            ("x=\"injected".to_string(), "evil".to_string()),
            ("123leading_digit".to_string(), "no".to_string()),
            (String::new(), "empty_key".to_string()),
        ];
        let s = wrap_channel_message("slack", "x", Some(&meta));
        assert!(s.contains("chat_id=\"C123\""));
        assert!(!s.contains("evil"));
        assert!(!s.contains("123leading_digit"));
    }

    #[test]
    fn wrap_escapes_meta_value_special_chars() {
        let meta = vec![
            ("user".to_string(), "<alice>".to_string()),
            ("note".to_string(), "line1\nline2".to_string()),
        ];
        let s = wrap_channel_message("slack", "x", Some(&meta));
        assert!(s.contains("user=\"&lt;alice&gt;\""));
        // newline escaped as numeric reference, not literal
        assert!(s.contains("&#10;"));
        assert!(!s.contains("line1\nline2"));
    }

    #[test]
    fn wrap_preserves_inner_content_verbatim() {
        let body = "first line\n  second & <stuff>";
        let s = wrap_channel_message("slack", body, None);
        // Inner content not escaped — model needs to read it as-is.
        assert!(s.contains("first line\n  second & <stuff>"));
    }

    #[test]
    fn is_safe_meta_key_rejects_garbage() {
        assert!(is_safe_meta_key("chat_id"));
        assert!(is_safe_meta_key("_private"));
        assert!(!is_safe_meta_key(""));
        assert!(!is_safe_meta_key("123abc"));
        assert!(!is_safe_meta_key("a-b"));
        assert!(!is_safe_meta_key("a b"));
        assert!(!is_safe_meta_key("a\""));
    }

    #[test]
    fn escape_xml_attr_handles_control_chars() {
        let s = escape_xml_attr("a\u{0001}b");
        assert!(s.contains("&#1;"));
    }

    // ---- capability detection ----

    #[test]
    fn has_channel_capability_object_is_truthy() {
        let v = serde_json::json!({"nexo/channel": {}});
        assert!(has_channel_capability(Some(&v)));
    }

    #[test]
    fn has_channel_capability_bool_true() {
        let v = serde_json::json!({"nexo/channel": true});
        assert!(has_channel_capability(Some(&v)));
    }

    #[test]
    fn has_channel_capability_false_or_null_or_absent() {
        let none: Option<&serde_json::Value> = None;
        assert!(!has_channel_capability(none));

        let v = serde_json::json!({"nexo/channel": false});
        assert!(!has_channel_capability(Some(&v)));

        let v = serde_json::json!({"nexo/channel": null});
        assert!(!has_channel_capability(Some(&v)));

        let v = serde_json::json!({"other/key": {}});
        assert!(!has_channel_capability(Some(&v)));
    }

    #[test]
    fn permission_capability_orthogonal_to_main_capability() {
        let v = serde_json::json!({
            "nexo/channel": {},
            "nexo/channel/permission": {}
        });
        assert!(has_channel_capability(Some(&v)));
        assert!(has_channel_permission_capability(Some(&v)));
    }

    // ---- notification parsing ----

    #[test]
    fn parse_notification_happy_path() {
        let params = serde_json::json!({
            "content": "hi from slack",
            "meta": {"chat_id": "C123", "user": "alice"}
        });
        let m = parse_channel_notification("slack", CHANNEL_NOTIFICATION_METHOD, &params, None)
            .unwrap();
        assert_eq!(m.server_name, "slack");
        assert_eq!(m.content, "hi from slack");
        assert_eq!(m.meta.get("chat_id"), Some(&"C123".to_string()));
        assert_eq!(m.meta.get("user"), Some(&"alice".to_string()));
        assert_eq!(
            m.session_key.as_str(),
            "slack|chat_id=C123",
            "session key derived from chat_id"
        );
    }

    #[test]
    fn parse_notification_rejects_unexpected_method() {
        let params = serde_json::json!({"content": "x"});
        let err = parse_channel_notification("slack", "notifications/something", &params, None)
            .unwrap_err();
        assert!(matches!(err, ChannelParseError::UnexpectedMethod(_)));
    }

    #[test]
    fn parse_notification_rejects_missing_content() {
        let params = serde_json::json!({"meta": {}});
        let err =
            parse_channel_notification("slack", CHANNEL_NOTIFICATION_METHOD, &params, None)
                .unwrap_err();
        assert_eq!(err, ChannelParseError::MissingContent);
    }

    #[test]
    fn parse_notification_rejects_non_object_meta() {
        let params = serde_json::json!({"content": "x", "meta": "not_an_object"});
        let err =
            parse_channel_notification("slack", CHANNEL_NOTIFICATION_METHOD, &params, None)
                .unwrap_err();
        assert_eq!(err, ChannelParseError::InvalidMeta);
    }

    #[test]
    fn parse_notification_drops_unsafe_meta_keys() {
        let params = serde_json::json!({
            "content": "x",
            "meta": {"chat_id": "C", "1bad": "drop", "with-dash": "drop"}
        });
        let m = parse_channel_notification("slack", CHANNEL_NOTIFICATION_METHOD, &params, None)
            .unwrap();
        assert!(m.meta.contains_key("chat_id"));
        assert!(!m.meta.contains_key("1bad"));
        assert!(!m.meta.contains_key("with-dash"));
    }

    #[test]
    fn parse_notification_truncates_with_cfg() {
        let cfg = ChannelsConfig {
            enabled: true,
            max_content_chars: 64,
            ..Default::default()
        };
        let body: String = "y".repeat(200);
        let params = serde_json::json!({"content": body, "meta": {}});
        let m = parse_channel_notification(
            "slack",
            CHANNEL_NOTIFICATION_METHOD,
            &params,
            Some(&cfg),
        )
        .unwrap();
        assert!(m.content.ends_with('…'));
        assert_eq!(m.content.chars().count(), 65);
    }

    #[test]
    fn parse_notification_rejects_empty_server_name() {
        let params = serde_json::json!({"content": "x"});
        let err = parse_channel_notification("", CHANNEL_NOTIFICATION_METHOD, &params, None)
            .unwrap_err();
        assert_eq!(err, ChannelParseError::EmptyServerName);
    }

    // ---- session key ----

    #[test]
    fn session_key_falls_back_to_server_name() {
        let key = ChannelSessionKey::derive("slack", &BTreeMap::new());
        assert_eq!(key.as_str(), "slack");
    }

    #[test]
    fn session_key_picks_first_known_key() {
        let mut m = BTreeMap::new();
        m.insert("user".to_string(), "alice".to_string()); // not a session key
        m.insert("chat_id".to_string(), "C123".to_string());
        let key = ChannelSessionKey::derive("slack", &m);
        assert_eq!(key.as_str(), "slack|chat_id=C123");
    }

    #[test]
    fn session_key_priority_thread_ts_over_chat_id() {
        let mut m = BTreeMap::new();
        m.insert("chat_id".to_string(), "C123".to_string());
        m.insert("thread_ts".to_string(), "1700000000.000".to_string());
        let key = ChannelSessionKey::derive("slack", &m);
        // thread_ts takes priority — Slack threads must split a
        // channel into per-thread sessions.
        assert_eq!(key.as_str(), "slack|thread_ts=1700000000.000");
    }

    #[test]
    fn session_key_ignores_empty_value() {
        let mut m = BTreeMap::new();
        m.insert("chat_id".to_string(), String::new());
        let key = ChannelSessionKey::derive("slack", &m);
        assert_eq!(key.as_str(), "slack");
    }

    #[test]
    fn session_key_is_deterministic() {
        let mut m = BTreeMap::new();
        m.insert("chat_id".to_string(), "C123".to_string());
        let a = ChannelSessionKey::derive("slack", &m);
        let b = ChannelSessionKey::derive("slack", &m);
        assert_eq!(a, b);
    }

    // ---- subjects + envelope ----

    #[test]
    fn channel_inbox_subject_replaces_dots() {
        assert_eq!(
            channel_inbox_subject("wp:default", "slack"),
            "mcp.channel.wp:default.slack"
        );
        assert_eq!(
            channel_inbox_subject("foo.bar", "ext.x"),
            "mcp.channel.foo_bar.ext_x"
        );
    }

    #[test]
    fn envelope_round_trips_through_json() {
        let mut m = BTreeMap::new();
        m.insert("chat_id".to_string(), "C123".to_string());
        let inbound = ChannelInbound {
            server_name: "slack".into(),
            content: "hi".into(),
            session_key: ChannelSessionKey::derive("slack", &m),
            meta: m,
        };
        let env = ChannelEnvelope::from_inbound("wp:default", inbound);
        let s = serde_json::to_string(&env).unwrap();
        let back: ChannelEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env.binding_id, back.binding_id);
        assert_eq!(env.session_key, back.session_key);
        assert_eq!(env.envelope_id, back.envelope_id);
        assert_eq!(env.rendered, back.rendered);
    }

    #[test]
    fn envelope_renders_xml_wrap() {
        let inbound = ChannelInbound {
            server_name: "slack".into(),
            content: "ping".into(),
            meta: BTreeMap::new(),
            session_key: ChannelSessionKey("slack".into()),
        };
        let env = ChannelEnvelope::from_inbound("wp:default", inbound);
        assert!(env.rendered.starts_with("<channel source=\"slack\">"));
        assert!(env.rendered.contains("\nping\n"));
    }

    // ---- registry ----

    #[tokio::test]
    async fn registry_register_get_unregister() {
        let r = ChannelRegistry::new();
        let ch = RegisteredChannel {
            binding_id: "wp:default".into(),
            server_name: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
            permission_relay: false,
            registered_at_ms: 0,
        };
        r.register(ch.clone()).await;
        assert_eq!(r.count().await, 1);
        let got = r.get("wp:default", "slack").await.unwrap();
        assert_eq!(got.server_name, "slack");
        assert!(r.unregister("wp:default", "slack").await);
        assert_eq!(r.count().await, 0);
    }

    #[tokio::test]
    async fn registry_list_for_binding_filters() {
        let r = ChannelRegistry::new();
        for (b, s) in [
            ("wp:default", "slack"),
            ("wp:default", "telegram"),
            ("tg:bot", "slack"),
        ] {
            r.register(RegisteredChannel {
                binding_id: b.into(),
                server_name: s.into(),
                plugin_source: None,
                outbound_tool_name: None,
                permission_relay: false,
                registered_at_ms: 0,
            })
            .await;
        }
        let wp = r.list_for_binding("wp:default").await;
        assert_eq!(wp.len(), 2);
        assert!(wp.iter().any(|c| c.server_name == "slack"));
        assert!(wp.iter().any(|c| c.server_name == "telegram"));
        assert_eq!(r.list_all().await.len(), 3);
    }

    #[tokio::test]
    async fn registry_register_overwrites_same_pair() {
        let r = ChannelRegistry::new();
        r.register(RegisteredChannel {
            binding_id: "b".into(),
            server_name: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
            permission_relay: false,
            registered_at_ms: 1,
        })
        .await;
        r.register(RegisteredChannel {
            binding_id: "b".into(),
            server_name: "slack".into(),
            plugin_source: None,
            outbound_tool_name: None,
            permission_relay: true, // changed
            registered_at_ms: 2,
        })
        .await;
        let got = r.get("b", "slack").await.unwrap();
        assert!(got.permission_relay);
        assert_eq!(r.count().await, 1);
    }

    // ---- summary ----

    #[test]
    fn channel_summary_round_trips() {
        let r = RegisteredChannel {
            binding_id: "b".into(),
            server_name: "slack".into(),
            plugin_source: Some("slack@anthropic".into()),
            outbound_tool_name: None,
            permission_relay: true,
            registered_at_ms: 0,
        };
        let s: ChannelSummary = (&r).into();
        let j = serde_json::to_string(&s).unwrap();
        let back: ChannelSummary = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }

    // ---- inbound loop ----

    #[derive(Default)]
    struct CountingDispatcher {
        dispatched: tokio::sync::Mutex<Vec<ChannelEnvelope>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl ChannelDispatcher for CountingDispatcher {
        async fn dispatch(
            &self,
            binding_id: &str,
            msg: ChannelInbound,
        ) -> Result<(), DispatchError> {
            if self.fail {
                return Err(DispatchError::Rejected("forced fail".into()));
            }
            let env = ChannelEnvelope::from_inbound(binding_id, msg);
            self.dispatched.lock().await.push(env);
            Ok(())
        }
    }

    fn cfg_active(server: &str) -> ChannelsConfig {
        ChannelsConfig {
            enabled: true,
            approved: vec![ApprovedChannel {
                server: server.into(),
                plugin_source: None,
                outbound_tool_name: None,
            }],
            ..Default::default()
        }
    }

    fn build_loop_config(
        server: &str,
        binding: &str,
        cfg: ChannelsConfig,
        capability: bool,
        dispatcher: Arc<dyn ChannelDispatcher>,
        registry: SharedChannelRegistry,
    ) -> ChannelInboundLoopConfig {
        ChannelInboundLoopConfig {
            server_name: server.into(),
            binding_id: binding.into(),
            plugin_source: None,
                        cfg: Arc::new(cfg),
            binding_allowlist: Arc::new(vec![server.into()]),
            capability_declared: capability,
            permission_capability: false,
            registry,
            dispatcher,
        }
    }

    #[tokio::test]
    async fn loop_skips_when_gate_fails() {
        let dispatcher: Arc<dyn ChannelDispatcher> = Arc::new(CountingDispatcher::default());
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        // capability missing → gate1 fails
        let cfg = build_loop_config(
            "slack",
            "b",
            cfg_active("slack"),
            false, // no capability
            dispatcher,
            registry.clone(),
        );
        let (_tx, rx) = broadcast::channel::<ClientEvent>(8);
        let cancel = CancellationToken::new();
        let handle = ChannelInboundLoop::new(cfg).spawn_with_events(rx, cancel);
        match handle {
            ChannelInboundLoopHandle::Skipped { kind, .. } => {
                assert_eq!(kind, SkipKind::Capability);
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        // No registration occurred.
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn loop_registers_and_dispatches_messages() {
        let dispatcher = Arc::new(CountingDispatcher::default());
        let dispatcher_dyn: Arc<dyn ChannelDispatcher> = dispatcher.clone();
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let cfg = build_loop_config(
            "slack",
            "b",
            cfg_active("slack"),
            true,
            dispatcher_dyn,
            registry.clone(),
        );
        let (tx, rx) = broadcast::channel::<ClientEvent>(8);
        let cancel = CancellationToken::new();
        let handle = ChannelInboundLoop::new(cfg).spawn_with_events(rx, cancel.clone());
        let join = match handle {
            ChannelInboundLoopHandle::Running { join } => join,
            other => panic!("expected Running, got {other:?}"),
        };

        // Give the loop a tick to run the gate + register.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(registry.count().await, 1);

        // Send a notification.
        tx.send(ClientEvent::ChannelMessage {
            params: serde_json::json!({"content": "hi", "meta": {"chat_id": "C123"}}),
        })
        .unwrap();

        // Allow the loop to process.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let dispatched = dispatcher.dispatched.lock().await.clone();
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].server_name, "slack");
        assert_eq!(dispatched[0].content, "hi");
        assert!(dispatched[0].rendered.contains("<channel source=\"slack\""));

        // Cancel + verify clean unregister.
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), join).await;
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn loop_logs_parse_errors_without_dying() {
        let dispatcher = Arc::new(CountingDispatcher::default());
        let dispatcher_dyn: Arc<dyn ChannelDispatcher> = dispatcher.clone();
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let cfg = build_loop_config(
            "slack",
            "b",
            cfg_active("slack"),
            true,
            dispatcher_dyn,
            registry.clone(),
        );
        let (tx, rx) = broadcast::channel::<ClientEvent>(8);
        let cancel = CancellationToken::new();
        let handle = ChannelInboundLoop::new(cfg).spawn_with_events(rx, cancel.clone());
        let join = match handle {
            ChannelInboundLoopHandle::Running { join } => join,
            _ => panic!(),
        };

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // First — malformed (no content).
        tx.send(ClientEvent::ChannelMessage {
            params: serde_json::json!({"meta": {}}),
        })
        .unwrap();
        // Second — well-formed; loop must still process this after
        // the bad one.
        tx.send(ClientEvent::ChannelMessage {
            params: serde_json::json!({"content": "ping"}),
        })
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let dispatched = dispatcher.dispatched.lock().await.clone();
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].content, "ping");
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), join).await;
    }

    #[tokio::test]
    async fn loop_unregisters_when_events_close() {
        let dispatcher: Arc<dyn ChannelDispatcher> = Arc::new(CountingDispatcher::default());
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let cfg = build_loop_config(
            "slack",
            "b",
            cfg_active("slack"),
            true,
            dispatcher,
            registry.clone(),
        );
        let (tx, rx) = broadcast::channel::<ClientEvent>(8);
        let cancel = CancellationToken::new();
        let handle = ChannelInboundLoop::new(cfg).spawn_with_events(rx, cancel);
        let join = match handle {
            ChannelInboundLoopHandle::Running { join } => join,
            _ => panic!(),
        };

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(registry.count().await, 1);

        // Drop the only sender — receiver sees Closed.
        drop(tx);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), join).await;
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn loop_continues_after_dispatch_failure() {
        let dispatcher = Arc::new(CountingDispatcher {
            fail: true,
            ..CountingDispatcher::default()
        });
        let dispatcher_dyn: Arc<dyn ChannelDispatcher> = dispatcher.clone();
        let registry: SharedChannelRegistry = Arc::new(ChannelRegistry::new());
        let cfg = build_loop_config(
            "slack",
            "b",
            cfg_active("slack"),
            true,
            dispatcher_dyn,
            registry.clone(),
        );
        let (tx, rx) = broadcast::channel::<ClientEvent>(8);
        let cancel = CancellationToken::new();
        let handle = ChannelInboundLoop::new(cfg).spawn_with_events(rx, cancel.clone());
        let join = match handle {
            ChannelInboundLoopHandle::Running { join } => join,
            _ => panic!(),
        };

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        for _ in 0..3 {
            tx.send(ClientEvent::ChannelMessage {
                params: serde_json::json!({"content": "hi"}),
            })
            .unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // No envelopes captured (every dispatch failed) but the
        // loop is still alive and the registration is intact.
        let captured = dispatcher.dispatched.lock().await.len();
        assert_eq!(captured, 0);
        assert_eq!(registry.count().await, 1);
        cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), join).await;
    }

    // ---- Phase 80.9.f hot-reload re-evaluation ----

    fn reg(binding: &str, server: &str, plugin_source: Option<&str>) -> RegisteredChannel {
        RegisteredChannel {
            binding_id: binding.into(),
            server_name: server.into(),
            plugin_source: plugin_source.map(str::to_string),
            outbound_tool_name: None,
            permission_relay: false,
            registered_at_ms: 0,
        }
    }

    fn make_inputs(
        binding: &str,
        cfg: ChannelsConfig,
        allowed: &[&str],
    ) -> ReevaluateInputs {
        let mut m = std::collections::HashMap::new();
        m.insert(
            binding.to_string(),
            ReevaluateBinding {
                cfg: Arc::new(cfg),
                allowed_servers: allowed.iter().map(|s| s.to_string()).collect(),
            },
        );
        ReevaluateInputs { by_binding: m }
    }

    #[tokio::test]
    async fn reevaluate_keeps_passing_registration() {
        let r = ChannelRegistry::new();
        r.register(reg("b", "slack", None)).await;
        let cfg = ChannelsConfig {
            enabled: true,
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: None,
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        let inputs = make_inputs("b", cfg, &["slack"]);
        let report = r.reevaluate(&inputs).await;
        assert_eq!(report.kept.len(), 1);
        assert!(report.evicted.is_empty());
        assert_eq!(r.count().await, 1);
    }

    #[tokio::test]
    async fn reevaluate_evicts_when_killswitch_off() {
        let r = ChannelRegistry::new();
        r.register(reg("b", "slack", None)).await;
        let cfg = ChannelsConfig {
            enabled: false,
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: None,
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        let inputs = make_inputs("b", cfg, &["slack"]);
        let report = r.reevaluate(&inputs).await;
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].kind, SkipKind::Disabled);
        assert_eq!(r.count().await, 0);
    }

    #[tokio::test]
    async fn reevaluate_evicts_when_server_no_longer_approved() {
        let r = ChannelRegistry::new();
        r.register(reg("b", "slack", None)).await;
        let cfg = ChannelsConfig {
            enabled: true,
            approved: vec![ApprovedChannel {
                server: "telegram".into(),
                plugin_source: None,
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        let inputs = make_inputs("b", cfg, &["slack"]);
        let report = r.reevaluate(&inputs).await;
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].kind, SkipKind::Allowlist);
        assert_eq!(r.count().await, 0);
    }

    #[tokio::test]
    async fn reevaluate_evicts_when_binding_disappears() {
        let r = ChannelRegistry::new();
        r.register(reg("b", "slack", None)).await;
        // Empty inputs — binding deleted from YAML.
        let inputs = ReevaluateInputs::default();
        let report = r.reevaluate(&inputs).await;
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].kind, SkipKind::Session);
        assert!(report.evicted[0].reason.contains("no longer present"));
        assert_eq!(r.count().await, 0);
    }

    #[tokio::test]
    async fn reevaluate_evicts_on_plugin_source_mismatch() {
        let r = ChannelRegistry::new();
        r.register(reg("b", "slack", Some("slack@anthropic"))).await;
        let cfg = ChannelsConfig {
            enabled: true,
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: Some("slack@evil".into()),
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        let inputs = make_inputs("b", cfg, &["slack"]);
        let report = r.reevaluate(&inputs).await;
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].kind, SkipKind::Marketplace);
        assert_eq!(r.count().await, 0);
    }

    #[tokio::test]
    async fn reevaluate_partial_some_kept_some_evicted() {
        let r = ChannelRegistry::new();
        r.register(reg("b", "slack", None)).await;
        r.register(reg("b", "telegram", None)).await;
        // New config keeps slack, drops telegram.
        let cfg = ChannelsConfig {
            enabled: true,
            approved: vec![ApprovedChannel {
                server: "slack".into(),
                plugin_source: None,
                outbound_tool_name: None,
            }],
            ..Default::default()
        };
        let inputs = make_inputs("b", cfg, &["slack", "telegram"]);
        let report = r.reevaluate(&inputs).await;
        assert_eq!(report.kept.len(), 1);
        assert_eq!(report.kept[0].server_name, "slack");
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].server_name, "telegram");
        assert_eq!(r.count().await, 1);
    }

    // ---- broker dispatcher ----

    #[tokio::test]
    async fn broker_dispatcher_publishes_to_topic() {
        let broker = AnyBroker::local();
        let mut sub = nexo_broker::handle::BrokerHandle::subscribe(
            &broker,
            "mcp.channel.b.slack",
        )
        .await
        .expect("subscribe");
        let dispatcher = BrokerChannelDispatcher::new(broker);
        let inbound = ChannelInbound {
            server_name: "slack".into(),
            content: "hi".into(),
            meta: BTreeMap::new(),
            session_key: ChannelSessionKey("slack".into()),
        };
        dispatcher.dispatch("b", inbound).await.unwrap();

        // Pull one event off the subscriber.
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next())
            .await
            .expect("timeout")
            .expect("event");
        assert_eq!(evt.topic, "mcp.channel.b.slack");
        let env: ChannelEnvelope = serde_json::from_value(evt.payload).unwrap();
        assert_eq!(env.binding_id, "b");
        assert_eq!(env.server_name, "slack");
        assert_eq!(env.content, "hi");
        assert!(env.rendered.contains("<channel source=\"slack\""));
    }
}
