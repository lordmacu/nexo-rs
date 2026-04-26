//! Channel dashboard for the interactive setup wizard.
//!
//! Replaces the previous "single link service call" step 3 of the
//! first-run wizard with a full status table + per-channel action
//! menu so an operator can:
//!
//! 1. See every configured `(channel, instance)` pair with auth
//!    status + which agent it's bound to.
//! 2. Re-authenticate without losing the agent binding.
//! 3. Reassign a channel to a different agent without re-running
//!    the auth flow.
//! 4. Remove a binding (channel stays authenticated; the agent
//!    just stops listening to it).
//! 5. Add a new channel + instance from scratch.
//!
//! All actions land in the same `agents.d/<agent>.yaml` files the
//! runtime loads at boot, so the dashboard is the source of truth
//! the wizard always agrees with.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use dialoguer::{theme::ColorfulTheme, Password};
use serde::Deserialize;

use crate::prompt;
use crate::services_imperative;
use crate::writer;
use crate::yaml_patch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    Authenticated,
    NotAuthenticated,
    Stale,
}

impl AuthState {
    fn icon(&self) -> &'static str {
        match self {
            AuthState::Authenticated => "✓",
            AuthState::NotAuthenticated => "✗",
            AuthState::Stale => "⚠",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            AuthState::Authenticated => "auth ok",
            AuthState::NotAuthenticated => "no auth",
            AuthState::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelEntry {
    pub channel: String,
    pub instance: String,
    pub auth: AuthState,
    pub bound_agents: Vec<String>,
}

pub fn detect_channels(config_dir: &Path, secrets_dir: &Path) -> Result<Vec<ChannelEntry>> {
    let mut out: Vec<ChannelEntry> = Vec::new();

    out.push(ChannelEntry {
        channel: "telegram".into(),
        instance: "default".into(),
        auth: telegram_auth_state(secrets_dir),
        bound_agents: list_agents_with_plugin(config_dir, "telegram")?,
    });

    let workspace_root = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("data/workspace");
    let mut wa_seen = false;
    if workspace_root.is_dir() {
        if let Ok(read) = std::fs::read_dir(&workspace_root) {
            for ent in read.flatten() {
                let agent = match ent.file_name().to_str().map(str::to_string) {
                    Some(s) => s,
                    None => continue,
                };
                let wa_dir = ent.path().join("whatsapp");
                if !wa_dir.is_dir() {
                    continue;
                }
                if let Ok(insts) = std::fs::read_dir(&wa_dir) {
                    for inst in insts.flatten() {
                        let instance = match inst.file_name().to_str().map(str::to_string) {
                            Some(s) => s,
                            None => continue,
                        };
                        out.push(ChannelEntry {
                            channel: "whatsapp".into(),
                            instance: format!("{agent}/{instance}"),
                            auth: whatsapp_auth_state(&inst.path()),
                            bound_agents: vec![agent.clone()],
                        });
                        wa_seen = true;
                    }
                }
            }
        }
    }
    if !wa_seen {
        out.push(ChannelEntry {
            channel: "whatsapp".into(),
            instance: "default".into(),
            auth: AuthState::NotAuthenticated,
            bound_agents: list_agents_with_plugin(config_dir, "whatsapp")?,
        });
    }

    out.push(ChannelEntry {
        channel: "email".into(),
        instance: "default".into(),
        auth: email_auth_state(secrets_dir),
        bound_agents: list_agents_with_plugin(config_dir, "email")?,
    });

    Ok(out)
}

fn telegram_auth_state(secrets_dir: &Path) -> AuthState {
    file_state(&secrets_dir.join("telegram_bot_token.txt"))
}

fn email_auth_state(secrets_dir: &Path) -> AuthState {
    file_state(&secrets_dir.join("email_password.txt"))
}

fn file_state(path: &Path) -> AuthState {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => AuthState::Authenticated,
        Ok(_) => AuthState::Stale,
        Err(_) => AuthState::NotAuthenticated,
    }
}

fn whatsapp_auth_state(session_dir: &Path) -> AuthState {
    if !session_dir.is_dir() {
        return AuthState::NotAuthenticated;
    }
    let candidates = ["session.db", "state.db", "device.json", "registration.json"];
    for c in candidates {
        if session_dir.join(c).exists() {
            return AuthState::Authenticated;
        }
    }
    if std::fs::read_dir(session_dir)
        .map(|r| r.flatten().next().is_some())
        .unwrap_or(false)
    {
        AuthState::Stale
    } else {
        AuthState::NotAuthenticated
    }
}

fn list_agents_with_plugin(config_dir: &Path, plugin: &str) -> Result<Vec<String>> {
    use serde_yaml::Value;
    let mut hits = Vec::new();
    let mut visit = |path: &Path| {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(doc) = serde_yaml::from_str::<Value>(&text) else {
            return;
        };
        let Some(seq) = doc.get("agents").and_then(Value::as_sequence) else {
            return;
        };
        for agent in seq {
            let id = agent
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default();
            let listed = agent
                .get("plugins")
                .and_then(Value::as_sequence)
                .map(|s| s.iter().any(|v| v.as_str() == Some(plugin)))
                .unwrap_or(false);
            if !id.is_empty() && listed {
                hits.push(id);
            }
        }
    };
    visit(&config_dir.join("agents.yaml"));
    let drop_in = config_dir.join("agents.d");
    if drop_in.is_dir() {
        if let Ok(read) = std::fs::read_dir(&drop_in) {
            for ent in read.flatten() {
                let p = ent.path();
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if n.ends_with(".yaml") && !n.ends_with(".example.yaml") {
                    visit(&p);
                }
            }
        }
    }
    hits.sort();
    hits.dedup();
    Ok(hits)
}

/// Single-shot link flow.
///
/// **Telegram-only por ahora** — el resto de canales caen en el flow
/// legacy `services_imperative::dispatch` (igual que antes; no toco
/// whatsapp ni email en esta iteración).
///
/// Pasos cuando el operador elige Telegram:
///   1. Pick agent (kate / cody / …).
///   2. ¿Telegram + ese agente ya está autenticado + linkeado?
///        - Sí → "¿Re-autenticar?":
///                · no → exit.
///                · sí → continuar al paso 3 sobreescribiendo.
///        - No → continuar al paso 3 directo.
///   3. Datos del bot:
///        a. Nombre del bot (instance label).
///        b. Bot token (@BotFather).
///   4. ¿Tipo del bot?
///        a. Libre — cualquier usuario puede escribir.
///        b. Asignado — solo el chat_id que se vincule en el paso 5.
///   5. Si ASIGNADO: el bot escucha hasta que el operador le escribe;
///      captura su chat_id y lo agrega a la allowlist. Si LIBRE: skip.
///   6. Exit. Un canal por invocación, sin loop, sin re-prompt.
pub fn run_link_flow(config_dir: &Path, secrets_dir: &Path) -> Result<()> {
    // 1. Pick channel.
    let kinds = ["telegram", "whatsapp", "email"];
    let labels: Vec<&str> = kinds.iter().copied().collect();
    let ch_idx = prompt::pick_from_list("¿Qué canal querés configurar?", &labels)?;
    let channel = kinds[ch_idx];

    if channel == "telegram" {
        return run_telegram_flow(config_dir, secrets_dir);
    }

    // Legacy path para whatsapp / email — sin tocar.
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    if agent_ids.is_empty() {
        anyhow::bail!("No hay agentes definidos.");
    }
    let agent = prompt::pick_agent(&agent_ids)?;
    let entries = detect_channels(config_dir, secrets_dir)?;
    let already_linked = entries.iter().any(|e| {
        e.channel == channel
            && matches!(e.auth, AuthState::Authenticated)
            && e.bound_agents.iter().any(|a| a == &agent)
    });
    if already_linked {
        println!();
        println!("✓ {channel} ya está autenticado y vinculado a `{agent}`.");
        if !prompt::yes_no("¿Re-autenticar?", false)? {
            println!("Sin cambios.");
            return Ok(());
        }
    }
    services_imperative::dispatch(channel, config_dir, secrets_dir).map(|_| ())?;
    println!();
    println!("✔ Configuración del canal terminada.");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct BotInfo {
    #[serde(default)]
    is_bot: bool,
    username: String,
    #[serde(default)]
    first_name: String,
}

#[derive(Debug, Deserialize)]
struct TgEnvelope<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMsg>,
}

#[derive(Debug, Deserialize)]
struct TgMsg {
    chat: TgChat,
    #[serde(default)]
    from: Option<TgFrom>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgFrom {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    first_name: Option<String>,
}

/// Existing telegram instance for an agent (read from agent file +
/// telegram.yaml + secret presence).
#[derive(Debug)]
struct CurrentTelegram {
    instance: String,
    has_token: bool,
}

fn detect_current_telegram(
    config_dir: &Path,
    secrets_dir: &Path,
    agent: &str,
) -> Result<Option<CurrentTelegram>> {
    let agent_file = match yaml_patch::find_agent_file(config_dir, agent)? {
        Some(p) => p,
        None => return Ok(None),
    };
    let text = std::fs::read_to_string(&agent_file).unwrap_or_default();
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).unwrap_or_default();
    let instance = doc
        .get("agents")
        .and_then(serde_yaml::Value::as_sequence)
        .and_then(|seq| {
            seq.iter()
                .find(|a| a.get("id").and_then(serde_yaml::Value::as_str) == Some(agent))
        })
        .and_then(|a| a.get("credentials"))
        .and_then(|c| c.get("telegram"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string);
    let Some(instance) = instance else {
        return Ok(None);
    };
    let secret = secrets_dir.join(format!("{instance}_telegram_token.txt"));
    let legacy = secrets_dir.join("telegram_bot_token.txt");
    let has_token = file_state(&secret) == AuthState::Authenticated
        || file_state(&legacy) == AuthState::Authenticated;
    Ok(Some(CurrentTelegram {
        instance,
        has_token,
    }))
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_underscore = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "bot".into()
    } else {
        out
    }
}

fn block_on<F>(fut: F) -> Result<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    // Always run on a dedicated thread with its own current-thread
    // runtime — works whether or not we're already inside one.
    std::thread::scope(|s| {
        let h = s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("create tokio runtime")?;
            Ok::<F::Output, anyhow::Error>(rt.block_on(fut))
        });
        h.join()
            .map_err(|_| anyhow::anyhow!("runtime thread panicked"))?
    })
}

