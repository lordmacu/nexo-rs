//! Per-agent setup submenu.
//!
//! Where the hub menu groups actions by *service* (Telegram, OpenAI,
//! browser plugin), the agent wizard groups them by *agent*: the
//! operator picks one agent up front, then sees a single dashboard
//! covering that agent's model, language, channels, and skills, and
//! mutates each from one place. Every action reuses the existing
//! channel / LLM / skill flows already shipped in
//! `services_imperative.rs`, `channels_dashboard.rs`, and the
//! declarative `ServiceDef` pipeline — this module just orchestrates
//! the selection + persistence so we don't grow a parallel codebase.
//!
//! Persistence rules:
//!   * `model.provider` + `model.model` → `agents[<id>].model`
//!   * `language`                       → `agents[<id>].language`
//!   * channel bind                    → `agents[<id>].plugins[]` +
//!                                        `agents[<id>].inbound_bindings[]`
//!   * skill toggle                     → `agents[<id>].skills[]`
//!
//! Every successful mutation triggers a best-effort `nexo … reload`
//! so a running daemon picks up the change without a restart.

use std::path::Path;

use anyhow::{Context, Result};
use serde_yaml::{Mapping, Value};

use crate::prompt;
use crate::registry::{Category, FieldKind, FieldTarget, ServiceDef};
use crate::services::channels_dashboard;
use crate::services_imperative;
use crate::status;
use crate::yaml_patch;

/// Provider id → list of suggested model names. An empty list means
/// the operator should type the model name freely (e.g. for the
/// generic `openai_custom` slot which fronts Groq, OpenRouter, Ollama,
/// LM Studio, vLLM, … with arbitrary model names).
pub const MODEL_CATALOG: &[(&str, &[&str])] = &[
    (
        "minimax",
        &["MiniMax-M2.5", "MiniMax-M2", "MiniMax-Text-01"],
    ),
    (
        "anthropic",
        &["claude-haiku-4-5", "claude-sonnet-4-5", "claude-opus-4-5"],
    ),
    (
        "openai",
        &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4.1-mini",
            "o1-mini",
        ],
    ),
    ("deepseek", &["deepseek-chat", "deepseek-reasoner"]),
    ("openai_custom", &[]),
];

#[derive(Debug, Clone)]
pub enum ModelStatus {
    None,
    Attached {
        provider: String,
        model: String,
        creds_ok: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub plugin: String,
    pub instance: Option<String>,
    pub auth_ok: bool,
    pub bound: bool,
}

#[derive(Debug, Clone)]
pub struct AgentDashboard {
    pub id: String,
    pub model: ModelStatus,
    pub language: Option<String>,
    pub channels: Vec<ChannelStatus>,
    pub skills_attached: usize,
    pub skills_total: usize,
    /// `allowed_tools` is a wildcard AND at least one inbound binding has
    /// `dispatch_policy.mode: full`. Means the agent can launch
    /// Phase 67 Claude Code goals from a paired channel.
    pub coding_enabled: bool,
    pub heartbeat_enabled: bool,
}

/// Create a new agent by cloning an existing one as a template.
/// Asks for the new id, copies the template's YAML body into a fresh
/// `agents.d/<id>.yaml`, and rewrites the top-level `id:` field.
/// Skips files that already exist to avoid clobbering an in-flight
/// agent. The new file gets a minimal post-processing pass:
///   * `id:` rewritten to the chosen value
///   * `workspace:` rebased on `./data/workspace/<id>` if present
///   * `transcripts_dir:` rebased on `./data/transcripts/<id>` if
///     present
/// Everything else is copied verbatim — operator runs the regular
/// agent wizard afterwards to finish wiring.
pub fn run_create_agent(config_dir: &Path) -> Result<()> {
    let existing = yaml_patch::list_agent_ids(&config_dir.join("agents.yaml")).unwrap_or_default();
    if existing.is_empty() {
        anyhow::bail!("no existing agents to clone — editá agents.yaml a mano");
    }

    println!();
    println!("Crear agente nuevo a partir de un template existente.");
    let template = prompt::pick_agent(&existing)?;

    use dialoguer::{theme::ColorfulTheme, Input};
    let theme = ColorfulTheme::default();
    let new_id: String = Input::with_theme(&theme)
        .with_prompt("ID del agente nuevo (sólo letras / números / -_)")
        .validate_with(|s: &String| -> Result<(), String> {
            let s = s.trim();
            if s.is_empty() {
                return Err("vacío".into());
            }
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                Ok(())
            } else {
                Err("sólo a-z / A-Z / 0-9 / - / _".into())
            }
        })
        .interact_text()?;
    let new_id = new_id.trim().to_string();

