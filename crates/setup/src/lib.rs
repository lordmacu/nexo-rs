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

pub mod capabilities;
pub mod credentials_check;
pub mod prompt;
pub mod registry;
pub mod services;
pub mod services_imperative;
pub mod status;
pub mod telegram_link;
pub mod writer;
pub mod yaml_patch;

use std::path::Path;

use anyhow::Result;

pub use registry::{Category, FieldDef, FieldKind, FieldTarget, ServiceDef, ServiceValues};
pub use status::{ServiceStatus, StatusReport};

/// Interactive entry point. Routes to a guided first-run wizard when
/// the system looks empty; otherwise drops the operator at a "what do
/// you want to do?" hub. Both paths delegate to the same per-service
/// handlers below.
pub fn run_interactive(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("secrets");
    writer::ensure_secrets_dir(&secrets_dir)?;
    let services = services::all();
    let report = status::audit(&services, &secrets_dir, config_dir);
    if is_first_run(&report) {
        run_guided_first_run(&services, &secrets_dir, config_dir, &report)
    } else {
        run_hub_menu(&services, &secrets_dir, config_dir, &report)
    }
}

/// "Nothing is wired yet" heuristic: no LLM provider has all required
/// fields filled. Everything else (memory, skills, channels) is
/// meaningful only after an LLM is up, so that's the gate.
fn is_first_run(report: &status::StatusReport) -> bool {
    !report
        .services
        .iter()
        .any(|s| s.category == registry::Category::Llm && s.is_fully_configured())
}

/// Linear 4-step wizard for a cold install: agent → LLM → channel →
/// skills. Each step can be escaped (esc) to drop into the hub menu.
fn run_guided_first_run(
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
    _report: &status::StatusReport,
) -> Result<()> {
    println!();
    println!("╭────────────────────────────────────────────────────────────╮");
    println!("│  Setup guiado — primera corrida                            │");
    println!("│  4 pasos: agente → LLM → canal → skills                    │");
    println!("╰────────────────────────────────────────────────────────────╯");

    // Step 1 — agent context. Single-agent installs auto-confirm; the
    // YAML is already seeded by git so we don't create agents here.
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    if agent_ids.is_empty() {
        anyhow::bail!(
            "agents.yaml sin agentes — editá el archivo o clonás el template antes del setup"
        );
    }
    let agent_id = prompt::pick_agent(&agent_ids)?;
    println!();
    println!("▎Paso 1/4 — Agente: `{agent_id}` ✔");

    // Step 2 — LLM. First-run cannot continue without at least one.
    println!();
    println!("▎Paso 2/4 — LLM");
    let llm_services: Vec<&ServiceDef> = services
        .iter()
        .filter(|s| s.category == registry::Category::Llm)
        .collect();
    let llm_labels: Vec<&str> = llm_services.iter().map(|s| s.label).collect();
    let idx = prompt::pick_from_list("¿Qué proveedor de LLM?", &llm_labels)?;
    let svc = llm_services[idx];
    run_service(svc, secrets_dir, config_dir)?;

    // Step 3 — Channel. `link` handles the inline pairing.
    println!();
    println!("▎Paso 3/4 — Canal");
    if let Some(link_svc) = services.iter().find(|s| s.id == "link") {
        run_service(link_svc, secrets_dir, config_dir)?;
    }

    // Step 4 — Skills (optional, pre-selects common ones).
    println!();
    println!("▎Paso 4/4 — Skills (Enter salta)");
    let skill_services: Vec<&ServiceDef> = services
        .iter()
        .filter(|s| s.category == registry::Category::Skill)
        .collect();
    let skill_labels: Vec<&str> = skill_services.iter().map(|s| s.label).collect();
    let defaults: Vec<bool> = skill_services
        .iter()
        .map(|s| {
            matches!(
                s.id,
                "weather" | "summarize" | "openstreetmap" | "goplaces" | "browser"
            )
        })
        .collect();
    let picked = prompt::multi_select(
        "Elegí skills (space = toggle, enter = confirmar, vacío = ninguno)",
        &skill_labels,
        &defaults,
    )?;
    for label in &picked {
        if let Some(svc) = skill_services.iter().find(|s| s.label == label) {
            if let Err(e) = run_service(svc, secrets_dir, config_dir) {
                eprintln!("⚠  {}: {e:#}", svc.label);
            }
        }
    }

    // Phase 17 — gauntlet at wizard close. Surfaces any
    // `credentials.<ch>` binding that points at a non-existent
    // instance / account before the user boots the daemon.
    if let Ok(summary) = credentials_check::run(config_dir) {
        credentials_check::print(&summary);
    }

    println!();
    println!("✔ Setup guiado listo. Ejecutá `./target/debug/agent --config {}`", config_dir.display());
    Ok(())
}