async fn validate_token(token: &str) -> Result<BotInfo> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let url = format!("https://api.telegram.org/bot{token}/getMe");
    let resp = http.get(&url).send().await.context("getMe request")?;
    let env: TgEnvelope<BotInfo> = resp.json().await.context("getMe response parse")?;
    if !env.ok {
        bail!(
            "Telegram rechazó el token: {}",
            env.description.unwrap_or_default()
        );
    }
    let info = env
        .result
        .ok_or_else(|| anyhow::anyhow!("getMe sin result"))?;
    if !info.is_bot {
        bail!("token resuelve a un usuario humano — usá @BotFather");
    }
    Ok(info)
}

async fn listen_first_chat(token: &str, deadline: std::time::Instant) -> Result<TgMsg> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(40))
        .build()?;
    let mut offset: Option<i64> = None;
    loop {
        if std::time::Instant::now() >= deadline {
            bail!("timeout esperando mensaje");
        }
        let url = format!(
            "https://api.telegram.org/bot{token}/getUpdates?timeout=25{}",
            offset.map(|o| format!("&offset={o}")).unwrap_or_default()
        );
        let resp = match http.get(&url).send().await {
            Ok(r) => r,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let env: TgEnvelope<Vec<TgUpdate>> = match resp.json().await {
            Ok(v) => v,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        if !env.ok {
            bail!("getUpdates falló: {}", env.description.unwrap_or_default());
        }
        for up in env.result.unwrap_or_default() {
            offset = Some(up.update_id + 1);
            if let Some(msg) = up.message {
                return Ok(msg);
            }
        }
    }
}

/// Telegram single-shot link flow.
fn run_telegram_flow(config_dir: &Path, secrets_dir: &Path) -> Result<()> {
    // 1. Pick agent.
    let agents_yaml = config_dir.join("agents.yaml");
    let mut agent_ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    let drop_in = config_dir.join("agents.d");
    if drop_in.is_dir() {
        if let Ok(read) = std::fs::read_dir(&drop_in) {
            for ent in read.flatten() {
                let p = ent.path();
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if n.ends_with(".yaml") && !n.ends_with(".example.yaml") {
                    agent_ids.extend(yaml_patch::list_agent_ids(&p).unwrap_or_default());
                }
            }
        }
    }
    agent_ids.sort();
    agent_ids.dedup();
    if agent_ids.is_empty() {
        anyhow::bail!("No hay agentes definidos.");
    }
    let agent = prompt::pick_agent(&agent_ids)?;

    // 2. Existing config?
    let current = detect_current_telegram(config_dir, secrets_dir, &agent)?;
    if let Some(cur) = &current {
        println!();
        println!(
            "✓ `{agent}` ya tiene telegram (instance `{}`, token {}).",
            cur.instance,
            if cur.has_token { "presente" } else { "FALTA" }
        );
        let opts = [
            "Mantener (no tocar nada)",
            "Reemplazar el bot",
            "Desvincular",
        ];
        let idx = prompt::pick_from_list("¿Qué hacer?", &opts)?;
        match idx {
            0 => {
                println!("Sin cambios.");
                return Ok(());
            }
            2 => {
                let agent_file = yaml_patch::find_agent_file(config_dir, &agent)?
                    .ok_or_else(|| anyhow::anyhow!("agent file no encontrado"))?;
                yaml_patch::remove_plugin_from_agent(&agent_file, &agent, "telegram").ok();
                println!("✔ `{agent}` desvinculado de telegram (secret y telegram.yaml intactos).");
                return Ok(());
            }
            _ => {}
        }
    }

    // 3. Token + validar.
    let theme = ColorfulTheme::default();
    let token: String = Password::with_theme(&theme)
        .with_prompt("Bot token (@BotFather)")
        .interact()?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("Token vacío — abortando.");
    }
    println!();
    println!("Validando token con Telegram…");
    let info = match block_on(validate_token(&token))? {
        Ok(i) => i,
        Err(e) => {
            println!("✗ {e}");
            anyhow::bail!("token inválido");
        }
    };
    let instance = slugify(&info.username);
    println!(
        "✔ Bot @{} ({}) — instance `{instance}`",
        info.username, info.first_name
    );

    // 4. Modo.
    let modes = [
        "Público — cualquiera puede escribir al bot",
        "Privado — sólo vos (capturo tu chat_id ahora)",
    ];
    let mode_idx = prompt::pick_from_list("¿Modo del bot?", &modes)?;
    let mut chat_ids: Vec<i64> = Vec::new();
    if mode_idx == 1 {
        println!();
        println!("Escuchando — escribile al bot ahora desde Telegram:");
        println!("    https://t.me/{}", info.username);
        println!();
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        let msg = match block_on(listen_first_chat(&token, deadline))? {
            Ok(m) => m,
            Err(e) => anyhow::bail!("no llegó mensaje: {e}"),
        };
        chat_ids.push(msg.chat.id);
        let who = msg
            .from
            .as_ref()
            .and_then(|f| f.username.clone().or(f.first_name.clone()))
            .unwrap_or_else(|| "?".into());
        let chat_name = msg
            .chat
            .title
            .clone()
            .or_else(|| msg.chat.first_name.clone())
            .unwrap_or_else(|| "?".into());
        println!("✔ chat_id {} capturado ({who} · {chat_name})", msg.chat.id);
        if let Some(t) = &msg.text {
            println!("  texto: {t}");
        }
    }

    // 5. Persistir secret + telegram.yaml + agente.
    writer::ensure_secrets_dir(secrets_dir)?;
    let secret_file = format!("{instance}_telegram_token.txt");
    writer::write_secret(secrets_dir, &secret_file, &token)?;
    let placeholder = format!("${{file:./secrets/{secret_file}}}");

    let telegram_yaml = config_dir.join("plugins").join("telegram.yaml");
    yaml_patch::telegram_upsert_instance(
        &telegram_yaml,
        &instance,
        &placeholder,
        &[agent.clone()],
        &chat_ids,
    )?;

    let agent_file = yaml_patch::find_agent_file(config_dir, &agent)?
        .ok_or_else(|| anyhow::anyhow!("agent file no encontrado"))?;
    yaml_patch::add_plugin_to_agent(&agent_file, &agent, "telegram").ok();
    yaml_patch::patch_agent_credentials(&agent_file, &agent, "telegram", &instance)?;
    yaml_patch::upsert_agent_inbound_binding(&agent_file, &agent, "telegram", &instance)?;
    // Seed pairing_allow_from for every captured chat_id so operators
    // disabling the YAML allowlist don't see redundant challenges.
    if !chat_ids.is_empty() {
        seed_pairing_allowlist_for_dashboard(config_dir, &instance, &chat_ids);
    }

    // 6. Resumen.
    println!();
    println!(
        "✔ @{} → `{agent}` (modo: {}, chat_ids: {})",
        info.username,
        if mode_idx == 0 { "público" } else { "privado" },
        chat_ids.len()
    );
    Ok(())
}

