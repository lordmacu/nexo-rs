//! Atomic, perms-aware persistence for secrets + YAML patches.
//!
//! Secrets land under `secrets/<file>` with mode `0600`. We write to a
//! sibling tempfile inside the same directory and `rename` into place so
//! a crash mid-write never leaves a half-file on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context, Result};

use crate::registry::{FieldDef, FieldTarget, ServiceDef, ServiceValues};
use crate::yaml_patch;

/// Guarantee the secrets directory exists with `0700` mode on unix.
pub fn ensure_secrets_dir(secrets_dir: &Path) -> Result<()> {
    fs::create_dir_all(secrets_dir).with_context(|| format!("mkdir {}", secrets_dir.display()))?;
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(secrets_dir)?.permissions();
        perms.set_mode(0o700);
        fs::set_permissions(secrets_dir, perms)
            .with_context(|| format!("chmod 0700 {}", secrets_dir.display()))?;
    }
    Ok(())
}

/// Write a value to `secrets/<file>` atomically with mode `0600`.
///
/// If an existing secret lives at the target path, it is copied to
/// `<target>.bak` before the new one lands — re-running setup can't
/// silently destroy a working key. Only one generation is kept
/// (`.bak` is overwritten on subsequent rotations) so the directory
/// stays bounded.
///
/// Skipped entirely if `value` is empty — we don't want a file full of
/// nothing to shadow a real env-var override.
pub fn write_secret(secrets_dir: &Path, file: &str, value: &str) -> Result<PathBuf> {
    let target = secrets_dir.join(file);
    if value.is_empty() {
        if target.exists() {
            fs::remove_file(&target).with_context(|| format!("remove {}", target.display()))?;
        }
        return Ok(target);
    }
    ensure_secrets_dir(secrets_dir)?;

    // Tempfile created with mode `0600` up front — no TOCTOU window
    // where the secret is world-readable between creation and chmod.
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        builder.permissions(Permissions::from_mode(0o600));
    }
    let tmp = builder
        .tempfile_in(secrets_dir)
        .with_context(|| format!("tempfile in {}", secrets_dir.display()))?;
    {
        let mut file = tmp.reopen()?;
        file.write_all(value.as_bytes())?;
        file.flush()?;
        file.sync_all().ok();
    }
    tmp.as_file().sync_all().ok();

    // Back up any existing secret before clobbering it. `.bak` is a
    // single generation — good enough to let the operator recover
    // from a typo without building up an unbounded history.
    if target.exists() {
        let backup = target.with_extension(
            target
                .extension()
                .map(|e| format!("{}.bak", e.to_string_lossy()))
                .unwrap_or_else(|| "bak".to_string()),
        );
        fs::copy(&target, &backup)
            .with_context(|| format!("backup {} → {}", target.display(), backup.display()))?;
        tracing::info!(backup = %backup.display(), "previous secret backed up");
    }

    tmp.persist(&target)
        .map_err(|e| anyhow::anyhow!("persist secret {}: {e}", target.display()))?;
    tracing::info!(path = %target.display(), "secret written");
    Ok(target)
}

/// Persist everything the user typed for one service.
pub fn persist(
    svc: &ServiceDef,
    values: &ServiceValues,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    // MiniMax is a special case: the destination secret file depends on
    // the `key_kind` the operator picked (Coding Plan vs API key), and
    // we also patch `config/llm.yaml::providers.minimax.base_url` based
    // on the region. We take over the whole persist path instead of
    // letting the default field-by-field loop handle it.
    if svc.id == "minimax" {
        return persist_minimax(values, secrets_dir, config_dir);
    }
    if svc.id == "google-auth" {
        return persist_google_auth(values, secrets_dir, config_dir);
    }
    if svc.id == "link" {
        return run_agent_link(secrets_dir, config_dir);
    }
    if svc.id == "anthropic" {
        return persist_anthropic(values, secrets_dir, config_dir);
    }

    for field in &svc.fields {
        let Some(value) = values.get(field.key) else {
            continue;
        };
        if value.is_empty() && !field.required {
            continue;
        }
        persist_field(field, value, secrets_dir, config_dir)?;
    }

    // Auto-attach: if this service is a skill or plugin, append its id
    // to the target agent's `skills:` / `plugins:` list in agents.yaml.
    // Single agent → auto-pick. Multi → prompt once. No-op if the entry
    // is already there (idempotent).
    attach_service_to_agent(svc, config_dir)?;

    Ok(())
}

/// Append `svc.id` to the chosen agent's skills/plugins list when the
/// service category matches. Quietly noops otherwise.
fn attach_service_to_agent(svc: &ServiceDef, config_dir: &Path) -> Result<()> {
    use crate::registry::Category;
    let list_key = match svc.category {
        Category::Skill => "skills",
        Category::Plugin => "plugins",
        _ => return Ok(()),
    };
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = crate::yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    if agent_ids.is_empty() {
        return Ok(());
    }
    let agent_id = crate::prompt::pick_agent(&agent_ids)?;
    let changed = match list_key {
        "skills" => crate::yaml_patch::add_skill_to_agent(&agents_yaml, &agent_id, svc.id)?,
        "plugins" => crate::yaml_patch::add_plugin_to_agent(&agents_yaml, &agent_id, svc.id)?,
        _ => false,
    };
    if changed {
        println!(
            "✔ `{}` parcheado → agente `{}` ahora tiene `{}: {}`",
            agents_yaml.display(),
            agent_id,
            list_key,
            svc.id
        );
    }
    Ok(())
}

