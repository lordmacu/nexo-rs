//! Interactive setup wizard for the agent framework.
//!
//! The wizard knows every service the framework can consume credentials
//! for, prompts the operator for each required field, and persists them
//! in the correct shape:
//!
//! * **Secrets** land under `secrets/<id>.txt` with mode `0600` and are
//!   referenced from YAML via `${file:/run/secrets/<id>}` (Docker) or
//!   `${<ENV_VAR>}` + the operator's own `export` / systemd EnvFile.
//! * **Non-secret config** (allow-lists, hosts, toggles) is patched in
//!   place into the corresponding `config/*.yaml` preserving comments
//!   where possible.
//!
//! Entry points:
//!
//! * [`run_interactive`] — full menu loop
//! * [`run_one`] — jump straight to a single service by id
//! * [`run_doctor`] — non-interactive audit of every configured secret
//! * [`run_list`] — print the service catalog + status
//!
//! See `src/services/` for the exhaustive service catalog.

pub mod prompt;
pub mod registry;
pub mod services;
pub mod status;
pub mod telegram_link;
pub mod writer;
pub mod yaml_patch;

use std::path::Path;

use anyhow::Result;

pub use registry::{Category, FieldDef, FieldKind, FieldTarget, ServiceDef, ServiceValues};
pub use status::{ServiceStatus, StatusReport};

/// Run the interactive menu loop: pick a service, fill fields, persist,
/// repeat until the user exits.
pub fn run_interactive(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir.parent().unwrap_or(Path::new(".")).join("secrets");
    writer::ensure_secrets_dir(&secrets_dir)?;
    let services = services::all();
    let picked = prompt::select_services(&services, &secrets_dir, config_dir)?;
    if picked.is_empty() {
        println!("No se seleccionó nada. Bye.");
        return Ok(());
    }
    let mut failures: Vec<(String, anyhow::Error)> = Vec::new();
    for svc in picked {
        if let Err(e) = run_service(svc, &secrets_dir, config_dir) {
            eprintln!("⚠  {}: {e:#}", svc.label);
            failures.push((svc.label.to_string(), e));
        }
    }
    println!();
    if failures.is_empty() {
        println!("Listo. `agent setup list` para ver el estado global.");
        Ok(())
    } else {
        // Non-zero exit so scripted runs can detect the partial failure;
        // the operator already saw per-service output above and the list
        // command can confirm what stuck.
        anyhow::bail!(
            "{} servicio(s) con error: {}. Corre `agent setup list` para ver el estado.",
            failures.len(),
            failures
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Run the wizard for a single service id (non-menu path).
pub fn run_one(config_dir: &Path, service_id: &str) -> Result<()> {
    let secrets_dir = config_dir.parent().unwrap_or(Path::new(".")).join("secrets");
    writer::ensure_secrets_dir(&secrets_dir)?;
    let services = services::all();
    let svc = services
        .iter()
        .find(|s| s.id == service_id)
        .ok_or_else(|| anyhow::anyhow!(
            "unknown service '{service_id}'. Run `agent setup --list` for the catalog.",
        ))?;
    run_service(svc, &secrets_dir, config_dir)
}

/// Print the catalog + per-service configuration status to stdout.
pub fn run_list(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir.parent().unwrap_or(Path::new(".")).join("secrets");
    let services = services::all();
    let report = status::audit(&services, &secrets_dir, config_dir);
    status::print_report(&report);
    Ok(())
}

/// Run the Telegram link flow: validate the persisted bot token, long-
/// poll for the user's first message, append their chat_id to the
/// allowlist.
pub fn run_telegram_link(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir.parent().unwrap_or(Path::new(".")).join("secrets");
    // Detect if we're being called from within an existing tokio
    // runtime (interactive setup is launched from `#[tokio::main]`)
    // vs a fresh sync context (standalone `agent setup telegram-link`).
    // Nested runtimes panic; `block_in_place` hands this thread back
    // to the existing scheduler while the blocking call runs.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(telegram_link::run(&secrets_dir, config_dir))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(telegram_link::run(&secrets_dir, config_dir))
        }
    }
}

/// Non-interactive audit — exits with non-zero when any required field
/// is missing.
pub fn run_doctor(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir.parent().unwrap_or(Path::new(".")).join("secrets");
    let services = services::all();
    let report = status::audit(&services, &secrets_dir, config_dir);
    let missing = report.missing_required();
    status::print_report(&report);
    if missing.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} required field(s) missing: {}",
            missing.len(),
            missing.join(", ")
        )
    }
}

fn run_service(svc: &ServiceDef, secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    println!();
    println!("── {} ─────────────────────────────────", svc.label);
    if let Some(desc) = svc.description {
        println!("{desc}");
    }
    let values = prompt::run_form(svc, secrets_dir, config_dir)?;
    if values.is_none() {
        println!("Cancelled.");
        return Ok(());
    }
    let values = values.unwrap();
    writer::persist(svc, &values, secrets_dir, config_dir)?;
    println!("✔ {} configured.", svc.label);

    // Telegram: offer to resolve the operator's chat_id right after
    // the token is saved. Vinculación is two-step (token + allowlist)
    // so chaining them here saves a context switch.
    if svc.id == "telegram" && should_offer_telegram_link() {
        println!();
        if prompt::yes_no(
            "¿Vincular tu chat_id ahora? Te pediré escribirle un mensaje al bot.",
            true,
        )? {
            if let Err(e) = run_telegram_link(config_dir) {
                eprintln!("⚠  telegram-link falló: {e}");
                eprintln!("   Puedes reintentar con: agent setup telegram-link");
            }
        }
    }
    Ok(())
}

fn should_offer_telegram_link() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}
