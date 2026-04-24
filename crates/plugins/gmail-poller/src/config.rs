//! YAML shape for `config/plugins/gmail-poller.yaml`.
//!
//! The plugin supports a list of independent jobs — typically one per
//! email pattern you want to route. Each job has its own query, regex
//! set, destination channel, and template. Adding a new route is a
//! pure config edit, never code.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct GmailPollerFile {
    #[serde(rename = "gmail_poller")]
    pub gmail_poller: GmailPollerConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GmailPollerConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Shared default interval for jobs that don't override it.
    #[serde(default = "default_interval")]
    pub interval_secs: u64,

    /// Back-compat: single-account shorthand. When `accounts` is
    /// empty we synthesize `{id: "default", token_path}` from this.
    /// New deployments should use the `accounts` list explicitly.
    #[serde(default)]
    pub token_path: Option<String>,
    /// Back-compat counterparts for the default account's OAuth app
    /// credentials. Ignored when `accounts` is non-empty.
    #[serde(default)]
    pub client_id_path: Option<String>,
    #[serde(default)]
    pub client_secret_path: Option<String>,

    /// Per-agent account list. Each entry names an OAuth app (via
    /// the two credential files) plus a token_path written by `setup
    /// google-auth` for that agent. Jobs pick one by id.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,

    pub jobs: Vec<JobConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountConfig {
    pub id: String,
    pub token_path: String,
    pub client_id_path: String,
    pub client_secret_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobConfig {
    pub name: String,
    /// Gmail search query, same syntax as the Gmail UI search box.
    /// `is:unread` is the conventional dedup — the plugin marks
    /// matched messages as read after dispatch.
    pub query: String,
    /// Override the root-level interval for noisy vs quiet routes.
    #[serde(default)]
    pub interval_secs: Option<u64>,
    /// Destination channel. `forward_to_subject` is the broker topic
    /// we publish on — e.g. `plugin.outbound.whatsapp.default`. The
    /// recipient goes in the payload via `to`.
    pub forward_to_subject: String,
    /// JID / chat_id / phone — plugin-specific format. We pass it
    /// through untouched.
    pub forward_to: String,
    /// Named regexes applied to the email body (first capture group).
    /// Field names become placeholders in `message_template`.
    #[serde(default)]
    pub extract: HashMap<String, String>,
    /// Template string. `{field_name}` is substituted with the
    /// capture. `{snippet}` always resolves to the raw Gmail snippet.
    /// `{subject}` to the email subject.
    pub message_template: String,
    /// POST `removeLabelIds: [UNREAD]` after successful dispatch so
    /// the next tick's `is:unread` query doesn't re-send. Default on.
    #[serde(default = "default_mark_read")]
    pub mark_read_on_dispatch: bool,

    /// Skip dispatch when any of these extracted fields is empty.
    /// Prevents forwarding malformed emails where key data didn't
    /// match the regexes. Empty list = always dispatch.
    #[serde(default)]
    pub require_fields: Vec<String>,

    /// Gmail `newer_than:` suffix appended to every query. Use on
    /// first deploy to avoid back-filling years of old matches. E.g.
    /// `"1d"` (24h) or `"2h"`. Empty = no bound (scan full inbox).
    #[serde(default)]
    pub newer_than: Option<String>,

    /// Seconds to sleep between dispatches when a single tick finds
    /// multiple matches. Protects downstream channels (WhatsApp rate
    /// limits) from burst sends. Default 1s.
    #[serde(default = "default_dispatch_delay")]
    pub dispatch_delay_ms: u64,

    /// Hard cap per tick. If Gmail returns more than this, only the
    /// first N are processed; the rest wait for the next tick. Keeps
    /// one spike from monopolizing the worker.
    #[serde(default = "default_max_per_tick")]
    pub max_per_tick: usize,

    /// Optional: match sender against allowlist before dispatching.
    /// Each entry is a substring or `@domain.com`. Empty = accept all.
    #[serde(default)]
    pub sender_allowlist: Vec<String>,

    /// Which account (from the root `accounts:` list) to poll. Defaults
    /// to `"default"` for single-account back-compat.
    #[serde(default = "default_account")]
    pub account: String,
}
fn default_account() -> String {
    "default".to_string()
}

fn default_enabled() -> bool {
    true
}
fn default_interval() -> u64 {
    60
}
fn default_mark_read() -> bool {
    true
}
fn default_dispatch_delay() -> u64 {
    1000
}
fn default_max_per_tick() -> usize {
    20
}

impl GmailPollerConfig {
    /// Load `config/plugins/gmail-poller.yaml`. Returns `None` when
    /// the file is absent so startup stays quiet for installs that
    /// don't need the poller.
    pub fn load(config_dir: &std::path::Path) -> anyhow::Result<Option<Self>> {
        let path = config_dir.join("plugins").join("gmail-poller.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        let file: GmailPollerFile = serde_yaml::from_str(&text)?;
        Ok(Some(file.gmail_poller))
    }
}
