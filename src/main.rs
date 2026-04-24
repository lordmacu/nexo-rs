use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use agent_broker::{AnyBroker, DiskQueue};
use agent_config::AppConfig;
use agent_core::agent::dreaming::{DreamEngine, DreamingConfig};
use agent_core::session::SessionManager;
use agent_core::telemetry::{add_extensions_discovered, render_prometheus};
use agent_core::{
    Agent, AgentRuntime, DelegationTool, ExtensionHook, ExtensionTool, HeartbeatTool, HookRegistry,
    LlmAgentBehavior, MemoryTool, MyStatsTool, PluginRegistry, ToolRegistry, WhatDoIKnowTool,
    WhoAmITool,
};
use agent_llm::LlmRegistry;
use agent_memory::LongTermMemory;
use agent_plugin_browser::BrowserPlugin;
use agent_plugin_whatsapp::WhatsappPlugin;

enum Mode {
    Run,
    DlqList,
    DlqReplay(String),
    DlqPurge,
    ExtList {
        json: bool,
    },
    ExtInfo {
        id: String,
        json: bool,
    },
    ExtEnable {
        id: String,
    },
    ExtDisable {
        id: String,
    },
    ExtValidate {
        path: PathBuf,
    },
    ExtDoctor {
        runtime: bool,
        json: bool,
    },
    ExtInstall {
        source: PathBuf,
        update: bool,
        enable: bool,
        dry_run: bool,
        link: bool,
        json: bool,
    },
    ExtUninstall {
        id: String,
        yes: bool,
        json: bool,
    },
    ExtHelp,
    McpServer,
    FlowList {
        json: bool,
    },
    FlowShow {
        id: String,
        json: bool,
    },
    FlowCancel {
        id: String,
    },
    FlowResume {
        id: String,
    },
    FlowHelp,
    SetupInteractive,
    SetupOne {
        service: String,
    },
    SetupList,
    SetupDoctor,
    SetupTelegramLink,
    /// Query the running agent's admin HTTP endpoint and pretty-print
    /// the agent directory. `json: true` returns raw JSON (machine
    /// consumable); otherwise a plain-text table goes to stdout.
    /// `agent_id: Some` narrows to one agent (uses `/admin/agents/<id>`).
    Status {
        json: bool,
        endpoint: Option<String>,
        agent_id: Option<String>,
    },
    /// Load the config, validate everything (env vars, plugin tokens,
    /// agent fields), print a summary, and exit 0. No network, no
    /// runtimes, no broker connect — just a pre-flight check suitable
    /// for CI gates (`agent --dry-run` before deploy).
    DryRun {
        json: bool,
    },
    Help,
}

struct CliArgs {
    config_dir: PathBuf,
    mode: Mode,
}

#[derive(Clone)]
struct RuntimeHealth {
    broker: AnyBroker,
    running_agents: Arc<AtomicUsize>,
    /// WhatsApp pairing states keyed by instance label. Unlabelled
    /// (legacy single-account) configs register under `"default"`.
    /// Health server exposes:
    ///   `/whatsapp/pair{,/qr,/status}` — first instance (back-compat)
    ///   `/whatsapp/<instance>/pair{,/qr,/status}` — targeted
    ///   `/whatsapp/instances` — JSON list of available instances
    wa_pairing:
        std::collections::BTreeMap<String, agent_plugin_whatsapp::pairing::SharedPairingState>,
}

#[derive(Clone, Copy)]
enum LogFormat {
    Pretty,
    Compact,
    Json,
}

struct JsonLogLayer;

#[derive(Default)]
struct JsonFieldVisitor {
    fields: JsonMap<String, JsonValue>,
}

