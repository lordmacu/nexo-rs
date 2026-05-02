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

pub mod admin_adapters;
pub mod admin_bootstrap;
pub mod admin_capability_collect;
pub mod agent_wizard;
pub mod capabilities;
pub mod http_supervisor;
#[cfg(feature = "config-self-edit")]
pub mod config_tool_bridge;
pub mod credentials_check;
pub mod pairing_check;
pub mod prompt;
pub mod registry;
pub mod services;
pub mod services_imperative;
pub mod skills_migrate;
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

    // Step 3 — Canales. Flujo linear: canal → agente → si ya está
    // vinculado pregunta re-auth; si no está vinculado corre auth +
    // agrega el plugin al agente. El loop deja agregar varios.
    println!();
    println!("▎Paso 3/4 — Canales");
    services::channels_dashboard::run_link_flow(config_dir, secrets_dir)?;

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
    println!(
        "✔ Setup guiado listo. Ejecutá `./target/debug/agent --config {}`",
        config_dir.display()
    );
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

/// Post-first-run menu. Reorganized for clarity: agent-first (the
/// thing the operator actually owns), services as a side branch, and
/// diagnostics isolated. Every label is plain Spanish — no jargon
/// like "rotar credencial" or "vincular canal adicional" in the top
/// level. The legacy service-centric paths still live underneath
/// "Configurar un servicio" → category submenu.
fn run_hub_menu(
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
    _report: &status::StatusReport,
) -> Result<()> {
    loop {
        print_hub_summary(services, secrets_dir, config_dir);
        let actions = [
            "Configurar un agente",
            "Crear un agente nuevo",
            "Conectar un servicio (LLM, canal, skill)",
            "Diagnóstico — ver qué falta",
            "Opciones avanzadas",
            "Salir",
        ];
        let idx = prompt::pick_from_list("¿Qué querés hacer?", &actions)?;
        match idx {
            0 => {
                if let Err(e) = agent_wizard::run_agent_wizard(services, secrets_dir, config_dir) {
                    eprintln!("⚠  configurar agente: {e:#}");
                }
            }
            1 => {
                if let Err(e) = agent_wizard::run_create_agent(config_dir) {
                    eprintln!("⚠  crear agente: {e:#}");
                }
            }
            2 => run_services_submenu(services, secrets_dir, config_dir)?,
            3 => {
                let report = status::audit(services, secrets_dir, config_dir);
                status::print_report(&report);
                if let Ok(summary) = credentials_check::run(config_dir) {
                    credentials_check::print(&summary);
                }
            }
            4 => run_advanced_submenu(services, secrets_dir, config_dir)?,
            _ => {
                if let Ok(summary) = credentials_check::run(config_dir) {
                    credentials_check::print(&summary);
                }
                return Ok(());
            }
        }
    }
}

/// One-screen summary: which agents exist + how many are healthy +
/// how many services are wired. Printed at the top of every hub
/// iteration so the operator always sees the current state before
/// answering the menu.
fn print_hub_summary(services: &[ServiceDef], secrets_dir: &Path, config_dir: &Path) {
    use console::style;
    let agents_yaml = config_dir.join("agents.yaml");
    let agent_ids = yaml_patch::list_agent_ids(&agents_yaml).unwrap_or_default();
    let report = status::audit(services, secrets_dir, config_dir);

    let llm_total = services
        .iter()
        .filter(|s| s.category == registry::Category::Llm)
        .count();
    let llm_ok = report
        .services
        .iter()
        .filter(|s| {
            s.is_fully_configured()
                && services
                    .iter()
                    .find(|sv| sv.id == s.service_id)
                    .map(|sv| sv.category == registry::Category::Llm)
                    .unwrap_or(false)
        })
        .count();
    let plugin_total = services
        .iter()
        .filter(|s| s.category == registry::Category::Plugin)
        .count();
    let plugin_ok = report
        .services
        .iter()
        .filter(|s| {
            s.is_fully_configured()
                && services
                    .iter()
                    .find(|sv| sv.id == s.service_id)
                    .map(|sv| sv.category == registry::Category::Plugin)
                    .unwrap_or(false)
        })
        .count();
    let skill_total = services
        .iter()
        .filter(|s| s.category == registry::Category::Skill)
        .count();
    let skill_ok = report
        .services
        .iter()
        .filter(|s| {
            s.is_fully_configured()
                && services
                    .iter()
                    .find(|sv| sv.id == s.service_id)
                    .map(|sv| sv.category == registry::Category::Skill)
                    .unwrap_or(false)
        })
        .count();

    println!();
    println!("─────────────────────────────────────────────");
    if agent_ids.is_empty() {
        println!("  Agentes:    {}", style("(ninguno)").dim());
    } else {
        println!(
            "  Agentes:    {} ({})",
            agent_ids.len(),
            agent_ids.join(", ")
        );
    }
    println!(
        "  Servicios:  {} LLM · {} canales · {} skills",
        style(format!("{llm_ok}/{llm_total}")).bold(),
        style(format!("{plugin_ok}/{plugin_total}")).bold(),
        style(format!("{skill_ok}/{skill_total}")).bold(),
    );
    println!("─────────────────────────────────────────────");
}