    if existing.iter().any(|id| id == &new_id) {
        anyhow::bail!("ya existe un agente con id `{new_id}`");
    }

    // Locate the template file. Same precedence as `find_agent_file`:
    // `agents.yaml` first, then `agents.d/*.yaml`.
    let template_file = yaml_patch::find_agent_file(config_dir, &template)?
        .ok_or_else(|| anyhow::anyhow!("template `{template}` not found"))?;

    let text = std::fs::read_to_string(&template_file)
        .with_context(|| format!("read {}", template_file.display()))?;
    let mut root: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("parse {}", template_file.display()))?;
    let agents = root
        .get_mut("agents")
        .and_then(serde_yaml::Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in template"))?;
    let template_item = agents
        .iter()
        .find(|it| it.get("id").and_then(serde_yaml::Value::as_str) == Some(template.as_str()))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("template entry not found in {}", template_file.display())
        })?;

    let mut new_item = template_item;
    if let Some(map) = new_item.as_mapping_mut() {
        map.insert(
            serde_yaml::Value::String("id".into()),
            serde_yaml::Value::String(new_id.clone()),
        );
        if map.contains_key(serde_yaml::Value::String("workspace".into())) {
            map.insert(
                serde_yaml::Value::String("workspace".into()),
                serde_yaml::Value::String(format!("./data/workspace/{new_id}")),
            );
        }
        if map.contains_key(serde_yaml::Value::String("transcripts_dir".into())) {
            map.insert(
                serde_yaml::Value::String("transcripts_dir".into()),
                serde_yaml::Value::String(format!("./data/transcripts/{new_id}")),
            );
        }
    }

    let mut new_root = serde_yaml::Mapping::new();
    new_root.insert(
        serde_yaml::Value::String("agents".into()),
        serde_yaml::Value::Sequence(vec![new_item]),
    );
    let yaml_text = serde_yaml::to_string(&serde_yaml::Value::Mapping(new_root))?;

    let drop_dir = config_dir.join("agents.d");
    std::fs::create_dir_all(&drop_dir).with_context(|| format!("mkdir {}", drop_dir.display()))?;
    let new_file = drop_dir.join(format!("{new_id}.yaml"));
    if new_file.exists() {
        anyhow::bail!("{} ya existe", new_file.display());
    }
    std::fs::write(&new_file, yaml_text)
        .with_context(|| format!("write {}", new_file.display()))?;

    println!();
    println!(
        "✔ Agente `{new_id}` creado en {} (clon de `{template}`).",
        new_file.display()
    );
    println!(
        "  Próximo paso: \"Configurar un agente\" → `{new_id}` para ajustarle modelo/canales/skills."
    );
    crate::try_hot_reload(config_dir);
    Ok(())
}

/// Public entry point for the per-agent wizard. Lists every agent the
/// runtime can see, lets the operator pick one (single agent →
/// auto-pick), then loops on the dashboard.
pub fn run_agent_wizard(
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let agents_yaml = config_dir.join("agents.yaml");
    let ids = yaml_patch::list_agent_ids(&agents_yaml)
        .with_context(|| format!("listing agents in {}", agents_yaml.display()))?;
    if ids.is_empty() {
        anyhow::bail!(
            "agents.yaml sin agentes — editá el archivo o clonás el template antes del setup"
        );
    }
    let agent_id = if ids.len() == 1 {
        println!();
        println!("(único agente disponible: `{}`)", ids[0]);
        ids[0].clone()
    } else {
        prompt::pick_agent(&ids)?
    };

    loop {
        let dashboard = compute_dashboard(&agent_id, services, secrets_dir, config_dir)?;
        print_dashboard(&dashboard);
        let actions = [
            "Cerebro (qué LLM usa)",
            "Idioma de respuesta",
            "Canales (dónde recibe mensajes)",
            "Habilidades / skills (qué sabe hacer)",
            "Permisos (programar, heartbeat)",
            "← volver",
        ];
        let pick = prompt::pick_from_list("Acción", &actions)?;
        match pick {
            0 => {
                if let Err(e) = handle_model(&agent_id, services, secrets_dir, config_dir) {
                    eprintln!("⚠  modelo: {e:#}");
                }
            }
            1 => {
                if let Err(e) = handle_language(&agent_id, config_dir) {
                    eprintln!("⚠  idioma: {e:#}");
                }
            }
            2 => {
                if let Err(e) = handle_channels(&agent_id, secrets_dir, config_dir) {
                    eprintln!("⚠  canales: {e:#}");
                }
            }
            3 => {
                if let Err(e) = handle_skills(&agent_id, services, secrets_dir, config_dir) {
                    eprintln!("⚠  skills: {e:#}");
                }
            }
            4 => {
                if let Err(e) = handle_capabilities(&agent_id, config_dir) {
                    eprintln!("⚠  capacidades: {e:#}");
                }
            }
            _ => return Ok(()),
        }
    }
}