impl Visit for JsonFieldVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), JsonValue::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), JsonValue::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), JsonValue::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), JsonValue::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .insert(field.name().to_string(), JsonValue::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields
            .insert(field.name().to_string(), JsonValue::from(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields.insert(
            field.name().to_string(),
            JsonValue::from(format!("{value:?}")),
        );
    }
}

impl<S> Layer<S> for JsonLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, ctx: LayerContext<'_, S>) {
        let mut visitor = JsonFieldVisitor::default();
        event.record(&mut visitor);

        let meta = event.metadata();
        let ts_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut payload = JsonMap::new();
        payload.insert("ts_unix_ms".to_string(), JsonValue::from(ts_unix_ms));
        payload.insert(
            "level".to_string(),
            JsonValue::from(meta.level().to_string()),
        );
        payload.insert("target".to_string(), JsonValue::from(meta.target()));
        payload.insert(
            "thread_id".to_string(),
            JsonValue::from(format!("{:?}", std::thread::current().id())),
        );

        if let Some(file) = meta.file() {
            payload.insert("file".to_string(), JsonValue::from(file));
        }
        if let Some(line) = meta.line() {
            payload.insert("line".to_string(), JsonValue::from(line as u64));
        }
        if !visitor.fields.is_empty() {
            payload.insert("fields".to_string(), JsonValue::Object(visitor.fields));
        }
        if let Some(scope) = ctx.event_scope(event) {
            let spans: Vec<String> = scope
                .from_root()
                .map(|span| span.metadata().name().to_string())
                .collect();
            if !spans.is_empty() {
                payload.insert("spans".to_string(), json!(spans));
            }
        }

        eprintln!("{}", JsonValue::Object(payload));
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    // Phase 11.1 follow-up — make the `agent` binary's version the one
    // compared against `plugin.min_agent_version`, instead of the
    // `agent-extensions` crate version. Ignore the result: if the
    // override was already set (double-init in tests), the existing
    // value wins — safe default.
    let _ = agent_extensions::set_agent_version(env!("CARGO_PKG_VERSION"));

    let args = parse_args();
    match args.mode {
        Mode::Help => {
            print_usage();
            return Ok(());
        }
        Mode::DlqList => return run_dlq_list(&args.config_dir).await,
        Mode::DlqReplay(id) => return run_dlq_replay(&args.config_dir, &id).await,
        Mode::DlqPurge => return run_dlq_purge(&args.config_dir).await,
        Mode::ExtHelp => return run_ext_help(),
        Mode::ExtList { json } => return run_ext_cli(&args.config_dir, ExtCmd::List { json }),
        Mode::ExtInfo { id, json } => {
            return run_ext_cli(&args.config_dir, ExtCmd::Info { id, json })
        }
        Mode::ExtEnable { id } => return run_ext_cli(&args.config_dir, ExtCmd::Enable { id }),
        Mode::ExtDisable { id } => return run_ext_cli(&args.config_dir, ExtCmd::Disable { id }),
        Mode::ExtValidate { path } => {
            return run_ext_cli(&args.config_dir, ExtCmd::Validate { path })
        }
        Mode::ExtDoctor { runtime, json } => {
            return run_ext_cli(&args.config_dir, ExtCmd::Doctor { runtime, json })
        }
        Mode::McpServer => return run_mcp_server(&args.config_dir).await,
        Mode::FlowHelp => return run_flow_help(),
        Mode::FlowList { json } => return run_flow_list(json).await,
        Mode::FlowShow { id, json } => return run_flow_show(&id, json).await,
        Mode::FlowCancel { id } => return run_flow_cancel(&id).await,
        Mode::FlowResume { id } => return run_flow_resume(&id).await,
        Mode::SetupInteractive => return agent_setup::run_interactive(&args.config_dir),
        Mode::SetupOne { service } => return agent_setup::run_one(&args.config_dir, &service),
        Mode::SetupList => return agent_setup::run_list(&args.config_dir),
        Mode::SetupDoctor => return agent_setup::run_doctor(&args.config_dir),
        Mode::SetupTelegramLink => return agent_setup::run_telegram_link(&args.config_dir),
        Mode::Status {
            json,
            endpoint,
            agent_id,
        } => return run_status(json, endpoint, agent_id).await,
        Mode::DryRun { json } => return run_dry_run(&args.config_dir, json),
        Mode::ExtInstall {
            source,
            update,
            enable,
            dry_run,
            link,
            json,
        } => {
            return run_ext_cli(
                &args.config_dir,
                ExtCmd::Install {
                    source,
                    update,
                    enable,
                    dry_run,
                    link,
                    json,
                },
            )
        }
        Mode::ExtUninstall { id, yes, json } => {
            return run_ext_cli(&args.config_dir, ExtCmd::Uninstall { id, yes, json })
        }
        Mode::Run => {}
    }

    // Single-instance guard: if another `agent` process is already
    // running against the same data dir, terminate it before we start.
    // Prevents the "two agents on one NATS" bug where both processes
    // subscribe to `plugin.outbound.*` and every message is sent twice.
    let _lock = acquire_single_instance_lock().context("failed to acquire agent lock")?;

    let config_dir = args.config_dir;
    tracing::info!(config_dir = %config_dir.display(), "loading config");
    let cfg = AppConfig::load(&config_dir).context("failed to load config")?;

    // Validate per-binding overrides before anything else spawns. We skip
    // the tool-name check here because the tool registry is assembled
    // after extensions / MCP discovery — the structural checks (duplicate
    // bindings, unknown telegram instances, missing skill dirs) are
    // enough to surface the most common YAML mistakes at startup.
    agent_core::agent::validate_agents(
        &cfg.agents.agents,
        &cfg.plugins.telegram,
        &agent_core::agent::KnownTools::default(),
    )
    .context("per-binding override validation failed")?;

    // Extension discovery (Phase 11.2) -------------------------------------
    // Runs before anything that depends on extensions. Spawns stdio runtimes
    // (Phase 11.3) for each discovered candidate and keeps them alive for
    // the agent's lifetime. Tool-registry injection lands in 11.5.
    let (extension_runtimes, ext_mcp_decls) =
        run_extension_discovery(cfg.extensions.as_ref()).await;

    // Phase 12.4+12.7 — MCP runtime manager. One per process; every agent
    // shares a sentinel session to avoid spawning duplicate MCP children.
    // `cfg.mcp.is_none()` or `enabled: false` → no manager, no tools.
    const MCP_SHARED_SESSION: uuid::Uuid = uuid::Uuid::nil();
    let watcher_shutdown = tokio_util::sync::CancellationToken::new();

    // Phase 11.2 follow-up — opt-in plugin.toml watcher. Logs manifest
    // changes; requires operator restart to apply.
    if let Some(ext_cfg) = cfg.extensions.as_ref() {
        if ext_cfg.watch.enabled {
            let mut snapshot = agent_extensions::KnownPluginSnapshot::new();
            for (_rt, cand) in &extension_runtimes {
                snapshot.insert(cand.manifest.id(), cand.manifest_path.clone());
            }
            let roots: Vec<PathBuf> = ext_cfg
                .search_paths
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .collect();
            if roots.is_empty() {
                tracing::warn!(
                    "extensions.watch.enabled=true but no existing search_paths — skipping"
                );
            } else {
                let debounce = std::time::Duration::from_millis(ext_cfg.watch.debounce_ms.max(50));
                agent_extensions::spawn_extensions_watcher(
                    roots,
                    snapshot,
                    debounce,
                    watcher_shutdown.clone(),
                );
                tracing::info!(
                    debounce_ms = ext_cfg.watch.debounce_ms,
                    "plugin.toml watcher enabled"
                );
            }
        }
    }
    let llm_registry = LlmRegistry::with_builtins();
    let mcp_sampling_provider = build_mcp_sampling_provider(&cfg, &llm_registry)
        .context("failed to initialize MCP sampling provider")?;
    let mcp_manager: Option<Arc<agent_mcp::McpRuntimeManager>> = match cfg.mcp.as_ref() {
        Some(mcp_cfg) if mcp_cfg.enabled => {
            let ext_decls: Vec<agent_mcp::runtime_config::ExtensionServerDecl> = ext_mcp_decls
                .iter()
                .map(|d| agent_mcp::runtime_config::ExtensionServerDecl {
                    ext_id: d.ext_id.clone(),
                    ext_version: d.ext_version.clone(),
                    ext_root: d.ext_root.clone(),
                    servers: d.servers.clone(),
                })
                .collect();
            let rt_cfg = agent_mcp::runtime_config::McpRuntimeConfig::from_yaml_with_extensions(
                mcp_cfg, &ext_decls,
            );
            tracing::info!(
                servers = rt_cfg.servers.len(),
                yaml_servers = mcp_cfg.servers.len(),
                extension_decls = ext_decls.len(),
                "initializing mcp runtime manager"
            );
            let mgr = agent_mcp::McpRuntimeManager::new_with_sampling(
                rt_cfg,
                mcp_sampling_provider.clone(),
            );
            if mcp_cfg.watch.enabled {
                let debounce = std::time::Duration::from_millis(mcp_cfg.watch.debounce_ms.max(50));
                agent_mcp::spawn_mcp_config_watcher(
                    config_dir.clone(),
                    Arc::clone(&mgr),
                    ext_decls,
                    cfg.extensions.clone(),
                    debounce,
                    watcher_shutdown.clone(),
                );
                tracing::info!(
                    debounce_ms = mcp_cfg.watch.debounce_ms,
                    "mcp config watcher enabled"
                );
            }
            Some(mgr)
        }
        Some(_) => {
            tracing::info!("mcp disabled in config/mcp.yaml — skipping runtime bootstrap");
            None
        }
        None => None,
    };

    // Broker ---------------------------------------------------------------
    let broker = AnyBroker::from_config(&cfg.broker.broker)
        .await
        .context("failed to initialize broker")?;
    tracing::info!(kind = ?cfg.broker.broker.kind, url = %cfg.broker.broker.url, "broker ready");

    // Long-term memory -----------------------------------------------------
    let memory = match cfg.memory.long_term.backend.as_str() {
        "sqlite" => {
            let path = cfg
                .memory
                .long_term
                .sqlite
                .as_ref()
                .map(|s| s.path.as_str())
                .unwrap_or("./data/memory.db");

            // Phase 5.4 — build optional embedding provider for vector recall.
            let embedding_provider: Option<Arc<dyn agent_memory::EmbeddingProvider>> = if cfg
                .memory
                .vector
                .enabled
            {
                let emb = &cfg.memory.vector.embedding;
                match emb.provider.as_str() {
                    "http" => match reqwest::Url::parse(&emb.base_url) {
                        Ok(url) => {
                            let api_key = if emb.api_key.is_empty() {
                                None
                            } else {
                                Some(emb.api_key.clone())
                            };
                            match agent_memory::HttpEmbeddingProvider::new(
                                url,
                                emb.model.clone(),
                                api_key,
                                emb.dimensions,
                                std::time::Duration::from_secs(emb.timeout_secs),
                            ) {
                                Ok(p) => {
                                    tracing::info!(
                                        model = %emb.model,
                                        base_url = %emb.base_url,
                                        dim = emb.dimensions,
                                        "embedding provider initialised"
                                    );
                                    Some(Arc::new(p) as Arc<dyn agent_memory::EmbeddingProvider>)
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "embedding provider init failed; vector disabled");
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, base_url = %emb.base_url, "invalid embedding base_url; vector disabled");
                            None
                        }
                    },
                    other => {
                        tracing::warn!(provider = %other, "unknown embedding provider; vector disabled");
                        None
                    }
                }
            } else {
                None
            };

            let mem = LongTermMemory::open_with_vector(path, embedding_provider)
                .await
                .with_context(|| format!("failed to open long-term memory at {path}"))?;
            tracing::info!(
                path = %path,
                vector = mem.embedding_provider().is_some(),
                "long-term memory ready"
            );
            Some(Arc::new(mem))
        }
        other => {
            tracing::warn!(backend = %other, "unsupported long-term memory backend — disabled");
            None
        }
    };

    // Sessions -------------------------------------------------------------
    let session_ttl =
        humantime::parse_duration(&cfg.memory.short_term.session_ttl).with_context(|| {
            format!(
                "invalid memory.short_term.session_ttl `{}`",
                cfg.memory.short_term.session_ttl
            )
        })?;
    let sessions = Arc::new(SessionManager::with_cap(
        session_ttl,
        cfg.memory.short_term.max_history_turns,
        cfg.memory.short_term.max_sessions,
    ));
    tracing::info!(
        ttl = ?session_ttl,
        max_turns = cfg.memory.short_term.max_history_turns,
        max_sessions = cfg.memory.short_term.max_sessions,
        "session manager ready"
    );

    // Wire MCP session disposal: every expired session tears down its
    // share of the shared runtime so unused clients are released.
    if let Some(mgr) = mcp_manager.clone() {
        let m = mgr.clone();
        sessions.on_expire(move |sid| {
            let m = m.clone();
            tokio::spawn(async move {
                m.dispose_session(sid).await;
            });
        });
    }

    // Plugins --------------------------------------------------------------
    let plugins = PluginRegistry::new();
    // Keep an Arc<BrowserPlugin> aside so agents with `plugins: [browser]`
    // can register the full `browser_*` tool family against it. Tool
    // handlers call `plugin.execute(...)` directly — no broker round-trip
    // — so each tool call hits the CDP session exactly once.
    let browser_plugin: Option<Arc<agent_plugin_browser::BrowserPlugin>> =
        cfg.plugins.browser.clone().map(|browser_cfg| {
            let plugin = Arc::new(BrowserPlugin::new(browser_cfg));
            tracing::info!("registered plugin: browser");
            plugin
        });
    if let Some(plugin) = browser_plugin.clone() {
        // Register into the PluginRegistry via the Plugin trait. The
        // registry stores `Arc<dyn Plugin>` so we keep our Arc handle
        // alive for tool registration below.
        plugins.register_arc(plugin as Arc<dyn agent_core::agent::plugin::Plugin>);
    }
    // WhatsApp plugins — zero, one, or many accounts. Each one registers
    // under `whatsapp` (legacy single-account) or `whatsapp.<instance>`.
    // Pairing states are collected per-instance so the health server
    // can expose `/whatsapp/<instance>/pair*` alongside the legacy
    // `/whatsapp/pair*` that targets the first account for back-compat.
    let mut wa_pairing: std::collections::BTreeMap<
        String,
        agent_plugin_whatsapp::pairing::SharedPairingState,
    > = std::collections::BTreeMap::new();
    let mut wa_tunnel_cfg: Option<agent_config::WhatsappPublicTunnelConfig> = None;
    for (idx, wa_cfg) in cfg.plugins.whatsapp.clone().into_iter().enumerate() {
        if !wa_cfg.enabled {
            let label = wa_cfg.instance.clone().unwrap_or_else(|| "default".into());
            tracing::info!(instance = %label, "whatsapp plugin configured but disabled — skipping");
            continue;
        }
        let instance_label = wa_cfg.instance.clone().unwrap_or_else(|| "default".into());
        if idx == 0 {
            wa_tunnel_cfg = Some(wa_cfg.public_tunnel.clone());
        }
        let plugin = WhatsappPlugin::new(wa_cfg);
        wa_pairing.insert(instance_label.clone(), plugin.pairing_state());
        plugins.register(plugin);
        tracing::info!(instance = %instance_label, "registered plugin: whatsapp");
    }
    // Telegram Bot plugins — zero, one, or many. Each instance registers
    // under its own unique name (`telegram` for legacy single-bot,
    // `telegram.<instance>` for multi-bot) so PluginRegistry doesn't
    // collapse them. Agents target a specific bot via `inbound_bindings`.
    for tg_cfg in cfg.plugins.telegram.clone() {
        let instance_label = tg_cfg
            .instance
            .clone()
            .unwrap_or_else(|| "<default>".into());
        let plugin = agent_plugin_telegram::TelegramPlugin::new(tg_cfg);
        plugins.register(plugin);
        tracing::info!(instance = %instance_label, "registered plugin: telegram");
    }
    // email: Phase 6+
    plugins
        .start_all(broker.clone())
        .await
        .context("failed to start plugins")?;

    // Agents ---------------------------------------------------------------
    let running_agents = Arc::new(AtomicUsize::new(0));
    let health = RuntimeHealth {
        broker: broker.clone(),
        running_agents: Arc::clone(&running_agents),
        wa_pairing: wa_pairing.clone(),
    };
    let metrics_handle = tokio::spawn(run_metrics_server(health.clone()));
    let health_handle = tokio::spawn(run_health_server(health.clone()));

    // Phase 6.10 follow-up — auto-open a Cloudflare Tunnel to expose
    // `/whatsapp/pair` publicly. Tunnels the first account's pairing
    // page; multi-account operators should reach their own instance
    // via `/whatsapp/<instance>/pair` on the tunnelled URL.
    let wa_first_pairing = wa_pairing.values().next().cloned();
    if let (Some(tcfg), Some(pairing)) = (wa_tunnel_cfg.as_ref(), wa_first_pairing) {
        if tcfg.enabled {
            let only_until_paired = tcfg.only_until_paired;
            tokio::spawn(async move {
                // Wait for the local HTTP server to actually bind before
                // cloudflared tries to open a tunnel to it — otherwise
                // the tunnel comes up first and Cloudflare returns 502
                // "error opening stream to origin" for every request
                // until the race resolves on its own.
                for attempt in 0..60u32 {
                    if reqwest::Client::new()
                        .get("http://127.0.0.1:8080/health")
                        .timeout(std::time::Duration::from_millis(500))
                        .send()
                        .await
                        .ok()
                        .filter(|r| r.status().is_success())
                        .is_some()
                    {
                        tracing::debug!(attempt, "local health server ready, starting tunnel");
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                match agent_tunnel::TunnelManager::new(8080).start().await {
                    Ok(handle) => {
                        // Big, hard-to-miss banner on stderr so
                        // operators see the URL even in noisy logs.
                        let url = handle.url.clone();
                        let pair_url = format!("{url}/whatsapp/pair");
                        eprintln!();
                        eprintln!("╭───────────────────────────────────────────────────────────╮");
                        eprintln!("│  WhatsApp pairing URL (Cloudflare Tunnel)                │");
                        eprintln!("│                                                           │");
                        eprintln!("│  {:<57} │", pair_url);
                        eprintln!("│                                                           │");
                        eprintln!("│  Abre esa URL desde el teléfono donde tengas WhatsApp.   │");
                        eprintln!("╰───────────────────────────────────────────────────────────╯");
                        eprintln!();
                        tracing::info!(%url, "cloudflared public tunnel up");

                        if only_until_paired {
                            // Poll pairing state; once connected, close
                            // the tunnel so the public URL doesn't
                            // outlive its need.
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                let s = pairing.status().await;
                                if s.state == "connected" {
                                    tracing::info!("pairing complete — closing public tunnel");
                                    handle.shutdown().await;
                                    return;
                                }
                            }
                        } else {
                            // Keep the handle alive for the rest of
                            // the process lifetime.
                            std::mem::forget(handle);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cloudflared tunnel failed to start — pairing will need LAN or port-forward");
                    }
                }
            });
        }
    }

    // Gmail poller — background task that polls Gmail on a fixed
    // interval and routes matching emails to channel plugins. No LLM
    // in the hot path; dedup via Gmail UNREAD label. Absent config
    // file = feature off.
    match agent_plugin_gmail_poller::GmailPollerConfig::load(&config_dir) {
        Ok(Some(cfg)) => {
            if let Err(e) = agent_plugin_gmail_poller::spawn(cfg, broker.clone()).await {
                tracing::warn!(error = %e, "gmail-poller failed to start");
            }
        }
        Ok(None) => {
            tracing::debug!("gmail-poller: config absent, skipping");
        }
        Err(e) => {
            tracing::warn!(error = %e, "gmail-poller: failed to load config");
        }
    }

    // Optional sidecar policy for tool caching / parallel-safety /
    // relevance filtering. File absence = feature off (back-compat).
    // The `Registry` owns a default `ToolPolicy` plus per-agent
    // overrides, so each agent gets its own `Arc<ToolPolicy>`.
    let tool_policy_registry = {
        let path = config_dir.join("tool_policy.yaml");
        if path.exists() {
            match std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|t| {
                    serde_yaml::from_str::<agent_core::agent::tool_policy::ToolPolicyConfig>(&t)
                        .map_err(anyhow::Error::from)
                }) {
                Ok(cfg) => {
                    tracing::info!(
                        cache_rules = cfg.cache.tools.len(),
                        parallel_rules = cfg.parallel_safe.len(),
                        per_agent_overrides = cfg.per_agent.len(),
                        "tool policy loaded"
                    );
                    agent_core::agent::tool_policy::ToolPolicyRegistry::from_config(&cfg)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "tool_policy.yaml parse failed — feature off");
                    agent_core::agent::tool_policy::ToolPolicyRegistry::disabled()
                }
            }
        } else {
            agent_core::agent::tool_policy::ToolPolicyRegistry::disabled()
        }
    };

    // Background sweep — evict expired cache entries across every
    // per-agent policy. Cheap retain pass every 60s; no-op on the
    // disabled registry (no cache entries to walk).
    {
        let registry = Arc::clone(&tool_policy_registry);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await;
            loop {
                tick.tick().await;
                registry.sweep_expired();
            }
        });
    }

    // Build a shared peer directory once so every agent's context sees
    // the same snapshot. Rendered as a `# PEERS` block in the system
    // prompt (filtered + annotated against `allowed_delegates`).
    let peer_directory = agent_core::agent::PeerDirectory::new(
        cfg.agents
            .agents
            .iter()
            .map(|a| agent_core::agent::PeerSummary {
                id: a.id.clone(),
                description: a.description.clone(),
            })
            .collect(),
    );
    // Ops-facing directory — served at `GET /admin/agents`. Snapshot
    // of the operator-relevant bits of each agent's config (no secrets,
    // no runtime state) so a dashboard / CLI can answer "who's running
    // and what are they configured to do?"
    let agents_directory = agent_core::agent::AgentsDirectory::new(
        cfg.agents
            .agents
            .iter()
            .map(agent_core::agent::AgentInfo::from_config)
            .collect(),
    );
    // Loopback-only admin HTTP server. Bound to `127.0.0.1` — nothing
    // here is authenticated, so exposing it to the LAN would let anyone
    // flush the cache or inspect the agent list. ssh-tunnel
    // `-L 9091:127.0.0.1:9091` to reach it remotely.
    let _admin_handle = tokio::spawn(run_admin_server(
        Arc::clone(&tool_policy_registry),
        Arc::clone(&agents_directory),
    ));
    let mut runtimes: Vec<AgentRuntime> = Vec::with_capacity(cfg.agents.agents.len());
    // Dreaming-side cancellation + handles. Each enabled agent spawns a
    // sweep loop; shutdown cancels the token and joins them with a
    // bounded timeout so SIGTERM cannot hang on an in-flight sweep.
    let dream_shutdown = tokio_util::sync::CancellationToken::new();
    let mut dream_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for agent_cfg in cfg.agents.agents {
        let agent_id = agent_cfg.id.clone();
        let dream_yaml = agent_cfg.dreaming.clone();
        let workspace_for_dream = agent_cfg.workspace.clone();
        let llm = llm_registry
            .build(&cfg.llm, &agent_cfg.model)
            .with_context(|| format!("failed to build LLM client for agent {agent_id}"))?;

        // Validate heartbeat interval eagerly even though the runtime is
        // pending Phase 7 — better to fail at startup than silently ignore.
        if agent_cfg.heartbeat.enabled {
            let interval =
                humantime::parse_duration(&agent_cfg.heartbeat.interval).with_context(|| {
                    format!(
                        "invalid heartbeat.interval `{}` for agent {agent_id}",
                        agent_cfg.heartbeat.interval
                    )
                })?;
            tracing::info!(
                agent = %agent_id,
                interval = ?interval,
                "heartbeat configured (runtime pending Phase 7)"
            );
        }

        let tools = Arc::new(ToolRegistry::new());
        tools.register(DelegationTool::tool_def(), DelegationTool);
        if agent_cfg.plugins.iter().any(|p| p == "memory") {
            if let Some(mem) = memory.clone() {
                tools.register(
                    MemoryTool::tool_def(),
                    MemoryTool::new_with_default_mode(
                        mem,
                        cfg.memory.vector.default_recall_mode.clone(),
                    ),
                );
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "agent requests `memory` plugin but long-term memory is disabled"
                );
            }
        }
        // Register the full browser_* tool family when the agent opts
        // in via `plugins: [browser]`. Tools call the shared
        // `Arc<BrowserPlugin>` directly so every LLM invocation hits
        // the CDP session exactly once (no broker round-trip).
        if agent_cfg.plugins.iter().any(|p| p == "browser") {
            if let Some(plugin) = browser_plugin.as_ref() {
                agent_plugin_browser::register_browser_tools(&tools, plugin);
                tracing::info!(
                    agent = %agent_id,
                    "registered browser_* tools for agent"
                );
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "agent requests `browser` plugin but config/plugins/browser.yaml is absent"
                );
            }
        }
        // WhatsApp outbound tools — gated on `plugins: [whatsapp]`.
        // Tools publish to `plugin.outbound.whatsapp`; the plugin's
        // dispatcher handles transport. Each tool honors the agent's
        // `outbound_allowlist.whatsapp` at call time.
        if agent_cfg.plugins.iter().any(|p| p == "whatsapp") {
            agent_plugin_whatsapp::register_whatsapp_tools(&tools);
            tracing::info!(agent = %agent_id, "registered whatsapp_* tools for agent");
        }
        // Telegram outbound tools — same shape as WhatsApp; gated on
        // `plugins: [telegram]` + per-agent allowlist.
        if agent_cfg.plugins.iter().any(|p| p == "telegram") {
            agent_plugin_telegram::register_telegram_tools(&tools);
            tracing::info!(agent = %agent_id, "registered telegram_* tools for agent");
        }
        // Google OAuth tools — gated on `agents.<id>.google_auth` being
        // set. The client holds the refresh_token on disk at
        // `<workspace>/<token_file>` so consent only runs once.
        if let Some(gcfg) = agent_cfg.google_auth.as_ref() {
            let core_cfg = agent_plugin_google::GoogleAuthConfig {
                client_id: gcfg.client_id.clone(),
                client_secret: gcfg.client_secret.clone(),
                scopes: gcfg.scopes.clone(),
                token_file: gcfg.token_file.clone(),
                redirect_port: gcfg.redirect_port,
            };
            let workspace_dir = if agent_cfg.workspace.trim().is_empty() {
                PathBuf::from("./data/workspace")
            } else {
                PathBuf::from(&agent_cfg.workspace)
            };
            let client = agent_plugin_google::GoogleAuthClient::new(core_cfg, &workspace_dir);
            if let Err(e) = client.load_from_disk().await {
                tracing::warn!(
                    agent = %agent_id,
                    error = %e,
                    "google_auth: failed to load persisted tokens; agent will need to re-consent"
                );
            }
            agent_plugin_google::register_tools(&tools, client);
            tracing::info!(
                agent = %agent_id,
                "registered google_* tools for agent"
            );
        }
        if agent_cfg.heartbeat.enabled {
            if let Some(mem) = memory.clone() {
                tools.register(HeartbeatTool::tool_def(), HeartbeatTool::new(mem));
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "agent has heartbeat enabled but long-term memory is disabled; reminders unavailable"
                );
            }
        }

        // Phase 10.8 — self-report tools. `who_am_i` + `what_do_i_know` are
        // pure workspace reads; `my_stats` additionally needs long-term memory.
        let workspace_path: Option<PathBuf> = if agent_cfg.workspace.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(&agent_cfg.workspace))
        };
        tools.register(
            WhoAmITool::tool_def(),
            WhoAmITool::new(
                agent_id.clone(),
                agent_cfg.model.model.clone(),
                workspace_path.clone(),
            ),
        );
        tools.register(
            WhatDoIKnowTool::tool_def(),
            WhatDoIKnowTool::new(workspace_path.clone()),
        );
        if let Some(mem) = memory.clone() {
            tools.register(
                MyStatsTool::tool_def(),
                MyStatsTool::new(mem, workspace_path.clone()),
            );
        }

        // Phase 10.9 — optional git-backed workspace. Registers
        // `forge_memory_checkpoint` + `memory_history` tools and feeds the
        // dreaming spawn closure below so sweeps auto-commit.
        let agent_git: Option<Arc<agent_core::agent::MemoryGitRepo>> =
            if agent_cfg.workspace_git.enabled {
                match workspace_path.as_deref() {
                    Some(ws) => match agent_core::agent::MemoryGitRepo::open_or_init(
                        ws,
                        agent_cfg.workspace_git.author_name.clone(),
                        agent_cfg.workspace_git.author_email.clone(),
                    ) {
                        Ok(repo) => {
                            tracing::info!(
                                agent = %agent_id,
                                root = %ws.display(),
                                "workspace git ready"
                            );
                            Some(Arc::new(repo))
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent = %agent_id,
                                error = %e,
                                "workspace git init failed; continuing without"
                            );
                            None
                        }
                    },
                    None => {
                        tracing::warn!(
                            agent = %agent_id,
                            "workspace_git.enabled=true but agent.workspace is empty — skipping"
                        );
                        None
                    }
                }
            } else {
                None
            };
        if let Some(g) = &agent_git {
            tools.register(
                agent_core::agent::MemoryCheckpointTool::tool_def(),
                agent_core::agent::MemoryCheckpointTool::new(Arc::clone(g)),
            );
            tools.register(
                agent_core::agent::MemoryHistoryTool::tool_def(),
                agent_core::agent::MemoryHistoryTool::new(Arc::clone(g)),
            );
            // Wire session-close commit: when a session expires, snapshot
            // the workspace so the day's memory edits land in history
            // even if the agent never hit a dreaming sweep.
            let git = Arc::clone(g);
            let aid = agent_id.clone();
            sessions.on_expire(move |sid| {
                let git = Arc::clone(&git);
                let aid = aid.clone();
                tokio::task::spawn_blocking(move || {
                    let subject = format!("session-close: {sid}");
                    let body = format!("agent={aid}");
                    if let Err(e) = git.commit_all(&subject, &body) {
                        tracing::warn!(
                            agent = %aid,
                            session = %sid,
                            error = %e,
                            "session-close commit failed"
                        );
                    }
                });
            });
        }

        // Phase 11.5 — extension tools. Each discovered-and-spawned extension
        // contributes its declared tools to this agent's registry.
        // Phase 11.6 — extension hooks built alongside, same iteration.
        let hooks = Arc::new(HookRegistry::new());
        let mut tools_registered = 0usize;
        let mut tools_skipped = 0usize;
        let mut hooks_registered = 0usize;
        let mut hooks_skipped = 0usize;
        for (rt, cand) in &extension_runtimes {
            let pid = cand.manifest.id();
            for desc in &rt.handshake().tools {
                let def = ExtensionTool::tool_def(desc, pid);
                let full_name = def.name.clone();
                let handler = ExtensionTool::new(pid, desc.name.clone(), Arc::clone(rt))
                    .with_descriptor_metadata(desc.description.clone(), desc.input_schema.clone())
                    .with_context_passthrough(cand.manifest.context.passthrough);
                if tools.register_if_absent(def, handler) {
                    tools_registered += 1;
                    tracing::info!(
                        agent = %agent_id,
                        ext = %pid,
                        tool = %full_name,
                        "extension tool registered"
                    );
                } else {
                    tools_skipped += 1;
                    tracing::warn!(
                        agent = %agent_id,
                        ext = %pid,
                        tool = %full_name,
                        "extension tool skipped: name already registered"
                    );
                }
            }
            for hook_name in &rt.handshake().hooks {
                if !agent_extensions::is_valid_hook(hook_name) {
                    hooks_skipped += 1;
                    tracing::warn!(
                        ext = %pid,
                        hook = %hook_name,
                        "unknown hook; skipping registration"
                    );
                    continue;
                }
                hooks.register(hook_name, pid, ExtensionHook::new(pid, Arc::clone(rt)));
                hooks_registered += 1;
                tracing::info!(
                    agent = %agent_id,
                    ext = %pid,
                    hook = %hook_name,
                    "extension hook registered"
                );
            }
        }
        if !extension_runtimes.is_empty() {
            tracing::info!(
                agent = %agent_id,
                extensions = extension_runtimes.len(),
                tools_registered,
                tools_skipped,
                hooks_registered,
                hooks_skipped,
                "extension registration summary"
            );
        }

        // Phase 12 — register MCP tools for this agent. Shared sentinel
        // session so every agent sees the same live clients; catalog built
        // lazily on first `register_session_tools` call.
        if let Some(mgr) = &mcp_manager {
            let rt = mgr.get_or_create(MCP_SHARED_SESSION).await;
            let mcp_ctx_pt = cfg
                .mcp
                .as_ref()
                .map(|m| m.context.passthrough)
                .unwrap_or(false);
            let mcp_overrides: std::collections::HashMap<String, bool> = cfg
                .mcp
                .as_ref()
                .map(|m| {
                    m.servers
                        .iter()
                        .filter_map(|(name, yaml)| match yaml {
                            agent_config::McpServerYaml::Stdio {
                                context_passthrough: Some(v),
                                ..
                            }
                            | agent_config::McpServerYaml::StreamableHttp {
                                context_passthrough: Some(v),
                                ..
                            }
                            | agent_config::McpServerYaml::Sse {
                                context_passthrough: Some(v),
                                ..
                            } => Some((name.clone(), *v)),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            agent_core::agent::register_session_tools_with_overrides(
                &rt,
                &tools,
                mcp_ctx_pt,
                mcp_overrides.clone(),
            )
            .await;
            tracing::info!(
                agent = %agent_id,
                total_tools = tools.to_tool_defs().len(),
                mcp_context_passthrough = mcp_ctx_pt,
                mcp_overrides = mcp_overrides.len(),
                "mcp tools registered"
            );

            // Phase 12.8 — hot-reload: when a server pushes
            // `notifications/tools/list_changed`, drop its prefix from the
            // registry and rebuild the session catalog. Closures are fired
            // after a 200 ms debounce window by SessionMcpRuntime.
            let tools_for_tools_reload = Arc::clone(&tools);
            let rt_for_tools_reload = Arc::clone(&rt);
            let agent_id_for_tools_reload = agent_id.to_string();
            let overrides_for_tools_reload = mcp_overrides.clone();
            rt.on_tools_changed(move |server_id| {
                let prefix = format!(
                    "mcp_{}_",
                    agent_core::agent::sanitize_name_fragment(&server_id)
                );
                let tools = Arc::clone(&tools_for_tools_reload);
                let rt = Arc::clone(&rt_for_tools_reload);
                let agent_id = agent_id_for_tools_reload.clone();
                let overrides = overrides_for_tools_reload.clone();
                tokio::spawn(async move {
                    let removed = tools.clear_by_prefix(&prefix);
                    agent_core::agent::register_session_tools_with_overrides(
                        &rt, &tools, mcp_ctx_pt, overrides,
                    )
                    .await;
                    tracing::info!(
                        agent = %agent_id,
                        mcp_server = %server_id,
                        removed,
                        total_tools = tools.to_tool_defs().len(),
                        "mcp tools hot-reloaded"
                    );
                });
            });

            // Same reload path for resources: rebuilding the session
            // catalog also re-registers resource meta-tools. Safe to call
            // concurrently with the tools callback because
            // `register_session_tools` is idempotent.
            let tools_for_res_reload = Arc::clone(&tools);
            let rt_for_res_reload = Arc::clone(&rt);
            let agent_id_for_res_reload = agent_id.to_string();
            let overrides_for_res_reload = mcp_overrides.clone();
            rt.on_resources_changed(move |server_id| {
                let prefix = format!(
                    "mcp_{}_",
                    agent_core::agent::sanitize_name_fragment(&server_id)
                );
                let tools = Arc::clone(&tools_for_res_reload);
                let rt = Arc::clone(&rt_for_res_reload);
                let agent_id = agent_id_for_res_reload.clone();
                let overrides = overrides_for_res_reload.clone();
                tokio::spawn(async move {
                    let cache_purged = rt.resource_cache().invalidate_server(&server_id);
                    let removed = tools.clear_by_prefix(&prefix);
                    agent_core::agent::register_session_tools_with_overrides(
                        &rt, &tools, mcp_ctx_pt, overrides,
                    )
                    .await;
                    tracing::info!(
                        agent = %agent_id,
                        mcp_server = %server_id,
                        removed,
                        cache_purged,
                        total_tools = tools.to_tool_defs().len(),
                        "mcp resources hot-reloaded"
                    );
                });
            });
            tracing::debug!(agent = %agent_id, "mcp hot-reload wired");
        }

        // Apply the agent-level tool allowlist ONLY for legacy agents
        // (no inbound_bindings). With bindings present, each binding
        // carries its own `allowed_tools` override via
        // EffectiveBindingPolicy; pruning the base registry here would
        // cap every binding below the agent-level list, making
        // `binding.allowed_tools: ["*"]` (or any expansion beyond the
        // agent list) silently lose tools. Per-binding enforcement
        // happens in llm_behavior at turn time instead, keeping the
        // registry authoritative and letting bindings narrow AND
        // expand freely within it.
        if agent_cfg.inbound_bindings.is_empty() && !agent_cfg.allowed_tools.is_empty() {
            let removed = tools.retain_matching(&agent_cfg.allowed_tools);
            tracing::info!(
                agent = %agent_id,
                kept = tools.to_tool_defs().len(),
                removed,
                patterns = ?agent_cfg.allowed_tools,
                "per-agent tool allowlist applied (legacy, no bindings)",
            );
        }

        let mut behavior = LlmAgentBehavior::new(llm, Arc::clone(&tools))
            .with_hooks(Arc::clone(&hooks))
            .with_tool_policy(tool_policy_registry.for_agent(&agent_id));
        if let Some(rl_cfg) = agent_cfg.tool_rate_limits.clone() {
            let rl_core = agent_core::agent::ToolRateLimitsConfig {
                patterns: rl_cfg
                    .patterns
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            agent_core::agent::ToolRateLimitConfig {
                                rps: v.rps,
                                burst: v.burst,
                            },
                        )
                    })
                    .collect(),
            };
            let limiter = Arc::new(agent_core::agent::ToolRateLimiter::new(rl_core));
            behavior = behavior.with_rate_limiter(limiter);
            tracing::info!(agent = %agent_id, "tool rate limiter enabled");
        }
        // Phase 9.2 follow-up — JSON Schema args validation.
        {
            let enabled = agent_cfg
                .tool_args_validation
                .as_ref()
                .map(|c| c.enabled)
                .unwrap_or(true);
            let validator = Arc::new(agent_core::agent::ToolArgsValidator::new(enabled));
            behavior = behavior.with_schema_validator(validator);
            tracing::info!(
                agent = %agent_id,
                schema_validation = enabled,
                "tool schema validator attached"
            );
        }
        let agent = Arc::new(Agent::new(agent_cfg, behavior));

        let mut runtime =
            AgentRuntime::new(Arc::clone(&agent), broker.clone(), Arc::clone(&sessions));
        if let Some(mem) = memory.clone() {
            runtime = runtime.with_memory(mem);
        }
        runtime = runtime.with_peers(Arc::clone(&peer_directory));
        runtime
            .start()
            .await
            .with_context(|| format!("failed to start agent runtime for {agent_id}"))?;
        running_agents.fetch_add(1, Ordering::Relaxed);
        tracing::info!(agent = %agent_id, "agent runtime started");
        runtimes.push(runtime);

        // Dreaming (Phase 10.6) — when enabled and long-term memory + workspace
        // are both available, spawn a periodic sweep. Fires one immediate sweep
        // on boot so new installs get a useful DREAMS.md right away; subsequent
        // runs honor `interval_secs`.
        let dream_cfg: DreamingConfig = dream_yaml.into();
        if dream_cfg.enabled {
            let workspace = workspace_for_dream.trim().to_string();
            if workspace.is_empty() {
                tracing::warn!(
                    agent = %agent_id,
                    "dreaming enabled but workspace path is empty — skipping sweep"
                );
            } else if let Some(mem) = memory.clone() {
                let agent_id_owned = agent_id.to_string();
                let interval = std::time::Duration::from_secs(dream_cfg.interval_secs.max(60));
                // Hard ceiling on one sweep. If the memory store or
                // embedding API stalls, the sweep drops rather than
                // pinning the loop forever — the next interval picks
                // up from a clean slate.
                const SWEEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
                let git_for_dream = agent_git.clone();
                let dream_cancel = dream_shutdown.clone();
                let handle = tokio::spawn(async move {
                    let engine = DreamEngine::new(mem, workspace, dream_cfg);
                    let mut first = true;
                    // Exponential backoff on consecutive failures to
                    // avoid log spam when the memory store or embedding
                    // API is down. Resets to 0 after a clean sweep.
                    let mut consecutive_failures: u32 = 0;
                    const MAX_BACKOFF: std::time::Duration =
                        std::time::Duration::from_secs(4 * 3600);
                    loop {
                        if !first {
                            let wait = if consecutive_failures == 0 {
                                interval
                            } else {
                                let shift = consecutive_failures.min(6);
                                interval.saturating_mul(1u32 << shift).min(MAX_BACKOFF)
                            };
                            tokio::select! {
                                _ = dream_cancel.cancelled() => break,
                                _ = tokio::time::sleep(wait) => {}
                            }
                        }
                        first = false;
                        let sweep = engine.run_sweep(&agent_id_owned);
                        let result = tokio::select! {
                            _ = dream_cancel.cancelled() => break,
                            r = tokio::time::timeout(SWEEP_TIMEOUT, sweep) => r,
                        };
                        let result = match result {
                            Ok(r) => r,
                            Err(_) => {
                                consecutive_failures = consecutive_failures.saturating_add(1);
                                tracing::warn!(
                                    agent = %agent_id_owned,
                                    timeout_secs = SWEEP_TIMEOUT.as_secs(),
                                    consecutive_failures,
                                    "dream sweep timed out; backing off"
                                );
                                continue;
                            }
                        };
                        match result {
                            Ok(report) => {
                                consecutive_failures = 0;
                                tracing::info!(
                                    agent = %agent_id_owned,
                                    candidates = report.candidates_considered,
                                    promoted = report.promoted.len(),
                                    "dream sweep completed"
                                );
                                // Phase 10.9 — auto-commit workspace changes.
                                if let Some(g) = git_for_dream.clone() {
                                    if !report.promoted.is_empty() {
                                        let subject = format!(
                                            "dream: {} promotion(s)",
                                            report.promoted.len()
                                        );
                                        let body: String = report
                                            .promoted
                                            .iter()
                                            .take(20)
                                            .map(|p| {
                                                let snippet: String =
                                                    p.content.chars().take(80).collect();
                                                format!("- {snippet}")
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        let agent = agent_id_owned.clone();
                                        let _ = tokio::task::spawn_blocking(move || {
                                            if let Err(e) = g.commit_all(&subject, &body) {
                                                tracing::warn!(
                                                    agent = %agent,
                                                    error = %e,
                                                    "dream commit failed"
                                                );
                                            }
                                        })
                                        .await;
                                    }
                                }
                            }
                            Err(e) => {
                                consecutive_failures = consecutive_failures.saturating_add(1);
                                tracing::error!(
                                    agent = %agent_id_owned,
                                    error = %e,
                                    consecutive_failures,
                                    "dream sweep failed"
                                );
                            }
                        }
                    }
                    tracing::debug!(agent = %agent_id_owned, "dream sweep loop exited");
                });
                dream_handles.push(handle);
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "dreaming enabled but long-term memory is disabled — skipping"
                );
            }
        }
    }

    tracing::info!("agent ready — waiting for shutdown signal (SIGTERM / Ctrl+C)");
    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping");

    // Stop the mcp config watcher (no-op if it was disabled).
    watcher_shutdown.cancel();

    // Signal dreaming sweep loops to exit and give them a short window
    // to drop in-flight sweeps cleanly. After the deadline the
    // detached tasks are abandoned — `kill_on_drop` handles any child
    // processes they may have spawned via spawn_blocking.
    if !dream_handles.is_empty() {
        dream_shutdown.cancel();
        let join_all = futures::future::join_all(dream_handles.drain(..));
        if tokio::time::timeout(std::time::Duration::from_secs(5), join_all)
            .await
            .is_err()
        {
            tracing::warn!("dream sweeps still running after 5s; abandoning");
        }
    }

    // Mark not-ready immediately so readiness probes stop routing traffic
    // while we drain in-flight work.
    running_agents.store(0, Ordering::Relaxed);

    // Stop plugin intake first to avoid accepting new inbound events during drain.
    if let Err(e) = plugins.stop_all().await {
        tracing::error!(error = %e, "plugin shutdown error");
    }

    // Shut down the MCP runtime before draining agents: in-flight tool calls
    // that are routed through MCP will cancel cleanly and the agents get
    // proper `TransportLost` errors instead of timing out.
    if let Some(mgr) = mcp_manager.clone() {
        tracing::info!("shutting down mcp runtime manager");
        mgr.shutdown_all_with_reason("sigterm").await;
    }

    // Phase 11.3 — graceful extension shutdown. Send the `shutdown`
    // notification to every live extension and give them up to 5s to
    // close on their own. Anything still running after that is killed by
    // `StdioRuntime::Drop` via `kill_on_drop`. Sits after MCP shutdown
    // because extensions may bundle MCP servers those clients were using.
    if !extension_runtimes.is_empty() {
        tracing::info!(count = extension_runtimes.len(), "shutting down extensions");
        let shutdown_fut = futures::future::join_all(
            extension_runtimes
                .iter()
                .map(|(rt, _)| rt.shutdown_with_reason("sigterm")),
        );
        if tokio::time::timeout(std::time::Duration::from_secs(5), shutdown_fut)
            .await
            .is_err()
        {
            tracing::warn!(
                "extension shutdown timeout after 5s; remaining children terminated via kill_on_drop"
            );
        }
    }

    // Then stop runtimes; each runtime drains buffered in-flight messages.
    for rt in &runtimes {
        rt.stop().await;
    }

    metrics_handle.abort();
    health_handle.abort();

    Ok(())
}