/// "Conectar un servicio" submenu — collapses the old hub options
/// 2 (canal), 3 (skill), 5 (LLM), 6 (rotar) into a single category
/// picker. Each branch keeps its existing flow.
fn run_services_submenu(
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    loop {
        let actions = [
            "Cerebro (LLM — minimax / anthropic / openai / …)",
            "Canal de mensajería (whatsapp / telegram / email)",
            "Habilidad / skill (clima, búsquedas, ssh, docker, …)",
            "← volver",
        ];
        let pick = prompt::pick_from_list("¿Qué tipo de servicio?", &actions)?;
        match pick {
            0 => pick_and_run_in_category(
                services,
                registry::Category::Llm,
                secrets_dir,
                config_dir,
            )?,
            1 => {
                // Channel link flow already shows auth + bind in one
                // wizard — keep using it instead of re-rolling here.
                if let Err(e) = services::channels_dashboard::run_link_flow(config_dir, secrets_dir)
                {
                    eprintln!("⚠  canal: {e:#}");
                }
            }
            2 => pick_and_run_in_category(
                services,
                registry::Category::Skill,
                secrets_dir,
                config_dir,
            )?,
            _ => return Ok(()),
        }
    }
}

/// "Opciones avanzadas" — old hub-7 (memory/broker/runtime) plus the
/// rotate-credential path for power users who want to swap a key
/// without re-auth-ing the whole flow.
fn run_advanced_submenu(
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    loop {
        let actions = [
            "Memoria / embeddings",
            "Broker NATS",
            "Runtime (timeouts, queues)",
            "Cambiar credencial existente (rotar key)",
            "← volver",
        ];
        let pick = prompt::pick_from_list("Avanzado", &actions)?;
        match pick {
            0 => pick_and_run_in_category(
                services,
                registry::Category::Memory,
                secrets_dir,
                config_dir,
            )?,
            1 => pick_and_run_in_category(
                services,
                registry::Category::Infra,
                secrets_dir,
                config_dir,
            )?,
            2 => pick_and_run_in_category(
                services,
                registry::Category::Runtime,
                secrets_dir,
                config_dir,
            )?,
            3 => {
                let report = status::audit(services, secrets_dir, config_dir);
                let rotatable: Vec<&ServiceDef> = services
                    .iter()
                    .filter(|s| {
                        matches!(
                            s.category,
                            registry::Category::Plugin
                                | registry::Category::Skill
                                | registry::Category::Llm
                        )
                    })
                    .collect();
                let mut labels: Vec<String> = vec!["← volver".to_string()];
                labels.extend(rotatable.iter().map(|s| labeled_with_status(s, &report)));
                let pick = prompt::pick_from_strings("¿Qué credencial rotar?", &labels)?;
                if pick == 0 {
                    continue;
                }
                run_service(rotatable[pick - 1], secrets_dir, config_dir)?;
            }
            _ => return Ok(()),
        }
    }
}