fn persist_minimax(values: &ServiceValues, secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    let key_kind = values.get("key_kind").unwrap_or("plan").trim().to_string();
    let key_value = values
        .get("key_value")
        .ok_or_else(|| anyhow::anyhow!("missing key_value"))?
        .trim()
        .to_string();
    if key_value.is_empty() {
        anyhow::bail!("key_value cannot be empty");
    }
    let group_id = values
        .get("group_id")
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if group_id.is_empty() {
        anyhow::bail!("group_id cannot be empty");
    }
    let region = values.get("region").unwrap_or("global").trim().to_string();

    let secret_file = match key_kind.as_str() {
        "plan" => "minimax_code_plan_key.txt",
        "api" => "minimax_api_key.txt",
        other => anyhow::bail!("unknown key_kind `{other}`"),
    };
    write_secret(secrets_dir, secret_file, &key_value)?;
    write_secret(secrets_dir, "minimax_group_id.txt", &group_id)?;

    // Token Plan keys (`sk-cp-…`) speak the Anthropic Messages wire
    // over `{host}/anthropic/v1/messages`; regular API keys stay on
    // the public OpenAI-compat endpoint. This is the critical
    // compatibility gate — point the client at the wrong path and
    // MiniMax returns 401/404.
    let (base_url, api_flavor) = match (key_kind.as_str(), region.as_str()) {
        ("plan", "cn") => ("https://api.minimaxi.com/anthropic", "anthropic_messages"),
        ("plan", "chat") => ("https://api.minimax.io/anthropic", "anthropic_messages"),
        ("plan", _) => ("https://api.minimax.io/anthropic", "anthropic_messages"),
        (_, "cn") => ("https://api.minimaxi.com/v1", "openai_compat"),
        (_, "chat") => ("https://api.minimax.chat/v1", "openai_compat"),
        _ => ("https://api.minimax.io/v1", "openai_compat"),
    };
    let yaml_path = config_dir.join("llm.yaml");
    crate::yaml_patch::upsert(
        &yaml_path,
        "providers.minimax.base_url",
        base_url,
        crate::yaml_patch::ValueKind::String,
    )?;
    crate::yaml_patch::upsert(
        &yaml_path,
        "providers.minimax.api_flavor",
        api_flavor,
        crate::yaml_patch::ValueKind::String,
    )?;

    println!();
    println!("✔ MiniMax configurado:");
    println!("  Tipo       : {key_kind} → secrets/{secret_file}");
    println!("  Región     : {region}");
    println!("  Base URL   : {base_url}");
    println!("  API flavor : {api_flavor}");
    println!();
    println!("Siguiente paso: exporta el key al entorno (o apunta config/llm.yaml");
    println!(
        "a `\"${{file:{}}}\"`):",
        secrets_dir.join(secret_file).display()
    );
    if key_kind == "plan" {
        println!(
            "  export MINIMAX_CODE_PLAN_KEY=$(cat {})",
            secrets_dir.join(secret_file).display()
        );
    } else {
        println!(
            "  export MINIMAX_API_KEY=$(cat {})",
            secrets_dir.join(secret_file).display()
        );
    }
    println!(
        "  export MINIMAX_GROUP_ID=$(cat {})",
        secrets_dir.join("minimax_group_id.txt").display()
    );
    Ok(())
}