fn build_mcp_sampling_provider(
    cfg: &AppConfig,
    llm_registry: &LlmRegistry,
) -> anyhow::Result<Option<Arc<dyn agent_mcp::sampling::SamplingProvider>>> {
    let Some(mcp_cfg) = cfg.mcp.as_ref() else {
        return Ok(None);
    };
    if !mcp_cfg.enabled || !mcp_cfg.sampling.enabled {
        return Ok(None);
    }
    if cfg.agents.agents.is_empty() {
        tracing::warn!("mcp.sampling.enabled=true but no agents are configured; sampling disabled");
        return Ok(None);
    }

    let mut named: std::collections::HashMap<String, Arc<dyn agent_llm::LlmClient>> =
        std::collections::HashMap::new();
    let mut default_client: Option<Arc<dyn agent_llm::LlmClient>> = None;
    for (idx, agent_cfg) in cfg.agents.agents.iter().enumerate() {
        let client = llm_registry
            .build(&cfg.llm, &agent_cfg.model)
            .with_context(|| {
                format!(
                    "failed to build sampling client for agent `{}` (provider={}, model={})",
                    agent_cfg.id, agent_cfg.model.provider, agent_cfg.model.model
                )
            })?;
        if idx == 0 {
            default_client = Some(client.clone());
        }
        named
            .entry(agent_cfg.model.provider.clone())
            .or_insert_with(|| client.clone());
        named
            .entry(agent_cfg.model.model.clone())
            .or_insert_with(|| client.clone());
        named
            .entry(format!(
                "{}/{}",
                agent_cfg.model.provider, agent_cfg.model.model
            ))
            .or_insert_with(|| client.clone());
    }
    let mut default_client = default_client
        .ok_or_else(|| anyhow::anyhow!("mcp.sampling: failed to resolve default client"))?;
    if let Some(hint) = mcp_cfg.sampling.default_hint.as_deref() {
        if let Some(c) = named.get(hint) {
            default_client = c.clone();
        } else {
            tracing::warn!(
                hint = %hint,
                "mcp.sampling.default_hint not found in named clients; using first agent model"
            );
        }
    }

    let per_server: std::collections::HashMap<String, agent_mcp::sampling::PerServerPolicy> =
        mcp_cfg
            .sampling
            .per_server
            .iter()
            .map(|(server, p)| {
                (
                    server.clone(),
                    agent_mcp::sampling::PerServerPolicy {
                        enabled: p.enabled,
                        rate_limit_per_minute: p.rate_limit_per_minute,
                        max_tokens_cap: p.max_tokens_cap,
                    },
                )
            })
            .collect();

    let policy = agent_mcp::sampling::SamplingPolicy::new(
        mcp_cfg.sampling.enabled,
        mcp_cfg.sampling.deny_servers.clone(),
        mcp_cfg.sampling.global_max_tokens_cap,
        per_server,
    );
    tracing::info!(
        named_clients = named.len(),
        default_hint = ?mcp_cfg.sampling.default_hint,
        "mcp sampling provider enabled"
    );
    Ok(Some(
        Arc::new(agent_mcp::sampling::DefaultSamplingProvider::new(
            default_client,
            named,
            policy,
        )) as Arc<dyn agent_mcp::sampling::SamplingProvider>,
    ))
}