/// Seed every captured chat_id into `pairing_allow_from` for the
/// telegram instance the dashboard just persisted. Mirrors the
/// helper in `telegram_link.rs` but accepts a vector of chat ids
/// (the dashboard captures only one today, but the schema supports
/// multiple). Best-effort: we never abort the wizard on a pairing-db
/// error since the YAML allowlist already admits these chats.
fn seed_pairing_allowlist_for_dashboard(config_dir: &Path, instance: &str, chat_ids: &[i64]) {
    let db_path = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("data")
        .join("pairing.db");
    let Some(db_str) = db_path.to_str().map(str::to_string) else {
        tracing::warn!(path = %db_path.display(), "pairing.db path is not utf-8 — skipping seed");
        return;
    };
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let senders: Vec<String> = chat_ids.iter().map(|n| n.to_string()).collect();
    let instance_owned = instance.to_string();
    let outcome = block_on(async move {
        let store = nexo_pairing::PairingStore::open(&db_str).await?;
        store
            .seed("telegram", &instance_owned, &senders)
            .await
            .map_err(anyhow::Error::from)
    });
    match outcome {
        Ok(Ok(rows)) => {
            println!("  ✔ pairing_allow_from sembrado ({rows} fila(s) para `{instance}`).")
        }
        Ok(Err(e)) => println!("  ⚠ pairing seed falló ({e}); allowlist YAML cubre."),
        Err(e) => println!("  ⚠ pairing seed runtime falló ({e}); allowlist YAML cubre."),
    }
}