fn persist_google_auth(
    values: &ServiceValues,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let client_id = values
        .get("client_id")
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let client_secret = values
        .get("client_secret")
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        anyhow::bail!("client_id and client_secret are required");
    }
    let scopes = values
        .get("scopes")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "userinfo.email,userinfo.profile".to_string());
    // Fixed at 8766 — appears only in the `redirect_uri` that Google
    // echoes back in the callback URL. Desktop OAuth clients accept
    // any loopback port without pre-registration, so no Cloud Console
    // edit is needed and nothing binds this port locally.
    let redirect_port: u16 = 8766;

    let scopes_vec: Vec<String> = scopes
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    // Pick target agent FIRST — secrets + YAML refs are scoped under
    // that agent's folder so two agents can hold different Google
    // OAuth apps without colliding. Zero agents → bail.
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = crate::yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    let agent_id = if agent_ids.is_empty() {
        None
    } else {
        Some(crate::prompt::pick_agent(&agent_ids)?)
    };

    let is_docker = config_dir
        .file_name()
        .map(|n| n == "docker")
        .unwrap_or(false);

    // Per-agent secret files → each agent holds its own Google OAuth
    // app. Two agents can point at different Cloud Console projects
    // without ever colliding on disk.
    let mut patched_agent = false;
    if let Some(id) = agent_id.as_deref() {
        let agent_secrets = secrets_dir.join(id);
        std::fs::create_dir_all(&agent_secrets)
            .with_context(|| format!("mkdir {}", agent_secrets.display()))?;
        write_secret(&agent_secrets, "google_client_id.txt", &client_id)?;
        write_secret(&agent_secrets, "google_client_secret.txt", &client_secret)?;

        let (client_id_ref, client_secret_ref) = if is_docker {
            (
                format!("${{file:/run/secrets/{id}_google_client_id}}"),
                format!("${{file:/run/secrets/{id}_google_client_secret}}"),
            )
        } else {
            (
                format!("${{file:./secrets/{id}/google_client_id.txt}}"),
                format!("${{file:./secrets/{id}/google_client_secret.txt}}"),
            )
        };
        crate::yaml_patch::patch_agent_google_auth(
            &agents_yaml,
            id,
            &client_id_ref,
            &client_secret_ref,
            redirect_port,
            &scopes_vec,
        )?;
        crate::yaml_patch::add_skill_to_agent(&agents_yaml, id, "google-auth")?;
        patched_agent = true;
    } else {
        // No agent available → write to the shared path as a fallback
        // so the files still exist for manual wiring later.
        write_secret(secrets_dir, "google_client_id.txt", &client_id)?;
        write_secret(secrets_dir, "google_client_secret.txt", &client_secret)?;
    }

    // Docker compose patch — only meaningful when the config dir is
    // the docker one. `docker-compose.yml` lives at the repo root;
    // resolve it from the secrets dir parent.
    let compose_path = secrets_dir
        .parent()
        .map(|p| p.join("docker-compose.yml"))
        .unwrap_or_else(|| std::path::PathBuf::from("docker-compose.yml"));
    let mut patched_compose = false;
    if is_docker && compose_path.exists() {
        crate::yaml_patch::patch_compose_google_secrets(&compose_path)?;
        patched_compose = true;
    }

    println!();
    println!("✔ Google OAuth guardado en secrets/");
    if patched_agent {
        println!(
            "✔ `{}` parcheado → agente `{}` tiene google_auth",
            agents_yaml.display(),
            agent_id.as_deref().unwrap_or("?")
        );
    }
    if patched_compose {
        println!(
            "✔ `{}` parcheado → secrets google_client_id/secret registrados",
            compose_path.display()
        );
    }
    println!();
    if !patched_agent {
        println!("⚠  No encontré agentes en agents.yaml. Pegá el bloque a mano:");
        println!();
        println!("    google_auth:");
        println!("      client_id: \"${{file:./secrets/google_client_id.txt}}\"");
        println!("      client_secret: \"${{file:./secrets/google_client_secret.txt}}\"");
        println!("      redirect_port: {redirect_port}");
        println!("      scopes:");
        for s in &scopes_vec {
            println!("        - {s}");
        }
        println!();
    }
    if is_docker {
        println!("Siguiente paso:");
        println!("  make docker-build && make docker-up");
    } else {
        println!("Siguiente paso:");
        println!("  ./target/release/agent --config {}", config_dir.display());
    }
    println!();
    if let Some(id) = agent_id.as_deref() {
        if crate::prompt::yes_no("¿Lanzar el consent de Google ahora?", true)? {
            let workspace = resolve_workspace_for_agent(config_dir, id)
                .unwrap_or_else(|| PathBuf::from(format!("./data/workspace/{id}")));
            std::fs::create_dir_all(&workspace)
                .with_context(|| format!("mkdir workspace {}", workspace.display()))?;

            run_google_consent_callback_url(
                &client_id,
                &client_secret,
                &scopes_vec,
                redirect_port,
                &workspace,
            )?;
        } else {
            println!("OK. Cuando quieras: pedile algo de Google al agente y llamará `google_auth_start`.");
        }
    } else {
        println!("Primera vez: el agente llamará `google_auth_start`, te da una URL,");
        println!("abrís, consent → refresh_token queda en <workspace>/google_tokens.json.");
        if redirect_port == 8765 {
            println!("Remoto: `ssh -L 8765:127.0.0.1:8765 <host>`");
        }
    }
    Ok(())
}

/// Resolve where to persist `google_tokens.json` for an agent. Reads
/// `agents.yaml::agents[<id>].workspace`; rewrites `/app/...` paths to
/// `./...` because the wizard runs on the host, not inside the container.
fn resolve_workspace_for_agent(config_dir: &Path, agent_id: &str) -> Option<PathBuf> {
    let agents_yaml = config_dir.join("agents.yaml");
    let raw = crate::yaml_patch::get_agent_workspace(&agents_yaml, agent_id)
        .ok()
        .flatten()?;
    if let Some(rest) = raw.strip_prefix("/app/") {
        Some(PathBuf::from(format!("./{rest}")))
    } else {
        Some(PathBuf::from(raw))
    }
}