/// Phase 11.2/11.3 — discover extensions and spawn stdio runtimes.
/// Never fatal: bad manifests or spawn failures produce diagnostics; the
/// agent keeps starting. Returns runtimes that the caller must keep alive
/// (drop → cascades SIGTERM to extension children).
#[allow(clippy::type_complexity)]
async fn run_extension_discovery(
    cfg: Option<&agent_config::ExtensionsConfig>,
) -> (
    Vec<(
        Arc<agent_extensions::StdioRuntime>,
        agent_extensions::ExtensionCandidate,
    )>,
    Vec<agent_extensions::ExtensionMcpDecl>,
) {
    let cfg = cfg.cloned().unwrap_or_default();
    if !cfg.enabled {
        tracing::info!("extension system disabled via config");
        return (Vec::new(), Vec::new());
    }

    let search_paths: Vec<PathBuf> = cfg.search_paths.iter().map(PathBuf::from).collect();
    let discovery = agent_extensions::ExtensionDiscovery::new(
        search_paths,
        cfg.ignore_dirs.clone(),
        cfg.disabled.clone(),
        cfg.allowlist.clone(),
        cfg.max_depth,
    );
    let report = discovery.discover();
    add_extensions_discovered("ok", report.candidates.len() as u64);
    add_extensions_discovered("disabled", report.disabled_count as u64);
    add_extensions_discovered("invalid", report.invalid_count as u64);

    for d in &report.diagnostics {
        match d.level {
            agent_extensions::DiagnosticLevel::Warn => tracing::warn!(
                path = %d.path.display(),
                message = %d.message,
                "extension discovery",
            ),
            agent_extensions::DiagnosticLevel::Error => tracing::error!(
                path = %d.path.display(),
                message = %d.message,
                "extension discovery",
            ),
        }
    }
    for c in &report.candidates {
        let transport = match &c.manifest.transport {
            agent_extensions::Transport::Stdio { .. } => "stdio",
            agent_extensions::Transport::Nats { .. } => "nats",
            agent_extensions::Transport::Http { .. } => "http",
        };
        tracing::info!(
            id = %c.manifest.id(),
            version = %c.manifest.version(),
            transport = transport,
            path = %c.root_dir.display(),
            "discovered extension",
        );
    }
    tracing::info!(
        extensions = report.candidates.len(),
        scanned_dirs = report.scanned_dirs,
        diagnostics = report.diagnostics.len(),
        "extension discovery complete",
    );

    // 12.7 — collect extension-declared MCP servers before we consume the
    // candidate list; main() later feeds these into `McpRuntimeManager`.
    let mcp_decls = agent_extensions::collect_mcp_declarations(&report, &cfg.disabled);

    // 11.3 — spawn stdio runtimes for each candidate whose transport is Stdio.
    // 11.5 will iterate the returned runtimes to register tools per agent.
    let mut runtimes: Vec<(
        Arc<agent_extensions::StdioRuntime>,
        agent_extensions::ExtensionCandidate,
    )> = Vec::new();
    for c in report.candidates {
        if !matches!(
            c.manifest.transport,
            agent_extensions::Transport::Stdio { .. }
        ) {
            continue;
        }
        let id = c.manifest.id().to_string();
        // Gate: skip spawn when declared `requires.bins` or `requires.env`
        // are missing. Prevents tools from being registered with an agent
        // only to fail on every invocation with an opaque PATH/env error.
        let (missing_bins, missing_env) = c.manifest.requires.missing();
        if !missing_bins.is_empty() || !missing_env.is_empty() {
            tracing::warn!(
                ext = %id,
                missing_bins = ?missing_bins,
                missing_env = ?missing_env,
                "extension skipped: declared preconditions not satisfied"
            );
            continue;
        }
        match agent_extensions::StdioRuntime::spawn(&c.manifest, c.root_dir.clone()).await {
            Ok(rt) => {
                tracing::info!(
                    ext = %id,
                    tools = rt.handshake().tools.len(),
                    "extension runtime ready",
                );
                runtimes.push((Arc::new(rt), c));
            }
            Err(e) => {
                tracing::error!(ext=%id, error=%e, "extension spawn failed");
            }
        }
    }
    (runtimes, mcp_decls)
}