/// Pick one service inside a single category and run its form. Shared
/// helper for the LLM / skill / memory / infra / runtime entries.
fn pick_and_run_in_category(
    services: &[ServiceDef],
    cat: registry::Category,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let report = status::audit(services, secrets_dir, config_dir);
    let in_cat: Vec<&ServiceDef> = services.iter().filter(|s| s.category == cat).collect();
    if in_cat.is_empty() {
        println!("(no hay servicios en esta categoría)");
        return Ok(());
    }
    let mut labels: Vec<String> = vec!["← volver".to_string()];
    labels.extend(in_cat.iter().map(|s| labeled_with_status(s, &report)));
    let pick = prompt::pick_from_strings("¿Cuál?", &labels)?;
    if pick == 0 {
        return Ok(());
    }
    run_service(in_cat[pick - 1], secrets_dir, config_dir)?;
    Ok(())
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
/// per-instance allowlist scoped to `agent_id` (when provided).
pub fn run_telegram_link(config_dir: &Path, agent_id: Option<&str>) -> Result<()> {
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
            handle.block_on(telegram_link::run(&secrets_dir, config_dir, agent_id))
        }),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(telegram_link::run(&secrets_dir, config_dir, agent_id))
        }
    }
}

/// Phase C4.c — render the latest LLM quota events per provider.
/// Reads the process-wide cache from
/// `nexo_llm::rate_limit_info::last_quota_events_all`. In a fresh
/// `setup doctor` invocation the cache is empty (no LLM calls have
/// happened in this process); we still render a "no recent events"
/// line so operators discover the surface and so the renderer is
/// exercised for a future runtime-attached doctor variant.
fn print_llm_quota_section() {
    use nexo_llm::rate_limit_info::{last_quota_events_all, RateLimitSeverity};

    println!("\n--- LLM quota ---");
    let events = last_quota_events_all();
    if events.is_empty() {
        println!("  no recent quota events");
        return;
    }
    let now = chrono::Utc::now();
    for ev in events {
        let age_min = (now - ev.at).num_minutes().max(0);
        let icon = match ev.severity {
            RateLimitSeverity::Error => "!",
            RateLimitSeverity::Warning => ".",
        };
        println!(
            "  [{icon}] {:?} ({} min ago): {}",
            ev.provider, age_min, ev.message
        );
        if let Some(hint) = ev.plan_hint {
            println!("      hint: {hint}");
        }
    }
}

/// Non-interactive audit — exits with non-zero when any required field
/// is missing OR the credential gauntlet reports errors. Async since
/// Phase 70.6 added a sqlx-backed pairing-store check; callers from
/// the binary's `#[tokio::main]` await it directly.
pub async fn run_doctor(config_dir: &Path) -> Result<()> {
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

    // Phase 70.6 — pairing-store audit. Flags bindings that have
    // `pairing.auto_challenge: true` but an empty allowlist, so the
    // operator notices before the gate silently drops their first
    // message. Non-fatal: doctor already returns the missing-fields
    // status; this is informational.
    let pairing_findings = pairing_check::audit(config_dir).await;
    pairing_check::print(&pairing_findings);

    // Phase C4.c — surface the most recent LLM quota rejections per
    // provider. Reads the process-wide cache populated by
    // `nexo_llm::retry::classify_429_error`. In `setup doctor`
    // invocations the cache is always empty (fresh process) — but
    // the section is rendered anyway so operators learn the surface
    // exists, and so future flows that pre-warm the cache (e.g. a
    // runtime-side `nexo doctor` that talks to the daemon) get a
    // ready-to-use renderer.
    print_llm_quota_section();

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
            // Legacy global-service flow: no agent context. The link
            // helper falls back to the lone instance; multi-bot setups
            // must use `setup → Configurar agente` (channels_dashboard)
            // which routes through the per-agent flow.
            if let Err(e) = run_telegram_link(config_dir, None) {
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

/// Best-effort hot-reload trigger. Spawns `nexo --config <dir> reload`
/// and ignores the result so the wizard stays usable when the daemon
/// isn't running, the binary isn't on `PATH`, or the IPC fails. The
/// agent-wizard handlers call this after every successful mutation so
/// a running daemon picks up the YAML edit without a manual restart.
pub(crate) fn try_hot_reload(config_dir: &Path) {
    use std::process::{Command, Stdio};
    let _ = Command::new("nexo")
        .arg("--config")
        .arg(config_dir)
        .arg("reload")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Public shim for `run_service` so sibling modules (`agent_wizard`)
/// can fall back to the declarative form when an LLM provider or
/// skill needs credentials before it's safe to attach.
pub(crate) fn run_service_pub(
    svc: &ServiceDef,
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    run_service(svc, secrets_dir, config_dir)
}
