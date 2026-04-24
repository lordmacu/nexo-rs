//! Telegram linking helper for the setup wizard.
//!
//! Telegram bots don't have a QR pairing flow — the "link" is really
//! two facts:
//!
//! 1. A valid bot token obtained from `@BotFather` (we persist it in
//!    `secrets/telegram_bot_token.txt`).
//! 2. An allowlist of chat IDs the bot is willing to talk to. Chat
//!    IDs are only discoverable after the user sends the bot its
//!    first message, so we long-poll `getUpdates` and capture the
//!    first `/start` (or any message) that hits.
//!
//! Flow this module implements:
//!
//! * Validate the bot token via `getMe` → get the bot's username so
//!   we can print a clickable `https://t.me/<username>` link.
//! * Poll `getUpdates` with a 30s long-poll until a message arrives
//!   or the user bails with Ctrl+C / timeout.
//! * Append the sender's chat_id to
//!   `config/plugins/telegram.yaml::telegram.allowlist.chat_ids`.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::writer;
use crate::yaml_patch::{self, ValueKind};

const LINK_TIMEOUT: Duration = Duration::from_secs(120);
const LONG_POLL_SECS: u64 = 25;

#[derive(Debug, Deserialize)]
struct TgResp<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BotMe {
    id: i64,
    is_bot: bool,
    first_name: String,
    username: String,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    #[serde(default)]
    text: Option<String>,
    chat: TgChat,
    #[serde(default)]
    from: Option<TgFrom>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct TgFrom {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

/// Entry point. Reads the bot token from `secrets/` (or prompts for
/// one if missing), validates it, and waits for the first user
/// message to capture their chat_id.
pub async fn run(secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    let token_path = secrets_dir.join("telegram_bot_token.txt");
    if !token_path.exists() {
        bail!(
            "bot token missing — corre primero `agent setup telegram` para pegar el token"
        );
    }
    let token = std::fs::read_to_string(&token_path)
        .with_context(|| format!("read {}", token_path.display()))?
        .trim()
        .to_string();
    if token.is_empty() {
        bail!("bot token file is empty");
    }

    println!();
    println!("── Telegram link ───────────────────────────────────────────");

    // Step 1: validate token.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(LONG_POLL_SECS + 10))
        .build()?;
    let me = get_me(&http, &token).await.context("validating bot token")?;
    if !me.is_bot {
        bail!("token resolves to a non-bot user — regenera con @BotFather");
    }
    println!();
    println!("  ✔ Token válido");
    println!("     Bot         : @{} ({})", me.username, me.first_name);
    println!("     Bot ID      : {}", me.id);
    println!();
    println!("  Abre este link en tu Telegram y escribe algo al bot:");
    println!();
    println!("     https://t.me/{}", me.username);
    println!();

    let pb = ProgressBar::new_spinner();
    pb.enable_steady_tick(Duration::from_millis(100));
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} esperando primer mensaje…")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );

    // Step 2: long-poll getUpdates until we see a message.
    let deadline = std::time::Instant::now() + LINK_TIMEOUT;
    let mut offset: Option<i64> = None;
    loop {
        if std::time::Instant::now() >= deadline {
            pb.finish_and_clear();
            bail!("timed out after {}s waiting for a message", LINK_TIMEOUT.as_secs());
        }
        let updates = get_updates(&http, &token, offset).await;
        match updates {
            Ok(list) => {
                for up in &list {
                    offset = Some(up.update_id + 1);
                    if let Some(msg) = &up.message {
                        pb.finish_and_clear();
                        let chat_id = msg.chat.id;
                        let who = msg
                            .from
                            .as_ref()
                            .and_then(|f| f.username.clone().or(f.first_name.clone()))
                            .unwrap_or_else(|| "(desconocido)".into());
                        let chat_name = msg
                            .chat
                            .title
                            .clone()
                            .or_else(|| msg.chat.first_name.clone())
                            .unwrap_or_else(|| "(sin nombre)".into());
                        println!(
                            "  ✔ Mensaje recibido de {who} · chat `{}` ({}) · id {chat_id}",
                            chat_name, msg.chat.kind
                        );
                        if let Some(text) = &msg.text {
                            println!("     Texto       : {text}");
                        }
                        append_chat_id_to_allowlist(config_dir, chat_id)?;
                        println!();
                        println!(
                            "  ✔ Agregado a telegram.allowlist.chat_ids en {}",
                            config_dir.join("plugins/telegram.yaml").display()
                        );
                        println!();
                        println!("  Ya puedes iniciar el agente; el bot solo responderá a ese chat.");
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "getUpdates failed, retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn get_me(http: &reqwest::Client, token: &str) -> Result<BotMe> {
    let url = format!("https://api.telegram.org/bot{token}/getMe");
    let resp = http.get(&url).send().await?;
    let raw: TgResp<BotMe> = resp.json().await?;
    if !raw.ok {
        bail!("getMe failed: {}", raw.description.unwrap_or_default());
    }
    raw.result
        .ok_or_else(|| anyhow::anyhow!("getMe returned ok with no result"))
}

async fn get_updates(
    http: &reqwest::Client,
    token: &str,
    offset: Option<i64>,
) -> Result<Vec<Update>> {
    let url = format!(
        "https://api.telegram.org/bot{token}/getUpdates?timeout={}{}",
        LONG_POLL_SECS,
        offset.map(|o| format!("&offset={o}")).unwrap_or_default(),
    );
    let resp = http.get(&url).send().await?;
    let raw: TgResp<Vec<Update>> = resp.json().await?;
    if !raw.ok {
        bail!("getUpdates failed: {}", raw.description.unwrap_or_default());
    }
    Ok(raw.result.unwrap_or_default())
}

/// Append a chat_id to the YAML allowlist, preserving existing entries
/// and avoiding duplicates.
fn append_chat_id_to_allowlist(config_dir: &Path, chat_id: i64) -> Result<()> {
    let yaml_path = config_dir.join("plugins/telegram.yaml");
    let mut current: Vec<i64> = read_current_allowlist(&yaml_path);
    if !current.contains(&chat_id) {
        current.push(chat_id);
    }
    let joined = current
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    yaml_patch::upsert(
        &yaml_path,
        "telegram.allowlist.chat_ids",
        &joined,
        ValueKind::IntList,
    )?;
    // The wizard's `write_secret` helper enforces 0700 on the parent;
    // YAML file itself keeps default perms (read-only config).
    let _ = writer::ensure_secrets_dir; // silence unused warning when calling-side optimises away
    Ok(())
}

fn read_current_allowlist(path: &Path) -> Vec<i64> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    v.get("telegram")
        .and_then(|t| t.get("allowlist"))
        .and_then(|a| a.get("chat_ids"))
        .and_then(|x| x.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|item| item.as_i64())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}