/// RAII handle for the agent's single-instance lockfile.
/// Removes the file on drop — but only if the PID inside still matches
/// ours, so a second-instance takeover doesn't wipe the new owner's lock.
struct SingleInstanceLock {
    path: PathBuf,
    pid: u32,
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        if let Ok(contents) = std::fs::read_to_string(&self.path) {
            if contents.trim().parse::<u32>().ok() == Some(self.pid) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn acquire_single_instance_lock() -> Result<SingleInstanceLock> {
    // Path kept stable regardless of --config so two configs against the
    // same cwd still collide (that's the case that caused dupes).
    let lock_path = PathBuf::from("./data/agent.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    if let Ok(contents) = std::fs::read_to_string(&lock_path) {
        if let Ok(prev_pid) = contents.trim().parse::<u32>() {
            if pid_alive(prev_pid) {
                tracing::warn!(prev_pid, "existing agent instance detected — terminating");
                terminate_pid(prev_pid);
                // Give it up to 5s to exit cleanly, then SIGKILL.
                for _ in 0..50 {
                    if !pid_alive(prev_pid) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if pid_alive(prev_pid) {
                    tracing::warn!(prev_pid, "previous agent still alive — SIGKILL");
                    kill_pid(prev_pid);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            } else {
                tracing::info!(prev_pid, "stale agent lockfile — overwriting");
            }
        }
    }

    let pid = std::process::id();
    std::fs::write(&lock_path, pid.to_string())
        .with_context(|| format!("write lockfile {}", lock_path.display()))?;
    tracing::info!(path = %lock_path.display(), pid, "acquired single-instance lock");
    Ok(SingleInstanceLock { path: lock_path, pid })
}

fn pid_alive(pid: u32) -> bool {
    // /proc/<pid> exists iff the process is alive on Linux. Good enough
    // for single-instance detection without pulling in a libc dep.
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn terminate_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
}

fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // Always write to stderr. stdout is reserved for wire protocols
    // (`agent mcp-server` uses it for JSON-RPC), and standard Unix
    // convention puts diagnostics on stderr anyway.
    match parse_log_format() {
        LogFormat::Pretty => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(env_filter)
                .with_target(true)
                .with_thread_ids(true)
                .init();
        }
        LogFormat::Compact => {
            tracing_subscriber::fmt()
                .compact()
                .with_writer(std::io::stderr)
                .with_env_filter(env_filter)
                .with_target(true)
                .with_thread_ids(true)
                .init();
        }
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(JsonLogLayer)
                .init();
        }
    }
}

fn parse_log_format() -> LogFormat {
    if let Ok(value) = std::env::var("AGENT_LOG_FORMAT") {
        let normalized = value.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "pretty" => LogFormat::Pretty,
            "compact" => LogFormat::Compact,
            "json" => LogFormat::Json,
            other => {
                eprintln!(
                    "unknown AGENT_LOG_FORMAT=`{other}`; expected pretty|compact|json; defaulting to pretty"
                );
                LogFormat::Pretty
            }
        };
    }

