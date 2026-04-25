use serde::Deserialize;

#[derive(Debug, Default)]
pub struct PluginsConfig {
    /// Zero, one, or many WhatsApp accounts. Each account needs a
    /// distinct `session_dir` and (optionally) an `instance` label
    /// driving the `plugin.inbound.whatsapp.<instance>` topic.
    pub whatsapp: Vec<WhatsappPluginConfig>,
    /// Zero, one, or many Telegram bot instances. Each instance has its
    /// own token and (optionally) an `instance` label that threads into
    /// the inbound topic (`plugin.inbound.telegram.<instance>`) so agent
    /// bindings can target a specific bot.
    pub telegram: Vec<TelegramPluginConfig>,
    pub email: Option<EmailPluginConfig>,
    pub browser: Option<BrowserConfig>,
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
    /// Kept for backward compatibility with the pre-Phase 6 minimal config;
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
    /// Phase 6.10 follow-up — on boot, spawn a Cloudflare Tunnel to
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
    /// Phase 17 — agents permitted to publish from this instance.
    /// Enforced by the plugin before broker dispatch as a second layer
    /// on top of the resolver's per-agent `credentials.whatsapp`
    /// binding. Empty = accept any agent that holds a valid resolver
    /// handle for this instance (back-compat).
    #[serde(default)]
    pub allow_agents: Vec<String>,
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
}

impl Default for WhatsappBehaviorConfig {
    fn default() -> Self {
        Self {
            ignore_chat_meta: true,
            ignore_from_me: true,
            ignore_groups: false,
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
    /// Phase 17 — agents permitted to publish from this bot.
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailPluginConfigFile {
    pub email: EmailPluginConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailPluginConfig {
    pub smtp: SmtpConfig,
    pub imap: Option<ImapConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
}

fn default_smtp_port() -> u16 {
    587
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapConfig {
    pub host: String,
    #[serde(default = "default_imap_port")]
    pub port: u16,
}

fn default_imap_port() -> u16 {
    993
}
