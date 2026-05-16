use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

#[derive(Debug, Default, Clone)]
pub struct PluginsConfig {
    /// Phase 93.3 — opaque per-plugin config slice keyed by
    /// plugin id (matches `manifest.plugin.id`). Populated by
    /// `load_plugins` from `<config_dir>/plugins/<id>.yaml` (flat
    /// file layout). Empty map = no plugin configs supplied.
    ///
    /// Each entry is the operator's YAML AST with the outer
    /// wrapper key stripped when it matches the filename stem.
    /// Plugins consume via `NexoPlugin::configure(value)`
    /// (Phase 93.2); typed fields below stay populated through
    /// the Phase 93.5 deprecation window.
    pub entries: BTreeMap<String, Value>,

    /// Zero, one, or many WhatsApp accounts. Each account needs a
    /// distinct `session_dir` and (optionally) an `instance` label
    /// driving the `plugin.inbound.whatsapp.<instance>` topic.
    pub whatsapp: Vec<WhatsappPluginConfig>,
    /// Zero, one, or many Telegram bot instances. Each instance has its
    /// own token and (optionally) an `instance` label that threads into
    /// the inbound topic (`plugin.inbound.telegram.<instance>`) so agent
    /// bindings can target a specific bot.
    pub telegram: Vec<TelegramPluginConfig>,
    /// 0.5.0: zero, one, or many tenants. Each tenant owns its own
    /// `accounts: Vec<EmailAccountConfig>` and isolated state.
    /// Legacy bare-map YAML normalises to a 1-element vec via the
    /// `EmailPluginShape::Single` shim.
    pub email: Vec<EmailPluginConfig>,
    pub browser: Option<BrowserConfig>,
    /// Operator-configured plugin discovery walk knobs.
    /// Loaded from `<config_dir>/plugins/discovery.yaml` (optional —
    /// missing file means defaults: empty search_paths so nothing is
    /// scanned; legacy plugins continue to register through the
    /// hardcoded boot loop).
    pub discovery: PluginDiscoveryConfig,
}

impl PluginsConfig {
    /// Phase 93.5.a — opaque accessor returning the raw YAML
    /// slice for `plugin_id`, or `None` when the operator did
    /// not declare that plugin. Daemon-native channels (whatsapp,
    /// telegram, email, browser) still expose typed fields above
    /// for legacy consumers; new plugins (slack, discord, sms)
    /// land only here.
    pub fn entries_for(&self, plugin_id: &str) -> Option<&Value> {
        self.entries.get(plugin_id)
    }