/// Decorate a service label with a status circle based on the audit
/// report. Green ● = fully configured, yellow ● = partial,
/// red ● = nothing. Returns an owned String since `console::style`
/// injects ANSI codes.
fn labeled_with_status(svc: &ServiceDef, report: &status::StatusReport) -> String {
    let st = report.services.iter().find(|s| s.service_id == svc.id);
    let dot = match st {
        Some(s) if s.is_fully_configured() => console::style("●").green(),
        Some(s) if s.is_partially_configured() => console::style("●").yellow(),
        _ => console::style("●").red(),
    };
    format!("{dot} {}", svc.label)
}

/// Post-first-run menu. Groups follow-up actions by intent rather than
/// by category. "Avanzado" falls through to the legacy multi-select
/// for power users.
fn run_hub_menu(
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
    _report: &status::StatusReport,
) -> Result<()> {
    let actions = [
        "Ver estado (qué está configurado)",
        "Vincular canal adicional (whatsapp / telegram / …)",
        "Agregar skills",
        "LLM (agregar / cambiar / re-autenticar proveedor)",
        "Rotar credencial (plugin / skill — no LLMs)",
        "Avanzado ⚙  (memory, broker, runtime, per-service)",
        "Salir",
    ];
    loop {
        println!();
        let idx = prompt::pick_from_list("¿Qué querés hacer?", &actions)?;
        match idx {
            0 => {
                let report = status::audit(services, secrets_dir, config_dir);
                status::print_report(&report);
            }
            1 => {
                if let Some(link) = services.iter().find(|s| s.id == "link") {
                    if let Err(e) = run_service(link, secrets_dir, config_dir) {
                        eprintln!("⚠  {}: {e:#}", link.label);
                    }
                }
            }
            2 => {
                let report = status::audit(services, secrets_dir, config_dir);
                let skill_services: Vec<&ServiceDef> = services
                    .iter()
                    .filter(|s| s.category == registry::Category::Skill)
                    .collect();
                let labels: Vec<String> = skill_services
                    .iter()
                    .map(|s| labeled_with_status(s, &report))
                    .collect();
                let defaults = vec![false; labels.len()];
                let idxs = prompt::multi_select_strings(
                    "Skills (space = toggle, enter = confirmar, ESC = volver)",
                    &labels,
                    &defaults,
                )?;
                for i in idxs {
                    let svc = skill_services[i];
                    if let Err(e) = run_service(svc, secrets_dir, config_dir) {
                        eprintln!("⚠  {}: {e:#}", svc.label);
                    }
                }
            }
            3 => {
                // Dedicated LLM path: pick provider → run its service
                // (covers add / swap / re-auth uniformly). Each entry
                // gets a colored status dot — green = fully configured,
                // yellow = partial, red = none.
                let report = status::audit(services, secrets_dir, config_dir);
                let llm_services: Vec<&ServiceDef> = services
                    .iter()
                    .filter(|s| s.category == registry::Category::Llm)
                    .collect();
                let mut labels: Vec<String> = vec!["← Volver al menú".to_string()];
                labels.extend(
                    llm_services
                        .iter()
                        .map(|s| labeled_with_status(s, &report)),
                );
                let pick = prompt::pick_from_strings("¿Qué proveedor de LLM?", &labels)?;
                if pick == 0 {
                    continue;
                }
                run_service(llm_services[pick - 1], secrets_dir, config_dir)?;
            }
            4 => {
                // Non-LLM credential rotation: plugin + skill services.
                let report = status::audit(services, secrets_dir, config_dir);
                let rotatable: Vec<&ServiceDef> = services
                    .iter()
                    .filter(|s| {
                        matches!(
                            s.category,
                            registry::Category::Plugin | registry::Category::Skill
                        )
                    })
                    .collect();
                let mut labels: Vec<String> = vec!["← Volver al menú".to_string()];
                labels.extend(
                    rotatable.iter().map(|s| labeled_with_status(s, &report)),
                );
                let pick = prompt::pick_from_strings("¿Qué servicio rotar?", &labels)?;
                if pick == 0 {
                    continue;
                }
                run_service(rotatable[pick - 1], secrets_dir, config_dir)?;
            }
            5 => {
                // Legacy advanced path — reuse the old multi-category
                // picker scoped to the technical bits.
                let advanced_services: Vec<ServiceDef> = services
                    .iter()
                    .filter(|s| {
                        matches!(
                            s.category,
                            registry::Category::Memory
                                | registry::Category::Infra
                                | registry::Category::Runtime
                        )
                    })
                    .cloned()
                    .collect();
                let picked =
                    prompt::select_services(&advanced_services, secrets_dir, config_dir)?;
                for svc in picked {
                    if let Err(e) = run_service(svc, secrets_dir, config_dir) {
                        eprintln!("⚠  {}: {e:#}", svc.label);
                    }
                }
            }
            _ => {
                // Phase 17 — final gauntlet before leaving the hub.
                if let Ok(summary) = credentials_check::run(config_dir) {
                    credentials_check::print(&summary);
                }
                return Ok(());
            }
        }
    }
}