/// Legacy dashboard mode (per-row action menu). Kept for a future
/// `nexo setup channels` subcommand; the wizard + hub menu both use
/// `run_link_flow` now.
#[allow(dead_code)]
pub fn run_dashboard(config_dir: &Path, secrets_dir: &Path) -> Result<()> {
    loop {
        let entries = detect_channels(config_dir, secrets_dir)?;
        let labels = render_labels(&entries);
        let mut menu: Vec<String> = labels;
        menu.push("+ Agregar canal nuevo".into());
        menu.push("Continuar al siguiente paso".into());

        println!();
        println!("─── Canales ────────────────────────────────────────");
        let pick = prompt::pick_from_strings("¿Qué canal querés tocar?", &menu)?;
        if pick == menu.len() - 1 {
            return Ok(());
        }
        if pick == menu.len() - 2 {
            run_add_new_channel(config_dir, secrets_dir)?;
            continue;
        }
        let entry = entries[pick].clone();
        run_action_menu(config_dir, secrets_dir, &entry)?;
    }
}

fn render_labels(entries: &[ChannelEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let bind = if e.bound_agents.is_empty() {
                "—".to_string()
            } else {
                e.bound_agents.join(", ")
            };
            format!(
                "{} {:<10}/{:<28} ({}) → {}",
                e.auth.icon(),
                e.channel,
                e.instance,
                e.auth.label(),
                bind,
            )
        })
        .collect()
}