    /// Phase 93.5.a — every declared plugin id, sorted (BTreeMap).
    pub fn plugin_ids(&self) -> impl Iterator<Item = &String> + '_ {
        self.entries.keys()
    }

    /// Phase 93.5.e — extract `instance` field values from the
    /// plugin's array-shaped YAML. Returns an empty vec when the
    /// plugin is absent, single-instance (Mapping), or has no
    /// `instance` fields. Used by the daemon's per-plugin binding
    /// validator so a new array-shape plugin (slack/discord/sms)
    /// participates without daemon-side typed access.
    ///
    /// Convention: array-shape plugin entries identify themselves
    /// via an `instance: <name>` field (WhatsApp + Telegram +
    /// Email accounts follow this pattern). Plugins that don't
    /// declare a manifest `[plugin.config_schema] shape = "array"`
    /// produce no instances here.
    pub fn instances_for(&self, plugin_id: &str) -> Vec<String> {
        let Some(value) = self.entries.get(plugin_id) else {
            return Vec::new();
        };
        let Value::Sequence(seq) = value else {
            return Vec::new();
        };
        seq.iter()
            .filter_map(|entry| {
                entry
                    .as_mapping()?
                    .get(Value::String("instance".to_string()))?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .collect()
    }

    /// Phase 93.5.a — uniform presence check. Returns `true` when
    /// the operator declared this plugin AND the declaration is
    /// non-empty (single-instance object OR non-empty array).
    /// Replaces the per-channel `cfg.plugins.X.is_empty()` /
    /// `cfg.plugins.X.is_some()` idioms so a new plugin (slack/
    /// discord/sms) participates without daemon-side typed access.
    pub fn is_active(&self, plugin_id: &str) -> bool {
        let Some(value) = self.entries.get(plugin_id) else {
            return false;
        };
        match value {
            // Single-instance object (email, browser, discovery shape).
            Value::Mapping(m) => !m.is_empty(),
            // Multi-instance array (whatsapp, telegram shape).
            Value::Sequence(s) => !s.is_empty(),
            // Null / explicit empty → inactive.
            Value::Null => false,
            // Other primitives (bool, number, string) — declared,
            // counted as active so the caller can decide how to
            // interpret. Plugin manifests own validation.
            _ => true,
        }
    }
}

#[cfg(test)]
mod plugins_config_helpers_tests {
    use super::*;

    fn entries_from(yaml: &str) -> std::collections::BTreeMap<String, Value> {
        let v: Value = serde_yaml::from_str(yaml).unwrap();
        v.as_mapping()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str().unwrap().to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn is_active_returns_true_for_non_empty_array() {
        let cfg = PluginsConfig {
            entries: entries_from("whatsapp:\n  - instance: main\n"),
            ..PluginsConfig::default()
        };
        assert!(cfg.is_active("whatsapp"));
    }

    #[test]
    fn is_active_returns_false_for_empty_array() {
        let cfg = PluginsConfig {
            entries: entries_from("telegram: []\n"),
            ..PluginsConfig::default()
        };
        assert!(!cfg.is_active("telegram"));
    }

    #[test]
    fn is_active_returns_false_for_unknown_plugin() {
        let cfg = PluginsConfig::default();
        assert!(!cfg.is_active("slack"));
        assert_eq!(cfg.entries_for("slack"), None);
    }

    #[test]
    fn is_active_returns_true_for_non_empty_object() {
        let cfg = PluginsConfig {
            entries: entries_from("email:\n  enabled: true\n"),
            ..PluginsConfig::default()
        };
        assert!(cfg.is_active("email"));
    }

    #[test]
    fn instances_for_extracts_named_entries_from_array() {
        let cfg = PluginsConfig {
            entries: entries_from(
                "telegram:\n  - instance: main\n    token: t1\n  - instance: backup\n    token: t2\n",
            ),
            ..PluginsConfig::default()
        };
        assert_eq!(
            cfg.instances_for("telegram"),
            vec!["main".to_string(), "backup".to_string()]
        );
    }

    #[test]
    fn instances_for_returns_empty_when_no_instance_field() {
        let cfg = PluginsConfig {
            entries: entries_from("telegram:\n  - token: only_token_no_instance\n"),
            ..PluginsConfig::default()
        };
        assert_eq!(cfg.instances_for("telegram"), Vec::<String>::new());
    }

    #[test]
    fn instances_for_returns_empty_for_single_instance_object() {
        let cfg = PluginsConfig {
            entries: entries_from("email:\n  enabled: true\n"),
            ..PluginsConfig::default()
        };
        assert_eq!(cfg.instances_for("email"), Vec::<String>::new());
    }

    #[test]
    fn instances_for_returns_empty_for_unknown_plugin() {
        let cfg = PluginsConfig::default();
        assert_eq!(cfg.instances_for("slack"), Vec::<String>::new());
    }

    #[test]
    fn plugin_ids_lists_declared_plugins_sorted() {
        let cfg = PluginsConfig {
            entries: entries_from("whatsapp: []\nslack:\n  workspace: T1\ntelegram: []\n"),
            ..PluginsConfig::default()
        };
        let ids: Vec<&String> = cfg.plugin_ids().collect();
        assert_eq!(ids, vec!["slack", "telegram", "whatsapp"]);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDiscoveryConfigFile {
    pub discovery: PluginDiscoveryConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDiscoveryConfig {
    /// Directories scanned at boot for `nexo-plugin.toml` manifests.
    /// Each entry's immediate children are tested as plugin dirs.
    /// Supports `$NEXO_HOME` and `$HOME` env-var expansion.
    #[serde(default)]
    pub search_paths: Vec<PathBuf>,
    /// Plugin ids to skip even when a valid manifest is found.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Empty = accept all valid plugins. Non-empty = whitelist; only
    /// plugins whose id is in this list are loaded.
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// When false (default), the walker refuses to follow symlinks
    /// that escape the search-path canonical root. Set true only in
    /// trusted dev environments.
    #[serde(default)]
    pub follow_symlinks: bool,
}

// ── Browser plugin config ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConfigFile {
    pub browser: BrowserConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConfig {
    #[serde(default)]
    pub headless: bool,
    #[serde(default)]
    pub executable: String,
    /// Empty = launch new Chrome. Set to e.g. "http://127.0.0.1:9222" to attach.
    #[serde(default)]
    pub cdp_url: String,
    #[serde(default = "default_user_data_dir")]
    pub user_data_dir: String,
    #[serde(default = "default_window_width")]
    pub window_width: u32,
    #[serde(default = "default_window_height")]
    pub window_height: u32,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_command_timeout_ms")]
    pub command_timeout_ms: u64,
    /// Extra CLI flags forwarded verbatim to the spawned Chrome/Chromium
    /// process. Empty by default — nothing changes for Linux/macOS
    /// deployments. Intended use is restricted environments that need
    /// e.g. `--no-sandbox --disable-dev-shm-usage` (Termux, certain
    /// hardened containers). Ignored when `cdp_url` is set, since
    /// attaching to an existing Chrome means the operator already
    /// launched it with their own flags.
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_user_data_dir() -> String {
    "./data/browser/profile".to_string()
}
fn default_window_width() -> u32 {
    1280
}
fn default_window_height() -> u32 {
    800
}
fn default_connect_timeout_ms() -> u64 {
    10_000
}
fn default_command_timeout_ms() -> u64 {
    15_000
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappPluginConfigFile {
    pub whatsapp: WhatsappPluginShape,
}

/// YAML shape for the `whatsapp:` key. Accepts either a single map
/// (legacy single-account) or a sequence of maps (multi-account). Each
/// account needs its own `session_dir` and `instance` label; `main.rs`
/// iterates and registers one `WhatsappPlugin` per entry.
// `Single` holds a full `WhatsappPluginConfig` (>400 bytes) while
// `Many` is a thin `Vec` header. Clippy flags the variance but
// boxing `Single` here would force an allocation on every minimal
// config load, which is the common path — accepted trade-off.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum WhatsappPluginShape {
    Single(WhatsappPluginConfig),
    Many(Vec<WhatsappPluginConfig>),
}

impl WhatsappPluginShape {
    pub fn into_vec(self) -> Vec<WhatsappPluginConfig> {
        match self {
            WhatsappPluginShape::Single(c) => vec![c],
            WhatsappPluginShape::Many(v) => v,
        }
    }
}

/// Runtime configuration for `nexo-plugin-whatsapp`.
///
/// Every section ships defaults so minimal config files stay valid; the
/// plugin reads this struct and drives `wa-agent` accordingly. See
/// `docs/wa-agent-integration.md` for the ADR behind these knobs.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappPluginConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_session_dir")]
    pub session_dir: String,
    #[serde(default = "default_media_dir")]
    pub media_dir: String,
    /// Kept for backward compatibility with the early minimal config;
    /// unused by the plugin runtime — credentials live under
    /// `session_dir/.whatsapp-rs/creds.json`.
    pub credentials_file: Option<String>,
    #[serde(default)]
    pub acl: WhatsappAclConfig,
    #[serde(default)]
    pub behavior: WhatsappBehaviorConfig,
    #[serde(default)]
    pub rate_limit: WhatsappRateLimitConfig,
    #[serde(default)]
    pub bridge: WhatsappBridgeConfig,
    #[serde(default)]
    pub transcriber: WhatsappTranscriberConfig,
    #[serde(default)]
    pub daemon: WhatsappDaemonConfig,
    /// On boot, spawn a Cloudflare Tunnel to
    /// expose `/whatsapp/pair` on a public `*.trycloudflare.com` URL
    /// so operators can scan the pairing QR from a phone without VPN /
    /// SSH / port forwarding. Off by default.
    #[serde(default)]
    pub public_tunnel: WhatsappPublicTunnelConfig,
    /// Optional instance label for multi-account routing. When set,
    /// events publish to `plugin.inbound.whatsapp.<instance>` instead
    /// of the legacy `plugin.inbound.whatsapp`. Each instance needs a
    /// distinct `session_dir` (otherwise the two accounts would stomp
    /// each other's Signal keys). Empty / absent = legacy single-account.
    #[serde(default)]
    pub instance: Option<String>,
    /// Agents permitted to publish from this instance.
    /// Enforced by the plugin before broker dispatch as a second layer
    /// on top of the resolver's per-agent `credentials.whatsapp`
    /// binding. Empty = accept any agent that holds a valid resolver
    /// handle for this instance (back-compat).
    #[serde(default)]
    pub allow_agents: Vec<String>,
    /// When the chat-presence heartbeat starts on each inbound
    /// turn. One of `instant`, `thinking`, `message`, `never`
    /// (case-insensitive). Default `instant` reproduces the
    /// pre-step-15 behaviour. v1 only honours `instant` and
    /// `never`; the other two parse OK but log a warn and behave
    /// as `instant` (queued as follow-up
    /// `whatsapp-typing-mode-thinking-message-impl`). Unrecognised
    /// values warn-fall-back to `instant` instead of failing
    /// boot, so a YAML typo doesn't kill the daemon.
    #[serde(default)]
    pub typing_mode: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappPublicTunnelConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Only spin up the tunnel while pairing is still needed. When
    /// `true` (default) the tunnel is torn down automatically once the
    /// session reports Connected, so the public URL is not kept alive
    /// past its purpose. When `false` the tunnel stays up for the
    /// lifetime of the agent process.
    #[serde(default = "default_true")]
    pub only_until_paired: bool,
}

impl Default for WhatsappPublicTunnelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            only_until_paired: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappAclConfig {
    /// Bare JIDs (device suffix stripped) allowed to reach the agent.
    /// Empty list + empty env = open ACL (accept everyone).
    #[serde(default)]
    pub allow_list: Vec<String>,
    /// Name of the env var to additionally merge into the allow-list.
    /// Comma-separated JIDs. Defaults to `WA_AGENT_ALLOW`.
    #[serde(default = "default_acl_env")]
    pub from_env: String,
}

impl Default for WhatsappAclConfig {
    fn default() -> Self {
        Self {
            allow_list: Vec::new(),
            from_env: default_acl_env(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappBehaviorConfig {
    /// When true (default), honor the user's phone-side mute / archive /
    /// lock flags by silently skipping those chats.
    #[serde(default = "default_true")]
    pub ignore_chat_meta: bool,
    /// When true (default), drop messages we sent ourselves so the agent
    /// never loops on its own replies.
    #[serde(default = "default_true")]
    pub ignore_from_me: bool,
    /// When true, skip group chats entirely. Defaults to false — groups
    /// are allowed unless the chat-meta flag excludes them.
    #[serde(default)]
    pub ignore_groups: bool,
    /// Skip messages older than `N` seconds. The
    /// WhatsApp Multi-Device protocol re-delivers buffered offline
    /// messages on every reconnect (the per-device ACK isn't always
    /// honored server-side), so without this gate the agent replies
    /// to the same backlog every time the daemon restarts. `0`
    /// disables the gate (legacy behavior). Default: 60 seconds —
    /// covers typical restart cycles while keeping live messages
    /// that arrived a few seconds before reconnect.
    #[serde(default = "default_skip_backlog_age_secs")]
    pub skip_backlog_age_secs: u64,
}

fn default_skip_backlog_age_secs() -> u64 {
    60
}

impl Default for WhatsappBehaviorConfig {
    fn default() -> Self {
        Self {
            ignore_chat_meta: true,
            ignore_from_me: true,
            ignore_groups: false,
            skip_backlog_age_secs: default_skip_backlog_age_secs(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappRateLimitConfig {
    #[serde(default = "default_rate_global")]
    pub global_per_sec: f32,
    #[serde(default = "default_rate_per_jid")]
    pub per_jid_per_sec: f32,
    #[serde(default = "default_rate_burst")]
    pub burst: u32,
}

impl Default for WhatsappRateLimitConfig {
    fn default() -> Self {
        Self {
            global_per_sec: default_rate_global(),
            per_jid_per_sec: default_rate_per_jid(),
            burst: default_rate_burst(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappBridgeConfig {
    /// How long the inbound handler waits for the LLM's outbound reply
    /// before giving up on a per-message basis.
    #[serde(default = "default_response_timeout_ms")]
    pub response_timeout_ms: u64,
    /// What to do on timeout — `"noop"` sends nothing (user just sees no
    /// reply), `"apology_text"` sends `apology_text` as a `Response::Text`.
    #[serde(default = "default_on_timeout")]
    pub on_timeout: String,
    #[serde(default = "default_apology")]
    pub apology_text: String,
}

impl Default for WhatsappBridgeConfig {
    fn default() -> Self {
        Self {
            response_timeout_ms: default_response_timeout_ms(),
            on_timeout: default_on_timeout(),
            apology_text: default_apology(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappTranscriberConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Skill id to invoke for audio → text. Defaults to `whisper`.
    #[serde(default = "default_transcriber_skill")]
    pub skill: String,
    #[serde(default = "default_transcriber_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhatsappDaemonConfig {
    /// When true (default), plugin boot aborts if a `wa-agent` daemon
    /// handle (`$XDG_DATA_HOME/.whatsapp-rs/daemon.json`) is already
    /// present — running both would double-socket the same account.
    #[serde(default = "default_true")]
    pub prefer_existing: bool,
}

impl Default for WhatsappDaemonConfig {
    fn default() -> Self {
        Self {
            prefer_existing: true,
        }
    }
}

impl Default for WhatsappTranscriberConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            skill: default_transcriber_skill(),
            timeout_ms: default_transcriber_timeout_ms(),
        }
    }
}

fn default_enabled() -> bool {
    false
}
fn default_true() -> bool {
    true
}
fn default_session_dir() -> String {
    "./data/whatsapp-session".to_string()
}
fn default_media_dir() -> String {
    "./data/media/whatsapp".to_string()
}
fn default_acl_env() -> String {
    "WA_AGENT_ALLOW".to_string()
}
fn default_rate_global() -> f32 {
    2.0
}
fn default_rate_per_jid() -> f32 {
    1.0
}
fn default_rate_burst() -> u32 {
    5
}
fn default_response_timeout_ms() -> u64 {
    30_000
}
fn default_on_timeout() -> String {
    "noop".to_string()
}
fn default_apology() -> String {
    "Sorry, I took too long to reply. Please try again.".to_string()
}
fn default_transcriber_skill() -> String {
    "whisper".to_string()
}
fn default_transcriber_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramPluginConfigFile {
    pub telegram: TelegramPluginShape,
}

/// YAML shape for the `telegram:` key. Accepts either a single map
/// (legacy single-bot) or a sequence of maps (multi-bot). serde's
/// untagged enum picks whichever matches the input — no migration
/// needed for existing configs.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TelegramPluginShape {
    Single(TelegramPluginConfig),
    Many(Vec<TelegramPluginConfig>),
}

impl TelegramPluginShape {
    pub fn into_vec(self) -> Vec<TelegramPluginConfig> {
        match self {
            TelegramPluginShape::Single(c) => vec![c],
            TelegramPluginShape::Many(v) => v,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TelegramPluginConfig {
    pub token: String,
    #[serde(default)]
    pub polling: TelegramPollingConfig,
    #[serde(default)]
    pub allowlist: TelegramAllowlistConfig,
    #[serde(default)]
    pub auto_transcribe: TelegramAutoTranscribeConfig,
    /// How long the bridge waits for the agent's reply before firing
    /// a BridgeTimeout event. Agents with long tool chains (multi-step
    /// LLM + external APIs) can breach the old 30s default — bump this
    /// to cover the slowest realistic turn.
    #[serde(default = "default_bridge_timeout_ms")]
    pub bridge_timeout_ms: u64,
    /// Optional instance label for multi-bot routing. When set, events
    /// publish to `plugin.inbound.telegram.<instance>` instead of the
    /// default `plugin.inbound.telegram`. Agents can target this bot
    /// specifically via `inbound_bindings: [{plugin: telegram, instance: X}]`.
    /// Empty / absent = legacy single-bot topic.
    #[serde(default)]
    pub instance: Option<String>,
    /// Agents permitted to publish from this bot.
    /// Enforced by the plugin before broker dispatch on top of the
    /// resolver's `credentials.telegram` binding. Empty = accept any
    /// agent holding a valid resolver handle (back-compat).
    #[serde(default)]
    pub allow_agents: Vec<String>,
}

fn default_bridge_timeout_ms() -> u64 {
    120_000
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct TelegramAutoTranscribeConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Path to the whisper extension binary (stdio JSON-RPC). Default is
    /// `./extensions/openai-whisper/target/release/openai-whisper` — the
    /// layout produced by the standard workspace build.
    #[serde(default = "default_whisper_command")]
    pub command: String,
    /// Hard cap on how long to wait for a transcription before giving up
    /// and publishing the message without text.
    #[serde(default = "default_whisper_timeout")]
    pub timeout_ms: u64,
    /// Forwarded verbatim to the whisper tool call (`language`, `prompt`).
    #[serde(default)]
    pub language: Option<String>,
}

fn default_whisper_command() -> String {
    "./extensions/openai-whisper/target/release/openai-whisper".to_string()
}
fn default_whisper_timeout() -> u64 {
    60_000
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct TelegramPollingConfig {
    #[serde(default = "default_polling_enabled")]
    pub enabled: bool,
    #[serde(default = "default_polling_interval")]
    pub interval_ms: u64,
    /// Path where the poller persists its `offset` between restarts so
    /// a restart doesn't replay the last 24h of updates. Default is
    /// `$TELEGRAM_MEDIA_DIR/offset` (alongside the media cache).
    #[serde(default)]
    pub offset_path: Option<String>,
}

fn default_polling_enabled() -> bool {
    true
}
/// Long-poll timeout hint in milliseconds. Telegram's own cap is 50s;
/// we clamp to [1, 50] seconds in the plugin. 25s keeps server round-
/// trips minimal without starving the connection of keepalives.
fn default_polling_interval() -> u64 {
    25_000
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct TelegramAllowlistConfig {
    #[serde(default)]
    pub chat_ids: Vec<i64>,
}

// ── Email plugin config ───────────────────────────────────────────────────────
//
// Multi-account IMAP/SMTP. Credentials live in `nexo-auth`,
// not in this YAML — only transport endpoints + per-account behavior
// flags belong here. The `instance` label threads into
// `plugin.inbound.email.<instance>` so agent bindings can target a
// specific mailbox.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailPluginConfigFile {
    pub email: EmailPluginShape,
}

/// 0.5.0 multi-tenant shape. Operator YAML accepts either a single
/// map (legacy 0.4.x bare object, normalised to a 1-element vec)
/// or a sequence of maps (one entry per tenant).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum EmailPluginShape {
    Single(EmailPluginConfig),
    Many(Vec<EmailPluginConfig>),
}

impl EmailPluginShape {
    pub fn into_vec(self) -> Vec<EmailPluginConfig> {
        match self {
            Self::Single(c) => vec![c],
            Self::Many(v) => v,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EmailPluginConfig {
    /// 0.5.0 tenant label. `None` ⇒ legacy single-tenant
    /// (resolved to `"default"` by the boot loop). Distinct
    /// namespace from `EmailAccountConfig.instance` (per-account
    /// label inside this tenant).
    #[serde(default)]
    pub instance: Option<String>,
    /// 0.5.0 agent allowlist per tenant. Empty = accept any agent.
    #[serde(default)]
    pub allow_agents: Vec<String>,
    #[serde(default = "default_email_enabled")]
    pub enabled: bool,
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: usize,
    /// Days to keep an attachment file under
    /// `attachments_dir` after the most recent message that
    /// referenced it. `0` disables GC entirely (legacy behaviour —
    /// attachments accumulate forever). Default 90.
    #[serde(default = "default_attachment_retention_days")]
    pub attachment_retention_days: u64,
    /// Audit follow-up I — DLQ size cap. When the per-instance
    /// `<dir>/<inst>.dlq.jsonl` reaches this many lines, the
    /// oldest entries are trimmed before each new append. `0`
    /// disables the cap (legacy unbounded behaviour). Default
    /// `10_000` — large enough for a multi-week incident
    /// history, small enough to never run a host out of disk.
    #[serde(default = "default_max_dlq_lines")]
    pub max_dlq_lines: usize,
    /// Audit #3 follow-up — bounce row retention. Days to keep a
    /// row in `email_bounces` past its `last_seen`. Rows whose
    /// `instance` no longer appears in `accounts` are pruned
    /// regardless of age. `0` disables retention pruning (rows
    /// stay forever — legacy behaviour). Default `365` so a year
    /// of bounce history is preserved without the table growing
    /// unboundedly.
    #[serde(default = "default_bounce_retention_days")]
    pub bounce_retention_days: u64,
    #[serde(default = "default_attachments_dir")]
    pub attachments_dir: String,
    #[serde(default = "default_outbound_queue_dir")]
    pub outbound_queue_dir: String,
    #[serde(default = "default_poll_fallback_seconds")]
    pub poll_fallback_seconds: u64,
    #[serde(default = "default_idle_reissue_minutes")]
    pub idle_reissue_minutes: u64,
    #[serde(default = "default_spf_dkim_warn")]
    pub spf_dkim_warn: bool,
    #[serde(default)]
    pub loop_prevention: LoopPreventionCfg,
    #[serde(default)]
    pub accounts: Vec<EmailAccountConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LoopPreventionCfg {
    #[serde(default = "default_true")]
    pub auto_submitted: bool,
    #[serde(default = "default_true")]
    pub list_headers: bool,
    #[serde(default = "default_true")]
    pub self_from: bool,
    /// Drop messages with `X-Spam-Flag: YES` (set by upstream
    /// spam filters such as SpamAssassin / Rspamd).
    #[serde(default = "default_true")]
    pub spam_flag: bool,
    /// Drop messages carrying an ESP-style `Feedback-ID` header
    /// (RFC 6438 / Google FBL convention). Almost always
    /// transactional/marketing mass mail.
    #[serde(default = "default_true")]
    pub feedback_id: bool,
    /// Drop messages whose `X-Mailer` / `User-Agent` matches a
    /// known mass-mailer ESP signature (Mailchimp, SendGrid,
    /// Mailgun, Marketo, Constant Contact, Sendinblue, …).
    #[serde(default = "default_true")]
    pub esp_mailer: bool,
}

impl Default for LoopPreventionCfg {
    fn default() -> Self {
        Self {
            auto_submitted: true,
            list_headers: true,
            self_from: true,
            spam_flag: true,
            feedback_id: true,
            esp_mailer: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EmailAccountConfig {
    /// Stable instance id — threads into `plugin.inbound.email.<instance>`
    /// and is the lookup key for credentials in `nexo-auth`.
    pub instance: String,
    /// Operator-visible address (used for `From` and self-loop detection).
    pub address: String,
    #[serde(default = "default_provider")]
    pub provider: EmailProvider,
    pub imap: ImapEndpoint,
    pub smtp: SmtpEndpoint,
    #[serde(default)]
    pub folders: EmailFolders,
    #[serde(default)]
    pub filters: EmailFilters,
    /// On a brand-new cursor (first boot OR `UIDVALIDITY` change),
    /// cap the historical backfill to the most recent N UIDs.
    /// `None` (default) processes the entire inbox — the
    /// pre-existing behaviour. Set to e.g. `50` on accounts with
    /// huge inboxes so the operator's first run doesn't churn
    /// through years of newsletters before reaching today's mail.
    #[serde(default)]
    pub bootstrap_limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmailProvider {
    Gmail,
    Outlook,
    Yahoo,
    Icloud,
    Custom,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ImapEndpoint {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_imap_tls")]
    pub tls: TlsMode,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SmtpEndpoint {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_smtp_tls")]
    pub tls: TlsMode,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// No TLS at all — only safe on localhost / loopback.
    Plain,
    /// Plaintext on connect, upgrade with STARTTLS (typical SMTP 587, IMAP 143).
    Starttls,
    /// TLS handshake on connect (typical SMTP 465, IMAP 993).
    ImplicitTls,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EmailFolders {
    #[serde(default = "default_folder_inbox")]
    pub inbox: String,
    #[serde(default = "default_folder_sent")]
    pub sent: String,
    #[serde(default = "default_folder_archive")]
    pub archive: String,
}

impl Default for EmailFolders {
    fn default() -> Self {
        Self {
            inbox: default_folder_inbox(),
            sent: default_folder_sent(),
            archive: default_folder_archive(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct EmailFilters {
    /// If non-empty, only addresses matching this allowlist produce
    /// inbound `ChatMessage`s. Other senders are dropped silently.
    #[serde(default)]
    pub from_allowlist: Vec<String>,
    /// Always-drop list — short-circuit before any other check.
    #[serde(default)]
    pub from_denylist: Vec<String>,
}

fn default_email_enabled() -> bool {
    true
}
fn default_max_body_bytes() -> usize {
    32 * 1024
}
fn default_max_attachment_bytes() -> usize {
    25 * 1024 * 1024
}
fn default_attachment_retention_days() -> u64 {
    90
}
fn default_max_dlq_lines() -> usize {
    10_000
}
fn default_bounce_retention_days() -> u64 {
    365
}
fn default_attachments_dir() -> String {
    "data/email-attachments".to_string()
}
fn default_outbound_queue_dir() -> String {
    "data/email-outbound".to_string()
}
fn default_poll_fallback_seconds() -> u64 {
    60
}
fn default_idle_reissue_minutes() -> u64 {
    28
}
fn default_spf_dkim_warn() -> bool {
    true
}
fn default_provider() -> EmailProvider {
    EmailProvider::Custom
}
fn default_imap_tls() -> TlsMode {
    TlsMode::ImplicitTls
}
fn default_smtp_tls() -> TlsMode {
    TlsMode::Starttls
}
fn default_folder_inbox() -> String {
    "INBOX".to_string()
}
fn default_folder_sent() -> String {
    "Sent".to_string()
}
fn default_folder_archive() -> String {
    "Archive".to_string()
}

#[cfg(test)]
mod email_config_tests {
    use super::*;

    #[test]
    fn parses_minimal_account() {
        let yaml = r#"
email:
  accounts:
    - instance: ops
      address: ops@example.com
      imap: { host: imap.example.com, port: 993 }
      smtp: { host: smtp.example.com, port: 587 }
"#;
        let f: EmailPluginConfigFile = serde_yaml::from_str(yaml).unwrap();
        // 0.5.0: f.email is EmailPluginShape; flatten to inspect.
        let tenants = f.email.into_vec();
        assert_eq!(tenants.len(), 1);
        let cfg = &tenants[0];
        assert_eq!(cfg.accounts.len(), 1);
        let acc = &cfg.accounts[0];
        assert_eq!(acc.instance, "ops");
        assert_eq!(acc.imap.tls, TlsMode::ImplicitTls);
        assert_eq!(acc.smtp.tls, TlsMode::Starttls);
        assert!(matches!(acc.provider, EmailProvider::Custom));
        assert_eq!(cfg.max_body_bytes, 32 * 1024);
        assert_eq!(cfg.max_attachment_bytes, 25 * 1024 * 1024);
        assert!(cfg.loop_prevention.auto_submitted);
    }

    #[test]
    fn parses_full_account() {
        let yaml = r#"
email:
  enabled: true
  max_body_bytes: 65536
  attachments_dir: /tmp/atts
  outbound_queue_dir: /tmp/outq
  poll_fallback_seconds: 30
  idle_reissue_minutes: 25
  spf_dkim_warn: false
  loop_prevention: { auto_submitted: false, list_headers: true, self_from: true }
  accounts:
    - instance: gmail-ops
      address: ops@gmail.com
      provider: gmail
      imap: { host: imap.gmail.com, port: 993, tls: implicit_tls }
      smtp: { host: smtp.gmail.com, port: 587, tls: starttls }
      folders: { inbox: INBOX, sent: "[Gmail]/Sent Mail", archive: "[Gmail]/All Mail" }
      filters:
        from_allowlist: ["@team.example.com"]
        from_denylist: ["noreply@spam.example"]
"#;
        let f: EmailPluginConfigFile = serde_yaml::from_str(yaml).unwrap();
        let tenants = f.email.into_vec();
        let cfg = &tenants[0];
        assert_eq!(cfg.poll_fallback_seconds, 30);
        assert_eq!(cfg.idle_reissue_minutes, 25);
        assert!(!cfg.spf_dkim_warn);
        assert!(!cfg.loop_prevention.auto_submitted);
        let acc = &cfg.accounts[0];
        assert!(matches!(acc.provider, EmailProvider::Gmail));
        assert_eq!(acc.folders.sent, "[Gmail]/Sent Mail");
        assert_eq!(acc.filters.from_allowlist.len(), 1);
    }

    #[test]
    fn rejects_unknown_field() {
        let yaml = r#"
email:
  bogus_field: 1
  accounts: []
"#;
        let r: Result<EmailPluginConfigFile, _> = serde_yaml::from_str(yaml);
        assert!(r.is_err());
    }
}
