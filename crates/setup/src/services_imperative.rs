//! Imperative wizard flows for multi-instance channels.
//!
//! The declarative `ServiceDef` pipeline can only write root-level
//! mapping fields; WhatsApp/Telegram multi-account and Google Auth
//! store each require array handling plus an `agents.d/*.yaml` patch
//! so the agent binds the new credential. Those flows live here.
//!
//! Triggered from `lib::run_service` when the service id matches
//! `whatsapp` / `telegram` / `google`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use dialoguer::{theme::ColorfulTheme, Input};

use crate::prompt;
use crate::writer;
use crate::yaml_patch;

fn ask(label: &str, default: Option<&str>, help: Option<&str>, required: bool) -> Result<String> {
    if let Some(h) = help {
        println!("  {h}");
    }
    let theme = ColorfulTheme::default();
    let mut input: Input<String> = Input::with_theme(&theme).with_prompt(label);
    if let Some(d) = default {
        input = input.default(d.to_string()).allow_empty(!required);
    } else {
        input = input.allow_empty(!required);
    }
    let raw: String = input.interact_text()?;
    Ok(raw)
}

pub enum Outcome {
    /// We handled the flow — caller should skip the declarative form.
    Handled,
    /// Not our channel / user declined — caller should run the legacy
    /// declarative form instead.
    NotHandled,
}

/// Pick the agent for a channel-bound service. Honours an
/// already-chosen agent passed via the thread-local override (set by
/// `dispatch_with_agent`) so callers higher up the stack can avoid
/// the redundant re-prompt the user noticed.
fn pick_agent(config_dir: &Path) -> Result<Option<String>> {
    if let Some(prefilled) = AGENT_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Ok(Some(prefilled));
    }
    let agents_yaml = config_dir.join("agents.yaml");
    let mut ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    let drop_dir = config_dir.join("agents.d");
    if drop_dir.exists() {
        for entry in std::fs::read_dir(&drop_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            ids.extend(yaml_patch::list_agent_ids(&path).unwrap_or_default());
        }
    }
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        println!("⚠  No hay agentes declarados — edita `agents.yaml` primero.");
        return Ok(None);
    }
    let idx = prompt::pick_agent(&ids)?;
    Ok(Some(idx))
}

fn pick_allow_agents(all_agents: &[String], default: &str) -> Result<Vec<String>> {
    println!(
        "  Agentes autorizados para publicar desde esta cuenta (coma-separado, vacío = solo `{default}`):"
    );
    let raw = ask("allow_agents", Some(default), None, false)?;
    let chosen: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    // Validate every entry exists.
    for ag in &chosen {
        if !all_agents.iter().any(|a| a == ag) {
            println!("    ⚠  `{ag}` no existe en agents.yaml; se añadirá igual al allow_agents.");
        }
    }
    Ok(chosen)
}

fn list_all_agent_ids(config_dir: &Path) -> Vec<String> {
    let mut ids = yaml_patch::list_agent_ids(&config_dir.join("agents.yaml")).unwrap_or_default();
    let drop_dir = config_dir.join("agents.d");
    if drop_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&drop_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                    continue;
                }
                ids.extend(yaml_patch::list_agent_ids(&path).unwrap_or_default());
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

