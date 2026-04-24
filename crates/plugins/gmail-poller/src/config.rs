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
    /// Absolute path to the JSON token file persisted by the google
    /// plugin (same file the `google_*` tools use for Ana/kate/etc.).
    /// Read-only from here — only the google plugin mutates it via the
    /// refresh flow.
    pub token_path: String,
    pub jobs: Vec<JobConfig>,
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