    match std::env::var("AGENT_ENV") {
        Ok(v)
            if matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "prod" | "production"
            ) =>
        {
            LogFormat::Json
        }
        _ => LogFormat::Pretty,
    }
}

fn parse_args() -> CliArgs {
    let mut config_dir = PathBuf::from("./config");
    let mut positional: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                if let Some(path) = args.next() {
                    config_dir = PathBuf::from(path);
                }
            }
            "--help" | "-h" => {
                return CliArgs {
                    config_dir,
                    mode: Mode::Help,
                }
            }
            other => positional.push(other.to_string()),
        }
    }

    let has_json_flag = positional.iter().any(|a| a == "--json");
    let pos_no_flags: Vec<String> = positional
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();

    // --dry-run is a flag, not a positional. Handle before the match
    // so `agent --dry-run` works without a subcommand slot.
    let dry_run_flag =
        positional.iter().any(|a| a == "--dry-run") && !positional.iter().any(|a| a == "ext"); // `ext install --dry-run` already exists; leave that alone
    if dry_run_flag && pos_no_flags.is_empty() {
        return CliArgs {
            config_dir,
            mode: Mode::DryRun {
                json: has_json_flag,
            },
        };
    }

    let mode = match pos_no_flags.as_slice() {
        [] => Mode::Run,
        [cmd] if cmd == "dlq" => {
            eprintln!("error: `dlq` requires a subcommand (list|replay <id>|purge)");
            Mode::Help
        }
        [cmd, sub] if cmd == "dlq" && sub == "list" => Mode::DlqList,
        [cmd, sub] if cmd == "dlq" && sub == "purge" => Mode::DlqPurge,
        [cmd, sub, id] if cmd == "dlq" && sub == "replay" => Mode::DlqReplay(id.clone()),
        [cmd] if cmd == "ext" => Mode::ExtHelp,
        [cmd, sub] if cmd == "ext" && sub == "list" => Mode::ExtList {
            json: has_json_flag,
        },
        [cmd, sub] if cmd == "ext" && sub == "doctor" => Mode::ExtDoctor {
            runtime: positional.iter().any(|a| a == "--runtime"),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "ext" && sub == "info" => Mode::ExtInfo {
            id: id.clone(),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "ext" && sub == "enable" => Mode::ExtEnable { id: id.clone() },
        [cmd, sub, id] if cmd == "ext" && sub == "disable" => Mode::ExtDisable { id: id.clone() },
        [cmd, sub, p] if cmd == "ext" && sub == "validate" => Mode::ExtValidate {
            path: PathBuf::from(p),
        },
        [cmd, sub, p] if cmd == "ext" && sub == "install" => Mode::ExtInstall {
            source: PathBuf::from(p),
            update: positional.iter().any(|a| a == "--update"),
            enable: positional.iter().any(|a| a == "--enable"),
            dry_run: positional.iter().any(|a| a == "--dry-run"),
            link: positional.iter().any(|a| a == "--link"),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "ext" && sub == "uninstall" => Mode::ExtUninstall {
            id: id.clone(),
            yes: positional.iter().any(|a| a == "--yes"),
            json: has_json_flag,
        },
        [cmd] if cmd == "mcp-server" => Mode::McpServer,
        [cmd] if cmd == "flow" => Mode::FlowHelp,
        [cmd, sub] if cmd == "flow" && sub == "list" => Mode::FlowList {
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "flow" && sub == "show" => Mode::FlowShow {
            id: id.clone(),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "flow" && sub == "cancel" => Mode::FlowCancel { id: id.clone() },
        [cmd, sub, id] if cmd == "flow" && sub == "resume" => Mode::FlowResume { id: id.clone() },
        [cmd] if cmd == "setup" => Mode::SetupInteractive,
        [cmd, sub] if cmd == "setup" && sub == "list" => Mode::SetupList,
        [cmd, sub] if cmd == "setup" && sub == "doctor" => Mode::SetupDoctor,
        [cmd, sub] if cmd == "setup" && sub == "telegram-link" => Mode::SetupTelegramLink,
        [cmd, service] if cmd == "setup" => Mode::SetupOne {
            service: service.clone(),
        },
        [cmd] if cmd == "status" => Mode::Status {
            json: has_json_flag,
            endpoint: positional
                .iter()
                .find_map(|a| a.strip_prefix("--endpoint=").map(|s| s.to_string())),
            agent_id: None,
        },
        [cmd, id] if cmd == "status" => Mode::Status {
            json: has_json_flag,
            endpoint: positional
                .iter()
                .find_map(|a| a.strip_prefix("--endpoint=").map(|s| s.to_string())),
            agent_id: Some(id.clone()),
        },
        _ => {
            eprintln!("error: unknown command `{}`", pos_no_flags.join(" "));
            Mode::Help
        }
    };

    CliArgs { config_dir, mode }
}

fn print_usage() {
    println!("agent — multi-agent runtime");
    println!();
    println!("USAGE:");
    println!("  agent [--config <dir>]                 Start the daemon (default)");
    println!("  agent [--config <dir>] dlq list        List entries in the dead-letter queue");
    println!("  agent [--config <dir>] dlq replay <id> Replay a dead-lettered event");
    println!("  agent [--config <dir>] dlq purge       Delete all dead-letter entries");
    println!("  agent [--config <dir>] ext <sub> ...   Extension admin (run `agent ext` for help)");
    println!(
        "  agent [--config <dir>] ext install <path> [--update|--link|--enable|--dry-run|--json]"
    );
    println!("  agent [--config <dir>] ext uninstall <id> --yes [--json]");
    println!("  agent [--config <dir>] ext doctor [--runtime] [--json]");
    println!("  agent flow <sub> ...                   TaskFlow admin (run `agent flow` for help)");
    println!("  agent status [<id>] [--json] [--endpoint=URL] Pretty-print running agents (or one by id)");
    println!(
        "  agent --dry-run [--json]               Validate config and print a summary (no runtime)"
    );
}

enum ExtCmd {
    List {
        json: bool,
    },
    Info {
        id: String,
        json: bool,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Validate {
        path: PathBuf,
    },
    Doctor {
        runtime: bool,
        json: bool,
    },
    Install {
        source: PathBuf,
        update: bool,
        enable: bool,
        dry_run: bool,
        link: bool,
        json: bool,
    },
    Uninstall {
        id: String,
        yes: bool,
        json: bool,
    },
}

fn run_ext_help() -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    agent_extensions::cli::print_help(&mut stdout)?;
    Ok(())
}

async fn run_mcp_server(config_dir: &std::path::Path) -> Result<()> {
    use agent_core::agent::self_report::WhoAmITool;
    use agent_core::agent::tool_registry::ToolRegistry;
    use agent_core::agent::{
        AgentContext, MemoryTool, MyStatsTool, ToolRegistryBridge, WhatDoIKnowTool,
    };
    use agent_core::session::SessionManager;
    use agent_mcp::{run_stdio_server_with_auth, McpServerInfo};
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // Phase 12.6 — tolerant loader: skip llm.yaml / broker.yaml / memory.yaml.
    // The operator exposing tools doesn't need a full runtime configured.
    let boot = agent_config::AppConfig::load_for_mcp_server(config_dir)
        .context("failed to load mcp-server config")?;
    let server_cfg = boot.mcp_server.clone().unwrap_or_default();
    if !server_cfg.enabled {
        eprintln!(
            "mcp-server is disabled in config/mcp_server.yaml (set `enabled: true` to opt in)."
        );
        return Ok(());
    }

    let primary = boot.agents.agents.first().ok_or_else(|| {
        anyhow::anyhow!("agents.yaml has no agents; cannot derive identity for mcp-server")
    })?;
    // AgentConfig lacks `Clone`; build a synthetic copy with the fields the
    // mcp-server context actually uses (id, model, workspace).
    let agent_cfg = Arc::new(agent_config::types::agents::AgentConfig {
        id: primary.id.clone(),
        model: agent_config::types::agents::ModelConfig {
            provider: primary.model.provider.clone(),
            model: primary.model.model.clone(),
        },
        plugins: primary.plugins.clone(),
        heartbeat: agent_config::types::agents::HeartbeatConfig::default(),
        config: agent_config::types::agents::AgentRuntimeConfig::default(),
        system_prompt: primary.system_prompt.clone(),
        workspace: primary.workspace.clone(),
        skills: primary.skills.clone(),
        skills_dir: primary.skills_dir.clone(),
        transcripts_dir: primary.transcripts_dir.clone(),
        dreaming: primary.dreaming.clone(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: primary.extra_docs.clone(),
        inbound_bindings: primary.inbound_bindings.clone(),
        allowed_tools: primary.allowed_tools.clone(),
        sender_rate_limit: primary.sender_rate_limit.clone(),
        allowed_delegates: primary.allowed_delegates.clone(),
        accept_delegates_from: primary.accept_delegates_from.clone(),
        description: primary.description.clone(),
        google_auth: primary.google_auth.clone(),
        outbound_allowlist: primary.outbound_allowlist.clone(),
    });
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(std::time::Duration::from_secs(300), 20));
    let ctx = AgentContext::new(primary.id.clone(), agent_cfg, broker, sessions);

    let workspace_dir = if primary.workspace.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&primary.workspace))
    };

    // Best-effort memory bootstrap for mcp-server mode: this subcommand
    // must remain tolerant when memory.yaml is absent/misconfigured.
    let mut memory_default_recall_mode = "keyword".to_string();
    let long_term_memory: Option<Arc<agent_memory::LongTermMemory>> =
        match agent_config::load_optional::<agent_config::types::MemoryConfig>(
            config_dir,
            "memory.yaml",
        ) {
            Ok(Some(mem_cfg)) => {
                memory_default_recall_mode = mem_cfg.vector.default_recall_mode.clone();
                if mem_cfg.long_term.backend == "sqlite" {
                    let path = mem_cfg
                        .long_term
                        .sqlite
                        .as_ref()
                        .map(|s| s.path.as_str())
                        .unwrap_or("./data/memory.db");
                    match agent_memory::LongTermMemory::open(path).await {
                        Ok(mem) => Some(Arc::new(mem)),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path,
                                "mcp-server memory bootstrap failed; memory tools disabled"
                            );
                            None
                        }
                    }
                } else {
                    tracing::warn!(
                        backend = %mem_cfg.long_term.backend,
                        "mcp-server supports sqlite memory bootstrap only; memory tools disabled"
                    );
                    None
                }
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to read memory.yaml for mcp-server; memory tools disabled"
                );
                None
            }
        };

    let registry = Arc::new(ToolRegistry::new());
    registry.register(
        WhoAmITool::tool_def(),
        WhoAmITool::new(&primary.id, &primary.model.model, workspace_dir.clone()),
    );
    registry.register(
        WhatDoIKnowTool::tool_def(),
        WhatDoIKnowTool::new(workspace_dir.clone()),
    );
    if let Some(mem) = long_term_memory.clone() {
        registry.register(
            MyStatsTool::tool_def(),
            MyStatsTool::new(mem.clone(), workspace_dir.clone()),
        );
        if primary.plugins.iter().any(|p| p == "memory") {
            registry.register(
                MemoryTool::tool_def(),
                MemoryTool::new_with_default_mode(mem, memory_default_recall_mode),
            );
        }
    }

    let name = server_cfg
        .name
        .clone()
        .unwrap_or_else(|| primary.id.clone());
    let server_info = McpServerInfo {
        name,
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let allowlist: Option<HashSet<String>> = if server_cfg.allowlist.is_empty() {
        None
    } else {
        Some(server_cfg.allowlist.iter().cloned().collect())
    };
    let bridge = ToolRegistryBridge::new(
        server_info,
        registry,
        ctx,
        allowlist,
        server_cfg.expose_proxies,
    );

    let auth_token = if let Some(env_name) = server_cfg.auth_token_env.as_deref() {
        let token = std::env::var(env_name).with_context(|| {
            format!(
                "mcp_server.auth_token_env={env_name} is set but env var `{env_name}` is missing"
            )
        })?;
        if token.trim().is_empty() {
            anyhow::bail!(
                "mcp_server.auth_token_env={env_name} resolved to an empty token; set a non-empty value"
            );
        }
        Some(token)
    } else {
        None
    };

    let shutdown = CancellationToken::new();
    let sh = shutdown.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            sh.cancel();
        }
    });

    run_stdio_server_with_auth(bridge, shutdown, auth_token)
        .await
        .context("mcp-server loop failed")?;
    Ok(())
}