pub fn run_whatsapp(config_dir: &Path, _secrets_dir: &Path) -> Result<Outcome> {
    println!();
    println!("── WhatsApp — cuenta nueva o adicional ─────────────────");
    let Some(agent_id) = pick_agent(config_dir)? else {
        return Ok(Outcome::NotHandled);
    };
    let default_instance = agent_id.clone();
    let instance = ask(
        "instance",
        Some(&default_instance),
        Some(
            "Etiqueta única de esta cuenta (usada en el topic plugin.outbound.whatsapp.<instance>)",
        ),
        true,
    )?;
    let instance = instance.trim().to_string();
    if instance.is_empty() {
        println!("⚠  instance vacío — abortando. Re-corre y proporciona una etiqueta.");
        return Ok(Outcome::Handled);
    }
    let default_session_dir = format!("./data/workspace/{agent_id}/whatsapp/{instance}");
    let session_dir = ask(
        "session_dir",
        Some(&default_session_dir),
        Some(
            "Directorio donde wa-agent persiste Signal keys + pairing (cada cuenta su propio dir).",
        ),
        true,
    )?;
    let default_media_dir = format!("./data/media/whatsapp/{instance}");
    let media_dir = ask("media_dir", Some(&default_media_dir), None, true)?;
    let all_agents = list_all_agent_ids(config_dir);
    let allow_agents = pick_allow_agents(&all_agents, &agent_id)?;
    let final_allow = if allow_agents.is_empty() {
        vec![agent_id.clone()]
    } else {
        allow_agents
    };

    let file = config_dir.join("plugins").join("whatsapp.yaml");
    yaml_patch::whatsapp_upsert_instance(&file, &instance, &session_dir, &media_dir, &final_allow)?;
    println!("✔ whatsapp.yaml: instancia `{instance}` escrita.");

    // Bind the agent.
    let agent_file = yaml_patch::find_agent_file(config_dir, &agent_id)?.ok_or_else(|| {
        anyhow::anyhow!("no pude encontrar el archivo YAML del agente `{agent_id}`")
    })?;
    yaml_patch::patch_agent_credentials(&agent_file, &agent_id, "whatsapp", &instance)?;
    println!(
        "✔ {}: credentials.whatsapp = `{instance}`.",
        agent_file.display()
    );

    Ok(Outcome::Handled)
}

pub fn run_telegram(config_dir: &Path, secrets_dir: &Path) -> Result<Outcome> {
    println!();
    println!("── Telegram — bot nuevo o adicional ─────────────────");
    let Some(agent_id) = pick_agent(config_dir)? else {
        return Ok(Outcome::NotHandled);
    };
    let default_instance = format!("{agent_id}_bot");
    let instance = ask(
        "instance",
        Some(&default_instance),
        Some("Etiqueta única del bot (usada en plugin.outbound.telegram.<instance>)"),
        true,
    )?;
    let instance = instance.trim().to_string();
    if instance.is_empty() {
        return Ok(Outcome::Handled);
    }
    let token = ask("bot_token", None, Some("Token del bot (@BotFather)."), true)?;
    // Persist token to secrets/<instance>_telegram_token.txt.
    let secret_name = format!("{instance}_telegram_token.txt");
    let secret_path = secrets_dir.join(&secret_name);
    writer::ensure_secrets_dir(secrets_dir)?;
    std::fs::write(&secret_path, token.trim())
        .with_context(|| format!("write {}", secret_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&secret_path)?.permissions();
        perm.set_mode(0o600);
        let _ = std::fs::set_permissions(&secret_path, perm);
    }
    let placeholder = format!("${{file:./secrets/{secret_name}}}");
    let chat_raw = ask(
        "allow_chat_ids",
        Some(""),
        Some("Chat IDs permitidos (coma-separado, vacío = abierto)."),
        false,
    )?;
    let chat_ids: Vec<i64> = chat_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    let all_agents = list_all_agent_ids(config_dir);
    let allow_agents = pick_allow_agents(&all_agents, &agent_id)?;
    let final_allow = if allow_agents.is_empty() {
        vec![agent_id.clone()]
    } else {
        allow_agents
    };

    let file = config_dir.join("plugins").join("telegram.yaml");
    yaml_patch::telegram_upsert_instance(&file, &instance, &placeholder, &final_allow, &chat_ids)?;
    println!("✔ telegram.yaml: instancia `{instance}` escrita.");

    let agent_file = yaml_patch::find_agent_file(config_dir, &agent_id)?.ok_or_else(|| {
        anyhow::anyhow!("no pude encontrar el archivo YAML del agente `{agent_id}`")
    })?;
    yaml_patch::patch_agent_credentials(&agent_file, &agent_id, "telegram", &instance)?;
    yaml_patch::upsert_agent_inbound_binding(&agent_file, &agent_id, "telegram", &instance)?;
    println!(
        "✔ {}: credentials.telegram = `{instance}`.",
        agent_file.display()
    );

    Ok(Outcome::Handled)
}

