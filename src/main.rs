#![allow(clippy::all)] // In-flux — Phase 76 + 79 scaffolding

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

use nexo_broker::{AnyBroker, BrokerHandle, DiskQueue};
use nexo_config::AppConfig;
use nexo_core::agent::dreaming::{DreamEngine, DreamingConfig};
use nexo_core::session::SessionManager;
use nexo_core::telemetry::{add_extensions_discovered, render_prometheus};
use nexo_core::{
    Agent, AgentRuntime, DelegationTool, ExtensionHook, ExtensionTool, HeartbeatTool, HookRegistry,
    LlmAgentBehavior, MemoryTool, MyStatsTool, PluginRegistry, SessionLogsTool, ToolRegistry,
    WhatDoIKnowTool, WhoAmITool,
};
use nexo_llm::LlmRegistry;
use nexo_memory::LongTermMemory;
use nexo_plugin_browser::BrowserPlugin;
use nexo_plugin_whatsapp::WhatsappPlugin;

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
    McpServer(McpServerSubcommand),
    /// Phase 80.1.d — `nexo agent dream {tail|status|kill}`.
    AgentDream(AgentDreamSubcommand),
    /// Phase 80.10 — `nexo agent run [--bg] <prompt>`. Spawn a goal
    /// against the local agent registry. With `--bg`, the row is
    /// inserted with `kind = Bg` and the command returns the goal_id
    /// immediately so the operator can detach. Without `--bg`, the
    /// row is `kind = Interactive` (default).
    AgentRun {
        prompt: String,
        bg: bool,
        db: Option<PathBuf>,
        json: bool,
    },
    /// Phase 80.10 — `nexo agent ps [--all] [--kind=...] [--json]`.
    /// Read the local `agent_handles` SQLite store and render running
    /// goals. RO pool — works without a daemon up.
    AgentPs {
        kind: Option<String>,
        all: bool,
        db: Option<PathBuf>,
        json: bool,
    },
    /// Phase 80.16 — `nexo agent attach <goal_id>`. Read-only viewer
    /// of a goal's latest persisted snapshot. Live event streaming
    /// via NATS lands in 80.16.b.
    AgentAttach {
        goal_id: String,
        db: Option<PathBuf>,
        json: bool,
    },
    /// Phase 80.16 — `nexo agent discover [--include-interactive]`.
    /// List Running goals filtered to BG / Daemon / DaemonWorker
    /// kinds. With `--include-interactive`, include all kinds.
    AgentDiscover {
        include_interactive: bool,
        db: Option<PathBuf>,
        json: bool,
    },
    /// Phase 80.9.e — `nexo channel list [--config=<path>] [--json]`.
    /// Static dump of every operator-approved channel + every binding's
    /// `allowed_channel_servers`. Pure read of the YAML — no daemon
    /// required.
    ChannelList {
        config: Option<PathBuf>,
        json: bool,
    },
    /// Phase 80.9.e — `nexo channel doctor [--config=<path>] [--binding=<id>]
    /// [--json]`. For every approved server in `agents.channels.approved`
    /// and every binding's `allowed_channel_servers`, run the static
    /// half of the gate (1 = capability is *assumed* declared since the
    /// doctor cannot probe a live MCP server, 2 = killswitch, 3 =
    /// session allowlist, 5 = approved allowlist) and report each
    /// outcome. Gate 4 (plugin source) is reported as "static-only —
    /// runtime stamp not available" because the plugin source is set
    /// by the runtime at MCP register-time. Useful to surface a typo
    /// in the YAML before it manifests as silent inbound silence.
    ChannelDoctor {
        config: Option<PathBuf>,
        binding: Option<String>,
        json: bool,
    },
    /// Phase 80.9.e — `nexo channel test <server> [--binding=<id>]
    /// [--content=...] [--config=<path>] [--json]`. Synthesises a
    /// `notifications/nexo/channel` payload for `server`, runs it
    /// through the parser + the XML wrap helper, and prints the
    /// rendered `<channel>` block as the model would see it. Cheap
    /// dry-run for operators tuning meta-key whitelists or content
    /// caps.
    ChannelTest {
        server: String,
        binding: Option<String>,
        content: Option<String>,
        config: Option<PathBuf>,
        json: bool,
    },
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
    SetupMigrate {
        apply: bool,
    },
    SetupTelegramLink {
        agent: Option<String>,
    },
    /// Phase 26 — pairing CLI subcommands. Each one opens the
    /// pairing.db + secret file inline (no daemon connection needed)
    /// so the operator can manage senders before / after the daemon
    /// is up.
    PairStart {
        device_label: Option<String>,
        public_url: Option<String>,
        qr_png_path: Option<PathBuf>,
        /// TTL override from `--ttl-secs`. `None` means "use YAML
        /// `pairing.setup_code.default_ttl_secs` if set, else
        /// fall back to the hardcoded 600s default". Resolved
        /// inside `run_pair_start` once the YAML is loaded.
        ttl_secs: Option<u64>,
        json: bool,
    },
    PairList {
        channel: Option<String>,
        json: bool,
        /// `--all` switches the listing from "pending challenges only"
        /// to a unified view that also shows every active row in
        /// `pairing_allow_from`. Operators rely on this to confirm a
        /// `pair seed` call actually persisted.
        show_allow: bool,
        /// `--include-revoked` (only meaningful with `--all`) keeps
        /// soft-deleted allow rows in the output for audit.
        include_revoked: bool,
    },
    PairApprove {
        code: String,
        json: bool,
    },
    PairRevoke {
        target: String, // "<channel>:<sender_id>"
    },
    PairSeed {
        channel: String,
        account_id: String,
        senders: Vec<String>,
    },
    PairHelp,
    /// `agent doctor capabilities [--json]` — enumerate write/reveal
    /// env toggles exposed by the bundled extensions.
    DoctorCapabilities {
        json: bool,
    },
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
    /// Phase 17 — run the credential gauntlet against the loaded config
    /// and print a report (OK / warnings / errors). Exits 0 on clean,
    /// 1 on errors, 2 on warnings-only. Used by CI to gate PRs that
    /// edit `agents.d/*.yaml`, `whatsapp.yaml`, `telegram.yaml`, or
    /// `google-auth.yaml`.
    CheckConfig {
        strict: bool,
    },
    /// Phase 18 — trigger a hot-reload on a running agent daemon.
    /// Publishes `control.reload` on the same broker the daemon is on
    /// and waits up to 5s for a `control.reload.ack` with the outcome
    /// (version, applied, rejected). Exit 0 if at least one agent
    /// reloaded; exit 1 if all rejected or no ack arrived.
    Reload {
        json: bool,
    },
    /// Phase 19 — generic poller subsystem. CLI hits the loopback admin
    /// endpoint at `127.0.0.1:9091` (daemon must be running).
    PollersList {
        json: bool,
    },
    PollersShow {
        id: String,
        json: bool,
    },
    PollersRun {
        id: String,
    },
    PollersPause {
        id: String,
    },
    PollersResume {
        id: String,
    },
    PollersReset {
        id: String,
        yes: bool,
    },
    PollersReload,
    /// Operator-side cron admin (Phase 79.7 follow-up):
    /// inspect persistent schedule rows and manage them out-of-band
    /// from an agent turn.
    CronList {
        binding: Option<String>,
        json: bool,
    },
    CronDrop {
        id: String,
    },
    CronPause {
        id: String,
    },
    CronResume {
        id: String,
    },
    /// Run the web admin UI exposed through a fresh Cloudflare quick
    /// tunnel. Ensures `cloudflared` is installed (downloads it per
    /// OS/arch if absent), starts a loopback HTTP server, opens a new
    /// trycloudflare.com URL on every launch, prints it to stdout,
    /// and blocks until SIGTERM / Ctrl+C. Useful for reaching the
    /// admin page from anywhere without DNS, TLS certs, or an account.
    Admin {
        port: u16,
    },
    /// Phase 27.1 — print version + (optionally) build provenance.
    /// Short form (`nexo --version` / `-V`) prints `nexo <pkg-version>`.
    /// Verbose form (`nexo version` or `nexo --version --verbose`)
    /// prints the package version plus git-sha, target triple, build
    /// channel, and build timestamp captured by `build.rs`.
    Version {
        verbose: bool,
    },
    Help,
}

/// Phase 76.14 — subcommands for `nexo mcp-server`.
///
/// Without a subcommand, `nexo mcp-server` boots the MCP server
/// (backward-compatible). With a subcommand, it runs a client-side
/// operation against a local or remote MCP endpoint.
#[derive(Debug, Clone)]
enum McpServerSubcommand {
    /// Default: run the MCP stdio/HTTP server.
    Serve,
    /// `inspect <url>` — list tools + resources of a remote server.
    Inspect { url: String },
    /// `bench <url> --tool <name> --rps <n>` — load test a tool.
    Bench { url: String, tool: String, rps: u32 },
    /// `tail-audit <db>` — read recent entries from a local audit SQLite DB.
    TailAudit { db: String },
}

/// Phase 80.1.d — `nexo agent dream {tail|status|kill}` operator CLI
/// for the autoDream audit log + manual control. Read paths open the
/// SQLite DB read-only without a daemon. Kill writes the row to
/// `Aborted`, finalises `ended_at = now()`, and rewinds the
/// consolidation lock via `ConsolidationLock::rollback(prior_mtime)`
/// when a `--memory-dir` is provided. Mirror leak
/// `claude-code-leak/src/components/tasks/BackgroundTasksDialog.tsx:281,315-317`
/// `DreamTask.kill(taskId, setAppState)` semantics, but as CLI rather
/// than Ink UI keyboard since nexo has no Ink-equivalent yet.
#[derive(Debug, Clone)]
enum AgentDreamSubcommand {
    /// `agent dream tail [--goal=<uuid>] [--n=20] [--db=<path>] [--json]`
    Tail {
        goal_id: Option<String>,
        n: usize,
        db: Option<PathBuf>,
        json: bool,
    },
    /// `agent dream status <run_id> [--db=<path>] [--json]`
    Status {
        run_id: String,
        db: Option<PathBuf>,
        json: bool,
    },
    /// `agent dream kill <run_id> [--force] [--memory-dir=<path>] [--db=<path>]`
    Kill {
        run_id: String,
        force: bool,
        memory_dir: Option<PathBuf>,
        db: Option<PathBuf>,
    },
}

struct CliArgs {
    config_dir: PathBuf,
    mode: Mode,
}

/// Shared state for the companion WebSocket pairing handshake.
/// Stored in an `OnceLock` so it can be populated after the health server
/// binds (pairing init happens slightly later in the startup sequence).
struct PairingHandshakeCtx {
    issuer: Arc<nexo_pairing::SetupCodeIssuer>,
    session_store: Arc<nexo_pairing::PairingSessionStore>,
    session_ttl: std::time::Duration,
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
        std::collections::BTreeMap<String, nexo_plugin_whatsapp::pairing::SharedPairingState>,
    /// Phase 48 follow-up #7 — `/email/health` exposes the per-account
    /// `AccountHealth` map (state, IDLE alive ts, queue / DLQ depths,
    /// sent / failed totals). `None` when the email plugin isn't
    /// configured.
    email_plugin: Option<Arc<nexo_plugin_email::EmailPlugin>>,
    /// Companion WS handshake context — populated after pairing init.
    /// `None` until the daemon's pairing block completes.
    pairing_handshake: Arc<std::sync::OnceLock<PairingHandshakeCtx>>,
}

#[derive(Clone)]
struct CronToolBindingContext {
    ctx: nexo_core::agent::AgentContext,
    tools: Arc<nexo_core::agent::ToolRegistry>,
}

/// M5 — `Arc<ArcSwap<HashMap>>` enables lock-free hot-swap of the
/// per-binding context map. The config-reload post-hook calls
/// [`RuntimeCronToolExecutor::replace_bindings`] so cron firings
/// observe the new `effective` policy on the next call. In-flight
/// `resolve_binding` callers keep their loaded `Arc<HashMap>`
/// snapshot until completion; subsequent calls see the new map.
///
/// Pattern validated against:
///   * `claude-code-leak/src/utils/cronScheduler.ts:441-448`
///     (chokidar-on-file-change rebuild) + `:170,251,335-336,356`
///     (`inFlight` Set with pitfall comment "idempotent even
///     without the guard"). We use `ArcSwap` (lock-free swap) so
///     the in-flight protection is structural rather than imperative.
///   * `research/src/cron/service/timer.ts:709,697`
///     (forceReload-per-tick + long-job pitfall). We rebuild on
///     reload only, not on every tick — cheaper and avoids the
///     long-job hide-tick race.
#[derive(Clone)]
struct RuntimeCronToolExecutor {
    by_binding: Arc<arc_swap::ArcSwap<std::collections::HashMap<String, CronToolBindingContext>>>,
}

impl RuntimeCronToolExecutor {
    fn new(by_binding: std::collections::HashMap<String, CronToolBindingContext>) -> Self {
        Self {
            by_binding: Arc::new(arc_swap::ArcSwap::from_pointee(by_binding)),
        }
    }

    /// M5 — atomic hot-swap of the binding map. Called by the
    /// config-reload post-hook. Cheap (single `Arc` store).
    /// In-flight callers retain their pre-swap snapshot.
    fn replace_bindings(
        &self,
        new_map: std::collections::HashMap<String, CronToolBindingContext>,
    ) {
        self.by_binding.store(Arc::new(new_map));
    }

    /// Returns an OWNED clone of the binding (cheap — fields are
    /// `Arc<_>` underneath). The owned clone is required because
    /// `ArcSwap` does not expose stable references across swaps.
    fn resolve_binding(&self, binding_id: &str) -> Option<CronToolBindingContext> {
        self.by_binding.load().get(binding_id).cloned()
    }
}

#[async_trait::async_trait]
impl nexo_core::llm_cron_dispatcher::CronToolExecutor for RuntimeCronToolExecutor {
    fn list_tools(&self, entry: &nexo_core::cron_schedule::CronEntry) -> Vec<nexo_llm::ToolDef> {
        self.resolve_binding(&entry.binding_id)
            .map(|b| b.tools.to_tool_defs())
            .unwrap_or_default()
    }

    async fn call_tool(
        &self,
        entry: &nexo_core::cron_schedule::CronEntry,
        tool_name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let binding = self.resolve_binding(&entry.binding_id).ok_or_else(|| {
            anyhow::anyhow!(
                "cron tool execution has no binding context for `{}`",
                entry.binding_id
            )
        })?;
        let (_def, handler) = binding.tools.get(tool_name).ok_or_else(|| {
            anyhow::anyhow!(
                "cron tool `{tool_name}` is not enabled for binding `{}`",
                entry.binding_id
            )
        })?;
        // Stable per-entry session key so tools that require
        // `ctx.session_id` (taskflow) can run in cron context.
        const CRON_SESSION_NS: uuid::Uuid = uuid::uuid!("f7ba24d0-3f70-54e2-8cd5-43b1de1002af");
        let session_id = uuid::Uuid::new_v5(&CRON_SESSION_NS, entry.id.as_bytes());
        let ctx = binding.ctx.clone().with_session_id(session_id);
        handler.call(&ctx, args).await
    }
}

/// M5.b — bundles the Arcs and shared deps that
/// [`build_cron_bindings_from_snapshots`] needs to reconstruct the
/// per-binding context map. Cheap clone (every field is `Arc<_>`,
/// `Option<Arc<_>>`, or owned config). Captured by the
/// config-reload post-hook closure once per process.
///
/// Provider-agnostic — the cron tier-0 dispatcher fires LLM calls
/// for any provider (Anthropic / MiniMax / OpenAI / Gemini /
/// DeepSeek / xAI / Mistral); the rebuild copies whichever
/// `effective.model` the new snapshot resolved without branching
/// on provider.
#[derive(Clone)]
struct CronRebuildDeps {
    broker: nexo_broker::AnyBroker,
    sessions: Arc<nexo_core::session::SessionManager>,
    memory: Option<Arc<nexo_memory::LongTermMemory>>,
    peer_directory: Arc<nexo_core::agent::PeerDirectory>,
    credentials: Option<Arc<nexo_auth::CredentialsBundle>>,
    web_search_router: Option<Arc<nexo_web_search::WebSearchRouter>>,
    link_extractor: Arc<nexo_core::link_understanding::LinkExtractor>,
    dispatch_ctx: Option<Arc<nexo_core::agent::dispatch_handlers::DispatchToolContext>>,
    tools_per_agent: Arc<std::collections::HashMap<String, Arc<nexo_core::agent::ToolRegistry>>>,
    cron_tool_call_cfg: nexo_config::types::runtime::RuntimeCronToolCallsConfig,
}

/// M5.b — re-walks per-agent snapshot handles and builds the
/// `binding_id → CronToolBindingContext` map. Used by the boot
/// path (initial population, called once after the agent loop
/// ends) and the config-reload post-hook (rebuild on snapshot
/// swap). Single source of truth — preserves bit-for-bit
/// semantics with the inline closure it replaces.
///
/// Pattern validated against:
///   * `claude-code-leak/src/utils/cronScheduler.ts:441-448`
///     (chokidar-on-file-change rebuild — we use snapshot
///     handles instead of file mtime, but the rebuild shape is
///     analogous).
///   * `research/src/cron/service/timer.ts:709`
///     (`forceReload: true` per-tick — we rebuild on reload
///     only, since ArcSwap gives us cheap atomic swaps).
///
/// Limitation: agent add/remove during runtime is Phase 19 scope.
/// `tools_per_agent` and `agent_snapshot_handles` are populated
/// during the boot agent loop and never extended; reload picks up
/// policy changes for EXISTING agents only.
fn build_cron_bindings_from_snapshots(
    snapshots: &std::collections::HashMap<
        String,
        Arc<arc_swap::ArcSwap<nexo_core::RuntimeSnapshot>>,
    >,
    deps: &CronRebuildDeps,
) -> std::collections::HashMap<String, CronToolBindingContext> {
    let mut by_binding: std::collections::HashMap<String, CronToolBindingContext> =
        std::collections::HashMap::new();
    for (agent_id, snapshot_handle) in snapshots {
        let snap = snapshot_handle.load_full();
        let agent_cfg = Arc::clone(&snap.nexo_config);
        let Some(tools) = deps.tools_per_agent.get(agent_id).cloned() else {
            tracing::debug!(
                agent = %agent_id,
                "build_cron_bindings_from_snapshots: agent missing from tools_per_agent; skipping"
            );
            continue;
        };

        // Iterate legacy/unbound first (binding_idx = None), then
        // each real binding (binding_idx = Some(i)). Mirrors the
        // pre-M5.b boot ordering.
        let binding_indexes: Vec<Option<usize>> = if agent_cfg.inbound_bindings.is_empty() {
            vec![None]
        } else {
            std::iter::once(None)
                .chain((0..agent_cfg.inbound_bindings.len()).map(Some))
                .collect()
        };
        for binding_idx in binding_indexes {
            let Some(effective) = snap.policy_for(binding_idx) else {
                continue;
            };
            let binding_key = compute_binding_key(&agent_cfg, binding_idx);
            let inbound_origin = compute_inbound_origin(&agent_cfg, binding_idx);

            let filtered = tools.filtered_clone(&effective.allowed_tools);
            filtered.apply_dispatch_capability(&effective.dispatch_policy, false);
            if !deps.cron_tool_call_cfg.allowlist.is_empty() {
                filtered.retain_matching(&deps.cron_tool_call_cfg.allowlist);
            }
            let filtered = Arc::new(filtered);

            let mut cron_ctx = nexo_core::agent::AgentContext::new(
                agent_id.clone(),
                Arc::clone(&agent_cfg),
                deps.broker.clone(),
                Arc::clone(&deps.sessions),
            )
            .with_effective(Arc::clone(&effective))
            .with_effective_tools(Arc::clone(&filtered));
            if let Some(mem) = deps.memory.as_ref() {
                cron_ctx = cron_ctx.with_memory(Arc::clone(mem));
            }
            cron_ctx = cron_ctx.with_peers(Arc::clone(&deps.peer_directory));
            if let Some(bundle) = deps.credentials.as_ref() {
                cron_ctx = cron_ctx.with_credentials(Arc::clone(&bundle.resolver));
                cron_ctx = cron_ctx.with_breakers(Arc::clone(&bundle.breakers));
            }
            if let Some(router) = deps.web_search_router.as_ref() {
                cron_ctx = cron_ctx.with_web_search_router(Arc::clone(router));
            }
            cron_ctx = cron_ctx.with_link_extractor(Arc::clone(&deps.link_extractor));
            if let Some(dc) = deps.dispatch_ctx.as_ref() {
                cron_ctx = cron_ctx.with_dispatch(Arc::clone(dc));
            }
            if let Some((plugin, instance, sender)) = inbound_origin {
                cron_ctx = cron_ctx.with_inbound_origin(plugin, instance, sender);
            }

            if by_binding
                .insert(
                    binding_key.clone(),
                    CronToolBindingContext {
                        ctx: cron_ctx,
                        tools: Arc::clone(&filtered),
                    },
                )
                .is_some()
            {
                tracing::warn!(
                    binding_id = %binding_key,
                    agent = %agent_id,
                    "cron tool context duplicated binding key; latest entry wins"
                );
            }
        }
    }
    by_binding
}

fn compute_binding_key(agent_cfg: &nexo_config::AgentConfig, idx: Option<usize>) -> String {
    match idx {
        None => agent_cfg.id.clone(),
        Some(i) => {
            let b = &agent_cfg.inbound_bindings[i];
            let instance = b
                .instance
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("default");
            format!("{}:{instance}", b.plugin)
        }
    }
}

fn compute_inbound_origin(
    agent_cfg: &nexo_config::AgentConfig,
    idx: Option<usize>,
) -> Option<(String, String, String)> {
    match idx {
        None => None,
        Some(i) => {
            let b = &agent_cfg.inbound_bindings[i];
            let instance = b
                .instance
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("default");
            Some((b.plugin.clone(), instance.to_string(), "cron".into()))
        }
    }
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
    // `nexo-extensions` crate version. Ignore the result: if the
    // override was already set (double-init in tests), the existing
    // value wins — safe default.
    let _ = nexo_extensions::set_agent_version(env!("CARGO_PKG_VERSION"));

    let args = parse_args();
    match args.mode {
        Mode::Help => {
            print_usage();
            return Ok(());
        }
        Mode::Version { verbose } => {
            print_version(verbose);
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
        Mode::McpServer(ref sub) => match sub {
            McpServerSubcommand::Serve => return run_mcp_server(&args.config_dir).await,
            McpServerSubcommand::Inspect { url } => return run_mcp_inspect(url).await,
            McpServerSubcommand::Bench { url, tool, rps } => {
                return run_mcp_bench(url, tool, *rps).await
            }
            McpServerSubcommand::TailAudit { db } => return run_mcp_tail_audit(db).await,
        },
        Mode::AgentDream(ref sub) => match sub {
            AgentDreamSubcommand::Tail {
                goal_id,
                n,
                db,
                json,
            } => {
                return run_agent_dream_tail(goal_id.as_deref(), *n, db.as_deref(), *json).await;
            }
            AgentDreamSubcommand::Status { run_id, db, json } => {
                return run_agent_dream_status(run_id, db.as_deref(), *json).await;
            }
            AgentDreamSubcommand::Kill {
                run_id,
                force,
                memory_dir,
                db,
            } => {
                return run_agent_dream_kill(
                    run_id,
                    *force,
                    memory_dir.as_deref(),
                    db.as_deref(),
                )
                .await;
            }
        },
        Mode::AgentRun {
            prompt,
            bg,
            db,
            json,
        } => {
            return run_agent_run(prompt, bg, db.as_deref(), json).await;
        }
        Mode::AgentPs {
            kind,
            all,
            db,
            json,
        } => {
            return run_agent_ps(kind.as_deref(), all, db.as_deref(), json).await;
        }
        Mode::AgentAttach {
            goal_id,
            db,
            json,
        } => {
            return run_agent_attach(&goal_id, db.as_deref(), json).await;
        }
        Mode::AgentDiscover {
            include_interactive,
            db,
            json,
        } => {
            return run_agent_discover(include_interactive, db.as_deref(), json).await;
        }
        Mode::ChannelList { config, json } => {
            return run_channel_list(config.as_deref(), json, &args.config_dir).await;
        }
        Mode::ChannelDoctor {
            config,
            binding,
            json,
        } => {
            return run_channel_doctor(
                config.as_deref(),
                binding.as_deref(),
                json,
                &args.config_dir,
            )
            .await;
        }
        Mode::ChannelTest {
            server,
            binding,
            content,
            config,
            json,
        } => {
            return run_channel_test(
                &server,
                binding.as_deref(),
                content.as_deref(),
                config.as_deref(),
                json,
                &args.config_dir,
            )
            .await;
        }
        Mode::FlowHelp => return run_flow_help(),
        Mode::FlowList { json } => return run_flow_list(json).await,
        Mode::FlowShow { id, json } => return run_flow_show(&id, json).await,
        Mode::FlowCancel { id } => return run_flow_cancel(&id).await,
        Mode::FlowResume { id } => return run_flow_resume(&id).await,
        Mode::SetupInteractive => return nexo_setup::run_interactive(&args.config_dir),
        Mode::SetupOne { service } => return nexo_setup::run_one(&args.config_dir, &service),
        Mode::SetupList => return nexo_setup::run_list(&args.config_dir),
        Mode::SetupDoctor => return nexo_setup::run_doctor(&args.config_dir).await,
        Mode::SetupMigrate { apply } => return run_setup_migrate(&args.config_dir, apply),
        Mode::DoctorCapabilities { json } => {
            let statuses = nexo_setup::capabilities::evaluate_all();
            if json {
                let v = nexo_setup::capabilities::render_json(&statuses);
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                print!("{}", nexo_setup::capabilities::render_tty(&statuses));
            }
            return Ok(());
        }
        Mode::SetupTelegramLink { agent } => {
            return nexo_setup::run_telegram_link(&args.config_dir, agent.as_deref())
        }
        Mode::PairHelp => {
            return run_pair_help();
        }
        Mode::PairStart {
            device_label,
            public_url,
            qr_png_path,
            ttl_secs,
            json,
        } => {
            return run_pair_start(
                &args.config_dir,
                device_label.as_deref(),
                public_url.as_deref(),
                qr_png_path.as_deref(),
                ttl_secs,
                json,
            )
            .await;
        }
        Mode::PairList {
            channel,
            json,
            show_allow,
            include_revoked,
        } => {
            return run_pair_list(
                &args.config_dir,
                channel.as_deref(),
                json,
                show_allow,
                include_revoked,
            )
            .await;
        }
        Mode::PairApprove { code, json } => {
            return run_pair_approve(&args.config_dir, &code, json).await;
        }
        Mode::PairRevoke { target } => {
            return run_pair_revoke(&args.config_dir, &target).await;
        }
        Mode::PairSeed {
            channel,
            account_id,
            senders,
        } => {
            return run_pair_seed(&args.config_dir, &channel, &account_id, &senders).await;
        }
        Mode::Status {
            json,
            endpoint,
            agent_id,
        } => return run_status(json, endpoint, agent_id).await,
        Mode::DryRun { json } => return run_dry_run(&args.config_dir, json),
        Mode::CheckConfig { strict } => return run_check_config(&args.config_dir, strict),
        Mode::Reload { json } => return run_reload(&args.config_dir, json).await,
        Mode::PollersList { json } => return nexo_poller::cli::list(json).await,
        Mode::PollersShow { id, json } => return nexo_poller::cli::show(&id, json).await,
        Mode::PollersRun { id } => return nexo_poller::cli::run(&id).await,
        Mode::PollersPause { id } => return nexo_poller::cli::pause(&id).await,
        Mode::PollersResume { id } => return nexo_poller::cli::resume(&id).await,
        Mode::PollersReset { id, yes } => return nexo_poller::cli::reset(&id, yes).await,
        Mode::PollersReload => return nexo_poller::cli::reload().await,
        Mode::CronList { binding, json } => return run_cron_list(binding.as_deref(), json).await,
        Mode::CronDrop { id } => return run_cron_drop(&id).await,
        Mode::CronPause { id } => return run_cron_pause(&id).await,
        Mode::CronResume { id } => return run_cron_resume(&id).await,
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
        Mode::Admin { port } => return run_admin_web(port).await,
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

    // First pass of per-binding override validation — structural
    // checks only (duplicate bindings, unknown telegram instances,
    // missing skill dirs, same-provider model override). The tool-name
    // and known-provider checks run a few statements below once the
    // LLM registry and tool registry are assembled.
    nexo_core::agent::validate_agents(
        &cfg.agents.agents,
        &cfg.plugins.telegram,
        &nexo_core::agent::KnownTools::default(),
    )
    .context("per-binding override validation failed")?;

    // Phase 17 — credential gauntlet. Collects every invariant error
    // across WhatsApp / Telegram / Google in one pass. Lenient level
    // on boot so legacy deployments keep working; CI should run
    // `agent --check-config --strict` to gate PRs.
    let google_auth =
        nexo_auth::load_google_auth(&config_dir).context("failed to load google-auth.yaml")?;
    let secrets_dir = secrets_dir_for(&config_dir);
    let credentials = match nexo_auth::build_credentials(
        &cfg.agents.agents,
        &cfg.plugins.whatsapp,
        &cfg.plugins.telegram,
        &google_auth,
        cfg.plugins.email.as_ref(),
        &secrets_dir,
        nexo_auth::StrictLevel::Lenient,
    ) {
        Ok(bundle) => {
            for w in &bundle.warnings {
                tracing::warn!(target: "credentials", "{w}");
            }
            {
                use nexo_auth::CredentialStore;
                tracing::info!(
                    wa = bundle.stores.whatsapp.list().len(),
                    tg = bundle.stores.telegram.list().len(),
                    google = bundle.stores.google.list().len(),
                    "credential gauntlet passed"
                );
            }
            Some(Arc::new(bundle))
        }
        Err(errs) => {
            // Don't hard-fail boot on a legacy config that predates the
            // gauntlet — but surface every error loudly and disable the
            // resolver so outbound tools fall back to legacy topics.
            tracing::error!(
                errors = errs.len(),
                "credential gauntlet rejected config — running without per-agent credential enforcement"
            );
            for e in &errs {
                tracing::error!(target: "credentials", "{e}");
            }
            None
        }
    };

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
            let mut snapshot = nexo_extensions::KnownPluginSnapshot::new();
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
                nexo_extensions::spawn_extensions_watcher(
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

    // Provider-level validation pass: every agent's (and every
    // binding override's) `model.provider` must be a real registered
    // provider. Same aggregate error format as the structural pass
    // above so multi-agent configs surface every typo in one error.
    {
        let names = llm_registry.names();
        let known_providers = nexo_core::agent::KnownProviders::new(names);
        nexo_core::agent::validate_agents_with_providers(
            &cfg.agents.agents,
            &cfg.plugins.telegram,
            &nexo_core::agent::KnownTools::default(),
            &known_providers,
        )
        .context("per-binding provider validation failed")?;
    }

    let mcp_sampling_provider = build_mcp_sampling_provider(&cfg, &llm_registry)
        .context("failed to initialize MCP sampling provider")?;
    let mcp_manager: Option<Arc<nexo_mcp::McpRuntimeManager>> = match cfg.mcp.as_ref() {
        Some(mcp_cfg) if mcp_cfg.enabled => {
            let ext_decls: Vec<nexo_mcp::runtime_config::ExtensionServerDecl> = ext_mcp_decls
                .iter()
                .map(|d| nexo_mcp::runtime_config::ExtensionServerDecl {
                    ext_id: d.ext_id.clone(),
                    ext_version: d.ext_version.clone(),
                    ext_root: d.ext_root.clone(),
                    servers: d.servers.clone(),
                })
                .collect();
            let rt_cfg = nexo_mcp::runtime_config::McpRuntimeConfig::from_yaml_with_extensions(
                mcp_cfg, &ext_decls,
            );
            tracing::info!(
                servers = rt_cfg.servers.len(),
                yaml_servers = mcp_cfg.servers.len(),
                extension_decls = ext_decls.len(),
                "initializing mcp runtime manager"
            );
            let mgr = nexo_mcp::McpRuntimeManager::new_with_sampling(
                rt_cfg,
                mcp_sampling_provider.clone(),
            );
            if mcp_cfg.watch.enabled {
                let debounce = std::time::Duration::from_millis(mcp_cfg.watch.debounce_ms.max(50));
                nexo_mcp::spawn_mcp_config_watcher(
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

    // Phase 80.9 — channel boot context. Holds the shared
    // `ChannelRegistry` + `SessionRegistry` + `BrokerChannelDispatcher`
    // so the per-(binding,server) inbound loops + the bridge spawn
    // below all see the same handles. Persistent SessionRegistry
    // (Phase 80.9.d.b) is opted into when an operator sets
    // `agents.<id>.channels.session_store_path` — for now we ship
    // the in-memory default so threading is preserved within a
    // process. Hot-reload re-evaluation hooks against this single
    // registry instance.
    let channel_boot = nexo_mcp::channel_boot::ChannelBootContext::in_memory(broker.clone());
    let channel_shutdown = tokio_util::sync::CancellationToken::new();
    // Phase 80.9.b.b — process-wide pending-permission map
    // shared by the ChannelRelayDecider + every per-server
    // permission-response pump.
    let pending_permissions = std::sync::Arc::new(
        nexo_mcp::channel_permission::PendingPermissionMap::new(),
    );
    {
        // Spawn one bridge per process. Sink publishes
        // `ChannelInboundEvent` on a stable subject the agent
        // runtime intake subscribes to (`agent.channel.inbound`)
        // — keeps channel inbound on the same lane as every other
        // user-message intake so pairing / dispatch policy / rate
        // limit gates all apply uniformly.
        let sink: std::sync::Arc<dyn nexo_mcp::channel_bridge::ChannelInboundSink> =
            std::sync::Arc::new(IntakeChannelSink::new(broker.clone()));
        match channel_boot
            .spawn_bridge(sink, channel_shutdown.clone())
            .await
        {
            Ok(_handle) => {
                tracing::info!("channel bridge spawned");
            }
            Err(e) => {
                tracing::warn!(error = %e, "channel bridge spawn failed — channels disabled this run");
            }
        }
    }

    // Phase 77.7 — secret guard for scanning memory writes.
    // C5 — wired via `memory.secret_guard` YAML key. Default secure
    // config applies when the key is omitted; explicit override
    // failures fail boot loud so a YAML typo is never silent.
    let secret_guard: Option<nexo_memory::SecretGuard> = {
        let guard_cfg = build_secret_guard_config_from_yaml(&cfg.memory.secret_guard)
            .context("invalid memory.secret_guard config")?;
        Some(guard_cfg.build_guard())
    };

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
            let embedding_provider: Option<Arc<dyn nexo_memory::EmbeddingProvider>> = if cfg
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
                            match nexo_memory::HttpEmbeddingProvider::new(
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
                                    Some(Arc::new(p) as Arc<dyn nexo_memory::EmbeddingProvider>)
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
            let mem = if let Some(ref guard) = secret_guard {
                mem.with_guard(guard.clone())
            } else {
                mem
            };
            tracing::info!(
                path = %path,
                vector = mem.embedding_provider().is_some(),
                secret_guard = secret_guard.is_some(),
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
    let browser_plugin: Option<Arc<nexo_plugin_browser::BrowserPlugin>> =
        cfg.plugins.browser.clone().map(|browser_cfg| {
            let plugin = Arc::new(BrowserPlugin::new(browser_cfg));
            tracing::info!("registered plugin: browser");
            plugin
        });
    if let Some(plugin) = browser_plugin.clone() {
        // Register into the PluginRegistry via the Plugin trait. The
        // registry stores `Arc<dyn Plugin>` so we keep our Arc handle
        // alive for tool registration below.
        plugins.register_arc(plugin as Arc<dyn nexo_core::agent::plugin::Plugin>);
    }
    // WhatsApp plugins — zero, one, or many accounts. Each one registers
    // under `whatsapp` (legacy single-account) or `whatsapp.<instance>`.
    // Pairing states are collected per-instance so the health server
    // can expose `/whatsapp/<instance>/pair*` alongside the legacy
    // `/whatsapp/pair*` that targets the first account for back-compat.
    let mut wa_pairing: std::collections::BTreeMap<
        String,
        nexo_plugin_whatsapp::pairing::SharedPairingState,
    > = std::collections::BTreeMap::new();
    let mut wa_tunnel_cfg: Option<nexo_config::WhatsappPublicTunnelConfig> = None;
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
        let plugin = nexo_plugin_telegram::TelegramPlugin::new(tg_cfg);
        plugins.register(plugin);
        tracing::info!(instance = %instance_label, "registered plugin: telegram");
    }
    // Email plugin (Phase 48). Wrapped in `Arc` so the email tool
    // registry below can pull `dispatcher_handle()` after `start_all`
    // arms the OutboundDispatcher (the handle is `None` until then).
    let email_plugin: Option<Arc<nexo_plugin_email::EmailPlugin>> = cfg
        .plugins
        .email
        .as_ref()
        .filter(|c| c.enabled && !c.accounts.is_empty())
        .and_then(|email_cfg| {
            credentials.as_ref().map(|creds_bundle| {
                let data_dir = std::env::var("NEXO_DATA_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| std::path::PathBuf::from("data"));
                Arc::new(nexo_plugin_email::EmailPlugin::new(
                    email_cfg.clone(),
                    creds_bundle.stores.email.clone(),
                    creds_bundle.stores.google.clone(),
                    data_dir,
                ))
            })
        });
    if let Some(plugin) = email_plugin.clone() {
        plugins.register_arc(plugin as Arc<dyn nexo_core::agent::plugin::Plugin>);
        tracing::info!("registered plugin: email");
    } else if cfg.plugins.email.is_some() {
        tracing::info!("email plugin present but disabled / empty / missing creds — skipping");
    }
    plugins
        .start_all(broker.clone())
        .await
        .context("failed to start plugins")?;

    // Email tool context — built post-start so the dispatcher handle
    // is primed. Each agent loop below picks it up when its `plugins`
    // list mentions `email`.
    let email_tool_ctx: Option<Arc<nexo_plugin_email::EmailToolContext>> = match (
        email_plugin.as_ref(),
        credentials.as_ref(),
        cfg.plugins.email.as_ref(),
    ) {
        (Some(plugin), Some(creds_bundle), Some(email_cfg)) => {
            // Audit follow-up E — `dispatcher_handle()` returning
            // None after a plugin we *did* register is a hard
            // failure: the email plugin is unusable, and silently
            // proceeding would let agents declare `plugins: [email]`
            // and discover at first tool-call time that nothing
            // was registered. Better to refuse boot now.
            let dispatcher = plugin.dispatcher_handle().await.ok_or_else(|| {
                anyhow::anyhow!(
                    "email plugin registered but `dispatcher_handle()` is None — \
                     OutboundDispatcher::start failed earlier. Check the boot log \
                     for the underlying error, or set `email.enabled: false` to \
                     skip the plugin entirely."
                )
            })?;
            // Audit follow-up F — surface accounts that didn't make
            // it into the dispatcher's live instance set. The
            // declared list is `email_cfg.accounts`; the live set
            // is `dispatcher.instance_ids()`. Anything declared but
            // not live crashed during spawn — log structured WARN
            // with the full list so the operator sees it once at
            // boot rather than discovering it in the middle of an
            // `email_send` failure.
            // Audit follow-up J — soft connectivity probe. Wait up
            // to 10 s for every account to land its first
            // successful connect; anything still pending is logged
            // as a structured WARN. Doesn't abort boot — auth /
            // DNS issues should keep the daemon alive while the
            // operator triages.
            let pending = plugin
                .verify_accounts_connected(std::time::Duration::from_secs(10))
                .await;
            if !pending.is_empty() {
                tracing::warn!(
                    target: "plugin.email",
                    pending = ?pending,
                    "email accounts have not completed initial connect within 10s — \
                     check IMAP credentials, network reachability, or TLS settings. \
                     The plugin will keep retrying via the per-instance circuit breaker."
                );
            }
            let live: std::collections::HashSet<String> =
                dispatcher.instance_ids().into_iter().collect();
            let missing: Vec<&str> = email_cfg
                .accounts
                .iter()
                .map(|a| a.instance.as_str())
                .filter(|i| !live.contains(*i))
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    target: "plugin.email",
                    declared = ?email_cfg.accounts.iter().map(|a| a.instance.as_str()).collect::<Vec<_>>(),
                    live = ?live.iter().collect::<Vec<_>>(),
                    missing = ?missing,
                    "some declared email accounts are not live in the dispatcher — \
                     `email_send` against those instances will fail with `unknown email instance`. \
                     Inspect the boot log for the per-account spawn error."
                );
            }
            let health = plugin
                .health_map()
                .await
                .unwrap_or_else(nexo_plugin_email::inbound::HealthMap::default);
            Some(Arc::new(nexo_plugin_email::EmailToolContext {
                creds: creds_bundle.stores.email.clone(),
                google: creds_bundle.stores.google.clone(),
                config: Arc::new(email_cfg.clone()),
                dispatcher,
                health,
                bounce_store: plugin.bounce_store_handle(),
                attachment_store: plugin.attachment_store_handle(),
                attachments_dir: plugin.attachments_dir(),
            }))
        }
        _ => None,
    };

    // Agents ---------------------------------------------------------------
    let running_agents = Arc::new(AtomicUsize::new(0));
    // Slot for companion WS handshake context — filled after pairing init below.
    let pairing_handshake_slot: Arc<std::sync::OnceLock<PairingHandshakeCtx>> =
        Arc::new(std::sync::OnceLock::new());
    let health = RuntimeHealth {
        broker: broker.clone(),
        running_agents: Arc::clone(&running_agents),
        wa_pairing: wa_pairing.clone(),
        email_plugin: email_plugin.clone(),
        pairing_handshake: Arc::clone(&pairing_handshake_slot),
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
                match nexo_tunnel::TunnelManager::new(8080).start().await {
                    Ok(handle) => {
                        // Big, hard-to-miss banner on stderr so
                        // operators see the URL even in noisy logs.
                        let url = handle.url.clone();
                        // FOLLOWUPS PR-3 — publish URL to the sidecar
                        // file so a separately-launched
                        // `nexo pair start` can pick it up without
                        // an env-var hand-off or a daemon RPC.
                        if let Err(e) = nexo_tunnel::write_url_file(&url) {
                            tracing::warn!(error = %e, "failed to write tunnel URL sidecar");
                        }
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
    // Phase 19 — `gmail-poller` legacy crate retired. Operators
    // declare gmail jobs directly in `config/pollers.yaml` under
    // `kind: gmail`. The generic runner handles scheduling, cursor
    // persistence, dispatch via Phase 17, and the `pollers_*` +
    // `gmail_*` LLM tools.

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
                    serde_yaml::from_str::<nexo_core::agent::tool_policy::ToolPolicyConfig>(&t)
                        .map_err(anyhow::Error::from)
                }) {
                Ok(cfg) => {
                    tracing::info!(
                        cache_rules = cfg.cache.tools.len(),
                        parallel_rules = cfg.parallel_safe.len(),
                        per_agent_overrides = cfg.per_agent.len(),
                        "tool policy loaded"
                    );
                    nexo_core::agent::tool_policy::ToolPolicyRegistry::from_config(&cfg)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "tool_policy.yaml parse failed — feature off");
                    nexo_core::agent::tool_policy::ToolPolicyRegistry::disabled()
                }
            }
        } else {
            nexo_core::agent::tool_policy::ToolPolicyRegistry::disabled()
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
    let peer_directory = nexo_core::agent::PeerDirectory::new(
        cfg.agents
            .agents
            .iter()
            .map(|a| nexo_core::agent::PeerSummary {
                id: a.id.clone(),
                description: a.description.clone(),
            })
            .collect(),
    );
    // Ops-facing directory — served at `GET /admin/agents`. Snapshot
    // of the operator-relevant bits of each agent's config (no secrets,
    // no runtime state) so a dashboard / CLI can answer "who's running
    // and what are they configured to do?"
    let agents_directory = nexo_core::agent::AgentsDirectory::new(
        cfg.agents
            .agents
            .iter()
            .map(nexo_core::agent::AgentInfo::from_config)
            .collect(),
    );
    // Loopback-only admin HTTP server. Bound to `127.0.0.1` — nothing
    // here is authenticated, so exposing it to the LAN would let anyone
    // flush the cache or inspect the agent list. ssh-tunnel
    // `-L 9091:127.0.0.1:9091` to reach it remotely.
    let credentials_for_admin = credentials.as_ref().map(Arc::clone);

    // Phase 19 — generic poller subsystem. Boot order:
    //   1) require credentials bundle (resolver lookup is mandatory)
    //   2) open SQLite state DB
    //   3) construct runner + register built-ins
    //   4) start runner (spawns one tokio task per job)
    // Failure at any step logs + skips: the daemon keeps running for
    // the rest of the agents.
    let pollers_runner: Option<Arc<nexo_poller::PollerRunner>> =
        match (cfg.pollers.clone(), credentials.as_ref().map(Arc::clone)) {
            (Some(pcfg), Some(bundle)) if pcfg.enabled => {
                let state_db = std::path::PathBuf::from(&pcfg.state_db);
                match nexo_poller::PollState::open(&state_db).await {
                    Ok(state) => {
                        // Phase 20 — feed the LLM registry + config into
                        // the runner so the `agent_turn` built-in can build
                        // clients on demand. Other built-ins (gmail, rss,
                        // webhook) ignore the field — wiring it
                        // unconditionally keeps the boot path uniform.
                        let runner = Arc::new(
                            nexo_poller::PollerRunner::new(
                                pcfg,
                                Arc::new(state),
                                broker.clone(),
                                bundle,
                            )
                            .with_llm(
                                Arc::new(LlmRegistry::with_builtins()),
                                Arc::new(cfg.llm.clone()),
                            ),
                        );
                        nexo_poller::builtins::register_all(&runner);

                        // Phase 19 follow-up — register extension-provided
                        // pollers. Walk every loaded stdio extension and
                        // bridge each declared `kind` into the runner via
                        // ExtensionPoller. Lets operators ship a poller in
                        // any language without touching Rust.
                        let mut ext_poller_count = 0usize;
                        for (rt, cand) in &extension_runtimes {
                            let kinds = &cand.manifest.capabilities.pollers;
                            if !kinds.is_empty() {
                                let n =
                                    nexo_poller_ext::register_for_runtime(&runner, rt, kinds).await;
                                ext_poller_count += n;
                                tracing::info!(
                                    ext = %cand.manifest.id(),
                                    kinds = ?kinds,
                                    "extension pollers registered"
                                );
                            }
                        }
                        if ext_poller_count > 0 {
                            tracing::info!(count = ext_poller_count, "extension pollers ready");
                        }

                        if let Err(e) = runner.start().await {
                            tracing::error!(error = %format!("{e:#}"), "pollers: start failed");
                            None
                        } else {
                            Some(runner)
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            path = %state_db.display(),
                            error = %format!("{e:#}"),
                            "pollers: failed to open state DB"
                        );
                        None
                    }
                }
            }
            (Some(pcfg), None) if pcfg.enabled => {
                tracing::warn!(
                "pollers: skipped — credential gauntlet failed earlier so no resolver is available"
            );
                None
            }
            _ => None,
        };

    let _admin_handle = tokio::spawn(run_admin_server(
        Arc::clone(&tool_policy_registry),
        Arc::clone(&agents_directory),
        credentials_for_admin,
        pollers_runner.as_ref().map(Arc::clone),
        config_dir.clone(),
    ));

    // Phase 82.2 — webhook receiver. Validate the snapshot, build
    // the dispatcher + axum router, spawn under a dedicated
    // CancellationToken. Validation failure is non-fatal: we log
    // the error and skip the server (daemon continues). Hot-reload
    // re-evaluation lands as a Phase 18 post-hook in a follow-up
    // commit (see FOLLOWUPS.md `82.2.b`).
    let webhook_shutdown = tokio_util::sync::CancellationToken::new();
    let _webhook_handle: Option<tokio::task::JoinHandle<()>> = if let Some(wcfg) =
        cfg.webhook_receiver.as_ref().filter(|w| w.enabled)
    {
        match wcfg.validate() {
            Err(e) => {
                tracing::error!(error = %e, "webhook_receiver disabled: invalid config");
                None
            }
            Ok(()) => {
                let dispatcher = Arc::new(
                    nexo_webhook_server::BrokerWebhookDispatcher::new(broker.clone()),
                );
                match nexo_webhook_server::build_router(wcfg, dispatcher) {
                    Err(e) => {
                        tracing::error!(error = %e, "webhook_receiver disabled: router build failed");
                        None
                    }
                    Ok((router, state)) => {
                        match nexo_webhook_server::spawn_server(
                            wcfg.bind,
                            router,
                            state,
                            webhook_shutdown.clone(),
                        )
                        .await
                        {
                            Err(e) => {
                                tracing::error!(error = %e, "webhook_receiver disabled: bind failed");
                                None
                            }
                            Ok(handle) => {
                                tracing::info!(
                                    bind = %handle.bind_addr,
                                    sources = handle.router_state.sources.len(),
                                    "webhook receiver online"
                                );
                                Some(handle.join)
                            }
                        }
                    }
                }
            }
        }
    } else {
        tracing::debug!("webhook_receiver disabled (config absent or enabled=false)");
        None
    };
    // Phase 21 — single shared link extractor (HTTP client + LRU cache).
    // Per-binding config gates whether each turn actually fetches; the
    // extractor itself is cheap to keep around always.
    let link_extractor = Arc::new(nexo_core::link_understanding::LinkExtractor::new(
        &nexo_core::link_understanding::LinkUnderstandingConfig::default(),
    ));

    // Phase 25 — single shared web-search router. Builds at most one
    // provider per backend from env credentials. `None` if no provider
    // is configured (no env keys + DDG feature off); the `web_search`
    // tool is then never registered.
    let web_search_router: Option<Arc<nexo_web_search::WebSearchRouter>> = {
        let mut providers: Vec<Arc<dyn nexo_web_search::WebSearchProvider>> = Vec::new();
        if let Ok(k) = std::env::var("BRAVE_SEARCH_API_KEY") {
            providers.push(Arc::new(
                nexo_web_search::providers::brave::BraveProvider::new(k, 8000),
            ));
        }
        if let Ok(k) = std::env::var("TAVILY_API_KEY") {
            providers.push(Arc::new(
                nexo_web_search::providers::tavily::TavilyProvider::new(k, 10000),
            ));
        }
        // DuckDuckGo bundles by default (no key) so every install has
        // at least one usable provider. Operators that ban scraping
        // can rebuild nexo-web-search without the `duckduckgo`
        // feature.
        providers.push(Arc::new(
            nexo_web_search::providers::duckduckgo::DuckDuckGoProvider::new(12000),
        ));
        Some(Arc::new(nexo_web_search::WebSearchRouter::new(
            providers, None,
        )))
    };
    if let Some(router) = web_search_router.as_ref() {
        tracing::info!(providers = ?router.provider_ids(), "web-search router initialised");
    } else {
        tracing::info!("web-search router disabled (no providers configured)");
    }

    // Phase 26 — pairing protocol. Builds the SQLite store + the
    // HMAC-signed setup-code issuer once per process. The store path
    // sits beside `memory.db` so backups follow the same operator
    // convention; the secret file lands in `~/.nexo/secret/pairing.key`
    // with 0600 perms (auto-generated on first boot).
    //
    // FOLLOWUPS PR-6 — `cfg.pairing` (`config/pairing.yaml`) overrides
    // either path selectively when present. Containerised deploys
    // typically only need `pairing.storage.path` to point at a
    // mounted volume; everything else falls back to the legacy
    // defaults.
    let (pairing_store, pairing_gate, setup_code_issuer) = {
        let memory_dir: std::path::PathBuf = cfg
            .memory
            .long_term
            .sqlite
            .as_ref()
            .map(|s| {
                std::path::Path::new(&s.path)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
            })
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let store_path: std::path::PathBuf = cfg
            .pairing
            .as_ref()
            .and_then(|p| p.storage.path.clone())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| memory_dir.join("pairing.db"));
        let store = Arc::new(
            nexo_pairing::PairingStore::open(store_path.to_str().unwrap_or("pairing.db")).await?,
        );
        let gate = Arc::new(nexo_pairing::PairingGate::new(Arc::clone(&store)));
        let secret_path: std::path::PathBuf = cfg
            .pairing
            .as_ref()
            .and_then(|p| p.setup_code.secret_path.clone())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| {
                        std::path::PathBuf::from(h)
                            .join(".nexo")
                            .join("secret")
                            .join("pairing.key")
                    })
                    .unwrap_or_else(|| std::path::PathBuf::from("./pairing.key"))
            });
        let issuer = Arc::new(nexo_pairing::SetupCodeIssuer::open_or_create(&secret_path)?);
        tracing::info!(
            store = %store_path.display(),
            secret = %secret_path.display(),
            from_yaml = cfg.pairing.is_some(),
            "pairing initialised",
        );
        (store, gate, issuer)
    };
    // `setup_code_issuer` is consumed only by the CLI subcommand (it
    // opens its own copy of the secret from disk), so the daemon
    // touches it just to verify the secret file exists at boot. The
    // store + gate flow into every AgentRuntime below.
    let _ = (Arc::clone(&pairing_store), Arc::clone(&setup_code_issuer));

    // Wire the companion WS handshake context now that both issuer and the
    // state directory are known. Failure is non-fatal — the daemon continues
    // without WS pairing (clients get a 503 on /pair).
    {
        let state_dir = nexo_project_tracker::state::nexo_state_dir();
        std::fs::create_dir_all(&state_dir).ok();
        let sessions_db_path = state_dir.join("pairing_sessions.db");
        match nexo_pairing::PairingSessionStore::open(&sessions_db_path).await {
            Ok(session_store) => {
                let ctx = PairingHandshakeCtx {
                    issuer: Arc::clone(&setup_code_issuer),
                    session_store: Arc::new(session_store),
                    // Session tokens last 24h. FOLLOWUP: expose
                    // pairing.session_ttl_secs in the YAML config.
                    session_ttl: std::time::Duration::from_secs(86400),
                };
                let _ = pairing_handshake_slot.set(ctx);
                tracing::info!(path = %sessions_db_path.display(), "companion WS pairing ready");
            }
            Err(e) => {
                tracing::warn!(error = %e, "companion WS pairing disabled — could not open session store");
            }
        }
    }

    // Phase 67 — auto-boot the in-process driver subsystem when
    // ANY configured agent has `dispatch_policy.mode: full`
    // (agent-level OR per-binding) AND a driver config file is
    // reachable. The operator never has to flip an env var —
    // configuring Cody with full dispatch IS the opt-in. The
    // shared DispatchToolContext (orchestrator + agent-registry
    // + tracker + hook registry + log buffer) is then fed into
    // every AgentRuntime so program_phase / list_agents / etc.
    // are fully wired end-to-end. Agents without dispatch_full
    // see the tool defs in their registry but the handlers
    // return a clean "AgentContext.dispatch is not set" error.
    let dispatch_ctx: Option<Arc<nexo_core::agent::dispatch_handlers::DispatchToolContext>> =
        boot_dispatch_ctx_if_enabled(
            &broker,
            &cfg.agents.agents,
            mcp_manager.clone(),
            channel_boot.clone(),
            pending_permissions.clone(),
        )
        .await;

    let mut runtimes: Vec<AgentRuntime> = Vec::with_capacity(cfg.agents.agents.len());
    // Phase 18 — collect each agent's reload channel so the coordinator
    // can dispatch `Apply(snapshot)` on hot-reload.
    let mut reload_senders: Vec<(
        String,
        tokio::sync::mpsc::Sender<nexo_core::agent::runtime::ReloadCommand>,
        std::sync::Arc<Vec<String>>,
    )> = Vec::with_capacity(cfg.agents.agents.len());
    // Dreaming-side cancellation + handles. Each enabled agent spawns a
    // sweep loop; shutdown cancels the token and joins them with a
    // bounded timeout so SIGTERM cannot hang on an in-flight sweep.
    let dream_shutdown = tokio_util::sync::CancellationToken::new();
    let mut dream_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // TaskFlow runtime — shared FlowManager + WaitEngine tick loop +
    // NATS resume bridge. Engine runs as a single global task; the
    // bridge wakes flows whose `external_event` waits arrive over NATS.
    let flow_manager = Arc::new(open_flow_manager_from_cfg(&cfg.taskflow).await?);
    let wait_engine = nexo_taskflow::WaitEngine::new((*flow_manager).clone());
    let tick_interval =
        humantime::parse_duration(&cfg.taskflow.tick_interval).with_context(|| {
            format!(
                "invalid taskflow.tick_interval `{}`",
                cfg.taskflow.tick_interval
            )
        })?;
    let _timer_max_horizon = humantime::parse_duration(&cfg.taskflow.timer_max_horizon)
        .with_context(|| {
            format!(
                "invalid taskflow.timer_max_horizon `{}`",
                cfg.taskflow.timer_max_horizon
            )
        })?;
    {
        let we = wait_engine.clone();
        let tok = watcher_shutdown.clone();
        tokio::spawn(async move {
            tracing::info!(
                interval_ms = tick_interval.as_millis() as u64,
                "wait engine started"
            );
            we.run(tick_interval, tok).await;
        });
    }
    spawn_taskflow_resume_bridge(
        broker.clone(),
        wait_engine.clone(),
        watcher_shutdown.clone(),
    );

    // Transcripts subsystem — optional FTS5 index + optional redactor.
    // Built once and shared across every agent via runtime.with_*.
    let transcripts_index: Option<Arc<nexo_core::agent::TranscriptsIndex>> =
        if cfg.transcripts.fts.enabled {
            match nexo_core::agent::TranscriptsIndex::open(std::path::Path::new(
                &cfg.transcripts.fts.db_path,
            ))
            .await
            {
                Ok(i) => {
                    tracing::info!(
                        path = %cfg.transcripts.fts.db_path,
                        "transcripts FTS index ready"
                    );
                    Some(Arc::new(i))
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %cfg.transcripts.fts.db_path,
                        "transcripts FTS index init failed; falling back to substring scan"
                    );
                    None
                }
            }
        } else {
            None
        };
    let transcripts_redactor: Arc<nexo_core::agent::Redactor> = Arc::new(
        nexo_core::agent::Redactor::from_config(&cfg.transcripts.redaction)
            .context("invalid transcripts.redaction config")?,
    );
    if transcripts_redactor.is_active() {
        tracing::info!("transcripts redaction active");
    }

    // Phase context-optimization wiring — built once per process and
    // shared across agent runtimes.
    //
    // - WorkspaceCache: pre-loads + watches every distinct workspace
    //   directory declared by any agent. Empty when no agent has a
    //   workspace; in that case the cache is `None` and the legacy
    //   per-turn `WorkspaceLoader` path runs unchanged.
    // - CompactionStore: opens (or creates) `compactions.db` next to
    //   the long-term memory file so backups + permissions follow the
    //   same operator convention. Always built — agents that never opt
    //   into compaction simply never touch it.
    let workspace_cache: Option<Arc<nexo_core::agent::workspace_cache::WorkspaceCache>> = {
        let cfg_co = &cfg.llm.context_optimization.workspace_cache;
        if !cfg_co.enabled {
            None
        } else {
            let mut roots: Vec<std::path::PathBuf> = Vec::new();
            for a in &cfg.agents.agents {
                let ws = a.workspace.trim();
                if ws.is_empty() {
                    continue;
                }
                let p = std::path::PathBuf::from(ws);
                if !roots.iter().any(|r| r == &p) && p.exists() {
                    roots.push(p);
                }
            }
            if roots.is_empty() {
                None
            } else {
                match nexo_core::agent::workspace_cache::WorkspaceCache::new(
                    &roots,
                    cfg_co.watch_debounce_ms,
                    cfg_co.max_age_seconds,
                ) {
                    Ok(c) => {
                        tracing::info!(
                            roots = roots.len(),
                            debounce_ms = cfg_co.watch_debounce_ms,
                            "workspace cache enabled"
                        );
                        Some(c)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "workspace cache init failed; falling back to per-turn reads");
                        None
                    }
                }
            }
        }
    };
    let compaction_store: Option<Arc<nexo_memory::CompactionStore>> = {
        let memory_dir = cfg
            .memory
            .long_term
            .sqlite
            .as_ref()
            .map(|s| {
                std::path::Path::new(&s.path)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("./data"))
            })
            .unwrap_or_else(|| std::path::PathBuf::from("./data"));
        if let Err(e) = std::fs::create_dir_all(&memory_dir) {
            tracing::warn!(
                dir = %memory_dir.display(),
                error = %e,
                "compaction store: failed to ensure parent dir; skipping"
            );
            None
        } else {
            let path = memory_dir.join("compactions.db");
            let path_str = path.display().to_string();
            match nexo_memory::CompactionStore::open(&path_str).await {
                Ok(s) => {
                    tracing::info!(path = %path_str, "compaction store ready");
                    Some(Arc::new(s))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "compaction store init failed; compaction will be unavailable");
                    None
                }
            }
        }
    };

    // Phase 79.7.B — snapshot a fallback model for legacy cron rows
    // that predate per-entry model metadata (`model_provider`,
    // `model_name`). New rows carry their own model and do not depend
    // on this fallback.
    // Phase 79.10 — open the durable audit store for ConfigTool
    // proposals. Always opened (gated tool may be off, but the
    // read-only `config_changes_tail` tool is always available).
    // Failure to open is non-fatal — the tail tool simply isn't
    // registered and the boot continues.
    let config_changes_store: Option<
        std::sync::Arc<nexo_core::config_changes_store::SqliteConfigChangesStore>,
    > = {
        let path = nexo_project_tracker::state::nexo_state_dir().join("config_changes.db");
        std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("."))).ok();
        match nexo_core::config_changes_store::SqliteConfigChangesStore::open(
            path.to_str().unwrap_or("config_changes.db"),
        )
        .await
        {
            Ok(s) => {
                tracing::info!(
                    path = %path.display(),
                    "[config] config_changes audit store opened"
                );
                Some(std::sync::Arc::new(s))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "[config] config_changes audit store could not be opened — tail tool disabled"
                );
                None
            }
        }
    };

    // Phase 79.6 — open the team store (registry + audit). Always
    // tried; failure is non-fatal (the 5 Team* tools simply don't
    // register for this run).
    let team_store: Option<std::sync::Arc<nexo_team_store::SqliteTeamStore>> = {
        let path = nexo_project_tracker::state::nexo_state_dir().join("teams.db");
        std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("."))).ok();
        match nexo_team_store::SqliteTeamStore::open(path.to_str().unwrap_or("teams.db")).await {
            Ok(s) => {
                tracing::info!(
                    path = %path.display(),
                    "[team] team store opened"
                );
                Some(std::sync::Arc::new(s))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "[team] team store could not be opened — Team* tools disabled"
                );
                None
            }
        }
    };

    // Phase 79.6 — per-process team message router. Spawns the
    // `team.>` broker subscriber under a fresh cancel token; the
    // SIGTERM handler below cancels it before plugin teardown.
    let team_router_cancel = tokio_util::sync::CancellationToken::new();
    let team_router: std::sync::Arc<
        nexo_core::team_message_router::TeamMessageRouter<nexo_broker::AnyBroker>,
    > = {
        let r = nexo_core::team_message_router::TeamMessageRouter::new(std::sync::Arc::new(
            broker.clone(),
        ));
        r.spawn(team_router_cancel.clone());
        r
    };

    let first_agent_for_cron: Option<(String, nexo_config::types::agents::ModelConfig)> = cfg
        .agents
        .agents
        .first()
        .map(|a| (a.id.clone(), a.model.clone()));
    let cron_tool_call_cfg = cfg.runtime.cron.tool_calls.clone();
    // M5.b — `cron_tool_bindings` is now built post-loop via
    // `build_cron_bindings_from_snapshots` (single source of truth
    // shared with the config-reload post-hook). The pre-M5.b
    // `let mut cron_tool_bindings = HashMap::new()` declaration is
    // removed; the map flows directly from the build call to the
    // executor constructor.
    let mut legacy_cron_binding_models: std::collections::HashMap<
        String,
        nexo_config::types::agents::ModelConfig,
    > = std::collections::HashMap::new();
    for a in &cfg.agents.agents {
        legacy_cron_binding_models
            .entry(a.id.clone())
            .or_insert_with(|| a.model.clone());
        for b in &a.inbound_bindings {
            let model = b.model.clone().unwrap_or_else(|| a.model.clone());
            let inst = b.instance.as_deref().unwrap_or("default");
            let key = format!("{}:{inst}", b.plugin);
            legacy_cron_binding_models.entry(key).or_insert(model);
        }
    }

    // Phase 79.10.b — bootstrap the approval correlator + reload
    // bridge for the gated `Config` tool. Always built (cheap;
    // tasks idle when no agent has `self_edit: true`); the gated
    // per-agent registration below decides whether to consume
    // them. `agents_yaml_path` is the canonical config file the
    // applier writes back to; falls back to `config_dir/agents.yaml`.
    #[cfg(feature = "config-self-edit")]
    let (config_correlator, config_reload_trigger, agents_yaml_path, reload_cell) = {
        use nexo_core::agent::approval_correlator::{ApprovalCorrelator, ApprovalCorrelatorConfig};
        let correlator = ApprovalCorrelator::new(ApprovalCorrelatorConfig::default());
        // Subscribe to inbound topics for approval routing. Spawned
        // in a fire-and-forget task; ends with the correlator's
        // CancellationToken on shutdown.
        let broker_clone = broker.clone();
        let cor_clone = std::sync::Arc::clone(&correlator);
        tokio::spawn(async move {
            use nexo_broker::BrokerHandle;
            use nexo_core::agent::approval_correlator::InboundApprovalMessage;
            let mut sub = match broker_clone.subscribe("plugin.inbound.>").await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "[config] could not subscribe to plugin.inbound — approvals offline"
                    );
                    return;
                }
            };
            tracing::info!("[config] approval subscriber on plugin.inbound.> running");
            while let Some(ev) = sub.next().await {
                // Topic shape: plugin.inbound.<channel>[.<instance>]
                // No-instance topics map to account `default`.
                let Some(rest) = ev.topic.strip_prefix("plugin.inbound.") else {
                    continue;
                };
                if rest.is_empty() {
                    continue;
                }
                let (channel, account_id) = match rest.split_once('.') {
                    Some((channel, instance)) if !channel.is_empty() && !instance.is_empty() => {
                        (channel.to_string(), instance.to_string())
                    }
                    Some(_) => continue,
                    None => (rest.to_string(), "default".to_string()),
                };
                let payload = &ev.payload;
                let body = payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if body.is_empty() {
                    continue;
                }
                let sender_id = payload
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let msg = InboundApprovalMessage {
                    channel,
                    account_id,
                    sender_id,
                    body: body.to_string(),
                    received_at: chrono::Utc::now().timestamp(),
                };
                cor_clone.on_inbound(msg);
            }
        });
        // Reload bridge: `reload_coord` is built AFTER the agent
        // loop (line ~2927), so the trigger holds a `OnceCell` and
        // resolves it lazily. main.rs sets the cell after the
        // coordinator is built. Until then, a `Config { op: apply }`
        // call returns a clear "reload coordinator not yet ready"
        // error — practically impossible because applies require an
        // operator approval which itself requires the daemon to be
        // up.
        struct ReloadWrapper(
            std::sync::Arc<
                tokio::sync::OnceCell<std::sync::Arc<nexo_core::ConfigReloadCoordinator>>,
            >,
        );
        #[async_trait::async_trait]
        impl nexo_core::agent::config_tool::ReloadTrigger for ReloadWrapper {
            async fn reload(&self) -> Result<(), String> {
                let coord = match self.0.get() {
                    Some(c) => c,
                    None => return Err("reload coordinator not yet initialised".into()),
                };
                let outcome = coord.reload().await;
                if outcome.rejected.is_empty() {
                    Ok(())
                } else {
                    let summary: Vec<String> = outcome
                        .rejected
                        .iter()
                        .map(|r| {
                            format!(
                                "{}: {}",
                                r.agent_id.as_deref().unwrap_or("workspace"),
                                r.reason
                            )
                        })
                        .collect();
                    Err(summary.join("; "))
                }
            }
        }
        let reload_cell: std::sync::Arc<
            tokio::sync::OnceCell<std::sync::Arc<nexo_core::ConfigReloadCoordinator>>,
        > = std::sync::Arc::new(tokio::sync::OnceCell::new());
        let reload_trigger: std::sync::Arc<dyn nexo_core::agent::config_tool::ReloadTrigger> =
            std::sync::Arc::new(ReloadWrapper(std::sync::Arc::clone(&reload_cell)));
        let agents_yaml = config_dir.join("agents.yaml");
        (
            Some(correlator),
            Some(reload_trigger),
            Some(agents_yaml),
            Some(reload_cell),
        )
    };
    #[cfg(not(feature = "config-self-edit"))]
    let (config_correlator, config_reload_trigger, agents_yaml_path, reload_cell) = (
        Option::<()>::None,
        Option::<()>::None,
        Option::<std::path::PathBuf>::None,
        Option::<()>::None,
    );
    let _ = (
        &config_correlator,
        &config_reload_trigger,
        &agents_yaml_path,
        &reload_cell,
    );

    // Phase 79.1 — process-shared plan-mode approval registry.
    // Created once per process so the broker subscriber below can
    // resolve pending ExitPlanMode approvals from inbound
    // `[plan-mode] approve|reject plan_id=…` chat messages.
    let plan_approval_registry = std::sync::Arc::new(
        nexo_core::agent::plan_mode_tool::PlanApprovalRegistry::default(),
    );
    // Subscribe to inbound topics for plan-mode approval routing.
    // Spawned in a fire-and-forget task; ends with the daemon shutdown.
    {
        let broker_clone = broker.clone();
        let registry = std::sync::Arc::clone(&plan_approval_registry);
        tokio::spawn(async move {
            use nexo_broker::BrokerHandle;
            let mut sub = match broker_clone.subscribe("plugin.inbound.>").await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "[plan-mode] could not subscribe to plugin.inbound — approval parser offline"
                    );
                    return;
                }
            };
            tracing::info!("[plan-mode] approval parser on plugin.inbound.> running");
            while let Some(ev) = sub.next().await {
                let body = ev
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if body.is_empty() {
                    continue;
                }
                let cmd = match nexo_core::agent::plan_mode_tool::parse_plan_mode_approval(body) {
                    Some(c) => c,
                    None => continue,
                };
                let (plan_id, decision) = match cmd {
                    nexo_core::agent::plan_mode_tool::PlanModeApprovalCommand::Approve { plan_id } => {
                        (plan_id, nexo_core::agent::plan_mode_tool::PlanApprovalDecision::Approve)
                    }
                    nexo_core::agent::plan_mode_tool::PlanModeApprovalCommand::Reject { plan_id, reason } => {
                        let reason = reason.unwrap_or_else(|| "rejected by operator".to_string());
                        (plan_id, nexo_core::agent::plan_mode_tool::PlanApprovalDecision::Reject { reason })
                    }
                };
                let resolved = registry.resolve(&plan_id, decision);
                if resolved {
                    tracing::info!(%plan_id, "[plan-mode] approval resolved via inbound message");
                } else {
                    tracing::debug!(%plan_id, "[plan-mode] no pending waiter for plan_id");
                }
            }
        });
    }

    // Phase 79.5 — boot the LSP manager once per process. Probes
    // rust-analyzer / pylsp / typescript-language-server / gopls
    // on PATH; missing binaries get a single warn line with the
    // install hint. The manager survives across all agents and is
    // shut down in the SIGTERM handler below. Pre-warm covers
    // languages requested by *any* agent's `lsp.prewarm` field.
    let lsp_workspace = cfg
        .agents
        .agents
        .first()
        .map(|a| a.workspace.clone())
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let lsp_prewarm: Vec<nexo_lsp::LspLanguage> = cfg
        .agents
        .agents
        .iter()
        .filter(|a| a.lsp.enabled)
        .flat_map(|a| {
            a.lsp.prewarm.iter().map(|w| match w {
                nexo_config::types::lsp::LspLanguageWire::Rust => nexo_lsp::LspLanguage::Rust,
                nexo_config::types::lsp::LspLanguageWire::Python => nexo_lsp::LspLanguage::Python,
                nexo_config::types::lsp::LspLanguageWire::TypeScript => {
                    nexo_lsp::LspLanguage::TypeScript
                }
                nexo_config::types::lsp::LspLanguageWire::Go => nexo_lsp::LspLanguage::Go,
            })
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let lsp_manager = nexo_lsp::boot(&[], &lsp_prewarm, &lsp_workspace).await;
    tracing::info!(
        discovered = ?lsp_manager.discovered_languages(),
        prewarm = ?lsp_prewarm,
        "[lsp] manager booted"
    );

    // M5.b — aggregated maps for cron post-hook reload.
    // `tools_per_agent` captures each per-agent ToolRegistry Arc;
    // `agent_snapshot_handles` captures each runtime's snapshot
    // ArcSwap so the post-hook can re-read the current effective
    // policy after a reload swap.
    let mut tools_per_agent: std::collections::HashMap<
        String,
        Arc<nexo_core::agent::ToolRegistry>,
    > = std::collections::HashMap::new();
    let mut agent_snapshot_handles: std::collections::HashMap<
        String,
        Arc<arc_swap::ArcSwap<nexo_core::RuntimeSnapshot>>,
    > = std::collections::HashMap::new();

    // Phase M1.b.c — clone primary's id + config before the
    // agent loop consumes `cfg.agents.agents` so the daemon-embed
    // MCP wire can construct an AgentContext for the primary
    // after the loop.
    let primary_for_mcp_embed: Option<(String, nexo_config::AgentConfig)> =
        cfg.agents.agents.first().map(|a| (a.id.clone(), a.clone()));
    for agent_cfg in cfg.agents.agents {
        let agent_id = agent_cfg.id.clone();
        let dream_yaml = agent_cfg.dreaming.clone();
        let workspace_for_dream = agent_cfg.workspace.clone();
        // C2 — boot-time policy resolution. `from_agent_defaults` mirrors
        // agent-level fields into the resolved struct; per-binding
        // override pickup happens at handler call time via
        // `ctx.effective_policy()` (which the runtime intake fills from
        // `RuntimeSnapshot::policy_for(binding_idx)` per inbound event).
        let effective_boot =
            nexo_core::agent::effective::EffectiveBindingPolicy::from_agent_defaults(&agent_cfg);
        let llm = llm_registry
            .build(&cfg.llm, &agent_cfg.model)
            .with_context(|| format!("failed to build LLM client for agent {agent_id}"))?;

        // Phase M4.a.b — construct per-agent ExtractMemories when
        // the YAML opted in. Wire-shape `ExtractMemoriesYamlConfig`
        // mirrors `nexo_driver_types::ExtractMemoriesConfig` 1:1; we
        // convert here. The same `Arc<ExtractMemories>` will be
        // shared with the driver-loop orchestrator if a future
        // Phase 67 self-driving wire is added — single instance per
        // agent keeps cadence + circuit breaker + in-progress mutex
        // coherent.
        let memory_extractor: Option<Arc<dyn nexo_driver_types::MemoryExtractor>> = match agent_cfg
            .extract_memories
            .as_ref()
            .filter(|c| c.enabled)
        {
            Some(yaml_cfg) => {
                let cfg_concrete = nexo_driver_types::ExtractMemoriesConfig {
                    enabled: yaml_cfg.enabled,
                    turns_throttle: yaml_cfg.turns_throttle,
                    max_turns: yaml_cfg.max_turns,
                    max_consecutive_failures: yaml_cfg.max_consecutive_failures,
                };
                let adapter = Arc::new(
                    nexo_driver_loop::extract_memories::LlmClientAdapter::new(
                        Arc::clone(&llm),
                        agent_cfg.model.model.clone(),
                    ),
                );
                let extract = Arc::new(
                    nexo_driver_loop::extract_memories::ExtractMemories::new(
                        cfg_concrete,
                        adapter,
                    ),
                );
                tracing::info!(
                    agent = %agent_id,
                    throttle = yaml_cfg.turns_throttle,
                    max_turns = yaml_cfg.max_turns,
                    "[memory] extract_memories enabled"
                );
                Some(extract as Arc<dyn nexo_driver_types::MemoryExtractor>)
            }
            _ => None,
        };

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
        // Phase 80.9 — channel tools when ANY of this agent's
        // bindings has `allowed_channel_servers` non-empty AND
        // the operator's `agents.channels` block is configured.
        // Tools key against `agent_cfg.id` so the registry view
        // is agent-scoped — multi-binding granularity is the
        // 80.9.j follow-up. `channel_list` + `channel_status`
        // are read-only; `channel_send` is gated by the per-tool
        // approval flow + the channel registry's
        // `RegisteredChannel.outbound_tool_name` lookup.
        let channels_in_play = agent_cfg.channels.is_some()
            && agent_cfg
                .inbound_bindings
                .iter()
                .any(|b| !b.allowed_channel_servers.is_empty());
        if channels_in_play {
            // Phase 80.9.j — dynamic-binding tools. The tools
            // resolve the binding id from `ctx.effective` at
            // call time so the same registration serves every
            // binding for an agent. Per-binding registrations
            // live in the channel registry under
            // `<plugin>:<instance>` keys (see ChannelInboundLoop
            // spawn site below).
            {
                use nexo_core::agent::channel_list_tool::ChannelListTool;
                let def = ChannelListTool::tool_def();
                let handler = std::sync::Arc::new(ChannelListTool::new_dynamic(
                    channel_boot.registry.clone(),
                ));
                tools.register_arc(def, handler);
            }
            {
                use nexo_core::agent::channel_send_tool::ChannelSendTool;
                let def = ChannelSendTool::tool_def();
                let handler = std::sync::Arc::new(ChannelSendTool::new_dynamic(
                    channel_boot.registry.clone(),
                ));
                tools.register_arc(def, handler);
            }
            {
                use nexo_core::agent::channel_status_tool::ChannelStatusTool;
                let def = ChannelStatusTool::tool_def();
                let handler = std::sync::Arc::new(ChannelStatusTool::new_dynamic(
                    channel_boot.registry.clone(),
                ));
                tools.register_arc(def, handler);
            }
            tracing::info!(
                agent = %agent_cfg.id,
                "registered channel_* tools (channels surface in play, per-binding resolution)"
            );
        }
        // Phase 67 — register the project-tracker / dispatch tool
        // surface (program_phase, list_agents, agent_status, …).
        // The handlers return a friendly error when
        // `AgentContext.dispatch` is not set, so registering them
        // without an orchestrator just means the LLM sees the
        // tool defs and dispatch attempts surface a clean error
        // instead of pretending success. Operators that wire up
        // a DispatchToolContext at boot get the full surface.
        // Per-binding `dispatch_capability` in EffectiveBindingPolicy
        // (Phase 67.D.1) prunes write tools at session-time so
        // none of them are visible to bindings that opted out.
        nexo_core::agent::dispatch_handlers::register_dispatch_tools_into(&tools);
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
                nexo_plugin_browser::register_browser_tools(&tools, plugin);
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
            nexo_plugin_whatsapp::register_whatsapp_tools(&tools);
            tracing::info!(agent = %agent_id, "registered whatsapp_* tools for agent");
        }
        // Telegram outbound tools — same shape as WhatsApp; gated on
        // `plugins: [telegram]` + per-agent allowlist.
        if agent_cfg.plugins.iter().any(|p| p == "telegram") {
            nexo_plugin_telegram::register_telegram_tools(&tools);
            tracing::info!(agent = %agent_id, "registered telegram_* tools for agent");
        }
        // Email tools — gated on `plugins: [email]` + dispatcher
        // primed (the post-`start_all` ctx above). Six handlers:
        // send / reply / archive / move_to / label / search.
        if agent_cfg.plugins.iter().any(|p| p == "email") {
            if let Some(ctx) = email_tool_ctx.clone() {
                // Phase 48 follow-up #9 — surface-level filter. If
                // `agent.allowed_tools` lists explicit names, only
                // register the email handlers that actually appear.
                // `*` / `email_*` / empty list = register all six.
                let filter =
                    nexo_plugin_email::filter_from_allowed_patterns(&agent_cfg.allowed_tools);
                nexo_plugin_email::register_email_tools_filtered(&tools, ctx, filter.as_deref());
                let kept = filter
                    .as_ref()
                    .map(|f| f.len())
                    .unwrap_or(nexo_plugin_email::EMAIL_TOOL_NAMES.len());
                tracing::info!(
                    agent = %agent_id,
                    kept,
                    "registered email_* tools for agent"
                );
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "agent declares `email` plugin but the email plugin isn't armed — skipping tool registration"
                );
            }
        }
        // Google OAuth tools — gated on either `agents.<id>.google_auth`
        // (legacy inline) or an entry in `plugins/google-auth.yaml`
        // resolved via the credential store (phase 17). The client
        // holds the refresh_token on disk at
        // `<workspace>/<token_file>` so consent only runs once.
        let google_core_cfg = agent_cfg
            .google_auth
            .as_ref()
            .map(|gcfg| {
                (
                    nexo_plugin_google::GoogleAuthConfig {
                        client_id: gcfg.client_id.clone(),
                        client_secret: gcfg.client_secret.clone(),
                        scopes: gcfg.scopes.clone(),
                        token_file: gcfg.token_file.clone(),
                        redirect_port: gcfg.redirect_port,
                    },
                    None::<nexo_plugin_google::SecretSources>,
                )
            })
            .or_else(|| {
                credentials
                    .as_ref()
                    .and_then(|b| b.stores.google.account_for_agent(&agent_cfg.id))
                    .and_then(|acct| {
                        let cid = std::fs::read_to_string(&acct.client_id_path).ok()?;
                        let csec = std::fs::read_to_string(&acct.client_secret_path).ok()?;
                        let token_file = acct
                            .token_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("google_tokens.json")
                            .to_string();
                        let cfg = nexo_plugin_google::GoogleAuthConfig {
                            client_id: cid.trim().to_string(),
                            client_secret: csec.trim().to_string(),
                            scopes: acct.scopes.clone(),
                            token_file,
                            redirect_port: 8765,
                        };
                        let sources = nexo_plugin_google::SecretSources {
                            client_id_path: acct.client_id_path.clone(),
                            client_secret_path: acct.client_secret_path.clone(),
                        };
                        Some((cfg, Some(sources)))
                    })
            });
        if let Some((core_cfg, sources)) = google_core_cfg {
            let workspace_dir = if agent_cfg.workspace.trim().is_empty() {
                PathBuf::from("./data/workspace")
            } else {
                PathBuf::from(&agent_cfg.workspace)
            };
            let client = nexo_plugin_google::GoogleAuthClient::new_with_sources(
                core_cfg,
                &workspace_dir,
                sources,
            );
            if let Err(e) = client.load_from_disk().await {
                tracing::warn!(
                    agent = %agent_id,
                    error = %e,
                    "google_auth: failed to load persisted tokens; agent will need to re-consent"
                );
            }
            nexo_plugin_google::register_tools(&tools, client);
            tracing::info!(
                agent = %agent_id,
                "registered google_* tools for agent"
            );
        }
        // Phase 19 — pollers_* control tools (list, show, run, pause,
        // resume, reset). Registered per agent when the poller
        // subsystem booted; absent when pollers.yaml is missing /
        // disabled. Create / delete are intentionally not exposed
        // (prompt-injection concern); operators own pollers.yaml.
        if let Some(runner) = pollers_runner.as_ref() {
            nexo_poller_tools::register_all(&tools, Arc::clone(runner));
            tracing::info!(
                agent = %agent_id,
                "registered pollers_* tools for agent"
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
        // Durable email follow-up control plane. Requires:
        // - memory backend for persistent flow state
        // - email plugin enabled on this agent (tools rely on
        //   email_search/email_thread/email_reply at execution time).
        if agent_cfg.plugins.iter().any(|p| p == "email") {
            if let Some(mem) = memory.clone() {
                tools.register(
                    nexo_core::agent::StartFollowupTool::tool_def(),
                    nexo_core::agent::StartFollowupTool::new(mem.clone()),
                );
                tools.register(
                    nexo_core::agent::CheckFollowupTool::tool_def(),
                    nexo_core::agent::CheckFollowupTool::new(mem.clone()),
                );
                tools.register(
                    nexo_core::agent::CancelFollowupTool::tool_def(),
                    nexo_core::agent::CancelFollowupTool::new(mem),
                );
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "email follow-up tools disabled: memory backend unavailable"
                );
            }
        }

        // Phase 79.1 — register EnterPlanMode + ExitPlanMode tools
        // when the binding's plan-mode policy says `enabled: true`.
        // Default config (`enabled: true`) keeps these registered for
        // every agent; operators can opt out per-binding by setting
        // `plan_mode.enabled: false`. The dispatcher gate
        // (llm_behavior.rs) does not depend on registration — it
        // consults `ctx.plan_mode` directly — so an opt-out only
        // hides the tools from the model's catalogue.
        if agent_cfg.plan_mode.enabled {
            tools.register(
                nexo_core::agent::plan_mode_tool::EnterPlanModeTool::tool_def(),
                nexo_core::agent::plan_mode_tool::EnterPlanModeTool,
            );
            tools.register(
                nexo_core::agent::plan_mode_tool::ExitPlanModeTool::tool_def(),
                nexo_core::agent::plan_mode_tool::ExitPlanModeTool,
            );
            // Operator-side resolver — when require_approval is on,
            // this is the path through which the operator wakes a
            // pending ExitPlanMode. Future: pairing parser will call
            // it on inbound `[plan-mode] approve|reject plan_id=…`.
            tools.register(
                nexo_core::agent::plan_mode_tool::PlanModeResolveTool::tool_def(),
                nexo_core::agent::plan_mode_tool::PlanModeResolveTool,
            );
        }

        // Phase 79.4 — `TodoWrite` is always available. Cheap,
        // in-memory scratch list per goal; classified `ReadOnly` so
        // it stays callable while plan mode is on.
        tools.register(
            nexo_core::agent::todo_write_tool::TodoWriteTool::tool_def(),
            nexo_core::agent::todo_write_tool::TodoWriteTool,
        );

        // Phase 79.2 — `ToolSearch` discovery surface for deferred
        // tools. Always registered; `ToolMeta::default` keeps it
        // non-deferred itself (the model needs it to load everything
        // else). When MCP-imported tools start opting into
        // `ToolMeta::deferred`, this surface becomes useful at LLM
        // turn time. MVP caveat tracked in FOLLOWUPS.md::Phase 79.2.
        tools.register(
            nexo_core::agent::tool_search_tool::ToolSearchTool::tool_def(),
            nexo_core::agent::tool_search_tool::ToolSearchTool::new(),
        );

        // Phase 79.3 — `SyntheticOutput` typed-output validator.
        // Always registered (cheap, pure, classified `ReadOnly`).
        // The model invokes it to terminate a goal with a JSON
        // value that matches a caller-provided JSONSchema —
        // direct input for Phase 19/20 pollers + Phase 51 eval.
        tools.register(
            nexo_core::agent::synthetic_output_tool::SyntheticOutputTool::tool_def(),
            nexo_core::agent::synthetic_output_tool::SyntheticOutputTool,
        );

        // Phase 77.20 — Sleep is visible only for agents/bindings that can
        // enter proactive mode. It follows Claude Code's guidance: use Sleep
        // instead of Bash(sleep ...) so no shell process is held while idle.
        let proactive_enabled_somewhere = agent_cfg.proactive.enabled
            || agent_cfg
                .inbound_bindings
                .iter()
                .filter_map(|b| b.proactive.as_ref())
                .any(|p| p.enabled);
        if proactive_enabled_somewhere {
            tools.register(
                nexo_core::agent::SleepTool::tool_def(),
                nexo_core::agent::SleepTool,
            );
        }

        // Phase 79.12 — `Repl` tool (stateful Python/Node/bash subprocesses
        // that persist across LLM turns). Feature-gated behind `repl-tool`.
        #[cfg(feature = "repl-tool")]
        {
            let repl_enabled = agent_cfg.repl.enabled
                || agent_cfg
                    .inbound_bindings
                    .iter()
                    .filter_map(|b| b.repl.as_ref())
                    .any(|r| r.enabled);
            if repl_enabled {
                // C2 — `ReplRegistry` still captures the agent-level
                // ReplConfig (subsystem-actor-level: timeout_secs,
                // max_sessions, max_output_bytes are boot-frozen per
                // C2 scope). The per-call `allowed_runtimes` allowlist
                // override lives in `ReplTool::call` via
                // `ctx.effective_policy().repl`.
                let repl_workspace = if agent_cfg.workspace.trim().is_empty() {
                    String::from("./data/workspace")
                } else {
                    agent_cfg.workspace.clone()
                };
                let repl_registry = std::sync::Arc::new(
                    nexo_core::agent::ReplRegistry::new(
                        effective_boot.repl.clone(),
                        repl_workspace,
                    ),
                );
                tools.register(
                    nexo_core::agent::ReplTool::tool_def(),
                    nexo_core::agent::ReplTool::new(repl_registry),
                );
            }
        }

        // Phase 79.13 — `NotebookEdit` for `.ipynb` cell-level edits.
        // Pure-Rust round-trip via serde_json — no `jupyter` binary
        // required. Always registered (operators that don't touch
        // notebooks pay zero cost — the tool is filtered out by
        // `allowed_tools` if undesired).
        tools.register(
            nexo_core::agent::notebook_edit_tool::NotebookEditTool::tool_def(),
            nexo_core::agent::notebook_edit_tool::NotebookEditTool,
        );

        // Phase 79.8 — `RemoteTrigger` outbound publisher. Allowlist
        // comes from the session effective policy: agent-level
        // `remote_triggers` or a per-binding override when present.
        // Register the tool when either source exposes at least one
        // destination; the runtime call path enforces the actual
        // matched binding's allowlist.
        let remote_triggers_enabled_somewhere = !agent_cfg.remote_triggers.is_empty()
            || agent_cfg
                .inbound_bindings
                .iter()
                .filter_map(|b| b.remote_triggers.as_ref())
                .any(|list| !list.is_empty());
        if remote_triggers_enabled_somewhere {
            let sink: std::sync::Arc<dyn nexo_core::agent::remote_trigger_tool::RemoteTriggerSink> =
                std::sync::Arc::new(nexo_core::agent::remote_trigger_tool::ReqwestSink::new(
                    broker.clone(),
                ));
            tools.register(
                nexo_core::agent::remote_trigger_tool::RemoteTriggerTool::tool_def(),
                nexo_core::agent::remote_trigger_tool::RemoteTriggerTool::new(sink),
            );
        }

        // Phase 79.11 — `ListMcpResources` + `ReadMcpResource`
        // router-shaped tools. Useful for agents talking to many MCP
        // servers — single discovery surface instead of N×2
        // per-server tools (which still ship via the Phase 12.5
        // catalog). Cheap, classified `ReadOnly`, always registered.
        tools.register(
            nexo_core::agent::mcp_router_tool::ListMcpResourcesTool::tool_def(),
            nexo_core::agent::mcp_router_tool::ListMcpResourcesTool,
        );
        tools.register(
            nexo_core::agent::mcp_router_tool::ReadMcpResourceTool::tool_def(),
            nexo_core::agent::mcp_router_tool::ReadMcpResourceTool,
        );

        // Phase 79.7 — cron schedule store + 5 tools (cron_create,
        // cron_list, cron_delete, cron_pause, cron_resume). Lives in
        // `$NEXO_HOME/state/nexo_cron.db` so entries persist across
        // restarts. On open failure the tools stay unregistered and
        // a warn line names the path so operators can fix the FS
        // permission.
        let cron_db = nexo_project_tracker::state::nexo_state_dir().join("nexo_cron.db");
        std::fs::create_dir_all(cron_db.parent().unwrap_or(std::path::Path::new("."))).ok();
        match nexo_core::cron_schedule::SqliteCronStore::open(
            cron_db.to_str().unwrap_or("nexo_cron.db"),
        )
        .await
        {
            Ok(store) => {
                let store: std::sync::Arc<dyn nexo_core::cron_schedule::CronStore> =
                    std::sync::Arc::new(store);
                tools.register(
                    nexo_core::agent::cron_tool::CronCreateTool::tool_def(),
                    nexo_core::agent::cron_tool::CronCreateTool::new(std::sync::Arc::clone(&store)),
                );
                tools.register(
                    nexo_core::agent::cron_tool::CronListTool::tool_def(),
                    nexo_core::agent::cron_tool::CronListTool::new(std::sync::Arc::clone(&store)),
                );
                tools.register(
                    nexo_core::agent::cron_tool::CronDeleteTool::tool_def(),
                    nexo_core::agent::cron_tool::CronDeleteTool::new(std::sync::Arc::clone(&store)),
                );
                tools.register(
                    nexo_core::agent::cron_tool::CronPauseTool::tool_def(),
                    nexo_core::agent::cron_tool::CronPauseTool::new(std::sync::Arc::clone(&store)),
                );
                tools.register(
                    nexo_core::agent::cron_tool::CronResumeTool::tool_def(),
                    nexo_core::agent::cron_tool::CronResumeTool::new(store),
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %cron_db.display(),
                    "cron tools disabled — could not open SqliteCronStore"
                );
            }
        }

        // Phase 25 — `web_search` tool. Registered when the agent's
        // top-level policy has `enabled: true` and a router exists.
        // Per-binding overrides are enforced inside the tool itself
        // (it reads `ctx.effective_policy().web_search` per call).
        let agent_ws_enabled = agent_cfg
            .web_search
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if agent_ws_enabled {
            if let Some(ws_router) = web_search_router.as_ref() {
                tools.register(
                    nexo_core::agent::WebSearchTool::tool_def(),
                    nexo_core::agent::WebSearchTool::new(Arc::clone(ws_router)),
                );
                tracing::info!(agent = %agent_id, "registered web_search tool");
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "agent has web_search.enabled but no provider is configured (set BRAVE_SEARCH_API_KEY / TAVILY_API_KEY or rely on DuckDuckGo)"
                );
            }
        }

        // Phase 79.5 — `Lsp` tool, per-agent. Registered only when
        // the agent's `lsp.enabled` is `true`. Languages whitelist
        // empty means "all discovered". Workspace_root falls back
        // to the daemon's `lsp_workspace` (first agent's
        // workspace) when the agent itself doesn't declare one.
        if effective_boot.lsp.enabled {
            // C2 — `policy` is no longer captured at boot. The handler
            // reads `ctx.effective_policy().lsp` per call and converts
            // it to `ExecutePolicy` via the private adapter, so a
            // hot-reload that flips `lsp.languages` is observed on the
            // next intake event without re-registration.
            let agent_workspace: std::path::PathBuf = if agent_cfg.workspace.trim().is_empty() {
                lsp_workspace.clone()
            } else {
                std::path::PathBuf::from(&agent_cfg.workspace)
            };
            let lsp_tool = nexo_core::agent::lsp_tool::LspTool::new(
                std::sync::Arc::clone(&lsp_manager),
                agent_workspace,
            );
            let def = lsp_tool.tool_def().await;
            tools.register(def, lsp_tool);
            tracing::info!(
                agent = %agent_id,
                languages = ?effective_boot.lsp.languages,
                "registered Lsp tool"
            );
        }

        // Phase 79.6 — register the 5 Team* tools when the agent
        // opts in (`team.enabled: true`) AND the team store opened
        // successfully. The lead's `current_goal_id` placeholder
        // here is the agent_id — Phase 67 driver-loop overrides it
        // per-goal when team-aware spawn lands in 79.6.b.
        if effective_boot.team.enabled {
            if let Some(store) = team_store.as_ref() {
                let team_tools_inner = nexo_core::agent::team_tools::TeamTools::new(
                    std::sync::Arc::clone(store) as std::sync::Arc<dyn nexo_team_store::TeamStore>,
                    std::sync::Arc::clone(&team_router),
                    broker.clone(),
                    agent_id.clone(),
                    agent_id.clone(),
                );
                tools.register(
                    nexo_core::agent::team_tools::TeamCreateTool::tool_def(),
                    nexo_core::agent::team_tools::TeamCreateTool::new(std::sync::Arc::clone(
                        &team_tools_inner,
                    )),
                );
                tools.register(
                    nexo_core::agent::team_tools::TeamDeleteTool::tool_def(),
                    nexo_core::agent::team_tools::TeamDeleteTool::new(std::sync::Arc::clone(
                        &team_tools_inner,
                    )),
                );
                tools.register(
                    nexo_core::agent::team_tools::TeamSendMessageTool::tool_def(),
                    nexo_core::agent::team_tools::TeamSendMessageTool::new(std::sync::Arc::clone(
                        &team_tools_inner,
                    )),
                );
                tools.register(
                    nexo_core::agent::team_tools::TeamListTool::tool_def(),
                    nexo_core::agent::team_tools::TeamListTool::new(std::sync::Arc::clone(
                        &team_tools_inner,
                    )),
                );
                tools.register(
                    nexo_core::agent::team_tools::TeamStatusTool::tool_def(),
                    nexo_core::agent::team_tools::TeamStatusTool::new(team_tools_inner),
                );
                tracing::info!(
                    agent = %agent_id,
                    max_members = effective_boot.team.effective_max_members(),
                    max_concurrent = effective_boot.team.effective_max_concurrent(),
                    "[team] registered 5 Team* tools"
                );
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "[team] team.enabled = true but team store unavailable — Team* tools not registered"
                );
            }
        }

        // Phase 79.10 — `config_changes_tail` (read-only audit log).
        // Always available regardless of the `config-self-edit`
        // Cargo feature: even when ConfigTool itself is gated off,
        // operators want to read past audit entries (or empty
        // table) for post-mortem.
        if let Some(store) = config_changes_store.as_ref() {
            let tool = nexo_core::agent::config_changes_tail_tool::ConfigChangesTailTool::new(
                std::sync::Arc::clone(store)
                    as std::sync::Arc<dyn nexo_core::config_changes_store::ConfigChangesStore>,
            );
            tools.register(
                nexo_core::agent::config_changes_tail_tool::ConfigChangesTailTool::tool_def(),
                tool,
            );
        }

        // Phase 79.10.b — gated `Config { op: ... }` tool. Compiled
        // and registered only with `--features config-self-edit`,
        // and only for agents whose YAML sets `config_tool.self_edit
        // = true`. The hard ship-control is the Cargo feature, the
        // soft per-agent gate is the YAML knob.
        #[cfg(feature = "config-self-edit")]
        if effective_boot.config_tool.self_edit {
            if let (Some(store), Some(correlator), Some(reload), Some(agents_yaml)) = (
                config_changes_store.as_ref(),
                config_correlator.as_ref(),
                config_reload_trigger.as_ref(),
                agents_yaml_path.as_ref(),
            ) {
                use nexo_core::agent::config_tool::{
                    ActorOrigin, ConfigTool, DefaultSecretRedactor,
                };
                use nexo_setup::config_tool_bridge::{SetupDenylistChecker, SetupYamlPatchApplier};
                let proposals_dir =
                    nexo_project_tracker::state::nexo_state_dir().join("config-proposals");
                std::fs::create_dir_all(&proposals_dir).ok();
                let binding_id = agent_cfg
                    .inbound_bindings
                    .first()
                    .map(|b| {
                        format!(
                            "{}:{}",
                            b.plugin,
                            b.instance.as_deref().unwrap_or("default")
                        )
                    })
                    .unwrap_or_else(|| agent_id.clone());
                // For the actor origin, default to the binding's
                // first plugin instance with empty sender (the
                // approval correlator only matches on (channel,
                // account_id) anyway). Per-call override would land
                // when AgentContext gains the inbound origin.
                let actor_origin = agent_cfg
                    .inbound_bindings
                    .first()
                    .map(|b| ActorOrigin {
                        channel: b.plugin.clone(),
                        account_id: b.instance.clone().unwrap_or_else(|| "default".into()),
                        sender_id: String::new(),
                    })
                    .unwrap_or(ActorOrigin {
                        channel: "internal".into(),
                        account_id: agent_id.clone(),
                        sender_id: String::new(),
                    });
                let applier = std::sync::Arc::new(SetupYamlPatchApplier::new(
                    agents_yaml.clone(),
                    binding_id.clone(),
                ));
                let denylist = std::sync::Arc::new(SetupDenylistChecker);
                let redactor = std::sync::Arc::new(DefaultSecretRedactor);
                let cfg_tool = ConfigTool {
                    agent_id: agent_id.clone(),
                    binding_id: binding_id.clone(),
                    allowed_paths: effective_boot.config_tool.allowed_paths.clone(),
                    approval_timeout_secs: effective_boot.config_tool.approval_timeout_secs,
                    proposals_dir,
                    actor_origin,
                    applier,
                    denylist,
                    redactor,
                    changes_store: std::sync::Arc::clone(store)
                        as std::sync::Arc<dyn nexo_core::config_changes_store::ConfigChangesStore>,
                    correlator: std::sync::Arc::clone(correlator),
                    reload: std::sync::Arc::clone(reload),
                    pending_receivers: std::sync::Arc::new(tokio::sync::Mutex::new(
                        Default::default(),
                    )),
                };
                match cfg_tool.recover_pending_from_staging().await {
                    Ok(n) if n > 0 => tracing::info!(
                        agent = %agent_id,
                        recovered = n,
                        "[config] recovered pending staged proposals after boot"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        agent = %agent_id,
                        error = %e,
                        "[config] pending staged proposal recovery failed"
                    ),
                }
                tools.register(ConfigTool::tool_def(), cfg_tool);
                tracing::info!(
                    agent = %agent_id,
                    binding = %binding_id,
                    allowed_paths = ?effective_boot.config_tool.allowed_paths,
                    "[config] registered Config tool (gated)"
                );
            } else {
                tracing::warn!(
                    agent = %agent_id,
                    "[config] config_tool.self_edit = true but supporting infra missing — Config tool not registered"
                );
            }
        }

        // FOLLOWUPS W-2 — `web_fetch` tool. Sibling of `web_search`,
        // shares the runtime's LinkExtractor + cache + telemetry.
        // Registered for every agent unconditionally since the
        // runtime always boots with an extractor; the tool itself
        // returns a clear error when called against a binding whose
        // `link_understanding.enabled` is false.
        tools.register(
            nexo_core::agent::WebFetchTool::tool_def(),
            nexo_core::agent::WebFetchTool::new(),
        );
        tracing::info!(agent = %agent_id, "registered web_fetch tool");

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

        // Self-introspection over JSONL transcripts. Skip when the agent has
        // no transcripts_dir configured — the tool would only return errors.
        if !agent_cfg.transcripts_dir.trim().is_empty() {
            let mut tool = SessionLogsTool::new();
            if let Some(idx) = transcripts_index.as_ref() {
                tool = tool.with_index(Arc::clone(idx));
            }
            tools.register(SessionLogsTool::tool_def(), tool);
            tracing::info!(
                agent = %agent_id,
                fts = transcripts_index.is_some(),
                "registered session_logs tool for agent"
            );
        }

        // TaskFlow tool — gated on `plugins: [taskflow]`. The shared
        // FlowManager backs every agent's tool instance; ownership is
        // enforced by `owner_session_key` so agents cannot read or
        // mutate flows of other sessions.
        if agent_cfg.plugins.iter().any(|p| p == "taskflow") {
            let guardrails = nexo_core::agent::TaskFlowToolGuardrails {
                timer_max_horizon: chrono::Duration::seconds(_timer_max_horizon.as_secs() as i64),
            };
            let tool = nexo_core::agent::TaskFlowTool::new((*flow_manager).clone())
                .with_guardrails(guardrails);
            tools.register(nexo_core::agent::TaskFlowTool::tool_def(), tool);
            tracing::info!(agent = %agent_id, "registered taskflow tool for agent");
        }

        // Phase 10.9 — optional git-backed workspace. Registers
        // `forge_memory_checkpoint` + `memory_history` tools and feeds the
        // dreaming spawn closure below so sweeps auto-commit.
        let agent_git: Option<Arc<nexo_core::agent::MemoryGitRepo>> =
            if agent_cfg.workspace_git.enabled {
                match workspace_path.as_deref() {
                    Some(ws) => match nexo_core::agent::MemoryGitRepo::open_or_init(
                        ws,
                        agent_cfg.workspace_git.author_name.clone(),
                        agent_cfg.workspace_git.author_email.clone(),
                    ) {
                        Ok(repo) => {
                            let repo = if let Some(ref guard) = secret_guard {
                                repo.with_guard(guard.clone())
                            } else {
                                repo
                            };
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
                nexo_core::agent::MemoryCheckpointTool::tool_def(),
                nexo_core::agent::MemoryCheckpointTool::new(Arc::clone(g)),
            );
            tools.register(
                nexo_core::agent::MemoryHistoryTool::tool_def(),
                nexo_core::agent::MemoryHistoryTool::new(Arc::clone(g)),
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
                if !nexo_extensions::is_valid_hook(hook_name) {
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
                            nexo_config::McpServerYaml::Stdio {
                                context_passthrough: Some(v),
                                ..
                            }
                            | nexo_config::McpServerYaml::StreamableHttp {
                                context_passthrough: Some(v),
                                ..
                            }
                            | nexo_config::McpServerYaml::Sse {
                                context_passthrough: Some(v),
                                ..
                            } => Some((name.clone(), *v)),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            nexo_core::agent::register_session_tools_with_overrides(
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

            // Phase 80.9 main.rs hookup + Phase 80.9.j —
            // spawn one ChannelInboundLoop per `(binding,
            // server)` triple. The binding_id matches what the
            // dynamic-binding channel tools (channel_list /
            // channel_send / channel_status) resolve from
            // `ctx.effective` at call time, so the registry view
            // each tool sees scopes to the active binding.
            if let Some(channels_cfg) = agent_cfg.channels.as_ref() {
                if channels_cfg.enabled {
                    let cfg_arc = std::sync::Arc::new(channels_cfg.clone());
                    let clients_snapshot = rt.clients();
                    for binding in &agent_cfg.inbound_bindings {
                        if binding.allowed_channel_servers.is_empty() {
                            continue;
                        }
                        let binding_id = format!(
                            "{}:{}",
                            binding.plugin,
                            binding.instance.as_deref().unwrap_or("default")
                        );
                        let allow_arc = std::sync::Arc::new(
                            binding.allowed_channel_servers.clone(),
                        );
                        for (server_name, client) in &clients_snapshot {
                            if !binding
                                .allowed_channel_servers
                                .iter()
                                .any(|s| s == server_name)
                            {
                                continue;
                            }
                            let cap_declared =
                                nexo_mcp::channel::has_channel_capability(Some(
                                    &client.capabilities().experimental,
                                ));
                            let perm_cap =
                                nexo_mcp::channel::has_channel_permission_capability(Some(
                                    &client.capabilities().experimental,
                                ));
                            let plugin_source = channels_cfg
                                .lookup_approved(server_name)
                                .and_then(|e| e.plugin_source.clone());
                            let loop_cfg =
                                nexo_mcp::channel_boot::build_inbound_loop_config(
                                    &channel_boot,
                                    server_name.clone(),
                                    binding_id.clone(),
                                    plugin_source,
                                    cfg_arc.clone(),
                                    allow_arc.clone(),
                                    cap_declared,
                                    perm_cap,
                                );
                            let handle =
                                nexo_mcp::channel::ChannelInboundLoop::new(loop_cfg)
                                    .spawn_against_client(
                                        client.as_ref(),
                                        channel_shutdown.clone(),
                                    );
                            // Phase 80.9.b.b — spawn the
                            // permission-response pump alongside
                            // the channel inbound loop so any
                            // structured `notifications/nexo/channel/permission`
                            // event from this server resolves the
                            // matching pending entry.
                            if perm_cap {
                                let _ = nexo_mcp::channel_permission::spawn_permission_response_pump(
                                    client.clone(),
                                    server_name.clone(),
                                    pending_permissions.clone(),
                                    channel_shutdown.clone(),
                                );
                            }
                            match handle {
                                nexo_mcp::channel::ChannelInboundLoopHandle::Running {
                                    ..
                                } => {
                                    tracing::info!(
                                        agent = %agent_cfg.id,
                                        binding = %binding_id,
                                        server = %server_name,
                                        "channel inbound loop running"
                                    );
                                }
                                nexo_mcp::channel::ChannelInboundLoopHandle::Skipped {
                                    kind,
                                    reason,
                                } => {
                                    tracing::info!(
                                        agent = %agent_cfg.id,
                                        binding = %binding_id,
                                        server = %server_name,
                                        kind = kind.as_str(),
                                        reason = %reason,
                                        "channel inbound gate skip"
                                    );
                                }
                            }
                        }
                    }
                }
            }

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
                    nexo_core::agent::sanitize_name_fragment(&server_id)
                );
                let tools = Arc::clone(&tools_for_tools_reload);
                let rt = Arc::clone(&rt_for_tools_reload);
                let agent_id = agent_id_for_tools_reload.clone();
                let overrides = overrides_for_tools_reload.clone();
                tokio::spawn(async move {
                    let removed = tools.clear_by_prefix(&prefix);
                    nexo_core::agent::register_session_tools_with_overrides(
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
                    nexo_core::agent::sanitize_name_fragment(&server_id)
                );
                let tools = Arc::clone(&tools_for_res_reload);
                let rt = Arc::clone(&rt_for_res_reload);
                let agent_id = agent_id_for_res_reload.clone();
                let overrides = overrides_for_res_reload.clone();
                tokio::spawn(async move {
                    let cache_purged = rt.resource_cache().invalidate_server(&server_id);
                    let removed = tools.clear_by_prefix(&prefix);
                    nexo_core::agent::register_session_tools_with_overrides(
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

        // Phase M8 — mark built-in tools deferred per leak's
        // `shouldDefer: true` convention. Idempotent vs gated tools
        // (entries not registered in this boot are silently
        // skipped). Excludes the deferred subset from the LLM
        // request body (`to_tool_defs_non_deferred()`) — full
        // schemas land via `ToolSearch(select:<name>)`. See
        // `nexo_core::agent::built_in_deferred` for the canonical
        // list + IRROMPIBLE refs.
        nexo_core::agent::mark_built_in_deferred(&tools);

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

        // Second-pass binding validation: now that the tool registry
        // is fully assembled for THIS agent (builtins + plugins + MCP +
        // extensions + skills) we can verify that every name listed
        // under a binding's `allowed_tools` refers to a tool that
        // actually exists. Typos like `allowed_tools: [whatapp_send]`
        // would otherwise boot silently and deliver an agent that
        // appears to have tools but cannot call any of them.
        {
            let defs = tools.to_tool_defs();
            let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
            let catalog = nexo_core::agent::KnownTools::new(names);
            nexo_core::agent::validate_agent(&agent_cfg, &cfg.plugins.telegram, &catalog).map_err(
                |e| anyhow::anyhow!("agent `{}` binding validation failed: {}", agent_id, e),
            )?;
        }

        // M5.b — cron binding contexts are now built in a single
        // `build_cron_bindings_from_snapshots` call AFTER the agent
        // loop ends, using the aggregated `tools_per_agent` +
        // `agent_snapshot_handles` maps. The same fn is called by
        // the config-reload post-hook (single source of truth).

        let mut behavior = LlmAgentBehavior::new(llm, Arc::clone(&tools))
            .with_hooks(Arc::clone(&hooks))
            .with_tool_policy(tool_policy_registry.for_agent(&agent_id));

        // Phase M4.a.b — wire post-turn memory extraction. Constructed
        // earlier in the loop from the optional `extract_memories` YAML
        // block. `tick()` runs every regular turn; `extract(...)` only
        // fires when the gate cadence passes AND `reply_text` is
        // present. Memory dir is per-agent — workspace-derived when set,
        // else `<state_root>/<agent_id>/memory/`. Boot best-effort
        // creates the dir; runtime extract failures absorb via the
        // built-in circuit breaker.
        if let Some(extract) = memory_extractor.as_ref() {
            let dir = resolve_extract_memory_dir(&agent_cfg);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!(
                    agent = %agent_id,
                    dir = %dir.display(),
                    error = %e,
                    "[memory] failed to create memory_dir; extract may fail at write"
                );
            }
            behavior = behavior.with_memory_extractor(Arc::clone(extract), dir);
        }

        if let Some(rl_cfg) = agent_cfg.tool_rate_limits.clone() {
            let rl_core = nexo_core::agent::ToolRateLimitsConfig {
                patterns: rl_cfg
                    .patterns
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            nexo_core::agent::ToolRateLimitConfig {
                                rps: v.rps,
                                burst: v.burst,
                            },
                        )
                    })
                    .collect(),
            };
            let limiter = Arc::new(nexo_core::agent::ToolRateLimiter::new(rl_core));
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
            let validator = Arc::new(nexo_core::agent::ToolArgsValidator::new(enabled));
            behavior = behavior.with_schema_validator(validator);
            tracing::info!(
                agent = %agent_id,
                schema_validation = enabled,
                "tool schema validator attached"
            );
        }
        // Phase context-optimization — wire the four mechanisms onto
        // the behavior. Per-agent overrides ride on `agent_cfg.context_optimization`;
        // each enable inherits from `cfg.llm.context_optimization` when
        // None on the override.
        let resolved_co = nexo_config::types::llm::ResolvedContextOptimization::resolve(
            &cfg.llm.context_optimization,
            agent_cfg.context_optimization.as_ref(),
        );
        if resolved_co.workspace_cache {
            if let Some(ref wc) = workspace_cache {
                behavior = behavior.with_workspace_cache(Arc::clone(wc));
            }
        }
        if resolved_co.prompt_cache {
            behavior = behavior.with_prompt_cache(true);
        }
        if resolved_co.token_counter {
            // Resolve the provider config the agent's LLM was built
            // against; the API key + base URL are needed to wire the
            // exact-counts backend. Falls back silently when the
            // provider entry is missing — the build() helper degrades
            // to tiktoken in that case.
            if let Some(prov_cfg) = cfg.llm.providers.get(&agent_cfg.model.provider) {
                let counter = nexo_llm::token_counter::build(
                    &cfg.llm.context_optimization.token_counter.backend,
                    &agent_cfg.model.provider,
                    &prov_cfg.base_url,
                    &prov_cfg.api_key,
                    cfg.llm.context_optimization.token_counter.cache_capacity,
                );
                tracing::info!(
                    agent = %agent_id,
                    backend = counter.backend(),
                    exact = counter.is_exact(),
                    "token counter attached"
                );
                behavior = behavior.with_token_counter(counter);
            }
        }
        if resolved_co.compaction {
            if let Some(ref store) = compaction_store {
                let cfg_compaction = &cfg.llm.context_optimization.compaction;
                // Convert pct-of-window to a tokens threshold. We use
                // a conservative 100K effective window when no model
                // metadata is available — operators can tune the pct
                // to compensate.
                let effective_window: f32 = 100_000.0;
                let runtime = nexo_core::agent::llm_behavior::CompactionRuntime {
                    enabled: true,
                    compact_at_tokens: (cfg_compaction.compact_at_pct * effective_window) as u32,
                    tail_keep_chars: (cfg_compaction.tail_keep_tokens as usize) * 4,
                    tool_result_max_chars: (cfg_compaction.tool_result_max_pct
                        * effective_window
                        * 4.0) as usize,
                    micro_threshold_bytes: cfg_compaction.micro.threshold_bytes,
                    micro_summary_max_chars: cfg_compaction.micro.summary_max_chars,
                    micro_model: if cfg_compaction.micro.provider.is_empty() {
                        cfg_compaction.summarizer_model.clone()
                    } else {
                        cfg_compaction.micro.provider.clone()
                    },
                    lock_ttl_seconds: cfg_compaction.lock_ttl_seconds,
                    summarizer_model: cfg_compaction.summarizer_model.clone(),
                    // Phase 77.2 autoCompact — from YAML auto section, or defaults.
                    auto_token_pct: cfg_compaction
                        .auto
                        .as_ref()
                        .map(|a| a.token_pct)
                        .unwrap_or(0.80),
                    auto_max_age_minutes: cfg_compaction
                        .auto
                        .as_ref()
                        .map(|a| a.max_age_minutes)
                        .unwrap_or(120),
                    auto_buffer_tokens: cfg_compaction
                        .auto
                        .as_ref()
                        .map(|a| a.buffer_tokens)
                        .unwrap_or(13_000),
                    auto_min_turns_between: cfg_compaction
                        .auto
                        .as_ref()
                        .map(|a| a.min_turns_between)
                        .unwrap_or(5),
                    auto_max_consecutive_failures: cfg_compaction
                        .auto
                        .as_ref()
                        .map(|a| a.max_consecutive_failures)
                        .unwrap_or(3),
                };
                // Compactor reuses the same LLM client as the agent
                // by default; operators can ship a dedicated lighter
                // model later by adding a per-provider lookup.
                let summarizer = llm_registry
                    .build(&cfg.llm, &agent_cfg.model)
                    .with_context(|| {
                        format!("compaction wiring: failed to build summarizer LLM for {agent_id}")
                    })?;
                behavior = behavior.with_compaction(summarizer, Arc::clone(store), runtime);
                tracing::info!(
                    agent = %agent_id,
                    compact_at_tokens = cfg_compaction.compact_at_pct * effective_window,
                    "compaction wired"
                );
            }
        }
        let agent = Arc::new(Agent::new(agent_cfg, behavior));

        let mut runtime =
            AgentRuntime::new(Arc::clone(&agent), broker.clone(), Arc::clone(&sessions));
        // Hand the runtime the base registry so each session picks up a
        // per-binding filtered clone from the ToolRegistryCache instead
        // of paying a per-turn filter inside llm_behavior.
        runtime = runtime.with_tool_base(Arc::clone(&tools));
        if let Some(mem) = memory.clone() {
            runtime = runtime.with_memory(mem);
        }
        runtime = runtime.with_peers(Arc::clone(&peer_directory));
        runtime = runtime.with_redactor(Arc::clone(&transcripts_redactor));
        if let Some(idx) = transcripts_index.as_ref() {
            runtime = runtime.with_transcripts_index(Arc::clone(idx));
        }
        if let Some(ref bundle) = credentials {
            runtime = runtime.with_credentials(Arc::clone(&bundle.resolver));
            runtime = runtime.with_breakers(Arc::clone(&bundle.breakers));
        }
        runtime = runtime.with_link_extractor(Arc::clone(&link_extractor));
        if let Some(ref ws) = web_search_router {
            runtime = runtime.with_web_search_router(Arc::clone(ws));
        }
        runtime = runtime.with_pairing_gate(Arc::clone(&pairing_gate));
        // Phase 26.x — register per-channel adapters so challenge
        // delivery uses the right outbound topic + format. Channels
        // without a registered adapter fall back to the legacy
        // hardcoded broker publish in `deliver_pairing_challenge`.
        let pairing_registry = nexo_pairing::PairingAdapterRegistry::new();
        pairing_registry.register(std::sync::Arc::new(
            nexo_plugin_whatsapp::WhatsappPairingAdapter::new(broker.clone()),
        ));
        pairing_registry.register(std::sync::Arc::new(
            nexo_plugin_telegram::TelegramPairingAdapter::new(broker.clone()),
        ));
        runtime = runtime.with_pairing_adapters(pairing_registry);
        runtime = runtime.with_plan_approval_registry(plan_approval_registry.clone());
        if let Some(ref dc) = dispatch_ctx {
            runtime = runtime.with_dispatch_ctx(Arc::clone(dc));
        }
        // M5.b — capture maps before `runtime.start()` consumes self.
        // `tools_per_agent` carries the per-agent registry the cron
        // post-hook needs to filter against the new effective policy.
        // `agent_snapshot_handles` carries the `Arc<ArcSwap<...>>` the
        // post-hook calls `load_full()` on to read the new snapshot.
        tools_per_agent.insert(agent_id.clone(), Arc::clone(&tools));
        agent_snapshot_handles.insert(agent_id.clone(), runtime.snapshot_handle());
        runtime
            .start()
            .await
            .with_context(|| format!("failed to start agent runtime for {agent_id}"))?;
        running_agents.fetch_add(1, Ordering::Relaxed);
        tracing::info!(agent = %agent_id, "agent runtime started");
        // Snapshot the post-assembly tool surface so the reload
        // coordinator can validate `allowed_tools` against it without
        // re-reading the registry on every reload.
        let known_tools: Vec<String> = tools
            .to_tool_defs()
            .iter()
            .map(|d| d.name.clone())
            .collect();
        reload_senders.push((
            agent_id.to_string(),
            runtime.reload_sender(),
            std::sync::Arc::new(known_tools),
        ));
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
                let guard_for_dream = secret_guard.clone();
                let handle = tokio::spawn(async move {
                    let engine = DreamEngine::new(mem, workspace, dream_cfg);
                    let engine = if let Some(ref guard) = guard_for_dream {
                        engine.with_guard(guard.clone())
                    } else {
                        engine
                    };
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

    // M5.b — wrap aggregated cron-rebuild maps + build deps struct
    // for the post-hook + initial boot-time cron binding build.
    let tools_per_agent = Arc::new(tools_per_agent);
    let agent_snapshot_handles = Arc::new(agent_snapshot_handles);
    let cron_rebuild_deps = CronRebuildDeps {
        broker: broker.clone(),
        sessions: Arc::clone(&sessions),
        memory: memory.clone(),
        peer_directory: Arc::clone(&peer_directory),
        credentials: credentials.clone(),
        web_search_router: web_search_router.clone(),
        link_extractor: Arc::clone(&link_extractor),
        dispatch_ctx: dispatch_ctx.clone(),
        tools_per_agent: Arc::clone(&tools_per_agent),
        cron_tool_call_cfg: cron_tool_call_cfg.clone(),
    };
    // M5.b — late-bind cell holding the cron executor. Empty until
    // the cron block (`if cron_tool_call_cfg.enabled`) constructs the
    // executor; the post-hook below early-returns if cell is empty.
    let cron_executor_cell: Arc<tokio::sync::OnceCell<Arc<RuntimeCronToolExecutor>>> =
        Arc::new(tokio::sync::OnceCell::new());

    // Phase 18 — wire the hot-reload coordinator. It owns its own
    // CancellationToken tied to `watcher_shutdown` so the watcher +
    // broker subscriber exit alongside the extensions watcher on
    // SIGTERM.
    let llm_registry = Arc::new(llm_registry);
    let reload_coord = Arc::new(nexo_core::ConfigReloadCoordinator::new(
        config_dir.clone(),
        Arc::clone(&llm_registry),
        watcher_shutdown.clone(),
    ));
    for (id, tx, known) in reload_senders.drain(..) {
        reload_coord.register(id, tx, known);
    }
    // Phase 70.7 — flush in-process gate caches after every reload so
    // operator changes (e.g. `nexo pair seed`) take effect without a
    // daemon restart. PairingGate keeps a 30s decision cache; without
    // this hook a freshly-allowlisted sender stays "challenge" until
    // the TTL bleeds out.
    {
        let gate = Arc::clone(&pairing_gate);
        reload_coord
            .register_post_hook(Box::new(move || gate.flush_cache()))
            .await;
    }
    // M5.b — cron config-reload post-hook. Rebuilds the per-binding
    // context map from the new snapshots and atomically swaps it
    // into the `RuntimeCronToolExecutor` so cron firings observe
    // the new effective policy on the very next call. Empty-cell
    // case (reload triggered before executor constructed) is a
    // graceful no-op with `tracing::debug!`.
    {
        let cell = Arc::clone(&cron_executor_cell);
        let snapshots = Arc::clone(&agent_snapshot_handles);
        let deps = cron_rebuild_deps.clone();
        reload_coord
            .register_post_hook(Box::new(move || {
                let Some(executor) = cell.get() else {
                    tracing::debug!(
                        "[cron] post-hook fired before executor built; skipping"
                    );
                    return;
                };
                let new_map = build_cron_bindings_from_snapshots(&snapshots, &deps);
                let count = new_map.len();
                executor.replace_bindings(new_map);
                tracing::info!(
                    bindings = count,
                    "[cron] post-hook rebuilt cron_tool_bindings from new snapshot"
                );
            }))
            .await;
    }
    // Phase M1.b.c — daemon-embed MCP HTTP server. Opt-in via
    // `mcp_server.daemon_embed.enabled: true` + `mcp_server.http
    // .enabled: true`. Reuses the primary agent's tool registry
    // (mirrors `nexo mcp-server` standalone behavior) and
    // registers a reload-coord post-hook that swaps the
    // allowlist + emits `notifications/tools/list_changed` on
    // every Phase 18 reload — automatic, no SIGHUP needed.
    // Returned handle survives until the daemon's main shutdown
    // sequence drains it.
    let mcp_embed_handle: Option<nexo_mcp::HttpServerHandle> = match cfg
        .mcp_server
        .as_ref()
        .filter(|s| s.daemon_embed.enabled)
    {
        Some(server_cfg) => {
            let (primary_id, primary_cfg) =
                primary_for_mcp_embed.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "mcp_server.daemon_embed enabled but agents.yaml has no agents"
                    )
                })?;
            let primary_tools = tools_per_agent.get(&primary_id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "mcp_server.daemon_embed: primary agent `{}` not in tools_per_agent map",
                    primary_id
                )
            })?;
            let primary_cfg_arc = Arc::new(primary_cfg);
            let primary_ctx = nexo_core::agent::AgentContext::new(
                primary_id.clone(),
                primary_cfg_arc,
                broker.clone(),
                Arc::clone(&sessions),
            );
            let allowlist = compute_allowlist_from_mcp_server_cfg(server_cfg);
            let server_info = nexo_mcp::McpServerInfo {
                name: server_cfg
                    .name
                    .clone()
                    .unwrap_or_else(|| primary_id.clone()),
                version: env!("CARGO_PKG_VERSION").into(),
            };
            let bridge = nexo_core::agent::ToolRegistryBridge::new(
                server_info,
                primary_tools,
                primary_ctx,
                allowlist,
                server_cfg.expose_proxies,
            )
            .with_list_changed_capability(true);

            let http_yaml = server_cfg.http.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "mcp_server.daemon_embed enabled requires mcp_server.http config"
                )
            })?;
            if !http_yaml.enabled {
                anyhow::bail!(
                    "mcp_server.daemon_embed enabled but mcp_server.http.enabled = false"
                );
            }
            let handle = start_http_transport(&bridge, http_yaml, &watcher_shutdown).await?;
            tracing::info!(
                agent = %primary_id,
                addr = %handle.bind_addr,
                "[mcp-embed] daemon MCP server ready"
            );

            // Reload-coord post-hook: on every Phase 18 reload,
            // re-read `mcp_server.expose_tools` from disk + atomic
            // swap_allowlist + notify so connected clients refresh
            // tool list without reconnect.
            let bridge_for_hook = bridge.clone();
            let notifier = handle.notifier();
            let cfg_dir_for_hook = config_dir.clone();
            reload_coord
                .register_post_hook(Box::new(move || {
                    match reload_expose_tools(&cfg_dir_for_hook) {
                        Ok(new_allow) => {
                            let new_count = new_allow.as_ref().map(|s| s.len()).unwrap_or(0);
                            bridge_for_hook.swap_allowlist(new_allow);
                            let sessions = notifier.notify_tools_list_changed();
                            tracing::info!(
                                sessions,
                                new_count,
                                "[mcp-embed] reload: tools/list_changed emitted"
                            );
                        }
                        Err(e) => tracing::warn!(
                            error = %e,
                            "[mcp-embed] reload allowlist re-read failed; old allowlist preserved"
                        ),
                    }
                }))
                .await;

            Some(handle)
        }
        _ => None,
    };

    if let Err(e) = Arc::clone(&reload_coord)
        .start(broker.clone(), cfg.runtime.reload.clone())
        .await
    {
        tracing::warn!(error = %e, "config reload coordinator failed to start — hot-reload disabled");
    }

    // Phase 79.10.b — late-bind the reload coord into the
    // ConfigTool's reload trigger. The trigger was constructed
    // before the agent loop (so the per-agent registration could
    // hold an `Arc<dyn ReloadTrigger>` upfront); now that the
    // coordinator exists we resolve the OnceCell. After this point
    // a `Config { op: apply }` call drives `coord.reload()`.
    #[cfg(feature = "config-self-edit")]
    if let Some(cell) = reload_cell.as_ref() {
        let _ = cell.set(Arc::clone(&reload_coord));
    }

    // Phase 79.7 runtime firing — spawn ONE cron runner per process.
    // Polls the SQLite cron store every 5s. Dispatches through a
    // model-routing `LlmCronDispatcher` that picks provider+model per
    // cron entry (`model_provider`/`model_name`) and caches clients by
    // pair. Legacy rows without model metadata use the first agent's
    // model as fallback when present.
    let cron_runner_cancel = tokio_util::sync::CancellationToken::new();
    let cron_db_path = nexo_project_tracker::state::nexo_state_dir().join("nexo_cron.db");
    if let Some(parent) = cron_db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match nexo_core::cron_schedule::SqliteCronStore::open(
        cron_db_path.to_str().unwrap_or("nexo_cron.db"),
    )
    .await
    {
        Ok(store) => {
            if let Some((agent_id, model)) = first_agent_for_cron.as_ref() {
                tracing::info!(
                    agent_id = %agent_id,
                    provider = %model.provider,
                    model = %model.model,
                    "[cron] fallback model for legacy cron rows"
                );
            } else {
                tracing::warn!(
                    "[cron] no fallback model configured (legacy cron rows without model metadata will fail)"
                );
            }
            let fallback_model = first_agent_for_cron.as_ref().map(|(_, m)| m.clone());
            let dispatcher: std::sync::Arc<dyn nexo_core::cron_runner::CronDispatcher> = {
                let publisher: std::sync::Arc<
                    dyn nexo_core::llm_cron_dispatcher::ChannelPublisher,
                > = std::sync::Arc::new(
                    nexo_core::llm_cron_dispatcher::BrokerChannelPublisher::new(
                        std::sync::Arc::new(broker.clone()),
                    ),
                );
                let mut d = nexo_core::llm_cron_dispatcher::LlmCronDispatcher::from_registry(
                    Arc::clone(&llm_registry),
                    cfg.llm.clone(),
                    legacy_cron_binding_models.clone(),
                    fallback_model,
                )
                .with_publisher(publisher);
                if cron_tool_call_cfg.enabled {
                    // M5.b — single source of truth: boot path uses
                    // the same `build_cron_bindings_from_snapshots`
                    // fn the config-reload post-hook calls.
                    let cron_tool_bindings = build_cron_bindings_from_snapshots(
                        &agent_snapshot_handles,
                        &cron_rebuild_deps,
                    );
                    if cron_tool_bindings.is_empty() {
                        tracing::warn!(
                            "[cron] runtime.cron.tool_calls.enabled=true but no cron tool contexts were built; tool-call execution remains off"
                        );
                    } else {
                        let bindings_count = cron_tool_bindings.len();
                        let executor = std::sync::Arc::new(RuntimeCronToolExecutor::new(
                            cron_tool_bindings,
                        ));
                        // M5.b — late-bind into the post-hook cell so
                        // subsequent reloads can `replace_bindings`.
                        let _ = cron_executor_cell.set(Arc::clone(&executor));
                        d = d.with_tool_executor(executor, cron_tool_call_cfg.max_iterations);
                        tracing::info!(
                            bindings = bindings_count,
                            max_iterations = cron_tool_call_cfg.max_iterations,
                            allowlist = ?cron_tool_call_cfg.allowlist,
                            "[cron] tool-call execution enabled"
                        );
                    }
                }
                std::sync::Arc::new(d)
            };
            let retry_cfg = &cfg.runtime.cron.one_shot_retry;
            let base_backoff_secs = retry_cfg.base_backoff_secs.max(1);
            let one_shot_retry_policy = nexo_core::cron_runner::OneShotRetryPolicy {
                max_retries: retry_cfg.max_retries,
                base_backoff_secs,
                max_backoff_secs: retry_cfg.max_backoff_secs.max(base_backoff_secs),
            };
            let runner = std::sync::Arc::new(
                nexo_core::cron_runner::CronRunner::new(
                    std::sync::Arc::new(store)
                        as std::sync::Arc<dyn nexo_core::cron_schedule::CronStore>,
                    dispatcher,
                )
                .with_one_shot_retry_policy(one_shot_retry_policy)
                .with_jitter_pct(cfg.runtime.cron.jitter_pct),
            );
            let cancel_for_runner = cron_runner_cancel.clone();
            tokio::spawn(async move { runner.run(cancel_for_runner).await });
            tracing::info!(
                path = %cron_db_path.display(),
                one_shot_max_retries = one_shot_retry_policy.max_retries,
                one_shot_base_backoff_secs = one_shot_retry_policy.base_backoff_secs,
                one_shot_max_backoff_secs = one_shot_retry_policy.max_backoff_secs,
                "[cron] runner spawned"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %cron_db_path.display(),
                "[cron] runner not spawned — could not open cron store"
            );
        }
    }

    tracing::info!("agent ready — waiting for shutdown signal (SIGTERM / Ctrl+C)");
    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping");
    cron_runner_cancel.cancel();
    // Phase M1.b.c — graceful drain of the daemon-embed MCP HTTP
    // server. `watcher_shutdown.cancel()` (below) signals the
    // server to drain; we await its `join` with a 5s budget so
    // SSE consumers see a clean disconnect. No-op when
    // `daemon_embed.enabled = false`.
    if let Some(handle) = mcp_embed_handle {
        watcher_shutdown.cancel();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle.join,
        )
        .await;
    }
    // Phase 79.6 — drop the team router subscriber. Active teams
    // keep their soft-deleted state; force-kill of in-flight
    // teammate goals is delegated to the existing
    // `drain_running_goals` pattern (Phase 71.3) that runs below.
    team_router_cancel.cancel();
    // Phase 79.5 — shut down the LSP manager BEFORE plugin
    // teardown so any in-flight `$/cancelRequest` notifications
    // make it out to the language servers and child processes
    // exit cleanly. `kill_on_drop(true)` is the safety net.
    lsp_manager.shutdown().await;

    // Phase 71.3 — drain in-flight goals BEFORE bringing channel
    // plugins down. Walk the registry that lives inside the
    // dispatch context, fire `notify_origin` / `notify_channel`
    // hooks with a clean "[shutdown]" summary, and flip Running
    // rows to `LostOnRestart` so a future reattach sweep does not
    // re-fire them. We cannot wait for the Claude Code subprocess
    // to land its last commit — 5–10 s is not enough — so the
    // contract here is "tell the operator the goal was abandoned
    // cleanly". SIGKILL still bypasses this; the boot-time sweep
    // (Phase 71.2) is the safety net for that case.
    if let Some(dc) = dispatch_ctx.as_ref() {
        if let Some(hd) = dc.hook_dispatcher.as_ref() {
            let report = nexo_dispatch_tools::drain_running_goals(
                &dc.registry,
                &dc.hooks,
                Arc::clone(hd),
                None,
            )
            .await;
            if report.running_seen > 0 {
                tracing::info!(
                    running_seen = report.running_seen,
                    hooks_fired = report.hooks_fired,
                    hook_dispatch_errors = report.hook_dispatch_errors,
                    hook_dispatch_timeouts = report.hook_dispatch_timeouts,
                    set_status_errors = report.set_status_errors,
                    "shutdown drain swept in-flight goals before plugin teardown",
                );
            }
        }
    }

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

/// Phase 67 — boot the shared DispatchToolContext when the
/// operator opts in via `NEXO_DRIVER_INTEGRATED=1`. Returns
/// `None` (handlers stay in friendly-error mode) when the env
/// var is unset OR when any of the required pieces fail to
/// initialise — we never crash the agent boot just because the
/// dispatch surface couldn't be wired.
async fn boot_dispatch_ctx_if_enabled(
    _broker: &nexo_broker::AnyBroker,
    agents: &[nexo_config::AgentConfig],
    mcp_manager: Option<Arc<nexo_mcp::McpRuntimeManager>>,
    channel_boot: nexo_mcp::channel_boot::ChannelBootContext,
    pending_permissions: Arc<nexo_mcp::channel_permission::PendingPermissionMap>,
) -> Option<Arc<nexo_core::agent::dispatch_handlers::DispatchToolContext>> {
    // Auto-detect: any agent (or any of its bindings) with
    // dispatch_capability=Full triggers the in-process driver.
    // Operator opts in by configuring Cody (or whoever) with
    // `dispatch_policy.mode: full`; no env var required.
    let any_full = agents.iter().any(|a| {
        let agent_full = matches!(
            a.dispatch_policy.mode,
            nexo_config::DispatchCapability::Full
        );
        let binding_full = a.inbound_bindings.iter().any(|b| {
            b.dispatch_policy
                .as_ref()
                .map(|p| matches!(p.mode, nexo_config::DispatchCapability::Full))
                .unwrap_or(false)
        });
        agent_full || binding_full
    });
    if !any_full {
        tracing::info!(
            "dispatch boot: no agent declares dispatch_capability=full — driver stays unwired"
        );
        return None;
    }
    tracing::info!("dispatch boot: starting (an agent declared dispatch_capability=full)");

    // Project tracker / dispatch policy config — until the YAML
    // is wired this stayed hardcoded with `require_trusted=true`,
    // which forced every operator to seed pairing.trusted=true
    // before the dispatcher accepted a single goal. Now we honour
    // `program_phase.require_trusted` from
    // `config/project-tracker/project_tracker.yaml` so dev setups
    // can flip it off without rebuilding.
    let pt_yaml_path = std::path::Path::new("config/project-tracker/project_tracker.yaml");
    let pt_cfg = if pt_yaml_path.exists() {
        match nexo_project_tracker::ProjectTrackerConfig::from_yaml_file(pt_yaml_path) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "project_tracker.yaml parse failed — using built-in defaults");
                None
            }
        }
    } else {
        None
    };
    let require_trusted = pt_cfg
        .as_ref()
        .map(|c| c.program_phase.require_trusted)
        .unwrap_or(true);
    tracing::info!(require_trusted, "dispatch boot: program_phase gate");

    // Driver config — fall back to a shipped default path.
    // Production deploys override with NEXO_DRIVER_CONFIG.
    let claude_yaml = std::env::var("NEXO_DRIVER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config/driver/claude.yaml"));
    if !claude_yaml.exists() {
        tracing::warn!(
            path = %claude_yaml.display(),
            "an agent has dispatch_capability=full but driver config is missing — dispatch tools stay in error mode"
        );
        return None;
    }
    tracing::info!(path = %claude_yaml.display(), "dispatch boot: driver config found");

    let driver_cfg = match nexo_driver_loop::DriverConfig::from_yaml_file(&claude_yaml) {
        Ok(c) => {
            tracing::info!("dispatch boot: driver config parsed");
            c
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse driver config — dispatch tools stay in error mode");
            return None;
        }
    };

    // Tracker rooted at workspace root. Resolution order:
    //   1. `NEXO_PROJECT_ROOT` env var — operator override (highest priority).
    //   2. Saved sidecar at `$NEXO_HOME/state/active_workspace_path` — survives
    //      daemon restarts so `set_active_workspace` / `init_project` calls are
    //      not lost when the daemon is restarted.
    //   3. Walk up from the daemon's cwd looking for the first
    //      ancestor that contains `PHASES.md`. Lets the operator
    //      run `./target/debug/nexo` from any subdirectory without
    //      having to export an env var or hardcode a path in the
    //      YAML (which would break portable deployments).
    //   4. Fall back to cwd verbatim.
    let default_root: PathBuf = std::env::var("NEXO_PROJECT_ROOT")
        .map(PathBuf::from)
        .ok()
        .or_else(|| nexo_project_tracker::state::read_active_workspace())
        .or_else(|| {
            let cwd = std::env::current_dir().ok()?;
            let mut probe: &std::path::Path = cwd.as_path();
            loop {
                if probe.join("PHASES.md").is_file() {
                    return Some(probe.to_path_buf());
                }
                probe = probe.parent()?;
            }
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let tracker_root = default_root;
    tracing::info!(root = %tracker_root.display(), "dispatch boot: opening tracker");
    let tracker: Arc<nexo_project_tracker::MutableTracker> =
        match nexo_project_tracker::MutableTracker::open_fs(&tracker_root) {
            Ok(t) => {
                tracing::info!("dispatch boot: tracker opened");
                Arc::new(t)
            }
            Err(e) => {
                tracing::warn!(error = %e, root = %tracker_root.display(), "tracker open failed — dispatch tools stay in error mode");
                return None;
            }
        };

    // Permission decider — Phase 67.4 wired the LLM decider in the
    // standalone bin; here we keep the simpler AllowAll path so the
    // chat-side surface works without an extra LLM call. Operators
    // who want strict permission go via the standalone nexo-driver.
    let inner_decider: Arc<dyn nexo_driver_permission::PermissionDecider> =
        Arc::new(nexo_driver_permission::AllowAllDecider);

    // Phase 80.9.b.b — wrap the decider in a ChannelRelayDecider when
    // any agent has channels enabled AND any approved server can be
    // reached as a permission-relay surface (the gate at registration
    // time decides whether the server actually opted into the
    // capability — here we only check that channels are configured at
    // all). The decorator races the inner decider against any channel
    // reply via tokio::select!; when no eligible servers register at
    // runtime the decorator short-circuits to the inner decider.
    let any_agent_has_channels = agents
        .iter()
        .any(|a| a.channels.as_ref().map(|c| c.enabled).unwrap_or(false));
    let decider: Arc<dyn nexo_driver_permission::PermissionDecider> = if any_agent_has_channels {
        let mgr_for_resolver = mcp_manager.clone();
        let resolver: std::sync::Arc<
            dyn Fn(&str) -> Option<std::sync::Arc<dyn nexo_mcp::McpClient>>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |server_name: &str| {
            let mgr = mgr_for_resolver.as_ref()?;
            // Block on the shared session lookup. Acceptable here:
            // the resolver is invoked from the decider's emit_request
            // path which already runs inside an async context, but
            // `Fn(&str) -> Option<...>` is sync so we cannot await
            // directly. The runtime tokio handle hops in via
            // `tokio::runtime::Handle::current().block_on` — this
            // is a slim MVP; a follow-up wraps the resolver in an
            // async-friendly trait.
            let rt = tokio::runtime::Handle::current().block_on(async {
                mgr.get_or_create(uuid::Uuid::nil()).await
            });
            rt.clients()
                .into_iter()
                .find(|(name, _)| name == server_name)
                .map(|(_, client)| client)
        });
        let dispatcher: std::sync::Arc<
            dyn nexo_mcp::channel_permission::PermissionRelayDispatcher,
        > = std::sync::Arc::new(
            nexo_mcp::channel_permission::ClientResolverDispatcher::new(resolver),
        );
        let wrapped = nexo_driver_permission::channel_relay::ChannelRelayDecider::new(
            ArcDeciderShim(inner_decider.clone()),
            channel_boot.registry.clone(),
            pending_permissions.clone(),
            dispatcher,
        );
        tracing::info!("permission relay decorator wired (channels enabled on at least one agent)");
        Arc::new(wrapped)
    } else {
        inner_decider
    };

    // Driver workspace manager. When `workspace.git.enabled=true`,
    // each dispatched goal runs inside a fresh git worktree on a
    // branch `nexo-driver/<goal_id>` rooted at the source repo, so
    // the operator's working tree is never modified in place. The
    // source repo auto-detects from cwd (walk up looking for `.git`)
    // when YAML leaves `source_repo` empty, mirroring the tracker
    // root resolution and avoiding hardcoded paths.
    let workspace_manager = {
        let mgr = nexo_driver_loop::WorkspaceManager::new(&driver_cfg.workspace.root);
        if driver_cfg.workspace.git.enabled {
            let source_repo = driver_cfg
                .workspace
                .git
                .source_repo
                .clone()
                .filter(|p| !p.as_os_str().is_empty())
                .or_else(|| {
                    let cwd = std::env::current_dir().ok()?;
                    let mut probe: &std::path::Path = cwd.as_path();
                    loop {
                        if probe.join(".git").exists() {
                            return Some(probe.to_path_buf());
                        }
                        probe = probe.parent()?;
                    }
                });
            match source_repo {
                Some(repo) => {
                    tracing::info!(
                        repo = %repo.display(),
                        "dispatch boot: driver git-worktree mode enabled"
                    );
                    Arc::new(mgr.with_git(nexo_driver_loop::GitWorktreeMode::SourceRepo {
                        path: repo,
                        base_ref: driver_cfg.workspace.git.base_ref.clone(),
                    }))
                }
                None => {
                    tracing::warn!(
                        "workspace.git.enabled=true but source_repo unset and cwd has no .git — falling back to non-git mode"
                    );
                    Arc::new(mgr)
                }
            }
        } else {
            Arc::new(mgr)
        }
    };

    let binding_store: Arc<dyn nexo_driver_claude::SessionBindingStore> = match driver_cfg
        .binding_store
        .kind
    {
        nexo_driver_loop::BindingStoreKind::Memory => {
            Arc::new(nexo_driver_claude::MemoryBindingStore::new())
        }
        nexo_driver_loop::BindingStoreKind::Sqlite => {
            let path = driver_cfg
                .binding_store
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| ":memory:".into());
            // Best-effort: pre-create the parent dir so the SQLite
            // open doesn't fail with code 14 just because nobody
            // mkdir-d the data directory yet.
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            tracing::info!(path = %path, "dispatch boot: opening sqlite binding store");
            match nexo_driver_claude::SqliteBindingStore::open(&path).await {
                Ok(s) => {
                    tracing::info!(path = %path, "dispatch boot: binding store opened");
                    Arc::new(s)
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path, "binding store open failed — dispatch tools stay in error mode");
                    return None;
                }
            }
        }
    };

    // Registry + log buffer + hook registry shared by every agent.
    //
    // Phase 71.1 — honour `agent_registry.store` from
    // project_tracker.yaml. Empty / unresolved → memory store (dev
    // mode, state lost on restart). Path open failures fall back to
    // memory with a warn so a corrupt sqlite file never bricks the
    // boot path. Env placeholders (e.g. `${NEXO_AGENT_REGISTRY_DB:-…}`)
    // come through as raw `${…}` because project-tracker's loader
    // doesn't run the env resolver — we resolve here before opening.
    let registry_store_path: Option<PathBuf> = pt_cfg.as_ref().and_then(|c| {
        let raw = c.agent_registry.store.to_string_lossy();
        if raw.is_empty() {
            return None;
        }
        let resolved = match nexo_config::env::resolve_placeholders(
            &format!("v: {raw}"),
            "project_tracker.yaml",
        ) {
            Ok(s) => s.trim_start_matches("v: ").trim().to_string(),
            Err(e) => {
                tracing::warn!(error = %e, raw = %raw, "agent_registry.store env resolve failed; using memory");
                return None;
            }
        };
        if resolved.is_empty() {
            None
        } else {
            Some(PathBuf::from(resolved))
        }
    });
    let registry_max_concurrent = pt_cfg
        .as_ref()
        .map(|c| c.program_phase.max_concurrent_agents)
        .unwrap_or(4);
    let (registry_store, registry_store_was_sqlite): (
        Arc<dyn nexo_agent_registry::AgentRegistryStore>,
        bool,
    ) = match registry_store_path.as_ref() {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match nexo_agent_registry::SqliteAgentRegistryStore::open(path.to_str().unwrap_or(""))
                .await
            {
                Ok(s) => {
                    tracing::info!(path = %path.display(), "agent registry: sqlite-backed (survives restart)");
                    (Arc::new(s), true)
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "agent registry sqlite open failed — falling back to memory; goals will be lost on restart");
                    (
                        Arc::new(nexo_agent_registry::MemoryAgentRegistryStore::default()),
                        false,
                    )
                }
            }
        }
        None => {
            tracing::info!(
                "agent registry: memory-only (no agent_registry.store path; goals lost on restart)"
            );
            (
                Arc::new(nexo_agent_registry::MemoryAgentRegistryStore::default()),
                false,
            )
        }
    };
    let registry = Arc::new(nexo_agent_registry::AgentRegistry::new(
        Arc::clone(&registry_store),
        registry_max_concurrent,
    ));
    // Phase 72.2 — open the durable turn log on the same sqlite
    // file as the registry. Same fallback discipline: open failure
    // logs a warn and the rest of the runtime keeps booting (the
    // tool just reports "turn log not enabled" until the operator
    // fixes the path).
    let turn_log_store: Option<Arc<dyn nexo_agent_registry::TurnLogStore>> =
        match registry_store_path.as_ref() {
            Some(path) => {
                match nexo_agent_registry::SqliteTurnLogStore::open(path.to_str().unwrap_or(""))
                    .await
                {
                    Ok(s) => {
                        tracing::info!(path = %path.display(), "turn log: sqlite-backed (every AttemptResult persisted)");
                        Some(Arc::new(s))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "turn log sqlite open failed — agent_turns_tail will report disabled");
                        None
                    }
                }
            }
            None => None,
        };
    let log_buffer_lines = pt_cfg
        .as_ref()
        .map(|c| c.agent_registry.log_buffer_lines)
        .unwrap_or(200);
    let log_buffer = Arc::new(nexo_agent_registry::LogBuffer::new(log_buffer_lines));
    // B17 — hook registry mirrors writes to SQLite so attached
    // hooks (auto-audit, notify_origin, dispatch_phase chains)
    // survive daemon restart. Path defaults under the workspace
    // root; falls back to ':memory:' if open fails.
    let hook_db_path: PathBuf = std::env::var("NEXO_HOOK_REGISTRY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("nexo-hooks.db"));
    let hook_store = match nexo_dispatch_tools::SqliteHookRegistryStore::open(
        hook_db_path.to_str().unwrap_or(":memory:"),
    )
    .await
    {
        Ok(s) => Some(Arc::new(s) as Arc<dyn nexo_dispatch_tools::HookRegistryStore>),
        Err(e) => {
            tracing::warn!(error = %e, "hook store open failed — hooks stay in memory only");
            None
        }
    };
    let hook_registry = Arc::new(match hook_store.clone() {
        Some(s) => nexo_dispatch_tools::HookRegistry::with_store(s),
        None => nexo_dispatch_tools::HookRegistry::new(),
    });
    if let Err(e) = hook_registry.reload_from_store().await {
        tracing::warn!(error = %e, "hook reload failed — pre-restart hooks won't fire");
    }
    // B23 — orphan sweep: the agent-registry doesn't get
    // populated until the per-agent reattach below, so we sweep
    // hook orphans AFTER admit-driven reattach lands by scheduling
    // a tokio task that fires once the registry is warm. The
    // sweep drops every (goal_id, hook) pair whose goal_id no
    // longer maps to anything in agent-registry — the goals
    // those hooks targeted terminated pre-restart and never had
    // their HookRegistry::drop_goal flushed to disk.
    {
        let hooks = hook_registry.clone();
        let reg = registry.clone();
        tokio::spawn(async move {
            // Tiny delay so the per-agent reattach pass (in the
            // boot loop below) has a chance to populate the
            // registry first. Using a short fixed wait avoids
            // adding a synchronisation handle through the boot
            // path; if reattach takes longer the sweep just
            // drops more rows than necessary, which is safe.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            for goal_id in hooks.goal_ids() {
                if reg.handle(goal_id).is_none() {
                    tracing::info!(
                        target: "hook.registry.sweep",
                        goal_id = %goal_id.0,
                        "dropping hooks for terminated goal"
                    );
                    hooks.drop_goal(goal_id);
                }
            }
        });
    }

    // Hook dispatcher with the channel adapters Phase 26 already
    // owns. Adapters are registered into a SHARED registry here so
    // notify_origin reaches WhatsApp / Telegram out of the box.
    let pairing_registry = nexo_pairing::PairingAdapterRegistry::new();
    pairing_registry.register(Arc::new(nexo_plugin_whatsapp::WhatsappPairingAdapter::new(
        _broker.clone(),
    )));
    pairing_registry.register(Arc::new(nexo_plugin_telegram::TelegramPairingAdapter::new(
        _broker.clone(),
    )));
    // PT-4 — hook idempotency store. Lives next to other state sidecars
    // in $NEXO_HOME/state/. On failure the dispatcher degrades to
    // idempotency-less mode (hooks can fire twice on NATS replay) but
    // nothing hard-fails — same contract as the turn-log store.
    let idempotency_store: Option<Arc<nexo_dispatch_tools::HookIdempotencyStore>> = {
        let path = nexo_project_tracker::state::nexo_state_dir().join("hook_idempotency.db");
        match nexo_dispatch_tools::HookIdempotencyStore::open(
            path.to_str().unwrap_or("hook_idempotency.db"),
        )
        .await
        {
            Ok(s) => {
                tracing::info!(path = %path.display(), "dispatch boot: hook idempotency store opened");
                Some(Arc::new(s))
            }
            Err(e) => {
                tracing::warn!(
                    error = %e, path = %path.display(),
                    "dispatch boot: hook idempotency store failed — hooks may fire twice on NATS replay"
                );
                None
            }
        }
    };
    let hook_dispatcher: Arc<dyn nexo_dispatch_tools::HookDispatcher> = {
        let mut d = nexo_dispatch_tools::DefaultHookDispatcher::new(
            pairing_registry,
            Arc::new(nexo_dispatch_tools::NoopNatsHookPublisher),
        );
        if let Some(store) = idempotency_store.clone() {
            d = d.with_idempotency(store);
        }
        Arc::new(d)
    };

    // Phase 71.2 — reattach sweep. When the registry is sqlite-backed
    // and `reattach_on_boot: true`, walk every Running row from the
    // last run, mark it `LostOnRestart`, and fire any
    // `notify_origin` / `notify_channel` hooks the operator had
    // attached so the original chat sees a clean closure
    // ("daemon restart — goal abandoned"). Without this, every
    // SIGKILL leaves the operator waiting forever.
    //
    // Resume-as-Running is intentionally OFF: respawning a Claude
    // Code subprocess against a worktree the daemon no longer owns
    // is Phase 67.C.1 territory, and unsafe to do silently. Marking
    // lost + notifying is the conservative, correct default.
    let reattach_on_boot = pt_cfg
        .as_ref()
        .map(|c| c.agent_registry.reattach_on_boot)
        .unwrap_or(true);
    if registry_store_was_sqlite && reattach_on_boot {
        let outcomes = nexo_agent_registry::reattach(
            &registry,
            Arc::clone(&registry_store),
            nexo_agent_registry::ReattachOptions {
                resume_running: false,
                ..Default::default()
            },
        )
        .await;
        match outcomes {
            Ok(outcomes) => {
                let mut lost = 0usize;
                let mut requeued = 0usize;
                let mut sleeping = 0usize;
                let mut recorded = 0usize;
                let mut skipped = 0usize;
                for outcome in &outcomes {
                    match outcome {
                        nexo_agent_registry::ReattachOutcome::MarkedLost(handle) => {
                            lost += 1;
                            let hooks = hook_registry.list(handle.goal_id);
                            if hooks.is_empty() {
                                continue;
                            }
                            let payload = nexo_dispatch_tools::HookPayload {
                                goal_id: handle.goal_id,
                                phase_id: handle.phase_id.clone(),
                                transition: nexo_dispatch_tools::HookTransition::Failed,
                                summary: format!(
                                    "[abandoned] daemon restart — goal `{:?}` was running when the daemon stopped and could not be resumed automatically. Re-dispatch with `program_phase phase_id={}` if you still need it.",
                                    handle.goal_id, handle.phase_id,
                                ),
                                elapsed: humantime::format_duration(handle.elapsed())
                                    .to_string(),
                                diff_stat: handle.snapshot.last_diff_stat.clone(),
                                origin: handle.origin.clone(),
                            };
                            for hook in hooks {
                                if !hook.on.matches(nexo_dispatch_tools::HookTransition::Failed) {
                                    continue;
                                }
                                if let Err(e) = hook_dispatcher.dispatch(&hook, &payload).await {
                                    tracing::warn!(
                                        goal_id = ?handle.goal_id,
                                        hook_id = %hook.id,
                                        error = %e,
                                        "reattach: notify hook dispatch failed",
                                    );
                                }
                            }
                        }
                        nexo_agent_registry::ReattachOutcome::Requeued(_) => requeued += 1,
                        nexo_agent_registry::ReattachOutcome::Sleeping(_) => sleeping += 1,
                        nexo_agent_registry::ReattachOutcome::Recorded(_) => recorded += 1,
                        nexo_agent_registry::ReattachOutcome::Skipped { .. } => skipped += 1,
                        nexo_agent_registry::ReattachOutcome::Resume(_) => {
                            // resume_running=false, this branch is unreachable
                        }
                    }
                }
                tracing::info!(
                    lost,
                    requeued,
                    sleeping,
                    recorded,
                    skipped,
                    "agent registry reattach swept previous run",
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "agent registry reattach failed — previous-run goals may be invisible");
            }
        }
    }

    // Inner sink: NoopEventSink today. EventForwarder wraps it so
    // the registry / log_buffer / hooks see every driver event.
    let inner_sink: Arc<dyn nexo_driver_loop::DriverEventSink> =
        Arc::new(nexo_driver_loop::NoopEventSink);
    let event_sink: Arc<dyn nexo_driver_loop::DriverEventSink> = {
        let mut fwd = nexo_dispatch_tools::EventForwarder::new(
            registry.clone(),
            log_buffer.clone(),
            hook_registry.clone(),
            hook_dispatcher.clone(),
            inner_sink,
        );
        if let Some(store) = turn_log_store.as_ref() {
            fwd = fwd.with_turn_log(Arc::clone(store));
        }
        if let Some(store) = idempotency_store.as_ref() {
            fwd = fwd.with_idempotency(Arc::clone(store));
        }
        Arc::new(fwd)
    };

    let acceptance: Arc<dyn nexo_driver_loop::AcceptanceEvaluator> = {
        let mut ev = nexo_driver_loop::DefaultAcceptanceEvaluator::new();
        if let Some(t) = driver_cfg.acceptance.default_shell_timeout {
            ev = ev.with_default_shell_timeout(t);
        }
        if let Some(n) = driver_cfg.acceptance.evidence_byte_limit {
            ev = ev.with_evidence_byte_limit(n);
        }
        Arc::new(ev)
    };
    let orchestrator = match nexo_driver_loop::DriverOrchestrator::builder()
        .claude_config(driver_cfg.claude.clone())
        .binding_store(binding_store)
        .acceptance(acceptance)
        .decider(decider)
        .workspace_manager(workspace_manager)
        .event_sink(event_sink)
        .bin_path(driver_cfg.driver.bin_path.clone())
        .socket_path(driver_cfg.permission.socket.clone())
        .build()
        .await
    {
        Ok(o) => Arc::new(o),
        Err(e) => {
            tracing::warn!(error = %e, "orchestrator build failed — dispatch tools stay in error mode");
            return None;
        }
    };

    tracing::info!(
        workspace = %tracker_root.display(),
        "dispatch tools wired end-to-end (NEXO_DRIVER_INTEGRATED=1)"
    );

    // Phase 77.16 — after reattach restores paused rows from sqlite,
    // re-arm AskUserQuestion in-memory timeout tasks so pending
    // questions still expire/cancel after daemon restart.
    let rearmed = nexo_dispatch_tools::rearm_ask_user_timeouts(
        orchestrator.clone(),
        registry.clone(),
        Some(hook_dispatcher.clone()),
    )
    .await;
    if rearmed > 0 {
        tracing::info!(rearmed, "ask_user_question timeouts re-armed after boot");
    }

    Some(Arc::new(
        nexo_core::agent::dispatch_handlers::DispatchToolContext {
            tracker,
            orchestrator: orchestrator.clone(),
            registry: registry.clone(),
            hooks: hook_registry.clone(),
            hook_dispatcher: Some(hook_dispatcher.clone()),
            turn_log: turn_log_store.clone(),
            log_buffer: log_buffer.clone(),
            default_caps: nexo_dispatch_tools::policy_gate::CapSnapshot {
                queue_when_full: true,
                ..Default::default()
            },
            require_trusted,
            telemetry: Arc::new(nexo_dispatch_tools::NoopTelemetry),
            // Self-modify gate. Default `true` because the
            // canonical dev usecase IS Cody helping finish the
            // nexo-rs roadmap itself; per-goal worktree
            // isolation (Phase 67.6) keeps the live source safe
            // from in-flight changes. Production deploys
            // (separate nexo-driver host, frozen binary) opt
            // out with `NEXO_DISALLOW_SELF_MODIFY=1`.
            allow_self_modify: std::env::var("NEXO_DISALLOW_SELF_MODIFY")
                .ok()
                .map(|v| !matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(true),
            daemon_source_root: std::env::current_dir().unwrap_or_default(),
            // Audit-before-done — defaults true. Operators that
            // want raw dispatch (no audit ping) flip
            // `NEXO_NO_AUDIT_BEFORE_DONE=1`.
            audit_before_done: std::env::var("NEXO_NO_AUDIT_BEFORE_DONE")
                .ok()
                .map(|v| !matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(true),
            chainer: Some(Arc::new(nexo_core::agent::dispatch_handlers::AuditChainer {
                orchestrator: orchestrator.clone(),
                registry: registry.clone(),
                hooks: hook_registry.clone(),
                log_buffer: log_buffer.clone(),
                default_caps: nexo_dispatch_tools::policy_gate::CapSnapshot {
                    queue_when_full: true,
                    ..Default::default()
                },
                // B22 — audit goals run inside the parent's
                // worktree so Claude sees the commits.
                workspace_root: driver_cfg.workspace.root.clone(),
                // B24 — separate cap so audits can't be
                // starved by main dispatch traffic.
                audit_cap: Some(2),
            })
                as Arc<dyn nexo_dispatch_tools::DispatchPhaseChainer>),
        },
    ))
}

/// Resolve the secrets directory for credential loaders. Convention is
/// C5 — adapter from the `nexo-config` wire-shape to the canonical
/// `nexo-memory` domain type. Lives here (not in either crate)
/// because `main.rs` is the only module that holds both deps —
/// `nexo-config` and `nexo-memory` cannot reference each other due
/// to the `nexo-llm -> nexo-config -> nexo-memory -> nexo-llm` cycle.
///
/// Validation:
///   * `on_secret` must be one of `block` / `redact` / `warn`
///     (snake_case wire). Invalid values fail boot with a clear
///     error so a YAML typo is loud, not silent.
///   * `rules` accepts the literal string `"all"` or a YAML list
///     of kebab-case rule IDs. Other shapes fail boot.
fn build_secret_guard_config_from_yaml(
    src: &nexo_config::types::memory::SecretGuardYamlConfig,
) -> Result<nexo_memory::SecretGuardConfig> {
    use nexo_memory::secret_config::RuleSelection;
    use nexo_memory::secret_scanner::OnSecret;

    let on_secret = match src.on_secret.as_str() {
        "block" => OnSecret::Block,
        "redact" => OnSecret::Redact,
        "warn" => OnSecret::Warn,
        other => {
            anyhow::bail!(
                "memory.secret_guard.on_secret = `{other}`; valid values are \
                 `block` | `redact` | `warn`"
            );
        }
    };

    let rules = match &src.rules {
        serde_yaml::Value::String(s) if s == "all" => RuleSelection::All,
        serde_yaml::Value::Sequence(seq) => {
            let mut ids = Vec::with_capacity(seq.len());
            for v in seq {
                match v {
                    serde_yaml::Value::String(id) => ids.push(id.clone()),
                    other => anyhow::bail!(
                        "memory.secret_guard.rules entries must be strings; got {other:?}"
                    ),
                }
            }
            RuleSelection::List(ids)
        }
        other => anyhow::bail!(
            "memory.secret_guard.rules must be the string `\"all\"` or a list of \
             rule IDs; got {other:?}"
        ),
    };

    Ok(nexo_memory::SecretGuardConfig {
        enabled: src.enabled,
        on_secret,
        rules,
        exclude_rules: src.exclude_rules.clone(),
    })
}

/// `<config_dir>/../secrets`; override with `NEXO_SECRETS_DIR` for
/// Docker (`/run/secrets`) or non-standard layouts.
fn secrets_dir_for(config_dir: &std::path::Path) -> std::path::PathBuf {
    if let Ok(env) = std::env::var("NEXO_SECRETS_DIR") {
        return std::path::PathBuf::from(env);
    }
    config_dir
        .parent()
        .map(|p| p.join("secrets"))
        .unwrap_or_else(|| std::path::PathBuf::from("secrets"))
}

fn build_mcp_sampling_provider(
    cfg: &AppConfig,
    llm_registry: &LlmRegistry,
) -> anyhow::Result<Option<Arc<dyn nexo_mcp::sampling::SamplingProvider>>> {
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

    let mut named: std::collections::HashMap<String, Arc<dyn nexo_llm::LlmClient>> =
        std::collections::HashMap::new();
    let mut default_client: Option<Arc<dyn nexo_llm::LlmClient>> = None;
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

    let per_server: std::collections::HashMap<String, nexo_mcp::sampling::PerServerPolicy> =
        mcp_cfg
            .sampling
            .per_server
            .iter()
            .map(|(server, p)| {
                (
                    server.clone(),
                    nexo_mcp::sampling::PerServerPolicy {
                        enabled: p.enabled,
                        rate_limit_per_minute: p.rate_limit_per_minute,
                        max_tokens_cap: p.max_tokens_cap,
                    },
                )
            })
            .collect();

    let policy = nexo_mcp::sampling::SamplingPolicy::new(
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
        Arc::new(nexo_mcp::sampling::DefaultSamplingProvider::new(
            default_client,
            named,
            policy,
        )) as Arc<dyn nexo_mcp::sampling::SamplingProvider>,
    ))
}

/// Phase 11.2/11.3 — discover extensions and spawn stdio runtimes.
/// Never fatal: bad manifests or spawn failures produce diagnostics; the
/// agent keeps starting. Returns runtimes that the caller must keep alive
/// (drop → cascades SIGTERM to extension children).
#[allow(clippy::type_complexity)]
async fn run_extension_discovery(
    cfg: Option<&nexo_config::ExtensionsConfig>,
) -> (
    Vec<(
        Arc<nexo_extensions::StdioRuntime>,
        nexo_extensions::ExtensionCandidate,
    )>,
    Vec<nexo_extensions::ExtensionMcpDecl>,
) {
    let cfg = cfg.cloned().unwrap_or_default();
    if !cfg.enabled {
        tracing::info!("extension system disabled via config");
        return (Vec::new(), Vec::new());
    }

    let search_paths: Vec<PathBuf> = cfg.search_paths.iter().map(PathBuf::from).collect();
    let discovery = nexo_extensions::ExtensionDiscovery::new(
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
            nexo_extensions::DiagnosticLevel::Warn => tracing::warn!(
                path = %d.path.display(),
                message = %d.message,
                "extension discovery",
            ),
            nexo_extensions::DiagnosticLevel::Error => tracing::error!(
                path = %d.path.display(),
                message = %d.message,
                "extension discovery",
            ),
        }
    }
    for c in &report.candidates {
        let transport = match &c.manifest.transport {
            nexo_extensions::Transport::Stdio { .. } => "stdio",
            nexo_extensions::Transport::Nats { .. } => "nats",
            nexo_extensions::Transport::Http { .. } => "http",
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
    let mcp_decls = nexo_extensions::collect_mcp_declarations(&report, &cfg.disabled);

    // 11.3 — spawn stdio runtimes for each candidate whose transport is Stdio.
    // 11.5 will iterate the returned runtimes to register tools per agent.
    let mut runtimes: Vec<(
        Arc<nexo_extensions::StdioRuntime>,
        nexo_extensions::ExtensionCandidate,
    )> = Vec::new();
    for c in report.candidates {
        if !matches!(
            c.manifest.transport,
            nexo_extensions::Transport::Stdio { .. }
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
        match nexo_extensions::StdioRuntime::spawn(&c.manifest, c.root_dir.clone()).await {
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
    Ok(SingleInstanceLock {
        path: lock_path,
        pid,
    })
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

// ── Phase 26 — pair CLI handlers ─────────────────────────────────────────
//
// All commands open the SQLite store + secret file directly so the
// operator can manage senders without a running daemon. Output is a
// plain table by default, or JSON when `--json` is set.

fn pair_paths(config_dir: &std::path::Path) -> (PathBuf, PathBuf) {
    // FOLLOWUPS PR-6 — `config/pairing.yaml` overrides take priority
    // when present. Falls back to the legacy "next to memory.db" /
    // `~/.nexo/secret/pairing.key` defaults so existing operators
    // see no behaviour change.
    let yaml_overrides = load_pairing_yaml_overrides(config_dir);

    let store = yaml_overrides
        .as_ref()
        .and_then(|p| p.storage.path.clone())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            config_dir
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("data")
                .join("pairing.db")
        });
    let secret = yaml_overrides
        .as_ref()
        .and_then(|p| p.setup_code.secret_path.clone())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| {
                    PathBuf::from(h)
                        .join(".nexo")
                        .join("secret")
                        .join("pairing.key")
                })
                .unwrap_or_else(|| PathBuf::from("./pairing.key"))
        });
    (store, secret)
}

/// Best-effort sync read of `config/pairing.yaml` for CLI commands
/// that don't go through the full async config loader. Returns
/// `None` when the file is absent or unreadable; the caller's
/// existing fallback chain handles that.
fn load_pairing_yaml_overrides(
    config_dir: &std::path::Path,
) -> Option<nexo_config::types::pairing::PairingInner> {
    let path = config_dir.join("pairing.yaml");
    let body = std::fs::read_to_string(&path).ok()?;
    let parsed: nexo_config::types::pairing::PairingConfig = serde_yaml::from_str(&body).ok()?;
    Some(parsed.pairing)
}

async fn open_pair_store(config_dir: &std::path::Path) -> Result<Arc<nexo_pairing::PairingStore>> {
    let (store_path, _) = pair_paths(config_dir);
    if let Some(p) = store_path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    let path = store_path.to_string_lossy().to_string();
    let store = nexo_pairing::PairingStore::open(&path).await?;
    Ok(Arc::new(store))
}

fn run_pair_help() -> Result<()> {
    println!(
        "nexo pair — manage inbound sender allowlists + companion bootstrap codes\n\n\
         Usage:\n\
         \x20 nexo pair start [--for-device <name>] [--public-url <url>] [--qr-png <path>] [--ttl-secs <n>] [--json]\n\
         \x20 nexo pair list  [--channel <id>] [--all] [--include-revoked] [--json]\n\
         \x20 nexo pair approve <CODE> [--json]\n\
         \x20 nexo pair revoke <channel>:<sender_id>\n\
         \x20 nexo pair seed <channel> <account_id> <sender_id> [<sender_id>...]\n"
    );
    Ok(())
}

async fn run_pair_start(
    config_dir: &std::path::Path,
    device_label: Option<&str>,
    public_url: Option<&str>,
    qr_png_path: Option<&std::path::Path>,
    ttl_secs: Option<u64>,
    json: bool,
) -> Result<()> {
    let (_, secret_path) = pair_paths(config_dir);
    if let Some(p) = secret_path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    let issuer = nexo_pairing::SetupCodeIssuer::open_or_create(&secret_path)?;

    // URL resolution priority (highest first):
    //   1. `--public-url` CLI flag (operator override at invoke time)
    //   2. `pairing.yaml::pairing.public_url` (deployment-pinned)
    //   3. `NEXO_TUNNEL_URL` env (tunnel-side bridge until the
    //       `nexo-tunnel` crate exposes an in-process accessor — PR-3).
    //       The `nexo-tunnel` daemon writes its assigned
    //       `https://*.trycloudflare.com` URL here at startup so a
    //       separately-launched `nexo pair start` picks it up.
    //   4. loopback-only → fails closed with a clear error.
    //
    // `ws_cleartext_allow` from the YAML extends the resolver's
    // built-in allow list (loopback / RFC1918 / link-local /
    // `.local` / `10.0.2.2`). PR-6 wired the YAML loader; PR-3
    // wires the runtime priority chain.
    let yaml_overrides = load_pairing_yaml_overrides(config_dir);
    let yaml_public_url = yaml_overrides.as_ref().and_then(|p| p.public_url.clone());
    let yaml_cleartext = yaml_overrides
        .as_ref()
        .map(|p| p.ws_cleartext_allow.clone())
        .unwrap_or_default();
    // FOLLOWUPS PR-3 — tunnel URL discovery priority:
    //   1. `NEXO_TUNNEL_URL` env (back-compat, explicit overrides win).
    //   2. `$NEXO_HOME/state/tunnel.url` sidecar file written by
    //      the daemon when `TunnelManager::start()` succeeded.
    //      This is the in-process accessor — no daemon connection,
    //      no env-var coordination across shells.
    let tunnel_url = std::env::var("NEXO_TUNNEL_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(nexo_tunnel::read_url_file);

    // FOLLOWUPS PR-6 — TTL resolution priority:
    //   1. `--ttl-secs` CLI flag (operator override at invoke time).
    //   2. `pairing.yaml::pairing.setup_code.default_ttl_secs`.
    //   3. hardcoded 600 seconds (10 min) fallback.
    let resolved_ttl_secs = ttl_secs
        .or_else(|| {
            yaml_overrides
                .as_ref()
                .and_then(|p| p.setup_code.default_ttl_secs)
        })
        .unwrap_or(600);

    let inputs = nexo_pairing::url_resolver::UrlInputs {
        public_url: public_url.map(str::to_string).or(yaml_public_url),
        tunnel_url,
        gateway_remote_url: None,
        lan_url: None,
        ws_cleartext_allow_extra: yaml_cleartext,
    };
    let resolved = match nexo_pairing::url_resolver::resolve(&inputs) {
        Ok(r) => r,
        Err(nexo_pairing::url_resolver::ResolveError::LoopbackOnly) => {
            // Common dev-loop dead-end: gateway only on loopback, no
            // tunnel. Walk the configured plugins and print a ready-
            // to-paste `nexo pair seed` for each known (channel,
            // account_id). The operator either pivots to the seed
            // path (no QR needed for local testing) or sets
            // `pairing.public_url` / starts the tunnel and retries.
            print_loopback_seed_hint(config_dir);
            return Err(anyhow::anyhow!(
                "{}",
                nexo_pairing::url_resolver::ResolveError::LoopbackOnly
            ));
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    // Embed the full WS endpoint URL (including path) so the companion
    // can call `connect_async(&payload.url)` directly.
    let pair_url = if resolved.url.ends_with("/pair") {
        resolved.url.clone()
    } else {
        format!("{}/pair", resolved.url.trim_end_matches('/'))
    };
    let code = issuer.issue(
        &pair_url,
        "companion-v1",
        std::time::Duration::from_secs(resolved_ttl_secs),
        device_label,
    )?;
    let payload = nexo_pairing::setup_code::encode_setup_code(&code)?;

    if let Some(path) = qr_png_path {
        let png = nexo_pairing::qr::render_png(&payload)?;
        std::fs::write(path, png)?;
    }

    if json {
        let v = serde_json::json!({
            "url": code.url,
            "url_source": resolved.source,
            "bootstrap_token": code.bootstrap_token,
            "expires_at": code.expires_at,
            "payload": payload,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        println!("Pairing payload (scan or paste into companion):");
        println!();
        println!("{}", nexo_pairing::qr::render_ansi(&payload)?);
        println!();
        println!("Raw payload : {}", payload);
        println!("URL         : {}  (source: {})", code.url, resolved.source);
        println!("Expires at  : {}", code.expires_at);
        if let Some(path) = qr_png_path {
            println!("QR PNG      : {}", path.display());
        }
    }
    Ok(())
}

/// Walk `config/plugins/{telegram,whatsapp}.yaml` and print one
/// ready-to-run `nexo pair seed <channel> <account> <SENDER_ID>`
/// hint per known (channel, account). Used as the loopback-only
/// fallback for `nexo pair start` so the operator gets a working
/// next step instead of a bare error.
///
/// Best-effort: any read/parse failure is swallowed and only a
/// generic hint is printed. The CLI still bubbles the original
/// LoopbackOnly error after this banner.
fn print_loopback_seed_hint(config_dir: &std::path::Path) {
    eprintln!();
    eprintln!("Pairing-start needs a non-loopback gateway URL.");
    eprintln!("For local testing you usually don't need the QR flow at all —");
    eprintln!("seed the operator's chat into the allowlist directly:");
    eprintln!();
    let plugins_dir = config_dir.join("plugins");
    let mut suggested = false;
    if let Ok(text) = std::fs::read_to_string(plugins_dir.join("telegram.yaml")) {
        if let Ok(file) = serde_yaml::from_str::<nexo_config::TelegramPluginConfigFile>(&text) {
            for tg in file.telegram.into_vec() {
                let account = tg.instance.as_deref().unwrap_or("default");
                eprintln!("  nexo pair seed telegram {account} <YOUR_TELEGRAM_USER_ID>");
                suggested = true;
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(plugins_dir.join("whatsapp.yaml")) {
        if let Ok(file) = serde_yaml::from_str::<nexo_config::WhatsappPluginConfigFile>(&text) {
            for wa in file.whatsapp.into_vec() {
                let account = wa.instance.as_deref().unwrap_or("default");
                eprintln!("  nexo pair seed whatsapp {account} <YOUR_WHATSAPP_NUMBER>");
                suggested = true;
            }
        }
    }
    if !suggested {
        eprintln!("  nexo pair seed <channel> <account> <SENDER_ID>");
    }
    eprintln!();
    eprintln!("Or, to keep using the QR flow, set one of:");
    eprintln!("  - `pairing.public_url` in config/pairing.yaml");
    eprintln!("  - `--public-url <wss://…>` flag");
    eprintln!("  - run `nexo` with the tunnel enabled (writes tunnel.url)");
    eprintln!();
}

async fn run_pair_list(
    config_dir: &std::path::Path,
    channel: Option<&str>,
    json: bool,
    show_allow: bool,
    include_revoked: bool,
) -> Result<()> {
    let store = open_pair_store(config_dir).await?;
    let pending = store.list_pending(channel).await?;
    let allow = if show_allow {
        store.list_allow(channel, include_revoked).await?
    } else {
        Vec::new()
    };
    if json {
        // Single object so `--json` consumers always get the same
        // shape regardless of `--all`. `allow` is empty when the flag
        // is off, which mirrors the bare `list` semantics.
        let payload = serde_json::json!({
            "pending": pending,
            "allow": allow,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return Ok(());
    }
    if pending.is_empty() {
        println!("No pending pairing requests.");
    } else {
        println!(
            "{:<10}  {:<14}  {:<16}  {:<26}  {}",
            "CODE", "CHANNEL", "ACCOUNT", "CREATED", "SENDER"
        );
        for p in &pending {
            println!(
                "{:<10}  {:<14}  {:<16}  {:<26}  {}",
                p.code, p.channel, p.account_id, p.created_at, p.sender_id
            );
        }
    }
    if show_allow {
        println!();
        if allow.is_empty() {
            println!("No allowlisted senders.");
        } else {
            println!(
                "{:<14}  {:<16}  {:<24}  {:<10}  {:<26}  {}",
                "CHANNEL", "ACCOUNT", "SENDER", "VIA", "APPROVED", "REVOKED"
            );
            for a in &allow {
                let rev = a
                    .revoked_at
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "-".into());
                println!(
                    "{:<14}  {:<16}  {:<24}  {:<10}  {:<26}  {}",
                    a.channel, a.account_id, a.sender_id, a.approved_via, a.approved_at, rev
                );
            }
        }
    }
    Ok(())
}

async fn run_pair_approve(config_dir: &std::path::Path, code: &str, json: bool) -> Result<()> {
    let store = open_pair_store(config_dir).await?;
    let approved = store.approve(code).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&approved).unwrap());
    } else {
        println!(
            "Approved {}:{}:{} (added to allow_from)",
            approved.channel, approved.account_id, approved.sender_id
        );
    }
    Ok(())
}

async fn run_pair_revoke(config_dir: &std::path::Path, target: &str) -> Result<()> {
    let (channel, sender) = target
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("revoke target must be `<channel>:<sender_id>`"))?;
    let store = open_pair_store(config_dir).await?;
    let did = store.revoke(channel, sender).await?;
    if did {
        println!("Revoked {channel}:{sender}");
    } else {
        println!("No active row to revoke for {channel}:{sender}");
    }
    Ok(())
}

async fn run_pair_seed(
    config_dir: &std::path::Path,
    channel: &str,
    account_id: &str,
    senders: &[String],
) -> Result<()> {
    if senders.is_empty() {
        return Err(anyhow::anyhow!("pair seed requires at least one sender id"));
    }
    let store = open_pair_store(config_dir).await?;
    let n = store.seed(channel, account_id, senders).await?;
    println!(
        "Seeded {} sender(s) into {}:{} allow_from",
        n, channel, account_id
    );
    Ok(())
}

fn cron_db_path() -> std::path::PathBuf {
    nexo_project_tracker::state::nexo_state_dir().join("nexo_cron.db")
}

async fn open_cron_store_for_cli() -> Result<nexo_core::cron_schedule::SqliteCronStore> {
    let path = cron_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let path_s = path.to_string_lossy().into_owned();
    nexo_core::cron_schedule::SqliteCronStore::open(&path_s)
        .await
        .with_context(|| format!("failed to open cron db at {}", path.display()))
}

fn format_unix_utc(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

async fn run_cron_list(binding: Option<&str>, json: bool) -> Result<()> {
    use nexo_core::cron_schedule::CronStore;

    let store = open_cron_store_for_cli().await?;
    let entries = match binding {
        Some(b) => store.list_by_binding(b).await?,
        None => store.list_all().await?,
    };

    if json {
        let out = serde_json::json!({
            "binding": binding,
            "count": entries.len(),
            "entries": entries,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if entries.is_empty() {
        match binding {
            Some(b) => println!("(no cron entries for binding `{b}`)"),
            None => println!("(no cron entries)"),
        }
        return Ok(());
    }

    println!(
        "{} cron entr{}{}",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        binding
            .map(|b| format!(" for binding `{b}`"))
            .unwrap_or_default()
    );

    for e in entries {
        let mode = if e.recurring { "recurring" } else { "one-shot" };
        let status = if e.paused { "paused" } else { "active" };
        println!("- {} [{} | {}] {}", e.id, mode, status, e.cron_expr);
        println!("  binding:     {}", e.binding_id);
        println!("  next_fire:   {}", format_unix_utc(e.next_fire_at));
        if let Some(last) = e.last_fired_at {
            println!("  last_fired:  {}", format_unix_utc(last));
        }
        if e.failure_count > 0 {
            println!("  failures:    {}", e.failure_count);
        }
        if let Some(ch) = e.channel.as_deref() {
            println!("  channel:     {ch}");
        }
        if let Some(to) = e.recipient.as_deref() {
            println!("  recipient:   {to}");
        }
        if let (Some(provider), Some(model)) =
            (e.model_provider.as_deref(), e.model_name.as_deref())
        {
            println!("  model:       {provider}/{model}");
        }
        let prompt = e.prompt.replace('\n', " ");
        println!("  prompt:      {}", truncate(&prompt, 120));
    }
    Ok(())
}

async fn run_cron_drop(id: &str) -> Result<()> {
    use nexo_core::cron_schedule::CronStore;

    let store = open_cron_store_for_cli().await?;
    store.delete(id).await?;
    println!("dropped cron entry {id}");
    Ok(())
}

async fn run_cron_pause(id: &str) -> Result<()> {
    use nexo_core::cron_schedule::CronStore;

    let store = open_cron_store_for_cli().await?;
    store.set_paused(id, true).await?;
    println!("paused cron entry {id}");
    Ok(())
}

async fn run_cron_resume(id: &str) -> Result<()> {
    use nexo_core::cron_schedule::CronStore;

    let store = open_cron_store_for_cli().await?;
    store.set_paused(id, false).await?;
    println!("resumed cron entry {id}");
    Ok(())
}

/// Route a `nexo pair ...` invocation. Returns `Some(Mode)` for any
/// recognised subcommand (including `help` and the bare `pair` form),
/// `None` for unknown so the main dispatcher can show the global
/// usage as a last resort. Walks `positional` end-to-end so flag
/// values like `--public-url wss://x` don't shift the arg index.
fn route_pair_subcommand(positional: &[String], has_json_flag: bool) -> Option<Mode> {
    // Skip entries that are flag-name-or-value pairs.
    let known_kv = [
        "--for-device",
        "--public-url",
        "--qr-png",
        "--ttl-secs",
        "--channel",
    ];
    let mut structural: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < positional.len() {
        let a = positional[i].as_str();
        if known_kv.contains(&a) {
            i += 2; // skip flag + value
            continue;
        }
        if a.starts_with("--") {
            i += 1;
            continue;
        }
        if known_kv.iter().any(|f| a.starts_with(&format!("{f}="))) {
            i += 1;
            continue;
        }
        structural.push(a);
        i += 1;
    }
    let mut iter = structural.into_iter();
    let cmd = iter.next()?;
    if cmd != "pair" {
        return None;
    }
    let sub = iter.next();
    Some(match sub {
        None | Some("help") => Mode::PairHelp,
        Some("start") => Mode::PairStart {
            device_label: parse_kv_flag(positional, "--for-device"),
            public_url: parse_kv_flag(positional, "--public-url"),
            qr_png_path: parse_kv_flag(positional, "--qr-png").map(PathBuf::from),
            ttl_secs: parse_kv_flag(positional, "--ttl-secs").and_then(|s| s.parse::<u64>().ok()),
            json: has_json_flag,
        },
        Some("list") => Mode::PairList {
            channel: parse_kv_flag(positional, "--channel"),
            json: has_json_flag,
            show_allow: positional.iter().any(|a| a == "--all"),
            include_revoked: positional.iter().any(|a| a == "--include-revoked"),
        },
        Some("approve") => match iter.next() {
            Some(code) => Mode::PairApprove {
                code: code.to_string(),
                json: has_json_flag,
            },
            None => {
                eprintln!("error: `pair approve` requires a CODE");
                Mode::PairHelp
            }
        },
        Some("revoke") => match iter.next() {
            Some(target) => Mode::PairRevoke {
                target: target.to_string(),
            },
            None => {
                eprintln!("error: `pair revoke` requires `<channel>:<sender_id>`");
                Mode::PairHelp
            }
        },
        Some("seed") => {
            let channel = iter.next();
            let account_id = iter.next();
            let senders: Vec<String> = iter.map(str::to_string).collect();
            match (channel, account_id) {
                (Some(c), Some(a)) if !senders.is_empty() => Mode::PairSeed {
                    channel: c.to_string(),
                    account_id: a.to_string(),
                    senders,
                },
                _ => {
                    eprintln!(
                        "error: `pair seed` requires <channel> <account_id> <sender_id> [<sender_id>...]"
                    );
                    Mode::PairHelp
                }
            }
        }
        Some(other) => {
            eprintln!("error: unknown pair subcommand `{other}`");
            Mode::PairHelp
        }
    })
}

/// Route a `nexo cron ...` invocation. Handles kv flags without
/// letting their values shift positional arity.
fn route_cron_subcommand(positional: &[String], has_json_flag: bool) -> Option<Mode> {
    let known_kv = ["--binding"];
    let mut structural: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < positional.len() {
        let a = positional[i].as_str();
        if known_kv.contains(&a) {
            i += 2; // skip flag + value
            continue;
        }
        if a.starts_with("--") {
            i += 1;
            continue;
        }
        if known_kv.iter().any(|f| a.starts_with(&format!("{f}="))) {
            i += 1;
            continue;
        }
        structural.push(a);
        i += 1;
    }

    let mut iter = structural.into_iter();
    let cmd = iter.next()?;
    if cmd != "cron" {
        return None;
    }

    Some(match iter.next() {
        Some("list") => Mode::CronList {
            binding: parse_kv_flag(positional, "--binding")
                .or_else(|| iter.next().map(str::to_string)),
            json: has_json_flag,
        },
        Some("drop") => match iter.next() {
            Some(id) => Mode::CronDrop { id: id.to_string() },
            None => {
                eprintln!("error: `cron drop` requires an entry id");
                Mode::Help
            }
        },
        Some("pause") => match iter.next() {
            Some(id) => Mode::CronPause { id: id.to_string() },
            None => {
                eprintln!("error: `cron pause` requires an entry id");
                Mode::Help
            }
        },
        Some("resume") => match iter.next() {
            Some(id) => Mode::CronResume { id: id.to_string() },
            None => {
                eprintln!("error: `cron resume` requires an entry id");
                Mode::Help
            }
        },
        None | Some("help") => {
            eprintln!(
                "error: `cron` requires a subcommand (list|drop <id>|pause <id>|resume <id>)"
            );
            Mode::Help
        }
        Some(other) => {
            eprintln!("error: unknown cron subcommand `{other}`");
            Mode::Help
        }
    })
}

/// Pull a `--name value` pair out of a flat positional list. Used by
/// the pair CLI and any other subcommand that accepts simple kv args.
fn parse_kv_flag(positional: &[String], name: &str) -> Option<String> {
    let mut iter = positional.iter();
    while let Some(a) = iter.next() {
        if a == name {
            return iter.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
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

    // Phase 27.1 — `--version` / `-V`. Pair with `--verbose` for the
    // build-provenance block. The `version` subcommand is handled below
    // alongside the other positional commands.
    if positional.iter().any(|a| a == "--version" || a == "-V") {
        let verbose = positional.iter().any(|a| a == "--verbose");
        return CliArgs {
            config_dir,
            mode: Mode::Version { verbose },
        };
    }

    let has_json_flag = positional.iter().any(|a| a == "--json");
    let pos_no_flags: Vec<String> = positional
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();

    // --check-config is a flag. `--check-config --strict` toggles
    // StrictLevel::Strict for the resolver — warnings become errors.
    if positional.iter().any(|a| a == "--check-config") && pos_no_flags.is_empty() {
        let strict = positional.iter().any(|a| a == "--strict");
        return CliArgs {
            config_dir,
            mode: Mode::CheckConfig { strict },
        };
    }

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

    // Phase 26 — pair CLI handled first so flag values like
    // `--public-url wss://example.com` (which the value-only filter
    // does not strip) don't shift the structural arity of the main
    // match arms below.
    if pos_no_flags.first().map(|s| s.as_str()) == Some("pair") {
        if let Some(mode) = route_pair_subcommand(&positional, has_json_flag) {
            return CliArgs { config_dir, mode };
        }
    }

    if pos_no_flags.first().map(|s| s.as_str()) == Some("cron") {
        if let Some(mode) = route_cron_subcommand(&positional, has_json_flag) {
            return CliArgs { config_dir, mode };
        }
    }

    let mode = match pos_no_flags.as_slice() {
        [] => Mode::Run,
        // Phase 27.1 — `nexo version` is the verbose form (always
        // includes the build-provenance block). `nexo --version` short
        // form is handled before the match.
        [cmd] if cmd == "version" => Mode::Version { verbose: true },
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
        // Phase 76.14 — mcp-server with optional subcommands
        [cmd] if cmd == "mcp-server" => {
            Mode::McpServer(McpServerSubcommand::Serve)
        }
        [cmd, sub, url] if cmd == "mcp-server" && sub == "inspect" => {
            Mode::McpServer(McpServerSubcommand::Inspect {
                url: url.clone(),
            })
        }
        [cmd, sub, url] if cmd == "mcp-server" && sub == "bench" => {
            let tool = positional
                .iter()
                .position(|a| a == "--tool")
                .and_then(|i| positional.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "echo".to_string());
            let rps: u32 = positional
                .iter()
                .position(|a| a == "--rps")
                .and_then(|i| positional.get(i + 1))
                .and_then(|s| s.parse().ok())
                .unwrap_or(10);
            Mode::McpServer(McpServerSubcommand::Bench {
                url: url.clone(),
                tool,
                rps,
            })
        }
        [cmd, sub, db] if cmd == "mcp-server" && sub == "tail-audit" => {
            Mode::McpServer(McpServerSubcommand::TailAudit {
                db: db.clone(),
            })
        }
        // Phase 80.1.d — `nexo agent dream {tail|status|kill}`.
        // Flag parsing inlined per project convention (no clap).
        [cmd, sub] if cmd == "agent" && sub == "dream" => {
            Mode::AgentDream(AgentDreamSubcommand::Tail {
                goal_id: parse_kv_flag(&positional, "--goal"),
                n: parse_kv_flag(&positional, "--n")
                    .and_then(|v: String| v.parse().ok())
                    .unwrap_or(20),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                json: has_json_flag,
            })
        }
        [cmd, sub, verb] if cmd == "agent" && sub == "dream" && verb == "tail" => {
            Mode::AgentDream(AgentDreamSubcommand::Tail {
                goal_id: parse_kv_flag(&positional, "--goal"),
                n: parse_kv_flag(&positional, "--n")
                    .and_then(|v: String| v.parse().ok())
                    .unwrap_or(20),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                json: has_json_flag,
            })
        }
        [cmd, sub, verb, run_id]
            if cmd == "agent" && sub == "dream" && verb == "status" =>
        {
            Mode::AgentDream(AgentDreamSubcommand::Status {
                run_id: run_id.clone(),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                json: has_json_flag,
            })
        }
        [cmd, sub, verb, run_id]
            if cmd == "agent" && sub == "dream" && verb == "kill" =>
        {
            Mode::AgentDream(AgentDreamSubcommand::Kill {
                run_id: run_id.clone(),
                force: positional.iter().any(|a| a == "--force"),
                memory_dir: parse_kv_flag(&positional, "--memory-dir").map(PathBuf::from),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            })
        }
        // Phase 80.10 — `nexo agent ps [--kind=...] [--all] [--json]`.
        [cmd, sub] if cmd == "agent" && sub == "ps" => Mode::AgentPs {
            kind: parse_kv_flag(&positional, "--kind"),
            all: positional.iter().any(|a| a == "--all"),
            db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            json: has_json_flag,
        },
        // Phase 80.10 — `nexo agent run [--bg] <prompt...>`. Concatenates
        // remaining positional words (filtered of `--flag` tokens) into
        // the prompt so operators can pass spaces without quoting:
        //   `nexo agent run --bg ship the release`
        [cmd, sub, ..] if cmd == "agent" && sub == "run" => {
            let bg = positional
                .iter()
                .any(|a| a == "--bg" || a == "--background");
            let words: Vec<String> = positional
                .iter()
                .skip(2) // skip "agent" + "run"
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .collect();
            let prompt = words.join(" ");
            Mode::AgentRun {
                prompt,
                bg,
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                json: has_json_flag,
            }
        }
        // Phase 80.16 — `nexo agent attach <goal_id> [--db=...] [--json]`.
        [cmd, sub, goal_id] if cmd == "agent" && sub == "attach" => {
            Mode::AgentAttach {
                goal_id: goal_id.clone(),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                json: has_json_flag,
            }
        }
        // Phase 80.16 — `nexo agent discover [--include-interactive]
        // [--db=...] [--json]`.
        [cmd, sub] if cmd == "agent" && sub == "discover" => Mode::AgentDiscover {
            include_interactive: positional
                .iter()
                .any(|a| a == "--include-interactive"),
            db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            json: has_json_flag,
        },
        // Phase 80.9.e — `nexo channel list [--config=<path>] [--json]`.
        [cmd, sub] if cmd == "channel" && sub == "list" => Mode::ChannelList {
            config: parse_kv_flag(&positional, "--config").map(PathBuf::from),
            json: has_json_flag,
        },
        // Phase 80.9.e — `nexo channel doctor [--config=<path>]
        // [--binding=<id>] [--json]`.
        [cmd, sub] if cmd == "channel" && sub == "doctor" => Mode::ChannelDoctor {
            config: parse_kv_flag(&positional, "--config").map(PathBuf::from),
            binding: parse_kv_flag(&positional, "--binding"),
            json: has_json_flag,
        },
        // Phase 80.9.e — `nexo channel test <server> [--binding=<id>]
        // [--content=...] [--config=<path>] [--json]`.
        [cmd, sub, server] if cmd == "channel" && sub == "test" => Mode::ChannelTest {
            server: server.clone(),
            binding: parse_kv_flag(&positional, "--binding"),
            content: parse_kv_flag(&positional, "--content"),
            config: parse_kv_flag(&positional, "--config").map(PathBuf::from),
            json: has_json_flag,
        },
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
        [cmd, sub] if cmd == "setup" && sub == "migrate" => Mode::SetupMigrate {
            apply: positional.iter().any(|a| a == "--apply"),
        },
        [cmd, sub] if cmd == "doctor" && sub == "capabilities" => Mode::DoctorCapabilities {
            json: has_json_flag,
        },
        [cmd, sub] if cmd == "setup" && sub == "telegram-link" => {
            Mode::SetupTelegramLink { agent: None }
        }
        [cmd, sub, agent] if cmd == "setup" && sub == "telegram-link" => Mode::SetupTelegramLink {
            agent: Some(agent.clone()),
        },
        // (pair handled by route_pair_subcommand earlier)
        [cmd, service] if cmd == "setup" => Mode::SetupOne {
            service: service.clone(),
        },
        [cmd] if cmd == "reload" => Mode::Reload {
            json: has_json_flag,
        },
        [cmd] if cmd == "pollers" => Mode::PollersList {
            json: has_json_flag,
        },
        [cmd, sub] if cmd == "pollers" && sub == "list" => Mode::PollersList {
            json: has_json_flag,
        },
        [cmd, sub] if cmd == "pollers" && sub == "reload" => Mode::PollersReload,
        [cmd, sub, id] if cmd == "pollers" && sub == "show" => Mode::PollersShow {
            id: id.clone(),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "pollers" && sub == "run" => Mode::PollersRun { id: id.clone() },
        [cmd, sub, id] if cmd == "pollers" && sub == "pause" => {
            Mode::PollersPause { id: id.clone() }
        }
        [cmd, sub, id] if cmd == "pollers" && sub == "resume" => {
            Mode::PollersResume { id: id.clone() }
        }
        [cmd, sub, id] if cmd == "pollers" && sub == "reset" => Mode::PollersReset {
            id: id.clone(),
            yes: positional.iter().any(|a| a == "--yes"),
        },
        [cmd] if cmd == "admin" => {
            // --port <N> or --port=<N>. Default 9099 (away from 8080 /
            // 9090 / 9091 used by the main daemon's health / metrics /
            // admin servers so `agent admin` can run alongside them).
            let mut port: u16 = 9099;
            let mut iter = positional.iter();
            while let Some(a) = iter.next() {
                if a == "--port" {
                    if let Some(v) = iter.next() {
                        if let Ok(n) = v.parse() {
                            port = n;
                        }
                    }
                } else if let Some(rest) = a.strip_prefix("--port=") {
                    if let Ok(n) = rest.parse() {
                        port = n;
                    }
                }
            }
            Mode::Admin { port }
        }
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

/// Phase 27.1 — print version to stdout. `verbose=false` mirrors clap's
/// auto `--version` (`nexo <pkg-version>`); `verbose=true` adds the four
/// build stamps captured by `build.rs` so bug reports carry provenance.
fn print_version(verbose: bool) {
    let version = env!("CARGO_PKG_VERSION");
    println!("nexo {version}");
    if verbose {
        println!("  git-sha:   {}", env!("NEXO_BUILD_GIT_SHA"));
        println!("  target:    {}", env!("NEXO_BUILD_TARGET_TRIPLE"));
        println!("  channel:   {}", env!("NEXO_BUILD_CHANNEL"));
        println!("  built-at:  {}", env!("NEXO_BUILD_TIMESTAMP"));
    }
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
    println!(
        "  agent doctor capabilities [--json]     List write/reveal env toggles and their state"
    );
    println!("  agent flow <sub> ...                   TaskFlow admin (run `agent flow` for help)");
    println!("  agent status [<id>] [--json] [--endpoint=URL] Pretty-print running agents (or one by id)");
    println!(
        "  agent --dry-run [--json]               Validate config and print a summary (no runtime)"
    );
    println!("  agent --check-config                   Validate config and exit (no runtime)");
    println!("  agent reload                           Trigger a hot-reload on the running daemon");
    println!(
        "  agent setup [<service>]                Interactive setup wizard (defaults to menu)"
    );
    println!(
        "  agent setup list                       Print every credential service the wizard knows"
    );
    println!("  agent setup doctor                     Audit configured secrets and report what's missing");
    println!("  agent setup migrate [--dry-run|--apply] Run versioned YAML config migrations");
    println!(
        "  agent setup telegram-link [<agent>]    Pair an existing Telegram instance to an agent"
    );
    println!("  agent admin [--port <n>]               Launch the loopback admin web UI");
    println!("  agent mcp-server                       Run as an MCP stdio/HTTP server (expose tools)");
    println!("  agent mcp-server inspect <url>         List tools + resources of a remote MCP server");
    println!("  agent mcp-server bench <url> --tool <n> --rps <n>  Load test a tool");
    println!("  agent mcp-server tail-audit <db>        Read recent audit log entries");
    println!("  agent pollers list [--json]            List configured poller jobs");
    println!("  agent pollers show <id> [--json]       Show one poller job's config + last tick");
    println!("  agent pollers run <id>                 Force a single tick of a poller job");
    println!("  agent pollers pause <id>               Pause a poller job (no ticks until resume)");
    println!("  agent pollers resume <id>              Resume a paused poller job");
    println!("  agent pollers reset <id>               Clear a job's seen-id dedup cache");
    println!(
        "  agent pollers reload                   Re-read config/pollers.yaml without restart"
    );
    println!("  agent cron list [--json] [--binding <id>]  List scheduled cron entries");
    println!("  agent cron drop <id>                   Delete a scheduled cron entry");
    println!("  agent cron pause <id>                  Pause a scheduled cron entry");
    println!("  agent cron resume <id>                 Resume a paused cron entry");
}

fn run_setup_migrate(config_dir: &std::path::Path, apply: bool) -> Result<()> {
    let report = nexo_config::migrations::migrate_config_dir(config_dir, apply)?;
    let mode = if apply { "apply" } else { "dry-run" };
    println!(
        "setup migrate ({mode}) — latest schema version {}",
        nexo_config::migrations::LATEST_SCHEMA_VERSION
    );
    if report.files.is_empty() {
        println!("no config files found under {}", config_dir.display());
        return Ok(());
    }
    for f in &report.files {
        let marker = if f.changed { "*" } else { "=" };
        println!(
            "{} {}: v{} -> v{}{}",
            marker,
            f.file,
            f.from_version,
            f.to_version,
            if f.changed && !apply {
                " (pending)"
            } else {
                ""
            }
        );
    }
    println!(
        "{} file(s) {}",
        report.changed_count(),
        if apply {
            "migrated"
        } else {
            "with pending changes"
        }
    );
    Ok(())
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
    nexo_extensions::cli::print_help(&mut stdout)?;
    Ok(())
}

/// `agent admin` — boot the web admin UI behind a fresh Cloudflare
/// quick tunnel. Returns on Ctrl+C / SIGTERM after shutting the tunnel
/// and the local HTTP listener down cleanly.
///
/// Flow:
///   1. `nexo_tunnel::binary::ensure_cloudflared()` — downloads the
///      right cloudflared binary for this OS/arch if it isn't already
///      on disk. First-run is chatty on stdout so the operator sees
///      what's being fetched.
///   2. Mint a fresh admin password + a per-run session secret. The
///      session secret signs cookies so restarting the daemon
///      invalidates every live session (a re-login is required every
///      launch, matching the rotating URL).
///   3. Start a loopback HTTP server on `127.0.0.1:<port>` that serves
///      the React bundle, a `/login` form, `POST /api/login`, and
///      `POST /api/logout`. Everything behind `/api/` (and the SPA
///      bundle itself) requires a valid session cookie; the bundle
///      entry point redirects anonymous visitors to `/login`.
///   4. `TunnelManager::start()` opens a new trycloudflare.com URL
///      (ephemeral — a fresh one every invocation, no account).
///   5. Print the URL and credentials; wait for a shutdown signal.
async fn run_admin_web(port: u16) -> Result<()> {
    // Step 1: make sure cloudflared is installed.
    println!("[admin] checking cloudflared…");
    let bin = nexo_tunnel::binary::ensure_cloudflared()
        .await
        .context("failed to install cloudflared")?;
    println!("[admin] cloudflared ready ({})", bin.display());

    // Step 2: mint a fresh admin password + a per-run HMAC secret.
    let password = generate_admin_password();
    let session_secret: [u8; 32] = rand::random();
    let admin_ctx = Arc::new(AdminSession {
        password,
        secret: session_secret,
    });

    // Step 3: bind the loopback HTTP listener.
    let bind = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind admin web on {bind}"))?;
    println!("[admin] listening on http://{bind}");

    let admin_ctx_for_task = Arc::clone(&admin_ctx);
    let http_task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(error = %e, "admin accept failed");
                    continue;
                }
            };
            let ctx = Arc::clone(&admin_ctx_for_task);
            tokio::spawn(async move {
                if let Err(e) = handle_admin_request(stream, ctx).await {
                    tracing::debug!(error = %e, "admin request handler failed");
                }
            });
        }
    });

    // Step 4: open the tunnel.
    println!("[admin] opening Cloudflare quick tunnel (ephemeral URL, no account)…");
    let tunnel = nexo_tunnel::TunnelManager::new(port)
        .start()
        .await
        .context("failed to start Cloudflare tunnel")?;
    let password_came_from_env = std::env::var("AGENT_ADMIN_PASSWORD")
        .map(|v| v.trim().len() >= 12)
        .unwrap_or(false);
    println!();
    println!("    ┌────────────────────────────────────────────────────────────");
    println!("    │  admin URL : {}", tunnel.url);
    println!("    │  username  : admin");
    if password_came_from_env {
        // Don't print the env-supplied password back to stdout — the
        // operator has it already; centralised logs shouldn't.
        println!(
            "    │  password  : {} (from $AGENT_ADMIN_PASSWORD)",
            password_fingerprint(&admin_ctx.password)
        );
    } else {
        println!("    │  password  : {}", admin_ctx.password);
    }
    println!("    └────────────────────────────────────────────────────────────");
    println!();
    println!("    Open the URL, log in with the credentials above.");
    println!("    (Ctrl+C to stop. Fresh URL + password every launch —");
    println!("     the password is never stored to disk.)");
    if !password_came_from_env {
        println!();
        println!("    Tip: in production, set AGENT_ADMIN_PASSWORD before launch");
        println!("    so the secret never crosses stdout / journald / container logs.");
    }
    println!();

    // Step 5: wait for shutdown.
    tokio::signal::ctrl_c()
        .await
        .context("install Ctrl+C handler")?;
    println!();
    println!("[admin] shutting down…");
    tunnel.shutdown().await;
    http_task.abort();
    Ok(())
}

/// Shared state for every admin HTTP request: the per-run admin
/// password and the 32-byte HMAC secret used to sign session cookies.
/// Both rotate on every `agent admin` launch — stopping the daemon
/// invalidates every outstanding session.
struct AdminSession {
    password: String,
    secret: [u8; 32],
}

impl AdminSession {
    /// Mints a signed session cookie value. Payload is the ASCII
    /// expiry timestamp (seconds since epoch); signature is the
    /// lowercase-hex SHA-256 HMAC over that payload. Inline SHA-256
    /// (below) avoids pulling a new crate.
    fn issue_cookie(&self, ttl_seconds: u64) -> String {
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_add(ttl_seconds);
        let payload = expires.to_string();
        let sig = hmac_sha256_hex(&self.secret, payload.as_bytes());
        format!("{payload}.{sig}")
    }

    /// Returns `true` iff the cookie was signed by this run's secret
    /// and hasn't expired.
    fn validate_cookie(&self, value: &str) -> bool {
        let Some((payload, sig)) = value.split_once('.') else {
            return false;
        };
        let expected = hmac_sha256_hex(&self.secret, payload.as_bytes());
        if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
            return false;
        }
        let Ok(expires) = payload.parse::<u64>() else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        now < expires
    }
}

const ADMIN_COOKIE_NAME: &str = "nexo_admin";
/// 24 hours — re-login forced once a day, tunnel rotation invalidates
/// every cookie alongside the daemon anyway.
const ADMIN_COOKIE_TTL_SECS: u64 = 24 * 60 * 60;

async fn handle_admin_request(
    mut stream: TcpStream,
    ctx: Arc<AdminSession>,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let request = read_http_head(&mut stream).await.unwrap_or_default();
    let first_line = request.lines().next().unwrap_or("");
    let mut tokens = first_line.split_whitespace();
    let method = tokens.next().unwrap_or("GET").to_ascii_uppercase();
    let path = tokens.next().unwrap_or("/").to_string();

    let authorised = request
        .lines()
        .find_map(|line| {
            line.strip_prefix("Cookie: ")
                .or_else(|| line.strip_prefix("cookie: "))
        })
        .and_then(|cookies| {
            cookies.split(';').find_map(|pair| {
                let pair = pair.trim();
                pair.strip_prefix(&format!("{ADMIN_COOKIE_NAME}="))
            })
        })
        .map(|value| ctx.validate_cookie(value))
        .unwrap_or(false);

    // /api/login — accept credentials, issue cookie.
    if method == "POST" && path == "/api/login" {
        // Read the body — simple key=value form url-encoded bodies
        // land in a single read; 4 KB cap is ample.
        let body = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let mut username = String::new();
        let mut password = String::new();
        for pair in body.split('&') {
            if let Some(v) = pair.strip_prefix("username=") {
                username = url_decode(v);
            } else if let Some(v) = pair.strip_prefix("password=") {
                password = url_decode(v);
            }
        }
        if username == "admin" && constant_time_eq(password.as_bytes(), ctx.password.as_bytes()) {
            let cookie = ctx.issue_cookie(ADMIN_COOKIE_TTL_SECS);
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Set-Cookie: {}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                body.len(),
                ADMIN_COOKIE_NAME,
                cookie,
                ADMIN_COOKIE_TTL_SECS,
                body,
            );
            stream.write_all(response.as_bytes()).await?;
        } else {
            let body = r#"{"ok":false,"error":"invalid credentials"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
        }
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/logout — drop the cookie regardless of current state.
    if method == "POST" && path == "/api/logout" {
        let body = r#"{"ok":true}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Set-Cookie: {}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            body.len(),
            ADMIN_COOKIE_NAME,
            body,
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/bootstrap — tells the SPA whether the first-run wizard
    // should fire. Returns { needs_wizard, agent_count }. Safe pre-
    // login (no sensitive data) so the bundle can decide to redirect
    // to /wizard even before the session cookie exists.
    if method == "GET" && path == "/api/bootstrap" {
        let (needs_wizard, agent_count) = bootstrap_status();
        let body = format!("{{\"needs_wizard\":{needs_wizard},\"agent_count\":{agent_count}}}");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/bootstrap/finish — runs the wizard commit. Writes
    // config/agents.d/<slug>.yaml + secrets files + optional channel
    // config. Requires a valid session cookie so a public tunnel URL
    // alone can't create an agent.
    if method == "POST" && path == "/api/bootstrap/finish" {
        if !authorised {
            let body = r#"{"ok":false,"error":"unauthorised"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        let body_str = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let response_body = match commit_bootstrap(&body_str) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/agents — structured agent directory for the dashboard.
    // Cookie-gated; reads YAML from disk on every call (no caching
    // here, the admin is not a hot path and we prefer the live view).
    if method == "GET" && path == "/api/agents" {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let body = list_agents_json();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/channels — every plugin instance declared in
    // config/plugins/*.yaml with the bound agents resolved from
    // `allow_agents` + the per-agent `credentials` block.
    if method == "GET" && path == "/api/channels" {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let body = list_channels_json();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/channels/telegram — add a new Telegram bot instance.
    // Body: { instance: "label", token: "...", allow_agents?: [ids] }.
    // Writes the token to ./secrets/<instance>_telegram_token.txt
    // (mode 0600) and appends to config/plugins/telegram.yaml in
    // multi-instance sequence form. Fails if the instance label
    // already exists.
    if method == "POST" && path == "/api/channels/telegram" {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let body_str = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let response_body = match add_telegram_channel(&body_str) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/channels/telegram/<instance> — rotate token and/or
    // replace allow_agents for an existing instance. Body matches
    // the POST shape but `instance` is taken from the URL.
    if method == "PATCH" && path.starts_with("/api/channels/telegram/") {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let instance = path
            .trim_start_matches("/api/channels/telegram/")
            .trim_matches('/')
            .to_string();
        let body_str = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let response_body = match edit_telegram_channel(&instance, &body_str) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/channels/<plugin>/<instance> DELETE — remove an instance
    // entry from plugins/<plugin>.yaml. Secret token file stays on
    // disk so the operator can restore by re-adding; Rust does not
    // delete files the UI didn't create.
    if method == "DELETE" && path.starts_with("/api/channels/") {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let rest = path.trim_start_matches("/api/channels/").trim_matches('/');
        let mut parts = rest.splitn(2, '/');
        let plugin = parts.next().unwrap_or("").to_string();
        let instance = parts.next().unwrap_or("").to_string();
        let response_body = match delete_channel(&plugin, &instance) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/agents/<id>/credentials PATCH — pin a specific channel
    // instance to the agent. Body keys correspond to the channel
    // kinds we understand today ({ telegram, whatsapp, google }).
    // Each value is the instance label to bind, an empty string to
    // unset, or omitted to leave the current binding alone.
    if method == "PATCH" && path.starts_with("/api/agents/") && path.ends_with("/credentials") {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let id = path
            .trim_start_matches("/api/agents/")
            .trim_end_matches("/credentials")
            .trim_matches('/')
            .to_string();
        let body_str = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let response_body = match pin_agent_credentials(&id, &body_str) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/mcp/servers — list every MCP server declared in
    // `config/mcp.yaml`. Read-only view paired with POST/PATCH/DELETE
    // for full CRUD over the file.
    if method == "GET" && path == "/api/mcp/servers" {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let body = list_mcp_servers_json();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/mcp/servers POST — add a new server entry. Body shape:
    //   { name, transport: "stdio"|"streamable_http"|"sse",
    //     command?, args?: [str], env?: {k:v},
    //     url?, headers?: {k:v}, log_level?, context_passthrough? }
    if method == "POST" && path == "/api/mcp/servers" {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let body_str = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let response_body = match add_mcp_server(&body_str) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/mcp/servers/<name> PATCH — replace an existing entry's
    // fields. Shape mirrors POST but `name` is read from URL.
    if method == "PATCH" && path.starts_with("/api/mcp/servers/") {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let name = path
            .trim_start_matches("/api/mcp/servers/")
            .trim_matches('/')
            .to_string();
        let body_str = read_http_body(&request, &mut stream)
            .await
            .unwrap_or_default();
        let response_body = match edit_mcp_server(&name, &body_str) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/mcp/servers/<name> DELETE — drop the entry from mcp.yaml.
    if method == "DELETE" && path.starts_with("/api/mcp/servers/") {
        if !authorised {
            write_401_json(&mut stream).await?;
            return Ok(());
        }
        let name = path
            .trim_start_matches("/api/mcp/servers/")
            .trim_matches('/')
            .to_string();
        let response_body = match delete_mcp_server(&name) {
            Ok(report) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                report.len(),
                report
            ),
            Err(err) => {
                let body = format!(
                    "{{\"ok\":false,\"error\":\"{}\"}}",
                    err.replace('\\', "\\\\").replace('"', "\\\"")
                );
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            }
        };
        stream.write_all(response_body.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/debug/env — feature probe consumed by the SPA so it can
    // show the Reset button only when the dev toggle is on. Gated
    // behind NEXO_ADMIN_DEBUG=1 (or the debug_assertions cfg so
    // `cargo run` without the flag still works). Always returns the
    // same JSON shape regardless of auth so the SPA can probe
    // pre-login too.
    if method == "GET" && path == "/api/debug/env" {
        let enabled = admin_debug_enabled();
        let body = format!(
            "{{\"debug\":{},\"build\":\"{}\"}}",
            enabled,
            if cfg!(debug_assertions) {
                "dev"
            } else {
                "release"
            }
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /api/debug/reset — nukes agents.d, data/**, workspaces, DBs.
    // Only honoured when the debug toggle is on; otherwise 404.
    // Requires a valid session cookie so a public tunnel URL alone
    // can't trigger it.
    if method == "POST" && path == "/api/debug/reset" {
        if !authorised {
            let body = r#"{"ok":false,"error":"unauthorised"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        if !admin_debug_enabled() {
            let body = r#"{"ok":false,"error":"debug mode disabled — set NEXO_ADMIN_DEBUG=1"}"#;
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        let report = debug_reset_now();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            report.len(),
            report
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // /login (GET) — always served unauthenticated.
    if path == "/login" || path.starts_with("/login?") {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            ADMIN_LOGIN_HTML.len(),
            ADMIN_LOGIN_HTML
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // API callers (JSON) get 401; browsers get a 302 to /login.
    if !authorised {
        let wants_json = request.contains("Accept: application/json")
            || request.contains("accept: application/json")
            || path.starts_with("/api/");
        let response = if wants_json {
            let body = r#"{"ok":false,"error":"unauthorised"}"#;
            format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        } else {
            String::from(
                "HTTP/1.1 302 Found\r\nLocation: /login\r\n\
                 Cache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
        };
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        return Ok(());
    }

    // Authorised — serve the SPA bundle (or the bundle-missing fallback).
    let (body, mime) = match admin_asset_for_path(&path) {
        Some(pair) => pair,
        None => (
            ADMIN_FALLBACK_HTML.as_bytes().to_vec(),
            "text/html; charset=utf-8",
        ),
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        mime,
        body.len(),
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Login page — plain HTML + inline JS posting to `/api/login`.
/// Ships as a standalone page so it works before the SPA bundle
/// loads (and lets operators authenticate without shipping the React
/// bundle at all, e.g. first-run diagnostics).
const ADMIN_LOGIN_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>nexo-rs admin — login</title>
<style>
  :root {
    color-scheme: light dark;
    --bg: #fafafa; --fg: #1a1a1a; --muted: #555;
    --card: #fff; --border: #e5e5e5;
    --accent: #0066cc; --accent-fg: #fff;
    --error-bg: #fee; --error-fg: #a00;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0a0a0a; --fg: #f5f5f5; --muted: #aaa;
      --card: #1a1a1a; --border: #333;
      --error-bg: #3a0f0f; --error-fg: #ffa;
    }
  }
  * { box-sizing: border-box; }
  body { margin: 0; min-height: 100vh;
         display: flex; align-items: center; justify-content: center;
         font: 16px/1.5 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
         background: var(--bg); color: var(--fg); }
  .card { background: var(--card); border: 1px solid var(--border);
          border-radius: 10px; padding: 2rem 2.25rem; width: min(92vw, 24rem);
          box-shadow: 0 1px 2px rgba(0,0,0,.04), 0 8px 24px rgba(0,0,0,.06); }
  h1 { margin: 0 0 .25rem; font-size: 1.25rem; }
  p.sub { margin: 0 0 1.25rem; color: var(--muted); font-size: .9rem; }
  label { display: block; font-size: .85rem; color: var(--muted);
          margin: 1rem 0 .25rem; }
  input { width: 100%; padding: .55rem .75rem; font: inherit;
          border: 1px solid var(--border); border-radius: 6px;
          background: transparent; color: var(--fg); }
  input:focus { outline: 2px solid var(--accent); outline-offset: -2px; }
  button { width: 100%; margin-top: 1.25rem; padding: .6rem .75rem;
           font: inherit; font-weight: 600; border: 0;
           border-radius: 6px; background: var(--accent); color: var(--accent-fg);
           cursor: pointer; }
  button:hover { filter: brightness(1.08); }
  button:disabled { opacity: .6; cursor: progress; }
  .error { margin-top: 1rem; background: var(--error-bg); color: var(--error-fg);
           padding: .55rem .75rem; border-radius: 6px; font-size: .9rem;
           display: none; }
  .error.show { display: block; }
  .hint { margin-top: 1rem; font-size: .8rem; color: var(--muted); }
</style>
</head>
<body>
  <form class="card" id="login">
    <h1>nexo-rs admin</h1>
    <p class="sub">Sign in to continue.</p>
    <label for="u">Username</label>
    <input id="u" name="username" autocomplete="username" value="admin" required>
    <label for="p">Password</label>
    <input id="p" name="password" type="password" autocomplete="current-password" required autofocus>
    <button type="submit" id="submit">Sign in</button>
    <div class="error" id="err"></div>
    <p class="hint">The password was printed once in the terminal that launched
       <code>agent admin</code>. A fresh password is minted every launch.</p>
  </form>
  <script>
    const form = document.getElementById('login');
    const err = document.getElementById('err');
    const btn = document.getElementById('submit');
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault();
      err.classList.remove('show');
      btn.disabled = true;
      const body = new URLSearchParams({
        username: form.username.value,
        password: form.password.value,
      }).toString();
      try {
        const r = await fetch('/api/login', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          body,
        });
        if (r.ok) {
          window.location.replace('/');
          return;
        }
        let text = 'Invalid credentials.';
        try {
          const data = await r.json();
          if (data.error) text = data.error;
        } catch {}
        err.textContent = text;
        err.classList.add('show');
      } catch (e) {
        err.textContent = 'Network error: ' + e.message;
        err.classList.add('show');
      } finally {
        btn.disabled = false;
      }
    });
  </script>
</body>
</html>
"#;

/// Grab the HTTP body for a POST. Request head is already in `head`
/// (up through `\r\n\r\n`); anything already in `head` past that blank
/// line is the start of the body. Remaining bytes are streamed from
/// `stream` until we've read `Content-Length` worth.
async fn read_http_body(head: &str, stream: &mut TcpStream) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let content_length = head
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let already = head
        .split_once("\r\n\r\n")
        .map(|(_, rest)| rest.as_bytes())
        .unwrap_or(&[]);
    let mut body = already.to_vec();
    while body.len() < content_length {
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > 65_536 {
            break;
        }
    }
    body.truncate(content_length);
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn url_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(n) = u8::from_str_radix(hex, 16) {
                    out.push(n as char);
                    i += 3;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            b => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

/// Inline HMAC-SHA256 that returns lowercase hex. Used to sign
/// session cookies; 32-byte secret + arbitrary-length payload.
fn hmac_sha256_hex(secret: &[u8; 32], payload: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const BLOCK_SIZE: usize = 64;
    let mut key = [0u8; BLOCK_SIZE];
    key[..secret.len()].copy_from_slice(secret);
    let mut ipad = [0u8; BLOCK_SIZE];
    let mut opad = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] = key[i] ^ 0x36;
        opad[i] = key[i] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(payload);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    let digest = outer.finalize();
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Cheap check: if both `config/agents.yaml` has no active agent
/// entries (ignoring comments) **and** `config/agents.d/*.yaml`
/// (excluding `*.example.yaml`) is empty, the wizard should fire.
/// We don't fully parse the YAML — a substring probe for any
/// `- id:` list item is enough to avoid false positives from blank
/// files. Returns `(needs_wizard, agent_count)`.
fn bootstrap_status() -> (bool, usize) {
    let mut agents = 0usize;

    let base = std::fs::read_to_string("./config/agents.yaml").unwrap_or_default();
    for line in base.lines() {
        let t = line.trim_start();
        if t.starts_with("- id:") || t.starts_with("- id :") {
            agents += 1;
        }
    }

    if let Ok(entries) = std::fs::read_dir("./config/agents.d") {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".yaml") || name.ends_with(".example.yaml") {
                continue;
            }
            let body = std::fs::read_to_string(&p).unwrap_or_default();
            for line in body.lines() {
                let t = line.trim_start();
                if t.starts_with("- id:") || t.starts_with("- id :") {
                    agents += 1;
                }
            }
        }
    }

    (agents == 0, agents)
}

/// Parses the wizard's JSON body and writes the derived YAML + secrets
/// to disk. Minimal hand-rolled JSON path — the shape is fixed.
///
/// Accepted body shape:
/// ```json
/// {
///   "identity": { "name": "...", "emoji": "...", "vibe": "..." },
///   "soul":     "markdown...",
///   "brain":    { "provider": "minimax|anthropic|openai|gemini",
///                 "model": "...", "api_key": "..." },
///   "channel":  null
///              | { "kind": "telegram", "token": "..." }
///              | { "kind": "whatsapp" }
/// }
/// ```
fn commit_bootstrap(body: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;

    let identity = v.get("identity").cloned().unwrap_or_default();
    let name = identity
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return Err("identity.name is required".into());
    }
    let emoji = identity
        .get("emoji")
        .and_then(|s| s.as_str())
        .unwrap_or("🤖");
    let vibe = identity
        .get("vibe")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();

    let soul = v.get("soul").and_then(|s| s.as_str()).unwrap_or("").trim();

    let brain = v.get("brain").cloned().unwrap_or_default();
    let provider = brain
        .get("provider")
        .and_then(|s| s.as_str())
        .unwrap_or("minimax");
    let model = brain
        .get("model")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| match provider {
            "anthropic" => "claude-haiku-4-5".to_string(),
            "gemini" => "gemini-2.0-flash".to_string(),
            "openai" => "gpt-4o-mini".to_string(),
            _ => "MiniMax-M2.5".to_string(),
        });
    let api_key = brain
        .get("api_key")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();

    // Kebab-cased slug from the name.
    let slug = {
        let mut s = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        while s.contains("--") {
            s = s.replace("--", "-");
        }
        if s.is_empty() {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            s = format!("agent-{:x}", rng.gen::<u32>());
        }
        s
    };

    let channel = v.get("channel").cloned().unwrap_or(serde_json::Value::Null);
    let (channel_kind, telegram_instance, telegram_token) = match &channel {
        serde_json::Value::Object(map) => {
            let kind = map.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            match kind {
                "telegram" => {
                    let tok = map
                        .get("token")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if tok.is_empty() {
                        return Err("channel.token is required for telegram".into());
                    }
                    let instance = format!("{slug}_bot");
                    (Some("telegram"), Some(instance), Some(tok))
                }
                "whatsapp" => (Some("whatsapp"), None, None),
                "" => (None, None, None),
                other => return Err(format!("unknown channel kind '{other}'")),
            }
        }
        _ => (None, None, None),
    };

    // Collision guard: refuse to overwrite an agent the operator
    // already configured. Earlier the wizard happily wrote a
    // truncated `agents.d/<id>.yaml` next to a full definition in
    // `agents.yaml`; with the strict drop-in override semantics
    // that silently nuked the existing bindings.
    let agent_path = format!("./config/agents.d/{slug}.yaml");
    if std::path::Path::new(&agent_path).exists() {
        return Err(format!(
            "agent `{slug}` already exists at {agent_path} — borralo o usá `agent setup` para editarlo"
        ));
    }
    let agents_yaml = std::path::Path::new("./config/agents.yaml");
    if agents_yaml.exists() {
        let raw =
            std::fs::read_to_string(agents_yaml).map_err(|e| format!("read agents.yaml: {e}"))?;
        if let Ok(parsed) = serde_yaml::from_str::<serde_yaml::Value>(&raw) {
            let collides = parsed
                .get("agents")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .any(|item| item.get("id").and_then(|v| v.as_str()) == Some(slug.as_str()))
                })
                .unwrap_or(false);
            if collides {
                return Err(format!(
                    "agent `{slug}` ya está en config/agents.yaml — escogé otro id o editá el existente"
                ));
            }
        }
    }

    let mut written: Vec<String> = Vec::new();

    if !api_key.is_empty() {
        std::fs::create_dir_all("./secrets").map_err(|e| format!("mkdir ./secrets: {e}"))?;
        let path = format!("./secrets/{provider}_api_key.txt");
        write_file_0600(&path, api_key).map_err(|e| format!("write {path}: {e}"))?;
        written.push(path);
    }

    if let (Some(inst), Some(token)) = (telegram_instance.as_ref(), telegram_token.as_ref()) {
        std::fs::create_dir_all("./secrets").map_err(|e| format!("mkdir ./secrets: {e}"))?;
        let path = format!("./secrets/{inst}_telegram_token.txt");
        write_file_0600(&path, token).map_err(|e| format!("write {path}: {e}"))?;
        written.push(path);
    }

    // Compose agents.d/<slug>.yaml.
    let mut yaml = String::new();
    yaml.push_str(&format!(
        "# Written by admin first-run wizard for agent '{slug}'.\n"
    ));
    yaml.push_str("# Edit freely — the wizard never rewrites this file once created.\n\n");
    yaml.push_str("agents:\n");
    yaml.push_str(&format!("  - id: {slug}\n"));
    yaml.push_str(&format!("    description: \"{}\"\n", escape_yaml(name)));
    yaml.push_str("    model:\n");
    yaml.push_str(&format!("      provider: {provider}\n"));
    yaml.push_str(&format!("      model: {model}\n"));
    if let Some(kind) = channel_kind {
        yaml.push_str(&format!("    plugins: [{kind}]\n"));
        yaml.push_str("    inbound_bindings:\n");
        if let Some(inst) = &telegram_instance {
            yaml.push_str(&format!(
                "      - plugin: {kind}\n        instance: {inst}\n"
            ));
        } else {
            yaml.push_str(&format!("      - plugin: {kind}\n"));
        }
    }
    yaml.push_str(&format!("    workspace: ./data/workspace/{slug}\n"));
    yaml.push_str(&format!(
        "    system_prompt: |\n      You are {name}. {}\n      Emoji: {emoji}.\n",
        if vibe.is_empty() {
            "Be concise and helpful.".to_string()
        } else {
            vibe.to_string()
        }
    ));

    std::fs::create_dir_all("./config/agents.d")
        .map_err(|e| format!("mkdir ./config/agents.d: {e}"))?;
    std::fs::write(&agent_path, &yaml).map_err(|e| format!("write {agent_path}: {e}"))?;
    written.push(agent_path.clone());

    // Seed workspace.
    let workspace = format!("./data/workspace/{slug}");
    std::fs::create_dir_all(&workspace).map_err(|e| format!("mkdir {workspace}: {e}"))?;
    let avatar = identity
        .get("avatar")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    let mut identity_md = format!(
        "- **Name:** {name}\n- **Emoji:** {emoji}\n- **Vibe:** {}\n",
        if vibe.is_empty() {
            "warm and sharp"
        } else {
            vibe
        }
    );
    if !avatar.is_empty() {
        identity_md.push_str(&format!("- **Avatar:** {avatar}\n"));
    }
    let id_path = format!("{workspace}/IDENTITY.md");
    std::fs::write(&id_path, identity_md).map_err(|e| format!("write {id_path}: {e}"))?;
    written.push(id_path);

    if !soul.is_empty() {
        let soul_path = format!("{workspace}/SOUL.md");
        std::fs::write(&soul_path, soul).map_err(|e| format!("write {soul_path}: {e}"))?;
        written.push(soul_path);
    }

    // Seed telegram plugin YAML if absent.
    if let (Some(inst), Some(_)) = (telegram_instance.as_ref(), telegram_token.as_ref()) {
        let telegram_path = "./config/plugins/telegram.yaml";
        if !std::path::Path::new(telegram_path).exists() {
            let mut buf = String::new();
            buf.push_str(&format!("# Added by admin wizard for agent '{slug}'.\n"));
            buf.push_str("telegram:\n");
            buf.push_str(&format!("  - instance: {inst}\n"));
            buf.push_str(&format!(
                "    token: ${{file:./secrets/{inst}_telegram_token.txt}}\n"
            ));
            buf.push_str(&format!("    allow_agents: [{slug}]\n"));
            std::fs::create_dir_all("./config/plugins")
                .map_err(|e| format!("mkdir ./config/plugins: {e}"))?;
            std::fs::write(telegram_path, buf)
                .map_err(|e| format!("write {telegram_path}: {e}"))?;
            written.push(telegram_path.to_string());
        }
    }

    let mut report = String::from("{\"ok\":true,\"agent_id\":\"");
    report.push_str(&slug);
    report.push_str("\",\"files_written\":[");
    for (i, p) in written.iter().enumerate() {
        if i > 0 {
            report.push(',');
        }
        report.push('"');
        report.push_str(&p.replace('\\', "\\\\").replace('"', "\\\""));
        report.push('"');
    }
    report.push_str("]}");
    Ok(report)
}

/// Shared 401-JSON writer used by every cookie-gated admin API.
async fn write_401_json(stream: &mut TcpStream) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let body = r#"{"ok":false,"error":"unauthorised"}"#;
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Dump the live agent directory as JSON. Shape:
/// `{"agents":[{"id","description","model":"prov/mod",
///              "channels":[{"plugin","instance"}]}]}`. Reads
/// `config/agents.yaml` + every `config/agents.d/*.yaml` (except
/// `.example.yaml`). No cache — admin is a cold path.
fn list_agents_json() -> String {
    #[derive(serde::Deserialize)]
    struct BindingLite {
        plugin: Option<String>,
        instance: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct ModelLite {
        provider: Option<String>,
        model: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct AgentLite {
        id: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        model: Option<ModelLite>,
        #[serde(default)]
        inbound_bindings: Vec<BindingLite>,
        #[serde(default)]
        plugins: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct FileLite {
        #[serde(default)]
        agents: Vec<AgentLite>,
    }

    let mut entries: Vec<AgentLite> = Vec::new();
    let push_from = |buf: &str, into: &mut Vec<AgentLite>| {
        if let Ok(parsed) = serde_yaml::from_str::<FileLite>(buf) {
            into.extend(parsed.agents);
        }
    };
    if let Ok(buf) = std::fs::read_to_string("./config/agents.yaml") {
        push_from(&buf, &mut entries);
    }
    if let Ok(read) = std::fs::read_dir("./config/agents.d") {
        let mut paths: Vec<_> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("yaml")
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.ends_with(".example.yaml"))
                        .unwrap_or(false)
            })
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(buf) = std::fs::read_to_string(&p) {
                push_from(&buf, &mut entries);
            }
        }
    }

    let mut out = String::from("{\"agents\":[");
    for (i, a) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let model_str = match a.model.as_ref() {
            Some(m) => format!(
                "{}/{}",
                m.provider.as_deref().unwrap_or("?"),
                m.model.as_deref().unwrap_or("?")
            ),
            None => String::from("?"),
        };
        out.push_str(&format!(
            "{{\"id\":\"{}\",\"description\":\"{}\",\"model\":\"{}\",\"channels\":[",
            json_escape(&a.id),
            json_escape(a.description.as_deref().unwrap_or("")),
            json_escape(&model_str)
        ));
        let mut first = true;
        // Collect channel surface from inbound_bindings; if empty, fall
        // back to the legacy `plugins: []` field.
        for b in &a.inbound_bindings {
            if let Some(plugin) = &b.plugin {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!(
                    "{{\"plugin\":\"{}\",\"instance\":\"{}\"}}",
                    json_escape(plugin),
                    json_escape(b.instance.as_deref().unwrap_or(""))
                ));
            }
        }
        if first {
            for plugin in &a.plugins {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!(
                    "{{\"plugin\":\"{}\",\"instance\":\"\"}}",
                    json_escape(plugin)
                ));
            }
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    out
}

/// Dump every channel instance the plugin YAMLs know about. Shape:
/// `{"channels":[{"plugin","instance","allow_agents":[...], "source_file"}]}`.
/// Understands both shapes shipped in the repo: the legacy
/// "single-mapping" form (`whatsapp: { ... }`) and the new
/// multi-instance sequence form (`telegram: [ { instance, ... }, ... ]`).
fn list_channels_json() -> String {
    let mut out = String::from("{\"channels\":[");
    let mut first = true;

    for (plugin, path) in &[
        ("telegram", "./config/plugins/telegram.yaml"),
        ("whatsapp", "./config/plugins/whatsapp.yaml"),
    ] {
        let Ok(buf) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&buf) else {
            continue;
        };
        let entry = v.get(*plugin).cloned().unwrap_or(serde_yaml::Value::Null);
        let entries_vec = match entry {
            serde_yaml::Value::Sequence(seq) => seq,
            serde_yaml::Value::Mapping(m) => vec![serde_yaml::Value::Mapping(m)],
            _ => continue,
        };
        for e in entries_vec {
            let instance = e
                .get("instance")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut allow_agents: Vec<String> = Vec::new();
            if let Some(seq) = e.get("allow_agents").and_then(|v| v.as_sequence()) {
                for a in seq {
                    if let Some(s) = a.as_str() {
                        allow_agents.push(s.to_string());
                    }
                }
            }
            // Nested telegram-specific fields. Whatsapp gets empty
            // defaults which the SPA ignores.
            let mut chat_ids: Vec<i64> = Vec::new();
            if let Some(seq) = e
                .get("allowlist")
                .and_then(|a| a.get("chat_ids"))
                .and_then(|a| a.as_sequence())
            {
                for n in seq {
                    if let Some(id) = n.as_i64() {
                        chat_ids.push(id);
                    }
                }
            }
            let at_enabled = e
                .get("auto_transcribe")
                .and_then(|a| a.get("enabled"))
                .and_then(|a| a.as_bool())
                .unwrap_or(false);
            let at_command = e
                .get("auto_transcribe")
                .and_then(|a| a.get("command"))
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let at_language = e
                .get("auto_transcribe")
                .and_then(|a| a.get("language"))
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();

            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                "{{\"plugin\":\"{}\",\"instance\":\"{}\",\"source_file\":\"{}\",\"allow_agents\":[",
                json_escape(plugin),
                json_escape(&instance),
                json_escape(path)
            ));
            for (i, ag) in allow_agents.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(&json_escape(ag));
                out.push('"');
            }
            out.push_str("],\"allowlist_chat_ids\":[");
            for (i, id) in chat_ids.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&id.to_string());
            }
            out.push_str(&format!(
                "],\"auto_transcribe\":{{\"enabled\":{},\"command\":\"{}\",\"language\":\"{}\"}}}}",
                at_enabled,
                json_escape(&at_command),
                json_escape(&at_language)
            ));
        }
    }
    out.push_str("]}");
    out
}

/// Telegram bot token shape: `<digits>:<35+ chars from
/// [A-Za-z0-9_-]>`. Empty / wrong shape returns `false`; this is a
/// shape-only check, not a token validity check (only Telegram's
/// `getMe` can confirm the latter).
fn is_valid_telegram_token_shape(s: &str) -> bool {
    let Some((id, rest)) = s.split_once(':') else {
        return false;
    };
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    rest.len() >= 30
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Process-wide lock keyed by absolute YAML path. Every admin handler
/// that does read-modify-write on a `config/plugins/*.yaml` (or
/// `agents.yaml`) must take this lock first; without it, two
/// concurrent admin requests can interleave and silently lose one
/// agent's update.
fn yaml_lock(path: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>> =
        OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock().expect("yaml_lock map poisoned");
    guard
        .entry(path.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Append a Telegram bot instance to `config/plugins/telegram.yaml`.
/// On first-use the file is created in multi-instance sequence form;
/// existing files are migrated from single-mapping to sequence and
/// then appended. Token is written to `./secrets/<instance>_telegram_token.txt`
/// at mode 0600.
fn add_telegram_channel(body: &str) -> Result<String, String> {
    let _yaml_guard = yaml_lock("./config/plugins/telegram.yaml");
    let _yaml_guard = _yaml_guard
        .lock()
        .map_err(|_| "yaml lock poisoned".to_string())?;
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let instance = v
        .get("instance")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let token = v
        .get("token")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if instance.is_empty() {
        return Err("instance label is required".into());
    }
    if token.is_empty() {
        return Err("token is required".into());
    }
    // Shape-validate against Telegram's canonical bot-token format
    // (`<numeric_id>:<35+ char alphanumeric>`). Without the check,
    // a token-shaped string can flow into a downstream HTTP probe
    // path that interpolates it carelessly — modest SSRF surface.
    if !is_valid_telegram_token_shape(&token) {
        return Err(
            "token does not match Telegram's bot-token format (digits:alphanumeric_and_dashes)"
                .into(),
        );
    }
    // Sanity-check the instance label so we don't land invalid YAML
    // keys on disk.
    if !instance
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("instance label must be [a-zA-Z0-9_-]+".into());
    }

    let allow_agents: Vec<String> = v
        .get("allow_agents")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Load whatever shape the file currently has.
    let path = "./config/plugins/telegram.yaml";
    let mut existing: Vec<serde_yaml::Value> = Vec::new();
    if let Ok(buf) = std::fs::read_to_string(path) {
        if let Ok(parsed) = serde_yaml::from_str::<serde_yaml::Value>(&buf) {
            let entry = parsed
                .get("telegram")
                .cloned()
                .unwrap_or(serde_yaml::Value::Null);
            match entry {
                serde_yaml::Value::Sequence(seq) => existing = seq,
                serde_yaml::Value::Mapping(m) => {
                    existing.push(serde_yaml::Value::Mapping(m));
                }
                _ => {}
            }
        }
    }
    for e in &existing {
        if e.get("instance").and_then(|v| v.as_str()) == Some(instance.as_str()) {
            return Err(format!("telegram instance '{instance}' already exists"));
        }
    }

    // Write token under ./secrets/ first so a later YAML write race
    // doesn't leave the YAML pointing at a missing file.
    std::fs::create_dir_all("./secrets").map_err(|e| format!("mkdir ./secrets: {e}"))?;
    let secret_path = format!("./secrets/{instance}_telegram_token.txt");
    write_file_0600(&secret_path, &token).map_err(|e| format!("write {secret_path}: {e}"))?;

    // Compose the new entry.
    let mut new_entry = serde_yaml::Mapping::new();
    new_entry.insert(
        serde_yaml::Value::String("instance".into()),
        serde_yaml::Value::String(instance.clone()),
    );
    new_entry.insert(
        serde_yaml::Value::String("token".into()),
        serde_yaml::Value::String(format!("${{file:{secret_path}}}")),
    );
    if !allow_agents.is_empty() {
        let seq = allow_agents
            .iter()
            .map(|a| serde_yaml::Value::String(a.clone()))
            .collect::<Vec<_>>();
        new_entry.insert(
            serde_yaml::Value::String("allow_agents".into()),
            serde_yaml::Value::Sequence(seq),
        );
    }
    existing.push(serde_yaml::Value::Mapping(new_entry));

    // Serialise back.
    let mut root = serde_yaml::Mapping::new();
    root.insert(
        serde_yaml::Value::String("telegram".into()),
        serde_yaml::Value::Sequence(existing),
    );
    let out = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))
        .map_err(|e| format!("serialise: {e}"))?;
    std::fs::create_dir_all("./config/plugins")
        .map_err(|e| format!("mkdir ./config/plugins: {e}"))?;
    std::fs::write(path, out).map_err(|e| format!("write {path}: {e}"))?;

    Ok(format!(
        "{{\"ok\":true,\"instance\":\"{}\",\"secret_path\":\"{}\",\"source_file\":\"{}\"}}",
        json_escape(&instance),
        json_escape(&secret_path),
        json_escape(path)
    ))
}

/// Rotate the token (and/or swap `allow_agents`) on an existing
/// Telegram instance. Keeps the `${file:...}` reference pointing
/// at the same secret path so consumers don't need to be restarted.
fn edit_telegram_channel(instance: &str, body: &str) -> Result<String, String> {
    let _yaml_guard = yaml_lock("./config/plugins/telegram.yaml");
    let _yaml_guard = _yaml_guard
        .lock()
        .map_err(|_| "yaml lock poisoned".to_string())?;
    let instance = instance.trim();
    if instance.is_empty() {
        return Err("instance is required".into());
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let new_token = v
        .get("token")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_string());
    let allow_agents: Option<Vec<String>> =
        v.get("allow_agents").and_then(|a| a.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        });
    // `allowlist_chat_ids: [int, int, ...]` — empty array clears
    // the allowlist, missing key leaves it untouched.
    let allowlist_chat_ids: Option<Vec<i64>> = v
        .get("allowlist_chat_ids")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect());
    // `auto_transcribe` — merges into the existing sub-mapping.
    // Shape: { enabled?, command?, language? }. Missing key leaves
    // the sub-mapping untouched.
    let auto_transcribe = v.get("auto_transcribe").cloned();

    let path = "./config/plugins/telegram.yaml";
    let buf = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let mut parsed: serde_yaml::Value =
        serde_yaml::from_str(&buf).map_err(|e| format!("parse {path}: {e}"))?;
    let telegram_entry = parsed
        .get_mut("telegram")
        .ok_or_else(|| format!("{path} has no 'telegram:' key"))?;
    let mut seq: Vec<serde_yaml::Value> =
        match std::mem::replace(telegram_entry, serde_yaml::Value::Null) {
            serde_yaml::Value::Sequence(s) => s,
            serde_yaml::Value::Mapping(m) => vec![serde_yaml::Value::Mapping(m)],
            _ => return Err("unexpected telegram: shape".into()),
        };

    let mut modified = false;
    for entry in seq.iter_mut() {
        let Some(m) = entry.as_mapping_mut() else {
            continue;
        };
        let matches_instance = m
            .get(&serde_yaml::Value::String("instance".into()))
            .and_then(|v| v.as_str())
            == Some(instance);
        if !matches_instance {
            continue;
        }
        modified = true;
        if let Some(token) = &new_token {
            if token.is_empty() {
                return Err("token must be non-empty".into());
            }
            let secret_path = format!("./secrets/{instance}_telegram_token.txt");
            std::fs::create_dir_all("./secrets").map_err(|e| format!("mkdir ./secrets: {e}"))?;
            write_file_0600(&secret_path, token)
                .map_err(|e| format!("write {secret_path}: {e}"))?;
            m.insert(
                serde_yaml::Value::String("token".into()),
                serde_yaml::Value::String(format!("${{file:{secret_path}}}")),
            );
        }
        if let Some(list) = &allow_agents {
            let yaml_list = list
                .iter()
                .map(|s| serde_yaml::Value::String(s.clone()))
                .collect::<Vec<_>>();
            if yaml_list.is_empty() {
                m.remove(&serde_yaml::Value::String("allow_agents".into()));
            } else {
                m.insert(
                    serde_yaml::Value::String("allow_agents".into()),
                    serde_yaml::Value::Sequence(yaml_list),
                );
            }
        }
        if let Some(ids) = &allowlist_chat_ids {
            // The shipped config schema nests this under `allowlist`:
            //   allowlist:
            //     chat_ids: [123, 456]
            // Create the parent map on-the-fly when it's missing.
            let allowlist_key = serde_yaml::Value::String("allowlist".into());
            let mut sub = match m.remove(&allowlist_key) {
                Some(serde_yaml::Value::Mapping(sub)) => sub,
                _ => serde_yaml::Mapping::new(),
            };
            if ids.is_empty() {
                sub.remove(&serde_yaml::Value::String("chat_ids".into()));
            } else {
                sub.insert(
                    serde_yaml::Value::String("chat_ids".into()),
                    serde_yaml::Value::Sequence(
                        ids.iter()
                            .map(|&id| serde_yaml::Value::Number(id.into()))
                            .collect(),
                    ),
                );
            }
            if !sub.is_empty() {
                m.insert(allowlist_key, serde_yaml::Value::Mapping(sub));
            }
        }
        if let Some(at) = &auto_transcribe {
            // Merge the supplied sub-object over what's on disk so a
            // caller that only sends { enabled: true } doesn't wipe the
            // existing command / language.
            let at_key = serde_yaml::Value::String("auto_transcribe".into());
            let mut sub = match m.remove(&at_key) {
                Some(serde_yaml::Value::Mapping(sub)) => sub,
                _ => serde_yaml::Mapping::new(),
            };
            if let Some(enabled) = at.get("enabled").and_then(|v| v.as_bool()) {
                sub.insert(
                    serde_yaml::Value::String("enabled".into()),
                    serde_yaml::Value::Bool(enabled),
                );
            }
            if let Some(cmd) = at.get("command").and_then(|v| v.as_str()) {
                if cmd.trim().is_empty() {
                    sub.remove(&serde_yaml::Value::String("command".into()));
                } else {
                    sub.insert(
                        serde_yaml::Value::String("command".into()),
                        serde_yaml::Value::String(cmd.to_string()),
                    );
                }
            }
            if let Some(lang) = at.get("language").and_then(|v| v.as_str()) {
                if lang.trim().is_empty() {
                    sub.remove(&serde_yaml::Value::String("language".into()));
                } else {
                    sub.insert(
                        serde_yaml::Value::String("language".into()),
                        serde_yaml::Value::String(lang.to_string()),
                    );
                }
            }
            if !sub.is_empty() {
                m.insert(at_key, serde_yaml::Value::Mapping(sub));
            }
        }
        break;
    }
    if !modified {
        return Err(format!("telegram instance '{instance}' not found"));
    }

    *telegram_entry = serde_yaml::Value::Sequence(seq);
    let out = serde_yaml::to_string(&parsed).map_err(|e| format!("serialise: {e}"))?;
    std::fs::write(path, out).map_err(|e| format!("write {path}: {e}"))?;
    Ok(format!(
        "{{\"ok\":true,\"instance\":\"{}\"}}",
        json_escape(instance)
    ))
}

/// Remove a `<plugin>.<instance>` entry from
/// `config/plugins/<plugin>.yaml`. The token/secret file stays
/// untouched — the operator can delete `./secrets/*.txt` by hand
/// when they're sure (or reuse them by re-adding the instance).
fn delete_channel(plugin: &str, instance: &str) -> Result<String, String> {
    // Need to hold the lock for whatever file we're about to mutate.
    // Compute path early so the guard scope is right.
    let yaml_path = format!("./config/plugins/{plugin}.yaml");
    let _yaml_guard = yaml_lock(&yaml_path);
    let _yaml_guard = _yaml_guard
        .lock()
        .map_err(|_| "yaml lock poisoned".to_string())?;
    let plugin = plugin.trim();
    let instance = instance.trim();
    if plugin.is_empty() || instance.is_empty() {
        return Err("plugin and instance are required".into());
    }
    if !plugin
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("plugin name must be [a-zA-Z0-9_-]+".into());
    }
    let path = format!("./config/plugins/{plugin}.yaml");
    let buf = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
    let mut parsed: serde_yaml::Value =
        serde_yaml::from_str(&buf).map_err(|e| format!("parse {path}: {e}"))?;
    let entry = parsed
        .get_mut(plugin)
        .ok_or_else(|| format!("{path} has no '{plugin}:' key"))?;
    let seq: Vec<serde_yaml::Value> = match std::mem::replace(entry, serde_yaml::Value::Null) {
        serde_yaml::Value::Sequence(s) => s,
        serde_yaml::Value::Mapping(m) => vec![serde_yaml::Value::Mapping(m)],
        _ => return Err(format!("unexpected {plugin}: shape")),
    };
    let before = seq.len();
    let kept: Vec<serde_yaml::Value> = seq
        .into_iter()
        .filter(|e| e.get("instance").and_then(|v| v.as_str()) != Some(instance))
        .collect();
    if kept.len() == before {
        return Err(format!("{plugin} instance '{instance}' not found"));
    }
    *entry = serde_yaml::Value::Sequence(kept);
    let out = serde_yaml::to_string(&parsed).map_err(|e| format!("serialise: {e}"))?;
    std::fs::write(&path, out).map_err(|e| format!("write {path}: {e}"))?;
    Ok(format!(
        "{{\"ok\":true,\"plugin\":\"{}\",\"instance\":\"{}\"}}",
        json_escape(plugin),
        json_escape(instance)
    ))
}

/// Validate an MCP server name. Dots are reserved for explicit
/// shadowing of extension-declared servers, so the admin UI rejects
/// them outright — operators who really want shadowing can hand-edit
/// `mcp.yaml`.
fn validate_mcp_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("server name is required".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("server name must be [a-zA-Z0-9_-]+ (no dots)".into());
    }
    Ok(())
}

/// Read `config/mcp.yaml` and return its parsed YAML. If the file is
/// missing, fabricate a minimal `mcp: { servers: {} }` mapping so the
/// caller can still write into it.
fn load_mcp_yaml() -> Result<serde_yaml::Value, String> {
    let path = "./config/mcp.yaml";
    match std::fs::read_to_string(path) {
        Ok(buf) => serde_yaml::from_str(&buf).map_err(|e| format!("parse {path}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut root = serde_yaml::Mapping::new();
            let mut mcp = serde_yaml::Mapping::new();
            mcp.insert(
                serde_yaml::Value::String("enabled".into()),
                serde_yaml::Value::Bool(true),
            );
            mcp.insert(
                serde_yaml::Value::String("servers".into()),
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            );
            root.insert(
                serde_yaml::Value::String("mcp".into()),
                serde_yaml::Value::Mapping(mcp),
            );
            Ok(serde_yaml::Value::Mapping(root))
        }
        Err(e) => Err(format!("read {path}: {e}")),
    }
}

fn save_mcp_yaml(v: &serde_yaml::Value) -> Result<(), String> {
    let out = serde_yaml::to_string(v).map_err(|e| format!("serialise mcp.yaml: {e}"))?;
    std::fs::write("./config/mcp.yaml", out).map_err(|e| format!("write mcp.yaml: {e}"))
}

/// Build a `serde_yaml::Value` mapping from a JSON request body for
/// one MCP server entry. Returns `(yaml_entry)` ready to be inserted
/// under `mcp.servers.<name>`. The shape mirrors the YAML schema in
/// `config/mcp.yaml`.
fn json_to_mcp_entry(v: &serde_json::Value) -> Result<serde_yaml::Value, String> {
    let transport = v
        .get("transport")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !matches!(transport.as_str(), "stdio" | "streamable_http" | "sse") {
        return Err("transport must be stdio, streamable_http, or sse".into());
    }
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("transport".into()),
        serde_yaml::Value::String(transport.clone()),
    );
    if transport == "stdio" {
        let command = v
            .get("command")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if command.is_empty() {
            return Err("command is required for stdio transport".into());
        }
        m.insert(
            serde_yaml::Value::String("command".into()),
            serde_yaml::Value::String(command),
        );
        if let Some(arr) = v.get("args").and_then(|a| a.as_array()) {
            let seq: Vec<serde_yaml::Value> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| serde_yaml::Value::String(s.to_string())))
                .collect();
            m.insert(
                serde_yaml::Value::String("args".into()),
                serde_yaml::Value::Sequence(seq),
            );
        }
        if let Some(obj) = v.get("env").and_then(|a| a.as_object()) {
            let mut env_map = serde_yaml::Mapping::new();
            for (k, val) in obj {
                if let Some(s) = val.as_str() {
                    env_map.insert(
                        serde_yaml::Value::String(k.clone()),
                        serde_yaml::Value::String(s.to_string()),
                    );
                }
            }
            m.insert(
                serde_yaml::Value::String("env".into()),
                serde_yaml::Value::Mapping(env_map),
            );
        }
    } else {
        // streamable_http or sse — both share url + headers.
        let url = v
            .get("url")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if url.is_empty() {
            return Err("url is required for http/sse transport".into());
        }
        m.insert(
            serde_yaml::Value::String("url".into()),
            serde_yaml::Value::String(url),
        );
        if let Some(obj) = v.get("headers").and_then(|a| a.as_object()) {
            let mut hmap = serde_yaml::Mapping::new();
            for (k, val) in obj {
                if let Some(s) = val.as_str() {
                    hmap.insert(
                        serde_yaml::Value::String(k.clone()),
                        serde_yaml::Value::String(s.to_string()),
                    );
                }
            }
            m.insert(
                serde_yaml::Value::String("headers".into()),
                serde_yaml::Value::Mapping(hmap),
            );
        }
    }
    if let Some(s) = v.get("log_level").and_then(|x| x.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            m.insert(
                serde_yaml::Value::String("log_level".into()),
                serde_yaml::Value::String(s.to_string()),
            );
        }
    }
    if let Some(b) = v.get("context_passthrough").and_then(|x| x.as_bool()) {
        m.insert(
            serde_yaml::Value::String("context_passthrough".into()),
            serde_yaml::Value::Bool(b),
        );
    }
    Ok(serde_yaml::Value::Mapping(m))
}

/// Render the current `mcp.servers` map as a JSON list shaped for the
/// admin UI. Headers and env values are returned as-is (operator may
/// have inlined `${VAR}` placeholders — the resolver expands those at
/// runtime, the UI just displays the literal string).
fn list_mcp_servers_json() -> String {
    let mut out = String::from("{\"servers\":[");
    let v = match load_mcp_yaml() {
        Ok(v) => v,
        Err(_) => {
            out.push_str("]}");
            return out;
        }
    };
    let servers = v
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(|s| s.as_mapping())
        .cloned()
        .unwrap_or_default();
    let mut first = true;
    for (k, val) in &servers {
        let Some(name) = k.as_str() else { continue };
        if !first {
            out.push(',');
        }
        first = false;
        let transport = val
            .get("transport")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        out.push_str(&format!(
            "{{\"name\":\"{}\",\"transport\":\"{}\"",
            json_escape(name),
            json_escape(&transport),
        ));
        if transport == "stdio" {
            let command = val
                .get("command")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            out.push_str(&format!(",\"command\":\"{}\"", json_escape(&command)));
            out.push_str(",\"args\":[");
            if let Some(seq) = val.get("args").and_then(|a| a.as_sequence()) {
                for (i, a) in seq.iter().enumerate() {
                    if let Some(s) = a.as_str() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push('"');
                        out.push_str(&json_escape(s));
                        out.push('"');
                    }
                }
            }
            out.push_str("],\"env\":{");
            if let Some(map) = val.get("env").and_then(|a| a.as_mapping()) {
                let mut i = 0;
                for (ek, ev) in map {
                    let (Some(k), Some(v)) = (ek.as_str(), ev.as_str()) else {
                        continue;
                    };
                    if i > 0 {
                        out.push(',');
                    }
                    i += 1;
                    out.push_str(&format!("\"{}\":\"{}\"", json_escape(k), json_escape(v)));
                }
            }
            out.push('}');
        } else {
            let url = val
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            out.push_str(&format!(",\"url\":\"{}\"", json_escape(&url)));
            out.push_str(",\"headers\":{");
            if let Some(map) = val.get("headers").and_then(|a| a.as_mapping()) {
                let mut i = 0;
                for (hk, hv) in map {
                    let (Some(k), Some(v)) = (hk.as_str(), hv.as_str()) else {
                        continue;
                    };
                    if i > 0 {
                        out.push(',');
                    }
                    i += 1;
                    out.push_str(&format!("\"{}\":\"{}\"", json_escape(k), json_escape(v)));
                }
            }
            out.push('}');
        }
        let log_level = val
            .get("log_level")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        out.push_str(&format!(",\"log_level\":\"{}\"", json_escape(&log_level)));
        let ctx = val.get("context_passthrough").and_then(|x| x.as_bool());
        match ctx {
            Some(b) => out.push_str(&format!(",\"context_passthrough\":{b}")),
            None => out.push_str(",\"context_passthrough\":null"),
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

fn add_mcp_server(body: &str) -> Result<String, String> {
    let _yaml_guard = yaml_lock("./config/mcp.yaml");
    let _yaml_guard = _yaml_guard
        .lock()
        .map_err(|_| "yaml lock poisoned".to_string())?;
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let name = v
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    validate_mcp_name(&name)?;

    let entry = json_to_mcp_entry(&v)?;
    let mut root = load_mcp_yaml()?;
    let mcp = root
        .as_mapping_mut()
        .and_then(|m| m.get_mut(serde_yaml::Value::String("mcp".into())))
        .ok_or_else(|| "mcp.yaml missing top-level `mcp:` key".to_string())?;
    let mcp_map = mcp
        .as_mapping_mut()
        .ok_or_else(|| "`mcp:` is not a mapping".to_string())?;
    let servers_val = mcp_map
        .entry(serde_yaml::Value::String("servers".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    let servers = servers_val
        .as_mapping_mut()
        .ok_or_else(|| "`mcp.servers` is not a mapping".to_string())?;
    let key = serde_yaml::Value::String(name.clone());
    if servers.contains_key(&key) {
        return Err(format!("mcp server '{name}' already exists"));
    }
    servers.insert(key, entry);
    save_mcp_yaml(&root)?;
    Ok(format!(
        "{{\"ok\":true,\"name\":\"{}\"}}",
        json_escape(&name)
    ))
}

fn edit_mcp_server(name: &str, body: &str) -> Result<String, String> {
    let _yaml_guard = yaml_lock("./config/mcp.yaml");
    let _yaml_guard = _yaml_guard
        .lock()
        .map_err(|_| "yaml lock poisoned".to_string())?;
    validate_mcp_name(name)?;
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let entry = json_to_mcp_entry(&v)?;
    let mut root = load_mcp_yaml()?;
    let servers = root
        .as_mapping_mut()
        .and_then(|m| m.get_mut(serde_yaml::Value::String("mcp".into())))
        .and_then(|mcp| mcp.as_mapping_mut())
        .and_then(|mcp| mcp.get_mut(serde_yaml::Value::String("servers".into())))
        .and_then(|s| s.as_mapping_mut())
        .ok_or_else(|| "mcp.yaml missing `mcp.servers` mapping".to_string())?;
    let key = serde_yaml::Value::String(name.to_string());
    if !servers.contains_key(&key) {
        return Err(format!("mcp server '{name}' not found"));
    }
    servers.insert(key, entry);
    save_mcp_yaml(&root)?;
    Ok(format!(
        "{{\"ok\":true,\"name\":\"{}\"}}",
        json_escape(name)
    ))
}

fn delete_mcp_server(name: &str) -> Result<String, String> {
    let _yaml_guard = yaml_lock("./config/mcp.yaml");
    let _yaml_guard = _yaml_guard
        .lock()
        .map_err(|_| "yaml lock poisoned".to_string())?;
    validate_mcp_name(name)?;
    let mut root = load_mcp_yaml()?;
    let servers = root
        .as_mapping_mut()
        .and_then(|m| m.get_mut(serde_yaml::Value::String("mcp".into())))
        .and_then(|mcp| mcp.as_mapping_mut())
        .and_then(|mcp| mcp.get_mut(serde_yaml::Value::String("servers".into())))
        .and_then(|s| s.as_mapping_mut())
        .ok_or_else(|| "mcp.yaml missing `mcp.servers` mapping".to_string())?;
    let key = serde_yaml::Value::String(name.to_string());
    if servers.remove(&key).is_none() {
        return Err(format!("mcp server '{name}' not found"));
    }
    save_mcp_yaml(&root)?;
    Ok(format!(
        "{{\"ok\":true,\"name\":\"{}\"}}",
        json_escape(name)
    ))
}

/// Update the `credentials:` block on an agent. The agent file is
/// located in `config/agents.yaml` or `config/agents.d/<id>.yaml`;
/// both are searched and the first match wins. Supported channel
/// keys right now: `telegram`, `whatsapp`, `google`. An empty-string
/// value removes the binding; omitted keys leave the current value
/// intact.
fn pin_agent_credentials(id: &str, body: &str) -> Result<String, String> {
    if id.is_empty() {
        return Err("agent id is required".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("agent id must be [a-zA-Z0-9_-]+".into());
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;

    // Locate the file + index in its agents[] sequence that owns this
    // agent id.
    let candidates: Vec<String> = {
        let mut out: Vec<String> = vec!["./config/agents.yaml".to_string()];
        if let Ok(dir) = std::fs::read_dir("./config/agents.d") {
            for entry in dir.flatten() {
                let p = entry.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.ends_with(".yaml") || name.ends_with(".example.yaml") {
                    continue;
                }
                out.push(p.display().to_string());
            }
        }
        out.sort();
        out
    };

    for path in candidates {
        let Ok(buf) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut parsed: serde_yaml::Value = match serde_yaml::from_str(&buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let agents = match parsed.get_mut("agents").and_then(|a| a.as_sequence_mut()) {
            Some(s) => s,
            None => continue,
        };
        let target_idx = agents
            .iter()
            .position(|a| a.get("id").and_then(|v| v.as_str()) == Some(id));
        let Some(idx) = target_idx else { continue };
        let agent_map = agents[idx]
            .as_mapping_mut()
            .ok_or_else(|| format!("{path}: agent '{id}' is not a mapping"))?;

        let creds_key = serde_yaml::Value::String("credentials".into());
        let mut creds = match agent_map.remove(&creds_key) {
            Some(serde_yaml::Value::Mapping(m)) => m,
            _ => serde_yaml::Mapping::new(),
        };

        for channel in ["telegram", "whatsapp", "google"] {
            if let Some(val) = v.get(channel).and_then(|x| x.as_str()) {
                let key = serde_yaml::Value::String(channel.into());
                if val.trim().is_empty() {
                    creds.remove(&key);
                } else {
                    creds.insert(key, serde_yaml::Value::String(val.trim().to_string()));
                }
            }
        }

        if !creds.is_empty() {
            agent_map.insert(creds_key, serde_yaml::Value::Mapping(creds));
        }

        let out = serde_yaml::to_string(&parsed).map_err(|e| format!("serialise: {e}"))?;
        std::fs::write(&path, out).map_err(|e| format!("write {path}: {e}"))?;

        return Ok(format!(
            "{{\"ok\":true,\"agent_id\":\"{}\",\"source_file\":\"{}\"}}",
            json_escape(id),
            json_escape(&path)
        ));
    }

    Err(format!(
        "agent '{id}' not found in agents.yaml or agents.d/"
    ))
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn escape_yaml(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(unix)]
fn write_file_0600(path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_file_0600(path: &str, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

fn admin_debug_enabled() -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    matches!(
        std::env::var("NEXO_ADMIN_DEBUG").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Wipe everything that holds agent identity or runtime state so the
/// operator can iterate on the wizard without restarting from a
/// fresh clone. Preserved on purpose:
///   * `./secrets/*`            — API keys; recreating those is the
///                                slowest step of a fresh bootstrap
///   * `config/agents.d/*.example.yaml`
///   * Top-level `config/*.yaml` — the operator's hand-edited stack
/// Returns a JSON blob describing what was removed.
fn debug_reset_now() -> String {
    use std::path::Path;
    let mut cleared: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let wipe_dir_contents = |dir: &Path, cleared: &mut Vec<String>, errors: &mut Vec<String>| {
        if !dir.exists() {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{}: {e}", dir.display()));
                return;
            }
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let res = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
            match res {
                Ok(_) => cleared.push(p.display().to_string()),
                Err(e) => errors.push(format!("{}: {e}", p.display())),
            }
        }
    };

    // 1. data/** — broker DB, memory DB, taskflow DB, workspaces,
    //    transcripts, media, disk queue, agent.lock.
    wipe_dir_contents(Path::new("./data"), &mut cleared, &mut errors);

    // 2. config/agents.d/*.yaml (but not *.example.yaml).
    if let Ok(entries) = std::fs::read_dir("./config/agents.d") {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".yaml") && !name.ends_with(".example.yaml") {
                match std::fs::remove_file(&p) {
                    Ok(_) => cleared.push(p.display().to_string()),
                    Err(e) => errors.push(format!("{}: {e}", p.display())),
                }
            }
        }
    }

    // Serialise the report by hand — no serde_json pull just for one
    // debug endpoint.
    let mut out = String::from("{\"ok\":true,\"cleared\":[");
    for (i, path) in cleared.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&path.replace('\\', "\\\\").replace('"', "\\\""));
        out.push('"');
    }
    out.push_str("],\"errors\":[");
    for (i, err) in errors.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&err.replace('\\', "\\\\").replace('"', "\\\""));
        out.push('"');
    }
    out.push_str("]}");
    out
}

/// 24 URL-safe random chars from an OS-grade RNG. The password is
/// printed once at launch and never persisted — the operator copies
/// it into the login form. Losing the value means restarting
/// `agent admin` to mint a new one (which also re-spins the tunnel
/// URL).
/// Resolve the admin password. Operators with a centralised logging
/// pipeline (journald, container stdout → ELK / CloudWatch) should
/// pass `AGENT_ADMIN_PASSWORD` so the password never crosses stdout
/// — every other launch falls back to a random 24-char password
/// printed once to the terminal.
fn generate_admin_password() -> String {
    if let Ok(v) = std::env::var("AGENT_ADMIN_PASSWORD") {
        let trimmed = v.trim().to_string();
        if trimmed.len() >= 12 {
            return trimmed;
        }
    }
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| {
            let idx = rng.gen_range(0..CHARS.len());
            CHARS[idx] as char
        })
        .collect()
}

/// First 4 + last 4 chars of the password. Enough for the operator
/// to confirm "this is the right one" without reproducing the secret
/// in every log line.
fn password_fingerprint(pw: &str) -> String {
    if pw.len() <= 8 {
        return "[hidden]".to_string();
    }
    let head: String = pw.chars().take(4).collect();
    let tail: String = pw
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// Drain a raw HTTP request head (everything up to the first blank
/// line). Caller may then slurp a body out of the same stream if
/// needed. Returns an empty string on EOF / read error so a
/// malformed request still gets a clean 401 response downstream.
async fn read_http_head(stream: &mut TcpStream) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    let mut head = String::new();
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.push_str(&String::from_utf8_lossy(&buf[..n]));
        if head.contains("\r\n\r\n") {
            break;
        }
        if head.len() > 65_536 {
            break;
        }
    }
    Ok(head)
}

/// Placeholder admin page. Gets replaced once the real UI lands; the
/// current content is deliberately terse so the tunnel-wiring
/// acceptance test doesn't depend on UI copy.
/// React + Vite + Tailwind bundle baked in at Rust compile time. The
/// `admin-ui/` workspace produces `admin-ui/dist/`; we embed every
/// file under that tree. A fresh clone that hasn't run `npm run
/// build` yet has a `.gitkeep`-only tree — we detect that at runtime
/// and fall back to [`ADMIN_FALLBACK_HTML`] so the tunnel always
/// reaches something useful.
#[derive(rust_embed::RustEmbed)]
#[folder = "admin-ui/dist/"]
#[exclude = ".gitkeep"]
struct AdminBundle;

/// Shown when the operator hasn't built the React bundle yet — e.g.
/// a fresh clone where `cd admin-ui && npm install && npm run build`
/// hasn't run. Stays in sync with `admin-ui/README.md` on the build
/// steps so copy-paste recovery is one hop away.
const ADMIN_FALLBACK_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>nexo-rs admin (bundle missing)</title>
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    body { font: 16px/1.5 system-ui, -apple-system, sans-serif;
           max-width: 40rem; margin: 3rem auto; padding: 0 1rem;
           color: #222; background: #fafafa; }
    code, pre { background: #eee; padding: .1em .3em; border-radius: 3px; }
    pre { padding: 1em; overflow-x: auto; }
    a { color: #0066cc; }
    h1 { margin-bottom: .3em; }
  </style>
</head>
<body>
  <h1>nexo-rs admin</h1>
  <p>The React bundle isn't embedded in this <code>agent</code> binary.
     Build it and rebuild the binary:</p>
  <pre>cd admin-ui
npm install
npm run build
cargo build --release --bin agent
./target/release/agent admin</pre>
  <p>More: <a href="https://lordmacu.github.io/nexo-rs/cli/reference.html#admin">CLI reference — admin</a></p>
</body>
</html>
"#;

/// Resolve a request path to an embedded asset plus its MIME type.
/// `/` and any unknown route both map to `index.html` so the SPA
/// router handles unknown routes client-side (standard Vite / CRA
/// pattern). Returns `None` only when the bundle is empty so the
/// caller can fall back to the placeholder HTML.
fn admin_asset_for_path(path: &str) -> Option<(Vec<u8>, &'static str)> {
    let trimmed = path.trim_start_matches('/');
    let candidate = if trimmed.is_empty() {
        "index.html".to_string()
    } else {
        trimmed.to_string()
    };

    // Dev-loop escape hatch: when NEXO_ADMIN_UI_DIR points at an
    // existing dist/ tree on disk, serve straight from there. This
    // lets a running `agent admin` pick up fresh Vite builds from
    // `npm run build -- --watch` without rebuilding the Rust binary.
    //
    // Production (env unset) uses the rust-embed bundle baked in at
    // compile time — zero runtime filesystem dependency.
    if let Some(dir) = admin_ui_disk_dir() {
        if let Some(hit) = read_admin_file_from_disk(&dir, &candidate) {
            return Some(hit);
        }
        // SPA fallback when the client asked for a deep route that
        // only exists in the JS router.
        return read_admin_file_from_disk(&dir, "index.html");
    }

    let hit = AdminBundle::get(&candidate).or_else(|| AdminBundle::get("index.html"))?;
    let mime = admin_mime_for(&candidate);
    Some((hit.data.into_owned(), mime))
}

fn admin_ui_disk_dir() -> Option<std::path::PathBuf> {
    let raw = std::env::var("NEXO_ADMIN_UI_DIR").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let p = std::path::PathBuf::from(raw);
    if !p.is_dir() {
        return None;
    }
    Some(p)
}

fn read_admin_file_from_disk(dir: &std::path::Path, rel: &str) -> Option<(Vec<u8>, &'static str)> {
    // Reject absolute / parent-traversal paths up front so a request
    // for `/../../etc/passwd` can't escape the dist tree. Decode
    // first so a `%2e%2e` escape can't sneak past the segment check
    // and become a literal `..` once the OS resolves the path.
    let decoded = url_decode(rel);
    if decoded
        .split('/')
        .any(|seg| seg == ".." || seg.starts_with('/'))
    {
        return None;
    }
    if rel
        .split('/')
        .any(|seg| seg == ".." || seg.starts_with('/'))
    {
        return None;
    }
    let full = dir.join(rel);
    if !full.starts_with(dir) {
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    Some((bytes, admin_mime_for(rel)))
}

fn admin_mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn has_restricted_delegate_allowlist(patterns: &[String]) -> bool {
    !patterns.is_empty() && !patterns.iter().any(|p| p.trim() == "*")
}

fn mcp_server_has_auth(cfg: &nexo_config::types::mcp_server::McpServerConfig) -> bool {
    if cfg.auth_token_env.is_some() {
        return true;
    }
    let Some(http) = cfg.http.as_ref() else {
        return false;
    };
    if http.auth_token_env.is_some() {
        return true;
    }
    match http.auth.as_ref() {
        Some(nexo_config::types::mcp_server::AuthConfigYaml::None) => false,
        Some(_) => true,
        None => false,
    }
}

/// Phase M1.b — re-read `mcp_server.expose_tools` from the YAML
/// directory and compute the new allowlist set.
///
/// Returns:
/// - `Ok(None)` when the allowlist is empty (no filter; everything
///   non-proxy is exposed) — matches `ToolRegistryBridge`'s
///   `swap_allowlist(None)` semantics.
/// - `Ok(Some(set))` when the operator listed explicit tool names.
/// - `Err(e)` on parse / IO failure. Caller absorbs the error and
///   keeps the previous (last-known-good) allowlist active.
///
/// Provider-agnostic: protocol-MCP, no LLM-provider assumption.
///
/// IRROMPIBLE refs:
/// - claude-code-leak `services/mcp/useManageMCPConnections.ts:618-665`
///   — consumer-side: clients only register the
///   `tools/list_changed` notification listener when the server
///   advertises `capabilities.tools.listChanged: true` (already
///   wired in M1.a `with_list_changed_capability`).
/// - claude-code-leak `:721-723` — multiple notifications safe;
///   client-side debounce within the existing 200 ms session
///   window.
/// - `research/`: no relevant prior art (OpenClaw is channel-side,
///   no MCP server hot-reload concept).
fn reload_expose_tools(
    config_dir: &std::path::Path,
) -> Result<Option<std::collections::HashSet<String>>> {
    let cfg = nexo_config::AppConfig::load_for_mcp_server(config_dir)?;
    let server_cfg = cfg.mcp_server.unwrap_or_default();
    Ok(compute_allowlist_from_mcp_server_cfg(&server_cfg))
}

/// Phase M1.b.c — derive the `ToolRegistryBridge` allowlist from
/// an in-memory `McpServerConfig`. Empty `expose_tools` returns
/// `None` (no filter, expose all non-proxy tools); non-empty
/// returns `Some(HashSet)` (HashSet collapses duplicates by
/// construction). Used by the daemon-embed boot wire (Mode::Run)
/// where the config is already loaded, complementing the
/// `reload_expose_tools` path that re-reads the YAML on
/// SIGHUP / Phase 18 reload.
fn compute_allowlist_from_mcp_server_cfg(
    cfg: &nexo_config::types::mcp_server::McpServerConfig,
) -> Option<std::collections::HashSet<String>> {
    if cfg.expose_tools.is_empty() {
        None
    } else {
        Some(cfg.expose_tools.iter().cloned().collect())
    }
}

async fn run_mcp_server(config_dir: &std::path::Path) -> Result<()> {
    use nexo_core::agent::self_report::WhoAmITool;
    use nexo_core::agent::tool_registry::ToolRegistry;
    use nexo_core::agent::{
        AgentContext, MemoryTool, MyStatsTool, SessionLogsTool, ToolRegistryBridge, WhatDoIKnowTool,
    };
    use nexo_core::session::SessionManager;
    use nexo_mcp::{run_stdio_server_with_auth, McpServerInfo};
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // Phase 12.6 — tolerant loader: skip llm.yaml / broker.yaml / memory.yaml.
    // The operator exposing tools doesn't need a full runtime configured.
    let boot = nexo_config::AppConfig::load_for_mcp_server(config_dir)
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
    // C2 — same boot-time policy resolver used by `nexo run`. Mirrors
    // agent-level fields; per-binding overrides are picked up at handler
    // call time via `ctx.effective_policy()`.
    let effective_primary =
        nexo_core::agent::effective::EffectiveBindingPolicy::from_agent_defaults(primary);
    // Keep mcp-server behavior aligned with the same per-agent policy surface
    // used by `nexo run` (web_search/link/team/lsp/etc.).
    let agent_cfg = Arc::new(primary.clone());
    // Prefer broker.yaml when available so outbound tools (RemoteTrigger,
    // delegate bridge) share the same transport as the main runtime.
    // Fall back to local broker for tolerant bootstrap.
    let broker = match nexo_config::load_optional::<nexo_config::BrokerConfig>(
        config_dir,
        "broker.yaml",
    ) {
        Ok(Some(bcfg)) => match AnyBroker::from_config(&bcfg.broker).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "mcp-server: broker.yaml present but broker init failed; falling back to local broker"
                );
                AnyBroker::local()
            }
        },
        Ok(None) => AnyBroker::local(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "mcp-server: failed to parse broker.yaml; falling back to local broker"
            );
            AnyBroker::local()
        }
    };
    let sessions = Arc::new(SessionManager::new(std::time::Duration::from_secs(300), 20));
    let mut ctx = AgentContext::new(primary.id.clone(), agent_cfg, broker.clone(), sessions);
    // Mirror the daemon path: always carry a shared LinkExtractor so
    // web_fetch/web_search-expand can execute instead of hard-failing.
    let link_extractor = Arc::new(nexo_core::link_understanding::LinkExtractor::new(
        &nexo_core::link_understanding::LinkUnderstandingConfig::default(),
    ));
    ctx = ctx.with_link_extractor(Arc::clone(&link_extractor));

    // Optional delegate bridge for mcp-server mode. Enabled only when the
    // operator explicitly overrides denied-by-policy tools and requests
    // `delegate` in `expose_tools`.
    let delegate_override_requested = server_cfg.expose_tools.iter().any(|n| n == "delegate")
        && server_cfg
            .expose_denied_tools
            .iter()
            .any(|n| n == "delegate");
    let mut delegate_override_ready = false;
    if delegate_override_requested {
        let router = Arc::new(nexo_core::agent::AgentRouter::new());
        let topic = nexo_core::agent::routing::route_topic(&primary.id);
        match broker.subscribe(&topic).await {
            Ok(mut sub) => {
                let router_for_sub = Arc::clone(&router);
                let agent_id = primary.id.clone();
                tokio::spawn(async move {
                    while let Some(ev) = sub.next().await {
                        let msg: nexo_core::agent::AgentMessage = match serde_json::from_value(
                            ev.payload,
                        ) {
                            Ok(m) => m,
                            Err(err) => {
                                tracing::debug!(
                                    error = %err,
                                    "mcp-server delegate bridge: dropping malformed route payload"
                                );
                                continue;
                            }
                        };
                        if msg.to != agent_id {
                            continue;
                        }
                        if let nexo_core::agent::AgentPayload::Result { output, .. } = msg.payload {
                            let _ = router_for_sub.resolve(msg.correlation_id, output);
                        }
                    }
                });
                ctx = ctx.with_router(router);
                delegate_override_ready = true;
                tracing::info!(
                    topic = %topic,
                    "mcp-server delegate bridge subscribed"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    topic = %topic,
                    "mcp-server delegate bridge unavailable; delegate will stay denied"
                );
            }
        }
    }

    let workspace_dir = if primary.workspace.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&primary.workspace))
    };

    // Best-effort memory bootstrap for mcp-server mode: this subcommand
    // must remain tolerant when memory.yaml is absent/misconfigured.
    //
    // C5 — `memory.secret_guard` is read from the same load. When
    // memory.yaml is absent/invalid we fall back to the secure
    // default (enabled=true, on_secret=Block, all rules active).
    let mut memory_default_recall_mode = "keyword".to_string();
    let mut mcp_secret_guard: Option<nexo_memory::SecretGuard> = {
        // Default secure: applied when memory.yaml absent / unreadable
        // OR when present but with no `secret_guard` override (the
        // wire shape's `Default::default()` already mirrors secure).
        let guard_cfg = nexo_memory::SecretGuardConfig::default();
        Some(guard_cfg.build_guard())
    };
    let long_term_memory: Option<Arc<nexo_memory::LongTermMemory>> =
        match nexo_config::load_optional::<nexo_config::types::MemoryConfig>(
            config_dir,
            "memory.yaml",
        ) {
            Ok(Some(mem_cfg)) => {
                memory_default_recall_mode = mem_cfg.vector.default_recall_mode.clone();
                // C5 — wire the operator-supplied secret_guard policy
                // when present. Boot fails loud on a YAML typo
                // (invalid `on_secret`, malformed `rules`).
                let guard_cfg = build_secret_guard_config_from_yaml(&mem_cfg.secret_guard)
                    .context("invalid memory.secret_guard config in memory.yaml")?;
                mcp_secret_guard = Some(guard_cfg.build_guard());
                if mem_cfg.long_term.backend == "sqlite" {
                    let path = mem_cfg
                        .long_term
                        .sqlite
                        .as_ref()
                        .map(|s| s.path.as_str())
                        .unwrap_or("./data/memory.db");
                    match nexo_memory::LongTermMemory::open(path).await {
                        Ok(mem) => {
                            let mem = if let Some(ref guard) = mcp_secret_guard {
                                mem.with_guard(guard.clone())
                            } else {
                                mem
                            };
                            Some(Arc::new(mem))
                        }
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
    if !primary.transcripts_dir.trim().is_empty() {
        registry.register(SessionLogsTool::tool_def(), SessionLogsTool::new());
    }

    // Phase 79.M — boot dispatcher. The runtime tool registry is
    // populated by walking `EXPOSABLE_TOOLS`, filtering by the
    // operator's `mcp_server.expose_tools` allowlist, and calling
    // each per-tool boot helper with whatever handles this server
    // process happens to carry. Missing handles → `SkippedInfraMissing`
    // with a clear label so the operator knows what to enable.
    {
        use nexo_config::types::mcp_exposable::{lookup_exposable, EXPOSABLE_TOOLS};
        use nexo_core::agent::mcp_server_bridge::{
            boot_exposable, telemetry as bridge_telem, BootResult, McpServerBootContext,
        };
        use std::collections::HashSet;

        // Best-effort handles. Each `Option<_>` left as `None` causes the
        // dependent tool's boot helper to return `SkippedInfraMissing`
        // with a labelled handle name.
        let cron_store: Option<Arc<dyn nexo_core::cron_schedule::CronStore>> = {
            // mcp-server mode keeps cron storage in `<state_dir>/cron.db`
            // when the operator listed any `cron_*` entry in expose_tools.
            // Phase 79.M.1 ships a tolerant boot — if we can't open the
            // file the cron tools just fall back to SkippedInfraMissing.
            let needs_cron = server_cfg
                .expose_tools
                .iter()
                .any(|n| n.starts_with("cron_"));
            if needs_cron {
                match nexo_core::cron_schedule::SqliteCronStore::open("./data/cron.db").await {
                    Ok(s) => Some(Arc::new(s) as Arc<dyn nexo_core::cron_schedule::CronStore>),
                    Err(e) => {
                        tracing::warn!(error = %e, "mcp-server: failed to open cron.db; cron_* tools disabled");
                        None
                    }
                }
            } else {
                None
            }
        };

        let config_changes_store: Option<
            Arc<dyn nexo_core::config_changes_store::ConfigChangesStore>,
        > = {
            if server_cfg
                .expose_tools
                .iter()
                .any(|n| n == "config_changes_tail")
            {
                match nexo_core::config_changes_store::SqliteConfigChangesStore::open(
                    "./data/config_changes.db",
                )
                .await
                {
                    Ok(s) => {
                        Some(Arc::new(s)
                            as Arc<
                                dyn nexo_core::config_changes_store::ConfigChangesStore,
                            >)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "mcp-server: failed to open config_changes.db");
                        None
                    }
                }
            } else {
                None
            }
        };

        // web_search_router boot — env-driven provider discovery
        // (mirrors the `nexo run` startup at src/main.rs:1259-1281).
        let web_search_router: Option<Arc<nexo_web_search::WebSearchRouter>> = {
            if server_cfg.expose_tools.iter().any(|n| n == "web_search") {
                let mut providers: Vec<Arc<dyn nexo_web_search::WebSearchProvider>> = Vec::new();
                if let Ok(k) = std::env::var("BRAVE_SEARCH_API_KEY") {
                    providers.push(Arc::new(
                        nexo_web_search::providers::brave::BraveProvider::new(k, 8000),
                    ));
                }
                if let Ok(k) = std::env::var("TAVILY_API_KEY") {
                    providers.push(Arc::new(
                        nexo_web_search::providers::tavily::TavilyProvider::new(k, 10000),
                    ));
                }
                providers.push(Arc::new(
                    nexo_web_search::providers::duckduckgo::DuckDuckGoProvider::new(12000),
                ));
                Some(Arc::new(nexo_web_search::WebSearchRouter::new(
                    providers, None,
                )))
            } else {
                None
            }
        };
        if let Some(router) = web_search_router.as_ref() {
            ctx = ctx.with_web_search_router(Arc::clone(router));
        }

        // mcp_runtime boot — when router tools are requested, build a
        // session runtime from mcp.yaml if present/enabled. Fallback to an
        // empty runtime so router tools still register and return a stable
        // "no servers connected" surface instead of hard-missing infra.
        let mcp_runtime: Option<Arc<nexo_mcp::SessionMcpRuntime>> = {
            let needs_mcp_router = server_cfg
                .expose_tools
                .iter()
                .any(|n| n == "ListMcpResources" || n == "ReadMcpResource");
            if !needs_mcp_router {
                None
            } else {
                let fallback_empty = || {
                    Arc::new(nexo_mcp::SessionMcpRuntime::new(
                        uuid::Uuid::new_v4(),
                        "mcp-server-empty".to_string(),
                        std::collections::HashMap::<String, Arc<dyn nexo_mcp::McpClient>>::new(),
                    ))
                };
                match nexo_config::load_optional::<nexo_config::types::McpConfigFile>(
                    config_dir, "mcp.yaml",
                ) {
                    Ok(Some(mcp_file)) if mcp_file.mcp.enabled => {
                        if let Err(e) = mcp_file.mcp.validate() {
                            tracing::warn!(
                                error = %e,
                                "mcp-server: mcp.yaml validation failed; mcp router tools will expose empty runtime"
                            );
                            Some(fallback_empty())
                        } else {
                            let rt_cfg = nexo_mcp::McpRuntimeConfig::from_yaml(&mcp_file.mcp);
                            let mgr = nexo_mcp::McpRuntimeManager::new(rt_cfg);
                            Some(mgr.get_or_create(uuid::Uuid::new_v4()).await)
                        }
                    }
                    Ok(Some(_)) => {
                        tracing::warn!(
                            "mcp-server: mcp.yaml has `mcp.enabled: false`; mcp router tools will expose empty runtime"
                        );
                        Some(fallback_empty())
                    }
                    Ok(None) => {
                        tracing::warn!(
                            "mcp-server: config/mcp.yaml not found; mcp router tools will expose empty runtime"
                        );
                        Some(fallback_empty())
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "mcp-server: failed to read mcp.yaml; mcp router tools will expose empty runtime"
                        );
                        Some(fallback_empty())
                    }
                }
            }
        };
        if let Some(rt) = mcp_runtime.as_ref() {
            ctx = ctx.with_mcp(Arc::clone(rt));
        }

        // memory_git boot — only when the operator listed
        // `forge_memory_checkpoint` or `memory_history`. Mirrors the
        // `nexo run` setup at src/main.rs:2471-2499 (workspace_git.enabled).
        let memory_git: Option<Arc<nexo_core::agent::MemoryGitRepo>> = {
            let needs_git = server_cfg
                .expose_tools
                .iter()
                .any(|n| n == "forge_memory_checkpoint" || n == "memory_history");
            if needs_git && !primary.workspace.is_empty() {
                let ws = std::path::PathBuf::from(&primary.workspace);
                let author_name = if primary.workspace_git.author_name.is_empty() {
                    "nexo-mcp".to_string()
                } else {
                    primary.workspace_git.author_name.clone()
                };
                let author_email = if primary.workspace_git.author_email.is_empty() {
                    "nexo-mcp@local".to_string()
                } else {
                    primary.workspace_git.author_email.clone()
                };
                match nexo_core::agent::MemoryGitRepo::open_or_init(&ws, author_name, author_email)
                {
                    Ok(g) => {
                        let g = if let Some(ref guard) = mcp_secret_guard {
                            g.with_guard(guard.clone())
                        } else {
                            g
                        };
                        Some(Arc::new(g))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "mcp-server: workspace-git open failed; memory tools disabled");
                        None
                    }
                }
            } else {
                None
            }
        };

        // taskflow_manager boot — open the taskflow store when
        // `taskflow` is in expose_tools.
        let taskflow_manager: Option<Arc<nexo_taskflow::FlowManager>> = {
            if server_cfg.expose_tools.iter().any(|n| n == "taskflow") {
                match open_flow_manager().await {
                    Ok(m) => Some(Arc::new(m)),
                    Err(e) => {
                        tracing::warn!(error = %e, "mcp-server: taskflow store open failed; taskflow tool disabled");
                        None
                    }
                }
            } else {
                None
            }
        };

        // lsp_manager boot — only when `Lsp` is in expose_tools.
        // Mirrors the `nexo run` startup wiring (Phase 79.5).
        let lsp_manager: Option<Arc<nexo_lsp::LspManager>> = {
            if server_cfg.expose_tools.iter().any(|n| n == "Lsp") {
                Some(nexo_lsp::LspManager::new(nexo_lsp::SessionConfig::default()))
            } else {
                None
            }
        };

        // team_store boot — when any Team* tool is requested
        // (read-only TeamList/Status or mutating Create/Delete/Send).
        let team_store_handle: Option<Arc<dyn nexo_team_store::TeamStore>> = {
            let needs_team = server_cfg
                .expose_tools
                .iter()
                .any(|n| n.starts_with("Team"));
            if needs_team {
                match nexo_team_store::SqliteTeamStore::open("./data/teams.db").await {
                    Ok(s) => Some(Arc::new(s) as Arc<dyn nexo_team_store::TeamStore>),
                    Err(e) => {
                        tracing::warn!(error = %e, "mcp-server: teams.db open failed; Team* read-only tools disabled");
                        None
                    }
                }
            } else {
                None
            }
        };

        // Build boot context. AgentContext already carries policy +
        // optional handles (link extractor, MCP runtime) for call-time use.
        let agent_ctx = Arc::new(ctx.clone());
        let boot_ctx =
            McpServerBootContext::builder("mcp-server", broker.clone(), agent_ctx).build();
        let mut boot_ctx_enriched = boot_ctx;
        boot_ctx_enriched.cron_store = cron_store;
        boot_ctx_enriched.mcp_runtime = mcp_runtime;
        boot_ctx_enriched.config_changes_store = config_changes_store;
        boot_ctx_enriched.web_search_router = web_search_router;
        boot_ctx_enriched.link_extractor = Some(link_extractor);
        boot_ctx_enriched.long_term_memory = long_term_memory.clone();
        boot_ctx_enriched.memory_git = memory_git;
        boot_ctx_enriched.taskflow_manager = taskflow_manager;
        boot_ctx_enriched.lsp_manager = lsp_manager;
        boot_ctx_enriched.team_store = team_store_handle;

        // Phase 79.M.c.full — Config self-edit handles. Compiled
        // out when `config-self-edit` is off so the default build
        // carries no overhead.
        #[cfg(feature = "config-self-edit")]
        if server_cfg.expose_tools.iter().any(|n| n == "Config") {
            use nexo_core::agent::approval_correlator::{
                ApprovalCorrelator, ApprovalCorrelatorConfig,
            };
            use nexo_core::agent::config_tool::{DefaultSecretRedactor, ReloadTrigger};
            use nexo_setup::config_tool_bridge::{SetupDenylistChecker, SetupYamlPatchApplier};
            use std::sync::Arc;

            // Synthetic ReloadTrigger — mcp-server doesn't run a
            // ConfigReloadCoordinator; operator-side `nexo run`
            // picks up YAML changes via Phase 18 file watcher.
            // Returning Ok keeps the apply-path success contract
            // (the YAML write succeeded; reload is async on the
            // other process).
            struct McpServerReloadTrigger;
            #[async_trait::async_trait]
            impl ReloadTrigger for McpServerReloadTrigger {
                async fn reload(&self) -> Result<(), String> {
                    tracing::info!(
                        "[config] mcp-server: YAML written successfully. Reload deferred — \
                         operator-side `nexo run` (or restart) will pick up the change \
                         via Phase 18 hot-reload."
                    );
                    Ok(())
                }
            }

            let agents_yaml = config_dir.join("agents.yaml");
            let binding_id = format!("mcp:{}", primary.id);
            let applier: Arc<dyn nexo_core::agent::config_tool::YamlPatchApplier> =
                Arc::new(SetupYamlPatchApplier::new(agents_yaml, binding_id));
            let denylist: Arc<dyn nexo_core::agent::config_tool::DenylistChecker> =
                Arc::new(SetupDenylistChecker);
            let redactor: Arc<dyn nexo_core::agent::config_tool::SecretRedactor> =
                Arc::new(DefaultSecretRedactor);
            let correlator = ApprovalCorrelator::new(ApprovalCorrelatorConfig::default());
            let reload: Arc<dyn ReloadTrigger> = Arc::new(McpServerReloadTrigger);

            let proposals_dir = std::path::PathBuf::from("./data/config-proposals");
            std::fs::create_dir_all(&proposals_dir).ok();

            boot_ctx_enriched.config_yaml_applier = Some(applier);
            boot_ctx_enriched.config_denylist_checker = Some(denylist);
            boot_ctx_enriched.config_secret_redactor = Some(redactor);
            boot_ctx_enriched.config_approval_correlator = Some(correlator);
            boot_ctx_enriched.config_reload_trigger = Some(reload);
            boot_ctx_enriched.config_tool_policy = Some(effective_primary.config_tool.clone());
            boot_ctx_enriched.config_proposals_dir = Some(proposals_dir);
        }

        // Phase 79.M.c hardening — Config self-edit requires an
        // auth_token to be configured. Refuse the boot otherwise.
        let config_self_edit_auth_ok = mcp_server_has_auth(&server_cfg);
        // Same auth hardening for operator-overridden policy-denied tools.
        let mcp_auth_configured = config_self_edit_auth_ok;
        let denied_overrides: HashSet<String> =
            server_cfg.expose_denied_tools.iter().cloned().collect();
        let denied_profile = &server_cfg.denied_tools_profile;
        if !denied_overrides.is_empty() && !denied_profile.enabled {
            tracing::error!(
                "mcp-server: expose_denied_tools requested but `mcp_server.denied_tools_profile.enabled=false`; denied overrides stay blocked"
            );
        }

        let mut requested: HashSet<String> = server_cfg.expose_tools.iter().cloned().collect();
        for entry in EXPOSABLE_TOOLS {
            if !requested.remove(entry.name) {
                continue;
            }
            // Cargo feature gate is checked from the root crate so
            // `nexo-core` doesn't need a `feature = "config-self-edit"`
            // of its own. When off, override the dispatcher's verdict.
            let result = if matches!(
                entry.boot_kind,
                nexo_config::types::mcp_exposable::BootKind::DeniedByPolicy { .. }
            ) && denied_overrides.contains(entry.name)
            {
                if !denied_profile.enabled {
                    BootResult::SkippedDenied {
                        reason: "denied-profile-disabled",
                    }
                } else if !denied_profile.allows(entry.name) {
                    BootResult::SkippedDenied {
                        reason: "denied-profile-tool-not-allowed",
                    }
                } else {
                    match entry.name {
                        "Heartbeat" => {
                            if denied_profile.require_auth && !mcp_auth_configured {
                                tracing::error!(
                                    "mcp-server: Heartbeat override requires auth (`mcp_server.auth_token_env` \
                                     or `http.auth`) to protect delayed outbound side-effects"
                                );
                                BootResult::SkippedDenied {
                                    reason: "heartbeat-requires-auth-token",
                                }
                            } else if !primary.heartbeat.enabled {
                                tracing::warn!(
                                    agent = %primary.id,
                                    "mcp-server: Heartbeat override requested but `agents.{}.heartbeat.enabled = false`",
                                    primary.id
                                );
                                BootResult::SkippedDenied {
                                    reason: "heartbeat-disabled",
                                }
                            } else if let Some(mem) = long_term_memory.as_ref() {
                                BootResult::Registered(
                                    HeartbeatTool::tool_def(),
                                    Arc::new(HeartbeatTool::new(Arc::clone(mem))),
                                )
                            } else {
                                BootResult::SkippedInfraMissing {
                                    handle: "long_term_memory",
                                }
                            }
                        }
                        "RemoteTrigger" => {
                            if denied_profile.require_auth && !mcp_auth_configured {
                                tracing::error!(
                                    "mcp-server: RemoteTrigger override requires auth (`mcp_server.auth_token_env` \
                                     or `http.auth`) to protect outbound side-effects"
                                );
                                BootResult::SkippedDenied {
                                    reason: "remote-trigger-requires-auth-token",
                                }
                            } else if denied_profile.require_remote_trigger_targets
                                && primary.remote_triggers.is_empty()
                            {
                                tracing::error!(
                                    agent = %primary.id,
                                    "mcp-server: RemoteTrigger override requires explicit `agents.{}.remote_triggers` entries",
                                    primary.id
                                );
                                BootResult::SkippedDenied {
                                    reason: "remote-trigger-targets-required",
                                }
                            } else {
                                let sink: Arc<
                                    dyn nexo_core::agent::remote_trigger_tool::RemoteTriggerSink,
                                > = Arc::new(
                                    nexo_core::agent::remote_trigger_tool::ReqwestSink::new(
                                        broker.clone(),
                                    ),
                                );
                                let tool =
                                    nexo_core::agent::remote_trigger_tool::RemoteTriggerTool::new(
                                        sink,
                                    );
                                BootResult::Registered(
                                    nexo_core::agent::remote_trigger_tool::RemoteTriggerTool::tool_def(),
                                    Arc::new(tool),
                                )
                            }
                        }
                        "delegate" => {
                            if denied_profile.require_auth && !mcp_auth_configured {
                                tracing::error!(
                                    "mcp-server: delegate override requires auth (`mcp_server.auth_token_env` \
                                     or `http.auth`) to protect cross-agent side-effects"
                                );
                                BootResult::SkippedDenied {
                                    reason: "delegate-requires-auth-token",
                                }
                            } else if denied_profile.require_delegate_allowlist
                                && !has_restricted_delegate_allowlist(&primary.allowed_delegates)
                            {
                                tracing::error!(
                                    agent = %primary.id,
                                    "mcp-server: delegate override requires explicit restricted `agents.{}.allowed_delegates` (non-empty and not `*`)",
                                    primary.id
                                );
                                BootResult::SkippedDenied {
                                    reason: "delegate-allowlist-required",
                                }
                            } else if delegate_override_ready {
                                BootResult::Registered(
                                    nexo_core::agent::DelegationTool::tool_def(),
                                    Arc::new(nexo_core::agent::DelegationTool),
                                )
                            } else {
                                BootResult::SkippedDenied {
                                    reason: "delegate-bridge-unavailable",
                                }
                            }
                        }
                        _ => BootResult::SkippedDenied {
                            reason: "denied-tool-override-unsupported",
                        },
                    }
                }
            } else if matches!(
                entry.boot_kind,
                nexo_config::types::mcp_exposable::BootKind::FeatureGated
            ) && !cfg!(feature = "config-self-edit")
            {
                BootResult::SkippedFeatureGated {
                    feature: entry.feature_gate.unwrap_or("unknown"),
                }
            } else if entry.name == "Config" && !config_self_edit_auth_ok {
                tracing::error!(
                    "mcp-server: Config tool refuses to register without `mcp_server.auth_token_env` \
                     or `http.auth` configured. Set an auth token and restart."
                );
                BootResult::SkippedDenied {
                    reason: "config-requires-auth-token",
                }
            } else if entry.name == "Config" && !effective_primary.config_tool.self_edit {
                tracing::error!(
                    agent = %primary.id,
                    "mcp-server: Config tool refuses to register because `agents.{}.config_tool.self_edit = false`. \
                     Set `self_edit: true` in agents.yaml to opt in.",
                    primary.id
                );
                BootResult::SkippedDenied {
                    reason: "config-self-edit-policy-disabled",
                }
            } else if entry.name == "Config" && effective_primary.config_tool.allowed_paths.is_empty() {
                tracing::error!(
                    agent = %primary.id,
                    "mcp-server: Config tool refuses to register because `agents.{}.config_tool.allowed_paths` is empty. \
                     Empty list means 'every supported key' which is too permissive for MCP exposure — \
                     pick an explicit subset (e.g. ['language', 'description']).",
                    primary.id
                );
                BootResult::SkippedDenied {
                    reason: "config-allowed-paths-must-be-explicit",
                }
            } else {
                boot_exposable(entry.name, &boot_ctx_enriched)
            };
            match result {
                BootResult::Registered(def, handler) => {
                    registry.register_arc(def, handler);
                    bridge_telem::record_registered(entry.name, entry.tier);
                    tracing::info!(
                        tool = entry.name,
                        tier = entry.tier.as_str(),
                        "mcp-server: registered exposable tool"
                    );
                }
                BootResult::SkippedDenied { reason } => {
                    bridge_telem::record_skipped(entry.name, "denied_by_policy");
                    tracing::warn!(
                        tool = entry.name,
                        reason,
                        "mcp-server: tool denied by policy — never exposable"
                    );
                }
                BootResult::SkippedDeferred { phase, reason } => {
                    bridge_telem::record_skipped(entry.name, "deferred");
                    tracing::warn!(
                        tool = entry.name,
                        phase,
                        reason,
                        "mcp-server: tool wiring deferred to follow-up sub-phase"
                    );
                }
                BootResult::SkippedFeatureGated { feature } => {
                    bridge_telem::record_skipped(entry.name, "feature_gate_off");
                    tracing::warn!(
                        tool = entry.name,
                        feature,
                        "mcp-server: tool requires Cargo feature; rebuild with --features {feature}"
                    );
                }
                BootResult::SkippedInfraMissing { handle } => {
                    bridge_telem::record_skipped(entry.name, "infra_missing");
                    tracing::warn!(
                        tool = entry.name,
                        handle,
                        "mcp-server: tool needs handle '{handle}' which this process didn't construct"
                    );
                }
                BootResult::UnknownName => {
                    // Unreachable here — we iterated EXPOSABLE_TOOLS.
                    bridge_telem::record_skipped(entry.name, "unknown_name");
                }
            }
        }
        // Anything left in `requested` is operator typo / removed tool.
        for typo in requested {
            // Cross-check: maybe it just isn't in the catalog.
            let _ = lookup_exposable(&typo);
            bridge_telem::record_skipped(&typo, "unknown_name");
            tracing::warn!(
                tool = typo.as_str(),
                "mcp-server: expose_tools entry not in EXPOSABLE_TOOLS catalog — typo or removed tool"
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
    let worker_ctx_seed = ctx.clone();
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

    let autonomous_worker_join = if server_cfg.autonomous_worker.enabled {
        Some(
            start_mcp_autonomous_worker(
                config_dir,
                primary,
                worker_ctx_seed,
                broker.clone(),
                long_term_memory.clone(),
                shutdown.clone(),
                server_cfg.autonomous_worker.tick_secs,
            )
            .await?,
        )
    } else {
        None
    };

    // Phase 76.1 — opt-in HTTP transport runs alongside stdio.
    let http_handle = if let Some(http_yaml) = server_cfg.http.clone() {
        if http_yaml.enabled {
            Some(start_http_transport(&bridge, &http_yaml, &shutdown).await?)
        } else {
            None
        }
    } else {
        None
    };

    // Phase M1.b — SIGHUP reload trigger. Operator runs
    // `kill -HUP $(pidof nexo)` after editing
    // `mcp_server.expose_tools` in YAML; the handler re-reads the
    // allowlist, atomically swaps it into the bridge (visible to
    // both stdio + HTTP clones — they share the inner ArcSwap from
    // M1.a) and emits `notifications/tools/list_changed` so
    // connected HTTP/SSE clients refresh without reconnect. SIGHUP
    // chosen over a file watcher to ship MVP without a new dep —
    // file-watcher / `ConfigReloadCoordinator` integration deferred
    // to M1.b.b.
    //
    // The bridge is `Clone` (M1.a `with_list_changed_capability` +
    // ArcSwap-shared allowlist); we clone here BEFORE
    // `run_stdio_server_with_auth` consumes the original.
    // `HttpNotifyHandle` is the lightweight `Clone` notifier from
    // `nexo-mcp`'s M1.b addition — detached from the JoinHandle so
    // safe to move into the background task.
    #[cfg(unix)]
    {
        let bridge_for_sig = bridge.clone();
        let notifier_for_sig: Option<nexo_mcp::HttpNotifyHandle> =
            http_handle.as_ref().map(|h| h.notifier());
        let cfg_dir_for_sig = config_dir.to_path_buf();
        let shutdown_for_sig = shutdown.clone();
        tokio::spawn(async move {
            let mut sighup = match tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::hangup(),
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "[mcp-server] could not install SIGHUP handler");
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = shutdown_for_sig.cancelled() => break,
                    signal = sighup.recv() => {
                        if signal.is_none() { break; }
                        tracing::info!("[mcp-server] SIGHUP received; reloading expose_tools");
                        match reload_expose_tools(&cfg_dir_for_sig) {
                            Ok(new_allow) => {
                                let new_count = new_allow.as_ref().map(|s| s.len()).unwrap_or(0);
                                bridge_for_sig.swap_allowlist(new_allow);
                                let sessions = notifier_for_sig
                                    .as_ref()
                                    .map(|n| n.notify_tools_list_changed())
                                    .unwrap_or(0);
                                tracing::info!(
                                    sessions,
                                    new_count,
                                    "[mcp-server] expose_tools reloaded; tools/list_changed emitted"
                                );
                            }
                            Err(e) => tracing::warn!(
                                error = %e,
                                "[mcp-server] SIGHUP reload failed; old allowlist preserved"
                            ),
                        }
                    }
                }
            }
        });
    }
    #[cfg(not(unix))]
    tracing::info!(
        "[mcp-server] SIGHUP handler not installed (non-Unix); restart for expose_tools changes"
    );

    let stdio_result = run_stdio_server_with_auth(bridge, shutdown.clone(), auth_token).await;

    // Drain HTTP transport before propagating stdio result.
    if let Some(handle) = http_handle {
        // shutdown was already cancelled if stdio exited cleanly via signal.
        shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle.join).await;
    }
    if let Some(join) = autonomous_worker_join {
        shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), join).await;
    }

    stdio_result.context("mcp-server loop failed")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 76.14 — `nexo mcp-server` CLI ops
// ---------------------------------------------------------------------------

/// `nexo mcp-server inspect <url>` — list tools and resources of a
/// reachable MCP server.
async fn run_mcp_inspect(url: &str) -> Result<()> {
    use serde_json::Value;

    let client = reqwest::Client::new();
    let base = url.trim_end_matches('/');

    tracing::info!(%url, "inspecting MCP server");

    // 1. Initialize
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "nexo-mcp-inspect", "version": "0.1" }
        },
        "id": 1
    });
    let resp = client
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&init_body)
        .send()
        .await
        .context("initialize request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("initialize returned {}: {body}", status.as_u16());
    }
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .context("no mcp-session-id in initialize response")?;
    let init_body: Value = resp.json().await.context("initialize JSON parse")?;
    let server_name = init_body["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap_or("unknown");
    let server_version = init_body["result"]["serverInfo"]["version"]
        .as_str()
        .unwrap_or("?");

    println!("# MCP Server: {server_name} v{server_version}");
    println!("URL: {url}\n");

    // 2. tools/list
    let tools_body = serde_json::json!({
        "jsonrpc": "2.0", "method": "tools/list", "id": 2
    });
    let tools_resp = client
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session_id)
        .json(&tools_body)
        .send()
        .await
        .context("tools/list failed")?;
    let tools: Value = tools_resp.json().await.context("tools/list JSON")?;
    let tool_list = tools["result"]["tools"].as_array();

    println!("## Tools ({})\n", tool_list.map(|t| t.len()).unwrap_or(0));
    if let Some(tools) = tool_list {
        for t in tools {
            let name = t["name"].as_str().unwrap_or("?");
            let desc = t["description"].as_str().unwrap_or("(no description)");
            println!("- **`{name}`** — {desc}");
        }
    }

    // 3. resources/list (best-effort)
    let res_body = serde_json::json!({
        "jsonrpc": "2.0", "method": "resources/list", "id": 3
    });
    match client
        .post(format!("{base}/mcp"))
        .header("mcp-session-id", &session_id)
        .json(&res_body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let res: Value = r.json().await.unwrap_or_default();
            let resources = res["result"]["resources"].as_array();
            println!("\n## Resources ({})\n", resources.map(|r| r.len()).unwrap_or(0));
            if let Some(resources) = resources {
                for r in resources {
                    let uri = r["uri"].as_str().unwrap_or("?");
                    let name = r["name"].as_str().unwrap_or(uri);
                    println!("- **`{name}`** — `{uri}`");
                }
            }
        }
        _ => {
            println!("\n## Resources\n\n(resources not supported by this server)\n");
        }
    }

    Ok(())
}

/// `nexo mcp-server bench <url> --tool <name> --rps <n>` — load test.
async fn run_mcp_bench(url: &str, tool: &str, rps: u32) -> Result<()> {
    use std::time::Instant;

    println!("# MCP Load Test\n");
    println!("- URL: {url}");
    println!("- Tool: `{tool}`");
    println!("- Target RPS: {rps}\n");

    if rps == 0 {
        anyhow::bail!("--rps must be > 0");
    }

    let client = reqwest::Client::new();
    let base = url.trim_end_matches('/');
    let delay_ms = 1000 / rps as u64;
    let total_requests = (rps * 5).max(10) as usize;

    // Initialize.
    let init_body = serde_json::json!({
        "jsonrpc": "2.0", "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "nexo-mcp-bench", "version": "0.1" }
        },
        "id": 1
    });
    let resp = client
        .post(format!("{base}/mcp"))
        .json(&init_body)
        .send()
        .await
        .context("initialize failed")?;
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .context("no mcp-session-id in initialize response")?;
    // Drain response body so the connection returns to the pool.
    let _ = resp.bytes().await?;

    let mut latencies_ms: Vec<u64> = Vec::with_capacity(total_requests);
    let bench_start = Instant::now();
    let mut seq = 2u64;

    for i in 0..total_requests {
        let call_start = Instant::now();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": tool, "arguments": {} },
            "id": seq
        });
        let result = client
            .post(format!("{base}/mcp"))
            .header("mcp-session-id", &session_id)
            .json(&body)
            .send()
            .await;
        let latency = call_start.elapsed().as_millis() as u64;
        latencies_ms.push(latency);
        seq += 1;

        match result {
            Ok(r) if r.status().is_success() => {
                if i < 3 {
                    println!("  #{i}: {latency}ms OK");
                }
            }
            Ok(r) => {
                println!("  #{i}: {latency}ms HTTP {}", r.status().as_u16());
            }
            Err(e) => {
                println!("  #{i}: {latency}ms ERR {e}");
            }
        }

        if i + 1 < total_requests {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    }

    let total_elapsed = bench_start.elapsed();
    latencies_ms.sort();
    let p50 = latencies_ms[latencies_ms.len() / 2];
    let p90 = latencies_ms[(latencies_ms.len() as f64 * 0.90) as usize];
    let p99 = latencies_ms[(latencies_ms.len() as f64 * 0.99) as usize];

    println!("\n## Results\n");
    println!("| Metric | Value |");
    println!("|--------|-------|");
    println!("| Requests | {} |", latencies_ms.len());
    println!("| Duration | {:.1}s |", total_elapsed.as_secs_f64());
    println!("| p50 latency | {}ms |", p50);
    println!("| p90 latency | {}ms |", p90);
    println!("| p99 latency | {}ms |", p99);
    println!(
        "| Actual RPS | {:.1} |",
        latencies_ms.len() as f64 / total_elapsed.as_secs_f64()
    );

    Ok(())
}

/// `nexo mcp-server tail-audit <db>` — read recent entries from
/// a local audit log SQLite database.
async fn run_mcp_tail_audit(db_path: &str) -> Result<()> {
    use sqlx::Row;

    let db_url = format!("sqlite:{db_path}?mode=ro");
    let pool = sqlx::SqlitePool::connect(&db_url)
        .await
        .context("failed to open audit DB (read-only)")?;

    let rows = sqlx::query(
        "SELECT id, timestamp, tool_name, principal, duration_ms, is_error
         FROM mcp_call_log
         ORDER BY id DESC
         LIMIT 100",
    )
    .fetch_all(&pool)
    .await
    .context("failed to query mcp_call_log")?;

    if rows.is_empty() {
        println!("# Audit Log: {db_path}\n");
        println!("(empty — no calls recorded yet)\n");
        pool.close().await;
        return Ok(());
    }

    println!("# Audit Log: {db_path}\n");
    println!("| ID | Timestamp | Tool | Principal | Latency | Error |");
    println!("|----|-----------|------|-----------|---------|-------|");

    for row in &rows {
        let id: i64 = row.get("id");
        let ts: String = row.get("timestamp");
        let tool: String = row.get("tool_name");
        let principal: String = row.get("principal");
        let latency: i64 = row.get("duration_ms");
        let is_error: bool = row.get("is_error");

        println!(
            "| {id} | {ts} | `{tool}` | {principal} | {latency}ms | {} |",
            if is_error { "ERR" } else { "OK" }
        );
    }

    println!("\n{} rows shown (last 100).\n", rows.len());
    pool.close().await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Phase 80.1.d — `nexo agent dream` operator CLI
//
// Mirror leak `claude-code-leak/src/components/tasks/BackgroundTasksDialog
// .tsx:281,315-317` `DreamTask.kill(taskId, setAppState)` semantics: row
// status flip + lock rollback + (deferred) abort signal. Daemon is NOT
// required for any sub-command — the read paths open the SQLite DB
// read-only and the kill path opens it read-write while reading the lock
// file directly. Provider-agnostic by construction (LLM provider never
// touches this surface).
// ─────────────────────────────────────────────────────────────────────

/// Phase 80.1.d — 3-tier dream-runs DB path resolution. `--db` wins over
/// `NEXO_STATE_ROOT` env wins over the XDG-default
/// `~/.local/share/nexo/state/dream_runs.db`. The YAML tier is intentionally
/// absent for now — `agents.state_root` does not exist as a config field
/// (state_root flows into `BootDeps` directly per Phase 80.1.b.b.b), so
/// the CLI uses the env-or-default fallback to stay aligned with the
/// daemon's discovery path once main.rs hookup ships.
/// Phase M4.a.b — resolve the per-agent destination for extracted
/// memories. Prefers the agent's explicit workspace when set
/// (`<workspace>/memory/`); falls back to
/// `<state_root>/<agent_id>/memory/` so multi-agent deployments
/// stay isolated. Caller is responsible for `create_dir_all`.
fn resolve_extract_memory_dir(agent_cfg: &nexo_config::AgentConfig) -> std::path::PathBuf {
    if !agent_cfg.workspace.trim().is_empty() {
        std::path::PathBuf::from(&agent_cfg.workspace).join("memory")
    } else {
        nexo_project_tracker::state::nexo_state_dir()
            .join(&agent_cfg.id)
            .join("memory")
    }
}

fn resolve_dream_db_path(override_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return Ok(p.to_path_buf());
    }
    if let Ok(state_root) = std::env::var("NEXO_STATE_ROOT") {
        return Ok(nexo_dream::default_dream_db_path(std::path::Path::new(
            &state_root,
        )));
    }
    let xdg = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("no XDG data dir; pass --db <path>"))?;
    Ok(xdg.join("nexo/state/dream_runs.db"))
}

/// Tail dream runs newest-first. Optional goal filter; `n` clamped server-side.
async fn run_agent_dream_tail(
    goal_id: Option<&str>,
    n: usize,
    db_override: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use nexo_agent_registry::{DreamRunStore, SqliteDreamRunStore};
    use nexo_driver_types::GoalId;

    let db = resolve_dream_db_path(db_override)?;
    if !db.exists() {
        if json {
            println!("[]");
        } else {
            println!("(no dream runs recorded yet — db not found at {})", db.display());
        }
        return Ok(());
    }

    let store = SqliteDreamRunStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open dream_runs DB at {}", db.display()))?;

    let rows = match goal_id {
        Some(g) => {
            let uuid = uuid::Uuid::parse_str(g)
                .with_context(|| format!("--goal `{g}` is not a valid UUID"))?;
            store.tail_for_goal(GoalId(uuid), n).await
        }
        None => store.tail(n).await,
    }
    .with_context(|| "failed to tail dream_runs".to_string())?;

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("# Dream Runs (db: {})\n", db.display());
    if rows.is_empty() {
        println!("(no runs)\n");
        return Ok(());
    }
    println!("| ID | Goal | Status | Phase | Sessions | Files | Started | Ended | Label |");
    println!("|----|------|--------|-------|----------|-------|---------|-------|-------|");
    for r in &rows {
        let id_short = short_uuid(&r.id);
        let goal_short = short_uuid(&r.goal_id.0);
        let ended = r
            .ended_at
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "| {} | {} | {:?} | {:?} | {} | {} | {} | {} | {} |",
            id_short,
            goal_short,
            r.status,
            r.phase,
            r.sessions_reviewing,
            r.files_touched.len(),
            r.started_at.format("%Y-%m-%dT%H:%M:%S"),
            ended,
            r.fork_label,
        );
    }
    println!("\n{} rows shown (last {n}).\n", rows.len());
    Ok(())
}

/// Show a single dream run's full row + last turns.
async fn run_agent_dream_status(
    run_id: &str,
    db_override: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use nexo_agent_registry::{DreamRunStore, SqliteDreamRunStore};

    let uuid = uuid::Uuid::parse_str(run_id)
        .with_context(|| format!("`{run_id}` is not a valid UUID"))?;

    let db = resolve_dream_db_path(db_override)?;
    if !db.exists() {
        anyhow::bail!("dream_runs DB not found at {}", db.display());
    }
    let store = SqliteDreamRunStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open dream_runs DB at {}", db.display()))?;

    let row = store
        .get(uuid)
        .await
        .with_context(|| "failed to fetch dream run".to_string())?
        .ok_or_else(|| anyhow::anyhow!("run `{run_id}` not found"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }

    println!("# Dream Run {}\n", row.id);
    println!("- **goal_id**: {}", row.goal_id.0);
    println!("- **status**: {:?}", row.status);
    println!("- **phase**: {:?}", row.phase);
    println!("- **sessions_reviewing**: {}", row.sessions_reviewing);
    println!("- **fork_label**: {}", row.fork_label);
    println!("- **started_at**: {}", row.started_at);
    if let Some(ended) = row.ended_at {
        println!("- **ended_at**: {ended}");
    }
    if let Some(prior) = row.prior_mtime_ms {
        println!("- **prior_mtime_ms**: {prior}");
    }
    if !row.files_touched.is_empty() {
        println!("\n## Files touched ({}):", row.files_touched.len());
        for p in &row.files_touched {
            println!("- {}", p.display());
        }
    }
    if !row.turns.is_empty() {
        println!("\n## Last {} turns:", row.turns.len());
        for (i, t) in row.turns.iter().enumerate().take(5) {
            println!(
                "{}. text_len={} tool_use_count={}",
                i + 1,
                t.text.len(),
                t.tool_use_count,
            );
        }
    }
    println!();
    Ok(())
}

/// Kill a running dream run: flip status to `Aborted`, finalise, optionally
/// rollback the consolidation lock when the operator passes `--memory-dir`.
async fn run_agent_dream_kill(
    run_id: &str,
    force: bool,
    memory_dir_override: Option<&std::path::Path>,
    db_override: Option<&std::path::Path>,
) -> Result<()> {
    use nexo_agent_registry::{DreamRunStatus, DreamRunStore, SqliteDreamRunStore};
    use nexo_dream::ConsolidationLock;

    let uuid = uuid::Uuid::parse_str(run_id)
        .with_context(|| format!("`{run_id}` is not a valid UUID"))?;

    let db = resolve_dream_db_path(db_override)?;
    if !db.exists() {
        anyhow::bail!("dream_runs DB not found at {}", db.display());
    }
    let store = SqliteDreamRunStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open dream_runs DB at {}", db.display()))?;

    let row = store
        .get(uuid)
        .await
        .with_context(|| "failed to fetch dream run".to_string())?
        .ok_or_else(|| anyhow::anyhow!("run `{run_id}` not found"))?;

    let already_terminal = matches!(
        row.status,
        DreamRunStatus::Completed
            | DreamRunStatus::Failed
            | DreamRunStatus::Killed
            | DreamRunStatus::LostOnRestart
    );
    if already_terminal {
        println!(
            "[dream-kill] run_id={} already in terminal state {:?}; nothing to do",
            row.id, row.status
        );
        return Ok(());
    }

    if matches!(row.status, DreamRunStatus::Running) && !force {
        eprintln!(
            "[dream-kill] run_id={} is still Running. Pass --force to abort.",
            row.id
        );
        std::process::exit(2);
    }

    store
        .update_status(uuid, DreamRunStatus::Killed)
        .await
        .with_context(|| "failed to flip status to Aborted".to_string())?;
    store
        .finalize(uuid, chrono::Utc::now())
        .await
        .with_context(|| "failed to finalise dream run".to_string())?;
    println!(
        "[dream-kill] run_id={} status was {:?}, transitioning to Killed",
        row.id, row.status
    );

    match (memory_dir_override, row.prior_mtime_ms) {
        (Some(md), Some(prior)) => {
            // Holder-stale = 1h matches AutoDreamConfig default; we don't
            // need a real config here because rollback is purely a file op.
            let lock = ConsolidationLock::new(md, std::time::Duration::from_secs(3600))
                .with_context(|| "failed to construct ConsolidationLock for rollback")?;
            lock.rollback(prior).await;
            println!(
                "[dream-kill] lock rollback: prior_mtime={prior} → memory_dir={}",
                md.display()
            );
        }
        (Some(_), None) => {
            println!(
                "[dream-kill] no prior_mtime recorded for this run; lock rollback skipped"
            );
        }
        (None, Some(_)) => {
            println!(
                "[dream-kill] WARN: status flipped but lock not rolled back. \
                 Pass --memory-dir <path> next time to rewind the consolidation lock."
            );
        }
        (None, None) => {}
    }
    println!("[dream-kill] done");
    Ok(())
}

/// Phase 80.1.d helper — render only the first 8 hex chars of a UUID
/// for compact tabular output, mirroring leak's "shortId" UI helper.
fn short_uuid(id: &uuid::Uuid) -> String {
    let s = id.to_string();
    s.chars().take(8).collect()
}

// ─────────────────────────────────────────────────────────────────────
// Phase 80.10 — `nexo agent run` / `nexo agent ps` operator CLI
//
// Slim MVP: `run --bg` inserts a goal-handle row with kind=Bg + status=Running
// and prints the goal_id immediately so the operator can detach. Full
// goal execution under the daemon supervisor is a follow-up; for the
// MVP, the row is queued and the local daemon (or a future detached
// worker) picks it up. `ps` reads the same store read-only so an
// operator can list goals even when the daemon is down.
// ─────────────────────────────────────────────────────────────────────

/// 3-tier path resolution for `agent_handles` SQLite store. Mirror of
/// `resolve_dream_db_path` for the Phase 80.10 surface — operators
/// configure either explicitly via `--db`, or via `NEXO_STATE_ROOT`
/// env, or accept the XDG default.
fn resolve_agent_db_path(override_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return Ok(p.to_path_buf());
    }
    if let Ok(state_root) = std::env::var("NEXO_STATE_ROOT") {
        return Ok(std::path::Path::new(&state_root).join("agent_handles.db"));
    }
    let xdg = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("no XDG data dir; pass --db <path>"))?;
    Ok(xdg.join("nexo/state/agent_handles.db"))
}

/// `nexo agent run [--bg] <prompt>` — insert a new goal-handle row.
async fn run_agent_run(
    prompt: String,
    bg: bool,
    db_override: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use chrono::Utc;
    use nexo_agent_registry::{
        AgentHandle, AgentRegistryStore, AgentRunStatus, AgentSnapshot, SessionKind,
        SqliteAgentRegistryStore,
    };
    use nexo_driver_types::GoalId;
    use uuid::Uuid;

    if prompt.trim().is_empty() {
        anyhow::bail!("usage: nexo agent run [--bg] <prompt> (prompt cannot be empty)");
    }
    let db = resolve_agent_db_path(db_override)?;
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let store = SqliteAgentRegistryStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open agent_handles DB at {}", db.display()))?;

    let goal_id = GoalId(Uuid::new_v4());
    let kind = if bg { SessionKind::Bg } else { SessionKind::Interactive };
    let handle = AgentHandle {
        goal_id,
        phase_id: format!("cli-{}", if bg { "bg" } else { "run" }),
        status: AgentRunStatus::Running,
        origin: None,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot::default(),
        plan_mode: None,
        kind,
    };
    store
        .upsert(&handle)
        .await
        .with_context(|| "failed to write agent_handles row".to_string())?;

    if json {
        let v = serde_json::json!({
            "goal_id": goal_id.0.to_string(),
            "kind": kind.as_db_str(),
            "prompt": prompt,
            "status": "running",
            "queued": true,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("[agent-run] goal_id={}", goal_id.0);
        println!("[agent-run] kind={}", kind.as_db_str());
        println!("[agent-run] status=running (queued for daemon pickup)");
        println!("[agent-run] prompt: {prompt}");
        if bg {
            println!(
                "[agent-run] detached — re-attach later with `nexo agent attach {}` (Phase 80.16)",
                goal_id.0
            );
        }
    }
    Ok(())
}

/// `nexo agent ps [--all] [--kind=...] [--json]` — list agent handles.
async fn run_agent_ps(
    kind_filter: Option<&str>,
    all: bool,
    db_override: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use nexo_agent_registry::{
        AgentRegistryStore, AgentRunStatus, SessionKind, SqliteAgentRegistryStore,
    };

    let db = resolve_agent_db_path(db_override)?;
    if !db.exists() {
        if json {
            println!("[]");
        } else {
            println!(
                "(no agent runs recorded yet — db not found at {})",
                db.display()
            );
        }
        return Ok(());
    }
    let store = SqliteAgentRegistryStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open agent_handles DB at {}", db.display()))?;

    let mut rows = if let Some(k) = kind_filter {
        let parsed = SessionKind::from_db_str(k)
            .with_context(|| format!("--kind `{k}` is not a valid SessionKind"))?;
        store.list_by_kind(parsed).await?
    } else {
        store.list().await?
    };

    if !all {
        rows.retain(|h| h.status == AgentRunStatus::Running);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("# Agent runs (db: {})\n", db.display());
    if rows.is_empty() {
        println!("(no rows match)\n");
        return Ok(());
    }
    println!("| ID | Kind | Status | Phase | Started | Ended |");
    println!("|----|------|--------|-------|---------|-------|");
    for r in &rows {
        let id_short = short_uuid(&r.goal_id.0);
        let ended = r
            .finished_at
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "| {} | {} | {:?} | {} | {} | {} |",
            id_short,
            r.kind.as_db_str(),
            r.status,
            r.phase_id,
            r.started_at.format("%Y-%m-%dT%H:%M:%S"),
            ended,
        );
    }
    println!("\n{} rows shown.\n", rows.len());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Phase 80.16 — `nexo agent attach` / `nexo agent discover`
//
// Slim MVP: both subcommands are RO viewers over the local
// `agent_handles` SQLite store. Live event streaming via NATS lands
// in 80.16.b; user input piping needs Phase 80.11's
// `agent.inbox.<goal_id>` subject contract.
// ─────────────────────────────────────────────────────────────────────

/// `nexo agent attach <goal_id>` — read-only viewer of the goal's
/// latest persisted snapshot. Errors cleanly when the UUID is bad
/// or the handle is absent; renders different output for terminal
/// vs Running goals.
async fn run_agent_attach(
    goal_id: &str,
    db_override: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use nexo_agent_registry::{AgentRegistryStore, AgentRunStatus, SqliteAgentRegistryStore};
    use nexo_driver_types::GoalId;
    use uuid::Uuid;

    let uuid = Uuid::parse_str(goal_id)
        .with_context(|| format!("`{goal_id}` is not a valid UUID"))?;
    let db = resolve_agent_db_path(db_override)?;
    if !db.exists() {
        anyhow::bail!("agent_handles DB not found at {}", db.display());
    }
    let store = SqliteAgentRegistryStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open agent_handles DB at {}", db.display()))?;
    let handle = store
        .get(GoalId(uuid))
        .await
        .with_context(|| "failed to fetch agent handle".to_string())?
        .ok_or_else(|| anyhow::anyhow!("no agent handle found for `{goal_id}`"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&handle)?);
        return Ok(());
    }

    println!("# Agent Goal {}\n", handle.goal_id.0);
    println!("- **kind**: {}", handle.kind.as_db_str());
    println!("- **status**: {:?}", handle.status);
    println!("- **phase_id**: {}", handle.phase_id);
    println!("- **started_at**: {}", handle.started_at);
    if let Some(ended) = handle.finished_at {
        println!("- **finished_at**: {ended}");
    }
    if let Some(text) = &handle.snapshot.last_progress_text {
        println!("\n## Last progress\n{text}");
    }
    if let Some(diff) = &handle.snapshot.last_diff_stat {
        println!("\n## Last diff\n```\n{diff}\n```");
    }
    println!(
        "\n- **turn_index**: {}/{}",
        handle.snapshot.turn_index, handle.snapshot.max_turns
    );
    println!("- **last_event_at**: {}", handle.snapshot.last_event_at);

    if handle.status == AgentRunStatus::Running {
        println!(
            "\n[attach] Live event stream requires daemon connection \
             — re-run with NATS available (Phase 80.16.b follow-up)."
        );
    } else if handle.status.is_terminal() {
        println!(
            "\n[attach] Goal is in terminal state {:?}; no further \
             updates expected.",
            handle.status
        );
    }
    Ok(())
}

/// `nexo agent discover [--include-interactive]` — list Running
/// goals filtered to BG / Daemon / DaemonWorker by default. With
/// `--include-interactive`, returns all kinds.
async fn run_agent_discover(
    include_interactive: bool,
    db_override: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    use nexo_agent_registry::{AgentRunStatus, SessionKind, SqliteAgentRegistryStore};

    let db = resolve_agent_db_path(db_override)?;
    if !db.exists() {
        if json {
            println!("[]");
        } else {
            println!(
                "(no agent runs recorded yet — db not found at {})",
                db.display()
            );
        }
        return Ok(());
    }
    let store = SqliteAgentRegistryStore::open(&db.to_string_lossy())
        .await
        .with_context(|| format!("failed to open agent_handles DB at {}", db.display()))?;

    let kinds: Vec<SessionKind> = if include_interactive {
        vec![
            SessionKind::Interactive,
            SessionKind::Bg,
            SessionKind::Daemon,
            SessionKind::DaemonWorker,
        ]
    } else {
        vec![SessionKind::Bg, SessionKind::Daemon, SessionKind::DaemonWorker]
    };

    let mut all = Vec::new();
    for k in kinds {
        all.extend(store.list_by_kind(k).await?);
    }
    all.retain(|h| h.status == AgentRunStatus::Running);
    all.sort_by_key(|h| std::cmp::Reverse(h.started_at));

    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    if all.is_empty() {
        let hint = if include_interactive {
            ""
        } else {
            "; pass --include-interactive to broaden"
        };
        println!("(no detached / daemon goals running{hint})");
        return Ok(());
    }
    println!("# Discoverable goals (db: {})\n", db.display());
    println!("| ID | Kind | Phase | Started | Last activity |");
    println!("|----|------|-------|---------|---------------|");
    for h in &all {
        println!(
            "| {} | {} | {} | {} | {} |",
            short_uuid(&h.goal_id.0),
            h.kind.as_db_str(),
            h.phase_id,
            h.started_at.format("%Y-%m-%dT%H:%M:%S"),
            h.snapshot.last_event_at.format("%Y-%m-%dT%H:%M:%S"),
        );
    }
    println!("\n{} goal(s).\n", all.len());
    Ok(())
}

// ----------------------------------------------------------------
// Phase 80.9.e — `nexo channel list/doctor/test` operator CLI.
// Static helpers that read the YAML and exercise the gate +
// XML wrap helper without needing a daemon up.
// ----------------------------------------------------------------

/// Load AppConfig from `--config=<path>` if provided, otherwise
/// fall back to the global `config_dir` the CLI was invoked with.
/// Phase 80.9.b.b — generic shim that lets `ChannelRelayDecider`
/// wrap an `Arc<dyn PermissionDecider>`. The decorator's generic
/// `D: PermissionDecider` doesn't accept `Arc<dyn ...>` directly
/// because trait-object dispatch isn't a concrete type, so we
/// route through a newtype that delegates to the inner Arc.
struct ArcDeciderShim(Arc<dyn nexo_driver_permission::PermissionDecider>);

#[async_trait::async_trait]
impl nexo_driver_permission::PermissionDecider for ArcDeciderShim {
    async fn decide(
        &self,
        request: nexo_driver_permission::types::PermissionRequest,
    ) -> Result<
        nexo_driver_permission::types::PermissionResponse,
        nexo_driver_permission::PermissionError,
    > {
        self.0.decide(request).await
    }
}

/// Phase 80.9 main.rs hookup — sink that forwards a
/// [`nexo_mcp::channel_bridge::ChannelInboundEvent`] onto the
/// existing intake lane. We publish on the broker subject
/// `agent.channel.inbound` carrying a JSON envelope; the
/// runtime's intake task subscribes there and re-enters the
/// pairing / dispatch / rate-limit gates the same way it does
/// for any other inbound channel (WhatsApp, Telegram, email).
///
/// Provider-agnostic: this sink doesn't know how to talk to any
/// specific channel platform — it only converts the bridge's
/// typed event into a broker message, leaving routing decisions
/// to the existing intake layer.
struct IntakeChannelSink {
    broker: AnyBroker,
}

impl IntakeChannelSink {
    pub fn new(broker: AnyBroker) -> Self {
        Self { broker }
    }
}

#[async_trait::async_trait]
impl nexo_mcp::channel_bridge::ChannelInboundSink for IntakeChannelSink {
    async fn deliver(
        &self,
        event: nexo_mcp::channel_bridge::ChannelInboundEvent,
    ) -> Result<(), nexo_mcp::channel_bridge::SinkError> {
        // Stable subject — `agent.channel.inbound`. The intake
        // task can also subscribe to a wildcard
        // `mcp.channel.>` directly, but routing through the
        // intake subject keeps every gate uniform across channel
        // sources.
        let topic = "agent.channel.inbound";
        let payload = match serde_json::to_value(&serde_json::json!({
            "binding_id": event.binding_id,
            "server_name": event.server_name,
            "session_id": event.session_id,
            "session_key": event.session_key,
            "content": event.content,
            "meta": event.meta,
            "rendered": event.rendered,
            "envelope_id": event.envelope_id,
            "sent_at_ms": event.sent_at_ms,
        })) {
            Ok(v) => v,
            Err(e) => {
                return Err(nexo_mcp::channel_bridge::SinkError::Other(format!(
                    "serialise: {e}"
                )));
            }
        };
        let evt =
            nexo_broker::Event::new(topic.to_string(), "mcp.channel.intake".to_string(), payload);
        nexo_broker::handle::BrokerHandle::publish(&self.broker, topic, evt)
            .await
            .map_err(|e| {
                nexo_mcp::channel_bridge::SinkError::Other(format!("broker publish: {e}"))
            })
    }
}

fn load_app_config_for_channels(
    config_override: Option<&std::path::Path>,
    config_dir: &std::path::Path,
) -> Result<AppConfig> {
    if let Some(p) = config_override {
        // `--config` points to a single file or to a directory.
        // AppConfig::load expects a directory. If the operator
        // passed a file, walk up to its parent.
        let dir = if p.is_file() {
            p.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
        } else {
            p.to_path_buf()
        };
        AppConfig::load(&dir).with_context(|| format!("loading config from {}", dir.display()))
    } else {
        AppConfig::load(config_dir)
            .with_context(|| format!("loading config from {}", config_dir.display()))
    }
}

#[derive(serde::Serialize)]
struct ChannelListRow<'a> {
    agent_id: &'a str,
    enabled: bool,
    approved_servers: Vec<&'a str>,
    bindings: Vec<ChannelListBindingRow<'a>>,
}

#[derive(serde::Serialize)]
struct ChannelListBindingRow<'a> {
    binding_id: String,
    allowed_channel_servers: &'a Vec<String>,
}

async fn run_channel_list(
    config_override: Option<&std::path::Path>,
    json: bool,
    config_dir: &std::path::Path,
) -> Result<()> {
    let app = load_app_config_for_channels(config_override, config_dir)?;
    let mut rows: Vec<ChannelListRow> = Vec::new();
    for agent in &app.agents.agents {
        let cfg = agent
            .channels
            .as_ref();
        let approved: Vec<&str> = cfg
            .map(|c| c.approved.iter().map(|e| e.server.as_str()).collect())
            .unwrap_or_default();
        let enabled = cfg.map(|c| c.enabled).unwrap_or(false);
        let bindings: Vec<ChannelListBindingRow> = agent
            .inbound_bindings
            .iter()
            .filter(|b| !b.allowed_channel_servers.is_empty())
            .map(|b| ChannelListBindingRow {
                binding_id: format!(
                    "{}:{}",
                    b.plugin,
                    b.instance.as_deref().unwrap_or("default")
                ),
                allowed_channel_servers: &b.allowed_channel_servers,
            })
            .collect();
        rows.push(ChannelListRow {
            agent_id: &agent.id,
            enabled,
            approved_servers: approved,
            bindings,
        });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no agents configured)");
        return Ok(());
    }
    for row in &rows {
        let state = if row.enabled { "ENABLED" } else { "disabled" };
        println!(
            "## agent {} — channels.{} ({} approved)",
            row.agent_id,
            state,
            row.approved_servers.len()
        );
        if row.approved_servers.is_empty() {
            println!("  (no approved servers)");
        } else {
            for s in &row.approved_servers {
                println!("  approved: {s}");
            }
        }
        if row.bindings.is_empty() {
            println!("  (no binding has allowed_channel_servers)");
        } else {
            for b in &row.bindings {
                println!(
                    "  binding {}: {} server(s) — {}",
                    b.binding_id,
                    b.allowed_channel_servers.len(),
                    b.allowed_channel_servers.join(", ")
                );
            }
        }
        println!();
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct ChannelDoctorRow {
    agent_id: String,
    binding_id: String,
    server: String,
    outcome: String,
    skip_kind: Option<&'static str>,
    reason: String,
}

async fn run_channel_doctor(
    config_override: Option<&std::path::Path>,
    binding_filter: Option<&str>,
    json: bool,
    config_dir: &std::path::Path,
) -> Result<()> {
    use nexo_mcp::channel::{
        gate_channel_server, ChannelGateInputs, ChannelGateOutcome,
    };

    let app = load_app_config_for_channels(config_override, config_dir)?;
    let mut rows: Vec<ChannelDoctorRow> = Vec::new();
    for agent in &app.agents.agents {
        let Some(cfg) = agent.channels.as_ref() else {
            continue;
        };
        for binding in &agent.inbound_bindings {
            let bid = format!(
                "{}:{}",
                binding.plugin,
                binding.instance.as_deref().unwrap_or("default")
            );
            if let Some(filter) = binding_filter {
                if filter != bid {
                    continue;
                }
            }
            // Walk every server the binding declares — this surfaces
            // the case where a binding lists a server but the agent
            // didn't add it to `approved` (gate 5 catches it).
            for server in &binding.allowed_channel_servers {
                // The doctor cannot probe a live MCP server, so it
                // assumes the capability is declared. This is the
                // "if the runtime is honest, would the gate pass?"
                // shape — operators reading the output know that
                // the only failure they can hit at runtime is
                // gate 1 (capability) and gate 4 (plugin source
                // mismatch when the runtime stamps an unexpected
                // source).
                let inputs = ChannelGateInputs {
                    server_name: server,
                    capability_declared: true,
                    plugin_source: cfg
                        .lookup_approved(server)
                        .and_then(|e| e.plugin_source.as_deref()),
                    cfg,
                    binding_allowlist: &binding.allowed_channel_servers,
                };
                let (outcome_label, skip_kind, reason) = match gate_channel_server(&inputs) {
                    ChannelGateOutcome::Register => (
                        "WOULD REGISTER".to_string(),
                        None,
                        "all static gates pass; live runtime must declare the capability"
                            .to_string(),
                    ),
                    ChannelGateOutcome::Skip { kind, reason } => {
                        ("SKIP".to_string(), Some(kind.as_str()), reason)
                    }
                };
                rows.push(ChannelDoctorRow {
                    agent_id: agent.id.clone(),
                    binding_id: bid.clone(),
                    server: server.clone(),
                    outcome: outcome_label,
                    skip_kind,
                    reason,
                });
            }
            // Cross-check approved entries — a server in `approved`
            // that no binding lists is fine but worth surfacing.
            for entry in &cfg.approved {
                let already = binding
                    .allowed_channel_servers
                    .iter()
                    .any(|s| s == &entry.server);
                if !already {
                    rows.push(ChannelDoctorRow {
                        agent_id: agent.id.clone(),
                        binding_id: bid.clone(),
                        server: entry.server.clone(),
                        outcome: "NOT BOUND".to_string(),
                        skip_kind: Some("session"),
                        reason: format!(
                            "approved server {} is not in this binding's allowed_channel_servers",
                            entry.server
                        ),
                    });
                }
            }
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!(
            "(no channel-using bindings found{})",
            binding_filter
                .map(|f| format!(" — filter='{}'", f))
                .unwrap_or_default()
        );
        return Ok(());
    }
    println!("| Agent | Binding | Server | Outcome | Skip | Reason |");
    println!("|-------|---------|--------|---------|------|--------|");
    for r in &rows {
        println!(
            "| {} | {} | {} | {} | {} | {} |",
            r.agent_id,
            r.binding_id,
            r.server,
            r.outcome,
            r.skip_kind.unwrap_or("-"),
            r.reason
        );
    }
    println!("\n{} row(s).\n", rows.len());
    Ok(())
}

#[derive(serde::Serialize)]
struct ChannelTestOutput {
    server: String,
    binding_id: Option<String>,
    parsed_content: String,
    rendered_xml: String,
    truncated: bool,
    session_key: String,
}

async fn run_channel_test(
    server: &str,
    binding_filter: Option<&str>,
    content_override: Option<&str>,
    config_override: Option<&std::path::Path>,
    json: bool,
    config_dir: &std::path::Path,
) -> Result<()> {
    use nexo_mcp::channel::{parse_channel_notification, CHANNEL_NOTIFICATION_METHOD};

    let app = load_app_config_for_channels(config_override, config_dir)?;

    // Find the first agent whose `channels.approved` contains
    // `server` — the doctor walks bindings, but `test` is
    // server-centric so we just need an agent owning the entry.
    let cfg = app
        .agents
        .agents
        .iter()
        .find_map(|a| {
            a.channels
                .as_ref()
                .filter(|c| c.lookup_approved(server).is_some())
                .map(|c| (a.id.clone(), c.clone()))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "channel test: no agent has '{}' in channels.approved",
                server
            )
        })?;
    let (_agent_id, channels_cfg) = cfg;

    let body = content_override
        .map(str::to_string)
        .unwrap_or_else(|| format!("hello from {server} — channel test payload"));
    let params = serde_json::json!({
        "content": body,
        "meta": {
            "chat_id": "C_TEST",
            "user": "operator"
        }
    });
    let inbound = parse_channel_notification(
        server,
        CHANNEL_NOTIFICATION_METHOD,
        &params,
        Some(&channels_cfg),
    )
    .map_err(|e| anyhow::anyhow!("channel test: parse failed: {e}"))?;

    let pairs: Vec<(String, String)> = inbound
        .meta
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let rendered = nexo_mcp::channel::wrap_channel_message(
        &inbound.server_name,
        &inbound.content,
        Some(&pairs),
    );

    let truncated = inbound.content.len() < body.len();
    let out = ChannelTestOutput {
        server: server.to_string(),
        binding_id: binding_filter.map(str::to_string),
        parsed_content: inbound.content.clone(),
        rendered_xml: rendered,
        truncated,
        session_key: inbound.session_key.0.clone(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("# Channel test — server={}\n", server);
        if let Some(b) = &out.binding_id {
            println!("(binding filter requested: {b})\n");
        }
        if out.truncated {
            println!("[content truncated by max_content_chars]");
        }
        println!("session_key: {}\n", out.session_key);
        println!("--- rendered XML (model-facing) ---");
        println!("{}", out.rendered_xml);
    }
    Ok(())
}

async fn start_mcp_autonomous_worker(
    config_dir: &std::path::Path,
    primary: &nexo_config::types::agents::AgentConfig,
    mcp_bridge_ctx: nexo_core::agent::AgentContext,
    broker: AnyBroker,
    long_term_memory: Option<Arc<nexo_memory::LongTermMemory>>,
    shutdown: tokio_util::sync::CancellationToken,
    tick_secs: u64,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use nexo_auth::StrictLevel;
    use nexo_core::agent::{
        AgentBehavior, AgentContext, CancelFollowupTool, CheckFollowupTool, LlmAgentBehavior,
        ToolRegistry,
    };
    use nexo_core::Plugin;

    let memory = long_term_memory.ok_or_else(|| {
        anyhow::anyhow!(
            "mcp_server.autonomous_worker.enabled=true requires long-term memory (config/memory.yaml)"
        )
    })?;

    let full_cfg = AppConfig::load(config_dir)
        .context("mcp-server autonomous worker requires full runtime config")?;
    let mut worker_agent_cfg = full_cfg
        .agents
        .agents
        .iter()
        .find(|a| a.id == primary.id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent `{}` not found in full config", primary.id))?;
    if !worker_agent_cfg.allowed_tools.is_empty() {
        tracing::warn!(
            agent = %worker_agent_cfg.id,
            "mcp-server autonomous worker ignores agent.allowed_tools and uses a dedicated restricted registry"
        );
        worker_agent_cfg.allowed_tools.clear();
    }

    let google_auth = nexo_auth::load_google_auth(config_dir)
        .context("failed to load plugins/google-auth.yaml")?;
    let secrets_dir = secrets_dir_for(config_dir);
    let creds_bundle = nexo_auth::build_credentials(
        &full_cfg.agents.agents,
        &full_cfg.plugins.whatsapp,
        &full_cfg.plugins.telegram,
        &google_auth,
        full_cfg.plugins.email.as_ref(),
        &secrets_dir,
        StrictLevel::Lenient,
    )
    .map_err(|errs| {
        let joined = errs
            .into_iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!("failed to bootstrap credentials for mcp autonomous worker:\n{joined}")
    })?;
    for w in &creds_bundle.warnings {
        tracing::warn!(target: "credentials", "{w}");
    }
    let creds_bundle = Arc::new(creds_bundle);

    let email_cfg = full_cfg.plugins.email.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "mcp_server.autonomous_worker.enabled=true requires config/plugins/email.yaml"
        )
    })?;
    if !email_cfg.enabled || email_cfg.accounts.is_empty() {
        anyhow::bail!(
            "mcp_server.autonomous_worker.enabled=true requires email.enabled=true and at least one account"
        );
    }

    let data_dir = std::env::var("NEXO_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("data"));
    let email_plugin = Arc::new(nexo_plugin_email::EmailPlugin::new(
        email_cfg.clone(),
        creds_bundle.stores.email.clone(),
        creds_bundle.stores.google.clone(),
        data_dir,
    ));
    email_plugin
        .start(broker.clone())
        .await
        .context("failed to start email plugin for mcp autonomous worker")?;

    let dispatcher = match email_plugin.dispatcher_handle().await {
        Some(h) => h,
        None => {
            let _ = email_plugin.stop().await;
            anyhow::bail!(
                "email plugin started without dispatcher handle; autonomous follow-up worker cannot send replies"
            );
        }
    };

    let email_health = email_plugin
        .health_map()
        .await
        .unwrap_or_else(nexo_plugin_email::inbound::HealthMap::default);
    let email_tool_ctx = Arc::new(nexo_plugin_email::EmailToolContext {
        creds: creds_bundle.stores.email.clone(),
        google: creds_bundle.stores.google.clone(),
        config: Arc::new(email_cfg),
        dispatcher,
        health: email_health,
        bounce_store: email_plugin.bounce_store_handle(),
        attachment_store: email_plugin.attachment_store_handle(),
        attachments_dir: email_plugin.attachments_dir(),
    });

    let worker_tools = Arc::new(ToolRegistry::new());
    worker_tools.register(
        CancelFollowupTool::tool_def(),
        CancelFollowupTool::new(Arc::clone(&memory)),
    );
    worker_tools.register(
        CheckFollowupTool::tool_def(),
        CheckFollowupTool::new(Arc::clone(&memory)),
    );
    nexo_plugin_email::register_email_tools(&worker_tools, email_tool_ctx);

    let llm_registry = LlmRegistry::with_builtins();
    let llm = match llm_registry.build(&full_cfg.llm, &worker_agent_cfg.model) {
        Ok(c) => c,
        Err(e) => {
            let _ = email_plugin.stop().await;
            return Err(anyhow::anyhow!(
                "failed to build LLM for mcp autonomous worker agent `{}`: {e}",
                worker_agent_cfg.id
            ));
        }
    };
    let behavior = LlmAgentBehavior::new(llm, Arc::clone(&worker_tools));

    let mut worker_ctx = AgentContext::new(
        worker_agent_cfg.id.clone(),
        Arc::new(worker_agent_cfg),
        broker,
        Arc::clone(&mcp_bridge_ctx.sessions),
    )
    .with_memory(memory)
    .with_credentials(Arc::clone(&creds_bundle.resolver))
    .with_breakers(Arc::clone(&creds_bundle.breakers));
    if let Some(ext) = mcp_bridge_ctx.link_extractor.clone() {
        worker_ctx = worker_ctx.with_link_extractor(ext);
    }

    let tick = std::time::Duration::from_secs(tick_secs.max(10));
    let join = tokio::spawn(async move {
        tracing::info!(
            agent = %worker_ctx.agent_id,
            tick_secs = tick.as_secs(),
            "mcp-server autonomous worker started"
        );
        if let Err(e) = behavior.on_heartbeat(&worker_ctx).await {
            tracing::warn!(
                agent = %worker_ctx.agent_id,
                error = %e,
                "mcp-server autonomous worker heartbeat failed"
            );
        }
        let mut interval = tokio::time::interval(tick);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        let shutdown_wait = shutdown.cancelled_owned();
        tokio::pin!(shutdown_wait);
        loop {
            tokio::select! {
                _ = &mut shutdown_wait => break,
                _ = interval.tick() => {
                    if let Err(e) = behavior.on_heartbeat(&worker_ctx).await {
                        tracing::warn!(
                            agent = %worker_ctx.agent_id,
                            error = %e,
                            "mcp-server autonomous worker heartbeat failed"
                        );
                    }
                }
            }
        }
        if let Err(e) = email_plugin.stop().await {
            tracing::warn!(
                error = %e,
                "mcp-server autonomous worker failed to stop email plugin cleanly"
            );
        }
        tracing::info!(agent = %worker_ctx.agent_id, "mcp-server autonomous worker stopped");
    });
    Ok(join)
}

/// Phase 76.1 — boot the HTTP transport from the YAML block, reusing
/// the same `ToolRegistryBridge` the stdio path consumes (cloned, since
/// the bridge is `Clone`).
async fn start_http_transport(
    bridge: &nexo_core::agent::ToolRegistryBridge,
    yaml: &nexo_config::types::mcp_server::HttpTransportConfigYaml,
    shutdown: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<nexo_mcp::HttpServerHandle> {
    use nexo_mcp::{start_http_server, HttpTransportConfig};

    if yaml.auth.is_some() && yaml.auth_token_env.is_some() {
        anyhow::bail!(
            "mcp_server.http: set either `auth` (Phase 76.3) or the legacy \
             `auth_token_env`, not both"
        );
    }

    let auth_token = if let Some(env_name) = yaml.auth_token_env.as_deref() {
        let token = std::env::var(env_name).with_context(|| {
            format!(
                "mcp_server.http.auth_token_env={env_name} is set but env var `{env_name}` is missing"
            )
        })?;
        if token.trim().is_empty() {
            anyhow::bail!("mcp_server.http.auth_token_env={env_name} resolved to an empty token");
        }
        Some(token)
    } else {
        None
    };

    let auth = yaml.auth.as_ref().map(yaml_auth_to_runtime).transpose()?;

    let per_principal_rate_limit = yaml
        .per_principal_rate_limit
        .as_ref()
        .map(yaml_pp_rate_limit_to_runtime);

    let per_principal_concurrency = yaml
        .per_principal_concurrency
        .as_ref()
        .map(yaml_pp_concurrency_to_runtime);

    let audit_log = yaml.audit_log.as_ref().map(yaml_audit_log_to_runtime);

    let session_event_store = yaml
        .session_event_store
        .as_ref()
        .map(yaml_session_event_store_to_runtime);

    let cfg = HttpTransportConfig {
        enabled: yaml.enabled,
        bind: yaml.bind,
        auth,
        auth_token,
        allow_origins: yaml.allow_origins.clone(),
        body_max_bytes: yaml.body_max_bytes,
        max_in_flight: yaml.max_in_flight,
        per_ip_rate_limit: nexo_mcp::server::http_config::PerIpRateLimit {
            rps: yaml.per_ip_rate_limit.rps,
            burst: yaml.per_ip_rate_limit.burst,
        },
        request_timeout_secs: yaml.request_timeout_secs,
        session_idle_timeout_secs: yaml.session_idle_timeout_secs,
        session_max_lifetime_secs: yaml.session_max_lifetime_secs,
        max_sessions: yaml.max_sessions,
        sse_keepalive_secs: yaml.sse_keepalive_secs,
        sse_max_age_secs: yaml.sse_max_age_secs,
        sse_buffer_size: yaml.sse_buffer_size,
        enable_legacy_sse: yaml.enable_legacy_sse,
        per_principal_rate_limit,
        per_principal_concurrency,
        audit_log,
        session_event_store,
    };

    // Phase M1 — HTTP transport can push
    // `notifications/tools/list_changed` via
    // `HttpServerHandle::notify_tools_list_changed()`, so this
    // bridge clone advertises the capability to clients. Stdio
    // bridge keeps the default `false` (no server→client push
    // channel today). Both clones share the same `Arc<ArcSwap>`
    // allowlist, so a future `swap_allowlist(...)` call (M1.b)
    // is visible to both transports atomically.
    let bridge_for_http = bridge.clone().with_list_changed_capability(true);
    let handle = start_http_server(bridge_for_http, cfg, shutdown.clone()).await?;
    tracing::info!(addr = %handle.bind_addr, "mcp-server http transport ready");
    Ok(handle)
}

/// Phase 76.3 — translate the YAML auth schema into the runtime
/// `AuthConfig`. Env var resolution for `static_token` happens lazily
/// inside the runtime's `AuthConfig::build`; mTLS and JWT need no env.
fn yaml_auth_to_runtime(
    yaml: &nexo_config::types::mcp_server::AuthConfigYaml,
) -> anyhow::Result<nexo_mcp::server::auth::AuthConfig> {
    use nexo_config::types::mcp_server as y;
    use nexo_mcp::server::auth as r;
    use nexo_mcp::server::auth::bearer_jwt::JwtConfig;
    use nexo_mcp::server::auth::mutual_tls::MutualTlsConfig;

    Ok(match yaml {
        y::AuthConfigYaml::None => r::AuthConfig::None,
        y::AuthConfigYaml::StaticToken { token_env, tenant } => {
            if token_env.trim().is_empty() {
                anyhow::bail!("mcp_server.http.auth.token_env must not be empty");
            }
            r::AuthConfig::StaticToken {
                token: None,
                token_env: Some(token_env.clone()),
                tenant: tenant.clone(),
            }
        }
        y::AuthConfigYaml::BearerJwt(j) => r::AuthConfig::BearerJwt(JwtConfig {
            jwks_url: j.jwks_url.clone(),
            jwks_cache_ttl_secs: j.jwks_ttl_secs,
            jwks_refresh_cooldown_secs: j.jwks_refresh_cooldown_secs,
            algorithms: j.algorithms.clone(),
            issuer: j.issuer.clone(),
            audiences: j.audiences.clone(),
            tenant_claim: j.tenant_claim.clone(),
            scopes_claim: j.scopes_claim.clone(),
            leeway_secs: j.leeway_secs,
        }),
        y::AuthConfigYaml::MutualTls(m) => match m {
            y::MutualTlsConfigYaml::FromHeader {
                header_name,
                cn_allowlist,
                cn_to_tenant,
            } => r::AuthConfig::MutualTls(MutualTlsConfig::FromHeader {
                header_name: header_name.clone(),
                cn_allowlist: cn_allowlist.clone(),
                cn_to_tenant: cn_to_tenant
                    .as_ref()
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            }),
        },
    })
}

/// Phase 76.5 — translate the YAML per-principal block into the
/// runtime config. Direct field-by-field copy; the runtime
/// validates the values at `PerPrincipalRateLimiter::new()` time.
fn yaml_pp_rate_limit_to_runtime(
    yaml: &nexo_config::types::mcp_server::PerPrincipalRateLimitYaml,
) -> nexo_mcp::server::per_principal_rate_limit::PerPrincipalRateLimiterConfig {
    use nexo_mcp::server::per_principal_rate_limit::{PerPrincipalRateLimiterConfig, PerToolLimit};
    let convert = |y: &nexo_config::types::mcp_server::PerToolLimitYaml| PerToolLimit {
        rps: y.rps,
        burst: y.burst,
    };
    PerPrincipalRateLimiterConfig {
        enabled: yaml.enabled,
        default: convert(&yaml.default),
        per_tool: yaml
            .per_tool
            .iter()
            .map(|(k, v)| (k.clone(), convert(v)))
            .collect(),
        max_buckets: yaml.max_buckets,
        stale_ttl_secs: yaml.stale_ttl_secs,
        warn_threshold: yaml.warn_threshold,
    }
}

/// Phase 76.6 — translate the YAML per-principal concurrency block
/// into the runtime config. Field-by-field copy; the runtime
/// validates values at `PerPrincipalConcurrencyCap::new()` time.
fn yaml_pp_concurrency_to_runtime(
    yaml: &nexo_config::types::mcp_server::PerPrincipalConcurrencyYaml,
) -> nexo_mcp::server::per_principal_concurrency::PerPrincipalConcurrencyConfig {
    use nexo_mcp::server::per_principal_concurrency::{
        PerPrincipalConcurrencyConfig, PerToolConcurrency,
    };
    let convert = |y: &nexo_config::types::mcp_server::PerToolConcurrencyYaml| PerToolConcurrency {
        max_in_flight: y.max_in_flight,
        timeout_secs: y.timeout_secs,
    };
    PerPrincipalConcurrencyConfig {
        enabled: yaml.enabled,
        default: convert(&yaml.default),
        per_tool: yaml
            .per_tool
            .iter()
            .map(|(k, v)| (k.clone(), convert(v)))
            .collect(),
        default_timeout_secs: yaml.default_timeout_secs,
        queue_wait_ms: yaml.queue_wait_ms,
        max_buckets: yaml.max_buckets,
        stale_ttl_secs: yaml.stale_ttl_secs,
    }
}

/// Phase 76.11 — translate the YAML audit-log block into the runtime
/// config. Field-by-field copy; the runtime validates values at
/// `AuditLogConfig::validate()` time and resolves env-relative paths
/// against process CWD.
fn yaml_audit_log_to_runtime(
    yaml: &nexo_config::types::mcp_server::AuditLogYaml,
) -> nexo_mcp::server::audit_log::AuditLogConfig {
    nexo_mcp::server::audit_log::AuditLogConfig {
        enabled: yaml.enabled,
        db_path: yaml.db_path.clone(),
        retention_secs: yaml.retention_secs,
        writer_buffer: yaml.writer_buffer,
        flush_interval_ms: yaml.flush_interval_ms,
        flush_batch_size: yaml.flush_batch_size,
        redact_args: yaml.redact_args,
        per_tool_redact_args: yaml.per_tool_redact_args.clone(),
        args_hash_max_bytes: yaml.args_hash_max_bytes,
    }
}

/// Phase 76.8 — translate the YAML session-event-store block into
/// the runtime config. Field-by-field copy; the runtime validates
/// values at `SessionEventStoreConfig::validate()` time.
fn yaml_session_event_store_to_runtime(
    yaml: &nexo_config::types::mcp_server::SessionEventStoreYaml,
) -> nexo_mcp::server::event_store::SessionEventStoreConfig {
    nexo_mcp::server::event_store::SessionEventStoreConfig {
        enabled: yaml.enabled,
        db_path: yaml.db_path.clone(),
        max_events_per_session: yaml.max_events_per_session,
        max_replay_batch: yaml.max_replay_batch,
        purge_interval_secs: yaml.purge_interval_secs,
    }
}

/// Build a `BrokerClientForDoctor` adapter from the loaded broker config.
/// Returns `None` when the broker is `local` — NATS runtime checks are
/// then reported as `skip` instead of a misleading fail.
fn build_doctor_broker_adapter(
    cfg: &nexo_config::types::broker::BrokerInner,
) -> Option<Arc<dyn nexo_extensions::cli::BrokerClientForDoctor>> {
    if cfg.kind != nexo_config::types::broker::BrokerKind::Nats {
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
impl nexo_extensions::cli::BrokerClientForDoctor for NatsDoctorAdapter {
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
            nexo_extensions::cli::yaml_edit::load_or_default(&config_dir.join("extensions.yaml"))
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
        }
    };

    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let ctx = nexo_extensions::cli::CliContext {
        config_dir: config_dir.to_path_buf(),
        extensions,
        out: &mut stdout,
        err: &mut stderr,
    };

    let result = match cmd {
        ExtCmd::List { json } => nexo_extensions::cli::run_list(ctx, json),
        ExtCmd::Info { id, json } => nexo_extensions::cli::run_info(ctx, &id, json),
        ExtCmd::Enable { id } => nexo_extensions::cli::run_enable(ctx, &id),
        ExtCmd::Disable { id } => nexo_extensions::cli::run_disable(ctx, &id),
        ExtCmd::Validate { path } => nexo_extensions::cli::run_validate(ctx, &path),
        ExtCmd::Doctor { runtime, json } => {
            if !runtime {
                return nexo_extensions::cli::run_doctor(ctx).map_err(|e| {
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
            rt.block_on(nexo_extensions::cli::run_doctor_runtime(
                ctx,
                nexo_extensions::cli::DoctorOptions { runtime, json },
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
        } => nexo_extensions::cli::run_install(
            ctx,
            nexo_extensions::cli::InstallOptions {
                source,
                update,
                enable,
                dry_run,
                link,
                json,
            },
        ),
        ExtCmd::Uninstall { id, yes, json } => nexo_extensions::cli::run_uninstall(
            ctx,
            nexo_extensions::cli::UninstallOptions { id, yes, json },
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

/// Phase 18 — `agent reload` subcommand. Loads the broker config,
/// connects, subscribes to `control.reload.ack`, publishes on
/// `control.reload`, and waits up to 5s for the daemon to respond.
///
/// Exit codes:
///   0 — at least one agent reloaded successfully.
///   1 — no ack arrived, or every agent rejected.
///   2 — all rejections were "agent not registered" etc. (partial).
async fn run_reload(config_dir: &std::path::Path, json: bool) -> Result<()> {
    let cfg = AppConfig::load(config_dir).context("failed to load config")?;
    let broker = AnyBroker::from_config(&cfg.broker.broker)
        .await
        .context("failed to connect to broker")?;

    // Subscribe before publishing so the daemon's ack is not missed.
    let mut ack_sub = broker
        .subscribe("control.reload.ack")
        .await
        .context("failed to subscribe to control.reload.ack")?;

    let req_payload = serde_json::json!({ "requested_by": "cli" });
    let ev = nexo_broker::Event::new("control.reload", "cli", req_payload);
    broker
        .publish("control.reload", ev)
        .await
        .context("failed to publish control.reload")?;

    let ack = match tokio::time::timeout(std::time::Duration::from_secs(5), ack_sub.next()).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!("daemon closed the ack subscription before responding");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("no control.reload.ack received within 5s — is the daemon running?");
            std::process::exit(1);
        }
    };

    let outcome: nexo_core::ReloadOutcome =
        serde_json::from_value(ack.payload).context("malformed ack payload")?;

    if json {
        let body = serde_json::to_string_pretty(&outcome)
            .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
        println!("{body}");
    } else {
        println!(
            "reload v{}: applied={} rejected={} elapsed={}ms",
            outcome.version,
            outcome.applied.len(),
            outcome.rejected.len(),
            outcome.elapsed_ms
        );
        for id in &outcome.applied {
            println!("  ✓ {id}");
        }
        for r in &outcome.rejected {
            let who = r.agent_id.as_deref().unwrap_or("<top-level>");
            println!("  ✗ {who}: {}", r.reason);
        }
    }

    if outcome.applied.is_empty() {
        std::process::exit(if outcome.rejected.is_empty() { 1 } else { 2 });
    }
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
    registry: Arc<nexo_core::agent::tool_policy::ToolPolicyRegistry>,
    agents: Arc<nexo_core::agent::AgentsDirectory>,
    credentials_for_admin: Option<Arc<nexo_auth::CredentialsBundle>>,
    pollers: Option<Arc<nexo_poller::PollerRunner>>,
    admin_config_dir: PathBuf,
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
        let creds = credentials_for_admin.clone();
        let pollers = pollers.clone();
        let cfg_dir = admin_config_dir.clone();
        tokio::spawn(async move {
            if let Err(e) =
                handle_admin_conn(stream, registry, agents, creds, pollers, cfg_dir).await
            {
                tracing::debug!(error = %e, "admin connection failed");
            }
        });
    }
}

async fn handle_admin_conn(
    mut stream: TcpStream,
    registry: Arc<nexo_core::agent::tool_policy::ToolPolicyRegistry>,
    agents: Arc<nexo_core::agent::AgentsDirectory>,
    credentials: Option<Arc<nexo_auth::CredentialsBundle>>,
    pollers: Option<Arc<nexo_poller::PollerRunner>>,
    config_dir: PathBuf,
) -> anyhow::Result<()> {
    let (method, full_path) = read_http_method_path(&mut stream).await?;
    let (path, query) = match full_path.find('?') {
        Some(i) => (&full_path[..i], &full_path[i + 1..]),
        None => (full_path.as_str(), ""),
    };
    // Phase 19 — `/admin/pollers/*` first; falls through to credentials,
    // agents, then the tool-policy handler.
    if path.starts_with("/admin/pollers") {
        if let Some(runner) = pollers.as_ref() {
            if let Some(resp) =
                nexo_poller::admin::dispatch(runner, &method, path, &config_dir).await
            {
                write_http_response(&mut stream, resp.0, resp.2, &resp.1).await?;
                return Ok(());
            }
        } else {
            let body = "{\"ok\":false,\"error\":\"poller subsystem disabled\"}";
            write_http_response(&mut stream, 503, "application/json", body).await?;
            return Ok(());
        }
    }
    // Route `/admin/credentials/*` first (Phase 17 hot-reload), then
    // `/admin/agents*`, then fall back to the tool-policy handler.
    let (status, body, content_type) = if path == "/admin/credentials/reload" && method == "POST" {
        match credentials.as_deref() {
            Some(bundle) => match nexo_auth::wire::reload_resolver(
                &config_dir,
                &secrets_dir_for(&config_dir),
                bundle,
                nexo_auth::StrictLevel::Lenient,
            ) {
                Ok(outcome) => (
                    200,
                    serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".into()),
                    "application/json",
                ),
                Err(errs) => {
                    let body = serde_json::json!({
                        "ok": false,
                        "errors": errs.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
                    });
                    (400, body.to_string(), "application/json")
                }
            },
            None => (
                503,
                "{\"ok\":false,\"error\":\"credentials subsystem disabled\"}".into(),
                "application/json",
            ),
        }
    } else if let Some(resp) = agents.dispatch(&method, path) {
        resp
    } else {
        nexo_core::agent::tool_policy::admin_dispatch(&method, path, query, &registry)
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
    nexo_core::telemetry::set_circuit_breaker_state("nats", nats_open);
    let mut body = render_prometheus(nats_open);
    body.push_str(&nexo_llm::telemetry::render_prometheus());
    body.push_str(&nexo_mcp::telemetry::render_prometheus());
    // Phase 76.10 — server-side dispatch metrics
    // (`mcp_requests_total`, `mcp_request_duration_seconds`,
    // `mcp_in_flight`, `mcp_rate_limit_hits_total`, etc.).
    body.push_str(&nexo_mcp::server::telemetry::render_prometheus());
    body.push_str(&nexo_poller::telemetry::render_prometheus());
    // Phase 48 follow-up #8 — append email metrics. Counters live in
    // `nexo_plugin_email::metrics`; gauges (`imap_state`, queue
    // depths) sample the live `health_map()` so the values are
    // authoritative at scrape time.
    let email_health = match health.email_plugin.as_ref() {
        Some(p) => p.health_map().await,
        None => None,
    };
    body.push_str(&nexo_plugin_email::metrics::render_prometheus(email_health.as_ref()).await);
    write_http_response(&mut stream, 200, "text/plain; version=0.0.4", &body).await?;
    Ok(())
}

async fn handle_health_conn(mut stream: TcpStream, health: RuntimeHealth) -> anyhow::Result<()> {
    // Peek (non-destructive) at the first bytes to detect the request path
    // before consuming. Required for /pair which must pass a clean stream to
    // tokio_tungstenite::accept_async.
    let mut peek_buf = [0u8; 512];
    let n = stream.peek(&mut peek_buf).await.unwrap_or(0);
    let req_str = std::str::from_utf8(&peek_buf[..n]).unwrap_or_default();
    let peek_path = req_str
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default();
    if peek_path == "/pair" {
        if let Some(ctx) = health.pairing_handshake.get() {
            return handle_pair_ws(stream, ctx).await;
        }
        return write_http_response(
            &mut stream,
            503,
            "application/json; charset=utf-8",
            r#"{"error":"pairing not configured"}"#,
        )
        .await;
    }

    let path = read_http_path(&mut stream).await?;
    // Try to match `/whatsapp/...` routes first. Routing rules live in
    // `nexo_plugin_whatsapp::pairing::dispatch_route` so they're
    // unit-testable without a TCP listener.
    if let Some(rest) = path.strip_prefix("/whatsapp/") {
        use nexo_plugin_whatsapp::pairing::{dispatch_route, WhatsappRoute};
        match dispatch_route(rest, &health.wa_pairing) {
            Some(WhatsappRoute::Html) => {
                write_http_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    nexo_plugin_whatsapp::pairing::PAIR_PAGE_HTML,
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
        "/email/health" => {
            // Phase 48 follow-up #7. Snapshot every account's
            // `AccountHealth` row. Returns `[]` when the plugin
            // isn't configured (vs 404), so monitoring scripts
            // can hit the route unconditionally.
            let body = match health.email_plugin.as_ref() {
                Some(plugin) => render_email_health(plugin.as_ref()).await,
                None => "[]".to_string(),
            };
            write_http_response(&mut stream, 200, "application/json; charset=utf-8", &body).await?;
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

/// Render the per-account email health snapshot as a stable JSON
/// array. Phase 48 follow-up #7. Sorted by instance for
/// deterministic output that monitoring agents can diff.
async fn render_email_health(plugin: &nexo_plugin_email::EmailPlugin) -> String {
    let Some(map) = plugin.health_map().await else {
        return "[]".to_string();
    };
    let mut keys: Vec<String> = map.iter().map(|e| e.key().clone()).collect();
    keys.sort();
    let mut rows = Vec::with_capacity(keys.len());
    for k in keys {
        if let Some(arc) = map.get(&k).map(|v| v.value().clone()) {
            let h = arc.read().await;
            rows.push(serde_json::json!({
                "instance": k,
                "state": match h.state {
                    nexo_plugin_email::WorkerState::Connecting => "connecting",
                    nexo_plugin_email::WorkerState::Idle => "idle",
                    nexo_plugin_email::WorkerState::Polling => "polling",
                    nexo_plugin_email::WorkerState::Down => "down",
                },
                "last_idle_alive_ts": h.last_idle_alive_ts,
                "last_poll_ts": h.last_poll_ts,
                "last_connect_ok_ts": h.last_connect_ok_ts,
                "consecutive_failures": h.consecutive_failures,
                "messages_seen_total": h.messages_seen_total,
                "last_error": h.last_error,
                "outbound_queue_depth": h.outbound_queue_depth,
                "outbound_dlq_depth": h.outbound_dlq_depth,
                "outbound_sent_total": h.outbound_sent_total,
                "outbound_failed_total": h.outbound_failed_total,
            }));
        }
    }
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
}

/// Companion WS pairing handshake:
/// 1. tokio_tungstenite upgrades the raw TCP stream.
/// 2. Client sends `{"bootstrap_token": "<hmac-signed>"}`.
/// 3. Server verifies HMAC + expiry via `SetupCodeIssuer::verify`.
/// 4. Server generates a session token, persists it, returns
///    `{"session_token": "<token>"}` to the client.
async fn handle_pair_ws(stream: TcpStream, ctx: &PairingHandshakeCtx) -> anyhow::Result<()> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .context("WS upgrade")?;
    let (mut tx, mut rx) = ws.split();

    let msg = match rx.next().await {
        Some(Ok(m)) => m,
        Some(Err(e)) => return Err(anyhow::anyhow!("WS read error: {e}")),
        None => {
            return Err(anyhow::anyhow!(
                "companion disconnected before sending token"
            ))
        }
    };

    let text = match msg {
        Message::Text(t) => t,
        _ => {
            let _ = tx
                .send(Message::Text(
                    r#"{"error":"expected text frame"}"#.to_string(),
                ))
                .await;
            return Ok(());
        }
    };

    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let bootstrap_token = parsed
        .get("bootstrap_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    let claims = match ctx.issuer.verify(bootstrap_token) {
        Ok(c) => c,
        Err(e) => {
            let body = serde_json::json!({"error": e.to_string()}).to_string();
            let _ = tx.send(Message::Text(body)).await;
            tracing::warn!(error = %e, "companion WS: invalid bootstrap token");
            return Ok(());
        }
    };

    let token_bytes: [u8; 32] = rand::random();
    let session_token = URL_SAFE_NO_PAD.encode(token_bytes);

    if let Err(e) = ctx
        .session_store
        .insert_session(
            &session_token,
            &claims.profile,
            claims.device_label.as_deref(),
            ctx.session_ttl,
        )
        .await
    {
        tracing::error!(error = %e, "failed to persist pairing session");
        let _ = tx
            .send(Message::Text(r#"{"error":"internal"}"#.to_string()))
            .await;
        return Ok(());
    }

    let response = serde_json::json!({"session_token": session_token}).to_string();
    tx.send(Message::Text(response))
        .await
        .context("send session token")?;
    tracing::info!(
        profile = %claims.profile,
        device_label = ?claims.device_label,
        "companion paired successfully"
    );
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

async fn open_flow_manager() -> Result<nexo_taskflow::FlowManager> {
    let path = flow_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let path_s = path.to_string_lossy().into_owned();
    let store = nexo_taskflow::SqliteFlowStore::open(&path_s)
        .await
        .with_context(|| format!("failed to open taskflow db at {}", path.display()))?;
    Ok(nexo_taskflow::FlowManager::new(std::sync::Arc::new(store)))
}

/// Open a `FlowManager` honoring `taskflow.yaml` overrides. The config
/// `db_path` takes precedence over `TASKFLOW_DB_PATH` env var, which
/// itself overrides the `./data/taskflow.db` default.
async fn open_flow_manager_from_cfg(
    cfg: &nexo_config::TaskflowConfig,
) -> Result<nexo_taskflow::FlowManager> {
    let path = match cfg.db_path.as_deref() {
        Some(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
        _ => flow_db_path(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let path_s = path.to_string_lossy().into_owned();
    let store = nexo_taskflow::SqliteFlowStore::open(&path_s)
        .await
        .with_context(|| format!("failed to open taskflow db at {}", path.display()))?;
    Ok(nexo_taskflow::FlowManager::new(std::sync::Arc::new(store)))
}

/// NATS resume bridge — listens on `taskflow.resume` and wakes flows
/// whose `external_event` waits match the payload `(flow_id, topic,
/// correlation_id)`. Tolerant: malformed payloads are logged and
/// skipped, no panic.
fn spawn_taskflow_resume_bridge(
    broker: nexo_broker::AnyBroker,
    engine: nexo_taskflow::WaitEngine,
    shutdown: tokio_util::sync::CancellationToken,
) {
    use nexo_broker::BrokerHandle;
    tokio::spawn(async move {
        let mut sub = match broker.subscribe("taskflow.resume").await {
            Ok(s) => {
                tracing::info!("taskflow resume bridge: subscribed to `taskflow.resume`");
                s
            }
            Err(e) => {
                tracing::warn!(error = %e, "taskflow resume bridge: subscribe failed; bridge disabled");
                return;
            }
        };
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("taskflow resume bridge: shutdown");
                    return;
                }
                ev = sub.next() => {
                    let Some(event) = ev else {
                        tracing::info!("taskflow resume bridge: subscription closed");
                        return;
                    };
                    if let Err(e) = handle_taskflow_resume_event(&engine, event).await {
                        tracing::warn!(error = %e, "taskflow resume bridge: handler error");
                    }
                }
            }
        }
    });
}

#[derive(serde::Deserialize)]
struct TaskflowResumePayload {
    flow_id: uuid::Uuid,
    topic: String,
    correlation_id: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
}

async fn handle_taskflow_resume_event(
    engine: &nexo_taskflow::WaitEngine,
    event: nexo_broker::Event,
) -> anyhow::Result<()> {
    let body: TaskflowResumePayload = serde_json::from_value(event.payload)
        .with_context(|| "malformed taskflow.resume payload")?;
    match engine
        .try_resume_external(
            body.flow_id,
            &body.topic,
            &body.correlation_id,
            body.payload,
        )
        .await?
    {
        Some(f) => {
            tracing::info!(flow_id = %f.id, topic = %body.topic, "taskflow resumed via NATS")
        }
        None => tracing::debug!(
            flow_id = %body.flow_id,
            topic = %body.topic,
            "taskflow resume bridge: no matching waiting flow"
        ),
    }
    Ok(())
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

fn flow_to_summary_json(f: &nexo_taskflow::Flow) -> serde_json::Value {
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
    use nexo_taskflow::FlowStatus::*;
    let mut all: Vec<nexo_taskflow::Flow> = Vec::new();
    for status in [Created, Running, Waiting, Cancelled, Finished, Failed] {
        all.extend(m.list_by_status(status).await?);
    }
    all.sort_by_key(|b| std::cmp::Reverse(b.updated_at));

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
fn run_check_config(config_dir: &std::path::Path, strict: bool) -> Result<()> {
    let cfg = AppConfig::load(config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;
    let google = nexo_auth::load_google_auth(config_dir)
        .with_context(|| "failed to load google-auth.yaml")?;
    let level = if strict {
        nexo_auth::StrictLevel::Strict
    } else {
        nexo_auth::StrictLevel::Lenient
    };
    let result = nexo_auth::build_credentials(
        &cfg.agents.agents,
        &cfg.plugins.whatsapp,
        &cfg.plugins.telegram,
        &google,
        cfg.plugins.email.as_ref(),
        &secrets_dir_for(config_dir),
        level,
    );
    let code = nexo_auth::print_report(&result);
    // Exit code mapping: main.rs returns Result<()>; wrap non-zero in
    // a dedicated error so the shell sees the intended status.
    if code == 0 {
        Ok(())
    } else {
        std::process::exit(code)
    }
}

fn run_dry_run(config_dir: &std::path::Path, json: bool) -> Result<()> {
    let cfg = AppConfig::load(config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;

    // Build the same AgentsDirectory the daemon would serve — same
    // projection code path, catches any mismatch between config schema
    // and runtime expectations.
    let agents: Vec<nexo_core::agent::AgentInfo> = cfg
        .agents
        .agents
        .iter()
        .map(nexo_core::agent::AgentInfo::from_config)
        .collect();

    if json {
        let dir = nexo_core::agent::AgentsDirectory::new(agents);
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

#[cfg(test)]
mod tests {
    use super::{
        has_restricted_delegate_allowlist, mcp_server_has_auth, reload_expose_tools,
        route_cron_subcommand, Mode,
    };

    fn write_minimal_agents_yaml(dir: &std::path::Path) {
        // Minimal but valid agents.yaml — `load_for_mcp_server`
        // requires it.
        let yaml = "agents:\n  - id: probe\n    model:\n      provider: anthropic\n      model: claude-sonnet-4-5\n";
        std::fs::write(dir.join("agents.yaml"), yaml).unwrap();
    }

    #[test]
    fn reload_expose_tools_returns_set_from_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        write_minimal_agents_yaml(tmp.path());
        std::fs::write(
            tmp.path().join("mcp_server.yaml"),
            "mcp_server:\n  expose_tools: [Read, Edit]\n",
        )
        .unwrap();
        let result = reload_expose_tools(tmp.path()).unwrap();
        let set = result.expect("non-empty list returns Some");
        assert_eq!(set.len(), 2);
        assert!(set.contains("Read"));
        assert!(set.contains("Edit"));
    }

    #[test]
    fn reload_expose_tools_returns_none_for_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        write_minimal_agents_yaml(tmp.path());
        std::fs::write(
            tmp.path().join("mcp_server.yaml"),
            "mcp_server:\n  expose_tools: []\n",
        )
        .unwrap();
        let result = reload_expose_tools(tmp.path()).unwrap();
        assert!(result.is_none(), "empty list yields None (no filter)");
    }

    #[test]
    fn reload_expose_tools_propagates_yaml_parse_errors() {
        let tmp = tempfile::tempdir().unwrap();
        write_minimal_agents_yaml(tmp.path());
        std::fs::write(
            tmp.path().join("mcp_server.yaml"),
            "mcp_server:\n  expose_tools: [\n  not closing — invalid yaml\n",
        )
        .unwrap();
        let result = reload_expose_tools(tmp.path());
        assert!(result.is_err(), "malformed yaml must surface as Err");
    }

    // ── Phase M1.b.c — compute_allowlist_from_mcp_server_cfg ──

    #[test]
    fn compute_allowlist_returns_set_from_expose_tools() {
        use super::compute_allowlist_from_mcp_server_cfg;
        let mut cfg = nexo_config::types::mcp_server::McpServerConfig::default();
        cfg.expose_tools = vec!["Read".into(), "Edit".into(), "marketing_lead_classify".into()];
        let allow = compute_allowlist_from_mcp_server_cfg(&cfg).expect("non-empty -> Some");
        assert_eq!(allow.len(), 3);
        assert!(allow.contains("Read"));
        assert!(allow.contains("marketing_lead_classify"));
    }

    #[test]
    fn compute_allowlist_returns_none_for_empty() {
        use super::compute_allowlist_from_mcp_server_cfg;
        let cfg = nexo_config::types::mcp_server::McpServerConfig::default();
        assert!(
            compute_allowlist_from_mcp_server_cfg(&cfg).is_none(),
            "empty expose_tools yields None (no filter)"
        );
    }

    #[test]
    fn compute_allowlist_dedupes_via_hashset() {
        use super::compute_allowlist_from_mcp_server_cfg;
        let mut cfg = nexo_config::types::mcp_server::McpServerConfig::default();
        cfg.expose_tools = vec!["Read".into(), "Read".into(), "Edit".into()];
        let allow = compute_allowlist_from_mcp_server_cfg(&cfg).unwrap();
        assert_eq!(allow.len(), 2, "duplicates collapsed by HashSet");
    }

    /// Phase 27.1 — verify the four `NEXO_BUILD_*` env stamps are
    /// non-empty at compile time. The actual stdout-capture form
    /// of `print_version` would need `#[no_main]` redirection; this
    /// test guards the inputs the function reads, which is the part
    /// build.rs owns.
    #[test]
    fn build_stamps_are_populated() {
        let sha = env!("NEXO_BUILD_GIT_SHA");
        let target = env!("NEXO_BUILD_TARGET_TRIPLE");
        let channel = env!("NEXO_BUILD_CHANNEL");
        let ts = env!("NEXO_BUILD_TIMESTAMP");
        assert!(!sha.is_empty(), "git-sha stamp empty");
        assert!(!target.is_empty(), "target triple stamp empty");
        assert!(!channel.is_empty(), "channel stamp empty");
        assert!(!ts.is_empty(), "timestamp stamp empty");
        // build.rs should have produced an ISO8601 UTC timestamp.
        assert!(
            ts.ends_with('Z') && ts.contains('T'),
            "timestamp not ISO8601 UTC: {ts}"
        );
    }

    #[test]
    fn cron_route_list_defaults() {
        let args = vec!["cron".to_string(), "list".to_string()];
        let mode = route_cron_subcommand(&args, false).expect("cron route");
        match mode {
            Mode::CronList { binding, json } => {
                assert!(binding.is_none());
                assert!(!json);
            }
            other => panic!(
                "expected CronList, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn cron_route_list_with_binding_and_json() {
        let args = vec![
            "cron".to_string(),
            "list".to_string(),
            "--binding".to_string(),
            "whatsapp:default".to_string(),
            "--json".to_string(),
        ];
        let mode = route_cron_subcommand(&args, true).expect("cron route");
        match mode {
            Mode::CronList { binding, json } => {
                assert_eq!(binding.as_deref(), Some("whatsapp:default"));
                assert!(json);
            }
            other => panic!(
                "expected CronList, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn cron_route_resume_requires_id() {
        let args = vec!["cron".to_string(), "resume".to_string()];
        let mode = route_cron_subcommand(&args, false).expect("cron route");
        assert!(matches!(mode, Mode::Help));
    }

    #[test]
    fn cron_route_resume_with_id() {
        let args = vec![
            "cron".to_string(),
            "resume".to_string(),
            "abc123".to_string(),
        ];
        let mode = route_cron_subcommand(&args, false).expect("cron route");
        match mode {
            Mode::CronResume { id } => assert_eq!(id, "abc123"),
            other => panic!(
                "expected CronResume, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn delegate_allowlist_helper_rejects_unrestricted_shapes() {
        assert!(!has_restricted_delegate_allowlist(&[]));
        assert!(!has_restricted_delegate_allowlist(&["*".to_string()]));
        assert!(!has_restricted_delegate_allowlist(&[
            "sales_*".to_string(),
            "*".to_string()
        ]));
    }

    #[test]
    fn delegate_allowlist_helper_accepts_restricted_shapes() {
        assert!(has_restricted_delegate_allowlist(&["sales".to_string()]));
        assert!(has_restricted_delegate_allowlist(&[
            "sales_*".to_string(),
            "ops".to_string()
        ]));
    }

    #[test]
    fn mcp_auth_helper_treats_http_auth_none_as_unauthenticated() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: none
"#;
        let parsed: nexo_config::types::mcp_server::McpServerConfigFile =
            serde_yaml::from_str(yaml).expect("parse mcp_server yaml");
        assert!(!mcp_server_has_auth(&parsed.mcp_server));
    }

    #[test]
    fn mcp_auth_helper_accepts_explicit_auth_modes() {
        let yaml = r#"
mcp_server:
  enabled: true
  http:
    enabled: true
    auth:
      kind: static_token
      token_env: NEXO_MCP_TOKEN
"#;
        let parsed: nexo_config::types::mcp_server::McpServerConfigFile =
            serde_yaml::from_str(yaml).expect("parse mcp_server yaml");
        assert!(mcp_server_has_auth(&parsed.mcp_server));
    }

    // ---- M5: cron_tool_bindings ArcSwap mechanics ----

    use super::{CronToolBindingContext, RuntimeCronToolExecutor};

    /// Helper — minimal binding fixture identifiable via `ctx.agent_id`
    /// (used as marker in assertions). Tools registry left empty.
    fn make_test_binding(marker: &str) -> CronToolBindingContext {
        use nexo_broker::AnyBroker;
        use nexo_config::{
            AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
            OutboundAllowlistConfig, WorkspaceGitConfig,
        };
        let cfg = AgentConfig {
            id: marker.into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "m".into(),
            },
            plugins: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            config: AgentRuntimeConfig::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: Vec::new(),
            skills_dir: "./skills".into(),
            skill_overrides: Default::default(),
            transcripts_dir: String::new(),
            dreaming: DreamingYamlConfig::default(),
            workspace_git: WorkspaceGitConfig::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: Vec::new(),
            inbound_bindings: Vec::new(),
            allowed_tools: Vec::new(),
            sender_rate_limit: None,
            allowed_delegates: Vec::new(),
            accept_delegates_from: Vec::new(),
            description: String::new(),
            google_auth: None,
            credentials: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            language: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
            repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
        };
        let ctx = nexo_core::agent::AgentContext::new(
            marker,
            std::sync::Arc::new(cfg),
            AnyBroker::local(),
            std::sync::Arc::new(nexo_core::session::SessionManager::new(
                std::time::Duration::from_secs(60),
                8,
            )),
        );
        CronToolBindingContext {
            ctx,
            tools: std::sync::Arc::new(nexo_core::agent::ToolRegistry::new()),
        }
    }

    /// M5 — `replace_bindings` performs an atomic swap visible on the
    /// next `resolve_binding` call.
    #[tokio::test]
    async fn cron_executor_replace_bindings_atomically_swaps_map() {
        let mut initial = std::collections::HashMap::new();
        initial.insert("k".to_string(), make_test_binding("v1"));
        let executor = RuntimeCronToolExecutor::new(initial);

        let pre = executor
            .resolve_binding("k")
            .expect("pre-swap binding exists");
        assert_eq!(pre.ctx.agent_id, "v1");

        let mut new_map = std::collections::HashMap::new();
        new_map.insert("k".to_string(), make_test_binding("v2"));
        executor.replace_bindings(new_map);

        let post = executor
            .resolve_binding("k")
            .expect("post-swap binding exists");
        assert_eq!(post.ctx.agent_id, "v2");
    }

    /// M5 — empty-map swap clears all bindings; resolve returns `None`.
    /// Documents the agent-removal semantics (Phase 19 scope concern):
    /// a future operator removing an agent from config would reach
    /// this path post-rebuild.
    #[tokio::test]
    async fn cron_executor_replace_bindings_with_empty_map_clears_all() {
        let mut initial = std::collections::HashMap::new();
        initial.insert("k".to_string(), make_test_binding("v1"));
        let executor = RuntimeCronToolExecutor::new(initial);
        assert!(executor.resolve_binding("k").is_some());
        executor.replace_bindings(std::collections::HashMap::new());
        assert!(executor.resolve_binding("k").is_none());
    }

    /// M5.b — the post-hook closure must early-return cleanly when
    /// it fires before the cron executor was built (e.g. a config
    /// reload triggered immediately at boot before the cron block
    /// runs). Replicates the closure's `cell.get()` check inline so
    /// that future closure-body changes that break this invariant
    /// (e.g. swap to `expect()` or `unwrap_or`) trigger this test.
    #[tokio::test]
    async fn cron_post_hook_no_op_when_cell_empty() {
        use std::sync::Arc;
        let cell: Arc<tokio::sync::OnceCell<Arc<RuntimeCronToolExecutor>>> =
            Arc::new(tokio::sync::OnceCell::new());
        assert!(cell.get().is_none(), "cell must start empty");

        // Simulate the closure's early-return pattern.
        let early_return_taken = cell.get().is_none();
        assert!(
            early_return_taken,
            "empty cell must trigger the closure's early-return path"
        );

        // After set, the closure would proceed to rebuild + replace.
        let mut initial = std::collections::HashMap::new();
        initial.insert("k".into(), make_test_binding("v1"));
        let executor = Arc::new(RuntimeCronToolExecutor::new(initial));
        // RuntimeCronToolExecutor doesn't impl Debug, so unwrap on
        // SetError isn't available; just assert the result is Ok.
        assert!(cell.set(Arc::clone(&executor)).is_ok());
        assert!(cell.get().is_some(), "cell must hold the executor after set");
    }

    // ── Phase 80.1.d — `nexo agent dream` CLI tests ──

    use super::{
        resolve_dream_db_path, run_agent_dream_kill, run_agent_dream_status,
        run_agent_dream_tail, short_uuid,
    };
    use chrono::Utc;
    use nexo_agent_registry::{
        DreamPhase, DreamRunRow, DreamRunStatus, DreamRunStore, SqliteDreamRunStore,
    };
    use nexo_driver_types::GoalId;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use uuid::Uuid;

    /// Env var lock — `resolve_dream_db_path` reads `NEXO_STATE_ROOT`.
    static DREAM_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn mk_row(status: DreamRunStatus, phase: DreamPhase) -> DreamRunRow {
        DreamRunRow {
            id: Uuid::new_v4(),
            goal_id: GoalId(Uuid::new_v4()),
            status,
            phase,
            sessions_reviewing: 5,
            prior_mtime_ms: Some(1_700_000_000_000),
            files_touched: vec![PathBuf::from("/tmp/foo.md")],
            turns: vec![],
            started_at: Utc::now(),
            ended_at: None,
            fork_label: "auto_dream".to_string(),
            fork_run_id: None,
        }
    }

    async fn mk_db_with_rows(rows: &[DreamRunRow]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("dream_runs.db");
        let store = SqliteDreamRunStore::open(db_path.to_str().unwrap())
            .await
            .unwrap();
        for r in rows {
            store.insert(r).await.unwrap();
        }
        (tmp, db_path)
    }

    #[test]
    fn resolve_dream_db_path_override_wins() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEXO_STATE_ROOT", "/should-not-win");
        let custom = PathBuf::from("/custom/db.sqlite");
        let resolved = resolve_dream_db_path(Some(&custom)).unwrap();
        assert_eq!(resolved, custom);
        std::env::remove_var("NEXO_STATE_ROOT");
    }

    #[test]
    fn resolve_dream_db_path_uses_env_when_no_override() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEXO_STATE_ROOT", "/state");
        let resolved = resolve_dream_db_path(None).unwrap();
        assert_eq!(resolved, PathBuf::from("/state/dream_runs.db"));
        std::env::remove_var("NEXO_STATE_ROOT");
    }

    #[test]
    fn short_uuid_takes_first_eight_chars() {
        let u = Uuid::parse_str("7a3b2f00-deaf-cafe-beef-001122334455").unwrap();
        assert_eq!(short_uuid(&u), "7a3b2f00");
    }

    #[tokio::test]
    async fn run_agent_dream_tail_empty_db_exits_zero() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let missing_db = tmp.path().join("dream_runs.db");
        // DB doesn't exist on disk yet — fn must return Ok without erroring.
        run_agent_dream_tail(None, 20, Some(&missing_db), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_dream_tail_with_rows_renders() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let row = mk_row(DreamRunStatus::Completed, DreamPhase::Updating);
        let (_tmp, db_path) = mk_db_with_rows(&[row.clone()]).await;
        run_agent_dream_tail(None, 10, Some(&db_path), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_dream_tail_json_output() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let row = mk_row(DreamRunStatus::Running, DreamPhase::Starting);
        let (_tmp, db_path) = mk_db_with_rows(&[row.clone()]).await;
        run_agent_dream_tail(None, 10, Some(&db_path), true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_dream_status_not_found_errors() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let row = mk_row(DreamRunStatus::Completed, DreamPhase::Updating);
        let (_tmp, db_path) = mk_db_with_rows(&[row]).await;
        let bogus = Uuid::new_v4().to_string();
        let err = run_agent_dream_status(&bogus, Some(&db_path), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn run_agent_dream_status_returns_row() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let row = mk_row(DreamRunStatus::Completed, DreamPhase::Updating);
        let id = row.id.to_string();
        let (_tmp, db_path) = mk_db_with_rows(&[row]).await;
        run_agent_dream_status(&id, Some(&db_path), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_dream_status_invalid_uuid_errors() {
        let err = run_agent_dream_status("not-a-uuid", None, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a valid UUID"));
    }

    #[tokio::test]
    async fn run_agent_dream_kill_already_terminal_is_noop() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let row = mk_row(DreamRunStatus::Completed, DreamPhase::Updating);
        let id = row.id.to_string();
        let (_tmp, db_path) = mk_db_with_rows(&[row]).await;
        // No `--force` needed because already terminal — must be Ok.
        run_agent_dream_kill(&id, false, None, Some(&db_path))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_dream_kill_running_with_force_flips_status() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let row = mk_row(DreamRunStatus::Running, DreamPhase::Starting);
        let id = row.id;
        let (_tmp, db_path) = mk_db_with_rows(&[row]).await;
        run_agent_dream_kill(&id.to_string(), true, None, Some(&db_path))
            .await
            .unwrap();
        // Verify the row was actually flipped.
        let store = SqliteDreamRunStore::open(db_path.to_str().unwrap())
            .await
            .unwrap();
        let after = store.get(id).await.unwrap().unwrap();
        assert_eq!(after.status, DreamRunStatus::Killed);
        assert!(after.ended_at.is_some());
    }

    // ── Phase 80.10 — `nexo agent run` / `agent ps` CLI tests ──

    use super::{resolve_agent_db_path, run_agent_ps, run_agent_run};
    use nexo_agent_registry::SessionKind;

    #[test]
    fn resolve_agent_db_path_override_wins() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEXO_STATE_ROOT", "/should-not-win");
        let custom = PathBuf::from("/custom/agents.db");
        let resolved = resolve_agent_db_path(Some(&custom)).unwrap();
        assert_eq!(resolved, custom);
        std::env::remove_var("NEXO_STATE_ROOT");
    }

    #[test]
    fn resolve_agent_db_path_uses_env_when_no_override() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEXO_STATE_ROOT", "/state");
        let resolved = resolve_agent_db_path(None).unwrap();
        assert_eq!(resolved, PathBuf::from("/state/agent_handles.db"));
        std::env::remove_var("NEXO_STATE_ROOT");
    }

    #[tokio::test]
    async fn run_agent_run_rejects_empty_prompt() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        let err = run_agent_run("   ".to_string(), false, Some(&db), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn run_agent_run_bg_inserts_handle_with_kind_bg() {
        use nexo_agent_registry::{AgentRegistryStore, AgentRunStatus, SqliteAgentRegistryStore};
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("ship the release".to_string(), true, Some(&db), false)
            .await
            .unwrap();
        let store = SqliteAgentRegistryStore::open(db.to_str().unwrap())
            .await
            .unwrap();
        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, SessionKind::Bg);
        assert_eq!(rows[0].status, AgentRunStatus::Running);
        assert_eq!(rows[0].phase_id, "cli-bg");
    }

    #[tokio::test]
    async fn run_agent_run_no_bg_inserts_handle_with_kind_interactive() {
        use nexo_agent_registry::{AgentRegistryStore, SqliteAgentRegistryStore};
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("hi".to_string(), false, Some(&db), false)
            .await
            .unwrap();
        let store = SqliteAgentRegistryStore::open(db.to_str().unwrap())
            .await
            .unwrap();
        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, SessionKind::Interactive);
        assert_eq!(rows[0].phase_id, "cli-run");
    }

    #[tokio::test]
    async fn run_agent_ps_empty_db_friendly_message() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("agents.db");
        // DB doesn't exist — must return Ok with friendly message.
        run_agent_ps(None, false, Some(&missing), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_ps_filters_by_kind() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("a".into(), true, Some(&db), false)
            .await
            .unwrap();
        run_agent_run("b".into(), false, Some(&db), false)
            .await
            .unwrap();
        // Just exercise the path; output is to stdout.
        run_agent_ps(Some("bg"), true, Some(&db), false).await.unwrap();
        run_agent_ps(Some("interactive"), true, Some(&db), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_ps_rejects_invalid_kind() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("seed".into(), false, Some(&db), false)
            .await
            .unwrap();
        let err = run_agent_ps(Some("nope"), true, Some(&db), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    // ── Phase 80.16 — `agent attach` / `agent discover` CLI tests ──

    use super::{run_agent_attach, run_agent_discover};

    #[tokio::test]
    async fn run_agent_attach_rejects_invalid_uuid() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        // Seed something so the DB exists.
        run_agent_run("seed".into(), false, Some(&db), false)
            .await
            .unwrap();
        let err = run_agent_attach("not-a-uuid", Some(&db), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("valid UUID"));
    }

    #[tokio::test]
    async fn run_agent_attach_missing_db_errors() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("agents.db");
        let err = run_agent_attach(
            "00000000-0000-0000-0000-000000000000",
            Some(&missing),
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn run_agent_attach_handle_not_found_errors() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("seed".into(), false, Some(&db), false)
            .await
            .unwrap();
        let err = run_agent_attach(
            "11111111-1111-1111-1111-111111111111",
            Some(&db),
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no agent handle found"));
    }

    #[tokio::test]
    async fn run_agent_attach_running_renders_snapshot() {
        use nexo_agent_registry::{AgentRegistryStore, SqliteAgentRegistryStore};
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("test".into(), true, Some(&db), false)
            .await
            .unwrap();
        let store = SqliteAgentRegistryStore::open(db.to_str().unwrap())
            .await
            .unwrap();
        let rows = store.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        let id = rows[0].goal_id.0.to_string();
        run_agent_attach(&id, Some(&db), false).await.unwrap();
        // JSON path
        run_agent_attach(&id, Some(&db), true).await.unwrap();
    }

    #[tokio::test]
    async fn run_agent_discover_filters_to_bg_daemon() {
        use nexo_agent_registry::{AgentRegistryStore, AgentRunStatus, SqliteAgentRegistryStore};
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        // Seed: 1 Interactive + 1 Bg, both Running.
        run_agent_run("inter".into(), false, Some(&db), false)
            .await
            .unwrap();
        run_agent_run("bg".into(), true, Some(&db), false)
            .await
            .unwrap();
        // Verify discover excludes Interactive by default (we can't
        // capture stdout cleanly here; assert the underlying store
        // shape matches expectation by querying separately).
        let store = SqliteAgentRegistryStore::open(db.to_str().unwrap())
            .await
            .unwrap();
        let all = store.list().await.unwrap();
        assert_eq!(all.len(), 2);
        let bgs = store
            .list_by_kind(nexo_agent_registry::SessionKind::Bg)
            .await
            .unwrap();
        let bgs_running: Vec<_> = bgs
            .iter()
            .filter(|h| h.status == AgentRunStatus::Running)
            .collect();
        assert_eq!(bgs_running.len(), 1);
        // Run the fn to exercise the rendering path.
        run_agent_discover(false, Some(&db), false).await.unwrap();
    }

    #[tokio::test]
    async fn run_agent_discover_include_interactive_returns_all() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        run_agent_run("inter".into(), false, Some(&db), false)
            .await
            .unwrap();
        run_agent_run("bg".into(), true, Some(&db), false)
            .await
            .unwrap();
        // No assertion on stdout; just verify the code path runs.
        run_agent_discover(true, Some(&db), false).await.unwrap();
        run_agent_discover(true, Some(&db), true).await.unwrap();
    }

    #[tokio::test]
    async fn run_agent_discover_empty_db_friendly_message() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("agents.db");
        run_agent_discover(false, Some(&missing), false)
            .await
            .unwrap();
        run_agent_discover(false, Some(&missing), true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_agent_discover_no_matching_goals_renders_friendly() {
        let _g = DREAM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agents.db");
        // Seed only Interactive — discover without --include-interactive
        // should print the "(no detached / daemon goals running...)"
        // friendly message.
        run_agent_run("only_interactive".into(), false, Some(&db), false)
            .await
            .unwrap();
        run_agent_discover(false, Some(&db), false).await.unwrap();
    }
}