/// Build a `BrokerClientForDoctor` adapter from the loaded broker config.
/// Returns `None` when the broker is `local` — NATS runtime checks are
/// then reported as `skip` instead of a misleading fail.
fn build_doctor_broker_adapter(
    cfg: &agent_config::types::broker::BrokerInner,
) -> Option<Arc<dyn agent_extensions::cli::BrokerClientForDoctor>> {
    if cfg.kind != agent_config::types::broker::BrokerKind::Nats {
        return None;
    }
    Some(Arc::new(NatsDoctorAdapter {
        url: cfg.url.clone(),
    }))
}

struct NatsDoctorAdapter {
    url: String,
}

#[async_trait::async_trait]
impl agent_extensions::cli::BrokerClientForDoctor for NatsDoctorAdapter {
    async fn wait_for_subject(
        &self,
        subject: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<()> {
        use futures::StreamExt;
        let client = async_nats::connect(&self.url).await?;
        let mut sub = client.subscribe(subject.to_string()).await?;
        match tokio::time::timeout(timeout, sub.next()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(anyhow::anyhow!("nats subscription closed")),
            Err(_) => Err(anyhow::anyhow!(
                "no beacon within {}ms",
                timeout.as_millis()
            )),
        }
    }
}

fn run_ext_cli(config_dir: &std::path::Path, cmd: ExtCmd) -> Result<()> {
    let extensions = match AppConfig::load(config_dir) {
        Ok(cfg) => cfg.extensions.unwrap_or_default(),
        Err(_) => {
            // Ext subcommands only need `extensions.yaml`; tolerate the rest
            // being absent so `agent ext list` works on a fresh checkout.
            agent_extensions::cli::yaml_edit::load_or_default(&config_dir.join("extensions.yaml"))
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
        }
    };

    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let ctx = agent_extensions::cli::CliContext {
        config_dir: config_dir.to_path_buf(),
        extensions,
        out: &mut stdout,
        err: &mut stderr,
    };

    let result = match cmd {
        ExtCmd::List { json } => agent_extensions::cli::run_list(ctx, json),
        ExtCmd::Info { id, json } => agent_extensions::cli::run_info(ctx, &id, json),
        ExtCmd::Enable { id } => agent_extensions::cli::run_enable(ctx, &id),
        ExtCmd::Disable { id } => agent_extensions::cli::run_disable(ctx, &id),
        ExtCmd::Validate { path } => agent_extensions::cli::run_validate(ctx, &path),
        ExtCmd::Doctor { runtime, json } => {
            if !runtime {
                return agent_extensions::cli::run_doctor(ctx).map_err(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(e.exit_code());
                });
            }
            // Runtime check: async + may need NATS. Spin a dedicated
            // current-thread runtime.
            let broker_adapter = AppConfig::load(config_dir)
                .ok()
                .and_then(|cfg| build_doctor_broker_adapter(&cfg.broker.broker));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(agent_extensions::cli::run_doctor_runtime(
                ctx,
                agent_extensions::cli::DoctorOptions { runtime, json },
                broker_adapter,
            ))
        }
        ExtCmd::Install {
            source,
            update,
            enable,
            dry_run,
            link,
            json,
        } => agent_extensions::cli::run_install(
            ctx,
            agent_extensions::cli::InstallOptions {
                source,
                update,
                enable,
                dry_run,
                link,
                json,
            },
        ),
        ExtCmd::Uninstall { id, yes, json } => agent_extensions::cli::run_uninstall(
            ctx,
            agent_extensions::cli::UninstallOptions { id, yes, json },
        ),
    };

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}

async fn open_disk_queue(config_dir: &std::path::Path) -> Result<DiskQueue> {
    let cfg = AppConfig::load(config_dir).context("failed to load config")?;
    let path = &cfg.broker.broker.persistence.path;
    let max_pending = cfg.broker.broker.limits.max_pending;
    DiskQueue::new(path, max_pending)
        .await
        .with_context(|| format!("failed to open disk queue at {path}"))
}

async fn run_dlq_list(config_dir: &std::path::Path) -> Result<()> {
    let queue = open_disk_queue(config_dir).await?;
    let entries = queue.list_dead_letters(1000).await?;
    if entries.is_empty() {
        println!("(no dead-letter entries)");
        return Ok(());
    }
    println!("{:<38}  {:<30}  {:<13}  reason", "id", "topic", "failed_at");
    for e in &entries {
        println!(
            "{:<38}  {:<30}  {:<13}  {}",
            e.id, e.topic, e.failed_at, e.reason
        );
    }
    println!();
    println!("total: {}", entries.len());
    Ok(())
}

async fn run_dlq_replay(config_dir: &std::path::Path, id: &str) -> Result<()> {
    let queue = open_disk_queue(config_dir).await?;
    let moved = queue.replay_dead_letter(id).await?;
    if moved {
        println!("replayed {id} → pending_events (next daemon drain will retry it)");
    } else {
        eprintln!("no dead-letter entry with id `{id}`");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_dlq_purge(config_dir: &std::path::Path) -> Result<()> {
    let queue = open_disk_queue(config_dir).await?;
    let n = queue.purge_dead_letters().await?;
    println!("purged {n} dead-letter entries");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let term = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

async fn run_metrics_server(health: RuntimeHealth) {
    let listener = match TcpListener::bind("0.0.0.0:9090").await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind metrics server on :9090");
            return;
        }
    };
    tracing::info!("metrics server listening on :9090");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "metrics accept failed");
                continue;
            }
        };
        let health = health.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_metrics_conn(stream, health).await {
                tracing::debug!(error = %e, "metrics connection failed");
            }
        });
    }
}

async fn run_health_server(health: RuntimeHealth) {
    let listener = match TcpListener::bind("0.0.0.0:8080").await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind health server on :8080");
            return;
        }
    };
    tracing::info!("health server listening on :8080");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "health accept failed");
                continue;
            }
        };
        let health = health.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_health_conn(stream, health).await {
                tracing::debug!(error = %e, "health connection failed");
            }
        });
    }
}

async fn run_admin_server(
    registry: Arc<agent_core::agent::tool_policy::ToolPolicyRegistry>,
    agents: Arc<agent_core::agent::AgentsDirectory>,
) {
    let listener = match TcpListener::bind("127.0.0.1:9091").await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind admin server on 127.0.0.1:9091");
            return;
        }
    };
    tracing::info!("admin server listening on 127.0.0.1:9091 (loopback only)");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "admin accept failed");
                continue;
            }
        };
        let registry = Arc::clone(&registry);
        let agents = Arc::clone(&agents);
        tokio::spawn(async move {
            if let Err(e) = handle_admin_conn(stream, registry, agents).await {
                tracing::debug!(error = %e, "admin connection failed");
            }
        });
    }
}

async fn handle_admin_conn(
    mut stream: TcpStream,
    registry: Arc<agent_core::agent::tool_policy::ToolPolicyRegistry>,
    agents: Arc<agent_core::agent::AgentsDirectory>,
) -> anyhow::Result<()> {
    let (method, full_path) = read_http_method_path(&mut stream).await?;
    let (path, query) = match full_path.find('?') {
        Some(i) => (&full_path[..i], &full_path[i + 1..]),
        None => (full_path.as_str(), ""),
    };
    // Route `/admin/agents*` to the agents directory; fall through to
    // the tool-policy handler otherwise. Adding a new admin subsection
    // is a matter of another `if let Some(...)` above this line.
    let (status, body, content_type) = if let Some(resp) = agents.dispatch(&method, path) {
        resp
    } else {
        agent_core::agent::tool_policy::admin_dispatch(&method, path, query, &registry)
    };
    write_http_response(&mut stream, status, content_type, &body).await?;
    Ok(())
}

async fn read_http_method_path(stream: &mut TcpStream) -> anyhow::Result<(String, String)> {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        anyhow::bail!("empty request");
    }
    let req = std::str::from_utf8(&buf[..n]).context("invalid request utf8")?;
    let line = req.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    Ok((method, path))
}

async fn handle_metrics_conn(mut stream: TcpStream, health: RuntimeHealth) -> anyhow::Result<()> {
    let path = read_http_path(&mut stream).await?;
    if path != "/metrics" {
        write_http_response(&mut stream, 404, "text/plain; charset=utf-8", "not found").await?;
        return Ok(());
    }
    // Keep the nats breaker gauge fresh: sample current readiness at scrape time.
    let nats_open = !health.broker.is_ready();
    agent_core::telemetry::set_circuit_breaker_state("nats", nats_open);
    let mut body = render_prometheus(nats_open);
    body.push_str(&agent_llm::telemetry::render_prometheus());
    body.push_str(&agent_mcp::telemetry::render_prometheus());
    write_http_response(&mut stream, 200, "text/plain; version=0.0.4", &body).await?;
    Ok(())
}

async fn handle_health_conn(mut stream: TcpStream, health: RuntimeHealth) -> anyhow::Result<()> {
    let path = read_http_path(&mut stream).await?;
    // Try to match `/whatsapp/...` routes first. Routing rules live in
    // `agent_plugin_whatsapp::pairing::dispatch_route` so they're
    // unit-testable without a TCP listener.
    if let Some(rest) = path.strip_prefix("/whatsapp/") {
        use agent_plugin_whatsapp::pairing::{dispatch_route, WhatsappRoute};
        match dispatch_route(rest, &health.wa_pairing) {
            Some(WhatsappRoute::Html) => {
                write_http_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    agent_plugin_whatsapp::pairing::PAIR_PAGE_HTML,
                )
                .await?;
                return Ok(());
            }
            Some(WhatsappRoute::Qr(pairing)) => {
                let body = match pairing.get_qr().await {
                    Some(qr) => serde_json::to_string(&qr).unwrap_or_else(|_| "{}".into()),
                    None => r#"{"state":"no_qr"}"#.to_string(),
                };
                write_http_response(&mut stream, 200, "application/json; charset=utf-8", &body)
                    .await?;
                return Ok(());
            }
            Some(WhatsappRoute::Status(pairing)) => {
                let body =
                    serde_json::to_string(&pairing.status().await).unwrap_or_else(|_| "{}".into());
                write_http_response(&mut stream, 200, "application/json; charset=utf-8", &body)
                    .await?;
                return Ok(());
            }
            Some(WhatsappRoute::Json(s)) => {
                write_http_response(&mut stream, 200, "application/json; charset=utf-8", &s)
                    .await?;
                return Ok(());
            }
            Some(WhatsappRoute::Disabled) => {
                write_http_response(
                    &mut stream,
                    200,
                    "application/json; charset=utf-8",
                    r#"{"state":"disabled"}"#,
                )
                .await?;
                return Ok(());
            }
            Some(WhatsappRoute::NotFound) => {
                write_http_response(
                    &mut stream,
                    404,
                    "application/json; charset=utf-8",
                    r#"{"error":"instance not found"}"#,
                )
                .await?;
                return Ok(());
            }
            None => {
                // Fall through to the 404 at the bottom.
            }
        }
    }

    match path.as_str() {
        "/health" => {
            write_http_response(
                &mut stream,
                200,
                "application/json; charset=utf-8",
                r#"{"status":"ok"}"#,
            )
            .await?;
        }
        "/ready" => {
            let broker_ready = health.broker.is_ready();
            let agents = health.running_agents.load(Ordering::Relaxed);
            if broker_ready && agents > 0 {
                let body = format!(r#"{{"status":"ready","agents_running":{agents}}}"#);
                write_http_response(&mut stream, 200, "application/json; charset=utf-8", &body)
                    .await?;
            } else {
                let body = format!(
                    r#"{{"status":"not_ready","broker_ready":{},"agents_running":{}}}"#,
                    broker_ready, agents
                );
                write_http_response(&mut stream, 503, "application/json; charset=utf-8", &body)
                    .await?;
            }
        }
        _ => {
            write_http_response(&mut stream, 404, "text/plain; charset=utf-8", "not found").await?;
        }
    }
    Ok(())
}