/// Run the wizard for a single service id (non-menu path).
pub fn run_one(config_dir: &Path, service_id: &str) -> Result<()> {
    let secrets_dir = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("secrets");
    writer::ensure_secrets_dir(&secrets_dir)?;
    let services = services::all();
    let svc = services
        .iter()
        .find(|s| s.id == service_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown service '{service_id}'. Run `agent setup --list` for the catalog.",
            )
        })?;
    run_service(svc, &secrets_dir, config_dir)?;
    // Phase 17 — always close a setup run with the gauntlet so the
    // user sees stale `credentials` bindings immediately.
    if matches!(
        svc.id,
        "whatsapp" | "telegram" | "google" | "google-auth" | "gmail-poller"
    ) {
        if let Ok(summary) = credentials_check::run(config_dir) {
            credentials_check::print(&summary);
        }
    }
    Ok(())
}

/// Print the catalog + per-service configuration status to stdout.
pub fn run_list(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("secrets");
    let services = services::all();
    let report = status::audit(&services, &secrets_dir, config_dir);
    status::print_report(&report);
    Ok(())
}

/// Run the Telegram link flow: validate the persisted bot token, long-
/// poll for the user's first message, append their chat_id to the
/// allowlist.
pub fn run_telegram_link(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("secrets");
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
/// is missing OR the credential gauntlet reports errors.
pub fn run_doctor(config_dir: &Path) -> Result<()> {
    let secrets_dir = config_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join("secrets");
    let services = services::all();
    let report = status::audit(&services, &secrets_dir, config_dir);
    let missing = report.missing_required();
    status::print_report(&report);

    // Phase 17 — credential gauntlet on top of the per-service audit.
    // Non-fatal on missing optional services but surfaces binding /
    // allow_agents mismatches before the daemon tries to boot.
    let cred_summary = credentials_check::run(config_dir).ok();
    if let Some(ref s) = cred_summary {
        credentials_check::print(s);
    }

    let cred_errors = cred_summary.as_ref().map(|s| s.errors.len()).unwrap_or(0);
    if missing.is_empty() && cred_errors == 0 {
        Ok(())
    } else if cred_errors > 0 {
        anyhow::bail!(
            "{} required field(s) missing + {} credential error(s); fix and re-run `agent setup doctor`",
            missing.len(),
            cred_errors
        )
    } else {
        anyhow::bail!(
            "{} required field(s) missing: {}",
            missing.len(),
            missing.join(", ")
        )
    }
}

fn run_service(svc: &ServiceDef, secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    // Phase 17 — hijack WhatsApp / Telegram / Google so they prompt
    // for `instance`, allow_agents, and auto-write the `credentials:`
    // block on the chosen agent. The declarative form cannot express
    // array entries + cross-file patches.
    match services_imperative::dispatch(svc.id, config_dir, secrets_dir)? {
        services_imperative::Outcome::Handled => {
            if let Ok(summary) = credentials_check::run(config_dir) {
                credentials_check::print(&summary);
            }
            return Ok(());
        }
        services_imperative::Outcome::NotHandled => {}
    }

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