/// Build the dashboard struct read straight from disk. Pure function:
/// no prompts, no mutations.
pub fn compute_dashboard(
    agent_id: &str,
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<AgentDashboard> {
    let agent_file = yaml_patch::find_agent_file(config_dir, agent_id)?
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;

    // Model.
    let provider = yaml_patch::read_agent_field(&agent_file, agent_id, "model.provider")?
        .and_then(|v| v.as_str().map(str::to_string));
    let model_name = yaml_patch::read_agent_field(&agent_file, agent_id, "model.model")?
        .and_then(|v| v.as_str().map(str::to_string));
    let model = match (provider, model_name) {
        (Some(p), Some(m)) => {
            let creds_ok = provider_creds_ok(&p, services, secrets_dir, config_dir);
            ModelStatus::Attached {
                provider: p,
                model: m,
                creds_ok,
            }
        }
        _ => ModelStatus::None,
    };

    // Language.
    let language = yaml_patch::read_agent_field(&agent_file, agent_id, "language")?
        .and_then(|v| v.as_str().map(str::to_string));

    // Channels — start from `channels_dashboard::detect_channels` so
    // we share the same auth heuristics as the channels submenu.
    let detected = channels_dashboard::detect_channels(config_dir, secrets_dir)?;
    let mut channels: Vec<ChannelStatus> = Vec::new();
    for entry in &detected {
        // Rely on `bound_agents` only. The previous fallback
        // (matching the agent's `plugins:` list against `entry.channel`)
        // marked every instance of a multi-instance plugin as bound —
        // whatsapp:kate showed up bound to cody just because cody had
        // `plugins: [whatsapp]`. `bound_agents` already takes per-
        // instance allow_lists into account.
        let bound = entry.bound_agents.iter().any(|a| a == agent_id);
        // Hide instances that already belong to a different agent —
        // operator selected this agent, so showing kate's whatsapp
        // session under cody's view is just noise.
        let owned_by_other = !bound
            && !entry.bound_agents.is_empty()
            && entry.bound_agents.iter().all(|a| a != agent_id);
        if owned_by_other {
            continue;
        }
        let instance = if entry.instance.is_empty() || entry.instance == "default" {
            None
        } else {
            Some(entry.instance.clone())
        };
        channels.push(ChannelStatus {
            plugin: entry.channel.clone(),
            instance,
            auth_ok: matches!(entry.auth, channels_dashboard::AuthState::Authenticated),
            bound,
        });
    }

    // Skills.
    let skill_services: Vec<&ServiceDef> = services
        .iter()
        .filter(|s| s.category == Category::Skill)
        .collect();
    let attached: Vec<String> = yaml_patch::get_agent_list(&agent_file, agent_id, "skills")?;
    let skills_attached = attached.len();
    let skills_total = skill_services.len();

    // Capabilities — coding is the combination of `allowed_tools: ["*"]`
    // (or a list containing `claude_code*`/`dispatch*`) AND at least one
    // inbound binding with `dispatch_policy.mode: full`. Heartbeat is a
    // simple bool.
    let allowed_tools: Vec<String> =
        yaml_patch::get_agent_list(&agent_file, agent_id, "allowed_tools").unwrap_or_default();
    let tools_wildcard = allowed_tools.iter().any(|t| t == "*");
    let bindings_value = yaml_patch::read_agent_field(&agent_file, agent_id, "inbound_bindings")?;
    let any_full_dispatch = bindings_value
        .as_ref()
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter().any(|item| {
                item.get("dispatch_policy")
                    .and_then(|p| p.get("mode"))
                    .and_then(|m| m.as_str())
                    .map(|s| s == "full")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    let coding_enabled = tools_wildcard && any_full_dispatch;
    let heartbeat_enabled =
        yaml_patch::read_agent_field(&agent_file, agent_id, "heartbeat.enabled")?
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    Ok(AgentDashboard {
        id: agent_id.to_string(),
        model,
        language,
        channels,
        skills_attached,
        skills_total,
        coding_enabled,
        heartbeat_enabled,
    })
}

/// Pretty-print the dashboard. Mirrors `channels_dashboard` styling.
pub fn print_dashboard(d: &AgentDashboard) {
    use console::style;
    println!();
    println!("Agente: {}", style(&d.id).cyan().bold());
    match &d.model {
        ModelStatus::None => {
            println!("  Modelo:   {}", style("(no configurado)").red());
        }
        ModelStatus::Attached {
            provider,
            model,
            creds_ok,
        } => {
            let creds = if *creds_ok {
                style("creds ✔").green().to_string()
            } else {
                style("creds ✗").red().to_string()
            };
            println!(
                "  Modelo:   {} / {}  [{}]",
                style(provider).bold(),
                style(model).bold(),
                creds
            );
        }
    }
    let lang = d
        .language
        .clone()
        .unwrap_or_else(|| style("(default)").dim().to_string());
    println!("  Idioma:   {}", lang);
    if d.channels.is_empty() {
        println!("  Canales:  {}", style("(ninguno)").dim());
    } else {
        for (i, c) in d.channels.iter().enumerate() {
            let label = match &c.instance {
                Some(inst) => format!("{}:{}", c.plugin, inst),
                None => format!("{}:default", c.plugin),
            };
            let auth_icon = if c.auth_ok {
                style("✔").green().to_string()
            } else {
                style("✗").red().to_string()
            };
            let bind_tag = if c.bound {
                style("(bound)").green().to_string()
            } else {
                style("(unbound)").dim().to_string()
            };
            let prefix = if i == 0 {
                "  Canales:  "
            } else {
                "            "
            };
            println!("{prefix}{auth_icon} {label}  {bind_tag}");
        }
    }
    println!(
        "  Skills:   {} / {} attached",
        d.skills_attached, d.skills_total
    );
    let code_icon = if d.coding_enabled {
        style("✔").green().to_string()
    } else {
        style("✗").red().to_string()
    };
    let hb_icon = if d.heartbeat_enabled {
        style("✔").green().to_string()
    } else {
        style("✗").red().to_string()
    };
    println!("  Programar: {code_icon}   Heartbeat: {hb_icon}");
}

/// Handle the `Modelo` action — attach, change, or detach the model.
fn handle_model(
    agent_id: &str,
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let agent_file = yaml_patch::find_agent_file(config_dir, agent_id)?
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;
    let current_provider = yaml_patch::read_agent_field(&agent_file, agent_id, "model.provider")?
        .and_then(|v| v.as_str().map(str::to_string));
    let current_model = yaml_patch::read_agent_field(&agent_file, agent_id, "model.model")?
        .and_then(|v| v.as_str().map(str::to_string));

    if current_provider.is_some() && current_model.is_some() {
        let actions = [
            "Cambiar modelo (mismo provider)",
            "Cambiar provider",
            "Detach (quitar modelo)",
            "← volver",
        ];
        let pick = prompt::pick_from_list("Modelo actual ya configurado", &actions)?;
        match pick {
            0 => {
                let provider = current_provider.unwrap();
                let model = pick_model_for_provider(&provider)?;
                if model.is_empty() {
                    return Ok(());
                }
                yaml_patch::upsert_agent_field(
                    &agent_file,
                    agent_id,
                    "model.model",
                    Value::String(model.clone()),
                )?;
                println!("✔ modelo `{provider}/{model}` aplicado.");
                crate::try_hot_reload(config_dir);
                return Ok(());
            }
            1 => {
                // Fall through to provider-pick path below.
            }
            2 => {
                yaml_patch::remove_agent_field(&agent_file, agent_id, "model")?;
                println!("✔ modelo detachado.");
                crate::try_hot_reload(config_dir);
                return Ok(());
            }
            _ => return Ok(()),
        }
    }

    // Provider pick — last option is always "← volver".
    let llm_services: Vec<&ServiceDef> = services
        .iter()
        .filter(|s| s.category == Category::Llm)
        .collect();
    if llm_services.is_empty() {
        anyhow::bail!("no LLM providers in catalog");
    }
    let mut labels: Vec<&str> = llm_services.iter().map(|s| s.label).collect();
    labels.push("← volver");
    let idx = prompt::pick_from_list("Proveedor", &labels)?;
    if idx == llm_services.len() {
        return Ok(());
    }
    let svc = llm_services[idx];
    let provider_id = svc.id.to_string();

    // Audit creds; if missing, run the service form to fix them up.
    if !provider_creds_ok(&provider_id, services, secrets_dir, config_dir) {
        println!("(faltan credenciales para `{provider_id}` — corriendo formulario)");
        crate::run_service_pub(svc, secrets_dir, config_dir)?;
        if !provider_creds_ok(&provider_id, services, secrets_dir, config_dir) {
            anyhow::bail!("credenciales de `{provider_id}` siguen incompletas — abortando attach");
        }
    }

    let model_name = pick_model_for_provider(&provider_id)?;
    if model_name.is_empty() {
        println!("Cancelado.");
        return Ok(());
    }

    // Persist `model.provider` + `model.model`.
    yaml_patch::upsert_agent_field(
        &agent_file,
        agent_id,
        "model.provider",
        Value::String(provider_id.clone()),
    )?;
    yaml_patch::upsert_agent_field(
        &agent_file,
        agent_id,
        "model.model",
        Value::String(model_name.clone()),
    )?;
    println!("✔ modelo `{provider_id}/{model_name}` aplicado.");
    crate::try_hot_reload(config_dir);
    Ok(())
}

/// Pick a model name for the given provider. Returns an empty string
/// if the operator cancelled.
fn pick_model_for_provider(provider: &str) -> Result<String> {
    let entry = MODEL_CATALOG.iter().find(|(p, _)| *p == provider);
    let suggestions: &[&str] = entry.map(|(_, m)| *m).unwrap_or(&[]);
    if suggestions.is_empty() {
        // Free-form for openai_custom or unknown providers.
        let theme = dialoguer::theme::ColorfulTheme::default();
        let raw: String = dialoguer::Input::with_theme(&theme)
            .with_prompt("Model name")
            .allow_empty(true)
            .interact_text()?;
        return Ok(raw.trim().to_string());
    }
    let mut labels: Vec<&str> = suggestions.iter().copied().collect();
    labels.push("← volver");
    let idx = prompt::pick_from_list("Modelo", &labels)?;
    if idx == suggestions.len() {
        return Ok(String::new());
    }
    Ok(suggestions[idx].to_string())
}

/// Handle the `Idioma` action. Choose from a fixed shortlist or clear.
fn handle_language(agent_id: &str, config_dir: &Path) -> Result<()> {
    let agent_file = yaml_patch::find_agent_file(config_dir, agent_id)?
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;
    let opts = [
        "es",
        "en",
        "pt",
        "fr",
        "it",
        "de",
        "(limpiar — sin idioma forzado)",
        "← volver",
    ];
    let idx = prompt::pick_from_list("Idioma", &opts)?;
    if opts[idx] == "← volver" {
        return Ok(());
    }
    if opts[idx].starts_with("(limpiar") {
        yaml_patch::remove_agent_field(&agent_file, agent_id, "language")?;
        println!("✔ idioma limpiado.");
    } else {
        yaml_patch::upsert_agent_field(
            &agent_file,
            agent_id,
            "language",
            Value::String(opts[idx].to_string()),
        )?;
        println!("✔ idioma `{}` aplicado.", opts[idx]);
    }
    crate::try_hot_reload(config_dir);
    Ok(())
}

/// Handle the `Canales` action. Surfaces the same channel inventory
/// the channels-dashboard renders, but scoped to bind/unbind per the
/// chosen agent.
fn handle_channels(agent_id: &str, secrets_dir: &Path, config_dir: &Path) -> Result<()> {
    let all_detected = channels_dashboard::detect_channels(config_dir, secrets_dir)?;
    // Filter out instances that already belong exclusively to another
    // agent — same rule as the dashboard.
    let detected: Vec<channels_dashboard::ChannelEntry> = all_detected
        .into_iter()
        .filter(|entry| {
            let bound_to_self = entry.bound_agents.iter().any(|a| a == agent_id);
            let owned_by_other = !bound_to_self
                && !entry.bound_agents.is_empty()
                && entry.bound_agents.iter().all(|a| a != agent_id);
            !owned_by_other
        })
        .collect();
    if detected.is_empty() {
        println!("(sin canales disponibles para este agente)");
        return Ok(());
    }
    let agent_file = yaml_patch::find_agent_file(config_dir, agent_id)?
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;
    let bound: Vec<String> = yaml_patch::get_agent_list(&agent_file, agent_id, "plugins")?;

    let mut labels: Vec<String> = Vec::with_capacity(detected.len() + 1);
    for entry in &detected {
        let auth_icon = match entry.auth {
            channels_dashboard::AuthState::Authenticated => "✔",
            _ => "✗",
        };
        let bind_tag = if bound.iter().any(|p| p == &entry.channel)
            || entry.bound_agents.iter().any(|a| a == agent_id)
        {
            "bound"
        } else {
            "unbound"
        };
        labels.push(format!(
            "{auth_icon} {}:{} ({bind_tag})",
            entry.channel, entry.instance
        ));
    }
    labels.push("← volver".to_string());

    let pick = prompt::pick_from_strings("Canal", &labels)?;
    if pick >= detected.len() {
        return Ok(());
    }
    let entry = detected[pick].clone();
    let actions = [
        "Autenticar / re-autenticar",
        "Conectar al agente (bind)",
        "Desconectar del agente (unbind)",
        "← volver",
    ];
    let act = prompt::pick_from_list("Acción", &actions)?;
    match act {
        0 => {
            services_imperative::dispatch(&entry.channel, config_dir, secrets_dir)?;
            crate::try_hot_reload(config_dir);
        }
        1 => {
            // Add to plugins[] (idempotent) + add inbound_bindings[]
            // entry mapping {plugin: <channel>}.
            yaml_patch::append_agent_list_item(
                &agent_file,
                agent_id,
                "plugins",
                Value::String(entry.channel.clone()),
            )?;
            let mut binding = Mapping::new();
            binding.insert(
                Value::String("plugin".into()),
                Value::String(entry.channel.clone()),
            );
            yaml_patch::append_agent_list_item(
                &agent_file,
                agent_id,
                "inbound_bindings",
                Value::Mapping(binding),
            )?;
            println!("✔ {} bound.", entry.channel);
            crate::try_hot_reload(config_dir);
        }
        2 => {
            yaml_patch::remove_agent_list_item(&agent_file, agent_id, "plugins", &|v: &Value| {
                v.as_str() == Some(&entry.channel)
            })?;
            yaml_patch::remove_agent_list_item(
                &agent_file,
                agent_id,
                "inbound_bindings",
                &|v: &Value| v.get("plugin").and_then(Value::as_str) == Some(&entry.channel),
            )?;
            println!("✔ {} unbound.", entry.channel);
            crate::try_hot_reload(config_dir);
        }
        _ => {}
    }
    Ok(())
}

/// Handle the `Skills` action. Multi-select against the catalog,
/// auth-prompt for any newly added skill that requires secret fields,
/// then write the full list back.
fn handle_skills(
    agent_id: &str,
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> Result<()> {
    let agent_file = yaml_patch::find_agent_file(config_dir, agent_id)?
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;
    let skill_services: Vec<&ServiceDef> = services
        .iter()
        .filter(|s| s.category == Category::Skill)
        .collect();
    if skill_services.is_empty() {
        println!("(no skills in catalog)");
        return Ok(());
    }
    let current: Vec<String> = yaml_patch::get_agent_list(&agent_file, agent_id, "skills")?;

    // Only surface skills the operator can actually use right now:
    // either already attached (so they can detach) or configured (no
    // pending secrets). Unconfigured + unattached skills hide until the
    // operator runs the service-centric wizard ("Rotar credencial" /
    // "Avanzado") — keeps this list short and free of dead options.
    let usable: Vec<&ServiceDef> = skill_services
        .iter()
        .copied()
        .filter(|svc| {
            current.iter().any(|c| c == svc.id) || skill_creds_ok(svc, secrets_dir, config_dir)
        })
        .collect();
    let hidden_count = skill_services.len() - usable.len();

    if usable.is_empty() {
        println!(
            "(no hay skills disponibles — configurá credenciales en \"Rotar credencial\" o \"Avanzado\" del menú principal)"
        );
        return Ok(());
    }
    if hidden_count > 0 {
        println!(
            "(ocultas {hidden_count} skills sin credenciales — configurálas en \"Rotar credencial\" para que aparezcan)"
        );
    }

    let preset_actions = [
        "Marcar todas (las disponibles)",
        "Desmarcar todas",
        "Selección personalizada (multi-select)",
        "← volver",
    ];
    let preset = prompt::pick_from_list("Skills", &preset_actions)?;
    let chosen_ids: Vec<String> = match preset {
        0 => usable.iter().map(|s| s.id.to_string()).collect(),
        1 => Vec::new(),
        2 => {
            let labels: Vec<&str> = usable.iter().map(|s| s.label).collect();
            let defaults: Vec<bool> = usable
                .iter()
                .map(|s| current.iter().any(|c| c == s.id))
                .collect();
            let chosen_labels = prompt::multi_select(
                "Skills (space = toggle, enter = confirmar)",
                &labels,
                &defaults,
            )?;
            usable
                .iter()
                .filter(|s| chosen_labels.iter().any(|l| l == s.label))
                .map(|s| s.id.to_string())
                .collect()
        }
        _ => return Ok(()),
    };

    // Skills that are new in this run + need secrets → run service.
    for svc in &skill_services {
        if !chosen_ids.iter().any(|id| id == svc.id) {
            continue;
        }
        if current.iter().any(|c| c == svc.id) {
            continue; // already attached, no need to re-auth
        }
        if !skill_creds_ok(svc, secrets_dir, config_dir) {
            println!(
                "(faltan credenciales para `{}` — corriendo formulario)",
                svc.id
            );
            if let Err(e) = crate::run_service_pub(svc, secrets_dir, config_dir) {
                eprintln!("⚠  {}: {e:#}", svc.label);
            }
        }
    }

    // Replace the skills list wholesale.
    let value = Value::Sequence(chosen_ids.iter().cloned().map(Value::String).collect());
    yaml_patch::upsert_agent_field(&agent_file, agent_id, "skills", value)?;
    println!("✔ {} skills attached.", chosen_ids.len());
    crate::try_hot_reload(config_dir);
    Ok(())
}

/// Handle the `Capacidades` action — flip "agent can program"
/// (Phase 67 dispatch) and the heartbeat toggle in one place.
///
/// "Programar" enabled means:
///   * `allowed_tools` contains `"*"` (so `claude_code_*` and
///     `dispatch_*` tools resolve at registry build time)
///   * Every `inbound_bindings[]` entry has
///     `dispatch_policy.mode: full` so the operator can launch goals
///     from the paired channel.
///
/// Disabling it removes the wildcard and forces every binding back to
/// `dispatch_policy.mode: none` (the safe default that ships in
/// `agents.yaml` for non-coding personas).
fn handle_capabilities(agent_id: &str, config_dir: &Path) -> Result<()> {
    let agent_file = yaml_patch::find_agent_file(config_dir, agent_id)?
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;

    let actions = [
        "Programar (puede escribir/ejecutar código vía Claude Code)",
        "Heartbeat (proactividad / recordatorios cada N min)",
        "← volver",
    ];
    let pick = prompt::pick_from_list("¿Qué permiso?", &actions)?;
    match pick {
        0 => toggle_coding(&agent_file, agent_id, config_dir)?,
        1 => toggle_heartbeat(&agent_file, agent_id, config_dir)?,
        _ => return Ok(()),
    }
    Ok(())
}

fn toggle_coding(agent_file: &Path, agent_id: &str, config_dir: &Path) -> Result<()> {
    let allowed: Vec<String> =
        yaml_patch::get_agent_list(agent_file, agent_id, "allowed_tools").unwrap_or_default();
    let wildcard = allowed.iter().any(|t| t == "*");
    let bindings_value = yaml_patch::read_agent_field(agent_file, agent_id, "inbound_bindings")?;
    let any_full = bindings_value
        .as_ref()
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter().any(|item| {
                item.get("dispatch_policy")
                    .and_then(|p| p.get("mode"))
                    .and_then(|m| m.as_str())
                    .map(|s| s == "full")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    let currently_on = wildcard && any_full;
    let prompt_label = if currently_on {
        "Programar está ON. ¿Apagar?"
    } else {
        "Programar está OFF. ¿Encender?"
    };
    if !prompt::yes_no(prompt_label, false)? {
        return Ok(());
    }

    if currently_on {
        // Disable: drop allowed_tools wildcard, force every binding
        // dispatch_policy.mode back to `none`. Leave the rest of the
        // bindings untouched.
        yaml_patch::remove_agent_field(agent_file, agent_id, "allowed_tools")?;
        set_dispatch_mode_on_bindings(agent_file, agent_id, "none")?;
        println!("✔ Programar OFF (allowed_tools cleared, bindings → mode: none).");
    } else {
        // Enable: wildcard tools, dispatch_policy.mode = full per
        // binding. We don't write the top-level dispatch_policy — the
        // existing one (or the default `none`) stays as the fallback
        // for delegation/heartbeat contexts. Mirrors `cody.yaml`.
        yaml_patch::upsert_agent_field(
            agent_file,
            agent_id,
            "allowed_tools",
            Value::Sequence(vec![Value::String("*".to_string())]),
        )?;
        set_dispatch_mode_on_bindings(agent_file, agent_id, "full")?;
        println!("✔ Programar ON (allowed_tools=[\"*\"], bindings → mode: full).");
    }
    crate::try_hot_reload(config_dir);
    Ok(())
}

fn toggle_heartbeat(agent_file: &Path, agent_id: &str, config_dir: &Path) -> Result<()> {
    let current = yaml_patch::read_agent_field(agent_file, agent_id, "heartbeat.enabled")?
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let prompt_label = if current {
        "Heartbeat está ON. ¿Apagar?"
    } else {
        "Heartbeat está OFF. ¿Encender?"
    };
    if !prompt::yes_no(prompt_label, false)? {
        return Ok(());
    }
    yaml_patch::upsert_agent_field(
        agent_file,
        agent_id,
        "heartbeat.enabled",
        Value::Bool(!current),
    )?;
    println!("✔ Heartbeat → {}.", if current { "OFF" } else { "ON" });
    crate::try_hot_reload(config_dir);
    Ok(())
}

/// Walks `inbound_bindings[]` and writes `dispatch_policy.mode = <mode>`
/// into every entry (creating the sub-map if missing). No-op when the
/// list is absent or empty — coding stays effectively-off until the
/// operator binds at least one channel.
fn set_dispatch_mode_on_bindings(agent_file: &Path, agent_id: &str, mode: &str) -> Result<()> {
    let bindings = match yaml_patch::read_agent_field(agent_file, agent_id, "inbound_bindings")? {
        Some(v) => v,
        None => return Ok(()),
    };
    let seq = match bindings {
        Value::Sequence(s) => s,
        _ => return Ok(()),
    };
    if seq.is_empty() {
        return Ok(());
    }
    let new_seq: Vec<Value> = seq
        .into_iter()
        .map(|item| {
            let mut map = match item {
                Value::Mapping(m) => m,
                other => {
                    let mut m = Mapping::new();
                    m.insert(Value::String("plugin".into()), other);
                    m
                }
            };
            let mut policy = map
                .get(Value::String("dispatch_policy".into()))
                .cloned()
                .and_then(|v| match v {
                    Value::Mapping(m) => Some(m),
                    _ => None,
                })
                .unwrap_or_default();
            policy.insert(
                Value::String("mode".into()),
                Value::String(mode.to_string()),
            );
            map.insert(
                Value::String("dispatch_policy".into()),
                Value::Mapping(policy),
            );
            Value::Mapping(map)
        })
        .collect();
    yaml_patch::upsert_agent_field(
        agent_file,
        agent_id,
        "inbound_bindings",
        Value::Sequence(new_seq),
    )?;
    Ok(())
}

/// Quick credential audit for an LLM provider id. Reuses the wizard's
/// status::audit, then checks every required field maps to either a
/// configured Secret or a non-empty env var.
fn provider_creds_ok(
    provider: &str,
    services: &[ServiceDef],
    secrets_dir: &Path,
    config_dir: &Path,
) -> bool {
    let svc = match services.iter().find(|s| s.id == provider) {
        Some(s) => s,
        None => return false,
    };
    let report = status::audit(std::slice::from_ref(svc), secrets_dir, config_dir);
    report
        .services
        .first()
        .map(|s| s.is_fully_configured())
        .unwrap_or(false)
}

/// Same idea for skills: every required Secret field must either land
/// in `secrets/` or in the matching env var.
fn skill_creds_ok(svc: &ServiceDef, secrets_dir: &Path, config_dir: &Path) -> bool {
    // Skills with no Secret fields are always considered "ok" — they
    // don't need an auth dance to attach.
    let has_secret = svc.fields.iter().any(|f| {
        matches!(f.kind, FieldKind::Secret)
            && f.required
            && matches!(f.target, FieldTarget::Secret { .. })
    });
    if !has_secret {
        return true;
    }
    let report = status::audit(std::slice::from_ref(svc), secrets_dir, config_dir);
    report
        .services
        .first()
        .map(|s| s.is_fully_configured())
        .unwrap_or(false)
}
