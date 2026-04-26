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

use anyhow::Result;

use crate::prompt;
use crate::services_imperative;
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

/// Linear flow: pick channel → pick agent → detect link state → if
/// linked ask "re-authenticate?", if not run the link flow + add to
/// the agent's `plugins:` array. Loops until "Continuar".
///
/// Used by the first-run wizard step 3 and the hub menu's
/// "Vincular canal adicional" option so both paths share the same
/// UX.
pub fn run_link_flow(config_dir: &Path, secrets_dir: &Path) -> Result<()> {
    loop {
        let kinds = ["telegram", "whatsapp", "email"];
        let mut channel_menu: Vec<String> =
            kinds.iter().map(|k| (*k).to_string()).collect();
        channel_menu.push("Continuar al siguiente paso".into());
        let ch_idx = prompt::pick_from_strings("¿Qué canal?", &channel_menu)?;
        if ch_idx == channel_menu.len() - 1 {
            return Ok(());
        }
        let channel = kinds[ch_idx];

        // Pick agent BEFORE auth — "está vinculado?" is per-(channel, agent).
        let agents_yaml = config_dir.join("agents.yaml");
        let agent_ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
        if agent_ids.is_empty() {
            anyhow::bail!(
                "No hay agentes definidos. Editá `agents.yaml` o `agents.d/*.yaml` y re-correlo."
            );
        }
        let agent = prompt::pick_agent(&agent_ids)?;

        // Detect link state for THIS (channel, agent) pair.
        let entries = detect_channels(config_dir, secrets_dir)?;
        let already_linked = entries.iter().any(|e| {
            e.channel == channel
                && matches!(e.auth, AuthState::Authenticated)
                && e.bound_agents.iter().any(|a| a == &agent)
        });

        if already_linked {
            println!();
            println!("✓ {} ya está autenticado y vinculado a `{}`.", channel, agent);
            if prompt::yes_no("¿Re-autenticar?", false)? {
                services_imperative::dispatch(channel, config_dir, secrets_dir).map(|_| ())?;
                println!("✔ {} re-autenticado.", channel);
            } else {
                println!("Sin cambios.");
            }
        } else {
            println!();
            println!("ℹ {} aún no está vinculado a `{}`. Vinculando…", channel, agent);
            // Auth (idempotent — keys ya válidas no piden re-prompt).
            services_imperative::dispatch(channel, config_dir, secrets_dir).map(|_| ())?;
            // Append a `plugins:` el agente.
            if let Some(file) = locate_agent_file(config_dir, &agent) {
                yaml_patch::add_plugin_to_agent(&file, &agent, channel).ok();
                println!("✔ {} agregado a plugins de `{}` en {}", channel, agent, file.display());
            } else {
                println!("⚠  No encontré el archivo YAML de `{}`. Agregalo manualmente.", agent);
            }
        }
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
    println!("── {} / {} ─────────────────", entry.channel, entry.instance);
    println!("Estado     : {} ({})", entry.auth.icon(), entry.auth.label());
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
        if seq.iter().any(|a| {
            a.get("id").and_then(serde_yaml::Value::as_str) == Some(agent_id)
        }) {
            return Some(path);
        }
    }
    None
}