/// Build the consent URL, print it, wait for the user to paste back
/// the localhost URL from their browser's address bar, extract `code`,
/// exchange → tokens. No listener, no tunnel, no GCP edits.
fn run_google_consent_callback_url(
    client_id: &str,
    client_secret: &str,
    scopes: &[String],
    redirect_port: u16,
    workspace: &Path,
) -> Result<()> {
    let client_id = client_id.to_string();
    let client_secret = client_secret.to_string();
    let scopes = scopes.to_vec();
    let workspace = workspace.to_path_buf();
    std::thread::spawn(move || -> Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for callback-url flow")?;
        rt.block_on(async move {
            use nexo_plugin_google::{GoogleAuthClient, GoogleAuthConfig};
            let cfg = GoogleAuthConfig {
                client_id,
                client_secret,
                scopes,
                token_file: "google_tokens.json".to_string(),
                redirect_port,
            };
            let client = GoogleAuthClient::new(cfg, &workspace);
            let state = format!("{:016x}", rand::random::<u64>());
            let url = client.build_auth_url(&state);

            println!();
            println!("╭─ Google OAuth consent ────────────────────────────────────────╮");
            println!("│ 1. Abrí este link en CUALQUIER browser (laptop/celular):      │");
            println!("╰───────────────────────────────────────────────────────────────╯");
            println!();
            println!("  {url}");
            println!();
            println!("2. Login con tu cuenta de Google → click Allow.");
            println!();
            println!(
                "3. El browser te va a redirigir a `http://127.0.0.1:{redirect_port}/callback?…`"
            );
            println!("   Va a aparecer error 'esta página no funciona' — ignoralo.");
            println!("   Lo único que importa: la URL completa en la barra de direcciones.");
            println!();
            println!("4. Copiá esa URL y pegala aquí abajo.");
            println!();

            let pasted = crate::prompt::ask_callback_url()?;
            let code = extract_code_from_url(&pasted)
                .context("no pude extraer `code=…` de la URL pegada")?;
            let tokens = client
                .exchange_code(&code)
                .await
                .context("intercambio de code falló — ¿URL completa? ¿se venció (10min)?")?;

            println!();
            println!("✔ Tokens guardados en {}", client.token_path().display());
            println!("  Scopes otorgados : {}", tokens.scopes.join(", "));
            println!(
                "  Refresh token    : {}",
                if tokens.refresh_token.is_some() {
                    "sí (auto-renovación activa)"
                } else {
                    "NO — re-corré este flow con access_type=offline forzado"
                }
            );
            Ok::<(), anyhow::Error>(())
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("callback-url thread panicked"))?
}

/// Parse `code=…` out of any string that looks like a URL or query.
fn extract_code_from_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let q = raw.split_once('?').map(|(_, q)| q).unwrap_or(raw);
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("code=") {
            let decoded = urlencoding_decode(v);
            return Some(decoded);
        }
    }
    None
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                    if let Ok(n) = u8::from_str_radix(hex, 16) {
                        out.push(n);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Channel-first linking UX. User picks a canal (whatsapp/telegram/…)
/// and the wizard branches on its current state:
/// - Not attached to any agent → pick agent + link + pair (inline
///   when possible, printed instructions when not).
/// - Already attached → show which agent owns it + multi-select of
///   other plugins/skills for that same agent (secondary editor).
fn run_agent_link(secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = crate::yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    if agent_ids.is_empty() {
        anyhow::bail!("no hay agentes en {}", agents_yaml.display());
    }

    // Outer loop: after each channel is configured, come back here to
    // ask for the next one. Breaks when the operator picks "Listo".
    loop {
        if !run_agent_link_once(&agents_yaml, &agent_ids, secrets_dir, config_dir)? {
            break;
        }
    }
    Ok(())
}

/// Run one iteration of the channel picker. Returns `true` if the
/// operator wants to configure another channel, `false` to exit.
fn run_agent_link_once(
    agents_yaml: &Path,
    agent_ids: &[String],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<bool> {
    // Channel catalog. "Listo" is the sentinel exit row so the
    // operator can bail without Ctrl+C.
    let channels = [
        ("whatsapp", "WhatsApp"),
        ("telegram", "Telegram"),
        ("email", "Email"),
        ("__done__", "Listo (salir)"),
    ];
    let labels: Vec<&str> = channels.iter().map(|(_, l)| *l).collect();
    let idx = crate::prompt::pick_from_list("¿Qué canal querés vincular?", &labels)?;
    let (canal_id, canal_label) = channels[idx];
    if canal_id == "__done__" {
        return Ok(false);
    }

    // Step 2 — ALWAYS pick the target agent, even if only one exists
    // and even if the channel is already attached somewhere. Visible
    // confirmation > silent auto-pick.
    let agent_ids_vec: Vec<String> = agent_ids.to_vec();
    println!();
    let target = crate::prompt::pick_agent(&agent_ids_vec)?;
    let existing_plugins =
        crate::yaml_patch::get_agent_list(agents_yaml, &target, "plugins").unwrap_or_default();
    let already_linked = existing_plugins.iter().any(|p| p == canal_id);

    if !already_linked {
        crate::yaml_patch::add_plugin_to_agent(agents_yaml, &target, canal_id)?;
        println!("✔ `{canal_id}` agregado a `agents.{target}.plugins`");
        run_channel_pairing(canal_id, &target, secrets_dir, config_dir)?;
    } else {
        println!("`{canal_label}` ya está en `agents.{target}.plugins`.");
        let options = [
            "Dejar como está",
            "Reconectar (usa credenciales guardadas, NO muestra QR)",
            "Forzar QR nuevo (borra sesión local, escanear de cero)",
            "Desvincular (quitar del agente, conservar sesión en disco)",
        ];
        let choice = crate::prompt::pick_from_list("¿Qué querés hacer?", &options)?;
        match choice {
            0 => {}
            1 => run_channel_pairing(canal_id, &target, secrets_dir, config_dir)?,
            2 => {
                wipe_channel_session(canal_id, &target, config_dir)?;
                run_channel_pairing(canal_id, &target, secrets_dir, config_dir)?;
            }
            3 => {
                let removed = crate::yaml_patch::remove_list_entry(
                    agents_yaml,
                    &target,
                    "plugins",
                    canal_id,
                )?;
                if removed {
                    println!("✔ `{canal_id}` removido de `agents.{target}.plugins`");
                    println!("  (sesión en disco intacta — volver a linkear = reconectar)");
                } else {
                    println!("ℹ  Nada que remover.");
                }
            }
            _ => {}
        }
    }

    // Ask if there's another channel to configure. Default = no so
    // Enter returns to the hub after a single channel.
    let again = crate::prompt::yes_no("¿Configurar otro canal?", false)?;
    Ok(again)
}

/// Dispatch the per-channel pairing ritual. For channels without a
/// dedicated flow, just print a note — they're ready as soon as the
/// plugin is in the YAML.
fn run_channel_pairing(
    canal_id: &str,
    agent_id: &str,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    match canal_id {
        "telegram" => {
            println!();
            println!("── Pairing Telegram ────────────────────────────────");
            run_telegram_link_sync(agent_id, secrets_dir, config_dir)?;
        }
        "whatsapp" => {
            // Inline pairing: wa-agent Client renders the QR, we block
            // the wizard until `connect()` returns (first real message
            // implies successful pairing).
            println!();
            println!("── Pairing WhatsApp ────────────────────────────────");
            run_whatsapp_pairing_sync(agent_id, config_dir)?;
        }
        "email" => {
            println!();
            println!("ℹ  Email requiere configurar IMAP/SMTP via `setup email` (próximamente).");
        }
        _ => {
            println!("✔ Plugin `{canal_id}` listo — no necesita pairing.");
        }
    }
    Ok(())
}

/// Blow away the on-disk session for a channel scoped to ONE agent.
/// Resolves the path from that agent's workspace (`<workspace>/<channel>/default`)
/// so wiping `ana` never touches `kate`.
fn wipe_channel_session(canal_id: &str, agent_id: &str, config_dir: &Path) -> Result<()> {
    let target = match canal_id {
        "whatsapp" => agent_whatsapp_session_dir(config_dir, agent_id),
        _ => return Ok(()),
    };
    if target.exists() {
        std::fs::remove_dir_all(&target).with_context(|| format!("rm -rf {}", target.display()))?;
        println!("✔ Sesión borrada: {}", target.display());
    } else {
        println!("ℹ  Nada que borrar en {} (ya vacío).", target.display());
    }
    Ok(())
}

/// Per-agent WhatsApp session path. Resolution order:
///   1. `config/plugins/whatsapp.yaml::whatsapp.session_dir` if present —
///      lets the operator pin a custom location (Docker bind mount,
///      encrypted volume, shared team storage) and have setup honor it
///      instead of silently overwriting on the next pair.
///   2. Otherwise derive from the agent's workspace:
///      `<workspace>/whatsapp/default`.
///
/// Docker-style `/app/` prefixes (from config files authored inside a
/// container image) are rewritten to host-relative paths so the wizard
/// running on the host touches the same dir the container agent would.
fn agent_whatsapp_session_dir(config_dir: &Path, agent_id: &str) -> PathBuf {
    let wa_yaml = config_dir.join("plugins").join("whatsapp.yaml");
    if let Ok(Some(existing)) = crate::yaml_patch::get_string(&wa_yaml, "whatsapp.session_dir") {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return resolve_docker_path(trimmed);
        }
    }
    let agents_yaml = config_dir.join("agents.yaml");
    let raw = crate::yaml_patch::get_agent_workspace(&agents_yaml, agent_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("./data/workspace/{agent_id}"));
    let ws = resolve_docker_path(&raw);
    ws.join("whatsapp").join("default")
}

/// Rewrite `/app/foo` → `./foo` so YAML authored for a container still
/// resolves correctly when the wizard runs on the host.
fn resolve_docker_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("/app/") {
        PathBuf::from(format!("./{rest}"))
    } else {
        PathBuf::from(raw)
    }
}

/// Update `config/plugins/whatsapp.yaml::whatsapp.session_dir` to
/// point at the given agent's session. Needed because the plugin
/// runtime reads this path as the active WhatsApp account; when
/// pairing for a different agent we switch it. Multi-account is
/// future work (list shape in the yaml).
fn set_active_whatsapp_session(config_dir: &Path, session_dir: &Path) -> Result<()> {
    let yaml_path = config_dir.join("plugins").join("whatsapp.yaml");
    crate::yaml_patch::upsert(
        &yaml_path,
        "whatsapp.session_dir",
        &session_dir.display().to_string(),
        crate::yaml_patch::ValueKind::String,
    )?;
    Ok(())
}

/// Inline WhatsApp pairing via wa-agent. Creates the session dir
/// under the agent's workspace, spawns a Client, renders each QR push
/// as Unicode blocks on the terminal, then blocks on `connect()`
/// until the user scans. First message after scan = paired.
///
/// Runs on its own thread+runtime (same pattern as the google consent
/// flow) because `persist` is sync.
fn run_whatsapp_pairing_sync(agent_id: &str, config_dir: &Path) -> Result<()> {
    // Session dir is scoped to the agent — each agent has its own
    // credentials under <workspace>/whatsapp/default. The plugin
    // config points at exactly one of these at a time; we flip that
    // pointer before pairing so the runtime picks up the new session.
    let session_dir = agent_whatsapp_session_dir(config_dir, agent_id);
    std::fs::create_dir_all(&session_dir)
        .with_context(|| format!("mkdir {}", session_dir.display()))?;
    set_active_whatsapp_session(config_dir, &session_dir)?;

    println!("Session: {}", session_dir.display());
    println!("Abrí WhatsApp → Settings → Linked Devices → Link a Device");
    println!("y escaneá el QR que aparece aquí abajo. (Ctrl+C cancela)");
    println!();

    std::thread::spawn(move || -> Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for whatsapp pairing")?;
        rt.block_on(async move {
            nexo_plugin_whatsapp::session::pair_once(&session_dir).await?;
            println!();
            println!("✔ WhatsApp paired — session en {}", session_dir.display());
            Ok::<(), anyhow::Error>(())
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("whatsapp pairing thread panicked"))?
}

/// Bridge to the existing async `telegram_link::run`. Reuses the
/// thread+runtime trick we use for google consent because `persist`
/// is a sync entry point. `agent_id` scopes the allowlist write to
/// the right telegram instance.
fn run_telegram_link_sync(
    agent_id: &str,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let secrets_dir = secrets_dir.to_path_buf();
    let config_dir = config_dir.to_path_buf();
    let agent_id = agent_id.to_string();
    std::thread::spawn(move || -> Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for telegram link")?;
        rt.block_on(crate::telegram_link::run(
            &secrets_dir,
            &config_dir,
            Some(agent_id.as_str()),
        ))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("telegram link thread panicked"))?
}

/// Persist credentials for the Anthropic provider. Branches by
/// `auth_mode` and patches `llm.yaml::providers.anthropic.auth.*`.
fn persist_anthropic(values: &ServiceValues, secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    let mode = values
        .get("auth_mode")
        .map(str::trim)
        .unwrap_or("api_key")
        .to_string();
    let secret_raw = values
        .get("secret_value")
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let yaml_path = config_dir.join("llm.yaml");

    match mode.as_str() {
        "api_key" => {
            if secret_raw.is_empty() {
                anyhow::bail!("auth_mode=api_key: secret_value (sk-ant-…) is required");
            }
            write_secret(secrets_dir, "anthropic_api_key.txt", &secret_raw)?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.mode",
                "api_key",
                yaml_patch::ValueKind::String,
            )?;
            println!("✔ Anthropic api_key guardado en secrets/anthropic_api_key.txt");
            println!(
                "  export ANTHROPIC_API_KEY=$(cat {})",
                secrets_dir.join("anthropic_api_key.txt").display()
            );
        }
        "setup_token" => {
            // Validate prefix + length by deferring to the llm crate.
            // We re-implement the shape check inline to keep the setup
            // crate free of an llm dependency.
            if !secret_raw.starts_with("sk-ant-oat01-") {
                anyhow::bail!("setup_token must start with `sk-ant-oat01-`");
            }
            if secret_raw.len() < 80 {
                anyhow::bail!("setup_token looks too short (< 80 chars)");
            }
            write_secret(secrets_dir, "anthropic_setup_token.txt", &secret_raw)?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.mode",
                "setup_token",
                yaml_patch::ValueKind::String,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.setup_token_file",
                &format!(
                    "{}",
                    secrets_dir.join("anthropic_setup_token.txt").display()
                ),
                yaml_patch::ValueKind::String,
            )?;
            println!("✔ Anthropic setup-token guardado en secrets/anthropic_setup_token.txt");
        }
        "cli_import" => {
            let home = std::env::var("HOME").unwrap_or_default();
            let candidate = PathBuf::from(&home)
                .join(".claude")
                .join(".credentials.json");
            if !candidate.is_file() {
                anyhow::bail!(
                    "no se encontró `{}`. Corré `claude login` primero (o elegí otro modo).",
                    candidate.display()
                );
            }
            let text = fs::read_to_string(&candidate)
                .with_context(|| format!("read {}", candidate.display()))?;
            let root: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("parse {}", candidate.display()))?;
            let creds = root.get("claudeAiOauth").ok_or_else(|| {
                anyhow::anyhow!("{} missing `claudeAiOauth` field", candidate.display())
            })?;
            let access = creds
                .get("accessToken")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing accessToken"))?;
            let refresh = creds
                .get("refreshToken")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let expires_ms = creds.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
            let expires_at = if expires_ms > 10_000_000_000 {
                expires_ms / 1000
            } else {
                expires_ms
            };
            let account_email = creds
                .get("account")
                .and_then(|v| v.get("emailAddress"))
                .and_then(|v| v.as_str());
            let bundle = serde_json::json!({
                "access_token": access,
                "refresh_token": refresh,
                "expires_at": expires_at,
                "account_email": account_email,
                "obtained_at": chrono::Utc::now().to_rfc3339(),
                "source": "claude-cli",
            });
            write_secret(
                secrets_dir,
                "anthropic_oauth.json",
                &serde_json::to_string_pretty(&bundle)?,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.mode",
                "oauth_bundle",
                yaml_patch::ValueKind::String,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.bundle",
                &format!("{}", secrets_dir.join("anthropic_oauth.json").display()),
                yaml_patch::ValueKind::String,
            )?;
            println!(
                "✔ Credenciales Claude CLI importadas ({}).",
                account_email.unwrap_or("cuenta desconocida")
            );
            println!("  → secrets/anthropic_oauth.json (se auto-refresca en runtime).");
        }
        "oauth_login" => {
            // Interactive PKCE browser flow (Claude.ai subscription).
            let token = crate::services::anthropic_oauth::run_flow()?;
            let bundle = serde_json::json!({
                "access_token": token.access_token,
                "refresh_token": token.refresh_token,
                "expires_at": token.expires_at,
                "account_email": token.account_email,
                "obtained_at": chrono::Utc::now().to_rfc3339(),
                "source": "oauth_login",
            });
            write_secret(
                secrets_dir,
                "anthropic_oauth.json",
                &serde_json::to_string_pretty(&bundle)?,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.mode",
                "oauth_bundle",
                yaml_patch::ValueKind::String,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.bundle",
                &format!("{}", secrets_dir.join("anthropic_oauth.json").display()),
                yaml_patch::ValueKind::String,
            )?;
            println!(
                "✔ OAuth completado ({}).",
                token
                    .account_email
                    .as_deref()
                    .unwrap_or("cuenta desconocida")
            );
            println!("  → secrets/anthropic_oauth.json (auto-refresh activo).");
        }
        "oauth_bundle" => {
            if secret_raw.is_empty() {
                anyhow::bail!(
                    "auth_mode=oauth_bundle: pegá el JSON {{access_token,refresh_token,expires_at}}"
                );
            }
            // Validate shape before writing.
            let parsed: serde_json::Value =
                serde_json::from_str(&secret_raw).context("secret_value no es JSON válido")?;
            if parsed
                .get("access_token")
                .and_then(|v| v.as_str())
                .is_none()
            {
                anyhow::bail!("bundle JSON missing access_token");
            }
            if parsed
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .is_none()
            {
                anyhow::bail!("bundle JSON missing refresh_token");
            }
            if parsed.get("expires_at").and_then(|v| v.as_i64()).is_none() {
                anyhow::bail!("bundle JSON missing expires_at (unix seconds)");
            }
            write_secret(
                secrets_dir,
                "anthropic_oauth.json",
                &serde_json::to_string_pretty(&parsed)?,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.mode",
                "oauth_bundle",
                yaml_patch::ValueKind::String,
            )?;
            yaml_patch::upsert(
                &yaml_path,
                "providers.anthropic.auth.bundle",
                &format!("{}", secrets_dir.join("anthropic_oauth.json").display()),
                yaml_patch::ValueKind::String,
            )?;
            println!("✔ Bundle OAuth guardado en secrets/anthropic_oauth.json");
        }
        other => anyhow::bail!("auth_mode `{other}` no reconocido"),
    }

    // Optional: make Anthropic the default provider for one or all
    // agents in `agents.yaml`. Controlled by the `set_as_default`
    // field. Skipped when the value is `no` or missing.
    let set_default = values.get("set_as_default").map(str::trim).unwrap_or("no");
    if set_default == "yes" || set_default == "first" {
        let model = values
            .get("default_model")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("claude-sonnet-4-5");
        let agents_yaml = config_dir.join("agents.yaml");
        if !agents_yaml.exists() {
            println!(
                "⚠ {} no existe — skip set_as_default",
                agents_yaml.display()
            );
        } else {
            let target_id = if set_default == "first" {
                // Read first agent's id so the patch is precise.
                let txt = fs::read_to_string(&agents_yaml).ok();
                txt.and_then(|t| {
                    let v: serde_yaml::Value = serde_yaml::from_str(&t).ok()?;
                    v.get("agents")?
                        .as_sequence()?
                        .first()?
                        .get("id")?
                        .as_str()
                        .map(str::to_string)
                })
                .unwrap_or_default()
            } else {
                "*".to_string()
            };
            match yaml_patch::patch_agent_model(&agents_yaml, &target_id, "anthropic", model) {
                Ok(patched) => {
                    println!(
                        "✔ agents.yaml: {} agente(s) ahora usan anthropic/{model}: {}",
                        patched.len(),
                        patched.join(", ")
                    );
                }
                Err(e) => {
                    println!("⚠ no se pudo parchear agents.yaml: {e}");
                }
            }
        }
    }

    Ok(())
}

