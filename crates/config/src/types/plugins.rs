use serde::Deserialize;

#[derive(Debug, Default)]
pub struct PluginsConfig {
    pub whatsapp: Option<WhatsappPluginConfig>,
    pub telegram: Option<TelegramPluginConfig>,
    pub email: Option<EmailPluginConfig>,
}

#[derive(Debug, Deserialize)]
pub struct WhatsappPluginConfigFile {
    pub whatsapp: WhatsappPluginConfig,
}

#[derive(Debug, Deserialize)]
pub struct WhatsappPluginConfig {
    #[serde(default = "default_session_dir")]
    pub session_dir: String,
    pub credentials_file: Option<String>,
}

fn default_session_dir() -> String { "./data/sessions".to_string() }

#[derive(Debug, Deserialize)]
pub struct TelegramPluginConfigFile {
    pub telegram: TelegramPluginConfig,
}

#[derive(Debug, Deserialize)]
pub struct TelegramPluginConfig {
    pub token: String,
    #[serde(default)]
    pub polling: TelegramPollingConfig,
    #[serde(default)]
    pub allowlist: TelegramAllowlistConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct TelegramPollingConfig {
    #[serde(default = "default_polling_enabled")]
    pub enabled: bool,
    #[serde(default = "default_polling_interval")]
    pub interval_ms: u64,
}

fn default_polling_enabled() -> bool { true }
fn default_polling_interval() -> u64 { 1000 }

#[derive(Debug, Deserialize, Default)]
pub struct TelegramAllowlistConfig {
    #[serde(default)]
    pub chat_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
pub struct EmailPluginConfigFile {
    pub email: EmailPluginConfig,
}

#[derive(Debug, Deserialize)]
pub struct EmailPluginConfig {
    pub smtp: SmtpConfig,
    pub imap: Option<ImapConfig>,
}

#[derive(Debug, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
}

fn default_smtp_port() -> u16 { 587 }

#[derive(Debug, Deserialize)]
pub struct ImapConfig {
    pub host: String,
    #[serde(default = "default_imap_port")]
    pub port: u16,
}

fn default_imap_port() -> u16 { 993 }