async fn read_http_path(stream: &mut TcpStream) -> anyhow::Result<String> {
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        anyhow::bail!("empty request");
    }
    let req = std::str::from_utf8(&buf[..n]).context("invalid request utf8")?;
    let line = req.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/");
    Ok(path.to_string())
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> anyhow::Result<()> {
    let status_text = match status {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        content_type,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

// ---- TaskFlow CLI (Phase 14.6) ---------------------------------------------

fn flow_db_path() -> std::path::PathBuf {
    std::env::var("TASKFLOW_DB_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("./data/taskflow.db"))
}

async fn open_flow_manager() -> Result<agent_taskflow::FlowManager> {
    let path = flow_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let path_s = path.to_string_lossy().into_owned();
    let store = agent_taskflow::SqliteFlowStore::open(&path_s)
        .await
        .with_context(|| format!("failed to open taskflow db at {}", path.display()))?;
    Ok(agent_taskflow::FlowManager::new(std::sync::Arc::new(store)))
}

fn run_flow_help() -> Result<()> {
    println!("agent flow — TaskFlow admin");
    println!();
    println!("USAGE:");
    println!("  agent flow list [--json]         List all flows");
    println!("  agent flow show <id> [--json]    Show details of one flow");
    println!("  agent flow cancel <id>           Cancel a flow");
    println!("  agent flow resume <id>           Manually resume a Waiting flow");
    println!();
    println!("ENV:");
    println!("  TASKFLOW_DB_PATH   SQLite path (default ./data/taskflow.db)");
    Ok(())
}

fn flow_to_summary_json(f: &agent_taskflow::Flow) -> serde_json::Value {
    serde_json::json!({
        "id": f.id.to_string(),
        "controller_id": f.controller_id,
        "goal": f.goal,
        "current_step": f.current_step,
        "status": f.status.as_str(),
        "cancel_requested": f.cancel_requested,
        "revision": f.revision,
        "owner_session_key": f.owner_session_key,
        "created_at": f.created_at.to_rfc3339(),
        "updated_at": f.updated_at.to_rfc3339(),
    })
}

async fn run_flow_list(json: bool) -> Result<()> {
    let m = open_flow_manager().await?;
    // list_by_status across all non-terminal + terminals, in one pass.
    use agent_taskflow::FlowStatus::*;
    let mut all: Vec<agent_taskflow::Flow> = Vec::new();
    for status in [Created, Running, Waiting, Cancelled, Finished, Failed] {
        all.extend(m.list_by_status(status).await?);
    }
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    if json {
        let out: Vec<_> = all.iter().map(flow_to_summary_json).collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!(out))?);
        return Ok(());
    }

    if all.is_empty() {
        println!("(no flows)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<10}  {:<14}  {:<20}  GOAL",
        "ID", "STATUS", "STEP", "UPDATED"
    );
    for f in &all {
        println!(
            "{:<36}  {:<10}  {:<14}  {:<20}  {}",
            f.id,
            f.status.as_str(),
            truncate(&f.current_step, 14),
            f.updated_at.format("%Y-%m-%d %H:%M:%S"),
            truncate(&f.goal, 60),
        );
    }
    Ok(())
}

async fn run_flow_show(id: &str, json: bool) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(id).with_context(|| format!("invalid flow id `{id}`"))?;
    let m = open_flow_manager().await?;
    let flow = m
        .get(uuid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("flow {id} not found"))?;
    let steps = m.list_steps(uuid).await?;

    if json {
        let out = serde_json::json!({
            "flow": {
                "id": flow.id.to_string(),
                "controller_id": flow.controller_id,
                "goal": flow.goal,
                "current_step": flow.current_step,
                "status": flow.status.as_str(),
                "cancel_requested": flow.cancel_requested,
                "revision": flow.revision,
                "owner_session_key": flow.owner_session_key,
                "requester_origin": flow.requester_origin,
                "state": flow.state_json,
                "wait": flow.wait_json,
                "created_at": flow.created_at.to_rfc3339(),
                "updated_at": flow.updated_at.to_rfc3339(),
            },
            "steps": steps.iter().map(|s| serde_json::json!({
                "id": s.id.to_string(),
                "runtime": s.runtime.as_str(),
                "run_id": s.run_id,
                "task": s.task,
                "status": s.status.as_str(),
                "result": s.result_json,
                "child_session_key": s.child_session_key,
                "updated_at": s.updated_at.to_rfc3339(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("Flow {}", flow.id);
    println!("  goal:          {}", flow.goal);
    println!("  controller:    {}", flow.controller_id);
    println!("  owner:         {}", flow.owner_session_key);
    println!("  status:        {}", flow.status.as_str());
    println!("  current_step:  {}", flow.current_step);
    println!("  revision:      {}", flow.revision);
    println!("  cancel_req:    {}", flow.cancel_requested);
    println!("  created_at:    {}", flow.created_at.to_rfc3339());
    println!("  updated_at:    {}", flow.updated_at.to_rfc3339());
    if let Some(w) = &flow.wait_json {
        println!("  wait:          {w}");
    }
    println!("  state:");
    for line in serde_json::to_string_pretty(&flow.state_json)?.lines() {
        println!("    {line}");
    }
    if !steps.is_empty() {
        println!("  steps:");
        for s in &steps {
            println!(
                "    - [{}] {} ({}) {}",
                s.status.as_str(),
                s.run_id,
                s.runtime.as_str(),
                truncate(&s.task, 80)
            );
        }
    }
    Ok(())
}

async fn run_flow_cancel(id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(id).with_context(|| format!("invalid flow id `{id}`"))?;
    let m = open_flow_manager().await?;
    let f = m.cancel(uuid).await?;
    println!("cancelled flow {} (status={})", f.id, f.status.as_str());
    Ok(())
}

async fn run_flow_resume(id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(id).with_context(|| format!("invalid flow id `{id}`"))?;
    let m = open_flow_manager().await?;
    let f = m.resume(uuid, None).await?;
    println!("resumed flow {} (status={})", f.id, f.status.as_str());
    Ok(())
}

/// Hit the admin HTTP endpoint and summarise the agent directory.
/// Default endpoint is loopback; `--endpoint=http://host:port` lets
/// an ssh-tunneled operator point at a remote process.
async fn run_status(json: bool, endpoint: Option<String>, agent_id: Option<String>) -> Result<()> {
    let base = endpoint.unwrap_or_else(|| "http://127.0.0.1:9091".to_string());
    let url = match &agent_id {
        Some(id) => format!("{}/admin/agents/{}", base.trim_end_matches('/'), id),
        None => format!("{}/admin/agents", base.trim_end_matches('/')),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .context("build http client")?;
    let body = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("non-200 from {url}"))?
        .text()
        .await
        .context("read response body")?;

    if json {
        println!("{body}");
        return Ok(());
    }

    // Single-agent route returns an object, not an array — wrap it so
    // the same table renderer works in both modes.
    let agents: Vec<JsonValue> = if agent_id.is_some() {
        let single: JsonValue = serde_json::from_str(&body)
            .with_context(|| format!("parse JSON from {url}: {body}"))?;
        vec![single]
    } else {
        serde_json::from_str(&body).with_context(|| format!("parse JSON from {url}: {body}"))?
    };
    if agents.is_empty() {
        println!("no agents running");
        return Ok(());
    }
    // Plain-text table — one line per agent. Width is generous; output
    // is meant for humans piping through `less`, not a fixed-width
    // terminal UI.
    println!(
        "{:<16} {:<16} {:<24} {:<28} DESCRIPTION",
        "ID", "MODEL", "BINDINGS", "DELEGATES"
    );
    println!("{}", "─".repeat(120));
    for a in agents {
        let id = a["id"].as_str().unwrap_or("-");
        let model = a["model"]["model"].as_str().unwrap_or("-");
        let desc = a["description"].as_str().unwrap_or("");
        let bindings = match a["inbound_bindings"].as_array() {
            Some(bs) if !bs.is_empty() => bs
                .iter()
                .map(|b| match b["instance"].as_str() {
                    Some(inst) => format!("{}:{}", b["plugin"].as_str().unwrap_or("-"), inst),
                    None => b["plugin"].as_str().unwrap_or("-").to_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
            _ => "*".to_string(),
        };
        let delegates = match a["allowed_delegates"].as_array() {
            Some(ds) if !ds.is_empty() => ds
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(","),
            _ => "*".to_string(),
        };
        println!(
            "{:<16} {:<16} {:<24} {:<28} {}",
            truncate(id, 16),
            truncate(model, 16),
            truncate(&bindings, 24),
            truncate(&delegates, 28),
            desc,
        );
    }
    Ok(())
}

/// Pre-flight config validation — loads `config/*.yaml`, resolves env
/// vars + file secrets, and prints a summary. Exits non-zero on any
/// error, so CI pipelines can gate deploys on `agent --dry-run` before
/// flipping traffic.
fn run_dry_run(config_dir: &std::path::Path, json: bool) -> Result<()> {
    let cfg = AppConfig::load(config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;

    // Build the same AgentsDirectory the daemon would serve — same
    // projection code path, catches any mismatch between config schema
    // and runtime expectations.
    let agents: Vec<agent_core::agent::AgentInfo> = cfg
        .agents
        .agents
        .iter()
        .map(agent_core::agent::AgentInfo::from_config)
        .collect();

    if json {
        let dir = agent_core::agent::AgentsDirectory::new(agents);
        if let Some((_, body, _)) = dir.dispatch("GET", "/admin/agents") {
            println!("{body}");
        }
        return Ok(());
    }

    println!("config: {}", config_dir.display());
    println!();
    println!("broker: {:?}", cfg.broker.broker.kind);
    println!();
    println!("plugins:");
    for (i, wa) in cfg.plugins.whatsapp.iter().enumerate() {
        let label = wa.instance.as_deref().unwrap_or("<default>");
        println!("  • whatsapp[{i}] (instance={label})");
    }
    for (i, tg) in cfg.plugins.telegram.iter().enumerate() {
        let label = tg.instance.as_deref().unwrap_or("<default>");
        println!("  • telegram[{i}] (instance={label})");
    }
    if cfg.plugins.email.is_some() {
        println!("  • email");
    }
    if cfg.plugins.browser.is_some() {
        println!("  • browser");
    }
    println!();
    println!("agents ({}):", agents.len());
    for a in &agents {
        let bindings = if a.inbound_bindings.is_empty() {
            "* (wildcard)".to_string()
        } else {
            a.inbound_bindings
                .iter()
                .map(|b| match &b.instance {
                    Some(i) => format!("{}:{}", b.plugin, i),
                    None => b.plugin.clone(),
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        let tools = if a.allowed_tools.is_empty() {
            "*".to_string()
        } else {
            a.allowed_tools.join(",")
        };
        let delegates = if a.allowed_delegates.is_empty() {
            "*".to_string()
        } else {
            a.allowed_delegates.join(",")
        };
        println!(
            "  • {} ({}/{}){}",
            a.id,
            a.model_provider,
            a.model_name,
            if a.description.is_empty() {
                String::new()
            } else {
                format!(" — {}", a.description)
            }
        );
        println!("      bindings:   {bindings}");
        println!("      tools:      {tools}");
        println!("      delegates:  {delegates}");
        if !a.extra_docs.is_empty() {
            println!("      extra_docs: {}", a.extra_docs.join(","));
        }
        if a.has_sender_rate_limit {
            println!("      sender_rate_limit: yes");
        }
        if a.has_workspace {
            println!("      workspace:  configured");
        }
    }
    println!();
    println!("dry-run OK — config valid, no runtime started");
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