#[allow(dead_code)]
fn persist_minimax_portal(values: &ServiceValues, secrets_dir: &Path) -> Result<()> {
    let region_url = match values.get("region").unwrap_or("global") {
        "cn" => "https://api.minimaxi.com",
        _ => "https://api.minimax.io",
    };
    let access_token = values
        .get("access_token")
        .ok_or_else(|| anyhow::anyhow!("missing access_token"))?
        .trim()
        .to_string();
    if access_token.is_empty() {
        anyhow::bail!("access_token cannot be empty");
    }
    let refresh_token = values
        .get("refresh_token")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default();
    let ttl: i64 = values
        .get("expires_in_secs")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(86_400);
    let now = chrono::Utc::now().timestamp();
    let expires_at = now + ttl;

    // Consolidated bundle — this is the ONLY file the LLM client needs.
    // We still persist the three sidecar files for operators who want
    // to `cat` individual pieces or source them into a shell.
    let bundle = serde_json::json!({
        "access_token":  access_token,
        "refresh_token": refresh_token,
        "expires_at":    expires_at,
        "region":        region_url,
        "obtained_at":   chrono::Utc::now().to_rfc3339(),
    });
    write_secret(
        secrets_dir,
        "minimax_portal.json",
        &serde_json::to_string_pretty(&bundle)?,
    )?;
    write_secret(secrets_dir, "minimax_portal_access.txt", &access_token)?;
    write_secret(secrets_dir, "minimax_portal_refresh.txt", &refresh_token)?;
    write_secret(
        secrets_dir,
        "minimax_portal_expires.txt",
        &expires_at.to_string(),
    )?;

    println!();
    println!("✔ Token Plan guardado.");
    println!("  Región    : {region_url}");
    println!("  Expira en : {} h", ttl / 3600);
    if refresh_token.is_empty() {
        println!();
        println!("⚠  Sin refresh_token → cuando el access expire, vuelve a correr");
        println!("   `agent setup minimax-portal` para pegar uno nuevo.");
    } else {
        println!();
        println!("✔ Refresh token presente → el cliente se auto-refresca.");
    }
    println!();
    println!("El cliente `crates/llm/src/minimax.rs` detecta");
    println!(
        "`{}` y usa este token directamente.",
        secrets_dir.join("minimax_portal.json").display()
    );
    Ok(())
}