pub fn run_google(config_dir: &Path, secrets_dir: &Path) -> Result<Outcome> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("device-code runtime")?;
    rt.block_on(run_google_inner(config_dir, secrets_dir))
}

async fn run_google_inner(config_dir: &Path, secrets_dir: &Path) -> Result<Outcome> {
    println!();
    println!("── Google OAuth — cuenta nueva o adicional ──────────────");
    let Some(agent_id) = pick_agent(config_dir)? else {
        return Ok(Outcome::NotHandled);
    };
    let default_id = format!("{agent_id}@google");
    let id = ask(
        "id",
        Some(&default_id),
        Some("Identificador de la cuenta Google (convencionalmente el email)."),
        true,
    )?;
    let client_id = ask(
        "client_id",
        None,
        Some("OAuth client_id de Google Cloud Console."),
        true,
    )?;
    let client_secret = ask(
        "client_secret",
        None,
        Some("OAuth client_secret de Google Cloud Console."),
        true,
    )?;
    let scopes_raw = ask(
        "scopes",
        Some("https://www.googleapis.com/auth/gmail.modify"),
        Some("Scopes separados por coma."),
        true,
    )?;
    let scopes: Vec<String> = scopes_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    writer::ensure_secrets_dir(secrets_dir)?;
    let cid_name = format!("{agent_id}_google_client_id.txt");
    let cs_name = format!("{agent_id}_google_client_secret.txt");
    let tok_name = format!("{agent_id}_google_token.json");
    let cid_path = secrets_dir.join(&cid_name);
    let cs_path = secrets_dir.join(&cs_name);
    let _tok_path = secrets_dir.join(&tok_name);
    std::fs::write(&cid_path, client_id.trim())?;
    std::fs::write(&cs_path, client_secret.trim())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [&cid_path, &cs_path] {
            let mut perm = std::fs::metadata(p)?.permissions();
            perm.set_mode(0o600);
            let _ = std::fs::set_permissions(p, perm);
        }
    }

    let file = config_dir.join("plugins").join("google-auth.yaml");
    let rel_cid = format!("./secrets/{cid_name}");
    let rel_cs = format!("./secrets/{cs_name}");
    let rel_tok = format!("./secrets/{tok_name}");
    yaml_patch::google_auth_upsert_account(
        &file, &id, &agent_id, &rel_cid, &rel_cs, &rel_tok, &scopes,
    )?;
    println!("✔ google-auth.yaml: cuenta `{id}` escrita.");
    println!(
        "   token_path: {} — se crea al primer consent (agent_google_auth_start tool)",
        rel_tok
    );

    let agent_file = yaml_patch::find_agent_file(config_dir, &agent_id)?.ok_or_else(|| {
        anyhow::anyhow!("no pude encontrar el archivo YAML del agente `{agent_id}`")
    })?;
    yaml_patch::patch_agent_credentials(&agent_file, &agent_id, "google", &id)?;
    println!("✔ {}: credentials.google = `{id}`.", agent_file.display());

    // Offer the device-code consent flow inline so headless setups
    // (servers without a browser) can get a refresh_token without
    // firing up `google_auth_start` as an LLM tool. Spawns the
    // standalone `nexo-plugin-google --oauth-once --device`
    // subprocess (Phase 94 extraction); the daemon and setup crate
    // no longer link the OAuth machinery directly.
    if crate::prompt::yes_no(
        "¿Autorizar ahora vía device-code OAuth? (sin abrir navegador local)",
        true,
    )? {
        let tok_path = secrets_dir.join(&tok_name);
        if let Err(e) = spawn_oauth_once_subprocess(
            &agent_id,
            &cid_path,
            &cs_path,
            &tok_path,
            &scopes,
            /* device = */ true,
            secrets_dir,
        ) {
            println!("⚠  Device-code falló: {e}");
            println!(
                "   Puedes reintentar más tarde con la tool `google_auth_start` desde el agente."
            );
        }
    } else {
        println!(
            "   Salta consent. token_path se crea al primer `google_auth_start` desde el agente."
        );
    }

    Ok(Outcome::Handled)
}