fn run_action_menu(config_dir: &Path, secrets_dir: &Path, entry: &ChannelEntry) -> Result<()> {
    println!();
    println!(
        "── {} / {} ─────────────────",
        entry.channel, entry.instance
    );
    println!(
        "Estado     : {} ({})",
        entry.auth.icon(),
        entry.auth.label()
    );
    let bind_label = if entry.bound_agents.is_empty() {
        "—".to_string()
    } else {
        entry.bound_agents.join(", ")
    };
    println!("Bound a    : {}", bind_label);
    println!();
    let actions = vec![
        "Re-autenticar (regenera token / QR)",
        "Reasignar a otro agente",
        "Remover este binding (canal no se borra)",
        "Volver",
    ];
    let idx = prompt::pick_from_list("Acción", &actions)?;
    match idx {
        0 => run_reauth(config_dir, secrets_dir, entry),
        1 => run_reassign(config_dir, entry),
        2 => run_remove_binding(config_dir, entry),
        _ => Ok(()),
    }
}

fn run_reauth(config_dir: &Path, secrets_dir: &Path, entry: &ChannelEntry) -> Result<()> {
    let svc_id = match entry.channel.as_str() {
        "telegram" => "telegram",
        "whatsapp" => "whatsapp",
        "email" => "email",
        _ => {
            println!("Reauth no soportado para canal {}.", entry.channel);
            return Ok(());
        }
    };
    services_imperative::dispatch(svc_id, config_dir, secrets_dir).map(|_| ())
}