fn persist_field(
    field: &FieldDef,
    value: &str,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    match &field.target {
        FieldTarget::Secret { file, env_var: _ } => {
            write_secret(secrets_dir, file, value)?;
        }
        FieldTarget::Yaml { file, path } => {
            let yaml_path = config_dir.join(file);
            yaml_patch::upsert(&yaml_path, path, value, field_to_yaml_type(field))?;
        }
        FieldTarget::EnvOnly(_) => {
            // Env-only fields: nothing to persist on disk. The caller
            // prints a summary at the end so the operator can pipe it
            // into their shell rc / systemd EnvFile.
        }
    }
    Ok(())
}

fn field_to_yaml_type(field: &FieldDef) -> yaml_patch::ValueKind {
    use crate::registry::FieldKind;
    match field.kind {
        FieldKind::Bool => yaml_patch::ValueKind::Bool,
        FieldKind::Number => yaml_patch::ValueKind::Number,
        FieldKind::List => yaml_patch::ValueKind::List,
        _ => yaml_patch::ValueKind::String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_secret_creates_0600_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        let path = write_secret(dir, "k.txt", "hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        #[cfg(unix)]
        {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn empty_value_removes_existing_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        let path = write_secret(dir, "k.txt", "hello").unwrap();
        assert!(path.exists());
        write_secret(dir, "k.txt", "").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn write_secret_is_atomic_across_rewrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        write_secret(dir, "k.txt", "v1").unwrap();
        write_secret(dir, "k.txt", "v2").unwrap();
        assert_eq!(fs::read_to_string(dir.join("k.txt")).unwrap(), "v2");
    }

    #[test]
    fn overwrite_backs_up_previous_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ensure_secrets_dir(dir).unwrap();
        write_secret(dir, "k.txt", "first").unwrap();
        write_secret(dir, "k.txt", "second").unwrap();
        assert_eq!(fs::read_to_string(dir.join("k.txt")).unwrap(), "second");
        assert_eq!(fs::read_to_string(dir.join("k.txt.bak")).unwrap(), "first");
    }

    fn anthropic_values(mode: &str, secret: &str) -> ServiceValues {
        let mut v = ServiceValues::default();
        v.insert("auth_mode", mode);
        v.insert("secret_value", secret);
        v
    }

    #[test]
    fn persist_anthropic_api_key_writes_secret_and_patches_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let secrets = tmp.path().join("secrets");
        let conf = tmp.path().join("config");
        ensure_secrets_dir(&secrets).unwrap();
        fs::create_dir_all(&conf).unwrap();
        persist_anthropic(
            &anthropic_values("api_key", "sk-ant-test-1234567890"),
            &secrets,
            &conf,
        )
        .unwrap();
        let written = fs::read_to_string(secrets.join("anthropic_api_key.txt")).unwrap();
        assert_eq!(written, "sk-ant-test-1234567890");
        let yaml = fs::read_to_string(conf.join("llm.yaml")).unwrap();
        assert!(yaml.contains("mode: api_key"), "got: {yaml}");
    }

    #[test]
    fn persist_anthropic_setup_token_validates_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let secrets = tmp.path().join("secrets");
        let conf = tmp.path().join("config");
        ensure_secrets_dir(&secrets).unwrap();
        fs::create_dir_all(&conf).unwrap();
        let err = persist_anthropic(
            &anthropic_values("setup_token", "sk-ant-api03-nope"),
            &secrets,
            &conf,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("sk-ant-oat01-"), "{err}");
    }

    #[test]
    fn persist_anthropic_oauth_bundle_requires_full_json() {
        let tmp = tempfile::tempdir().unwrap();
        let secrets = tmp.path().join("secrets");
        let conf = tmp.path().join("config");
        ensure_secrets_dir(&secrets).unwrap();
        fs::create_dir_all(&conf).unwrap();
        let good = r#"{"access_token":"a","refresh_token":"r","expires_at":1700000000}"#;
        persist_anthropic(&anthropic_values("oauth_bundle", good), &secrets, &conf).unwrap();
        let yaml = fs::read_to_string(conf.join("llm.yaml")).unwrap();
        assert!(yaml.contains("mode: oauth_bundle"));
        assert!(yaml.contains("anthropic_oauth.json"));
    }
}