/// Phase 94 — spawn `nexo-plugin-google --oauth-once <agent>` as a
/// child process so the setup crate doesn't link the OAuth client.
/// Operator must have run `cargo install nexo-plugin-google` first;
/// otherwise the spawn fails with a clear hint.
///
/// AGNOSTIC by name only: each canonical channel plugin has its own
/// `--oauth-once` / `--pair-once` subcommand convention. The shape
/// (binary on PATH + interactive prompts on inherited stdio) is the
/// same pattern future channel plugins should follow.
pub(crate) fn spawn_oauth_once_subprocess(
    agent_id: &str,
    client_id_file: &Path,
    client_secret_file: &Path,
    token_file: &Path,
    scopes: &[String],
    device: bool,
    workspace_dir: &Path,
) -> anyhow::Result<()> {
    let scopes_csv = scopes.join(",");
    let workspace = workspace_dir.to_string_lossy().into_owned();
    let cid = client_id_file.to_string_lossy().into_owned();
    let cs = client_secret_file.to_string_lossy().into_owned();
    let tok = token_file.to_string_lossy().into_owned();

    let mut cmd = std::process::Command::new("nexo-plugin-google");
    cmd.arg("--oauth-once")
        .arg(agent_id)
        .args(["--client-id-file", &cid])
        .args(["--client-secret-file", &cs])
        .args(["--token-file", &tok])
        .args(["--scopes", &scopes_csv])
        .args(["--workspace-dir", &workspace])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    if device {
        cmd.arg("--device");
    }

    let status = cmd.status().with_context(|| {
        "spawning `nexo-plugin-google --oauth-once` — is the binary on PATH? \
         Run `cargo install nexo-plugin-google` to install it."
    })?;
    if !status.success() {
        anyhow::bail!(
            "nexo-plugin-google exited with status {}",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
        );
    }
    Ok(())
}

pub fn dispatch(svc_id: &str, config_dir: &Path, secrets_dir: &Path) -> Result<Outcome> {
    match svc_id {
        "whatsapp" => run_whatsapp(config_dir, secrets_dir),
        "telegram" => run_telegram(config_dir, secrets_dir),
        "google" => run_google(config_dir, secrets_dir),
        "email" => {
            crate::services::email::run_email_wizard(config_dir, secrets_dir)?;
            Ok(Outcome::Handled)
        }
        _ => Ok(Outcome::NotHandled),
    }
}

thread_local! {
    /// Pre-filled agent id for the inner `pick_agent` helper. Set by
    /// `dispatch_with_agent` so callers higher up the stack
    /// (channels_dashboard's link flow) don't get re-prompted for the
    /// agent they already picked.
    static AGENT_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Variant of [`dispatch`] that accepts an already-picked agent id
/// so the inner per-channel handler skips its own
/// `pick_agent` prompt. Restores the previous override on exit so
/// nested calls in the same thread stay correct.
pub fn dispatch_with_agent(
    svc_id: &str,
    agent_id: &str,
    config_dir: &Path,
    secrets_dir: &Path,
) -> Result<Outcome> {
    let previous = AGENT_OVERRIDE.with(|cell| cell.replace(Some(agent_id.to_string())));
    let result = dispatch(svc_id, config_dir, secrets_dir);
    AGENT_OVERRIDE.with(|cell| {
        *cell.borrow_mut() = previous;
    });
    result
}

// Helper used by PathBuf return sites.
pub fn _ensure_pathbuf(p: PathBuf) -> PathBuf {
    p
}