fn run_reassign(config_dir: &Path, entry: &ChannelEntry) -> Result<()> {
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    if agent_ids.is_empty() {
        println!("No hay agentes para asignar.");
        return Ok(());
    }
    let new_owner = prompt::pick_agent(&agent_ids)?;
    for old in &entry.bound_agents {
        if old != &new_owner {
            if let Some(file) = locate_agent_file(config_dir, old) {
                yaml_patch::remove_plugin_from_agent(&file, old, &entry.channel).ok();
            }
        }
    }
    if let Some(file) = locate_agent_file(config_dir, &new_owner) {
        yaml_patch::add_plugin_to_agent(&file, &new_owner, &entry.channel).ok();
    } else {
        println!("⚠  No encontré el archivo YAML del agente {new_owner}.");
    }
    println!(
        "✔ {} ahora escucha {}/{}",
        new_owner, entry.channel, entry.instance
    );
    Ok(())
}

fn run_remove_binding(config_dir: &Path, entry: &ChannelEntry) -> Result<()> {
    if entry.bound_agents.is_empty() {
        println!("Ya no había binding.");
        return Ok(());
    }
    if !prompt::yes_no(
        &format!(
            "¿Quitar {} de {}? (canal sigue auth-ado)",
            entry.channel,
            entry.bound_agents.join(", ")
        ),
        false,
    )? {
        return Ok(());
    }
    for agent in &entry.bound_agents {
        if let Some(file) = locate_agent_file(config_dir, agent) {
            yaml_patch::remove_plugin_from_agent(&file, agent, &entry.channel).ok();
        }
    }
    println!("✔ binding removido.");
    Ok(())
}

fn run_add_new_channel(config_dir: &Path, secrets_dir: &Path) -> Result<()> {
    let kinds = ["telegram", "whatsapp", "email"];
    let labels: Vec<&str> = kinds.iter().copied().collect();
    let idx = prompt::pick_from_list("¿Qué canal agregar?", &labels)?;
    services_imperative::dispatch(kinds[idx], config_dir, secrets_dir).map(|_| ())
}

fn locate_agent_file(config_dir: &Path, agent_id: &str) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = vec![config_dir.join("agents.yaml")];
    let drop_in = config_dir.join("agents.d");
    if let Ok(read) = std::fs::read_dir(&drop_in) {
        for ent in read.flatten() {
            let p = ent.path();
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if n.ends_with(".yaml") && !n.ends_with(".example.yaml") {
                candidates.push(p);
            }
        }
    }
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let Some(seq) = doc.get("agents").and_then(serde_yaml::Value::as_sequence) else {
            continue;
        };
        if seq
            .iter()
            .any(|a| a.get("id").and_then(serde_yaml::Value::as_str) == Some(agent_id))
        {
            return Some(path);
        }
    }
    None
}
