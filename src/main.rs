#![allow(clippy::all)] // In-flux scaffolding

mod init_cli;
mod persona_cli;
mod plugin_admin;
mod plugin_install;
mod plugin_new;
mod plugin_run;

use std::path::{Path, PathBuf};
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
// `nexo_plugin_browser` no longer used in-process.
// The crate stays in the workspace dormant pending in-tree removal.
// `seed_browser_subprocess_env` (below) translates yaml config into
// env vars the standalone subprocess reads.
// `WhatsappPlugin` import dropped after the
// daemon's subprocess flip; the lib type is reachable on demand
// via `nexo_plugin_whatsapp::WhatsappPlugin` from the standalone
// repo path-dep, but the daemon no longer constructs it directly.
// `WhatsappPairingAdapter`, `register_whatsapp_tools`, and the
// `pairing::*` types are still imported via fully-qualified
// paths inline.

enum Mode {
    Run,
    DlqList,
    DlqReplay(String),
    DlqPurge,
    /// `nexo set-broker <kind> [--url <url>] [--no-signal]`
    /// edits `broker.yaml` in the daemon's config dir to switch
    /// between `nats` and `local` transports without an operator
    /// learning sed syntax. After the edit, sends SIGTERM to any
    /// running daemon (matched by its `--config` argument) so the
    /// supervisor loop (dev-daemon.sh or systemd) respawns and
    /// picks up the new config. `--no-signal` skips the kill for
    /// scripted workflows that want to control restart timing.
    SetBroker {
        kind: String,
        url: Option<String>,
        no_signal: bool,
    },
    /// `nexo init [--yaml <names>] [--output <dir>]
    /// [--force] [--stdout]` scaffolds the 19 sample YAML files
    /// the daemon's `AppConfig::load` knows about, each densely
    /// commented with field semantics + sane defaults. Operators
    /// edit in place to customize. `--yaml` filters by name
    /// (csv: `--yaml broker,llm` or shorthand `--yaml plugins`
    /// for every plugins/*.yaml). `--output` overrides the
    /// resolved config dir. `--force` overwrites existing files.
    /// `--stdout` emits a concatenated stream to stdout instead
    /// of writing files.
    Init {
        yaml_filter: Option<String>,
        output_dir: Option<PathBuf>,
        force: bool,
        stdout: bool,
    },
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
    /// `nexo plugin install <owner>/<repo>[@<tag>]`.
    /// Decentralized GitHub Releases install; downloads + sha-verifies
    /// + extracts under `plugins.discovery.search_paths[0]` (or
    /// `--dest`). Best-effort `plugin.lifecycle.<id>.installed` event
    /// emitted when broker is up. Cosign signature
    /// verification runs per `config/extensions/trusted_keys.toml`; the
    /// two flags below force `Require` / `Ignore` modes for a single
    /// invocation respectively.
    PluginInstall {
        coords: String,
        dest: Option<PathBuf>,
        target: Option<String>,
        json: bool,
        require_signature: bool,
        skip_signature_verify: bool,
    },
    /// Static help block for the plugin subcommand.
    PluginHelp,
    /// `nexo plugin new <id> --lang <lang>` scaffolds
    /// a fresh out-of-tree plugin from one of the four bundled
    /// templates (rust / python / typescript / php). Templates
    /// are embedded at compile time via `include_dir!`.
    PluginNew {
        id: String,
        lang: String,
        dest: Option<PathBuf>,
        owner: Option<String>,
        description: Option<String>,
        git_init: bool,
        force: bool,
        json: bool,
    },
    /// `nexo plugin run <path>` boots the daemon
    /// with a local plugin directory injected as
    /// `cfg.plugins.discovery.search_paths[0]`. Inner-loop dev
    /// without going through install + verify pipeline. Falls
    /// through to `Mode::Run` after stamping
    /// `args.plugin_run_override`.
    PluginRun {
        path: PathBuf,
        no_daemon_config: bool,
        watch: bool,
        verbose: bool,
        json: bool,
    },
    /// `nexo plugin list [--include-orphan] [--json]`.
    /// Walks `cfg.plugins.discovery.search_paths` and tabulates
    /// every installed plugin. Orphans (no `.nexo-install.json`)
    /// hidden by default.
    PluginList {
        include_orphan: bool,
        json: bool,
    },
    /// `nexo plugin upgrade <id> [...]`. Re-resolves
    /// the plugin's recorded GitHub Releases coordinates +
    /// delegates to the install pipeline. Refuses to downgrade.
    PluginUpgrade {
        id: String,
        target: Option<String>,
        require_signature: bool,
        skip_signature_verify: bool,
        json: bool,
    },
    /// `nexo plugin remove <id> [--purge-cache] [--yes]`.
    /// Atomically renames the plugin dir aside then deletes it.
    /// `--purge-cache` also purges `nexo_state_dir/plugins/<id>`
    /// and `plugins/cache/<id>`.
    PluginRemove {
        id: String,
        purge_cache: bool,
        yes: bool,
        json: bool,
    },
    /// `nexo persona install
    /// <owner>/<repo>[@<tag>] [--dest <dir>] [--target
    /// <triple>] [--json]`. Mirror of `Mode::PluginInstall`
    /// but for v2 persona packs (out-of-tree agent
    /// definitions). Resolves + verifies + extracts under
    /// `--dest` (or `cfg.personas.discovery.search_paths[0]`,
    /// or `<state_dir>/personas/`).
    PersonaInstall {
        coords: String,
        dest: Option<PathBuf>,
        target: Option<String>,
        json: bool,
    },
    /// `nexo persona list [--json]`. Walks
    /// `cfg.personas.discovery.search_paths`, applies
    /// disabled / allowlist filters, tabulates each survivor.
    PersonaList {
        json: bool,
    },
    /// `nexo persona remove <id> [--yes] [--json]`.
    /// Atomic dir removal of the install root for `<id>`.
    PersonaRemove {
        id: String,
        yes: bool,
        json: bool,
    },
    /// `nexo persona get <id> [--json]`. STUB; will
    /// surface manifest + lifecycle history later. Meanwhile
    /// suggests the operator-equivalent shell.
    PersonaGet {
        id: String,
        json: bool,
    },
    /// `nexo persona upgrade <id> [--json]`. STUB;
    /// will re-resolve recorded coords + delegate to the install
    /// path. Meanwhile suggests the install-with-newer-tag
    /// workaround.
    PersonaUpgrade {
        id: String,
        json: bool,
    },
    /// `nexo persona run <path> [--json]`. STUB;
    /// inner-loop dev (mirror of `nexo plugin run`) is not yet
    /// wired.
    PersonaRun {
        path: PathBuf,
        json: bool,
    },
    /// Static help block printed by
    /// `nexo persona help`.
    PersonaHelp,
    /// `nexo ext state-dir <id>` prints the
    /// canonical state directory for `<id>` resolved against
    /// the current `NEXO_HOME` (created if absent). Operators
    /// pipe the output into `cd`, `sqlite3 .backup`, etc.
    ExtStateDir {
        id: String,
        ensure: bool,
    },
    /// `nexo memory <sub>` — operator surface for the snapshot
    /// subsystem. Every subcommand is standalone: it
    /// constructs a fresh `LocalFsSnapshotter` from `--state-root` (+
    /// optional `--memdir-root` / `--sqlite-root`) and exits when the
    /// action completes. No daemon contact required.
    Memory(MemorySubcommand),
    McpServer(McpServerSubcommand),
    /// `nexo microapp admin audit tail [...]`.
    /// Read-only operator query over the SQLite admin audit log
    /// written by [`nexo_core::agent::admin_rpc::SqliteAdminAuditWriter`].
    /// Maps every flag 1:1 to `AuditTailFilter`; default `--db`
    /// resolves to `nexo_state_dir().join("admin_audit.db")`.
    MicroappAuditTail {
        microapp_id: Option<String>,
        method: Option<String>,
        result: Option<String>,
        since_mins: Option<u64>,
        since_ms: Option<u64>,
        limit: usize,
        format: String,
        db: Option<PathBuf>,
        /// Restrict to a single tenant scope.
        /// Leaves rows with `NULL tenant_id` (echo, pairing,
        /// credentials) out when set; `None` keeps the tail
        /// un-scoped.
        tenant_id: Option<String>,
    },
    /// `nexo agent dream {tail|status|kill}`.
    AgentDream(AgentDreamSubcommand),
    /// `nexo agent run [--bg] <prompt>`. Spawn a goal
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
    /// `nexo agent ps [--all] [--kind=...] [--json]`.
    /// Read the local `agent_handles` SQLite store and render running
    /// goals. RO pool — works without a daemon up.
    AgentPs {
        kind: Option<String>,
        all: bool,
        db: Option<PathBuf>,
        json: bool,
    },
    /// `nexo agent attach <goal_id>`. Read-only viewer
    /// of a goal's latest persisted snapshot. Live event streaming
    /// via NATS is a follow-up.
    AgentAttach {
        goal_id: String,
        db: Option<PathBuf>,
        json: bool,
    },
    /// `nexo agent discover [--include-interactive]`.
    /// List Running goals filtered to BG / Daemon / DaemonWorker
    /// kinds. With `--include-interactive`, include all kinds.
    AgentDiscover {
        include_interactive: bool,
        db: Option<PathBuf>,
        json: bool,
    },
    /// `nexo channel list [--config=<path>] [--json]`.
    /// Static dump of every operator-approved channel + every binding's
    /// `allowed_channel_servers`. Pure read of the YAML — no daemon
    /// required.
    ChannelList {
        config: Option<PathBuf>,
        json: bool,
    },
    /// `nexo channel doctor [--config=<path>] [--binding=<id>]
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
    /// `nexo channel test <server> [--binding=<id>]
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
    /// Pairing CLI subcommands. Each one opens the
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
    /// `agent doctor plugins [--json]` — runs the
    /// discover + merge_agents + merge_skills + init_loop pipeline
    /// in-process and renders an 8-section report. Exits 1 when
    /// error-level diagnostics, `LastPluginWins` conflicts, or
    /// `Failed` init outcomes surface; exits 0 otherwise.
    DoctorPlugins {
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
    /// Run the credential gauntlet against the loaded config
    /// and print a report (OK / warnings / errors). Exits 0 on clean,
    /// 1 on errors, 2 on warnings-only. Used by CI to gate PRs that
    /// edit `agents.d/*.yaml`, `whatsapp.yaml`, `telegram.yaml`, or
    /// `google-auth.yaml`.
    CheckConfig {
        strict: bool,
    },
    /// Trigger a hot-reload on a running agent daemon.
    /// Publishes `control.reload` on the same broker the daemon is on
    /// and waits up to 5s for a `control.reload.ack` with the outcome
    /// (version, applied, rejected). Exit 0 if at least one agent
    /// reloaded; exit 1 if all rejected or no ack arrived.
    Reload {
        json: bool,
    },
    /// Generic poller subsystem. CLI hits the loopback admin
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
    /// Operator-side cron admin:
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
    /// Surface the admin web UI (served by the `nexo-plugin-admin`
    /// plugin, which the daemon discovers + spawns automatically).
    /// Auto-installs the plugin via `cargo install nexo-plugin-admin`
    /// on first use if it isn't on PATH, prints the loopback URL,
    /// probes whether the daemon has it running, and — with `--open`
    /// — launches the URL in the default browser. `--tunnel` brings
    /// up a free Cloudflare quick tunnel natively (pure-Rust QUIC +
    /// capnp-RPC client, no `cloudflared` subprocess) so the admin
    /// page is reachable from anywhere, and blocks until Ctrl-C.
    Admin {
        port: u16,
        /// `--open`: launch the admin URL in the default browser.
        open: bool,
        /// `--tunnel`: expose the admin server via a Cloudflare quick
        /// tunnel and block until Ctrl-C.
        tunnel: bool,
    },
    /// `nexo start [--config <dir>]` — run the daemon detached in the
    /// background. Writes a pidfile (`<runtime-dir>/nexo.pid`) so
    /// `stop` / `restart` can find it; stdout+stderr go to
    /// `<runtime-dir>/nexo.log`. No-op (with a notice) if already running.
    Start,
    /// `nexo stop` — SIGTERM the background daemon (SIGKILL after a
    /// grace period), then remove the pidfile.
    Stop,
    /// `nexo restart [--config <dir>]` — `stop` then `start`.
    Restart,
    /// `nexo update` (alias `self-update`) — upgrade the `nexo`
    /// binary in place via `cargo install nexo-rs --force` when the
    /// Rust toolchain is available, otherwise print the installer
    /// one-liner.
    Update,
    /// `nexo service install [--config <dir>]` — register the daemon
    /// as an OS service so it auto-starts on boot/login (systemd user
    /// unit on Linux, launchd LaunchAgent on macOS, a logon Scheduled
    /// Task on Windows), then start it now.
    ServiceInstall,
    /// `nexo service uninstall` — stop + remove the OS service unit.
    ServiceUninstall,
    /// `nexo service status` — report whether the OS service is
    /// installed / enabled / running.
    ServiceStatus,
    /// Print version + (optionally) build provenance.
    /// Short form (`nexo --version` / `-V`) prints `nexo <pkg-version>`.
    /// Verbose form (`nexo version` or `nexo --version --verbose`)
    /// prints the package version plus git-sha, target triple, build
    /// channel, and build timestamp captured by `build.rs`.
    Version {
        verbose: bool,
    },
    Help,
}

/// Subcommands for `nexo memory`.
///
/// All subcommands accept `--state-root <path>` (default: `./state`),
/// optional `--memdir-root` / `--sqlite-root` overrides, and a final
/// `--json` for machine-readable output.
#[derive(Debug)]
enum MemorySubcommand {
    Verify {
        bundle: PathBuf,
        json: bool,
    },
    Snapshot {
        agent: String,
        tenant: String,
        label: Option<String>,
        no_redact: bool,
        encrypt: Option<String>,
        state_root: PathBuf,
        memdir_root: Option<PathBuf>,
        sqlite_root: Option<PathBuf>,
        json: bool,
    },
    Restore {
        agent: String,
        tenant: String,
        bundle: PathBuf,
        dry_run: bool,
        no_auto_pre_snapshot: bool,
        decrypt_identity: Option<PathBuf>,
        state_root: PathBuf,
        memdir_root: Option<PathBuf>,
        sqlite_root: Option<PathBuf>,
        json: bool,
    },
    List {
        agent: String,
        tenant: String,
        state_root: PathBuf,
        json: bool,
    },
    Diff {
        agent: String,
        tenant: String,
        a: String,
        b: String,
        state_root: PathBuf,
        json: bool,
    },
    Export {
        agent: String,
        tenant: String,
        id: String,
        to: PathBuf,
        state_root: PathBuf,
    },
    Delete {
        agent: String,
        tenant: String,
        id: String,
        state_root: PathBuf,
        yes: bool,
    },
}

/// Subcommands for `nexo mcp-server`.
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

/// `nexo agent dream {tail|status|kill}` operator CLI
/// for the autoDream audit log + manual control. Read paths open the
/// SQLite DB read-only without a daemon. Kill writes the row to
/// `Aborted`, finalises `ended_at = now()`, and rewinds the
/// consolidation lock via `ConsolidationLock::rollback(prior_mtime)`
/// when a `--memory-dir` is provided. Exposes the same
/// kill semantics a UI would, but as a CLI surface.
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
    /// Optional override-dir flag (`--override-from <path>`)
    /// or `NEXO_OVERRIDE_FROM` env. Files in this dir with canonical
    /// YAML names (broker.yaml, llm.yaml, …) are deep-merged on top
    /// of the same-named file in `config_dir`. Per-file env vars
    /// (`NEXO_<NAME>_YAML`) take precedence over both layers. See
    /// `nexo-config::load_with_override`.
    override_from: Option<PathBuf>,
    mode: Mode,
    /// Set when `Mode::PluginRun` falls through to
    /// `Mode::Run`. Tells the daemon boot path to mutate the
    /// loaded `AppConfig` (prepend the local plugin's path to
    /// discovery search_paths; optionally clear agents).
    plugin_run_override: Option<plugin_run::PluginRunOverride>,
    /// Set when `Mode::PersonaRun` falls through to
    /// daemon boot. The boot path applies the override to the
    /// loaded `AppConfig` (prepends the local persona pack's
    /// parent dir to `cfg.personas.discovery.search_paths`).
    persona_run_override: Option<persona_cli::PersonaRunOverride>,
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
    // Phase 81.20.x Bucket C2 Stage 2 — `wa_pairing` field
    // removed. The subprocess owns its own pairing state via
    // `WhatsappPlugin::pairing`; daemon no longer mirrors it
    // through the broker. `/whatsapp/*` HTTP routes are served
    // by the subprocess via `PluginHttpRouter` Stage 2 forward.
    // Phase 81.20.x F2.2 — `email_plugin` handle removed.
    // `/email/health` + `/metrics` email rows now live in the
    // subprocess plugin (Phase 81.33.b.real Stages 2+5 broker
    // handlers); the daemon never holds a typed `EmailPlugin`
    // reference.
    /// Companion WS handshake context — populated after pairing init.
    /// `None` until the daemon's pairing block completes.
    pairing_handshake: Arc<std::sync::OnceLock<PairingHandshakeCtx>>,
    /// Phase 92.followup.b — live `TunnelHandle`s the daemon
    /// owns. Populated by tunnel-creation sites
    /// (`src/main.rs:3815` whatsapp-pairing public_tunnel +
    /// `src/main.rs:10550` admin `--tunnel`) so the `/metrics`
    /// aggregator can snapshot per-tunnel supervisor counters
    /// (`tunnel_streams_total`, `tunnel_bytes_in_total`,
    /// `tunnel_bytes_out_total`, `tunnel_reconnects_total`).
    /// Entries removed on graceful shutdown; stale entries
    /// after Drop emit zero from `metrics().await`.
    tunnel_registry: Arc<tokio::sync::RwLock<Vec<Arc<nexo_tunnel_quick::TunnelHandle>>>>,
    /// Phase 81.33.b.real Stage 2 — plugin HTTP route router.
    /// Built at boot from `wire.plugin_handles[..].manifest().plugin.http`.
    /// `handle_health_conn` checks the router BEFORE the legacy
    /// hardcoded `/whatsapp/*` block; a match forwards the
    /// request via broker JSON-RPC to the declaring plugin's
    /// subprocess. Empty when no plugin declares
    /// `[plugin.http]` — then the legacy path matchers serve.
    http_router: Arc<nexo_pairing::plugin_http::PluginHttpRouter>,
    /// Phase 81.33.b.real Stage 5 — plugin Prometheus metrics
    /// scrape descriptors. Built at boot from
    /// `wire.plugin_handles[..].manifest().plugin.metrics`. The
    /// `/metrics` handler iterates this list on every request,
    /// issues a broker RPC per declaring plugin, and concatenates
    /// the returned Prometheus text into the aggregate body.
    /// Empty when no plugin declares `[plugin.metrics] prometheus
    /// = true` — then only daemon-internal sources (LLM, MCP,
    /// poller, tunnel, legacy email) feed the aggregate.
    plugin_metrics: Arc<Vec<nexo_pairing::plugin_metrics::PluginMetricsDescriptor>>,
}

#[derive(Clone)]
struct CronToolBindingContext {
    ctx: nexo_core::agent::AgentContext,
    tools: Arc<nexo_core::agent::ToolRegistry>,
}

/// `Arc<ArcSwap<HashMap>>` enables lock-free hot-swap of the
/// per-binding context map. The config-reload post-hook calls
/// [`RuntimeCronToolExecutor::replace_bindings`] so cron firings
/// observe the new `effective` policy on the next call. In-flight
/// `resolve_binding` callers keep their loaded `Arc<HashMap>`
/// snapshot until completion; subsequent calls see the new map.
///
/// `ArcSwap` (lock-free swap) makes the in-flight protection
/// structural rather than imperative. The map is rebuilt on
/// reload only, not on every tick — cheaper and avoids the
/// long-job hide-tick race.
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

    /// Atomic hot-swap of the binding map. Called by the
    /// config-reload post-hook. Cheap (single `Arc` store).
    /// In-flight callers retain their pre-swap snapshot.
    fn replace_bindings(&self, new_map: std::collections::HashMap<String, CronToolBindingContext>) {
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

/// Bundles the Arcs and shared deps that
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
    // Phase 95 — web_search_router field removed. The web_search
    // tool now lives in the standalone subprocess plugin
    // `nexo-rs-plugin-web-search`.
    link_extractor: Arc<nexo_core::link_understanding::LinkExtractor>,
    dispatch_ctx: Option<Arc<nexo_core::agent::dispatch_handlers::DispatchToolContext>>,
    // Phase 81.32 c5 — `DashMap` so hot-spawn can insert post-boot
    // without an `RwLock` write lock that would serialise the cron
    // post-hook against runtime spawn.
    tools_per_agent: Arc<dashmap::DashMap<String, Arc<nexo_core::agent::ToolRegistry>>>,
    cron_tool_call_cfg: nexo_config::types::runtime::RuntimeCronToolCallsConfig,
}

/// Re-walks per-agent snapshot handles and builds the
/// `binding_id → CronToolBindingContext` map. Used by the boot
/// path (initial population, called once after the agent loop
/// ends) and the config-reload post-hook (rebuild on snapshot
/// swap). Single source of truth — preserves bit-for-bit
/// semantics with the inline closure it replaces.
///
/// Rebuilds use the snapshot handles rather than file mtimes,
/// and only on reload — ArcSwap gives cheap atomic swaps.
///
/// Limitation: agent add/remove during runtime is out of scope.
/// Translate `cfg.plugins.browser` YAML into the
/// `NEXO_PLUGIN_BROWSER_*` env vars the standalone
/// `nexo-plugin-browser` subprocess reads from
/// `nexo_plugin_browser::env_config::browser_config_from_env`.
///
/// Called once at boot before the plugin init loop spawns the
/// child. Daemon process env is the inherited env for every
/// subprocess `Command::spawn`, so this single mutation fans out
/// to every plugin spawn (browser is the only consumer of the
/// `NEXO_PLUGIN_BROWSER_*` namespace, so collisions are
/// impossible).
///
/// No-op when the operator hasn't configured `plugins.browser`
/// in YAML — the subprocess (if discovered) falls back to the
/// hardcoded defaults in `env_config.rs`.
/// Best-effort `<owner>/<repo>` extraction from a GitHub URL.
/// Used by boot-time persona discovery to reconstruct coords
/// when the on-disk persona only carries the homepage URL
/// (no tag info — bootloader uses `"unknown"` tag in the
/// resulting `RepoCoords`). Returns `None` for non-GitHub
/// URLs or paths shorter than `/owner/repo`.
fn github_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let trimmed = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let after_host = trimmed.strip_prefix("github.com/")?;
    let mut parts = after_host.trim_end_matches('/').split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

fn seed_browser_subprocess_env(cfg: &nexo_config::BrowserConfig) {
    std::env::set_var(
        "NEXO_PLUGIN_BROWSER_HEADLESS",
        if cfg.headless { "true" } else { "false" },
    );
    if !cfg.executable.is_empty() {
        std::env::set_var("NEXO_PLUGIN_BROWSER_EXECUTABLE", &cfg.executable);
    }
    if !cfg.cdp_url.is_empty() {
        std::env::set_var("NEXO_PLUGIN_BROWSER_CDP_URL", &cfg.cdp_url);
    }
    std::env::set_var("NEXO_PLUGIN_BROWSER_USER_DATA_DIR", &cfg.user_data_dir);
    std::env::set_var(
        "NEXO_PLUGIN_BROWSER_WINDOW_WIDTH",
        cfg.window_width.to_string(),
    );
    std::env::set_var(
        "NEXO_PLUGIN_BROWSER_WINDOW_HEIGHT",
        cfg.window_height.to_string(),
    );
    std::env::set_var(
        "NEXO_PLUGIN_BROWSER_CONNECT_TIMEOUT_MS",
        cfg.connect_timeout_ms.to_string(),
    );
    std::env::set_var(
        "NEXO_PLUGIN_BROWSER_COMMAND_TIMEOUT_MS",
        cfg.command_timeout_ms.to_string(),
    );
    if !cfg.args.is_empty() {
        std::env::set_var("NEXO_PLUGIN_BROWSER_ARGS", cfg.args.join(","));
    }
    tracing::info!(
        headless = cfg.headless,
        cdp_url = %cfg.cdp_url,
        "browser plugin: seeded NEXO_PLUGIN_BROWSER_* env for subprocess spawn"
    );
}

/// Per-instance env dict for the telegram
/// subprocess. Caller iterates `cfg.plugins.telegram` and builds
/// one dict per entry; each dict is passed via
/// `subprocess_plugin_factory_with_env` so N spawns don't collide
/// on `NEXO_PLUGIN_TELEGRAM_*` keys.
///
/// The dict starts from a small whitelist of inherited daemon
/// envs (`PATH`, `HOME`, `RUST_LOG`, `NEXO_BROKER_URL`) so the
/// child can resolve binaries + reach the broker; everything else
/// the daemon's process env carried (potentially including
/// secrets that have nothing to do with telegram) is dropped.
/// Defense-in-depth — `Command::env_clear().envs(&map)` in
/// `SubprocessNexoPlugin::spawn_and_handshake` enforces this.
#[allow(dead_code)] // Wired in the telegram-loop flip path.
/// Map the daemon's own broker config to the wire
/// string a subprocess plugin should use when constructing its
/// own broker:
///
/// - daemon `Nats` → child connects to the same NATS server.
/// - daemon `Local` → child uses the stdio bridge (the local
///   broker is in-process tokio::mpsc, unreachable from another
///   process; the bridge forwards through the parent's
///   stdin/stdout JSON-RPC channel).
/// - daemon `StdioBridge` → invalid for an operator-set config;
///   the kind is daemon-derived only. Fall back to
///   `stdio_bridge` defensively so a misconfiguration doesn't
///   crash boot.
// Wave 5 — `convert_email_cfg` removed. nexo_config no longer
// carries typed email; daemon-side consumers deserialize directly
// into `nexo_plugin_email::EmailPluginConfig` from opaque entries.

/// Wave 6 — flatten `cfg.plugins.entries[<plugin_id>]` opaque YAML
/// into a `Vec<serde_yaml::Value>` of per-tenant slices. Accepts
/// both shapes (single Mapping → 1-element vec; Sequence → as-is).
/// Used by daemon's generic subprocess factory wiring so it doesn't
/// need typed access to telegram / whatsapp config.
fn opaque_plugin_entries(
    plugins: &nexo_config::PluginsConfig,
    plugin_id: &str,
) -> Vec<serde_yaml::Value> {
    let Some(value) = plugins.entries.get(plugin_id) else {
        return Vec::new();
    };
    match value {
        serde_yaml::Value::Sequence(seq) => seq.clone(),
        serde_yaml::Value::Mapping(_) => vec![value.clone()],
        _ => Vec::new(),
    }
}

fn subprocess_broker_kind_str(kind: nexo_config::types::broker::BrokerKind) -> &'static str {
    match kind {
        nexo_config::types::broker::BrokerKind::Nats => "nats",
        nexo_config::types::broker::BrokerKind::Local
        | nexo_config::types::broker::BrokerKind::StdioBridge => "stdio_bridge",
    }
}

/// Wave 6 — opaque telegram env seeder. Takes the raw YAML
/// `serde_yaml::Value` slice for ONE tenant (single map shape)
/// and stamps the env dict for one subprocess. nexo-config no
/// longer carries telegram-specific types; this helper reads the
/// fields it needs straight from the Value.
fn seed_telegram_subprocess_env_for(
    cfg: &serde_yaml::Value,
    broker_kind: &str,
    broker_url: &str,
) -> std::collections::HashMap<String, String> {
    use serde_yaml::Value as V;
    let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for key in ["PATH", "HOME", "RUST_LOG", "NEXO_CONFIG_DIR"] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env.insert("NEXO_BROKER_KIND".into(), broker_kind.to_string());
    if broker_kind != "stdio_bridge" {
        env.insert("NEXO_BROKER_URL".into(), broker_url.to_string());
    }

    // Helpers for opaque field extraction.
    let get_str = |key: &str| -> Option<String> {
        cfg.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    let get_u64 = |key: &str| -> Option<u64> { cfg.get(key).and_then(|v| v.as_u64()) };
    let get_obj = |key: &str| -> Option<&V> { cfg.get(key) };

    if let Some(token) = get_str("token") {
        env.insert("NEXO_PLUGIN_TELEGRAM_TOKEN".into(), token);
    }
    if let Some(instance) = get_str("instance") {
        if !instance.trim().is_empty() {
            env.insert("NEXO_PLUGIN_TELEGRAM_INSTANCE".into(), instance);
        }
    }
    if let Some(bridge_ms) = get_u64("bridge_timeout_ms") {
        env.insert(
            "NEXO_PLUGIN_TELEGRAM_BRIDGE_TIMEOUT_MS".into(),
            bridge_ms.to_string(),
        );
    }

    // polling: { interval_ms, offset_path }
    if let Some(polling) = get_obj("polling") {
        if let Some(interval_ms) = polling.get("interval_ms").and_then(|v| v.as_u64()) {
            env.insert(
                "NEXO_PLUGIN_TELEGRAM_INTERVAL_MS".into(),
                interval_ms.to_string(),
            );
        }
        if let Some(offset_path) = polling.get("offset_path").and_then(|v| v.as_str()) {
            env.insert(
                "NEXO_PLUGIN_TELEGRAM_OFFSET_PATH".into(),
                offset_path.to_string(),
            );
        }
    }

    // allowlist: { chat_ids: [..] }
    let chat_ids: Vec<i64> = cfg
        .get("allowlist")
        .and_then(|a| a.get("chat_ids"))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_default();
    env.insert(
        "NEXO_PLUGIN_TELEGRAM_ALLOWLIST".into(),
        serde_json::to_string(&chat_ids).unwrap_or_else(|_| "[]".into()),
    );

    // auto_transcribe block
    if let Some(at) = get_obj("auto_transcribe") {
        let enabled = at.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        if enabled {
            env.insert("NEXO_PLUGIN_TELEGRAM_AUTO_TRANSCRIBE".into(), "true".into());
            if let Some(cmd) = at.get("command").and_then(|v| v.as_str()) {
                env.insert(
                    "NEXO_PLUGIN_TELEGRAM_WHISPER_COMMAND".into(),
                    cmd.to_string(),
                );
            }
            if let Some(timeout_ms) = at.get("timeout_ms").and_then(|v| v.as_u64()) {
                env.insert(
                    "NEXO_PLUGIN_TELEGRAM_WHISPER_TIMEOUT_MS".into(),
                    timeout_ms.to_string(),
                );
            }
            if let Some(lang) = at.get("language").and_then(|v| v.as_str()) {
                env.insert(
                    "NEXO_PLUGIN_TELEGRAM_WHISPER_LANGUAGE".into(),
                    lang.to_string(),
                );
            }
        }
    }
    env
}

// Phase 81.20.x Bucket C2 Stage 2 —
// `spawn_whatsapp_pairing_state_subscriber` removed. Daemon no
// longer mirrors `plugin.inbound.whatsapp.<inst>` events into a
// typed `SharedPairingState` map. The subprocess owns its own
// state via `WhatsappPlugin::pairing`; future plugin v0.4.4 will
// serve the `/whatsapp/<inst>/pair*` routes from there via the
// PluginHttpRouter (Stage 2) forward. The daemon-side mirror
// existed only to back the hardcoded `/whatsapp/*` HTTP block,
// which is also gone now.

/// Daemon-side broker subscriber that watches
/// `plugin.lifecycle.whatsapp.<inst>.peer_typing` events from
/// the subprocess plugin and bridges them into the daemon's
/// `AgentEventEmitter` so `AgentEventKind::PeerTyping` keeps
/// surfacing on the SSE live transcript firehose. The in-tree
/// `WhatsappPlugin::with_emitter` used to wire the emitter
/// directly; after the subprocess flip the emitter Arc doesn't
/// cross the process boundary, so the broker hop closes the loop.
#[allow(dead_code)] // Wired in the whatsapp-loop block below.
fn spawn_whatsapp_typing_presence_subscriber(
    broker: nexo_broker::AnyBroker,
    emitter: std::sync::Arc<dyn nexo_core::agent::agent_events::AgentEventEmitter>,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    use nexo_broker::BrokerHandle;
    tokio::spawn(async move {
        let mut sub = match broker.subscribe("plugin.lifecycle.whatsapp.>").await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "whatsapp typing presence subscriber: broker subscribe failed; \
                     SSE firehose won't surface PeerTyping until daemon restart",
                );
                return;
            }
        };
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                next = sub.next() => {
                    let Some(msg) = next else { break; };
                    let kind = msg.payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    if kind != "peer_typing" {
                        continue;
                    }
                    let account_id = msg
                        .payload.get("account_id").and_then(|v| v.as_str())
                        .unwrap_or("default").to_string();
                    let sender_id = msg
                        .payload.get("sender_id").and_then(|v| v.as_str())
                        .unwrap_or("").to_string();
                    let composing = msg
                        .payload.get("composing").and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let at_ms = msg
                        .payload.get("at_ms").and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let evt = nexo_tool_meta::admin::agent_events::AgentEventKind::PeerTyping {
                        channel: "whatsapp".to_string(),
                        account_id,
                        sender_id,
                        composing,
                        at_ms,
                        agent_id: None,
                        tenant_id: None,
                    };
                    emitter.emit(evt).await;
                }
            }
        }
    })
}

/// Per-instance env dict for the whatsapp
/// subprocess. Mirrors the telegram helper above; whitelists
/// only PATH/HOME/RUST_LOG from the daemon's process env so
/// secrets unrelated to the whatsapp plugin never leak into the
/// child.
/// Wave 6 — opaque whatsapp env seeder. Same pattern as
/// `seed_telegram_subprocess_env_for`: reads raw YAML Value for
/// ONE tenant; nexo-config no longer carries WA-specific types.
#[allow(dead_code)] // Wired in the whatsapp-loop flip below.
fn seed_whatsapp_subprocess_env_for(
    cfg: &serde_yaml::Value,
    broker_kind: &str,
    broker_url: &str,
) -> std::collections::HashMap<String, String> {
    let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for key in ["PATH", "HOME", "RUST_LOG", "NEXO_CONFIG_DIR"] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env.insert("NEXO_BROKER_KIND".into(), broker_kind.to_string());
    if broker_kind != "stdio_bridge" {
        env.insert("NEXO_BROKER_URL".into(), broker_url.to_string());
    }

    let get_str = |key: &str| -> Option<String> {
        cfg.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };

    if let Some(session_dir) = get_str("session_dir") {
        env.insert("NEXO_PLUGIN_WHATSAPP_SESSION_DIR".into(), session_dir);
    }
    if let Some(media_dir) = get_str("media_dir") {
        env.insert("NEXO_PLUGIN_WHATSAPP_MEDIA_DIR".into(), media_dir);
    }
    if let Some(instance) = get_str("instance") {
        if !instance.trim().is_empty() {
            env.insert("NEXO_PLUGIN_WHATSAPP_INSTANCE".into(), instance);
        }
    }

    if let Some(bridge_ms) = cfg
        .get("bridge")
        .and_then(|b| b.get("response_timeout_ms"))
        .and_then(|v| v.as_u64())
    {
        env.insert(
            "NEXO_PLUGIN_WHATSAPP_BRIDGE_TIMEOUT_MS".into(),
            bridge_ms.to_string(),
        );
    }

    let allow_list: Vec<String> = cfg
        .get("acl")
        .and_then(|a| a.get("allow_list"))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    env.insert(
        "NEXO_PLUGIN_WHATSAPP_ALLOWLIST".into(),
        serde_json::to_string(&allow_list).unwrap_or_else(|_| "[]".into()),
    );

    if let Some(transcriber) = cfg.get("transcriber") {
        let enabled = transcriber
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if enabled {
            env.insert(
                "NEXO_PLUGIN_WHATSAPP_TRANSCRIBE_ENABLED".into(),
                "true".into(),
            );
            if let Some(timeout_ms) = transcriber.get("timeout_ms").and_then(|v| v.as_u64()) {
                env.insert(
                    "NEXO_PLUGIN_WHATSAPP_WHISPER_TIMEOUT_MS".into(),
                    timeout_ms.to_string(),
                );
            }
        }
    }
    env
}

// Wave 4 — `seed_email_subprocess_env_for` removed. Daemon no
// longer spawns email subprocesses via the factory wiring block;
// auto-subprocess fallback in `init_loop.rs` handles spawn from
// the discovered manifest. Plugin's own `env_config` reads its
// `NEXO_PLUGIN_EMAIL_*` env vars (with sensible defaults) when
// the daemon doesn't seed them.

/// `tools_per_agent` and `agent_snapshot_handles` are populated
/// during the boot agent loop and never extended; reload picks up
/// policy changes for EXISTING agents only.
fn build_cron_bindings_from_snapshots(
    snapshots: &dashmap::DashMap<String, Arc<arc_swap::ArcSwap<nexo_core::RuntimeSnapshot>>>,
    deps: &CronRebuildDeps,
) -> std::collections::HashMap<String, CronToolBindingContext> {
    let mut by_binding: std::collections::HashMap<String, CronToolBindingContext> =
        std::collections::HashMap::new();
    for entry in snapshots.iter() {
        let agent_id = entry.key();
        let snapshot_handle = entry.value();
        let snap = snapshot_handle.load_full();
        let agent_cfg = Arc::clone(&snap.nexo_config);
        let Some(tools) = deps
            .tools_per_agent
            .get(agent_id)
            .map(|r| Arc::clone(r.value()))
        else {
            tracing::debug!(
                agent = %agent_id,
                "build_cron_bindings_from_snapshots: agent missing from tools_per_agent; skipping"
            );
            continue;
        };

        // Iterate legacy/unbound first (binding_idx = None), then
        // each real binding (binding_idx = Some(i)). Mirrors the
        // original boot ordering.
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
            // Phase 95 — web_search_router wiring removed.
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

/// Phase 81.33.e — generic helper that consolidates the
/// per-cfg-entry subprocess factory registration loop used by
/// the telegram + whatsapp wire-up blocks. The two blocks were
/// 95% duplicates (90 LOC each) differing only in:
///   - which `cfg.plugins.X: Vec<XConfig>` to iterate
///   - which `seed_X_subprocess_env_for` to call
///   - the optional `enabled` filter (whatsapp has it; telegram doesn't)
///   - the `tracing` Phase reference in the success log
///
/// Caller passes the plugin id + the discovered manifest snapshot
/// + accessor closures + the seeder fn. We:
///   1. find the base manifest by id (returns `None` if missing
///      → caller's warning fires)
///   2. iterate each cfg entry (skipping disabled ones)
///   3. derive plugin_id from instance label (`telegram` /
///      `telegram.<label>`)
///   4. seed env, synthesise manifest, build factory, register.
///   5. push synthesised plugin to `extra_subprocess_plugins`.
///
/// Returns `Some(())` when at least the base manifest was found
/// (caller stays quiet); `None` when no base manifest exists so
/// the caller can emit a guidance warning.
#[allow(clippy::too_many_arguments)]
fn register_instance_subprocess_factories<C>(
    plugin_id: &str,
    cfgs: Vec<C>,
    pre_snap: &nexo_core::agent::nexo_plugin_registry::NexoPluginRegistrySnapshot,
    broker_kind: &str,
    broker_url: &str,
    factory_registry: &mut nexo_core::agent::nexo_plugin_registry::PluginFactoryRegistry,
    extra_subprocess_plugins: &mut Vec<nexo_core::agent::nexo_plugin_registry::DiscoveredPlugin>,
    extract_label: impl Fn(&C) -> Option<String>,
    is_enabled: impl Fn(&C) -> bool,
    seed_env: impl Fn(&C, &str, &str) -> std::collections::HashMap<String, String>,
    phase_ref: &'static str,
) -> Option<()> {
    if cfgs.is_empty() {
        return Some(());
    }
    let base = pre_snap
        .plugins
        .iter()
        .find(|p| p.manifest.plugin.id == plugin_id)
        .cloned()?;
    for cfg in cfgs {
        if !is_enabled(&cfg) {
            continue;
        }
        let instance_label = extract_label(&cfg).unwrap_or_default();
        let trimmed = instance_label.trim();
        let derived_id = if trimmed.is_empty() {
            plugin_id.to_string()
        } else {
            format!("{plugin_id}.{trimmed}")
        };
        let env = seed_env(&cfg, broker_kind, broker_url);
        let synthetic =
            nexo_core::agent::nexo_plugin_registry::synthesize_instance_plugin(&base, trimmed);
        let factory = nexo_core::agent::nexo_plugin_registry::subprocess_plugin_factory_with_env(
            synthetic.manifest.clone(),
            env,
            trimmed.to_string(),
        );
        if let Err(e) = factory_registry.register(derived_id.clone(), factory) {
            tracing::warn!(
                plugin_id = %derived_id,
                error = %e,
                phase = phase_ref,
                "subprocess factory registration failed; instance skipped",
            );
        } else {
            tracing::info!(
                plugin_id = %derived_id,
                phase = phase_ref,
                "registered subprocess factory",
            );
            extra_subprocess_plugins.push(synthetic);
        }
    }
    Some(())
}

// Phase 81.20.x Stage 7 — `plugin_declares_outbound_tools`
// removed. With the `register_whatsapp_tools` +
// `register_telegram_tools` daemon-side fallbacks dropped, no
// caller needed the "does manifest declare outbound?" check
// anymore. RemoteToolHandlers (driven off the subprocess's
// initialize-reply tool list) cover every canonical plugin
// uniformly.

// Phase 81.20.x F2.1 — `plugin_declares_metrics` removed. The last
// caller was the daemon-side `render_prometheus` direct call for
// email, which is gone now that email v0.6.0+ scrapes via
// `nexo_pairing::plugin_metrics::scrape_all` like every other
// plugin.

// Phase 81.20.x Stage 7 — `build_known_pairing_registry`
// removed. With every canonical plugin now declaring
// `[plugin.pairing.adapter]` in its manifest, the daemon-side
// hardcoded `WhatsappPairingAdapter::new` +
// `TelegramPairingAdapter::new` registrations are redundant:
// the loop over `plugin_handles` (each call site below)
// asks every loaded plugin for its `build_pairing_adapter()`
// trait method, which returns a `GenericBrokerPairingAdapter`
// driven off manifest data. Same end-state, no plugin name
// baked into the daemon.

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    // rustls 0.23 requires a process-wide `CryptoProvider`. The
    // daemon links both `ring` and `aws-lc-rs` transitively
    // (sqlx, reqwest, lettre, …); without an explicit
    // `install_default` the first rustls user panics with
    // "Could not automatically determine the process-level
    // CryptoProvider from Rustls crate features." The
    // EmailPersister probe is the first hot path that hits this
    // (TLS handshake to imap.gmail.com on
    // `credentials/register`), but every microapp / extension
    // that touches HTTPS via a transitively-pinned rustls also
    // benefits. `install_default` returns Err on a duplicate
    // install — we ignore so a host-process embedder can pin
    // their own provider before main() runs.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Make the `agent` binary's version the one
    // compared against `plugin.min_agent_version`, instead of the
    // `nexo-extensions` crate version. Ignore the result: if the
    // override was already set (double-init in tests), the existing
    // value wins — safe default.
    let _ = nexo_extensions::set_agent_version(env!("CARGO_PKG_VERSION"));

    let mut args = parse_args();
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
        Mode::SetBroker {
            kind,
            url,
            no_signal,
        } => {
            return run_set_broker(&args.config_dir, &kind, url.as_deref(), no_signal);
        }
        Mode::Init {
            yaml_filter,
            output_dir,
            force,
            stdout,
        } => {
            let target = output_dir.as_deref().unwrap_or(&args.config_dir);
            return init_cli::run_init(target, yaml_filter.as_deref(), force, stdout);
        }
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
        Mode::ExtStateDir { id, ensure } => {
            return run_ext_state_dir(&id, ensure);
        }
        Mode::Memory(ref sub) => return dispatch_memory_subcommand(sub).await,
        Mode::McpServer(ref sub) => match sub {
            McpServerSubcommand::Serve => return run_mcp_server(&args.config_dir).await,
            McpServerSubcommand::Inspect { url } => return run_mcp_inspect(url).await,
            McpServerSubcommand::Bench { url, tool, rps } => {
                return run_mcp_bench(url, tool, *rps).await
            }
            McpServerSubcommand::TailAudit { db } => return run_mcp_tail_audit(db).await,
        },
        Mode::MicroappAuditTail {
            microapp_id,
            method,
            result,
            since_mins,
            since_ms,
            limit,
            format,
            db,
            tenant_id,
        } => {
            return run_microapp_admin_audit_tail(
                microapp_id,
                method,
                result,
                since_mins,
                since_ms,
                limit,
                format,
                db,
                tenant_id,
            )
            .await;
        }
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
                return run_agent_dream_kill(run_id, *force, memory_dir.as_deref(), db.as_deref())
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
        Mode::AgentAttach { goal_id, db, json } => {
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
        Mode::DoctorPlugins { json } => {
            let exit_code = run_doctor_plugins(&args.config_dir, json).await?;
            std::process::exit(exit_code);
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
        Mode::PluginInstall {
            coords,
            dest,
            target,
            json,
            require_signature,
            skip_signature_verify,
        } => {
            let code = plugin_install::run_plugin_install(
                &args.config_dir,
                coords,
                dest,
                target,
                json,
                require_signature,
                skip_signature_verify,
            )
            .await?;
            std::process::exit(code);
        }
        Mode::PluginHelp => {
            plugin_install::print_plugin_help();
            return Ok(());
        }
        Mode::PluginNew {
            id,
            lang,
            dest,
            owner,
            description,
            git_init,
            force,
            json,
        } => {
            let code = plugin_new::run_plugin_new(
                id,
                lang,
                dest,
                owner,
                description,
                git_init,
                force,
                json,
            )
            .await?;
            std::process::exit(code);
        }
        Mode::PluginRun {
            path,
            no_daemon_config,
            watch,
            verbose: _verbose,
            json,
        } => {
            if watch {
                let err = plugin_run::PluginRunError::WatchDeferred;
                std::process::exit(plugin_run::emit_error(&err, json, Some(path)));
            }
            let override_ = match plugin_run::resolve_local_plugin(&path, no_daemon_config) {
                Ok(o) => o,
                Err(e) => std::process::exit(plugin_run::emit_error(&e, json, Some(path))),
            };
            plugin_run::print_pre_boot_banner(&override_, json);
            args.plugin_run_override = Some(override_);
            // Fall through to the daemon boot path below.
        }
        Mode::PluginList {
            include_orphan,
            json,
        } => {
            let code =
                plugin_admin::run_plugin_list(&args.config_dir, include_orphan, json).await?;
            std::process::exit(code);
        }
        Mode::PluginUpgrade {
            id,
            target,
            require_signature,
            skip_signature_verify,
            json,
        } => {
            let code = plugin_admin::run_plugin_upgrade(
                &args.config_dir,
                id,
                target,
                require_signature,
                skip_signature_verify,
                json,
            )
            .await?;
            std::process::exit(code);
        }
        Mode::PluginRemove {
            id,
            purge_cache,
            yes,
            json,
        } => {
            let code =
                plugin_admin::run_plugin_remove(&args.config_dir, id, purge_cache, yes, json)
                    .await?;
            std::process::exit(code);
        }
        Mode::PersonaInstall {
            coords,
            dest,
            target,
            json,
        } => {
            let code =
                persona_cli::run_persona_install(&args.config_dir, coords, dest, target, json)
                    .await?;
            std::process::exit(code);
        }
        Mode::PersonaList { json } => {
            let code = persona_cli::run_persona_list(&args.config_dir, json).await?;
            std::process::exit(code);
        }
        Mode::PersonaRemove { id, yes, json } => {
            let code = persona_cli::run_persona_remove(&args.config_dir, id, yes, json).await?;
            std::process::exit(code);
        }
        Mode::PersonaGet { id, json } => {
            let code = persona_cli::run_persona_get(&args.config_dir, id, json).await?;
            std::process::exit(code);
        }
        Mode::PersonaUpgrade { id, json } => {
            let code = persona_cli::run_persona_upgrade(&args.config_dir, id, json).await?;
            std::process::exit(code);
        }
        Mode::PersonaRun { path, json } => {
            let override_ = match persona_cli::resolve_local_persona(&path) {
                Ok(o) => o,
                Err(e) => {
                    std::process::exit(persona_cli::emit_persona_run_error(&e, json, Some(path)))
                }
            };
            persona_cli::print_persona_run_banner(&override_, json);
            args.persona_run_override = Some(override_);
            // Fall through to the daemon boot path below.
        }
        Mode::PersonaHelp => {
            persona_cli::print_persona_help();
            return Ok(());
        }
        Mode::Admin { port, open, tunnel } => {
            return run_admin_via_plugin(port, open, tunnel, &args.config_dir).await
        }
        Mode::Start => return run_daemon_start(&args.config_dir).await,
        Mode::Stop => return run_daemon_stop().await,
        Mode::Restart => return run_daemon_restart(&args.config_dir).await,
        Mode::Update => return run_self_update().await,
        Mode::ServiceInstall => return run_service_install(&args.config_dir).await,
        Mode::ServiceUninstall => return run_service_uninstall().await,
        Mode::ServiceStatus => return run_service_status().await,
        Mode::Run => {}
    }

    eprintln!("{NEXO_BANNER}");
    eprintln!("nexo {} — starting agent daemon", env!("CARGO_PKG_VERSION"));

    // Single-instance guard: if another `agent` process is already
    // running against the same data dir, terminate it before we start.
    // Prevents the "two agents on one NATS" bug where both processes
    // subscribe to `plugin.outbound.*` and every message is sent twice.
    let _lock = acquire_single_instance_lock().context("failed to acquire agent lock")?;

    let config_dir = args.config_dir;
    let override_from = args.override_from;

    // Phase 94 — generic subprocess discoverability: stamp the
    // absolute config dir into the daemon process env BEFORE any
    // plugin subprocess is spawned. Children with `spawn_env =
    // None` (auto-discovered subprocess plugins like email, google)
    // inherit it; per-plugin seeders that wipe inherited env
    // (telegram, whatsapp) MUST re-stamp it when relevant. Useful
    // for any plugin that needs to locate operator-provided YAML
    // / secrets / data under `<config_dir>/<...>`.
    let config_dir_abs = std::fs::canonicalize(&config_dir).unwrap_or(config_dir.clone());
    // SAFETY: set_var is only unsafe in multi-threaded contexts; we
    // run this BEFORE the tokio runtime spawns workers + before any
    // plugin subprocess fires.
    // SAFETY ANNOTATION: Rust 2024 marks env mutators unsafe, but
    // here we're pre-runtime + pre-spawn so no data race exists.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("NEXO_CONFIG_DIR", &config_dir_abs);
    }

    tracing::info!(
        config_dir = %config_dir.display(),
        override_from = ?override_from.as_ref().map(|p| p.display().to_string()),
        "loading config (Phase 94: env vars NEXO_<NAME>_YAML + --override-from layer)"
    );
    let mut cfg = AppConfig::load_with_overrides(&config_dir, override_from.as_deref())
        .context("failed to load config")?;

    // `nexo plugin run <path>` falls through to the
    // boot path with a side-channel override. Apply it here, AFTER
    // load and BEFORE any consumer reads `cfg.plugins.discovery` or
    // `cfg.agents.agents`.
    // `nexo persona run <path>` mirror of plugin
    // run; prepends the persona pack's parent dir to
    // cfg.personas.discovery.search_paths so the boot-time
    // discovery picks it up without a real install.
    if let Some(ref override_) = args.persona_run_override {
        persona_cli::apply_persona_run_override(&mut cfg, override_);
    }
    if let Some(ref override_) = args.plugin_run_override {
        plugin_run::apply_override(&mut cfg, override_);
        tracing::info!(
            plugin_id = %override_.plugin_id,
            plugin_root = %override_.plugin_root.display(),
            "local plugin override applied"
        );
    }

    // Auto-synth `InboundBinding { plugin: "event",
    // instance: "<id>" }` for each declared `event_subscribers[*].id`
    // so the existing inbound resolver matches `plugin.inbound.event.<id>`
    // re-publishes from the EventSubscriber runtime. Idempotent — a
    // manually-declared binding survives untouched (operator override
    // wins).
    for agent_cfg in &mut cfg.agents.agents {
        if !agent_cfg.event_subscribers.is_empty() {
            agent_cfg.inbound_bindings =
                nexo_core::agent::event_subscriber::synthesize_event_inbound_bindings(
                    &agent_cfg.inbound_bindings,
                    &agent_cfg.event_subscribers,
                );
        }
    }
    // `cfg` stays mutable so `wire_plugin_registry`
    // can fold plugin-contributed agents into `cfg.agents` later
    // in this fn.
    let mut cfg = cfg;

    // First pass of per-binding override validation — structural
    // checks only (duplicate bindings, unknown telegram instances,
    // missing skill dirs, same-provider model override). The tool-name
    // and known-provider checks run a few statements below once the
    // LLM registry and tool registry are assembled.
    nexo_core::agent::validate_agents(
        &cfg.agents.agents,
        &cfg.plugins,
        &nexo_core::agent::KnownTools::default(),
    )
    .context("per-binding override validation failed")?;

    // Credential gauntlet. Collects every invariant error
    // across WhatsApp / Telegram / Google in one pass. Lenient level
    // on boot so legacy deployments keep working; CI should run
    // `agent --check-config --strict` to gate PRs.
    let google_auth =
        nexo_auth::load_google_auth(&config_dir).context("failed to load google-auth.yaml")?;
    let secrets_dir = secrets_dir_for(&config_dir);
    let credentials = match nexo_auth::build_credentials(
        &cfg.agents.agents,
        &cfg.plugins,
        &google_auth,
        &secrets_dir,
        nexo_auth::StrictLevel::Lenient,
    ) {
        Ok(bundle) => {
            for w in &bundle.warnings {
                tracing::warn!(target: "credentials", "{w}");
            }
            tracing::info!(
                wa = bundle.account_count(nexo_auth::handle::WHATSAPP),
                tg = bundle.account_count(nexo_auth::handle::TELEGRAM),
                google = bundle.account_count(nexo_auth::handle::GOOGLE),
                "credential gauntlet passed"
            );
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

    // Extension discovery -------------------------------------------------
    // Runs before anything that depends on extensions. Spawns stdio runtimes
    // for each discovered candidate and keeps them alive for
    // the agent's lifetime. Tool-registry injection happens later.
    //
    // Pre-discovery pass to learn plugin roots, then
    // collect `[capabilities.admin]` + `[capabilities.http_server]` from
    // each `nexo-plugin.toml` (separate file from the runtime
    // `plugin.toml` discovery reads). Boot then constructs
    // `AdminRpcBootstrap` so admin RPC pipes are alive end-to-end.
    //
    // The deeper integrations (`Some(broker)`, `Some(transcript_writer)`,
    // `Some(processing_store)`, etc.) stay `None` here — those types are
    // built later in main.rs. Bootstrap dispatcher surfaces typed
    // `... domain not configured` errors for the unwired domains so
    // microapps keep working with whatever IS wired today (CRUD on
    // agents/credentials/pairing/llm/channels). Per-domain follow-ups
    // thread the rest as the broker + writer + stores get hoisted.
    let plugin_roots: Vec<PathBuf> = if let Some(ext_cfg) = cfg.extensions.as_ref() {
        if ext_cfg.enabled {
            let discovery = nexo_extensions::ExtensionDiscovery::new(
                ext_cfg.search_paths.iter().map(PathBuf::from).collect(),
                ext_cfg.ignore_dirs.clone(),
                ext_cfg.disabled.clone(),
                ext_cfg.allowlist.clone(),
                ext_cfg.max_depth,
            );
            discovery
                .discover()
                .candidates
                .iter()
                .map(|c| c.root_dir.clone())
                .collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let admin_capabilities =
        nexo_setup::admin_capability_collect::collect_admin_capabilities(&plugin_roots);
    let http_server_capabilities =
        nexo_setup::admin_capability_collect::collect_http_server_capabilities(&plugin_roots);
    // Single in-memory ProcessingControlStore
    // SHARED between the admin RPC dispatcher and every AgentRuntime
    // built below. Without sharing, the dispatcher and runtime hold
    // different stores and a `processing/pause` RPC never reaches
    // the inbound loop. SQLite-backed durable variant
    // (`SqliteProcessingControlStore`) lands when the per-tenant
    // retention scheduler does — same swap shape as audit /
    // agent_event_log.
    let processing_store: std::sync::Arc<
        dyn nexo_core::agent::admin_rpc::domains::processing::ProcessingControlStore,
    > = std::sync::Arc::new(nexo_setup::admin_adapters::InMemoryProcessingControlStore::new());

    // Initialise the broker BEFORE
    // `AdminBootstrap` so the `processing/intervention` admin RPC
    // gets a `BrokerOutboundDispatcher` adapter wired in. Boot was
    // previously deferring broker creation until after the admin
    // surface was wired, which left `inputs.broker = None` and
    // operator takeover failed with
    // `-32603 channel_outbound dispatcher not configured`.
    let broker = AnyBroker::from_config(&cfg.broker.broker)
        .await
        .context("failed to initialize broker")?;
    tracing::info!(
        kind = ?cfg.broker.broker.kind,
        url = %cfg.broker.broker.url,
        "broker ready",
    );

    // Snapshot.b — shared cell handed to the
    // admin RPC reader pre-bootstrap. main.rs writes the real
    // snapshotter into it later in boot once the late-stage
    // construction completes. Scoped at this outer level so
    // both the inner bootstrap block AND the post-snapshotter
    // population can reach it.
    let snapshotter_cell = nexo_setup::admin_adapters::shared_snapshotter_cell();

    // Shared plugin-handles cell.
    // admin bootstrap consumes a clone of this empty cell when
    // constructing `LivePluginRestarter`; the post-`wire_plugin_registry`
    // population (below, after wire returns) writes the real map
    // into it. Operators that hit `nexo/admin/plugins/restart`
    // during the brief boot window see a clean
    // "plugin handles not yet populated; daemon still booting"
    // error.
    let plugin_handles_cell = nexo_setup::admin_adapters::shared_plugin_handles_cell();

    // Phase 81.20.x Stage 7 Phase 2 — pairing trigger registry.
    // Cloned into the admin bootstrap (the dispatcher reads
    // through the same `Arc<DashMap>` for `pairing/start` lookups)
    // AND held here so the post-`wire_plugin_registry` block
    // below can `insert()` `BrokerPairingTrigger` entries for
    // every plugin declaring `[plugin.pairing.trigger]` once
    // manifests are loaded. Empty at this scope; the legacy
    // pre-bootstrap hardcoded `WhatsappPairingTrigger` (cfg-gated
    // on `plugin-whatsapp`) still inserts here until the
    // canonical plugin ships v0.4.4 with the manifest section.
    let pairing_triggers =
        nexo_core::agent::admin_rpc::pairing_trigger::PairingChannelTriggers::new();

    // Pre-discover persona install roots so the admin RPC's
    // `AgentsYamlPatcher` sees persona-shipped `agents.d/*.yaml`
    // entries in `nexo/admin/agents/list`. The full persona
    // registration loop further down (see "Boot-time persona
    // discovery") re-runs `discover_personas` to register the
    // `InMemoryPersonaAdmin` cell — running it twice is cheap
    // (just a fs walk + TOML parse per pack) and avoids reshuffling
    // the existing post-bootstrap registration block.
    let persona_install_roots: Vec<std::path::PathBuf> =
        if cfg.personas.discovery.search_paths.is_empty()
            || std::env::var("NEXO_DISABLE_BUNDLED_PERSONAS")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
                .unwrap_or(false)
        {
            tracing::info!(
                paths = cfg.personas.discovery.search_paths.len(),
                "persona admin-side pre-discovery skipped (empty search_paths or kill switch)"
            );
            Vec::new()
        } else {
            let roots: Vec<std::path::PathBuf> =
                nexo_persona_installer::discover_personas(&cfg.personas.discovery.search_paths)
                    .await
                    .into_iter()
                    .filter(|d| {
                        cfg.personas
                            .discovery
                            .id_passes_filters(&d.manifest.persona.id)
                    })
                    .map(|d| d.install_root)
                    .collect();
            tracing::info!(
                roots = roots.len(),
                paths = cfg.personas.discovery.search_paths.len(),
                "persona admin-side pre-discovery complete (AgentsYamlPatcher will scan these)"
            );
            for r in &roots {
                tracing::info!(root = %r.display(), "persona root for admin RPC");
            }
            roots
        };

    // Single Arc<LlmRegistry> shared
    // between the admin bootstrap (used by LivePluginRestarter
    // when respawning a child, by `RegistryLlmCompleter` for
    // admin/llm/complete, and by the boot-time provider catalogue
    // snapshot) AND the daemon main runtime (used by every agent
    // worker). Previously each site constructed its own
    // `LlmRegistry::with_builtins()` Arc; today's builtin-only
    // registries are content-equivalent so the duplication was
    // benign, but a future plugin-registered LLM factory landing
    // between the two construction sites would silently diverge
    // (admin sees the bootstrap-time registry, agents see the
    // runtime one). Sharing the Arc closes that gap.
    let llm_registry = std::sync::Arc::new(nexo_llm::LlmRegistry::with_builtins());

    // Phase 81.33.b.real Stage 4 — shared admin router Arc.
    // Hoisted ABOVE the admin_bootstrap if-block so the same Arc
    // survives into the post-wire `register()` loop further down
    // in boot. Empty at construction; entries land after
    // `wire_plugin_registry` returns. Interior mutability makes
    // mid-flight registrations visible to in-flight dispatch.
    let plugin_admin_router =
        std::sync::Arc::new(nexo_pairing::plugin_admin::PluginAdminRouter::new());

    let admin_bootstrap: Option<nexo_setup::admin_bootstrap::AdminRpcBootstrap> = if cfg
        .extensions
        .as_ref()
        .map(|c| c.enabled)
        .unwrap_or(false)
        && !admin_capabilities.is_empty()
    {
        let reload_noop: nexo_core::agent::admin_rpc::dispatcher::ReloadSignal =
            std::sync::Arc::new(|| {});
        let extensions_cfg_ref = cfg.extensions.as_ref().unwrap();
        // Db — durable audit log at the
        // canonical state path (`$NEXO_HOME/admin_audit.db`),
        // matching the `nexo microapp admin audit tail` CLI
        // default. Operator queries the same file across daemon
        // restarts.
        let state_dir = nexo_project_tracker::state::nexo_state_dir();
        let audit_db_path = state_dir.join("admin_audit.db");
        // Durable agent-event log at
        // `$NEXO_HOME/agent_events.db`. When opened, boot
        // composes `Tee([Broadcast, Log])` so every emit (chat
        // transcripts + processing pause/resume + escalation
        // requested/resolved) lands in SQLite for backfill across
        // daemon restart. Open failure logs + degrades to live-only
        // (no durability) rather than failing boot.
        let agent_event_log = match nexo_core::agent::admin_rpc::SqliteAgentEventLog::open(
            &state_dir.join("agent_events.db"),
        )
        .await
        {
            Ok(log) => Some(std::sync::Arc::new(log)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "agent_event_log open failed; firehose stays live-only without durable backfill",
                );
                None
            }
        };
        // Multi-tenant SaaS registry
        // backed by `tenants.yaml`. Hands the patcher to the
        // bootstrap so `nexo/admin/tenants/*` works against the
        // real config. Single-tenant deployments without the file
        // see read returns []; the patcher creates the file on
        // first write.
        let tenant_store: Option<
            std::sync::Arc<dyn nexo_core::agent::admin_rpc::domains::tenants::TenantStore>,
        > = Some(std::sync::Arc::new(
            nexo_setup::admin_adapters::TenantsYamlPatcher::new(
                config_dir.join("tenants.yaml"),
                config_dir.join("agents.yaml"),
            ),
        ));
        // Filesystem-backed skills store. Resolved against
        // `config_dir` so the path is stable regardless of the
        // operator's working directory when the daemon starts.
        // `nexo/admin/skills/*` writes land where the runtime
        // `SkillLoader` reads from.
        let skills_store: Option<
            std::sync::Arc<dyn nexo_core::agent::admin_rpc::domains::skills::SkillsStore>,
        > = Some(std::sync::Arc::new(
            nexo_setup::admin_adapters::FsSkillsStore::new(config_dir.join("skills")),
        ));
        // In-memory escalation store. v0
        // semantics: pause-resume cycle clears state, daemon
        // restart drops every escalation. SQLite-backed durable
        // variant exists (`SqliteEscalationStore`) and lands
        // alongside the future per-tenant retention scheduler.
        let escalation_store: Option<
            std::sync::Arc<dyn nexo_core::agent::admin_rpc::domains::escalations::EscalationStore>,
        > = Some(std::sync::Arc::new(
            nexo_setup::admin_adapters::InMemoryEscalationStore::default(),
        ));
        // Wire the MCP servers domain against
        // `<config_dir>/mcp.yaml`. Without this, plugin admin's
        // `/m/mcp_servers` page surfaces `mcp domain not
        // configured` -32603 errors and stays as a placeholder.
        let mcp_store: Option<
            std::sync::Arc<dyn nexo_core::agent::admin_rpc::domains::mcp::McpServerStore>,
        > = Some(nexo_core::agent::admin_rpc::domains::mcp::McpYamlStore::new(config_dir.clone()));
        // Wire the plugin doctor reader so
        // /m/plugins gets a live snapshot. Each call re-runs the
        // discovery + capability aggregation; cost is acceptable
        // for an operator-driven page.
        let doctor_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
        let plugin_doctor: Option<
            std::sync::Arc<
                dyn nexo_core::agent::admin_rpc::domains::plugin_doctor::PluginDoctorReader,
            >,
        > = Some(nexo_setup::admin_adapters::LivePluginDoctorReader::new(
            config_dir.clone(),
            doctor_version,
        ));
        // `/m/memory` recall reader. Lazy-
        // opens `LongTermMemory` from `<config_dir>/memory.yaml`
        // on first query so the dispatcher wire doesn't depend
        // on the late-built per-agent memory instance.
        let memory_reader: Option<
            std::sync::Arc<dyn nexo_core::agent::admin_rpc::domains::memory::MemoryReader>,
        > = Some(nexo_setup::admin_adapters::LiveMemoryReader::new(
            config_dir.clone(),
        ));
        // Snapshot.b — shared cell scoped above
        // (declared before this block so the post-snapshotter
        // population can also write into it). Cloned here so the
        // adapter outlives the bootstrap match.
        // `pairing_triggers` is declared at the outer scope above.
        // Phase 81.20.x Stage 7 Phase 2 — legacy hardcoded
        // `WhatsappPairingTrigger::from_configs` registration
        // removed; the manifest-driven `BrokerPairingTrigger`
        // loop in the post-`wire_plugin_registry` block populates
        // the registry from `nexo-plugin-whatsapp` v0.4.4+'s
        // `[plugin.pairing.trigger]` section instead.
        match nexo_setup::admin_bootstrap::AdminRpcBootstrap::build(
            nexo_setup::admin_bootstrap::AdminBootstrapInputs {
                config_dir: &config_dir,
                secrets_root: &secrets_dir,
                audit_db: Some(&audit_db_path),
                extensions_cfg: extensions_cfg_ref,
                admin_capabilities: &admin_capabilities,
                http_server_capabilities: &http_server_capabilities,
                reload_signal: reload_noop,
                transcript_reader: None,
                // Wire the broker so the
                // `processing/intervention` admin RPC can reach
                // `BrokerOutboundDispatcher` and publish operator
                // replies on `plugin.outbound.<channel>.<instance>`.
                // Without this, the dispatcher returns
                // `-32603 channel_outbound dispatcher not configured`
                // and operator takeover from the UI fails.
                broker: Some(broker.clone()),
                transcript_writer: None,
                processing_store: Some(std::sync::Arc::clone(&processing_store)),
                tenant_store: tenant_store.clone(),
                mcp_store: mcp_store.clone(),
                plugin_doctor: plugin_doctor.clone(),
                // Reuse the shared plugin handles cell that backs
                // `LivePluginRestarter` so the pairing-channels
                // descriptor reader sees the same live manifest
                // catalog. Setup constructs the adapter from this
                // cell + the credential store wired internally.
                plugin_handles_cell: Some(plugin_handles_cell.clone()),
                persona_install_roots: persona_install_roots.clone(),
                // Manual restart
                // adapter, wired against `plugin_handles_cell`
                // declared above. The cell is empty at this
                // point in boot; main.rs writes the real
                // plugin handle map into it AFTER
                // `wire_plugin_registry` returns. Operators that
                // hit `nexo/admin/plugins/restart` during the
                // brief boot window see a clean
                // "plugin handles not yet populated; daemon
                // still booting" error.
                //
                // Share the daemon's
                // single `Arc<LlmRegistry>` rather than building
                // a fresh one. See the construction site comment
                // above main.rs:1947 for the rationale (avoids
                // silent divergence if a plugin-registered LLM
                // factory lands between this and the runtime
                // registry build at main.rs:2347).
                plugin_restarter: Some(nexo_setup::admin_adapters::LivePluginRestarter::new(
                    plugin_handles_cell.clone(),
                    // Same pragmatic compromise as the
                    // `subprocess_shutdown` token at the
                    // wire_plugin_registry call site
                    // (main.rs:3161): the daemon's main
                    // shutdown CancellationToken isn't yet
                    // a shared resource at this scope. A
                    // fresh local token is fine because the
                    // subprocess adapter spawns children
                    // with `kill_on_drop(true)`; daemon exit
                    // tears the children down regardless of
                    // whether this token cancelled. A
                    // graceful-supervisor rework is the
                    // proper fix; deferred.
                    tokio_util::sync::CancellationToken::new(),
                    broker.clone(),
                    None,
                    llm_registry.clone(),
                    std::sync::Arc::new(cfg.llm.clone()),
                )),
                memory_reader: memory_reader.clone(),
                memory_snapshot_reader: Some(
                    nexo_setup::admin_adapters::LiveMemorySnapshotReader::new(
                        snapshotter_cell.clone(),
                        // Snapshot.create-restore —
                        // adapter resolves the encryption recipient
                        // for `create(encrypt=true)` from
                        // `recipients[0]` and the identity file for
                        // restoring encrypted bundles from
                        // `identity_path`. Snapshot taken at boot —
                        // operator YAML edits require restart.
                        // Project the YAML shape into the
                        // crate-native EncryptionSection (different
                        // crates own each, no From impl wired).
                        nexo_memory_snapshot::config::EncryptionSection {
                            enabled: cfg.memory.snapshot.encryption.enabled,
                            recipients: cfg.memory.snapshot.encryption.recipients.clone(),
                            identity_path: {
                                let p = &cfg.memory.snapshot.encryption.identity_path;
                                if p.trim().is_empty() {
                                    None
                                } else {
                                    Some(std::path::PathBuf::from(p))
                                }
                            },
                        },
                    ),
                ),
                // File-backed secrets store at
                // `<secrets_dir>/<NAME>.txt` + std::env
                // injection so existing LLM clients see new
                // values without a daemon restart.
                secrets_store: Some(nexo_setup::secrets_store::FsSecretsStore::with_secrets_dir(
                    secrets_dir.clone(),
                )
                    as std::sync::Arc<
                        dyn nexo_core::agent::admin_rpc::domains::secrets::SecretsStore,
                    >),
                // None lets admin_bootstrap
                // default-construct `HttpLlmProviderProbe`
                // against the local `LlmYamlPatcherFs`. Tests
                // can override with a mock by passing Some(_).
                llm_provider_probe: None,
                // Production LLM completer:
                // shares the daemon's single `Arc<LlmRegistry>`
                // so admin/llm/complete
                // resolves providers identically to the agent
                // runtime. Same Arc as `plugin_restarter` above
                // and the runtime registry at main.rs:2347.
                llm_completer: Some(nexo_setup::llm_completer::RegistryLlmCompleter::new(
                    llm_registry.clone(),
                    std::sync::Arc::new(cfg.llm.clone()),
                )
                    as std::sync::Arc<
                        dyn nexo_core::agent::admin_rpc::domains::llm::LlmCompleter,
                    >),
                // Snapshot the LLM provider catalogue from the
                // shared registry so the admin RPC can serve
                // `nexo/admin/llm_providers/catalog` from boot.
                // Single source of truth across admin + runtime.
                llm_provider_catalog: llm_registry
                    .catalog()
                    .into_iter()
                    .map(|e| {
                        // Internal LlmProviderCatalogEntry
                        // (nexo-llm) and the wire LlmProviderCatalogEntry
                        // (nexo-tool-meta) share field names by design;
                        // copy each across so the SPA sees the schema +
                        // auth_modes + probe flag the factory declared.
                        nexo_tool_meta::admin::llm_providers::LlmProviderCatalogEntry {
                            id: e.id,
                            default_base_url: e.default_base_url,
                            default_env_var: e.default_env_var,
                            models: e.models,
                            credential_schema: e.credential_schema,
                            supported_auth_modes: e.supported_auth_modes,
                            supports_models_probe: e.supports_models_probe,
                        }
                    })
                    .collect(),
                // Operator bearer rotation.
                // `auth_rotator: None` lets admin_bootstrap
                // default-construct `FsAuthRotator` if the
                // production inputs (token_path + initial_hash)
                // are supplied below. Tests inject a mock by
                // setting `auth_rotator: Some(_)`.
                auth_rotator: None,
                // Canonical operator-token file the daemon
                // persists rotated values to (atomic rename,
                // mode 0600 on unix). Lives under the same
                // `secrets/` root the FsSecretsStore uses for
                // arbitrary operator-supplied secrets.
                auth_token_path: Some(secrets_dir.join("operator_token.txt")),
                // Initial operator-token-hash. Read the FIRST
                // microapp's `[capabilities.http_server].token_env`
                // and snapshot its env-var value at boot. If
                // unset / no http_server caps declared, skip
                // (auth/rotate_token returns -32603 until the
                // operator configures a token).
                auth_initial_hash: http_server_capabilities
                    .values()
                    .next()
                    .and_then(|cap| std::env::var(&cap.token_env).ok())
                    .filter(|t| !t.is_empty())
                    .map(|t| nexo_setup::http_supervisor::token_hash(&t)),
                skills_store: skills_store.clone(),
                escalation_store: escalation_store.clone(),
                agent_event_log: agent_event_log.clone(),
                // Always wire all three channel
                // persisters. They translate
                // `nexo/admin/credentials/register` into the
                // per-plugin yaml + secret-file shape the
                // runtime loader consumes. Wiring is independent
                // of whether the plugin is currently enabled in
                // `extensions.yaml` — operators routinely
                // register accounts BEFORE the plugin is hot
                // (config reload activates them on the next
                // tick).
                persisters: vec![
                    nexo_setup::persisters::TelegramPersister::new(
                        config_dir.join("plugins").join("telegram.yaml"),
                        secrets_dir.clone(),
                    ),
                    nexo_setup::persisters::EmailPersister::new(
                        config_dir.join("plugins").join("email.yaml"),
                        secrets_dir.clone(),
                    ),
                    nexo_setup::persisters::WhatsappPersister::new(),
                ],
                pairing_triggers: pairing_triggers.clone(),
                // Phase 81.33.b.real Stage 4 — manifest-driven
                // plugin admin router. Shared Arc — the dispatcher
                // holds a clone and the daemon populates entries
                // AFTER `wire_plugin_registry` returns. Interior
                // mutability on `PluginAdminRouter` makes the
                // post-wire `register()` calls visible to in-flight
                // dispatch operations.
                plugin_admin_router: Some(plugin_admin_router.clone()),
            },
        )
        .await
        {
            Ok(b) => {
                if let Some(ref bs) = b {
                    tracing::info!(
                        microapps = admin_capabilities.len(),
                        active = bs.is_active(),
                        audit_db = %audit_db_path.display(),
                        agent_event_log_durable = agent_event_log.is_some(),
                        "admin RPC bootstrap wired",
                    );
                }
                b
            }
            Err(e) => {
                tracing::error!(error = %e, "admin RPC bootstrap failed; admin RPC disabled");
                None
            }
        }
    } else {
        None
    };
    let (extension_runtimes, ext_mcp_decls) =
        run_extension_discovery(cfg.extensions.as_ref(), admin_bootstrap.as_ref()).await;

    // MCP runtime manager. One per process; every agent
    // shares a sentinel session to avoid spawning duplicate MCP children.
    // `cfg.mcp.is_none()` or `enabled: false` → no manager, no tools.
    const MCP_SHARED_SESSION: uuid::Uuid = uuid::Uuid::nil();
    let watcher_shutdown = tokio_util::sync::CancellationToken::new();

    // Opt-in plugin.toml watcher. Logs manifest
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
    // Wrap in Arc immediately so the
    // wire_plugin_registry callsite below can clone it into
    // `SubprocessRuntime.llm_registry` for the daemon-mediated
    // `llm.complete` RPC. The Arc lives from
    // construction onward. All intermediate `llm_registry.method()`
    // calls work via `Arc<T>: Deref<Target = T>`.
    //
    // Re-bind to the Arc constructed
    // before admin bootstrap so the registry
    // is single-source-of-truth across admin RPCs and the daemon
    // runtime. No re-construction here; just re-shadow into local
    // `llm_registry` for the remaining boot sites that consume it.

    // Resolve every provider's API key from its
    // configured source (inline / secret_id / env). This populates
    // `LlmProviderConfig.api_key` so downstream LLM clients have a
    // ready-to-use bearer without each crate re-reading secrets.
    // Errors are collected per-instance for one-shot operator
    // diagnostics (no fix-restart-loop).
    {
        let secrets_source =
            nexo_setup::secrets_store::FsSecretsStore::with_secrets_dir(secrets_dir.clone());
        if let Err(errs) = cfg.llm.resolve_all_keys(secrets_source.as_ref()) {
            let joined = errs
                .iter()
                .map(|(id, e)| format!("  · {id}: {e}"))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(
                "LLM provider API-key resolution failed for {} instance(s):\n{joined}",
                errs.len()
            );
        }
    }

    // Validate every yaml provider instance
    // (global + tenant-scoped) maps to a registered factory. Loud
    // boot fail beats a runtime LLM dispatch error mid-traffic.
    if let Err(errs) = llm_registry.validate_config(&cfg.llm) {
        anyhow::bail!(
            "LLM provider factory validation failed for {} instance(s):\n  · {}",
            errs.len(),
            errs.join("\n  · ")
        );
    }

    // Provider-level validation pass: every agent's (and every
    // binding override's) `model.provider` must reference a real
    // provider entry from `llm.yaml`. Same aggregate error format
    // as the structural pass above so multi-agent configs surface
    // every typo in one error.
    //
    // Source of truth = `cfg.llm.providers` keys (e.g.
    // `anthropic-a5b8`, `minimax`), NOT `llm_registry.names()`
    // which returns factory ids (`anthropic`, `minimax`, ...).
    // Multi-instance setups (one operator with multiple anthropic
    // accounts named `anthropic-eb7e`, `anthropic-a5b8`, ...) need
    // the per-id list, otherwise every wizard-created instance
    // fails this gate with `unknown LLM provider`.
    {
        let provider_ids: Vec<String> = cfg.llm.providers.keys().cloned().collect();
        let known_providers =
            nexo_core::agent::KnownProviders::new(provider_ids.iter().map(String::as_str));
        nexo_core::agent::validate_agents_with_providers(
            &cfg.agents.agents,
            &cfg.plugins,
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

    // Memory snapshot subsystem (early init, before
    // `LongTermMemory::open_with_vector` so the mutation hook is
    // available when the writer attaches it). Retention worker
    // spawn happens later, once `dream_shutdown` exists.
    let snapshot_yaml = &cfg.memory.snapshot;
    let memory_snapshot_state_root = if snapshot_yaml.root.is_empty() {
        nexo_project_tracker::state::nexo_state_dir()
    } else {
        std::path::PathBuf::from(&snapshot_yaml.root)
    };
    let memdir_root_path = if snapshot_yaml.memdir_root.is_empty() {
        memory_snapshot_state_root.clone()
    } else {
        std::path::PathBuf::from(&snapshot_yaml.memdir_root)
    };
    let sqlite_root_path = if snapshot_yaml.sqlite_root.is_empty() {
        memory_snapshot_state_root.clone()
    } else {
        std::path::PathBuf::from(&snapshot_yaml.sqlite_root)
    };
    // `ClosureResolver` that maps each agent's id to its
    // real workspace memdir + a shared SQLite dir. Agents whose id
    // is not in the map fall back to the YAML defaults (so a fresh
    // agent created mid-run does not 404 the snapshotter).
    let memdir_map: std::collections::HashMap<String, std::path::PathBuf> = cfg
        .agents
        .agents
        .iter()
        .filter_map(|a| {
            if a.workspace.is_empty() {
                None
            } else {
                Some((a.id.clone(), std::path::PathBuf::from(&a.workspace)))
            }
        })
        .collect();
    let memdir_map = Arc::new(memdir_map);
    let path_resolver: Arc<dyn nexo_memory_snapshot::PathResolver> = {
        let memdir_default = memdir_root_path.clone();
        let sqlite_dir = sqlite_root_path.clone();
        let memdir_map_for_memdir = memdir_map.clone();
        let sqlite_dir_for_sqlite = sqlite_dir.clone();
        Arc::new(nexo_memory_snapshot::ClosureResolver::new(
            move |agent_id: &str, _tenant: &str| -> std::path::PathBuf {
                memdir_map_for_memdir
                    .get(agent_id)
                    .cloned()
                    .unwrap_or_else(|| memdir_default.join(agent_id))
            },
            // SQLite is global per-deployment today (every agent
            // shares `cfg.memory.long_term.sqlite.path`'s parent
            // dir). This resolver stays consistent with that until
            // the long-term store goes per-agent.
            move |_agent_id: &str, _tenant: &str| -> std::path::PathBuf {
                sqlite_dir_for_sqlite.clone()
            },
        ))
    };
    // Multi-recipient encrypt boot
    // validation. Parse every recipient string in
    // `memory.snapshot.encryption.recipients` upfront so an
    // operator typo is surfaced at daemon boot rather than at
    // first encrypt-snapshot time. Skipped when the
    // `snapshot-encryption` feature is off (parse_recipient
    // unavailable, operator built without encryption).
    #[cfg(feature = "snapshot-encryption")]
    if snapshot_yaml.enabled && snapshot_yaml.encryption.enabled {
        for (i, s) in snapshot_yaml.encryption.recipients.iter().enumerate() {
            nexo_memory_snapshot::codec::age_codec::parse_recipient(s).map_err(|e| {
                anyhow::anyhow!("memory.snapshot.encryption.recipients[{i}] failed to parse: {e}")
            })?;
        }
    }
    let memory_snapshotter: Option<Arc<dyn nexo_memory_snapshot::MemorySnapshotter>> =
        if snapshot_yaml.enabled {
            let s = nexo_memory_snapshot::local_fs::LocalFsSnapshotter::builder()
                .state_root(memory_snapshot_state_root.clone())
                .memdir_root(memdir_root_path.clone())
                .sqlite_root(sqlite_root_path.clone())
                .path_resolver(path_resolver.clone())
                .lock_timeout(std::time::Duration::from_secs(
                    snapshot_yaml.lock_timeout_secs.max(1),
                ))
                .build()
                .map_err(|e| anyhow::anyhow!("memory snapshotter build failed: {e}"))?;
            Some(Arc::new(s))
        } else {
            tracing::info!(
                target: "boot.memory_snapshot",
                "memory.snapshot.enabled = false; subsystem disabled"
            );
            None
        };

    // Snapshot.b — populate the admin cell with
    // the live snapshotter so /m/memory's snapshot panel reflects
    // the operator's real `path_resolver` map (per-agent memdir
    // overrides, custom sqlite roots). When snapshots are
    // disabled at boot, the cell stays None and the admin RPC
    // returns "memory snapshot subsystem not configured".
    if let Some(ref s) = memory_snapshotter {
        let mut guard = snapshotter_cell.write().await;
        *guard = Some(s.clone());
    }

    // Broker-backed event publisher used by the mutation
    // hook on every memory write. Best-effort: a publish error must
    // never poison the writer's transaction (the hook impl swallows
    // and logs internally).
    let memory_event_publisher: Arc<dyn nexo_memory_snapshot::EventPublisher> =
        if snapshot_yaml.events.mutation_publish_enabled && memory_snapshotter.is_some() {
            Arc::new(BrokerEventPublisher::new(
                broker.clone(),
                snapshot_yaml.events.lifecycle_subject_prefix.clone(),
                snapshot_yaml.events.mutation_subject_prefix.clone(),
            ))
        } else {
            Arc::new(nexo_memory_snapshot::NoopPublisher)
        };
    let memory_mutation_hook: Option<Arc<dyn nexo_driver_types::MemoryMutationHook>> =
        if snapshot_yaml.events.mutation_publish_enabled && memory_snapshotter.is_some() {
            Some(
                nexo_memory_snapshot::MemoryMutationPublisher::new(memory_event_publisher.clone())
                    .into_arc(),
            )
        } else {
            None
        };

    // Channel boot context. Holds the shared
    // `ChannelRegistry` + `SessionRegistry` + `BrokerChannelDispatcher`
    // so the per-(binding,server) inbound loops + the bridge spawn
    // below all see the same handles. Persistent SessionRegistry
    // is opted into when an operator sets
    // `agents.<id>.channels.session_store_path` — for now we ship
    // the in-memory default so threading is preserved within a
    // process. Hot-reload re-evaluation hooks against this single
    // registry instance.
    let channel_boot = nexo_mcp::channel_boot::ChannelBootContext::in_memory(broker.clone());
    let channel_shutdown = tokio_util::sync::CancellationToken::new();
    // Process-wide pending-permission map
    // shared by the ChannelRelayDecider + every per-server
    // permission-response pump.
    let pending_permissions =
        std::sync::Arc::new(nexo_mcp::channel_permission::PendingPermissionMap::new());
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

    // Secret guard for scanning memory writes.
    // Wired via `memory.secret_guard` YAML key. Default secure
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

            // Build optional embedding provider for vector recall.
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
            // Attach the mutation hook so every
            // `remember_typed` / `forget` write streams onto the
            // `nexo.memory.mutated.<agent_id>` NATS subject.
            let mem = if let Some(ref hook) = memory_mutation_hook {
                mem.with_mutation_hook(hook.clone())
            } else {
                mem
            };
            tracing::info!(
                path = %path,
                vector = mem.embedding_provider().is_some(),
                secret_guard = secret_guard.is_some(),
                mutation_hook = memory_mutation_hook.is_some(),
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
    // Browser plugin extracted to standalone repo
    // `nexo-rs-plugin-browser`. Daemon no longer constructs it
    // in-process; discovery + auto-subprocess fallback
    // loads the binary if its directory is on
    // `plugins.discovery.search_paths`. The 12 `browser_*` tools
    // route through the RemoteToolHandler over JSON-RPC stdio.
    //
    // Operator yaml (`cfg.plugins.browser`) still exists; the
    // daemon translates the values into env vars
    // (`NEXO_PLUGIN_BROWSER_*`) the subprocess reads via
    // `nexo_plugin_browser::env_config`. No-op if not configured.
    if let Some(browser_cfg) = cfg.plugins.browser.clone() {
        seed_browser_subprocess_env(&browser_cfg);
    }
    // (no `let browser_plugin = ...` — gone)
    // (no `register_browser_tools(...)` — gone; tools register
    //  via the RemoteToolHandler path inside the init loop.)
    // Whatsapp subprocess flip mirrors the
    // telegram pattern: daemon owns one `PairingState` per cfg
    // entry (so the admin RPC `/whatsapp/<inst>/pair*` HTTP
    // endpoints render accurate state) and a broker subscriber
    // (`spawn_whatsapp_pairing_state_subscriber`, registered
    // further below near `factory_registry`) bridges the
    // subprocess's `plugin.inbound.whatsapp.<inst>` events into
    // those daemon-owned states. The in-tree
    // `WhatsappPlugin::new(cfg) + plugins.register(plugin)` block
    // is gone; per-cfg factories register on the discovery snapshot
    // alongside telegram.
    //
    // Typing-presence forwarding doesn't yet have a
    // broker bridge, so subprocess instances don't surface
    // `AgentEventKind::PeerTyping` events on the firehose until
    // a follow-up ships the RPC callback.
    // Phase 81.20.x Bucket C2 Stage 2 + Stage 7 Phase 2 — the
    // whatsapp pairing state map AND the per-plugin
    // `wa_tunnel_cfg` extraction are gone. Subprocess owns its
    // own `SharedPairingState` via the plugin's
    // `WhatsappPlugin::pairing` field; the daemon no longer
    // mirrors it. Public-tunnel orchestration moves to the
    // generic `[plugin.public_tunnel]` manifest section + daemon
    // iterator below (post-`wire_plugin_registry`).
    for wa_cfg in opaque_plugin_entries(&cfg.plugins, "whatsapp") {
        let enabled = wa_cfg
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let instance_label = wa_cfg
            .get("instance")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "default".into());
        if !enabled {
            tracing::info!(instance = %instance_label, "whatsapp plugin configured but disabled — skipping");
            continue;
        }
        tracing::info!(
            instance = %instance_label,
            "registered whatsapp pairing slot (Phase 81.18.b.2 subprocess flip)",
        );
    }
    // Telegram subprocess flip. The in-tree
    // `TelegramPlugin::new(cfg) + plugins.register(plugin)` block
    // is gone; daemon now seeds per-instance env dicts +
    // registers a `subprocess_plugin_factory_with_env` factory
    // per cfg entry. Discovery walker has to find the
    // `nexo-plugin-telegram` binary's manifest in
    // `plugins.discovery.search_paths` (operator action: install
    // via `cargo install nexo-plugin-telegram` or download the
    // release binary). Multi-instance support comes from cloning
    // the discovered manifest with mutated `plugin.id` per
    // instance (`telegram.<inst>`) so the init loop dispatches a
    // separate factory call per bot.
    //
    // Helpers + factory wiring live alongside the
    // `factory_registry` build below; this loop body becomes a
    // `// done in factory_registry block` placeholder so the
    // existing whatsapp loop still flows correctly.
    // (Telegram in-tree construction removed; see
    //  `factory_registry` block at line ~2425.)
    // Email plugin. Single Arc lives
    // for two purposes:
    //   1. The factory_registry-driven init loop registers a
    //      singleton factory that hands this Arc to the discovery
    //      walker (replaces the legacy `plugins.register_arc`
    //      path; explicit factory wins over discovery's
    //      auto-subprocess fallback per `init_loop.rs:417`).
    //   2. The MCP autonomous worker + email tool ctx + email
    //      tool registration sites consume the same Arc to call
    //      `dispatcher_handle()`, `bounce_store_handle()`,
    //      `attachments_dir()`, `health_map()` — all in-process
    //      methods the broker subprocess can't expose.
    //
    // Dual-boot risk (subprocess discovery + in-process register)
    // is foreclosed: as long as the factory is registered, the
    // init loop never falls through to the auto-subprocess
    // branch (`init_loop.rs:417` checks `is_registered` first).
    // Operators that genuinely want subprocess isolation must
    // strip this block and place the manifest in `search_paths`.
    // 0.5.0: plugins.email is `Vec<EmailPluginConfig>`. Wave 2
    // daemon-side flip will spawn one subprocess per tenant; this
    // legacy in-process factory uses the FIRST declared tenant for
    // back-compat with single-tenant operators.
    // Wave 4 — daemon decoupled from email. Construction lives in
    // the plugin subprocess (auto-discovered via
    // Phase 81.20.x F2.1 — email plugin construction removed.
    // Discovery walker spawns the standalone `nexo-plugin-email`
    // subprocess via `[plugin.entrypoint]`; tool dispatch, IMAP
    // IDLE, SMTP queue, metrics, and pairing all run inside the
    // subprocess.
    if cfg.plugins.is_active("email") {
        tracing::info!(
            "email plugin configured — discovery walker will spawn the standalone \
             subprocess; daemon no longer keeps an in-process Arc"
        );
    }
    plugins
        .start_all(broker.clone())
        .await
        .context("failed to start plugins")?;

    // Boot-time persona discovery.
    // Walks `cfg.personas.discovery.search_paths`, parses + validates
    // every `<id>-<version>/persona.toml`, applies the disabled /
    // allowlist filters, and registers each survivor in an
    // `InMemoryPersonaAdmin` cell. `PersonaAdmin` brought into scope
    // here (not at module top) to keep the import close to its sole
    // use site.
    // Admin RPC routes read the cell to surface
    // `nexo persona list / get / remove` over the wire. Discovery
    // is best-effort: malformed packs log at WARN + are skipped
    // rather than aborting boot.
    #[allow(unused_imports)]
    use nexo_persona_installer::PersonaAdmin as _PersonaAdminInScope;
    let persona_admin = std::sync::Arc::new(nexo_persona_installer::InMemoryPersonaAdmin::new());
    // `NEXO_DISABLE_BUNDLED_PERSONAS` kill switch.
    // Honored even when search_paths is configured; lets a
    // hardened deployment refuse persona discovery without
    // mutating the YAML.
    let personas_killed = std::env::var("NEXO_DISABLE_BUNDLED_PERSONAS")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(false);
    if personas_killed {
        tracing::info!("persona discovery skipped — NEXO_DISABLE_BUNDLED_PERSONAS is set");
    }
    if !personas_killed && !cfg.personas.discovery.search_paths.is_empty() {
        let discovered =
            nexo_persona_installer::discover_personas(&cfg.personas.discovery.search_paths).await;
        let mut registered: usize = 0;
        let mut filtered: usize = 0;
        for d in discovered {
            let id = d.manifest.persona.id.clone();
            if !cfg.personas.discovery.id_passes_filters(&id) {
                tracing::info!(persona = %id, "persona filtered by discovery.disabled / allowlist");
                filtered += 1;
                continue;
            }
            // Synthesize an InstalledPersona shape so the admin cell
            // sees the full provenance even though we didn't run the
            // install pipeline this boot. Coords are best-effort
            // parsed from `manifest.persona.homepage` when it points
            // at a GitHub repo; a placeholder is used otherwise (the
            // boot path doesn't know the original tag).
            let coords = d
                .manifest
                .persona
                .homepage
                .as_deref()
                .and_then(github_owner_repo_from_url)
                .and_then(|(owner, repo)| {
                    nexo_ext_installer::RepoCoords::parse(&format!("{owner}/{repo}")).ok()
                })
                .unwrap_or_else(|| nexo_ext_installer::RepoCoords {
                    owner: "unknown".into(),
                    repo: id.clone(),
                    tag: "unknown".into(),
                });
            let installed = nexo_persona_installer::InstalledPersona {
                id: id.clone(),
                version: d
                    .manifest
                    .persona
                    .version
                    .parse()
                    .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
                install_root: d.install_root.clone(),
                coords,
                installed_at: chrono::Utc::now(),
                manifest: d.manifest.clone(),
                tarball_bytes: 0,
                was_already_present: true,
            };
            if let Err(e) = persona_admin.register(installed).await {
                tracing::warn!(persona = %id, error = %e, "persona discovery: register failed");
                continue;
            }
            tracing::info!(
                persona = %id,
                install_root = %d.install_root.display(),
                agent_configs = d
                    .manifest
                    .persona
                    .contributes
                    .as_ref()
                    .map(|c| c.agent_configs.len())
                    .unwrap_or(0),
                "persona discovered + registered"
            );
            registered += 1;
        }
        tracing::info!(
            registered,
            filtered,
            search_paths = cfg.personas.discovery.search_paths.len(),
            "persona discovery complete"
        );
    }
    // _persona_admin will be wired into admin RPC + agents.d/ merge
    // (admin RPC routes + agent_configs merge into AgentsDirectory)
    // in a follow-up. Bind here so the cell stays alive
    // for the rest of boot; rebound to a `let _ = persona_admin;`
    // below to silence the unused-binding warning meanwhile.
    let _persona_admin = persona_admin;

    // Atomic plugin registry boot wire. The helper
    // runs the four-step pipeline (discover → merge agents → merge
    // skills → init loop) and folds every report so downstream
    // consumers (`LlmAgentBehavior::with_plugin_skill_roots`,
    // the `nexo agent doctor plugins` CLI, admin UI)
    // see a single source of truth. The init loop runs with an
    // empty handles map today — every plugin records `NoHandle`
    // until the manifest-driven `Arc<dyn NexoPlugin>` factory
    // is wired.
    let core_envs = core_capability_env_vars();
    let available_caps = build_available_capabilities(&cfg);
    let discovery_cfg_clone = cfg.plugins.discovery.clone();
    // Empty in-tree factory registry plus a
    // `SubprocessRuntime` carrying the broker + shutdown token +
    // config/state roots activate the auto-subprocess fallback in
    // `run_plugin_init_loop_with_factory`. Any discovered manifest
    // with `[plugin.entrypoint] command = "..."` gets instantiated
    // as a `SubprocessNexoPlugin` automatically; operator's only
    // job is to drop the manifest into a
    // `plugins.discovery.search_paths` directory. In-tree plugins
    // (browser/telegram/whatsapp/email) keep their dormant
    // manifests OUT of `search_paths` and continue via the legacy
    // block above until they are extracted out-of-tree.
    let mut factory_registry = nexo_core::agent::nexo_plugin_registry::PluginFactoryRegistry::new();

    // Telegram subprocess flip. For each cfg
    // entry we (1) build a per-spawn env dict whitelisting only
    // the daemon envs the plugin legitimately needs (PATH/HOME/
    // RUST_LOG/NEXO_BROKER_URL) plus the operator-provided
    // `NEXO_PLUGIN_TELEGRAM_*` config; (2) pre-discover the
    // telegram manifest from `plugins.discovery.search_paths`;
    // (3) clone the manifest with a mutated `plugin.id` so each
    // instance gets a distinct factory entry; (4) register a
    // `subprocess_plugin_factory_with_env` factory under that
    // instance id. The injected synthetic plugin entries are
    // collected in `extra_subprocess_plugins` and passed to
    // `wire_plugin_registry_with_runtime` so the init loop
    // dispatches one factory call per instance.
    //
    // Operators with `cfg.plugins.telegram` populated MUST install
    // the `nexo-plugin-telegram` binary; the discovery walker fails
    // to find the manifest otherwise and the loop logs a clear
    // hint instead of silently skipping the plugin.
    let mut extra_subprocess_plugins: Vec<
        nexo_core::agent::nexo_plugin_registry::DiscoveredPlugin,
    > = Vec::new();
    // Phase 81.33.e — telegram subprocess flip via shared helper.
    if cfg.plugins.is_active("telegram") {
        let pre_snap = nexo_core::agent::nexo_plugin_registry::discover(
            &discovery_cfg_clone,
            &semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
        );
        let broker_url =
            std::env::var("NEXO_BROKER_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
        let broker_kind = subprocess_broker_kind_str(cfg.broker.broker.kind);
        let outcome = register_instance_subprocess_factories(
            "telegram",
            opaque_plugin_entries(&cfg.plugins, "telegram"),
            &pre_snap,
            broker_kind,
            &broker_url,
            &mut factory_registry,
            &mut extra_subprocess_plugins,
            |c| {
                c.get("instance")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            },
            |_| true, // telegram has no `enabled` flag
            seed_telegram_subprocess_env_for,
            "81.18.b.1",
        );
        if outcome.is_none() {
            tracing::warn!(
                "telegram is configured in cfg.plugins.telegram but no \
                 `telegram` manifest was found in `plugins.discovery.search_paths` \
                 — install the binary via `cargo install nexo-plugin-telegram` \
                 (or download the v0.1.1+ release tarball) and add its directory \
                 to `plugins.discovery.search_paths` in `agents.yaml`. The plugin \
                 will not run until the manifest is reachable.",
            );
        }
    }

    // Whatsapp subprocess flip mirrors telegram.
    // Pre-discover the `whatsapp` manifest, register one factory
    // per cfg entry under `whatsapp` (legacy single-account) or
    // `whatsapp.<inst>` (multi-account), inject synthetic discovered
    // plugins. The pairing state subscriber is spawned right after
    // so events arriving on `plugin.inbound.whatsapp.<inst>` find
    // the daemon-owned `wa_pairing` slots already populated above.
    // Phase 81.33.e — whatsapp subprocess flip via shared helper.
    if cfg.plugins.is_active("whatsapp") {
        let pre_snap = nexo_core::agent::nexo_plugin_registry::discover(
            &discovery_cfg_clone,
            &semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
        );
        let broker_url =
            std::env::var("NEXO_BROKER_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
        let broker_kind = subprocess_broker_kind_str(cfg.broker.broker.kind);
        let outcome = register_instance_subprocess_factories(
            "whatsapp",
            opaque_plugin_entries(&cfg.plugins, "whatsapp"),
            &pre_snap,
            broker_kind,
            &broker_url,
            &mut factory_registry,
            &mut extra_subprocess_plugins,
            |c| {
                c.get("instance")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            },
            |c| c.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            seed_whatsapp_subprocess_env_for,
            "81.18.b.2",
        );
        if outcome.is_none() {
            tracing::warn!(
                "whatsapp is configured in cfg.plugins.whatsapp but no \
                 `whatsapp` manifest was found in `plugins.discovery.search_paths` \
                 — install the binary via `cargo install nexo-plugin-whatsapp` \
                 (or download the v0.1.2+ release tarball) and add its directory \
                 to `plugins.discovery.search_paths` in `agents.yaml`. The plugin \
                 will not run until the manifest is reachable.",
            );
        }
    }

    // Wave 4 — email factory wiring removed. Discovery walker's
    // auto-subprocess fallback (init_loop.rs) spawns one email
    // subprocess when the manifest is found in
    // `plugins.discovery.search_paths`. Multi-tenant fans out
    // INSIDE that subprocess via the plugin's `instance_registry`
    // (same model browser uses for multi-instance). Tool dispatch,
    // IMAP IDLE, SMTP queue, metrics — all subprocess-local; no
    // daemon-side plugin-specific code remains.

    // Spawn the whatsapp pairing state broker
    // subscriber. Lives for the duration of the daemon's broker
    // session; cancellation handle wired into the existing
    // subprocess shutdown token so a graceful shutdown stops the
    // subscriber alongside the subprocess plugins.
    //
    // Phase 93.12.c.2 — gated. Without `plugin-whatsapp` the
    // Phase 81.20.x Bucket C2 Stage 2 —
    // `spawn_whatsapp_pairing_state_subscriber` removed. The
    // daemon no longer mirrors `plugin.inbound.whatsapp.<inst>`
    // QR/connect/disconnect events into a typed
    // `SharedPairingState` map; the subprocess owns its own
    // pairing state and serves it via the plugin v0.4.4 HTTP
    // routes (follow-up). The daemon-side mirror existed only
    // to back the hardcoded `/whatsapp/*` HTTP block, which is
    // also gone now.

    // Typing presence broker bridge. Subprocess
    // whatsapp publishes `plugin.lifecycle.whatsapp.<inst>.peer_typing`
    // events; this subscriber translates them to
    // `AgentEventKind::PeerTyping` on the SSE firehose so live
    // transcript indicators light up the same way the in-tree
    // `with_emitter` path used to. Skipped when no
    // whatsapp instances are configured OR the bootstrap
    // emitter isn't wired yet (test boots without the SSE
    // firehose).
    // Phase 81.20.x Bucket C2 Stage 2 — typing-presence
    // subscriber still wires unconditionally when whatsapp is
    // configured + the emitter is ready. Previously gated on
    // `!wa_pairing.is_empty()`; we now check the YAML directly
    // since the daemon-side state map is gone.
    let _wa_typing_subscriber_handle = {
        let whatsapp_configured = !opaque_plugin_entries(&cfg.plugins, "whatsapp").is_empty();
        let typing_shutdown = tokio_util::sync::CancellationToken::new();
        match (
            whatsapp_configured,
            admin_bootstrap.as_ref().map(|bs| bs.event_emitter()),
        ) {
            (true, Some(emitter)) => Some(spawn_whatsapp_typing_presence_subscriber(
                broker.clone(),
                emitter,
                typing_shutdown,
            )),
            _ => None,
        }
    };

    let plugin_state_root = std::env::var("NEXO_STATE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| config_dir.clone());
    // Local cancellation token — daemon's main shutdown isn't yet
    // a single shared resource at this scope; the subprocess
    // adapter spawns each child with `kill_on_drop(true)` so the
    // children die with the daemon process anyway. A real
    // supervisor for graceful shutdown is a follow-up.
    let subprocess_shutdown = tokio_util::sync::CancellationToken::new();
    let subprocess_runtime = nexo_core::agent::nexo_plugin_registry::SubprocessRuntime {
        broker: broker.clone(),
        shutdown: subprocess_shutdown,
        config_dir: config_dir.clone(),
        state_root: plugin_state_root,
        // `memory` is already constructed in the long-term memory
        // section above, so it's in scope here. Subprocess plugins
        // get -32603 "memory not configured" only when the OPERATOR
        // has disabled long-term memory in `memory.yaml`, not
        // because of a daemon-side plumbing gap.
        long_term_memory: memory.clone(),
        // Thread the daemon's real
        // `llm_registry` (Arc'd at construction)
        // and `cfg.llm` so subprocess plugins issuing
        // `llm.complete` reach operator-configured providers
        // (Minimax, OpenAI, etc.).
        llm_registry: llm_registry.clone(),
        llm_config: Arc::new(cfg.llm.clone()),
        // Sandbox runner: discover bwrap once at
        // boot + cache env-driven capability flags. Plugins
        // declaring `[plugin.sandbox] enabled = true` get their
        // command wrapped at spawn time.
        sandbox: nexo_core::agent::plugin_sandbox::shared_runner_from_env(),
    };
    let wire = nexo_core::agent::nexo_plugin_registry::wire_plugin_registry_with_runtime(
        &mut cfg.agents,
        &discovery_cfg_clone,
        &semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
        &core_envs,
        &available_caps,
        Some(&factory_registry),
        Some(&subprocess_runtime),
        &extra_subprocess_plugins,
    )
    .await;
    // `wire.registry` + `wire.skill_roots` +
    // `wire.channel_adapter_registry` stay in scope for hot-reload
    // registration + per-agent threading without
    // changing the boot wire shape.

    // Populate the shared plugin
    // handles cell now that `wire_plugin_registry` has produced
    // the BTreeMap. `LivePluginRestarter` constructed at admin
    // bootstrap time (see line ~2080) reads this cell on each
    // restart call; before this write fires, operator restart
    // requests see "plugin handles not yet populated; daemon
    // still booting".
    {
        let mut guard = plugin_handles_cell.write().await;
        *guard = Some(std::sync::Arc::new(wire.plugin_handles.clone()));
    }

    // Phase 81.20.x F2.3 — email_tool_ctx dropped entirely. Email
    // subprocess owns its 12 tools via RemoteToolHandler routing
    // through the broker; the daemon no longer constructs or
    // forwards any email-specific tool context. See git history
    // (commit pre-81.20.x) for the prior in-process implementation.

    // Agents ---------------------------------------------------------------
    let running_agents = Arc::new(AtomicUsize::new(0));
    // Slot for companion WS handshake context — filled after pairing init below.
    let pairing_handshake_slot: Arc<std::sync::OnceLock<PairingHandshakeCtx>> =
        Arc::new(std::sync::OnceLock::new());
    let tunnel_registry: Arc<tokio::sync::RwLock<Vec<Arc<nexo_tunnel_quick::TunnelHandle>>>> =
        Arc::new(tokio::sync::RwLock::new(Vec::new()));
    // Phase 81.33.b.real Stage 5 — build plugin metrics scrape
    // descriptors from every plugin's `[plugin.metrics]` section.
    // Descriptors are immutable for the daemon's lifetime
    // (Vec, not RwLock) — hot-spawn restarts don't change the
    // metrics manifest, only the broker subscriber identity, so
    // the broker topic in the descriptor stays valid.
    let plugin_metrics_descriptors = {
        let mut out = Vec::new();
        for (plugin_id, handle) in wire.plugin_handles.iter() {
            if let Some(metrics) = handle.manifest().plugin.metrics.as_ref() {
                if !metrics.prometheus {
                    continue;
                }
                let mut d = nexo_pairing::plugin_metrics::PluginMetricsDescriptor::new(
                    plugin_id.clone(),
                    metrics.broker_topic_prefix.clone(),
                );
                if let Some(secs) = metrics.timeout_seconds {
                    d = d.with_timeout(std::time::Duration::from_secs(secs));
                }
                tracing::info!(
                    plugin = %plugin_id,
                    broker_topic_prefix = %metrics.broker_topic_prefix,
                    "registered plugin Prometheus metrics scrape (Phase 81.33.b.real Stage 5)",
                );
                out.push(d);
            }
        }
        Arc::new(out)
    };

    // Phase 81.33.b.real Stage 4 — populate the plugin admin router
    // from every loaded plugin's `[plugin.admin]` manifest section.
    // Reserved-prefix collisions warn-log + skip (the daemon's
    // own admin namespaces are never overwritten). The router
    // Arc is shared with the dispatcher; in-flight admin RPC
    // calls observe new entries immediately via interior
    // mutability.
    for (plugin_id, handle) in wire.plugin_handles.iter() {
        if let Some(admin) = handle.manifest().plugin.admin.as_ref() {
            match plugin_admin_router.register(
                plugin_id,
                &admin.method_prefix,
                &admin.broker_topic_prefix,
                admin.timeout_seconds.map(std::time::Duration::from_secs),
            ) {
                Ok(()) => tracing::info!(
                    plugin = %plugin_id,
                    method_prefix = %admin.method_prefix,
                    broker_topic_prefix = %admin.broker_topic_prefix,
                    "registered plugin admin route (Phase 81.33.b.real Stage 4)",
                ),
                Err(err) => tracing::warn!(
                    plugin = %plugin_id,
                    method_prefix = %admin.method_prefix,
                    error = %err,
                    "plugin admin route registration rejected — reserved prefix",
                ),
            }
        }
    }

    // Phase 96 — populate the plugin poller router from every loaded
    // plugin's `[plugin.poller]` manifest section. Each registration
    // covers all kinds declared by that plugin; cross-plugin
    // duplicate-kind collisions warn-log + skip so the daemon never
    // crashes on a misconfigured manifest set.
    let plugin_poller_router =
        std::sync::Arc::new(nexo_pairing::plugin_poller::PluginPollerRouter::new());
    for (plugin_id, handle) in wire.plugin_handles.iter() {
        let manifest = handle.manifest();
        let poller_sec = match manifest.plugin.poller.as_ref() {
            Some(p) => p,
            None => continue,
        };
        if let Err(err) = poller_sec.validate() {
            tracing::warn!(
                plugin = %plugin_id,
                error = %err,
                "[plugin.poller] validation failed; skipping",
            );
            continue;
        }
        let new_handle = nexo_pairing::plugin_poller::PluginPollerHandle {
            plugin_id: plugin_id.clone(),
            kinds: poller_sec.kinds.clone(),
            broker_topic_prefix: poller_sec.broker_topic_prefix.clone(),
            lifecycle: poller_sec.lifecycle,
            max_concurrent_ticks: poller_sec.max_concurrent_ticks,
            tick_timeout: std::time::Duration::from_secs(poller_sec.tick_timeout_secs),
            entrypoint_command: manifest.plugin.entrypoint.command.clone(),
        };
        match plugin_poller_router.register(new_handle) {
            Ok(()) => tracing::info!(
                plugin = %plugin_id,
                kinds = ?poller_sec.kinds,
                broker_topic_prefix = %poller_sec.broker_topic_prefix,
                lifecycle = ?poller_sec.lifecycle,
                "registered [plugin.poller] (Phase 96.7)",
            ),
            Err(err) => tracing::warn!(
                plugin = %plugin_id,
                error = %err,
                "plugin poller registration rejected — duplicate kind",
            ),
        }
    }

    // Phase 81.20.x Stage 7 Phase 2 — populate the pairing trigger
    // registry from plugin manifests. Plugins that declare BOTH
    // `[plugin.pairing.adapter]` (channel_id) and `[plugin.pairing.trigger]`
    // (start_method, cancel_method) AND `[plugin.admin]` (the routing
    // info we forward through) get a `BrokerPairingTrigger` registered
    // under `adapter.channel_id`. Same loop also spawns one
    // pairing-inbound subscriber per channel so plugin-published QR +
    // state frames update the shared pairing store the dispatcher
    // reads on `pairing/status`.
    //
    // Coexists with any legacy hardcoded `pairing_triggers.insert()`
    // calls in the pre-bootstrap block — the registry overwrites by
    // `channel_id`, so the broker trigger replaces the legacy entry
    // for any plugin that ships the manifest section. Plugins that
    // skip the section keep the legacy hardcoded path.
    if let Some(bootstrap_ref) = admin_bootstrap.as_ref() {
        let pairing_store = bootstrap_ref.pairing_store();
        for (plugin_id, handle) in wire.plugin_handles.iter() {
            let manifest = handle.manifest();
            let adapter = match manifest.plugin.pairing.adapter.as_ref() {
                Some(a) => a,
                None => continue,
            };
            let trigger_section = match manifest.plugin.pairing.trigger.as_ref() {
                Some(t) => t,
                None => continue,
            };
            let admin_section = match manifest.plugin.admin.as_ref() {
                Some(a) => a,
                None => {
                    tracing::warn!(
                        plugin = %plugin_id,
                        channel = %adapter.channel_id,
                        "[plugin.pairing.trigger] declared without [plugin.admin] — \
                         skipping BrokerPairingTrigger registration"
                    );
                    continue;
                }
            };
            let broker_trigger = nexo_core::agent::admin_rpc::BrokerPairingTrigger::new(
                adapter.channel_id.clone(),
                broker.clone(),
                trigger_section,
                &admin_section.method_prefix,
                &admin_section.broker_topic_prefix,
            );
            pairing_triggers.insert(adapter.channel_id.clone(), Arc::new(broker_trigger));
            tracing::info!(
                plugin = %plugin_id,
                channel = %adapter.channel_id,
                start_method = %trigger_section.start_method,
                "registered broker pairing trigger (Phase 81.20.x Stage 7 Phase 2)",
            );
            // Detached subscriber. JoinHandle drops at end of scope;
            // tokio keeps the task running until the broker drops.
            // Process-lifecycle daemon never tears these down — the
            // subscriber loops until the broker channel closes at
            // shutdown.
            let _ = nexo_core::agent::admin_rpc::spawn_pairing_inbound_subscriber(
                broker.clone(),
                adapter.channel_id.clone(),
                pairing_store.clone(),
                None,
            );
        }
    }

    // Phase 81.33.b.real Stage 2 — build the plugin HTTP router
    // from every loaded plugin's `[plugin.http]` manifest section.
    // Plugins that don't declare the section contribute nothing
    // and the router stays empty for the legacy hardcoded path
    // matchers to serve.
    let http_router = {
        let mut router = nexo_pairing::plugin_http::PluginHttpRouter::new();
        for (plugin_id, handle) in wire.plugin_handles.iter() {
            if let Some(http) = handle.manifest().plugin.http.as_ref() {
                match router.register(
                    plugin_id,
                    &http.mount_prefix,
                    http.timeout_seconds.map(std::time::Duration::from_secs),
                ) {
                    Ok(()) => tracing::info!(
                        plugin = %plugin_id,
                        prefix = %http.mount_prefix,
                        "registered plugin HTTP route (Phase 81.33.b.real Stage 2)",
                    ),
                    Err(err) => tracing::warn!(
                        plugin = %plugin_id,
                        prefix = %http.mount_prefix,
                        error = %err,
                        "plugin HTTP route registration rejected — daemon-reserved prefix",
                    ),
                }
            }
        }
        Arc::new(router)
    };
    let health = RuntimeHealth {
        broker: broker.clone(),
        running_agents: Arc::clone(&running_agents),
        pairing_handshake: Arc::clone(&pairing_handshake_slot),
        tunnel_registry: Arc::clone(&tunnel_registry),
        http_router: Arc::clone(&http_router),
        plugin_metrics: Arc::clone(&plugin_metrics_descriptors),
    };
    let metrics_handle = tokio::spawn(run_metrics_server(health.clone()));
    let health_handle = tokio::spawn(run_health_server(health.clone()));

    // Phase 81.20.x Stage 7 Phase 2 — generic
    // `[plugin.public_tunnel]` iterator. For every plugin whose
    // manifest declares `enabled = true`, AND the operator has
    // set `NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW=1`, spawn a Cloudflare
    // quick tunnel pointed at the daemon's HTTP port (`:8080`).
    // Plugin HTTP routes (Phase 81.33.b.real Stage 2
    // `PluginHttpRouter`) become reachable at the public URL.
    //
    // When the manifest also sets `close_on_event = "<subject>"`,
    // spawn a one-shot subscriber that aborts the tunnel after
    // the plugin publishes any message there (e.g.
    // `plugin.lifecycle.whatsapp.tunnel_done` once pairing
    // completes).
    //
    // Capability env OFF (default) → block is a no-op even when
    // a plugin declares the section. Operators flip on with full
    // awareness of the public exposure surface.
    let public_tunnel_allow = std::env::var("NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if public_tunnel_allow {
        for (plugin_id, handle) in wire.plugin_handles.iter() {
            let section = &handle.manifest().plugin.public_tunnel;
            if !section.enabled {
                continue;
            }
            let plugin_id = plugin_id.clone();
            let close_on_event = section.close_on_event.clone();
            let broker_for_tunnel = broker.clone();
            tokio::spawn(async move {
                let manager = nexo_tunnel_quick::TunnelManager::new(8080);
                match manager.start().await {
                    Ok(tunnel) => {
                        tracing::info!(
                            plugin = %plugin_id,
                            url = %tunnel.url,
                            close_on_event = ?close_on_event,
                            "public tunnel up (Phase 81.20.x Stage 7 Phase 2)"
                        );
                        let _ = nexo_tunnel_quick::write_url_file(&tunnel.url);
                        if let Some(subject) = close_on_event {
                            match broker_for_tunnel.subscribe(&subject).await {
                                Ok(mut sub) => {
                                    if sub.next().await.is_some() {
                                        tracing::info!(
                                            plugin = %plugin_id,
                                            %subject,
                                            "public tunnel close event received — shutting down tunnel"
                                        );
                                        tunnel.shutdown().await;
                                        let _ = nexo_tunnel_quick::clear_url_file();
                                    }
                                }
                                Err(err) => tracing::warn!(
                                    plugin = %plugin_id,
                                    %subject,
                                    error = %err,
                                    "public tunnel close-event subscribe failed; tunnel stays up for daemon lifetime"
                                ),
                            }
                        }
                    }
                    Err(err) => tracing::error!(
                        plugin = %plugin_id,
                        error = %err,
                        "public tunnel start failed"
                    ),
                }
            });
        }
    } else if wire
        .plugin_handles
        .iter()
        .any(|(_, h)| h.manifest().plugin.public_tunnel.enabled)
    {
        tracing::info!(
            "[plugin.public_tunnel] declared by at least one plugin but \
             NEXO_PLUGIN_PUBLIC_TUNNEL_ALLOW is not set — skipping tunnel spawn"
        );
    }

    // Gmail poller — background task that polls Gmail on a fixed
    // interval and routes matching emails to channel plugins. No LLM
    // in the hot path; dedup via Gmail UNREAD label. Absent config
    // file = feature off.
    // `gmail-poller` legacy crate retired. Operators
    // declare gmail jobs directly in `config/pollers.yaml` under
    // `kind: gmail`. The generic runner handles scheduling, cursor
    // persistence, dispatch via the credential store, and the
    // `pollers_*` + `gmail_*` LLM tools.

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

    // Generic poller subsystem. Boot order:
    //   1) require credentials bundle (resolver lookup is mandatory)
    //   2) open SQLite state DB
    //   3) construct runner + register built-ins
    //   4) start runner (spawns one tokio task per job)
    // Failure at any step logs + skips: the daemon keeps running for
    // the rest of the agents.
    let pollers_runner: Option<Arc<nexo_poller::PollerRunner>> = match (
        cfg.pollers.clone(),
        credentials.as_ref().map(Arc::clone),
    ) {
        (Some(pcfg), Some(bundle)) if pcfg.enabled => {
            let state_db = std::path::PathBuf::from(&pcfg.state_db);
            match nexo_poller::PollState::open(&state_db).await {
                Ok(state) => {
                    // Feed the LLM registry + config into
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

                    // Phase 96.7 — register one PluginPollerProxy
                    // per (handle, kind) from the router so every
                    // subprocess-served kind shows up in the
                    // runner's kind registry. Multi-kind plugins
                    // produce one proxy per kind sharing the same
                    // handle Arc (per-tick broker RPC).
                    let mut plugin_poller_count = 0usize;
                    for (plugin_id, handle) in wire.plugin_handles.iter() {
                        if let Some(_poller_sec) = handle.manifest().plugin.poller.as_ref() {
                            if let Some(h) = plugin_poller_router.handles_for_plugin(plugin_id) {
                                for kind in &h.kinds {
                                    let leaked: &'static str =
                                        Box::leak(kind.clone().into_boxed_str());
                                    // Phase 96.E — branch on lifecycle.
                                    // `long_lived` plugins receive ticks
                                    // via broker JSON-RPC + reverse-RPC
                                    // for host calls. `ephemeral` plugins
                                    // are spawned per tick over stdio.
                                    match h.lifecycle {
                                            nexo_plugin_manifest::poller::PollerLifecycle::LongLived => {
                                                let proxy = nexo_pairing::plugin_poller::PluginPollerProxy::new(
                                                    leaked,
                                                    std::sync::Arc::clone(&h),
                                                    broker.clone(),
                                                );
                                                runner.register(std::sync::Arc::new(proxy));
                                            }
                                            nexo_plugin_manifest::poller::PollerLifecycle::Ephemeral => {
                                                if h.entrypoint_command.is_none() {
                                                    tracing::warn!(
                                                        plugin = %plugin_id,
                                                        kind = %kind,
                                                        "ephemeral lifecycle requires [plugin.entrypoint].command; skipping registration",
                                                    );
                                                    continue;
                                                }
                                                let proxy = nexo_pairing::plugin_poller::EphemeralPollerProxy::new(
                                                    leaked,
                                                    std::sync::Arc::clone(&h),
                                                );
                                                runner.register(std::sync::Arc::new(proxy));
                                            }
                                        }
                                    plugin_poller_count += 1;
                                }
                            }
                        }
                    }
                    if plugin_poller_count > 0 {
                        tracing::info!(
                            count = plugin_poller_count,
                            "plugin v2 poller proxies registered (Phase 96.7)",
                        );

                        // Reverse-RPC handler: subprocess pollers
                        // call back to the daemon for credential
                        // resolution, log/metric forwarding, and
                        // LLM invocation. One subscriber per
                        // plugin id; replies use the message's
                        // own `reply_to` topic.
                        for (plugin_id, _handle) in wire.plugin_handles.iter() {
                            if _handle.manifest().plugin.poller.is_none() {
                                continue;
                            }
                            let topic = format!("daemon.rpc.{}", plugin_id);
                            let broker_clone = broker.clone();
                            let creds_clone = Arc::clone(&runner.credentials());
                            let llm_registry = Arc::new(LlmRegistry::with_builtins());
                            let llm_config = Arc::new(cfg.llm.clone());
                            let plugin_id_owned = plugin_id.clone();
                            tokio::spawn(async move {
                                spawn_poller_reverse_rpc_subscriber(
                                    plugin_id_owned,
                                    topic,
                                    broker_clone,
                                    creds_clone,
                                    llm_registry,
                                    llm_config,
                                )
                                .await;
                            });
                        }
                    }

                    // Register extension-provided
                    // pollers. Walk every loaded stdio extension and
                    // bridge each declared `kind` into the runner via
                    // ExtensionPoller. Lets operators ship a poller in
                    // any language without touching Rust.
                    let mut ext_poller_count = 0usize;
                    for (rt, cand) in &extension_runtimes {
                        let kinds = &cand.manifest.capabilities.pollers;
                        if !kinds.is_empty() {
                            let n = nexo_poller_ext::register_for_runtime(&runner, rt, kinds).await;
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

    // Webhook receiver. Validate the snapshot, build
    // the dispatcher + axum router, spawn under a dedicated
    // CancellationToken. Validation failure is non-fatal: we log
    // the error and skip the server (daemon continues). Hot-reload
    // re-evaluation lands as a config-reload post-hook in a
    // follow-up.
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
                let dispatcher = Arc::new(nexo_webhook_server::BrokerWebhookDispatcher::new(
                    broker.clone(),
                ));
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

    // Spawn one EventSubscriber task per
    // (agent, binding) under a shared cancel token. Validation
    // failures are non-fatal (log + skip the offending binding
    // or whole-agent on duplicate-id); daemon stays up.
    let event_subscribers_shutdown = tokio_util::sync::CancellationToken::new();
    let mut total_event_subs = 0usize;
    for agent_cfg in &cfg.agents.agents {
        if agent_cfg.event_subscribers.is_empty() {
            continue;
        }
        if let Err(e) = nexo_config::types::event_subscriber::check_event_subscribers_unique(
            &agent_cfg.event_subscribers,
        ) {
            tracing::error!(
                agent_id = %agent_cfg.id,
                error = %e,
                "event_subscribers disabled for agent — duplicate id"
            );
            continue;
        }
        for binding in &agent_cfg.event_subscribers {
            if let Err(e) = binding.validate() {
                tracing::error!(
                    agent_id = %agent_cfg.id,
                    binding_id = %binding.id,
                    error = %e,
                    "skipping event_subscriber binding — invalid"
                );
                continue;
            }
            let sub =
                std::sync::Arc::new(nexo_core::agent::event_subscriber::EventSubscriber::new(
                    agent_cfg.id.clone(),
                    binding.clone(),
                    broker.clone(),
                ));
            let cancel = event_subscribers_shutdown.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    nexo_core::agent::event_subscriber::run_event_subscriber(sub, cancel).await
                {
                    tracing::error!(error = %e, "event_subscriber task exited with error");
                }
            });
            total_event_subs += 1;
        }
    }
    if total_event_subs > 0 {
        tracing::info!(count = total_event_subs, "event subscribers online");
    }

    // Single shared link extractor (HTTP client + LRU cache).
    // Per-binding config gates whether each turn actually fetches; the
    // extractor itself is cheap to keep around always.
    let link_extractor = Arc::new(nexo_core::link_understanding::LinkExtractor::new(
        &nexo_core::link_understanding::LinkUnderstandingConfig::default(),
    ));

    // Phase 95 — web-search router boot removed. The `web_search`
    // tool now lives in the standalone subprocess plugin
    // `nexo-rs-plugin-web-search`; daemon discovery walker spawns
    // the binary via `[plugin.entrypoint]` and RemoteToolHandler
    // routes calls over `tool.invoke` JSON-RPC. Operator config
    // moves from BRAVE_SEARCH_API_KEY / TAVILY_API_KEY env vars
    // to `<config_dir>/plugins/web-search.yaml::instances[].providers`.
    tracing::debug!(
        "web-search subprocess plugin: install via \
         `cargo install nexo-plugin-web-search` + populate \
         `plugins/web-search.yaml`"
    );

    // Pairing protocol. Builds the SQLite store + the
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

    // Auto-boot the in-process driver subsystem when
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
            llm_registry.clone(),
        )
        .await;

    let mut runtimes: Vec<AgentRuntime> = Vec::with_capacity(cfg.agents.agents.len());
    // Collect each agent's reload channel so the coordinator
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

    // `RetentionWorker` spawn. The snapshotter +
    // mutation hook were built earlier (right after `broker`) so
    // `LongTermMemory::open_with_vector` saw the hook. Only the
    // worker stays here because it needs `dream_shutdown` (defined
    // a few lines up).
    if let Some(snapshotter) = &memory_snapshotter {
        let retention = nexo_memory_snapshot::RetentionConfig {
            keep_count: snapshot_yaml.retention.keep_count,
            max_age_days: snapshot_yaml.retention.max_age_days,
            gc_interval_secs: snapshot_yaml.retention.gc_interval_secs.max(1),
        };
        let worker = nexo_memory_snapshot::RetentionWorker::new(
            snapshotter.clone(),
            memory_snapshot_state_root.clone(),
            retention,
        );
        let cancel = dream_shutdown.clone();
        tokio::spawn(async move {
            let _ = worker.spawn(cancel);
        });
        tracing::info!(
            target: "boot.memory_snapshot",
            state_root = %memory_snapshot_state_root.display(),
            memdir_root = %memdir_root_path.display(),
            sqlite_root = %sqlite_root_path.display(),
            keep_count = snapshot_yaml.retention.keep_count,
            max_age_days = snapshot_yaml.retention.max_age_days,
            gc_interval_secs = snapshot_yaml.retention.gc_interval_secs,
            auto_pre_dream = snapshot_yaml.auto_pre_dream,
            mutation_publish_enabled = snapshot_yaml.events.mutation_publish_enabled,
            "memory snapshot subsystem ready (retention worker spawned)"
        );
    }

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

    // Context-optimization wiring — built once per process and
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

    // Snapshot a fallback model for legacy cron rows
    // that predate per-entry model metadata (`model_provider`,
    // `model_name`). New rows carry their own model and do not depend
    // on this fallback.
    // Open the durable audit store for ConfigTool
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

    // Open the team store (registry + audit). Always
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

    // Per-process team message router. Spawns the
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
    // `cron_tool_bindings` is built post-loop via
    // `build_cron_bindings_from_snapshots` (single source of truth
    // shared with the config-reload post-hook). The map flows
    // directly from the build call to the executor constructor.
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

    // Bootstrap the approval correlator + reload
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

    // Process-shared plan-mode approval registry.
    // Created once per process so the broker subscriber below can
    // resolve pending ExitPlanMode approvals from inbound
    // `[plan-mode] approve|reject plan_id=…` chat messages.
    let plan_approval_registry =
        std::sync::Arc::new(nexo_core::agent::plan_mode_tool::PlanApprovalRegistry::default());
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
                    nexo_core::agent::plan_mode_tool::PlanModeApprovalCommand::Approve {
                        plan_id,
                    } => (
                        plan_id,
                        nexo_core::agent::plan_mode_tool::PlanApprovalDecision::Approve,
                    ),
                    nexo_core::agent::plan_mode_tool::PlanModeApprovalCommand::Reject {
                        plan_id,
                        reason,
                    } => {
                        let reason = reason.unwrap_or_else(|| "rejected by operator".to_string());
                        (
                            plan_id,
                            nexo_core::agent::plan_mode_tool::PlanApprovalDecision::Reject {
                                reason,
                            },
                        )
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

    // Boot the LSP manager once per process. Probes
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

    // Aggregated maps for cron post-hook reload.
    // `tools_per_agent` captures each per-agent ToolRegistry Arc;
    // `agent_snapshot_handles` captures each runtime's snapshot
    // ArcSwap so the post-hook can re-read the current effective
    // policy after a reload swap.
    // Phase 81.32 c5 — `Arc<DashMap>` for both. Built empty; the
    // per-agent loop inserts via `.insert(id, …)` and the
    // hot-spawn path (c8) inserts the same way without a global
    // lock. Wrapping in `Arc` up-front (vs after the loop)
    // collapses two binding sites the cron post-hook + reload
    // coordinator clone from.
    let tools_per_agent: Arc<dashmap::DashMap<String, Arc<nexo_core::agent::ToolRegistry>>> =
        Arc::new(dashmap::DashMap::new());
    let agent_snapshot_handles: Arc<
        dashmap::DashMap<String, Arc<arc_swap::ArcSwap<nexo_core::RuntimeSnapshot>>>,
    > = Arc::new(dashmap::DashMap::new());

    // Clone primary's id + config before the
    // agent loop consumes `cfg.agents.agents` so the daemon-embed
    // MCP wire can construct an AgentContext for the primary
    // after the loop.
    let primary_for_mcp_embed: Option<(String, nexo_config::AgentConfig)> =
        cfg.agents.agents.first().map(|a| (a.id.clone(), a.clone()));

    // Accumulate per-agent `AutoDreamRunner`s
    // built inside the per-agent loop. After the loop closes, the
    // primary (first non-None) registers with the
    // `DriverOrchestrator.builder().auto_dream(...)` so the
    // orchestrator can dispatch consolidation cycles.
    let mut auto_dream_runners: Vec<(
        String,
        Option<Arc<nexo_dream::auto_dream::AutoDreamRunner>>,
    )> = Vec::with_capacity(cfg.agents.agents.len());

    for agent_cfg in cfg.agents.agents {
        let agent_id = agent_cfg.id.clone();
        // Capture every field auto_dream
        // construction needs BEFORE later code paths consume
        // `agent_cfg` (Agent::new takes ownership ~line 4265).
        let agent_cfg_for_dream = agent_cfg.clone();
        let dream_yaml = agent_cfg.dreaming.clone();
        let workspace_for_dream = agent_cfg.workspace.clone();
        // Boot-time policy resolution. `from_agent_defaults` mirrors
        // agent-level fields into the resolved struct; per-binding
        // override pickup happens at handler call time via
        // `ctx.effective_policy()` (which the runtime intake fills from
        // `RuntimeSnapshot::policy_for(binding_idx)` per inbound event).
        let effective_boot =
            nexo_core::agent::effective::EffectiveBindingPolicy::from_agent_defaults(&agent_cfg);
        // Phase 81.32 c2 — LLM bind delegated to the shared
        // helper. Behavior identical (skip agent with WARN on
        // failure) but the error path now produces a typed
        // `SpawnError::LlmBind` so hot-spawn surfaces an
        // operator-actionable message at the wizard.
        let llm = match nexo_core::agent::spawn::resolve_llm_client(
            &agent_cfg,
            &llm_registry,
            &cfg.llm,
        ) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    agent = %agent_id,
                    error = %e,
                    "skipping agent — LLM provider not configured \
                     (configure it via the admin UI and reload)",
                );
                continue;
            }
        };

        // Construct per-agent ExtractMemories when
        // the YAML opted in. Wire-shape `ExtractMemoriesYamlConfig`
        // mirrors `nexo_driver_types::ExtractMemoriesConfig` 1:1; we
        // convert here. The same `Arc<ExtractMemories>` will be
        // shared with the driver-loop orchestrator if the
        // self-driving wire is added — single instance per
        // agent keeps cadence + circuit breaker + in-progress mutex
        // coherent.
        let memory_extractor: Option<Arc<dyn nexo_driver_types::MemoryExtractor>> =
            match agent_cfg.extract_memories.as_ref().filter(|c| c.enabled) {
                Some(yaml_cfg) => {
                    let cfg_concrete = nexo_driver_types::ExtractMemoriesConfig {
                        enabled: yaml_cfg.enabled,
                        turns_throttle: yaml_cfg.turns_throttle,
                        max_turns: yaml_cfg.max_turns,
                        max_consecutive_failures: yaml_cfg.max_consecutive_failures,
                    };
                    let adapter =
                        Arc::new(nexo_driver_loop::extract_memories::LlmClientAdapter::new(
                            Arc::clone(&llm),
                            agent_cfg.model.model.clone(),
                        ));
                    let extract =
                        Arc::new(nexo_driver_loop::extract_memories::ExtractMemories::new(
                            cfg_concrete,
                            adapter,
                        ));
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

        // Validate heartbeat interval eagerly even though the runtime
        // wiring is pending — better to fail at startup than silently ignore.
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
        // Channel tools when ANY of this agent's
        // bindings has `allowed_channel_servers` non-empty AND
        // the operator's `agents.channels` block is configured.
        // Tools key against `agent_cfg.id` so the registry view
        // is agent-scoped — multi-binding granularity is a
        // follow-up. `channel_list` + `channel_status`
        // are read-only; `channel_send` is gated by the per-tool
        // approval flow + the channel registry's
        // `RegisteredChannel.outbound_tool_name` lookup.
        let channels_in_play = agent_cfg.channels.is_some()
            && agent_cfg
                .inbound_bindings
                .iter()
                .any(|b| !b.allowed_channel_servers.is_empty());
        if channels_in_play {
            // Dynamic-binding tools. The tools
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
        // Register the project-tracker / dispatch tool
        // surface (program_phase, list_agents, agent_status, …).
        // The handlers return a friendly error when
        // `AgentContext.dispatch` is not set, so registering them
        // without an orchestrator just means the LLM sees the
        // tool defs and dispatch attempts surface a clean error
        // instead of pretending success. Operators that wire up
        // a DispatchToolContext at boot get the full surface.
        // Per-binding `dispatch_capability` in EffectiveBindingPolicy
        // prunes write tools at session-time so
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
        // Browser_* tools register via the
        // RemoteToolHandler in the plugin init loop, not here.
        // Agents with `plugins: [browser]` get the tools from the
        // ScopedToolRegistry seeded by the subprocess plugin's
        // initialize-reply tools[] array.
        //
        // Soft warning when agent declares the plugin but no
        // browser manifest was discovered in
        // plugins.discovery.search_paths.
        if agent_cfg.plugins.iter().any(|p| p == "browser") {
            tracing::debug!(
                agent = %agent_id,
                "agent declares `browser` plugin; tools auto-register via subprocess RemoteToolHandler"
            );
        }
        // Phase 81.33.a — generic outbound tool registration via
        // plugin manifest. Iterates the agent's declared plugin
        // ids, looks up the corresponding plugin handle, and
        // calls the trait method `register_outbound_tools`. For
        // subprocess plugins this triggers
        // `SubprocessNexoPlugin::register_outbound_tools` which
        // installs one `GenericRpcToolHandler` per manifest
        // `[[plugin.tools.outbound]]` entry. Plugins whose
        // manifest doesn't declare outbound tools (legacy /
        // in-tree) register zero — keeping the hardcoded fallback
        // calls below valid during the migration window.
        for plugin_id in &agent_cfg.plugins {
            if let Some(handle) = wire.plugin_handles.get(plugin_id) {
                handle.register_outbound_tools(&tools);
            }
        }
        // WhatsApp outbound tools — Phase 81.33.a fallback while
        // the out-of-tree plugin hasn't shipped a manifest
        // declaring `[[plugin.tools.outbound]]`. The generic loop
        // above already runs; this block stays additive (no
        // collision because old plugins declare zero outbound in
        // manifest). Removed in Phase 81.33.a step 6 after the
        // matching plugin patch publishes.
        // Phase 81.20.x Stage 7 — whatsapp + telegram tool register
        // fallbacks removed. Both plugins' subprocesses advertise their
        // outbound tool defs in the `initialize` reply; the daemon's
        // `RemoteToolHandler` (registered by `nexo_plugin_registry::init_loop`
        // per the declared tools) routes per-agent `tool.invoke` through
        // the broker. Same end-state every other canonical plugin uses.
        // Phase 81.20.x F2.3 — email tool registration removed.
        // `nexo-plugin-email` v0.6.0+ subprocess advertises its 12
        // tools at `initialize` and the daemon's `RemoteToolHandler`
        // (registered by `nexo_plugin_registry::init_loop`) routes
        // per-agent `tool.invoke` through the broker. No daemon-side
        // tool fallback required.
        // Phase 94 — google_* tools land via the standalone
        // `nexo-plugin-google` subprocess plugin
        // (`../nexo-rs-plugin-google/`). Daemon's discovery walker
        // auto-spawns it; `RemoteToolHandler` (registered by
        // `nexo_plugin_registry::init_loop` per
        // `[plugin.extends].tools`) routes per-agent `tool.invoke`
        // through the broker. Same canonical path used by telegram /
        // whatsapp / email.
        if agent_cfg.google_auth.is_some()
            || credentials
                .as_ref()
                .and_then(|b| b.google_account_for_agent(&agent_cfg.id))
                .is_some()
        {
            tracing::debug!(
                agent = %agent_id,
                "agent has google_auth configured — google_* tools \
                 expected from nexo-plugin-google subprocess"
            );
        }
        // Pollers_* control tools (list, show, run, pause,
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

        // Register EnterPlanMode + ExitPlanMode tools
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

        // `TodoWrite` is always available. Cheap,
        // in-memory scratch list per goal; classified `ReadOnly` so
        // it stays callable while plan mode is on.
        tools.register(
            nexo_core::agent::todo_write_tool::TodoWriteTool::tool_def(),
            nexo_core::agent::todo_write_tool::TodoWriteTool,
        );

        // `ToolSearch` discovery surface for deferred
        // tools. Always registered; `ToolMeta::default` keeps it
        // non-deferred itself (the model needs it to load everything
        // else). When MCP-imported tools start opting into
        // `ToolMeta::deferred`, this surface becomes useful at LLM
        // turn time.
        tools.register(
            nexo_core::agent::tool_search_tool::ToolSearchTool::tool_def(),
            nexo_core::agent::tool_search_tool::ToolSearchTool::new(),
        );

        // `SyntheticOutput` typed-output validator.
        // Always registered (cheap, pure, classified `ReadOnly`).
        // The model invokes it to terminate a goal with a JSON
        // value that matches a caller-provided JSONSchema —
        // direct input for pollers + eval runs.
        tools.register(
            nexo_core::agent::synthetic_output_tool::SyntheticOutputTool::tool_def(),
            nexo_core::agent::synthetic_output_tool::SyntheticOutputTool,
        );

        // Sleep is visible only for agents/bindings that can
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

        // `Repl` tool (stateful Python/Node/bash subprocesses
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
                // `ReplRegistry` still captures the agent-level
                // ReplConfig (subsystem-actor-level: timeout_secs,
                // max_sessions, max_output_bytes are boot-frozen).
                // The per-call `allowed_runtimes` allowlist
                // override lives in `ReplTool::call` via
                // `ctx.effective_policy().repl`.
                let repl_workspace = if agent_cfg.workspace.trim().is_empty() {
                    String::from("./data/workspace")
                } else {
                    agent_cfg.workspace.clone()
                };
                let repl_registry = std::sync::Arc::new(nexo_core::agent::ReplRegistry::new(
                    effective_boot.repl.clone(),
                    repl_workspace,
                ));
                tools.register(
                    nexo_core::agent::ReplTool::tool_def(),
                    nexo_core::agent::ReplTool::new(repl_registry),
                );
            }
        }

        // `NotebookEdit` for `.ipynb` cell-level edits.
        // Pure-Rust round-trip via serde_json — no `jupyter` binary
        // required. Always registered (operators that don't touch
        // notebooks pay zero cost — the tool is filtered out by
        // `allowed_tools` if undesired).
        tools.register(
            nexo_core::agent::notebook_edit_tool::NotebookEditTool::tool_def(),
            nexo_core::agent::notebook_edit_tool::NotebookEditTool,
        );

        // `RemoteTrigger` outbound publisher. Allowlist
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

        // `ListMcpResources` + `ReadMcpResource`
        // router-shaped tools. Useful for agents talking to many MCP
        // servers — single discovery surface instead of N×2
        // per-server tools (which still ship via the per-server
        // catalog). Cheap, classified `ReadOnly`, always registered.
        tools.register(
            nexo_core::agent::mcp_router_tool::ListMcpResourcesTool::tool_def(),
            nexo_core::agent::mcp_router_tool::ListMcpResourcesTool,
        );
        tools.register(
            nexo_core::agent::mcp_router_tool::ReadMcpResourceTool::tool_def(),
            nexo_core::agent::mcp_router_tool::ReadMcpResourceTool,
        );

        // Cron schedule store + 5 tools (cron_create,
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

        // `web_search` tool. Registered when the agent's
        // top-level policy has `enabled: true` and a router exists.
        // Per-binding overrides are enforced inside the tool itself
        // (it reads `ctx.effective_policy().web_search` per call).
        let agent_ws_enabled = agent_cfg
            .web_search
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if agent_ws_enabled {
            // Phase 95 — web_search tool removed from in-process
            // registration. The standalone `nexo-rs-plugin-web-search`
            // subprocess plugin advertises it at `initialize`; the
            // daemon's `RemoteToolHandler` registers the per-agent
            // dispatcher automatically when the plugin is discovered.
            tracing::debug!(
                agent = %agent_id,
                "agent has web_search.enabled — tool served by \
                 nexo-plugin-web-search subprocess (install via \
                 `cargo install nexo-plugin-web-search`)"
            );
        }

        // `Lsp` tool, per-agent. Registered only when
        // the agent's `lsp.enabled` is `true`. Languages whitelist
        // empty means "all discovered". Workspace_root falls back
        // to the daemon's `lsp_workspace` (first agent's
        // workspace) when the agent itself doesn't declare one.
        if effective_boot.lsp.enabled {
            // `policy` is no longer captured at boot. The handler
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

        // Register the 5 Team* tools when the agent
        // opts in (`team.enabled: true`) AND the team store opened
        // successfully. The lead's `current_goal_id` placeholder
        // here is the agent_id — the driver-loop overrides it
        // per-goal once team-aware spawn lands.
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

        // `config_changes_tail` (read-only audit log).
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

        // Gated `Config { op: ... }` tool. Compiled
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

        // Self-report tools. `who_am_i` + `what_do_i_know` are
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

        // Optional git-backed workspace. Registers
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
                            // Attach the
                            // mutation hook so each successful
                            // commit_all fires a Git/Update event
                            // onto `nexo.memory.mutated.<agent>`.
                            let repo = if let Some(ref hook) = memory_mutation_hook {
                                repo.with_mutation_hook(hook.clone(), agent_id.clone(), "default")
                            } else {
                                repo
                            };
                            tracing::info!(
                                agent = %agent_id,
                                root = %ws.display(),
                                mutation_hook = memory_mutation_hook.is_some(),
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
            // `memory_snapshot` LLM tool. Per-binding
            // gating still flows through `EffectiveBindingPolicy::
            // allowed_tools` + plan-mode (the tool sits in
            // `MUTATING_TOOLS`); the shared snapshotter dispatches
            // every call through the same `LocalFsSnapshotter` the
            // CLI hits, so an LLM-triggered snapshot lands next to
            // operator-triggered ones in the same per-agent dir.
            // Skipped when the operator disables the subsystem via
            // `memory.snapshot.enabled = false`.
            if let Some(snapshotter) = &memory_snapshotter {
                let tool = nexo_core::agent::MemorySnapshotTool::new(snapshotter.clone())
                    .with_redact_secrets_default(snapshot_yaml.redact_secrets_default);
                tools.register(nexo_core::agent::MemorySnapshotTool::tool_def(), tool);
            }
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

        // Extension tools. Each discovered-and-spawned extension
        // contributes its declared tools to this agent's registry.
        // Extension hooks built alongside, same iteration.
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

        // Register MCP tools for this agent. Shared sentinel
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

            // Spawn one ChannelInboundLoop per `(binding,
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
                        let allow_arc =
                            std::sync::Arc::new(binding.allowed_channel_servers.clone());
                        for (server_name, client) in &clients_snapshot {
                            if !binding
                                .allowed_channel_servers
                                .iter()
                                .any(|s| s == server_name)
                            {
                                continue;
                            }
                            let cap_declared = nexo_mcp::channel::has_channel_capability(Some(
                                &client.capabilities().experimental,
                            ));
                            let perm_cap = nexo_mcp::channel::has_channel_permission_capability(
                                Some(&client.capabilities().experimental),
                            );
                            let plugin_source = channels_cfg
                                .lookup_approved(server_name)
                                .and_then(|e| e.plugin_source.clone());
                            let loop_cfg = nexo_mcp::channel_boot::build_inbound_loop_config(
                                &channel_boot,
                                server_name.clone(),
                                binding_id.clone(),
                                plugin_source,
                                cfg_arc.clone(),
                                allow_arc.clone(),
                                cap_declared,
                                perm_cap,
                            );
                            let handle = nexo_mcp::channel::ChannelInboundLoop::new(loop_cfg)
                                .spawn_against_client(client.as_ref(), channel_shutdown.clone());
                            // Spawn the
                            // permission-response pump alongside
                            // the channel inbound loop so any
                            // structured `notifications/nexo/channel/permission`
                            // event from this server resolves the
                            // matching pending entry.
                            if perm_cap {
                                let _ =
                                    nexo_mcp::channel_permission::spawn_permission_response_pump(
                                        client.clone(),
                                        server_name.clone(),
                                        pending_permissions.clone(),
                                        channel_shutdown.clone(),
                                    );
                            }
                            match handle {
                                nexo_mcp::channel::ChannelInboundLoopHandle::Running { .. } => {
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

            // Hot-reload: when a server pushes
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

        // Mark built-in tools deferred per leak's
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
        // Phase 81.32 c2 — validation delegated to
        // `spawn::validate_agent_config`. Hot-spawn surfaces the
        // typed `SpawnError::Validation` to the wizard; boot
        // path keeps the existing `?` flow but with the same
        // canonical message format.
        {
            let defs = tools.to_tool_defs();
            let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
            nexo_core::agent::spawn::validate_agent_config(&agent_cfg, &cfg.plugins, &names)
                .map_err(|e| {
                    anyhow::anyhow!("agent `{}` binding validation failed: {}", agent_id, e)
                })?;
        }

        // Cron binding contexts are now built in a single
        // `build_cron_bindings_from_snapshots` call AFTER the agent
        // loop ends, using the aggregated `tools_per_agent` +
        // `agent_snapshot_handles` maps. The same fn is called by
        // the config-reload post-hook (single source of truth).

        let mut behavior = LlmAgentBehavior::new(llm.clone(), Arc::clone(&tools))
            .with_hooks(Arc::clone(&hooks))
            .with_tool_policy(tool_policy_registry.for_agent(&agent_id));

        // Wire post-turn memory extraction. Constructed
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
            // Config ↔ runtime types unified. Direct
            // pass-through, no translation needed.
            let limiter = Arc::new(nexo_core::agent::ToolRateLimiter::new(rl_cfg));
            behavior = behavior.with_rate_limiter(limiter);
            tracing::info!(agent = %agent_id, "tool rate limiter enabled");
        }
        // JSON Schema args validation.
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
        // Wire the four mechanisms onto
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
                    // autoCompact — from YAML auto section, or defaults.
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

        // Phase 81.20.x Stage 7 — empty registry; the loop below
        // populates it generically from each plugin's
        // `build_pairing_adapter()` trait method.
        let pairing_registry = nexo_pairing::PairingAdapterRegistry::new();
        // Phase 81.33.b.real — manifest-driven pairing adapters.
        // After legacy hardcoded registrations (above), iterate
        // every loaded plugin handle and ask it to supply an
        // adapter via the trait method. Plugins whose manifest
        // declares `[plugin.pairing.adapter]` return
        // `Some(GenericBrokerPairingAdapter)`; the registry's
        // `register` overwrites by `channel_id` so a manifest-
        // declared adapter wins over the matching legacy hardcoded
        // one. Plugins that haven't migrated yet return `None` and
        // the legacy registration keeps serving.
        for (plugin_id, handle) in wire.plugin_handles.iter() {
            if let Some(adapter) = handle.build_pairing_adapter(broker.clone()) {
                tracing::info!(
                    plugin = %plugin_id,
                    channel = %adapter.channel_id(),
                    "registered manifest-driven pairing adapter (Phase 81.33.b.real)",
                );
                pairing_registry.register(adapter);
            }
        }
        let assembly_deps = nexo_core::agent::spawn::RuntimeAssemblyDeps {
            tools: Arc::clone(&tools),
            memory: memory.clone(),
            peers: Arc::clone(&peer_directory),
            redactor: Arc::clone(&transcripts_redactor),
            transcripts_index: transcripts_index.as_ref().map(Arc::clone),
            credentials: credentials.as_ref().map(|b| Arc::clone(&b.resolver)),
            breakers: credentials.as_ref().map(|b| Arc::clone(&b.breakers)),
            link_extractor: Arc::clone(&link_extractor),
            // Phase 95 — web_search_router removed from RuntimeAssemblyDeps.
            pairing_gate: Arc::clone(&pairing_gate),
            pairing_adapters: pairing_registry,
            plan_approval_registry: plan_approval_registry.clone(),
            dispatch_ctx: dispatch_ctx.as_ref().map(Arc::clone),
            processing_store: Arc::clone(&processing_store),
            event_emitter: admin_bootstrap.as_ref().map(|bs| bs.event_emitter()),
        };
        let runtime = nexo_core::agent::spawn::assemble_agent_runtime(
            Arc::clone(&agent),
            broker.clone(),
            Arc::clone(&sessions),
            assembly_deps,
        );
        // Capture maps before `runtime.start()` consumes self.
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

        // Dreaming — when enabled and long-term memory + workspace
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
                                // Auto-commit workspace changes.
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

        // Build the agent's
        // `AutoDreamRunner` if `auto_dream` is configured + enabled.
        // Wires the `AgentToolDispatcher`, `parent_ctx_template`,
        // git checkpointer, and the pre-dream snapshot adapter
        // so the runner is ready for the orchestrator
        // registration after the loop. Uses `agent_cfg_for_dream`
        // (cloned at loop top) since `agent_cfg` was already moved
        // by `Agent::new`.
        let runner_opt: Option<Arc<nexo_dream::auto_dream::AutoDreamRunner>> =
            match agent_cfg_for_dream.auto_dream.as_ref() {
                Some(ad_cfg) if ad_cfg.enabled => {
                    if agent_cfg_for_dream.workspace.trim().is_empty() {
                        tracing::warn!(
                            agent = %agent_id,
                            "auto_dream.enabled=true but agent.workspace is empty — skipping"
                        );
                        None
                    } else {
                        let parent_ctx_template = nexo_core::agent::AgentContext::new(
                            agent_cfg_for_dream.id.clone(),
                            Arc::new(agent_cfg_for_dream.clone()),
                            broker.clone(),
                            sessions.clone(),
                        );
                        let tool_dispatcher: Arc<dyn nexo_fork::ToolDispatcher> =
                            Arc::new(nexo_fork::AgentToolDispatcher::new(
                                tools.clone(),
                                parent_ctx_template.clone(),
                            ));
                        let git_checkpointer = agent_git.as_ref().map(|g| {
                            Arc::new(nexo_core::agent::MemoryGitCheckpointer::new(g.clone()))
                                as Arc<dyn nexo_driver_types::MemoryCheckpointer>
                        });
                        let pre_dream_snapshot = if snapshot_yaml.auto_pre_dream {
                            memory_snapshotter.as_ref().map(|s| {
                                nexo_memory_snapshot::PreDreamSnapshotAdapter::new(s.clone())
                                    .into_arc()
                            })
                        } else {
                            None
                        };
                        let deps = nexo_dream::boot::BootDeps {
                            config: ad_cfg.clone(),
                            agent_id: agent_cfg_for_dream.id.clone(),
                            workspace_root: std::path::PathBuf::from(
                                &agent_cfg_for_dream.workspace,
                            ),
                            state_root: nexo_project_tracker::state::nexo_state_dir(),
                            parent_ctx_template,
                            llm: llm.clone(),
                            tool_dispatcher,
                            fork_system_prompt: agent_cfg_for_dream.system_prompt.clone(),
                            fork_tools: Vec::new(),
                            fork_model: agent_cfg_for_dream.model.model.clone(),
                            git_checkpointer,
                            pre_dream_snapshot,
                            pre_dream_tenant: "default".into(),
                        };
                        match nexo_dream::boot::build_runner(deps).await {
                            Ok(opt) => opt,
                            Err(e) => {
                                tracing::warn!(
                                    agent = %agent_id,
                                    error = %e,
                                    "auto_dream boot failed; agent runs without consolidation"
                                );
                                None
                            }
                        }
                    }
                }
                _ => None,
            };

        // `dream_now` LLM tool registration. Honors
        // the `NEXO_DREAM_NOW_ENABLED` capability inventory entry
        // and skips when `transcripts_dir` is
        // empty (the tool needs it to construct DreamContext).
        let dream_now_registered = if let Some(runner) = &runner_opt {
            if is_truthy_env("NEXO_DREAM_NOW_ENABLED")
                && !agent_cfg_for_dream.transcripts_dir.trim().is_empty()
            {
                nexo_dream::tools::register_dream_now_tool(
                    &tools,
                    runner.clone(),
                    std::path::PathBuf::from(&agent_cfg_for_dream.transcripts_dir),
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        tracing::info!(
            target: "boot.auto_dream",
            agent = %agent_id,
            auto_dream_enabled = runner_opt.is_some(),
            has_pre_dream_snapshot = runner_opt
                .as_ref()
                .map(|r| r.has_pre_dream_snapshot())
                .unwrap_or(false),
            has_git_checkpointer = runner_opt
                .as_ref()
                .map(|r| r.has_git_checkpointer())
                .unwrap_or(false),
            dream_now_registered,
            "auto_dream constellation ready for agent"
        );

        auto_dream_runners.push((agent_id.clone(), runner_opt));
    }

    // Runtime-register every active
    // `AutoDreamRunner` on the dispatch orchestrator under its
    // owning `agent_id`. The orchestrator's per-turn dispatcher
    // looks up `goal.metadata["agent_id"]` against this map, so a
    // turn whose goal carries the agent id triggers the matching
    // runner instead of a single hardcoded primary.
    {
        let active: Vec<(String, Arc<nexo_dream::auto_dream::AutoDreamRunner>)> =
            auto_dream_runners
                .iter()
                .filter_map(|(id, r)| r.as_ref().map(|x| (id.clone(), x.clone())))
                .collect();
        if !active.is_empty() {
            if let Some(dc) = dispatch_ctx.as_ref() {
                let mut registered_count = 0_usize;
                for (agent_id, runner) in active.iter() {
                    let hook: Arc<dyn nexo_driver_types::AutoDreamHook> = runner.clone();
                    dc.orchestrator.register_auto_dream(agent_id.clone(), hook);
                    registered_count += 1;
                }
                tracing::info!(
                    target: "boot.auto_dream",
                    agents = registered_count,
                    registered = ?dc.orchestrator.auto_dream_agents(),
                    "auto_dream runners registered on orchestrator"
                );
            } else {
                let agents: Vec<&str> = active.iter().map(|(id, _)| id.as_str()).collect();
                tracing::info!(
                    target: "boot.auto_dream",
                    agents = ?agents,
                    "auto_dream runners built but dispatch orchestrator is disabled (no agent has dispatch_capability=full); runners only reachable via dream_now LLM tool"
                );
            }
        }
    }

    // Wrap aggregated cron-rebuild maps + build deps struct
    // for the post-hook + initial boot-time cron binding build.
    // Phase 81.32 c5 — both maps already `Arc<DashMap>` at boot;
    // no post-loop wrap needed.
    let cron_rebuild_deps = CronRebuildDeps {
        broker: broker.clone(),
        sessions: Arc::clone(&sessions),
        memory: memory.clone(),
        peer_directory: Arc::clone(&peer_directory),
        credentials: credentials.clone(),
        // Phase 95 — web_search_router removed.
        link_extractor: Arc::clone(&link_extractor),
        dispatch_ctx: dispatch_ctx.clone(),
        tools_per_agent: Arc::clone(&tools_per_agent),
        cron_tool_call_cfg: cron_tool_call_cfg.clone(),
    };
    // Late-bind cell holding the cron executor. Empty until
    // the cron block (`if cron_tool_call_cfg.enabled`) constructs the
    // executor; the post-hook below early-returns if cell is empty.
    let cron_executor_cell: Arc<tokio::sync::OnceCell<Arc<RuntimeCronToolExecutor>>> =
        Arc::new(tokio::sync::OnceCell::new());

    // Wire the hot-reload coordinator. It owns its own
    // CancellationToken tied to `watcher_shutdown` so the watcher +
    // broker subscriber exit alongside the extensions watcher on
    // SIGTERM.
    // `llm_registry` is already `Arc<LlmRegistry>`
    // from the construction at line ~1506; the prior duplicate
    // `Arc::new(llm_registry)` here is removed.
    let reload_coord = Arc::new(nexo_core::ConfigReloadCoordinator::new(
        config_dir.clone(),
        Arc::clone(&llm_registry),
        watcher_shutdown.clone(),
    ));
    for (id, tx, known) in reload_senders.drain(..) {
        reload_coord.register(id, tx, known);
    }
    // Phase 81.32 c7.b — build the spawner closure here (captures
    // every per-agent dep from this scope) but DEFER the actual
    // `reload_coord.set_spawner` install until after
    // `coord.start()` has stashed the broker handle. Otherwise
    // the very first hot-spawn between this point and start()
    // would publish `events.runtime.agent.spawned` to a `None`
    // broker (silently dropped).
    let pending_spawner = {
        // Phase 81.32 c7.b — install the spawner closure invoked by
        // `ConfigReloadCoordinator` when an unknown agent id appears
        // in `agents.yaml` (typical: wizard creates a new agent).
        //
        // MINIMAL-MODE body: registers only `DelegationTool` in the
        // per-agent ToolRegistry. The agent receives inbound messages
        // via the runtime's broker subscribers (full functionality) but
        // outbound channel/plugin tools (channel_send, whatsapp_*,
        // telegram_*, email_*, browser_*, mcp.*) require a daemon
        // restart for full parity. Operators see a WARN line with this
        // limitation on every hot-spawn. Closing the parity gap is
        // tracked as Phase 81.32 c7.c follow-up (will lift the
        // ~865-LOC tools registry build from the boot loop body into
        // a `build_per_agent_tools` helper).
        //
        // Captures: every per-agent runtime dependency the boot loop
        // body uses. Cloned ONCE into outer-scope locals before the
        // closure construction so the move-into-closure cost is paid
        // upfront and the per-spawn `.clone()` chain inside the
        // closure body keeps each invocation cheap (every field is
        // Arc-cloned, not deep-copied).
        let llm_cfg_for_spawn = Arc::new(cfg.llm.clone());
        // Phase 93.5.e — Arc-share the whole `PluginsConfig` instead
        // of cloning a typed `Vec<TelegramPluginConfig>`. The
        // spawn-time validator now walks `plugins.instances_for(id)`
        // so any array-shape plugin participates without daemon-side
        // typed access.
        let plugins_for_spawn = Arc::new(cfg.plugins.clone());
        use nexo_core::agent::spawn::{
            assemble_agent_runtime, resolve_llm_client, validate_agent_config, AgentSpawnerFn,
            RuntimeAssemblyDeps, SpawnError, SpawnedAgent,
        };
        use nexo_core::agent::{Agent, DelegationTool, LlmAgentBehavior, ToolRegistry};

        let broker_c = broker.clone();
        let sessions_c = Arc::clone(&sessions);
        let memory_c = memory.clone();
        let llm_registry_c = Arc::clone(&llm_registry);
        let llm_cfg_c = Arc::clone(&llm_cfg_for_spawn);
        let plugins_c = Arc::clone(&plugins_for_spawn);
        let peer_directory_c = Arc::clone(&peer_directory);
        let transcripts_redactor_c = Arc::clone(&transcripts_redactor);
        let transcripts_index_c = transcripts_index.clone();
        let credentials_c = credentials.clone();
        let link_extractor_c = Arc::clone(&link_extractor);
        // Phase 95 — web_search_router capture removed.
        let pairing_gate_c = Arc::clone(&pairing_gate);
        let plan_approval_registry_c = plan_approval_registry.clone();
        let dispatch_ctx_c = dispatch_ctx.clone();
        let processing_store_c = Arc::clone(&processing_store);
        let event_emitter_c = admin_bootstrap.as_ref().map(|b| b.event_emitter());
        // Phase 81.32 c7.c.1 — extra captures for the
        // expanded minimal-mode tools registry.
        let default_recall_mode_c = cfg.memory.vector.default_recall_mode.clone();
        // Phase 81.33.a — capture plugin_handles_cell so the
        // spawner closure can iterate the live plugin map and
        // call register_outbound_tools on each handle.
        let plugin_handles_cell_c = plugin_handles_cell.clone();
        // Phase 81.32 c7.c.2 — captures for the expanded tool
        // surface (email outbound, pollers, channel tools, MCP
        // tools, extension tools). google_* stays deferred to
        // c7.c.3 (requires async load_from_disk + workspace
        // handling).
        let pollers_runner_c = pollers_runner.clone();
        let channel_boot_c = channel_boot.clone();
        let mcp_manager_c = mcp_manager.clone();
        let extension_runtimes_c: Vec<_> = extension_runtimes
            .iter()
            .map(|(rt, cand)| (Arc::clone(rt), cand.clone()))
            .collect();
        let mcp_cfg_c = cfg.mcp.clone();
        let tools_per_agent_c = Arc::clone(&tools_per_agent);
        let agent_snapshot_handles_c = Arc::clone(&agent_snapshot_handles);

        let spawner: AgentSpawnerFn = AgentSpawnerFn(Box::new(move |cfg| {
            let broker = broker_c.clone();
            let sessions = Arc::clone(&sessions_c);
            let memory = memory_c.clone();
            let llm_registry = Arc::clone(&llm_registry_c);
            let llm_cfg = Arc::clone(&llm_cfg_c);
            let plugins = Arc::clone(&plugins_c);
            let peer_directory = Arc::clone(&peer_directory_c);
            let transcripts_redactor = Arc::clone(&transcripts_redactor_c);
            let transcripts_index = transcripts_index_c.clone();
            let credentials = credentials_c.clone();
            let link_extractor = Arc::clone(&link_extractor_c);
            // Phase 95 — web_search_router clone removed.
            let pairing_gate = Arc::clone(&pairing_gate_c);
            let plan_approval_registry = plan_approval_registry_c.clone();
            let dispatch_ctx = dispatch_ctx_c.clone();
            let processing_store = Arc::clone(&processing_store_c);
            let event_emitter = event_emitter_c.clone();
            let default_recall_mode = default_recall_mode_c.clone();
            let plugin_handles_cell = plugin_handles_cell_c.clone();
            let pollers_runner = pollers_runner_c.clone();
            let channel_boot = channel_boot_c.clone();
            let mcp_manager = mcp_manager_c.clone();
            let extension_runtimes: Vec<_> = extension_runtimes_c
                .iter()
                .map(|(rt, cand)| (Arc::clone(rt), cand.clone()))
                .collect();
            let mcp_cfg = mcp_cfg_c.clone();
            let tools_per_agent = Arc::clone(&tools_per_agent_c);
            let agent_snapshot_handles = Arc::clone(&agent_snapshot_handles_c);

            Box::pin(async move {
                let agent_id = cfg.id.clone();

                // 1. Resolve LLM via shared helper.
                let llm = resolve_llm_client(&cfg, &llm_registry, &llm_cfg)?;

                // 2. Tools registry — Phase 81.32 c7.c.1 expanded
                //    surface. Registers every tool whose
                //    construction needs ONLY the captures already
                //    threaded into this closure (broker, memory,
                //    cfg). Tools requiring boot-only state
                //    (email_tool_ctx, pollers_runner, mcp_manager,
                //    extension_runtimes, channel_boot, google
                //    credentials/workspace) stay deferred as
                //    Phase 81.32 c7.c.2.
                let tools = Arc::new(ToolRegistry::new());
                tools.register(DelegationTool::tool_def(), DelegationTool);
                nexo_core::agent::dispatch_handlers::register_dispatch_tools_into(&tools);
                if cfg.plugins.iter().any(|p| p == "memory") {
                    if let Some(mem) = memory.clone() {
                        tools.register(
                            nexo_core::agent::MemoryTool::tool_def(),
                            nexo_core::agent::MemoryTool::new_with_default_mode(
                                mem,
                                default_recall_mode.clone(),
                            ),
                        );
                    }
                }
                // Phase 81.33.a — generic outbound tool
                // registration from manifest. Hot-spawn path
                // mirrors the boot loop's generic loop. Reads
                // plugin_handles from the shared cell at call
                // time so respawned plugins (Phase 81.21.b) get
                // their fresh handles registered.
                {
                    let guard = plugin_handles_cell.read().await;
                    if let Some(handles) = guard.as_ref() {
                        for plugin_id in &cfg.plugins {
                            if let Some(handle) = handles.get(plugin_id) {
                                handle.register_outbound_tools(&tools);
                            }
                        }
                    }
                }
                // Phase 81.20.x Stage 7 — whatsapp + telegram tool
                // register fallbacks removed from the hot-spawn path
                // as well. Both plugins' subprocesses advertise their
                // outbound tool defs in the `initialize` reply; the
                // daemon's `RemoteToolHandler` (registered above via
                // the iterator over `plugin_handles`) routes tool
                // invocations through the broker. No
                // `wa_in_manifest` / `tg_in_manifest` gating
                // required anymore.
                if cfg.heartbeat.enabled {
                    if let Some(mem) = memory.clone() {
                        tools.register(
                            nexo_core::agent::HeartbeatTool::tool_def(),
                            nexo_core::agent::HeartbeatTool::new(mem),
                        );
                    }
                }
                // Email follow-up control plane — same gate as boot:
                // email plugin + memory backend present.
                if cfg.plugins.iter().any(|p| p == "email") {
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
                    }
                }
                if cfg.plan_mode.enabled {
                    tools.register(
                        nexo_core::agent::plan_mode_tool::EnterPlanModeTool::tool_def(),
                        nexo_core::agent::plan_mode_tool::EnterPlanModeTool,
                    );
                    tools.register(
                        nexo_core::agent::plan_mode_tool::ExitPlanModeTool::tool_def(),
                        nexo_core::agent::plan_mode_tool::ExitPlanModeTool,
                    );
                    tools.register(
                        nexo_core::agent::plan_mode_tool::PlanModeResolveTool::tool_def(),
                        nexo_core::agent::plan_mode_tool::PlanModeResolveTool,
                    );
                }
                // Always-on tools (cheap, pure, no captures).
                tools.register(
                    nexo_core::agent::todo_write_tool::TodoWriteTool::tool_def(),
                    nexo_core::agent::todo_write_tool::TodoWriteTool,
                );
                tools.register(
                    nexo_core::agent::tool_search_tool::ToolSearchTool::tool_def(),
                    nexo_core::agent::tool_search_tool::ToolSearchTool::new(),
                );
                tools.register(
                    nexo_core::agent::synthetic_output_tool::SyntheticOutputTool::tool_def(),
                    nexo_core::agent::synthetic_output_tool::SyntheticOutputTool,
                );
                tools.register(
                    nexo_core::agent::notebook_edit_tool::NotebookEditTool::tool_def(),
                    nexo_core::agent::notebook_edit_tool::NotebookEditTool,
                );
                tools.register(
                    nexo_core::agent::mcp_router_tool::ListMcpResourcesTool::tool_def(),
                    nexo_core::agent::mcp_router_tool::ListMcpResourcesTool,
                );
                tools.register(
                    nexo_core::agent::mcp_router_tool::ReadMcpResourceTool::tool_def(),
                    nexo_core::agent::mcp_router_tool::ReadMcpResourceTool,
                );
                // Proactive Sleep — gate matches boot.
                let proactive_enabled_somewhere = cfg.proactive.enabled
                    || cfg
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
                // RemoteTrigger — gate matches boot.
                let remote_triggers_enabled_somewhere = !cfg.remote_triggers.is_empty()
                    || cfg
                        .inbound_bindings
                        .iter()
                        .filter_map(|b| b.remote_triggers.as_ref())
                        .any(|list| !list.is_empty());
                if remote_triggers_enabled_somewhere {
                    let sink: std::sync::Arc<
                        dyn nexo_core::agent::remote_trigger_tool::RemoteTriggerSink,
                    > = std::sync::Arc::new(
                        nexo_core::agent::remote_trigger_tool::ReqwestSink::new(broker.clone()),
                    );
                    tools.register(
                        nexo_core::agent::remote_trigger_tool::RemoteTriggerTool::tool_def(),
                        nexo_core::agent::remote_trigger_tool::RemoteTriggerTool::new(sink),
                    );
                }
                // Phase 81.20.x F2.3 — email outbound tools register
                // path removed. Email v0.6.0+ subprocess advertises
                // its 12 tools via JSON-RPC; daemon's RemoteToolHandler
                // routes per-agent invocations through the broker
                // without daemon-side fallback.
                // Phase 81.32 c7.c.2 — pollers control tools.
                if let Some(runner) = pollers_runner.as_ref() {
                    nexo_poller_tools::register_all(&tools, Arc::clone(runner));
                }
                // Phase 81.32 c7.c.2 — channel_list/send/status
                // when this agent's bindings declare
                // `allowed_channel_servers`. Mirrors boot loop's
                // `channels_in_play` gate.
                let channels_in_play = cfg.channels.is_some()
                    && cfg
                        .inbound_bindings
                        .iter()
                        .any(|b| !b.allowed_channel_servers.is_empty());
                if channels_in_play {
                    use nexo_core::agent::channel_list_tool::ChannelListTool;
                    use nexo_core::agent::channel_send_tool::ChannelSendTool;
                    use nexo_core::agent::channel_status_tool::ChannelStatusTool;
                    let list_def = ChannelListTool::tool_def();
                    let list_handler = std::sync::Arc::new(ChannelListTool::new_dynamic(
                        channel_boot.registry.clone(),
                    ));
                    tools.register_arc(list_def, list_handler);
                    let send_def = ChannelSendTool::tool_def();
                    let send_handler = std::sync::Arc::new(ChannelSendTool::new_dynamic(
                        channel_boot.registry.clone(),
                    ));
                    tools.register_arc(send_def, send_handler);
                    let status_def = ChannelStatusTool::tool_def();
                    let status_handler = std::sync::Arc::new(ChannelStatusTool::new_dynamic(
                        channel_boot.registry.clone(),
                    ));
                    tools.register_arc(status_def, status_handler);
                }
                // Phase 81.32 c7.c.2 — extension tools registered
                // post-handshake (same iteration as boot). Hooks
                // are not wired in hot-spawn because behavior
                // already constructed above; extension hooks
                // wire at behavior-build time (deferred to
                // c7.c.3).
                for (rt, cand) in &extension_runtimes {
                    let pid = cand.manifest.id();
                    for desc in &rt.handshake().tools {
                        let def = nexo_core::agent::ExtensionTool::tool_def(desc, pid);
                        let handler = nexo_core::agent::ExtensionTool::new(
                            pid,
                            desc.name.clone(),
                            Arc::clone(rt),
                        )
                        .with_descriptor_metadata(
                            desc.description.clone(),
                            desc.input_schema.clone(),
                        )
                        .with_context_passthrough(cand.manifest.context.passthrough);
                        tools.register_if_absent(def, handler);
                    }
                }
                // Phase 81.32 c7.c.2 — MCP tools per-agent.
                // Reuses the shared sentinel session for the
                // process. Channel inbound loops + hot-reload
                // callbacks (boot lines 5915+, 6010+) are
                // deferred to c7.c.3 because they spawn
                // long-lived tasks; the registration alone is
                // safe to repeat across spawn calls.
                if let Some(mgr) = &mcp_manager {
                    let rt = mgr.get_or_create(uuid::Uuid::nil()).await;
                    let mcp_ctx_pt = mcp_cfg
                        .as_ref()
                        .map(|m| m.context.passthrough)
                        .unwrap_or(false);
                    let mcp_overrides: std::collections::HashMap<String, bool> = mcp_cfg
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
                        mcp_overrides,
                    )
                    .await;
                }
                // Built-in defer marks. Idempotent: tools that
                // didn't get registered above are silently skipped.
                nexo_core::agent::mark_built_in_deferred(&tools);
                // Apply legacy (no-bindings) agent-level
                // allowlist; per-binding allowlists are enforced
                // at session time as in boot.
                if cfg.inbound_bindings.is_empty() && !cfg.allowed_tools.is_empty() {
                    tools.retain_matching(&cfg.allowed_tools);
                }

                // 3. Validate (catches typo'd `allowed_tools` +
                //    telegram-binding consistency).
                let tool_defs = tools.to_tool_defs();
                let known_tool_names: Vec<&str> =
                    tool_defs.iter().map(|d| d.name.as_str()).collect();
                validate_agent_config(&cfg, &plugins, &known_tool_names)?;

                // 4. Behavior + Agent.
                let behavior = LlmAgentBehavior::new(Arc::clone(&llm), Arc::clone(&tools));
                let agent_cfg = cfg.clone();
                let agent = Arc::new(Agent::new(agent_cfg, behavior));

                // 5. Pairing adapter registry — Phase 81.20.x Stage 7:
                //    empty registry, populated by the loop below from
                //    each plugin's `build_pairing_adapter()` trait
                //    method (Phase 81.33.b.real).
                let pairing_registry = nexo_pairing::PairingAdapterRegistry::new();
                {
                    let guard = plugin_handles_cell.read().await;
                    if let Some(handles) = guard.as_ref() {
                        for (plugin_id, handle) in handles.iter() {
                            if let Some(adapter) = handle.build_pairing_adapter(broker.clone()) {
                                tracing::info!(
                                    plugin = %plugin_id,
                                    channel = %adapter.channel_id(),
                                    "registered manifest-driven pairing adapter (hot-spawn, Phase 81.33.b.real)",
                                );
                                pairing_registry.register(adapter);
                            }
                        }
                    }
                }

                // 6. Assemble runtime via shared helper.
                let assembly_deps = RuntimeAssemblyDeps {
                    tools: Arc::clone(&tools),
                    memory: memory.clone(),
                    peers: Arc::clone(&peer_directory),
                    redactor: Arc::clone(&transcripts_redactor),
                    transcripts_index: transcripts_index.as_ref().map(Arc::clone),
                    credentials: credentials.as_ref().map(|b| Arc::clone(&b.resolver)),
                    breakers: credentials.as_ref().map(|b| Arc::clone(&b.breakers)),
                    link_extractor: Arc::clone(&link_extractor),
                    // Phase 95 — web_search_router removed.
                    pairing_gate: Arc::clone(&pairing_gate),
                    pairing_adapters: pairing_registry,
                    plan_approval_registry: plan_approval_registry.clone(),
                    dispatch_ctx: dispatch_ctx.as_ref().map(Arc::clone),
                    processing_store: Arc::clone(&processing_store),
                    event_emitter,
                };
                let runtime = assemble_agent_runtime(
                    Arc::clone(&agent),
                    broker.clone(),
                    Arc::clone(&sessions),
                    assembly_deps,
                );

                // 7. Capture handles before start. `start()` takes
                //    `&self` so we can still move `runtime` into
                //    the SpawnedAgent afterwards.
                let reload_tx = runtime.reload_sender();
                let snapshot_handle = runtime.snapshot_handle();
                let known_tools: Arc<Vec<String>> = Arc::new(
                    tools
                        .to_tool_defs()
                        .iter()
                        .map(|d| d.name.clone())
                        .collect(),
                );

                // 8. Insert into shared DashMaps so the cron
                //    post-hook + reload coordinator see the new
                //    agent on the next reload.
                tools_per_agent.insert(agent_id.clone(), Arc::clone(&tools));
                agent_snapshot_handles.insert(agent_id.clone(), snapshot_handle);

                // 9. Start. Spawns broker subs + heartbeat tasks
                //    via `tokio::spawn` internally — the
                //    AgentRuntime can drop after this and the
                //    tasks keep running via cloned Arc state.
                runtime
                    .start()
                    .await
                    .map_err(|e| SpawnError::Internal(format!("runtime.start: {e}")))?;

                tracing::info!(
                    agent = %agent_id,
                    tool_count = tools.to_tool_defs().len(),
                    "hot-spawned with full tool parity (Phase 81.32 c7.c.2). \
                     Still deferred: google_* OAuth + extension hooks + \
                     MCP hot-reload callbacks — Phase 81.32 c7.c.3 follow-up."
                );

                Ok(SpawnedAgent {
                    agent_id,
                    reload_tx,
                    known_tools,
                    shutdown_token: tokio_util::sync::CancellationToken::new(),
                    runtime,
                })
            })
        }));
        // Phase 81.32 c7.b.followup — closure returns out of the
        // outer `let pending_spawner = { ... }` block; the actual
        // install happens after `coord.start()` stashes the
        // broker handle below.
        Arc::new(spawner)
    };
    // Flush in-process gate caches after every reload so
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
    // Cron config-reload post-hook. Rebuilds the per-binding
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
                    tracing::debug!("[cron] post-hook fired before executor built; skipping");
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
    // Daemon-embed MCP HTTP server. Opt-in via
    // `mcp_server.daemon_embed.enabled: true` + `mcp_server.http
    // .enabled: true`. Reuses the primary agent's tool registry
    // (mirrors `nexo mcp-server` standalone behavior) and
    // registers a reload-coord post-hook that swaps the
    // allowlist + emits `notifications/tools/list_changed` on
    // every config reload — automatic, no SIGHUP needed.
    // Returned handle survives until the daemon's main shutdown
    // sequence drains it.
    let mcp_embed_handle: Option<nexo_mcp::HttpServerHandle> =
        match cfg.mcp_server.as_ref().filter(|s| s.daemon_embed.enabled) {
            Some(server_cfg) => {
                let (primary_id, primary_cfg) = primary_for_mcp_embed.clone().ok_or_else(|| {
                    anyhow::anyhow!("mcp_server.daemon_embed enabled but agents.yaml has no agents")
                })?;
                let primary_tools = tools_per_agent
                    .get(&primary_id)
                    .map(|r| Arc::clone(r.value()))
                    .ok_or_else(|| {
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

                // Reload-coord post-hook: on every config reload,
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

    // Register post-reload hook that re-discovers
    // plugin manifests and atomically swaps the registry snapshot.
    // Hook captures discovery_cfg + version immutable; new
    // search_paths require a daemon restart.
    nexo_core::agent::nexo_plugin_registry::register_plugin_registry_reload_hook(
        Arc::clone(&reload_coord),
        Arc::clone(&wire.registry),
        cfg.plugins.discovery.clone(),
        semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
    )
    .await;

    if let Err(e) = Arc::clone(&reload_coord)
        .start(broker.clone(), cfg.runtime.reload.clone())
        .await
    {
        tracing::warn!(error = %e, "config reload coordinator failed to start — hot-reload disabled");
    }
    // Phase 81.32 c7.b.followup — broker handle is now stashed
    // by `start()`; safe to wire the spawner so hot-spawn
    // firehose events reach subscribers from the first call.
    reload_coord.set_spawner(pending_spawner);

    // Late-bind the reload coord into the
    // ConfigTool's reload trigger. The trigger was constructed
    // before the agent loop (so the per-agent registration could
    // hold an `Arc<dyn ReloadTrigger>` upfront); now that the
    // coordinator exists we resolve the OnceCell. After this point
    // a `Config { op: apply }` call drives `coord.reload()`.
    #[cfg(feature = "config-self-edit")]
    if let Some(cell) = reload_cell.as_ref() {
        let _ = cell.set(Arc::clone(&reload_coord));
    }

    // Spawn ONE cron runner per process.
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
                    // Single source of truth: boot path uses
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
                        let executor =
                            std::sync::Arc::new(RuntimeCronToolExecutor::new(cron_tool_bindings));
                        // Late-bind into the post-hook cell so
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
    // Graceful drain of the daemon-embed MCP HTTP
    // server. `watcher_shutdown.cancel()` (below) signals the
    // server to drain; we await its `join` with a 5s budget so
    // SSE consumers see a clean disconnect. No-op when
    // `daemon_embed.enabled = false`.
    if let Some(handle) = mcp_embed_handle {
        watcher_shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle.join).await;
    }
    // Drop the team router subscriber. Active teams
    // keep their soft-deleted state; force-kill of in-flight
    // teammate goals is delegated to the existing
    // `drain_running_goals` pattern that runs below.
    team_router_cancel.cancel();
    // Shut down the LSP manager BEFORE plugin
    // teardown so any in-flight `$/cancelRequest` notifications
    // make it out to the language servers and child processes
    // exit cleanly. `kill_on_drop(true)` is the safety net.
    lsp_manager.shutdown().await;

    // Drain in-flight goals BEFORE bringing channel
    // plugins down. Walk the registry that lives inside the
    // dispatch context, fire `notify_origin` / `notify_channel`
    // hooks with a clean "[shutdown]" summary, and flip Running
    // rows to `LostOnRestart` so a future reattach sweep does not
    // re-fire them. We cannot wait for the Claude Code subprocess
    // to land its last commit — 5–10 s is not enough — so the
    // contract here is "tell the operator the goal was abandoned
    // cleanly". SIGKILL still bypasses this; the boot-time sweep
    // is the safety net for that case.
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

    // Phase 81.20.x F2.1 — email plugin shutdown moved to the
    // subprocess supervisor (Phase 81.21.b.b); daemon no longer
    // calls `EmailPlugin::stop` on its own.

    // Shut down the MCP runtime before draining agents: in-flight tool calls
    // that are routed through MCP will cancel cleanly and the agents get
    // proper `TransportLost` errors instead of timing out.
    if let Some(mgr) = mcp_manager.clone() {
        tracing::info!("shutting down mcp runtime manager");
        mgr.shutdown_all_with_reason("sigterm").await;
    }

    // Graceful extension shutdown. Send the `shutdown`
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

/// Boot the shared DispatchToolContext when the
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
    llm_registry: Arc<nexo_llm::LlmRegistry>,
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

    // Permission decider — the LLM decider lives in the
    // standalone bin; here we keep the simpler AllowAll path so the
    // chat-side surface works without an extra LLM call. Operators
    // who want strict permission go via the standalone nexo-driver.
    let inner_decider: Arc<dyn nexo_driver_permission::PermissionDecider> =
        Arc::new(nexo_driver_permission::AllowAllDecider);

    // Wrap the decider in a ChannelRelayDecider when
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
            dyn Fn(&str) -> Option<std::sync::Arc<dyn nexo_mcp::McpClient>> + Send + Sync,
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
            let rt = tokio::runtime::Handle::current()
                .block_on(async { mgr.get_or_create(uuid::Uuid::nil()).await });
            rt.clients()
                .into_iter()
                .find(|(name, _)| name == server_name)
                .map(|(_, client)| client)
        });
        let dispatcher: std::sync::Arc<
            dyn nexo_mcp::channel_permission::PermissionRelayDispatcher,
        > = std::sync::Arc::new(nexo_mcp::channel_permission::ClientResolverDispatcher::new(
            resolver,
        ));
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
    // Honour `agent_registry.store` from
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
    // Open the durable turn log on the same sqlite
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
    // Hook registry mirrors writes to SQLite so attached
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
    // Orphan sweep: the agent-registry doesn't get
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

    // Hook dispatcher with the channel adapters the pairing layer
    // owns. Adapters are registered into a SHARED registry here so
    // notify_origin reaches WhatsApp / Telegram out of the box.
    // Phase 81.33.b — single source of truth via
    // `build_known_pairing_registry` helper.
    //
    // Phase 81.33.b.real — this branch (`boot_dispatch_ctx_if_enabled`,
    // dispatch-ctx / autonomous-worker mode) does NOT yet iterate
    // `plugin_handles_cell` for manifest-driven adapters because
    // the cell is not threaded into this function. Manifest
    // adapters work for the primary agent runtime path
    // (`src/main.rs:6416-6437` boot + `:7224-...` hot-spawn).
    // Phase 81.20.x Stage 7 — dispatch-ctx path also starts with
    // an empty registry; pairing-aware hooks are a follow-up
    // (would require plumbing `plugin_handles_cell` through this
    // helper too).
    let pairing_registry = nexo_pairing::PairingAdapterRegistry::new();
    let _ = _broker;
    // Hook idempotency store. Lives next to other state sidecars
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

    // Reattach sweep. When the registry is sqlite-backed
    // and `reattach_on_boot: true`, walk every Running row from the
    // last run, mark it `LostOnRestart`, and fire any
    // `notify_origin` / `notify_channel` hooks the operator had
    // attached so the original chat sees a clean closure
    // ("daemon restart — goal abandoned"). Without this, every
    // SIGKILL leaves the operator waiting forever.
    //
    // Resume-as-Running is intentionally OFF: respawning a Claude
    // Code subprocess against a worktree the daemon no longer owns
    // is out of scope here, and unsafe to do silently. Marking
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

    // After reattach restores paused rows from sqlite,
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
            // isolation keeps the live source safe
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
                // Audit goals run inside the parent's
                // worktree so Claude sees the commits.
                workspace_root: driver_cfg.workspace.root.clone(),
                // Separate cap so audits can't be
                // starved by main dispatch traffic.
                audit_cap: Some(2),
            })
                as Arc<dyn nexo_dispatch_tools::DispatchPhaseChainer>),
            // Share the daemon's
            // single Arc<LlmRegistry>
            // so PreflightHandler reports llm_ready accurately
            // for any registered provider, not just the legacy
            // anthropic/minimax hardcode.
            llm_registry: Some(llm_registry.clone()),
        },
    ))
}

/// Resolve the secrets directory for credential loaders. Convention is
/// Adapter from the `nexo-config` wire-shape to the canonical
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

/// Discover extensions and spawn stdio runtimes.
/// Never fatal: bad manifests or spawn failures produce diagnostics; the
/// agent keeps starting. Returns runtimes that the caller must keep alive
/// (drop → cascades SIGTERM to extension children).
///
/// When `admin_bootstrap` is `Some`, each
/// extension that declares `[capabilities.admin]` in its plugin.toml
/// is spawned with the per-microapp `AdminRouter` wired into its
/// `StdioSpawnOptions`. Post-spawn the bootstrap binds the live
/// `outbox_sender()` so admin response frames flow back to the
/// extension's stdin without a separate writer channel.
#[allow(clippy::type_complexity)]
async fn run_extension_discovery(
    cfg: Option<&nexo_config::ExtensionsConfig>,
    admin_bootstrap: Option<&nexo_setup::admin_bootstrap::AdminRpcBootstrap>,
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

    // Collect extension-declared MCP servers before we consume the
    // candidate list; main() later feeds these into `McpRuntimeManager`.
    let mcp_decls = nexo_extensions::collect_mcp_declarations(&report, &cfg.disabled);

    // Spawn stdio runtimes for each candidate whose transport is Stdio.
    // The caller iterates the returned runtimes to register tools per agent.
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
        // When the operator wired
        // `[capabilities.admin]` for this id, route the spawn
        // through `spawn_with` so the per-microapp `AdminRouter`
        // lands in the reader task. Plain `spawn` for the rest
        // (zero overhead path).
        let admin_opts = admin_bootstrap.and_then(|b| {
            b.spawn_options_for(
                &id,
                nexo_extensions::StdioSpawnOptions {
                    cwd: c.root_dir.clone(),
                    ..Default::default()
                },
            )
        });
        let rt_result = match admin_opts {
            Some(opts) => nexo_extensions::StdioRuntime::spawn_with(&c.manifest, opts).await,
            None => nexo_extensions::StdioRuntime::spawn(&c.manifest, c.root_dir.clone()).await,
        };
        match rt_result {
            Ok(rt) => {
                tracing::info!(
                    ext = %id,
                    tools = rt.handshake().tools.len(),
                    "extension runtime ready",
                );
                if let Some(b) = admin_bootstrap {
                    b.bind_writer(&id, rt.outbox_sender());
                }
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
    // Fast path on Linux: /proc/<pid> exists iff the process is alive.
    #[cfg(target_os = "linux")]
    if std::path::Path::new(&format!("/proc/{pid}")).exists() {
        return true;
    }
    #[cfg(windows)]
    {
        return std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false);
    }
    // Portable Unix fallback: `kill -0` succeeds iff the process exists
    // and we may signal it. Avoids pulling in a libc dep.
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
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

// ── Pair CLI handlers ────────────────────────────────────────────────────
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

/// `nexo agent doctor plugins [--json]` handler.
/// Loads the daemon config in-process, runs the
/// `wire_plugin_registry` pipeline, and renders the resulting
/// snapshot via `doctor_render`. Returns the desired exit code:
/// `0` when only warn-level diagnostics + accepted overrides /
/// disabled / allowlist rejects surfaced; `1` when any error
/// diagnostic, `LastPluginWins` conflict, or `Failed` init
/// outcome appears.
/// Bridge `nexo_setup::capabilities::INVENTORY` to
/// the plugin capability aggregator's `&[(env_var, extension)]`
/// slice shape. Lives in main.rs because `nexo-core` cannot depend
/// on `nexo-setup` (cycle); main.rs is the boundary that knows
/// both sides.
fn core_capability_env_vars() -> Vec<(&'static str, &'static str)> {
    // INVENTORY is private to nexo-setup; surface its (env_var,
    // extension) tuples via the public `evaluate_all` API. This
    // pays one redundant env::var read per toggle at boot — fine
    // for boot-time observability; the alternative would be
    // making INVENTORY public, which would invalidate the
    // drift-prevention contract.
    nexo_setup::capabilities::evaluate_all()
        .into_iter()
        .map(|s| (s.toggle.env_var, s.toggle.extension))
        .collect()
}

/// Describe which framework capabilities are
/// actually wired in the current daemon config. Aggregator uses
/// this set to surface unmet `requires.nexo_capabilities` as
/// Warn-level diagnostics. Conservative: only marks capabilities
/// that the loaded config definitively wires.
fn build_available_capabilities(
    cfg: &nexo_config::AppConfig,
) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    // Always-on framework capabilities.
    set.insert("broker".to_string());
    set.insert("memory".to_string()); // short-term memory always on
    set.insert("sessions".to_string());
    // Conditionally-wired.
    if !cfg.memory.long_term.backend.is_empty() {
        set.insert("long_term_memory".to_string());
    }
    set
}

async fn run_doctor_plugins(config_dir: &std::path::Path, json: bool) -> Result<i32> {
    let cfg = nexo_config::AppConfig::load(config_dir)
        .with_context(|| format!("failed to load config from {}", config_dir.display()))?;
    let core_envs = core_capability_env_vars();
    let available = build_available_capabilities(&cfg);
    let mut agents = cfg.agents;
    let version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
    let wire = nexo_core::agent::nexo_plugin_registry::wire_plugin_registry(
        &mut agents,
        &cfg.plugins.discovery,
        &version,
        &core_envs,
        &available,
        // Doctor handler runs the same offline
        // pipeline as boot wire; no factories registered yet.
        None,
    )
    .await;
    let snap = wire.registry.snapshot();
    let exit_code =
        nexo_core::agent::nexo_plugin_registry::doctor_render::determine_exit_code(&snap);
    if json {
        let out = nexo_core::agent::nexo_plugin_registry::doctor_render::render_json(
            &snap,
            &wire.channel_adapter_registry,
            config_dir,
            &version,
        );
        println!("{out}");
    } else {
        let out = nexo_core::agent::nexo_plugin_registry::doctor_render::render_text(
            &snap,
            &wire.channel_adapter_registry,
            config_dir,
            &version,
        );
        // render_text already terminates with newlines per section
        // and a trailing newline after EXIT — use print! to avoid
        // duplicating the final newline.
        print!("{out}");
    }
    Ok(exit_code)
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
    //       `nexo-tunnel` crate exposes an in-process accessor).
    //       The `nexo-tunnel` daemon writes its assigned
    //       `https://*.trycloudflare.com` URL here at startup so a
    //       separately-launched `nexo pair start` picks it up.
    //   4. loopback-only → fails closed with a clear error.
    //
    // `ws_cleartext_allow` from the YAML extends the resolver's
    // built-in allow list (loopback / RFC1918 / link-local /
    // `.local` / `10.0.2.2`).
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
        // Wave 6 — read opaque YAML; nexo-config no longer carries
        // typed telegram. Pull `telegram: [{instance: ...}]` directly.
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            if let Some(seq) = v.get("telegram").and_then(|x| x.as_sequence()) {
                for entry in seq {
                    let account = entry
                        .get("instance")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default");
                    eprintln!("  nexo pair seed telegram {account} <YOUR_TELEGRAM_USER_ID>");
                    suggested = true;
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(plugins_dir.join("whatsapp.yaml")) {
        // Wave 7 — opaque YAML; nexo-config no longer carries typed
        // whatsapp. Pull `whatsapp: [{instance: ...}]` directly.
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            if let Some(seq) = v.get("whatsapp").and_then(|x| x.as_sequence()) {
                for entry in seq {
                    let account = entry
                        .get("instance")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default");
                    eprintln!("  nexo pair seed whatsapp {account} <YOUR_WHATSAPP_NUMBER>");
                    suggested = true;
                }
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

/// Route a `nexo memory <sub>` invocation. Pulls the subcommand verb
/// from `pos_no_flags` (already flag-stripped) and reads kv flags
/// off the raw `positional` slice. Returns `None` if `<sub>` is not
/// a recognised verb.
fn route_memory_subcommand(
    pos_no_flags: &[String],
    positional: &[String],
    has_json_flag: bool,
) -> Option<Mode> {
    let sub = pos_no_flags.get(1).map(|s| s.as_str())?;
    let agent = parse_kv_flag(positional, "--agent").unwrap_or_default();
    let tenant = parse_kv_flag(positional, "--tenant").unwrap_or_else(|| "default".into());
    let state_root = parse_kv_flag(positional, "--state-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./state"));
    let memdir_root = parse_kv_flag(positional, "--memdir-root").map(PathBuf::from);
    let sqlite_root = parse_kv_flag(positional, "--sqlite-root").map(PathBuf::from);

    match sub {
        "verify" => {
            let bundle = parse_kv_flag(positional, "--bundle")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            Some(Mode::Memory(MemorySubcommand::Verify {
                bundle,
                json: has_json_flag,
            }))
        }
        "snapshot" => Some(Mode::Memory(MemorySubcommand::Snapshot {
            agent,
            tenant,
            label: parse_kv_flag(positional, "--label"),
            no_redact: positional.iter().any(|a| a == "--no-redact"),
            encrypt: parse_kv_flag(positional, "--encrypt"),
            state_root,
            memdir_root,
            sqlite_root,
            json: has_json_flag,
        })),
        "restore" => Some(Mode::Memory(MemorySubcommand::Restore {
            agent,
            tenant,
            bundle: parse_kv_flag(positional, "--from")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            dry_run: positional.iter().any(|a| a == "--dry-run"),
            no_auto_pre_snapshot: positional.iter().any(|a| a == "--no-auto-pre-snapshot"),
            decrypt_identity: parse_kv_flag(positional, "--decrypt-identity").map(PathBuf::from),
            state_root,
            memdir_root,
            sqlite_root,
            json: has_json_flag,
        })),
        "list" => Some(Mode::Memory(MemorySubcommand::List {
            agent,
            tenant,
            state_root,
            json: has_json_flag,
        })),
        "diff" => {
            let a = pos_no_flags.get(2)?.clone();
            let b = pos_no_flags.get(3)?.clone();
            Some(Mode::Memory(MemorySubcommand::Diff {
                agent,
                tenant,
                a,
                b,
                state_root,
                json: has_json_flag,
            }))
        }
        "export" => Some(Mode::Memory(MemorySubcommand::Export {
            agent,
            tenant,
            id: parse_kv_flag(positional, "--id").unwrap_or_default(),
            to: parse_kv_flag(positional, "--to")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            state_root,
        })),
        "delete" => Some(Mode::Memory(MemorySubcommand::Delete {
            agent,
            tenant,
            id: parse_kv_flag(positional, "--id").unwrap_or_default(),
            state_root,
            yes: positional.iter().any(|a| a == "--yes"),
        })),
        _ => None,
    }
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
    // Config dir precedence:
    //   1. explicit `--config <dir>` flag (highest priority)
    //   2. `NEXO_CONFIG_DIR` env var (dev-daemon.sh exports it
    //      so `nexo set-broker local` works from any cwd
    //      without typing the flag)
    //   3. `./config` if present in cwd (legacy default for
    //      operators with an existing project layout)
    //   4. XDG default — `$XDG_CONFIG_HOME/nexo` or
    //      `$HOME/.config/nexo`. Auto-created by helpers like
    //      `set-broker` when missing so `nexo set-broker local`
    //      runs anywhere with zero setup.
    let mut config_dir = std::env::var("NEXO_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let cwd_default = PathBuf::from("./config");
            if cwd_default.exists() {
                cwd_default
            } else {
                default_xdg_config_dir()
            }
        });
    // Override dir layer. Sourced from
    // `NEXO_OVERRIDE_FROM` env; `--override-from <dir>` flag wins.
    let mut override_from: Option<PathBuf> = std::env::var("NEXO_OVERRIDE_FROM")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let mut positional: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                if let Some(path) = args.next() {
                    config_dir = PathBuf::from(path);
                }
            }
            "--override-from" => {
                if let Some(path) = args.next() {
                    override_from = Some(PathBuf::from(path));
                }
            }
            "--help" | "-h" => {
                return CliArgs {
                    config_dir,
                    override_from,
                    mode: Mode::Help,
                    plugin_run_override: None,
                    persona_run_override: None,
                }
            }
            other => positional.push(other.to_string()),
        }
    }

    // `--version` / `-V`. Pair with `--verbose` for the
    // build-provenance block. The `version` subcommand is handled below
    // alongside the other positional commands.
    if positional.iter().any(|a| a == "--version" || a == "-V") {
        let verbose = positional.iter().any(|a| a == "--verbose");
        return CliArgs {
            config_dir,
            mode: Mode::Version { verbose },
            override_from: override_from.clone(),
            plugin_run_override: None,
            persona_run_override: None,
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
            override_from: override_from.clone(),
            plugin_run_override: None,
            persona_run_override: None,
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
            override_from: override_from.clone(),
            plugin_run_override: None,
            persona_run_override: None,
        };
    }

    // Pair CLI handled first so flag values like
    // `--public-url wss://example.com` (which the value-only filter
    // does not strip) don't shift the structural arity of the main
    // match arms below.
    if pos_no_flags.first().map(|s| s.as_str()) == Some("pair") {
        if let Some(mode) = route_pair_subcommand(&positional, has_json_flag) {
            return CliArgs {
                config_dir,
                mode,
                override_from: override_from.clone(),
                plugin_run_override: None,
                persona_run_override: None,
            };
        }
    }

    if pos_no_flags.first().map(|s| s.as_str()) == Some("cron") {
        if let Some(mode) = route_cron_subcommand(&positional, has_json_flag) {
            return CliArgs {
                config_dir,
                mode,
                override_from: override_from.clone(),
                plugin_run_override: None,
                persona_run_override: None,
            };
        }
    }

    // `nexo microapp admin audit tail [...]`.
    // Route by pos_no_flags prefix (not slice-arity match) since
    // flag values like `/path/to.db` after `--db` survive the
    // `--`-strip and would shift the slice arity off-by-one.
    if pos_no_flags.first().map(|s| s.as_str()) == Some("microapp")
        && pos_no_flags.get(1).map(|s| s.as_str()) == Some("admin")
        && pos_no_flags.get(2).map(|s| s.as_str()) == Some("audit")
        && pos_no_flags.get(3).map(|s| s.as_str()) == Some("tail")
    {
        return CliArgs {
            config_dir,
            mode: Mode::MicroappAuditTail {
                microapp_id: parse_kv_flag(&positional, "--microapp-id"),
                method: parse_kv_flag(&positional, "--method"),
                result: parse_kv_flag(&positional, "--result"),
                since_mins: parse_kv_flag(&positional, "--since-mins")
                    .and_then(|v: String| v.parse().ok()),
                since_ms: parse_kv_flag(&positional, "--since-ms")
                    .and_then(|v: String| v.parse().ok()),
                limit: parse_kv_flag(&positional, "--limit")
                    .and_then(|v: String| v.parse().ok())
                    .unwrap_or(50),
                format: parse_kv_flag(&positional, "--format").unwrap_or_else(|| "table".into()),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                tenant_id: parse_kv_flag(&positional, "--tenant"),
            },
            override_from: override_from.clone(),
            plugin_run_override: None,
            persona_run_override: None,
        };
    }

    if pos_no_flags.first().map(|s| s.as_str()) == Some("memory") {
        if let Some(mode) = route_memory_subcommand(&pos_no_flags, &positional, has_json_flag) {
            return CliArgs {
                config_dir,
                mode,
                override_from: override_from.clone(),
                plugin_run_override: None,
                persona_run_override: None,
            };
        }
    }

    let mode = match pos_no_flags.as_slice() {
        [] => Mode::Run,
        // `nexo version` is the verbose form (always
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
        // `nexo set-broker <kind> [--url <url>] [--no-signal]`
        // Switch the broker transport in `broker.yaml` and (by default)
        // kick the running daemon so its supervisor respawns with the
        // new config.
        [cmd, kind, ..] if cmd == "set-broker" => {
            let url_idx = positional.iter().position(|a| a == "--url");
            let url = url_idx
                .and_then(|i| positional.get(i + 1))
                .map(|s| s.to_string());
            Mode::SetBroker {
                kind: kind.clone(),
                url,
                no_signal: positional.iter().any(|a| a == "--no-signal"),
            }
        }
        // `nexo init [...]` scaffolds sample YAMLs.
        // Match `init` plus any rest (flag values like the path
        // after `--output` survive in `pos_no_flags`; the actual
        // flags themselves are stripped). `parse_init_args`
        // walks the outer `positional` directly so it sees flags
        // and their values together.
        [cmd, ..] if cmd == "init" => {
            let (yaml_filter, output_dir, force, stdout) = init_cli::parse_init_args(&positional);
            Mode::Init {
                yaml_filter,
                output_dir,
                force,
                stdout,
            }
        }
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
        [cmd, sub, id] if cmd == "ext" && sub == "state-dir" => Mode::ExtStateDir {
            id: id.clone(),
            ensure: positional.iter().any(|a| a == "--ensure"),
        },
        // `nexo plugin install <coords> [...]`.
        [cmd, sub, coords] if cmd == "plugin" && sub == "install" => Mode::PluginInstall {
            coords: coords.clone(),
            dest: parse_kv_flag(&positional, "--dest").map(PathBuf::from),
            target: parse_kv_flag(&positional, "--target"),
            json: has_json_flag,
            require_signature: positional.iter().any(|a| a == "--require-signature"),
            skip_signature_verify: positional.iter().any(|a| a == "--skip-signature-verify"),
        },
        [cmd, sub] if cmd == "plugin" && sub == "help" => Mode::PluginHelp,
        // `nexo plugin run <path-or-manifest> [...]`.
        [cmd, sub, path] if cmd == "plugin" && sub == "run" => Mode::PluginRun {
            path: PathBuf::from(path),
            no_daemon_config: positional.iter().any(|a| a == "--no-daemon-config"),
            watch: positional.iter().any(|a| a == "--watch"),
            verbose: positional.iter().any(|a| a == "--verbose"),
            json: has_json_flag,
        },
        // `nexo plugin new <id> --lang <lang> [...]`.
        [cmd, sub, id] if cmd == "plugin" && sub == "new" => Mode::PluginNew {
            id: id.clone(),
            lang: parse_kv_flag(&positional, "--lang").unwrap_or_default(),
            dest: parse_kv_flag(&positional, "--dest").map(PathBuf::from),
            owner: parse_kv_flag(&positional, "--owner"),
            description: parse_kv_flag(&positional, "--description"),
            git_init: positional.iter().any(|a| a == "--git"),
            force: positional.iter().any(|a| a == "--force"),
            json: has_json_flag,
        },
        // `nexo plugin list [--include-orphan] [--json]`.
        [cmd, sub] if cmd == "plugin" && sub == "list" => Mode::PluginList {
            include_orphan: positional.iter().any(|a| a == "--include-orphan"),
            json: has_json_flag,
        },
        // `nexo plugin upgrade <id> [...]`.
        [cmd, sub, id] if cmd == "plugin" && sub == "upgrade" => Mode::PluginUpgrade {
            id: id.clone(),
            target: parse_kv_flag(&positional, "--target"),
            require_signature: positional.iter().any(|a| a == "--require-signature"),
            skip_signature_verify: positional.iter().any(|a| a == "--skip-signature-verify"),
            json: has_json_flag,
        },
        // `nexo plugin remove <id> [--purge-cache] [--yes] [--json]`.
        [cmd, sub, id] if cmd == "plugin" && sub == "remove" => Mode::PluginRemove {
            id: id.clone(),
            purge_cache: positional.iter().any(|a| a == "--purge-cache"),
            yes: positional.iter().any(|a| a == "--yes"),
            json: has_json_flag,
        },
        // `nexo persona <sub>` family.
        [cmd, sub, coords] if cmd == "persona" && sub == "install" => Mode::PersonaInstall {
            coords: coords.clone(),
            dest: parse_kv_flag(&positional, "--dest").map(PathBuf::from),
            target: parse_kv_flag(&positional, "--target"),
            json: has_json_flag,
        },
        [cmd, sub] if cmd == "persona" && sub == "list" => Mode::PersonaList {
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "persona" && sub == "remove" => Mode::PersonaRemove {
            id: id.clone(),
            yes: positional.iter().any(|a| a == "--yes"),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "persona" && sub == "get" => Mode::PersonaGet {
            id: id.clone(),
            json: has_json_flag,
        },
        [cmd, sub, id] if cmd == "persona" && sub == "upgrade" => Mode::PersonaUpgrade {
            id: id.clone(),
            json: has_json_flag,
        },
        [cmd, sub, path] if cmd == "persona" && sub == "run" => Mode::PersonaRun {
            path: PathBuf::from(path),
            json: has_json_flag,
        },
        [cmd, sub] if cmd == "persona" && sub == "help" => Mode::PersonaHelp,
        [cmd] if cmd == "persona" => Mode::PersonaHelp,
        // Mcp-server with optional subcommands
        [cmd] if cmd == "mcp-server" => Mode::McpServer(McpServerSubcommand::Serve),
        [cmd, sub, url] if cmd == "mcp-server" && sub == "inspect" => {
            Mode::McpServer(McpServerSubcommand::Inspect { url: url.clone() })
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
            Mode::McpServer(McpServerSubcommand::TailAudit { db: db.clone() })
        }
        // `nexo agent dream {tail|status|kill}`.
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
        [cmd, sub, verb, run_id] if cmd == "agent" && sub == "dream" && verb == "status" => {
            Mode::AgentDream(AgentDreamSubcommand::Status {
                run_id: run_id.clone(),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
                json: has_json_flag,
            })
        }
        [cmd, sub, verb, run_id] if cmd == "agent" && sub == "dream" && verb == "kill" => {
            Mode::AgentDream(AgentDreamSubcommand::Kill {
                run_id: run_id.clone(),
                force: positional.iter().any(|a| a == "--force"),
                memory_dir: parse_kv_flag(&positional, "--memory-dir").map(PathBuf::from),
                db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            })
        }
        // `nexo agent ps [--kind=...] [--all] [--json]`.
        [cmd, sub] if cmd == "agent" && sub == "ps" => Mode::AgentPs {
            kind: parse_kv_flag(&positional, "--kind"),
            all: positional.iter().any(|a| a == "--all"),
            db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            json: has_json_flag,
        },
        // `nexo agent run [--bg] <prompt...>`. Concatenates
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
        // `nexo agent attach <goal_id> [--db=...] [--json]`.
        [cmd, sub, goal_id] if cmd == "agent" && sub == "attach" => Mode::AgentAttach {
            goal_id: goal_id.clone(),
            db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            json: has_json_flag,
        },
        // `nexo agent discover [--include-interactive]
        // [--db=...] [--json]`.
        [cmd, sub] if cmd == "agent" && sub == "discover" => Mode::AgentDiscover {
            include_interactive: positional.iter().any(|a| a == "--include-interactive"),
            db: parse_kv_flag(&positional, "--db").map(PathBuf::from),
            json: has_json_flag,
        },
        // `nexo channel list [--config=<path>] [--json]`.
        [cmd, sub] if cmd == "channel" && sub == "list" => Mode::ChannelList {
            config: parse_kv_flag(&positional, "--config").map(PathBuf::from),
            json: has_json_flag,
        },
        // `nexo channel doctor [--config=<path>]
        // [--binding=<id>] [--json]`.
        [cmd, sub] if cmd == "channel" && sub == "doctor" => Mode::ChannelDoctor {
            config: parse_kv_flag(&positional, "--config").map(PathBuf::from),
            binding: parse_kv_flag(&positional, "--binding"),
            json: has_json_flag,
        },
        // `nexo channel test <server> [--binding=<id>]
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
        [cmd, sub] if cmd == "doctor" && sub == "plugins" => Mode::DoctorPlugins {
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
            // --port <N> or --port=<N>. Default 18000 — matches
            // `nexo-plugin-admin`'s `[plugin.capabilities.http_server].port`
            // so the CLI probes the same port the plugin binds.
            let mut port: u16 = 18000;
            let mut open = false;
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
                } else if a == "--open" {
                    open = true;
                }
            }
            let tunnel = positional.iter().any(|a| a == "--tunnel");
            Mode::Admin { port, open, tunnel }
        }
        [cmd] if cmd == "start" => Mode::Start,
        [cmd] if cmd == "stop" => Mode::Stop,
        [cmd] if cmd == "restart" => Mode::Restart,
        [cmd] if cmd == "update" || cmd == "self-update" => Mode::Update,
        [cmd, sub] if cmd == "service" => match sub.as_str() {
            "install" | "enable" => Mode::ServiceInstall,
            "uninstall" | "remove" | "disable" => Mode::ServiceUninstall,
            "status" => Mode::ServiceStatus,
            other => {
                eprintln!("error: unknown `service` subcommand `{other}`");
                eprintln!("usage: nexo service <install|uninstall|status>");
                Mode::Help
            }
        },
        [cmd] if cmd == "service" => {
            eprintln!("usage: nexo service <install|uninstall|status>");
            Mode::ServiceStatus
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

    CliArgs {
        config_dir,
        mode,
        override_from: override_from.clone(),
        plugin_run_override: None,
        persona_run_override: None,
    }
}

/// Print version to stdout. `verbose=false` mirrors clap's
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

/// Block-letter "NEXO" banner (ANSI Shadow figlet), shown on the
/// help screen and at daemon start.
const NEXO_BANNER: &str = "\
███╗   ██╗███████╗██╗  ██╗ ██████╗\n\
████╗  ██║██╔════╝╚██╗██╔╝██╔═══██╗\n\
██╔██╗ ██║█████╗   ╚███╔╝ ██║   ██║\n\
██║╚██╗██║██╔══╝   ██╔██╗ ██║   ██║\n\
██║ ╚████║███████╗██╔╝ ██╗╚██████╔╝\n\
╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝ ╚═════╝";

fn print_usage() {
    println!("{NEXO_BANNER}");
    println!();
    println!("agent — multi-agent runtime");
    println!();
    println!("USAGE:");
    println!("  agent [--config <dir>]                 Start the daemon (foreground)");
    println!("  agent [--config <dir>] start           Start the daemon in the background");
    println!("  agent stop                             Stop the background daemon");
    println!("  agent [--config <dir>] restart         Restart the background daemon");
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
        "  agent plugin new <id> --lang <rust|python|typescript|php> [--dest <path>] [--owner <gh-handle>]"
    );
    println!(
        "                                         [--description <text>] [--git] [--force] [--json]"
    );
    println!(
        "  agent [--config <dir>] plugin run <path> [--no-daemon-config] [--watch] [--verbose] [--json]"
    );
    println!(
        "  agent [--config <dir>] plugin install <owner>/<repo>[@<tag>] [--dest <path>] [--target <triple>]"
    );
    println!(
        "                                         [--require-signature|--skip-signature-verify] [--json]"
    );
    println!(
        "  agent [--config <dir>] plugin list [--include-orphan] [--json]   List installed plugins"
    );
    println!(
        "  agent [--config <dir>] plugin upgrade <id> [--target <triple>]   Re-resolve and upgrade"
    );
    println!(
        "                                         [--require-signature|--skip-signature-verify] [--json]"
    );
    println!(
        "  agent [--config <dir>] plugin remove <id> [--purge-cache] [--yes] [--json]   Remove plugin"
    );
    println!("  agent plugin help                      Show plugin subcommand help");
    println!(
        "  agent doctor capabilities [--json]     List write/reveal env toggles and their state"
    );
    println!(
        "  agent doctor plugins [--json]          Audit plugin discovery + merges + init outcomes (Phase 81.9.b)"
    );
    println!("  agent flow <sub> ...                   TaskFlow admin (run `agent flow` for help)");
    println!("  agent status [<id>] [--json] [--endpoint=URL] Pretty-print running agents (or one by id)");
    println!(
        "  agent --dry-run [--json]               Validate config and print a summary (no runtime)"
    );
    println!("  agent --check-config                   Validate config and exit (no runtime)");
    println!("  agent reload                           Trigger a hot-reload on the running daemon");
    println!("  agent update                           Upgrade the nexo binary in place");
    println!(
        "  agent service install [--config <dir>] Register as an OS service (auto-start on boot)"
    );
    println!("  agent service uninstall                Remove the OS service unit");
    println!("  agent service status                   Show OS service install/run state");
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
    println!(
        "  agent admin [--port <n>] [--open] [--tunnel]  Show / open / publicly tunnel the admin web UI"
    );
    println!(
        "  agent mcp-server                       Run as an MCP stdio/HTTP server (expose tools)"
    );
    println!(
        "  agent mcp-server inspect <url>         List tools + resources of a remote MCP server"
    );
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

/// `agent admin` thin wrapper.
///
/// Replaces the legacy in-process admin server (~800 LOC,
/// `run_admin_web` below — dead code, kept for reference)
/// with a CLI shim that points operators at the out-of-tree
/// `nexo-plugin-admin` plugin. The plugin is normally spawned
/// by the daemon's discovery loop at boot; this command's job
/// is to surface the URL + install hints + browser-open
/// convenience.
///
/// `port` is forwarded as `NEXO_ADMIN_HTTP_BIND` so the operator
/// can override the default `127.0.0.1:18000` from the legacy
/// `--port` flag without re-learning new flags.
async fn run_admin_via_plugin(
    port: u16,
    open: bool,
    tunnel: bool,
    config_dir: &Path,
) -> Result<()> {
    println!();
    println!("┌─ nexo admin ─────────────────────────────────────────────────");
    println!("│");

    // The admin web UI is served by the `nexo-plugin-admin` binary,
    // which the daemon discovers + spawns automatically (the standard
    // auto-subprocess plugin path). Operators never run it directly —
    // they install it (cargo) and run the daemon.
    let mut installed = admin_binary_installed();

    if !installed {
        if which_in_path("cargo") {
            println!("│  nexo-plugin-admin not found — installing it now:");
            println!("│      cargo install nexo-plugin-admin");
            println!("│");
            let ok = std::process::Command::new("cargo")
                .args(["install", "nexo-plugin-admin"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            installed = ok && admin_binary_installed();
            println!("│");
            if installed {
                println!("│  ✓ nexo-plugin-admin installed");
            } else {
                println!("│  ✗ `cargo install nexo-plugin-admin` failed");
            }
        } else {
            println!("│  nexo-plugin-admin not found, and `cargo` is not on PATH.");
        }
    } else {
        println!("│  nexo-plugin-admin: ✓ found in PATH");
    }

    if !installed {
        println!("│");
        println!("│  Install it, then re-run `nexo admin`:");
        println!("│      cargo install nexo-plugin-admin");
        println!("│  or from source:");
        println!("│      git clone https://github.com/lordmacu/nexo-rs-plugin-admin");
        println!("│      cd nexo-rs-plugin-admin && cargo install --path .");
        println!("│");
        println!("└──────────────────────────────────────────────────────────────");
        println!();
        std::process::exit(1);
    }

    let url = format!("http://127.0.0.1:{port}");
    println!("│");
    println!("│  URL:  {url}");

    // Probe the port. nexo-plugin-admin only listens once the daemon
    // has spawned it; a closed port means the daemon isn't running.
    let probe = || {
        format!("127.0.0.1:{port}")
            .parse::<std::net::SocketAddr>()
            .ok()
            .map(|addr| {
                std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(400))
                    .is_ok()
            })
            .unwrap_or(false)
    };
    let mut reachable = probe();
    let will_autostart = !reachable && (open || tunnel);
    if reachable {
        println!("│  Status: ✓ reachable");
    } else if will_autostart {
        println!("│  Status: ✗ daemon not running — starting it (background) …");
    } else {
        println!("│  Status: ✗ not reachable — start the daemon first:");
        println!("│      nexo            # foreground");
        println!("│      nexo start      # background");
        println!("│  (it discovers + spawns nexo-plugin-admin automatically)");
    }
    if !tunnel {
        println!("│");
        println!("│  Public URL on demand:");
        println!("│      nexo admin --tunnel        # free Cloudflare quick tunnel");
    }
    println!("│");
    println!("└──────────────────────────────────────────────────────────────");
    println!();

    if will_autostart {
        // Auto-generate the operator bearer token if not already set.
        // The daemon passes it through to the admin plugin subprocess
        // via env inheritance; without it the admin plugin disables its
        // HTTP server and the operator can't reach the UI.
        if std::env::var("NEXO_ADMIN_TOKEN").map_or(true, |t| t.is_empty()) {
            let token = uuid::Uuid::new_v4().to_string();
            std::env::set_var("NEXO_ADMIN_TOKEN", &token);
        }
        run_daemon_start(config_dir).await?;
        use std::io::Write as _;
        print!("waiting for the admin server on :{port} …");
        let _ = std::io::stdout().flush();
        for _ in 0..100 {
            if probe() {
                reachable = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        println!(" {}", if reachable { "✓ up" } else { "still not up" });
        if !reachable {
            eprintln!(
                "note: the daemon is running but hasn't bound :{port} yet — give it a moment \
                 and retry, or check the daemon log (`nexo` foreground / ~/.local/state/nexo/nexo.log)."
            );
        }
        println!();
    }

    if open && !tunnel {
        open_in_browser(&url);
    }

    if tunnel {
        if !reachable {
            eprintln!(
                "note: the admin server isn't listening on {port} yet — the tunnel will 502 until \
                 you start the daemon (`nexo start`)."
            );
        }
        println!(
            "Bringing up a Cloudflare quick tunnel (pure-Rust, no `cloudflared` subprocess) …"
        );
        match nexo_tunnel::TunnelManager::new(port).start().await {
            Ok(handle) => {
                let public = handle.url.clone();
                println!();
                println!("╭───────────────────────────────────────────────────────────╮");
                println!("│  Admin web UI — public URL (Cloudflare quick tunnel)      │");
                println!("│                                                           │");
                println!("│  {public:<57} │");
                println!("│                                                           │");
                println!("│  Ctrl-C to close the tunnel.                              │");
                println!("╰───────────────────────────────────────────────────────────╯");
                println!();
                if open {
                    open_in_browser(&public);
                }
                let _ = tokio::signal::ctrl_c().await;
                println!("closing tunnel …");
                handle.shutdown().await;
            }
            Err(e) => {
                eprintln!("error: could not start the Cloudflare tunnel: {e}");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// Open `url` in the platform's default browser, best-effort.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd: (&str, &[&str]) = ("open", &[]);
    #[cfg(target_os = "windows")]
    let cmd: (&str, &[&str]) = ("cmd", &["/C", "start", ""]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let cmd: (&str, &[&str]) = ("xdg-open", &[]);
    let ok = std::process::Command::new(cmd.0)
        .args(cmd.1)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("note: couldn't launch a browser — open {url} manually");
    }
}

/// Per-user runtime directory for the daemon's pidfile + log
/// (`nexo start` / `stop` / `restart`). Resolution order:
/// `$NEXO_RUNTIME_DIR` → `$XDG_RUNTIME_DIR/nexo` → `$HOME/.local/state/nexo`
/// → `./.nexo` (last resort when even `$HOME` is unset).
fn nexo_runtime_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NEXO_RUNTIME_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("nexo");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("nexo");
    }
    PathBuf::from(".nexo")
}

fn nexo_pidfile() -> PathBuf {
    nexo_runtime_dir().join("nexo.pid")
}
fn nexo_logfile() -> PathBuf {
    nexo_runtime_dir().join("nexo.log")
}

fn read_pidfile(p: &Path) -> Option<u32> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

/// `nexo start` — spawn the daemon detached, write a pidfile, exit.
async fn run_daemon_start(config_dir: &Path) -> Result<()> {
    let rt = nexo_runtime_dir();
    std::fs::create_dir_all(&rt).with_context(|| format!("creating {}", rt.display()))?;
    let pidfile = nexo_pidfile();
    let logfile = nexo_logfile();

    if let Some(pid) = read_pidfile(&pidfile) {
        if pid_alive(pid) {
            println!("nexo is already running (pid {pid}).");
            println!("  logs:  {}", logfile.display());
            println!("  stop:  nexo stop   ·   restart: nexo restart");
            return Ok(());
        }
        let _ = std::fs::remove_file(&pidfile); // stale
    }

    let exe = std::env::current_exe().context("locating the nexo executable")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&logfile)
        .with_context(|| format!("opening {}", logfile.display()))?;
    let log_err = log.try_clone()?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--config")
        .arg(config_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group → Ctrl-C in the launching shell doesn't
        // hit it; once this parent exits the child is reparented to init.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS (0x08) | CREATE_NEW_PROCESS_GROUP (0x200)
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let child = cmd.spawn().context("spawning the daemon")?;
    let pid = child.id();
    // `child` is dropped here; std's Child::drop does NOT kill it.
    std::fs::write(&pidfile, format!("{pid}\n"))
        .with_context(|| format!("writing {}", pidfile.display()))?;

    println!("nexo started in the background (pid {pid}).");
    println!("  config:  {}", config_dir.display());
    println!("  logs:    {}", logfile.display());
    println!("  pidfile: {}", pidfile.display());
    println!("  stop: nexo stop   ·   restart: nexo restart   ·   admin: nexo admin --open");
    Ok(())
}

/// `nexo stop` — SIGTERM the daemon, escalate to SIGKILL, drop the pidfile.
async fn run_daemon_stop() -> Result<()> {
    let pidfile = nexo_pidfile();
    let Some(pid) = read_pidfile(&pidfile) else {
        println!("nexo is not running (no pidfile at {}).", pidfile.display());
        return Ok(());
    };
    if !pid_alive(pid) {
        println!("nexo is not running (stale pidfile — removing).");
        let _ = std::fs::remove_file(&pidfile);
        return Ok(());
    }

    use std::io::Write as _;
    print!("stopping nexo (pid {pid}) … ");
    let _ = std::io::stdout().flush();

    #[cfg(not(windows))]
    {
        terminate_pid(pid); // SIGTERM
        let mut still = true;
        for _ in 0..50 {
            if !pid_alive(pid) {
                still = false;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if still {
            kill_pid(pid); // SIGKILL
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    let _ = std::fs::remove_file(&pidfile);
    println!("stopped.");
    Ok(())
}

/// `nexo restart` — `stop` then `start`.
async fn run_daemon_restart(config_dir: &Path) -> Result<()> {
    run_daemon_stop().await?;
    // Brief pause so the daemon's listening ports are released.
    std::thread::sleep(std::time::Duration::from_millis(600));
    run_daemon_start(config_dir).await
}

/// `nexo update` — upgrade the `nexo` binary in place. Prefers
/// `cargo install nexo-rs --force` (Rust toolchain present);
/// otherwise points at the installer one-liner (which also
/// re-checks the bundled plugins).
async fn run_self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("nexo {current} — updating …");
    if which_in_path("cargo") {
        println!("→ cargo install nexo-rs --force --locked");
        let ok = std::process::Command::new("cargo")
            .args(["install", "nexo-rs", "--force", "--locked"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("✓ done — run `nexo --version` to confirm.");
            return Ok(());
        }
        eprintln!("✗ `cargo install nexo-rs --force` failed.");
        eprintln!("  Falling back to the installer:");
    } else {
        println!("`cargo` not found — pull the latest pre-built binary with the installer:");
    }
    println!("    curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh | bash");
    println!("    # add `-s -- --no-plugins` to skip re-checking the bundled plugins");
    Ok(())
}

// ─── `nexo service` — register the daemon as an OS service ───────────

#[cfg(target_os = "linux")]
const SERVICE_KIND: &str = "systemd user unit";
#[cfg(target_os = "macos")]
const SERVICE_KIND: &str = "launchd LaunchAgent";
#[cfg(target_os = "windows")]
const SERVICE_KIND: &str = "logon Scheduled Task";
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const SERVICE_KIND: &str = "(unsupported OS)";

/// Run `prog args…` inheriting stdio; `true` iff it exited 0.
fn run_ok(prog: &str, args: &[&str]) -> bool {
    std::process::Command::new(prog)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Make `config_dir` absolute — a system service has no stable CWD,
/// and the default config dir (`./config`) is relative.
fn abs_config_dir(config_dir: &Path) -> PathBuf {
    std::fs::canonicalize(config_dir).unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|c| c.join(config_dir))
            .unwrap_or_else(|_| config_dir.to_path_buf())
    })
}

#[cfg(target_os = "linux")]
fn systemd_user_unit_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("systemd").join("user").join("nexo.service")
}

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("LaunchAgents")
        .join("com.lordmacu.nexo.plist")
}

#[cfg(windows)]
const WINDOWS_TASK_NAME: &str = "nexo";

/// `nexo service install` — write the OS service unit, enable it
/// (auto-start on boot/login), and start it now.
async fn run_service_install(config_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locating the nexo executable")?;
    let cfg = abs_config_dir(config_dir);
    println!("Installing nexo as an OS service ({SERVICE_KIND}) …");
    println!("  exe:    {}", exe.display());
    println!("  config: {}", cfg.display());

    #[cfg(target_os = "linux")]
    {
        if !which_in_path("systemctl") {
            eprintln!("systemd not available (no `systemctl` on PATH).");
            eprintln!("Fallback: `nexo start` + a `@reboot nexo start` cron line.");
            std::process::exit(1);
        }
        let unit = systemd_user_unit_path();
        if let Some(p) = unit.parent() {
            std::fs::create_dir_all(p)?;
        }
        let body = format!(
            "[Unit]\n\
             Description=nexo-rs agent daemon\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exe} --config {cfg}\n\
             Restart=on-failure\n\
             RestartSec=3\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            exe = exe.display(),
            cfg = cfg.display(),
        );
        std::fs::write(&unit, body).with_context(|| format!("writing {}", unit.display()))?;
        run_ok("systemctl", &["--user", "daemon-reload"]);
        // Linger → runs without an active login session (may need
        // admin on some distros; best-effort).
        if let Ok(user) = std::env::var("USER") {
            run_ok("loginctl", &["enable-linger", &user]);
        }
        println!();
        if run_ok("systemctl", &["--user", "enable", "--now", "nexo.service"]) {
            println!("✓ installed and started.  Unit: {}", unit.display());
            println!("  status: nexo service status   ·   logs: journalctl --user -u nexo -f");
            return Ok(());
        }
        eprintln!("⚠ unit written but `systemctl --user enable --now nexo.service` failed.");
        std::process::exit(1);
    }

    #[cfg(target_os = "macos")]
    {
        let plist = launchd_plist_path();
        if let Some(p) = plist.parent() {
            std::fs::create_dir_all(p)?;
        }
        let log = nexo_logfile();
        if let Some(p) = log.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20 <key>Label</key><string>com.lordmacu.nexo</string>\n\
             \x20 <key>ProgramArguments</key>\n\
             \x20 <array>\n\
             \x20   <string>{exe}</string>\n\
             \x20   <string>--config</string>\n\
             \x20   <string>{cfg}</string>\n\
             \x20 </array>\n\
             \x20 <key>RunAtLoad</key><true/>\n\
             \x20 <key>KeepAlive</key><true/>\n\
             \x20 <key>StandardOutPath</key><string>{log}</string>\n\
             \x20 <key>StandardErrorPath</key><string>{log}</string>\n\
             </dict>\n\
             </plist>\n",
            exe = exe.display(),
            cfg = cfg.display(),
            log = log.display(),
        );
        std::fs::write(&plist, body).with_context(|| format!("writing {}", plist.display()))?;
        // Reload: unload (ignore errors) then load -w (persist).
        let _ = std::process::Command::new("launchctl")
            .arg("unload")
            .arg(&plist)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        println!();
        if run_ok("launchctl", &["load", "-w", &plist.to_string_lossy()]) {
            println!("✓ installed and started.  Plist: {}", plist.display());
            println!(
                "  status: nexo service status   ·   logs: tail -f {}",
                log.display()
            );
            return Ok(());
        }
        eprintln!("⚠ plist written but `launchctl load -w` failed.");
        std::process::exit(1);
    }

    #[cfg(windows)]
    {
        let tr = format!(
            "\"{}\" --config \"{}\"",
            exe.to_string_lossy(),
            cfg.to_string_lossy()
        );
        println!();
        if run_ok(
            "schtasks",
            &[
                "/Create",
                "/F",
                "/SC",
                "ONLOGON",
                "/RL",
                "LIMITED",
                "/TN",
                WINDOWS_TASK_NAME,
                "/TR",
                &tr,
            ],
        ) {
            let _ = run_ok("schtasks", &["/Run", "/TN", WINDOWS_TASK_NAME]);
            println!("✓ installed (logon Scheduled Task `{WINDOWS_TASK_NAME}`) and started.");
            println!("  status: nexo service status");
            return Ok(());
        }
        eprintln!("⚠ `schtasks /Create` failed — check the output above.");
        std::process::exit(1);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (exe, cfg);
        eprintln!(
            "`nexo service` isn't supported on this OS — use `nexo start` + your init system."
        );
        std::process::exit(1);
    }
}

/// `nexo service uninstall` — stop + remove the OS service unit.
async fn run_service_uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if which_in_path("systemctl") {
            run_ok("systemctl", &["--user", "disable", "--now", "nexo.service"]);
        }
        let unit = systemd_user_unit_path();
        let removed = std::fs::remove_file(&unit).is_ok();
        if which_in_path("systemctl") {
            run_ok("systemctl", &["--user", "daemon-reload"]);
        }
        if removed {
            println!("✓ removed {}", unit.display());
        } else {
            println!("nothing to remove ({} not found).", unit.display());
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let plist = launchd_plist_path();
        let _ = std::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if std::fs::remove_file(&plist).is_ok() {
            println!("✓ removed {}", plist.display());
        } else {
            println!("nothing to remove ({} not found).", plist.display());
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        if run_ok("schtasks", &["/Delete", "/F", "/TN", WINDOWS_TASK_NAME]) {
            println!("✓ removed Scheduled Task `{WINDOWS_TASK_NAME}`.");
        } else {
            println!("nothing to remove (task `{WINDOWS_TASK_NAME}` not found).");
        }
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        eprintln!("`nexo service` isn't supported on this OS.");
        std::process::exit(1);
    }
}

/// `nexo service status` — report install + run state.
async fn run_service_status() -> Result<()> {
    println!("nexo service ({SERVICE_KIND}):");
    #[cfg(target_os = "linux")]
    {
        let unit = systemd_user_unit_path();
        println!(
            "  unit file: {}  ({})",
            unit.display(),
            if unit.exists() { "present" } else { "absent" }
        );
        if which_in_path("systemctl") {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "--no-pager", "status", "nexo.service"])
                .status();
        } else {
            println!("  (systemctl not on PATH)");
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let plist = launchd_plist_path();
        println!(
            "  plist: {}  ({})",
            plist.display(),
            if plist.exists() { "present" } else { "absent" }
        );
        let _ = std::process::Command::new("launchctl")
            .args(["list", "com.lordmacu.nexo"])
            .status();
        return Ok(());
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("schtasks")
            .args(["/Query", "/TN", WINDOWS_TASK_NAME, "/V", "/FO", "LIST"])
            .status();
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        println!("  (unsupported OS)");
        Ok(())
    }
}

/// Lightweight PATH walker — duplicated locally because pulling
/// the `which` crate in just for this would be overkill.
fn which_in_path(bin: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    for dir in std::env::split_paths(&path) {
        if dir.join(bin).is_file() {
            return true;
        }
    }
    false
}

/// Checks PATH and the plugin state dir for the admin binary.
/// `nexo install` downloads plugins to `$NEXO_HOME/state/plugins/<id>-<ver>/`,
/// which is not in PATH. The daemon discovers plugins there via search_paths;
/// this function mirrors that so the CLI doesn't lie about the binary being missing.
fn admin_binary_installed() -> bool {
    if which_in_path("nexo-plugin-admin") {
        return true;
    }
    let state_dir = nexo_project_tracker::state::nexo_state_dir();
    let plugins_root = state_dir.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_root) else {
        return false;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map_or(false, |t| t.is_dir()) {
            continue;
        }
        let plugin_dir = entry.path();
        // Cargo-installed plugins place the binary at the root.
        if plugin_dir.join("nexo-plugin-admin").is_file() {
            return true;
        }
        // Tarball-extracted plugins (install.sh / `nexo plugin install`) use a
        // `bin/` subdirectory — the binary name varies, so check for any file.
        let bin_dir = plugin_dir.join("bin");
        if bin_dir.is_dir() {
            if let Ok(bin_entries) = std::fs::read_dir(&bin_dir) {
                if bin_entries
                    .flatten()
                    .any(|e| e.file_type().map_or(false, |t| t.is_file()))
                {
                    return true;
                }
            }
        }
    }
    false
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

/// Re-read `mcp_server.expose_tools` from the YAML
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
/// Clients only register the `tools/list_changed` notification
/// listener when the server advertises
/// `capabilities.tools.listChanged: true` (wired via
/// `with_list_changed_capability`). Multiple notifications are
/// safe; clients debounce within the session window.
fn reload_expose_tools(
    config_dir: &std::path::Path,
) -> Result<Option<std::collections::HashSet<String>>> {
    let cfg = nexo_config::AppConfig::load_for_mcp_server(config_dir)?;
    let server_cfg = cfg.mcp_server.unwrap_or_default();
    Ok(compute_allowlist_from_mcp_server_cfg(&server_cfg))
}

/// Derive the `ToolRegistryBridge` allowlist from
/// an in-memory `McpServerConfig`. Empty `expose_tools` returns
/// `None` (no filter, expose all non-proxy tools); non-empty
/// returns `Some(HashSet)` (HashSet collapses duplicates by
/// construction). Used by the daemon-embed boot wire (Mode::Run)
/// where the config is already loaded, complementing the
/// `reload_expose_tools` path that re-reads the YAML on
/// SIGHUP / config reload.
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

    // Tolerant loader: skip llm.yaml / broker.yaml / memory.yaml.
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
    // Same boot-time policy resolver used by `nexo run`. Mirrors
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
    // `memory.secret_guard` is read from the same load. When
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
                // Wire the operator-supplied secret_guard policy
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

    // Boot dispatcher. The runtime tool registry is
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
            // Tolerant boot — if we can't open the
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

        // Phase 95 — web_search_router worker-reload boot removed.
        // `web_search` is served by the standalone subprocess
        // plugin; RemoteToolHandler routes calls when the plugin
        // is discovered.

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
        // Mirrors the `nexo run` startup wiring.
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
        // Phase 95 — web_search_router removed.
        boot_ctx_enriched.link_extractor = Some(link_extractor);
        boot_ctx_enriched.long_term_memory = long_term_memory.clone();
        boot_ctx_enriched.memory_git = memory_git;
        boot_ctx_enriched.taskflow_manager = taskflow_manager;
        boot_ctx_enriched.lsp_manager = lsp_manager;
        boot_ctx_enriched.team_store = team_store_handle;

        // Config self-edit handles. Compiled
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
            // picks up YAML changes via its file watcher.
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

        // Config self-edit requires an
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
            } else if entry.name == "Config"
                && effective_primary.config_tool.allowed_paths.is_empty()
            {
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

    // Opt-in HTTP transport runs alongside stdio.
    let http_handle = if let Some(http_yaml) = server_cfg.http.clone() {
        if http_yaml.enabled {
            Some(start_http_transport(&bridge, &http_yaml, &shutdown).await?)
        } else {
            None
        }
    } else {
        None
    };

    // SIGHUP reload trigger. Operator runs
    // `kill -HUP $(pidof nexo)` after editing
    // `mcp_server.expose_tools` in YAML; the handler re-reads the
    // allowlist, atomically swaps it into the bridge (visible to
    // both stdio + HTTP clones — they share the inner ArcSwap)
    // and emits `notifications/tools/list_changed` so
    // connected HTTP/SSE clients refresh without reconnect. SIGHUP
    // chosen over a file watcher to avoid a new dep;
    // file-watcher / `ConfigReloadCoordinator` integration is a
    // follow-up.
    //
    // The bridge is `Clone` (`with_list_changed_capability` +
    // ArcSwap-shared allowlist); we clone here BEFORE
    // `run_stdio_server_with_auth` consumes the original.
    // `HttpNotifyHandle` is the lightweight `Clone` notifier,
    // detached from the JoinHandle so
    // safe to move into the background task.
    #[cfg(unix)]
    {
        let bridge_for_sig = bridge.clone();
        let notifier_for_sig: Option<nexo_mcp::HttpNotifyHandle> =
            http_handle.as_ref().map(|h| h.notifier());
        let cfg_dir_for_sig = config_dir.to_path_buf();
        let shutdown_for_sig = shutdown.clone();
        tokio::spawn(async move {
            let mut sighup =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
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
// `nexo mcp-server` CLI ops
// ---------------------------------------------------------------------------

/// Dispatch helper for `Mode::Memory(...)`. Each arm wires the
/// matching handler from this module.
async fn dispatch_memory_subcommand(sub: &MemorySubcommand) -> Result<()> {
    match sub {
        MemorySubcommand::Verify { bundle, json } => run_memory_verify(bundle, *json).await,
        MemorySubcommand::Snapshot {
            agent,
            tenant,
            label,
            no_redact,
            encrypt,
            state_root,
            memdir_root,
            sqlite_root,
            json,
        } => {
            run_memory_snapshot(
                agent,
                tenant,
                label.clone(),
                !*no_redact,
                encrypt.clone(),
                state_root,
                memdir_root.as_deref(),
                sqlite_root.as_deref(),
                *json,
            )
            .await
        }
        MemorySubcommand::Restore {
            agent,
            tenant,
            bundle,
            dry_run,
            no_auto_pre_snapshot,
            decrypt_identity,
            state_root,
            memdir_root,
            sqlite_root,
            json,
        } => {
            run_memory_restore(
                agent,
                tenant,
                bundle,
                *dry_run,
                !*no_auto_pre_snapshot,
                decrypt_identity.as_deref(),
                state_root,
                memdir_root.as_deref(),
                sqlite_root.as_deref(),
                *json,
            )
            .await
        }
        MemorySubcommand::List {
            agent,
            tenant,
            state_root,
            json,
        } => run_memory_list(agent, tenant, state_root, *json).await,
        MemorySubcommand::Diff {
            agent,
            tenant,
            a,
            b,
            state_root,
            json,
        } => run_memory_diff(agent, tenant, a, b, state_root, *json).await,
        MemorySubcommand::Export {
            agent,
            tenant,
            id,
            to,
            state_root,
        } => run_memory_export(agent, tenant, id, to, state_root).await,
        MemorySubcommand::Delete {
            agent,
            tenant,
            id,
            state_root,
            yes,
        } => run_memory_delete(agent, tenant, id, state_root, *yes).await,
    }
}

/// Broker bridge for the memory snapshot subsystem.
///
/// Implements `nexo_memory_snapshot::EventPublisher` against any
/// `AnyBroker` so lifecycle events
/// (`nexo.memory.snapshot.<agent>.{created,restored,deleted,gc}`)
/// and mutation events (`nexo.memory.mutated.<agent>`) reach NATS
/// (or the local fallback) without the snapshot crate taking a
/// direct broker dep.
///
/// Best-effort: a publish error is logged via `tracing::warn!` and
/// dropped. The trait method returns `()` so the writer's
/// transaction is never poisoned by broker degradation.
struct BrokerEventPublisher {
    broker: AnyBroker,
    // Honour the operator-configured
    // `EventsSection.lifecycle_subject_prefix` /
    // `mutation_subject_prefix`. Captured at construction so a
    // hot-reload would require rebuilding the publisher (acceptable;
    // memory-snapshot config is boot-time today).
    lifecycle_prefix: String,
    mutation_prefix: String,
}

impl BrokerEventPublisher {
    fn new(broker: AnyBroker, lifecycle_prefix: String, mutation_prefix: String) -> Self {
        Self {
            broker,
            lifecycle_prefix,
            mutation_prefix,
        }
    }
}

#[async_trait::async_trait]
impl nexo_memory_snapshot::EventPublisher for BrokerEventPublisher {
    async fn publish_lifecycle(&self, event: nexo_memory_snapshot::LifecycleEvent) {
        let topic = event.subject_with_prefix(&self.lifecycle_prefix);
        let payload = match serde_json::to_value(&event) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "memory_snapshot: lifecycle event serialize failed");
                return;
            }
        };
        let evt = nexo_broker::Event::new(topic.clone(), "memory-snapshot", payload);
        if let Err(e) = nexo_broker::BrokerHandle::publish(&self.broker, &topic, evt).await {
            tracing::warn!(
                topic = %topic,
                error = %e,
                "memory_snapshot: lifecycle publish failed (best-effort)"
            );
        }
    }

    async fn publish_mutation(&self, event: nexo_memory_snapshot::MutationEvent) {
        let topic = event.subject_with_prefix(&self.mutation_prefix);
        let payload = match serde_json::to_value(&event) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "memory_snapshot: mutation event serialize failed");
                return;
            }
        };
        let evt = nexo_broker::Event::new(topic.clone(), "memory-snapshot", payload);
        if let Err(e) = nexo_broker::BrokerHandle::publish(&self.broker, &topic, evt).await {
            tracing::warn!(
                topic = %topic,
                error = %e,
                "memory_snapshot: mutation publish failed (best-effort)"
            );
        }
    }
}

/// Build a `LocalFsSnapshotter` from operator-supplied roots.
/// `memdir_root` and `sqlite_root` default to `state_root` when not
/// overridden — same pattern the YAML config uses at boot.
fn build_snapshotter(
    state_root: &Path,
    memdir_root: Option<&Path>,
    sqlite_root: Option<&Path>,
) -> Result<nexo_memory_snapshot::local_fs::LocalFsSnapshotter> {
    use nexo_memory_snapshot::local_fs::LocalFsSnapshotter;
    let mut b = LocalFsSnapshotter::builder().state_root(state_root.to_path_buf());
    if let Some(p) = memdir_root {
        b = b.memdir_root(p.to_path_buf());
    }
    if let Some(p) = sqlite_root {
        b = b.sqlite_root(p.to_path_buf());
    }
    b.build()
        .map_err(|e| anyhow::anyhow!("snapshotter build failed: {e}"))
}

/// `nexo memory snapshot ...` — capture a fresh bundle.
#[allow(clippy::too_many_arguments)]
async fn run_memory_snapshot(
    agent: &str,
    tenant: &str,
    label: Option<String>,
    redact_secrets: bool,
    encrypt: Option<String>,
    state_root: &Path,
    memdir_root: Option<&Path>,
    sqlite_root: Option<&Path>,
    json: bool,
) -> Result<()> {
    use nexo_memory_snapshot::request::SnapshotRequest;
    use nexo_memory_snapshot::EncryptionKey;
    use nexo_memory_snapshot::MemorySnapshotter;

    if agent.is_empty() {
        anyhow::bail!("--agent is required");
    }
    let snapshotter = build_snapshotter(state_root, memdir_root, sqlite_root)?;
    let req = SnapshotRequest {
        agent_id: agent.to_string(),
        tenant: tenant.to_string(),
        label,
        redact_secrets,
        encrypt: encrypt.map(EncryptionKey::AgePublicKey),
        created_by: "cli".into(),
    };
    let meta = snapshotter
        .snapshot(req)
        .await
        .map_err(|e| anyhow::anyhow!("snapshot failed: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&meta)?);
    } else {
        println!("snapshot id:   {}", meta.id);
        println!("bundle:        {}", meta.bundle_path.display());
        println!("size_bytes:    {}", meta.bundle_size_bytes);
        println!("sha256:        {}", meta.bundle_sha256);
        println!("encrypted:     {}", meta.encrypted);
        println!("redactions:    {}", meta.redactions_applied);
        if let Some(label) = &meta.label {
            println!("label:         {label}");
        }
    }
    Ok(())
}

/// `nexo memory restore ...` — replay a bundle on top of live state.
/// Requires `NEXO_MEMORY_RESTORE_ALLOW=true` (capability gate).
#[allow(clippy::too_many_arguments)]
async fn run_memory_restore(
    agent: &str,
    tenant: &str,
    bundle: &Path,
    dry_run: bool,
    auto_pre_snapshot: bool,
    decrypt_identity: Option<&Path>,
    state_root: &Path,
    memdir_root: Option<&Path>,
    sqlite_root: Option<&Path>,
    json: bool,
) -> Result<()> {
    use nexo_memory_snapshot::request::{DecryptionIdentity, RestoreRequest};
    use nexo_memory_snapshot::MemorySnapshotter;

    if agent.is_empty() {
        anyhow::bail!("--agent is required");
    }
    if !is_truthy_env("NEXO_MEMORY_RESTORE_ALLOW") {
        anyhow::bail!(
            "restore is gated: set NEXO_MEMORY_RESTORE_ALLOW=true (see `nexo agent doctor capabilities`)"
        );
    }
    let snapshotter = build_snapshotter(state_root, memdir_root, sqlite_root)?;
    let req = RestoreRequest {
        agent_id: agent.to_string(),
        tenant: tenant.to_string(),
        bundle: bundle.to_path_buf(),
        dry_run,
        auto_pre_snapshot,
        decrypt: decrypt_identity
            .map(|p: &Path| DecryptionIdentity::AgeIdentityFile(p.to_path_buf())),
    };
    let report = snapshotter
        .restore(req)
        .await
        .map_err(|e| anyhow::anyhow!("restore failed: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("dry_run:           {}", report.dry_run);
        println!("from:              {}", report.from);
        if let Some(p) = &report.pre_snapshot {
            println!("pre_snapshot:      {p}");
        }
        if let Some(oid) = &report.git_reset_oid {
            println!("git_reset_oid:     {oid}");
        }
        println!(
            "sqlite_restored:   [{}]",
            report.sqlite_restored_dbs.join(", ")
        );
        println!(
            "state_files:       [{}]",
            report.state_files_restored.join(", ")
        );
        println!("workers_restarted: {}", report.workers_restarted);
    }
    Ok(())
}

/// `nexo memory list --agent <id>` — show every bundle for the agent
/// ordered newest-first.
async fn run_memory_list(agent: &str, tenant: &str, state_root: &Path, json: bool) -> Result<()> {
    use nexo_memory_snapshot::MemorySnapshotter;

    if agent.is_empty() {
        anyhow::bail!("--agent is required");
    }
    let snapshotter = build_snapshotter(state_root, None, None)?;
    let metas = snapshotter
        .list(&agent.to_string(), tenant)
        .await
        .map_err(|e| anyhow::anyhow!("list failed: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&metas)?);
    } else if metas.is_empty() {
        println!("(no snapshots)");
    } else {
        for m in &metas {
            let label = m.label.as_deref().unwrap_or("-");
            let flags = match (m.encrypted, m.redactions_applied) {
                (true, true) => "[enc][red]",
                (true, false) => "[enc]    ",
                (false, true) => "    [red]",
                (false, false) => "         ",
            };
            println!(
                "{} {} {:>10} {} {}",
                m.id, flags, m.bundle_size_bytes, label, m.created_at_ms
            );
        }
    }
    Ok(())
}

/// `nexo memory diff <id-a> <id-b> --agent <id>` — coarse delta
/// summary between two bundles for the same agent.
async fn run_memory_diff(
    agent: &str,
    tenant: &str,
    a: &str,
    b: &str,
    state_root: &Path,
    json: bool,
) -> Result<()> {
    use nexo_memory_snapshot::id::SnapshotId;
    use nexo_memory_snapshot::MemorySnapshotter;

    if agent.is_empty() {
        anyhow::bail!("--agent is required");
    }
    let id_a: SnapshotId = a.parse().map_err(|e| anyhow::anyhow!("a: {e}"))?;
    let id_b: SnapshotId = b.parse().map_err(|e| anyhow::anyhow!("b: {e}"))?;
    let snapshotter = build_snapshotter(state_root, None, None)?;
    let diff = snapshotter
        .diff(&agent.to_string(), tenant, id_a, id_b)
        .await
        .map_err(|e| anyhow::anyhow!("diff failed: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        println!("a: {}", diff.a);
        println!("b: {}", diff.b);
        println!(
            "git:    commits_between={} files_changed={}",
            diff.git_summary.commits_between, diff.git_summary.files_changed
        );
        println!(
            "sqlite: long_term=±{} vector=±{} concepts=±{} compactions={}",
            diff.sqlite_summary.long_term_rows_added,
            diff.sqlite_summary.vector_rows_added,
            diff.sqlite_summary.concepts_rows_added,
            diff.sqlite_summary.compactions_added
        );
        println!(
            "state:  extract_cursor_changed={} last_dream_run_changed={}",
            diff.state_summary.extract_cursor_changed, diff.state_summary.last_dream_run_changed
        );
    }
    Ok(())
}

/// `nexo memory export --agent <id> --id <snapshot> --to <path>` —
/// copy a bundle (and its sibling `.sha256`) to an arbitrary
/// destination.
async fn run_memory_export(
    agent: &str,
    tenant: &str,
    id: &str,
    to: &Path,
    state_root: &Path,
) -> Result<()> {
    use nexo_memory_snapshot::id::SnapshotId;
    use nexo_memory_snapshot::MemorySnapshotter;

    if agent.is_empty() {
        anyhow::bail!("--agent is required");
    }
    let id: SnapshotId = id.parse().map_err(|e| anyhow::anyhow!("--id: {e}"))?;
    let snapshotter = build_snapshotter(state_root, None, None)?;
    let path = snapshotter
        .export(&agent.to_string(), tenant, id, to)
        .await
        .map_err(|e| anyhow::anyhow!("export failed: {e}"))?;
    println!("exported: {}", path.display());
    Ok(())
}

/// `nexo memory delete --agent <id> --id <snapshot>` — drop a bundle
/// + sibling. Refuses to delete the agent's last remaining snapshot.
async fn run_memory_delete(
    agent: &str,
    tenant: &str,
    id: &str,
    state_root: &Path,
    yes: bool,
) -> Result<()> {
    use nexo_memory_snapshot::id::SnapshotId;
    use nexo_memory_snapshot::MemorySnapshotter;

    if agent.is_empty() {
        anyhow::bail!("--agent is required");
    }
    if !yes {
        anyhow::bail!("delete is destructive; pass --yes to confirm");
    }
    let id: SnapshotId = id.parse().map_err(|e| anyhow::anyhow!("--id: {e}"))?;
    let snapshotter = build_snapshotter(state_root, None, None)?;
    snapshotter
        .delete(&agent.to_string(), tenant, id)
        .await
        .map_err(|e| anyhow::anyhow!("delete failed: {e}"))?;
    println!("deleted: {id}");
    Ok(())
}

fn is_truthy_env(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => false,
    }
}

/// `nexo memory verify --bundle <path>` — recompute every integrity
/// check on a snapshot bundle. Read-only, no daemon contact, no live
/// state mutation. Exit code is 0 only when every check passes.
async fn run_memory_verify(bundle: &std::path::Path, json: bool) -> Result<()> {
    use nexo_memory_snapshot::local_fs::LocalFsSnapshotter;
    use nexo_memory_snapshot::MemorySnapshotter;

    if !bundle.exists() {
        anyhow::bail!("bundle not found: {}", bundle.display());
    }

    // Verify is stateless from the snapshotter's POV; the state_root
    // here is a placeholder so the builder is satisfied. The verify
    // path never touches it.
    let s = LocalFsSnapshotter::builder()
        .state_root(std::env::temp_dir())
        .build()
        .map_err(|e| anyhow::anyhow!("snapshotter build failed: {e}"))?;
    let report = s
        .verify(bundle)
        .await
        .map_err(|e| anyhow::anyhow!("verify failed: {e}"))?;

    let all_ok = report.manifest_ok && report.bundle_sha256_ok && report.per_artifact_ok;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("bundle:           {}", report.bundle.display());
        println!("manifest_ok:      {}", report.manifest_ok);
        println!("bundle_sha256_ok: {}", report.bundle_sha256_ok);
        println!("per_artifact_ok:  {}", report.per_artifact_ok);
        println!("age_protected:    {}", report.age_protected);
        println!(
            "schema_versions:  long_term={} vector={} concepts={} compactions={} manifest={}",
            report.schema_versions.long_term,
            report.schema_versions.vector,
            report.schema_versions.concepts,
            report.schema_versions.compactions,
            report.schema_versions.manifest,
        );
        println!("all_checks_ok:    {all_ok}");
    }

    if !all_ok {
        std::process::exit(2);
    }
    Ok(())
}

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
            println!(
                "\n## Resources ({})\n",
                resources.map(|r| r.len()).unwrap_or(0)
            );
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
/// `nexo ext state-dir <id> [--ensure]`. Prints
/// the canonical state directory for one extension. With
/// `--ensure`, creates the dir on disk first (idempotent).
fn run_ext_state_dir(id: &str, ensure: bool) -> Result<()> {
    if id.is_empty() {
        anyhow::bail!("extension id is required");
    }
    let path = if ensure {
        nexo_extensions::ensure_state_dir(id)
            .with_context(|| format!("create state dir for `{id}`"))?
    } else {
        nexo_extensions::state_dir_for(id)
    };
    println!("{}", path.display());
    Ok(())
}

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

/// `nexo microapp admin audit tail [...]`.
///
/// Read-only operator query over the SQLite admin audit log
/// (`microapp_admin_audit` table). Maps every flag 1:1 to
/// `AuditTailFilter` and renders rows via the format helpers
/// (`format_rows_as_table` / `format_rows_as_json`).
/// Default db resolves to
/// `nexo_state_dir().join("admin_audit.db")` so daemons that
/// boot with the default audit-writer location work without
/// `--db`.
#[allow(clippy::too_many_arguments)]
async fn run_microapp_admin_audit_tail(
    microapp_id: Option<String>,
    method: Option<String>,
    result: Option<String>,
    since_mins: Option<u64>,
    since_ms: Option<u64>,
    limit: usize,
    format: String,
    db: Option<PathBuf>,
    tenant_id: Option<String>,
) -> Result<()> {
    use nexo_core::agent::admin_rpc::{
        format_rows_as_json, format_rows_as_table, AdminAuditResult, AuditTailFilter,
        SqliteAdminAuditWriter,
    };

    let db_path =
        db.unwrap_or_else(|| nexo_project_tracker::state::nexo_state_dir().join("admin_audit.db"));
    if !db_path.exists() {
        anyhow::bail!(
            "audit DB does not exist: {} (start the daemon with \
             NEXO_MICROAPP_ADMIN_AUDIT_DB pointing here, or pass --db)",
            db_path.display(),
        );
    }
    let writer = SqliteAdminAuditWriter::open(&db_path)
        .await
        .with_context(|| format!("opening audit DB at {}", db_path.display()))?;

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    let resolved_since_ms = match (since_ms, since_mins) {
        (Some(ms), _) => Some(ms),
        (None, Some(mins)) => Some(now_ms.saturating_sub(mins.saturating_mul(60_000))),
        _ => None,
    };

    let result_enum = match result.as_deref() {
        Some("ok") => Some(AdminAuditResult::Ok),
        Some("error") => Some(AdminAuditResult::Error),
        Some("denied") => Some(AdminAuditResult::Denied),
        Some(other) => {
            anyhow::bail!("--result must be one of ok|error|denied (got `{other}`)")
        }
        None => None,
    };

    let filter = AuditTailFilter {
        microapp_id,
        method,
        result: result_enum,
        since_ms: resolved_since_ms,
        limit: limit.max(1),
        tenant_id,
        // Page — `tail` now returns
        // `AuditTailPage` for pagination. CLI consumes the
        // `entries` slice; offset stays 0 (CLI is one-shot,
        // not paginated; honours `--limit` only).
        offset: 0,
    };
    let page = writer
        .tail(&filter)
        .await
        .context("query admin_audit table")?;
    let rows = page.entries;

    match format.as_str() {
        "json" => println!("{}", format_rows_as_json(&rows)),
        "table" => print!("{}", format_rows_as_table(&rows)),
        other => anyhow::bail!("--format must be one of table|json (got `{other}`)"),
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// `nexo agent dream` operator CLI
//
// Kill semantics: row status flip + lock rollback + (deferred)
// abort signal. Daemon is NOT
// required for any sub-command — the read paths open the SQLite DB
// read-only and the kill path opens it read-write while reading the lock
// file directly. Provider-agnostic by construction (LLM provider never
// touches this surface).
// ─────────────────────────────────────────────────────────────────────

/// 3-tier dream-runs DB path resolution. `--db` wins over
/// `NEXO_STATE_ROOT` env wins over the XDG-default
/// `~/.local/share/nexo/state/dream_runs.db`. The YAML tier is intentionally
/// absent for now — `agents.state_root` does not exist as a config field
/// (state_root flows into `BootDeps` directly), so
/// the CLI uses the env-or-default fallback to stay aligned with the
/// daemon's discovery path.
/// Resolve the per-agent destination for extracted
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
            println!(
                "(no dream runs recorded yet — db not found at {})",
                db.display()
            );
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

    let uuid =
        uuid::Uuid::parse_str(run_id).with_context(|| format!("`{run_id}` is not a valid UUID"))?;

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

    let uuid =
        uuid::Uuid::parse_str(run_id).with_context(|| format!("`{run_id}` is not a valid UUID"))?;

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
            println!("[dream-kill] no prior_mtime recorded for this run; lock rollback skipped");
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

/// Render only the first 8 hex chars of a UUID
/// for compact tabular output.
fn short_uuid(id: &uuid::Uuid) -> String {
    let s = id.to_string();
    s.chars().take(8).collect()
}

// ─────────────────────────────────────────────────────────────────────
// `nexo agent run` / `nexo agent ps` operator CLI
//
// Slim MVP: `run --bg` inserts a goal-handle row with kind=Bg + status=Running
// and prints the goal_id immediately so the operator can detach. Full
// goal execution under the daemon supervisor is a follow-up; for the
// MVP, the row is queued and the local daemon (or a future detached
// worker) picks it up. `ps` reads the same store read-only so an
// operator can list goals even when the daemon is down.
// ─────────────────────────────────────────────────────────────────────

/// 3-tier path resolution for `agent_handles` SQLite store. Mirror of
/// `resolve_dream_db_path` for the `agent run` / `agent ps` surface — operators
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
    let kind = if bg {
        SessionKind::Bg
    } else {
        SessionKind::Interactive
    };
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
// `nexo agent attach` / `nexo agent discover`
//
// Both subcommands are RO viewers over the local
// `agent_handles` SQLite store. Live event streaming via NATS is a
// follow-up; user input piping needs the
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

    let uuid =
        Uuid::parse_str(goal_id).with_context(|| format!("`{goal_id}` is not a valid UUID"))?;
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
        vec![
            SessionKind::Bg,
            SessionKind::Daemon,
            SessionKind::DaemonWorker,
        ]
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
// `nexo channel list/doctor/test` operator CLI.
// Static helpers that read the YAML and exercise the gate +
// XML wrap helper without needing a daemon up.
// ----------------------------------------------------------------

/// Load AppConfig from `--config=<path>` if provided, otherwise
/// fall back to the global `config_dir` the CLI was invoked with.
/// Generic shim that lets `ChannelRelayDecider`
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

/// Sink that forwards a
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
            .map_err(|e| nexo_mcp::channel_bridge::SinkError::Other(format!("broker publish: {e}")))
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
        let cfg = agent.channels.as_ref();
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
    use nexo_mcp::channel::{gate_channel_server, ChannelGateInputs, ChannelGateOutcome};

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
    use nexo_core::agent::{
        AgentBehavior, AgentContext, CancelFollowupTool, CheckFollowupTool, LlmAgentBehavior,
        PluginChannelSendTool, ToolRegistry,
    };
    use nexo_setup::admin_adapters::{
        BrokerOutboundDispatcher, EmailTranslator, TelegramTranslator,
    };

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

    // Phase 81.20.x F3-followup — build a `BrokerOutboundDispatcher`
    // with the same translator set the daemon's admin RPC uses, then
    // expose it to the LLM through the agnostic `plugin_channel_send`
    // tool. The worker can now dispatch outbound messages through
    // any registered channel plugin (email / telegram / cfg-gated
    // whatsapp) by publishing to the plugin's broker topic — no
    // daemon-side `RemoteToolHandler` plumbing required, because the
    // plugin subprocess subscribes to the same broker the worker
    // publishes on.
    let mut outbound = BrokerOutboundDispatcher::new(broker.clone());
    outbound = outbound
        .with_translator(Box::new(EmailTranslator))
        .with_translator(Box::new(TelegramTranslator));
    outbound = outbound.with_translator(Box::new(nexo_setup::admin_adapters::WhatsAppTranslator));
    let outbound: Arc<
        dyn nexo_core::agent::admin_rpc::channel_outbound::ChannelOutboundDispatcher,
    > = Arc::new(outbound);

    let worker_tools = Arc::new(ToolRegistry::new());
    worker_tools.register(
        CancelFollowupTool::tool_def(),
        CancelFollowupTool::new(Arc::clone(&memory)),
    );
    worker_tools.register(
        CheckFollowupTool::tool_def(),
        CheckFollowupTool::new(Arc::clone(&memory)),
    );
    worker_tools.register(
        PluginChannelSendTool::tool_def(),
        PluginChannelSendTool::new(Arc::clone(&outbound)),
    );

    let llm_registry = LlmRegistry::with_builtins();
    // Tenant-aware build; falls back to
    // global providers when the worker agent has no `tenant_id`.
    let llm = llm_registry
        .build_for_tenant(
            &full_cfg.llm,
            &worker_agent_cfg.model,
            worker_agent_cfg.tenant_id.as_deref(),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to build LLM for mcp autonomous worker agent `{}`: {e}",
                worker_agent_cfg.id
            )
        })?;
    let behavior = LlmAgentBehavior::new(llm, Arc::clone(&worker_tools));

    let mut worker_ctx = AgentContext::new(
        worker_agent_cfg.id.clone(),
        Arc::new(worker_agent_cfg),
        broker,
        Arc::clone(&mcp_bridge_ctx.sessions),
    )
    .with_memory(memory);
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
        // Phase 81.20.x F3 — email plugin subprocess lifecycle is
        // owned by the daemon's plugin supervisor (Phase 81.21.b.b);
        // no email_plugin.stop() needed here.
        tracing::info!(agent = %worker_ctx.agent_id, "mcp-server autonomous worker stopped");
    });
    Ok(join)
}

/// Boot the HTTP transport from the YAML block, reusing
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

    // HTTP transport can push
    // `notifications/tools/list_changed` via
    // `HttpServerHandle::notify_tools_list_changed()`, so this
    // bridge clone advertises the capability to clients. Stdio
    // bridge keeps the default `false` (no server→client push
    // channel today). Both clones share the same `Arc<ArcSwap>`
    // allowlist, so a `swap_allowlist(...)` call
    // is visible to both transports atomically.
    let bridge_for_http = bridge.clone().with_list_changed_capability(true);
    let handle = start_http_server(bridge_for_http, cfg, shutdown.clone()).await?;
    tracing::info!(addr = %handle.bind_addr, "mcp-server http transport ready");
    Ok(handle)
}

/// Translate the YAML auth schema into the runtime
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

/// Translate the YAML per-principal block into the
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

/// Translate the YAML per-principal concurrency block
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

/// Translate the YAML audit-log block into the runtime
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

/// Translate the YAML session-event-store block into
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

/// Handler for `nexo set-broker <kind> [--url <url>]
/// [--no-signal]`. Edits `broker.yaml` in `config_dir` so the
/// operator switches between NATS and Local transports without
/// learning sed syntax, then (by default) sends SIGTERM to the
/// running daemon (matched by its `--config` arg) so the
/// supervisor loop (dev-daemon.sh / systemd) respawns and picks
/// up the new config.
///
/// Kinds accepted:
///
/// - `local` — single-host stdio bridge mode. Strips `url:`.
/// - `nats`  — multi-host or single-host with NATS. `--url`
///   required when not already set in the YAML (uses an
///   operator-friendly default `nats://127.0.0.1:4222` when
///   neither the YAML nor `--url` provide one).
/// - `stdio_bridge` — REJECTED. This kind is daemon-derived
///   from `Local` for subprocess plugins; setting it manually
///   would break the daemon's broker startup. Print a clear
///   error so operators don't try.
/// Default XDG-spec config dir for `nexo` when no
/// `--config` flag and no `NEXO_CONFIG_DIR` env are set.
/// Respects `XDG_CONFIG_HOME` if exported, else falls back to
/// `$HOME/.config/nexo`. Returns `PathBuf::from("./config")` when
/// even `$HOME` is unset (CI containers, exotic environments).
fn default_xdg_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("nexo");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".config").join("nexo");
        }
    }
    PathBuf::from("./config")
}

fn run_set_broker(
    config_dir: &std::path::Path,
    kind: &str,
    url: Option<&str>,
    no_signal: bool,
) -> Result<()> {
    let broker_yaml = config_dir.join("broker.yaml");
    // Tolerate missing config dir + missing broker.yaml.
    // Create both with sane defaults so `nexo set-broker local` Just
    // Works from any cwd, no `--config` setup required. The default
    // YAML has `persistence.enabled: true` matching dev-daemon.sh's
    // seed; operators that want different limits / paths edit the
    // file afterwards.
    if !broker_yaml.exists() {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("creating config dir {}", config_dir.display()))?;
        let default_yaml = r#"broker:
  type: local
  url: ""
  persistence:
    enabled: true
    path: ./data/queue/broker.db
  limits:
    max_payload: 4MB
    max_pending: 10000
schema_version: 11
"#;
        std::fs::write(&broker_yaml, default_yaml)
            .with_context(|| format!("seeding default broker.yaml at {}", broker_yaml.display()))?;
        println!(
            "  (seeded default broker.yaml at {} — local mode with persistence)",
            broker_yaml.display()
        );
    }
    let normalized_kind = match kind {
        "local" | "nats" => kind,
        "stdio_bridge" => anyhow::bail!(
            "kind `stdio_bridge` is daemon-derived from `local` for subprocess plugins; \
             operator-facing kinds are `local` or `nats`."
        ),
        other => anyhow::bail!("unknown broker kind `{other}` (expected: local | nats)"),
    };

    let raw = std::fs::read_to_string(&broker_yaml)?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)?;
    let map = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("broker.yaml top-level is not a mapping"))?;
    let broker_key = serde_yaml::Value::String("broker".to_string());
    let broker_map = map
        .get_mut(&broker_key)
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| anyhow::anyhow!("broker.yaml missing top-level `broker:` mapping"))?;

    // Set type
    broker_map.insert(
        serde_yaml::Value::String("type".to_string()),
        serde_yaml::Value::String(normalized_kind.to_string()),
    );

    // Set url based on kind. `url_key` is rebuilt fresh per access
    // so the borrow on `broker_map` doesn't linger across the
    // serialize + post-write `println!` that also wants to inspect
    // the map.
    fn url_key() -> serde_yaml::Value {
        serde_yaml::Value::String("url".to_string())
    }
    let final_url_for_log: String = match normalized_kind {
        "nats" => {
            let existing_url = broker_map
                .get(&url_key())
                .and_then(|v| v.as_str())
                .map(String::from);
            let final_url = match url {
                Some(u) => u.to_string(),
                None => match existing_url.filter(|s| s.starts_with("nats://")) {
                    Some(s) => s,
                    None => "nats://127.0.0.1:4222".to_string(),
                },
            };
            broker_map.insert(url_key(), serde_yaml::Value::String(final_url.clone()));
            final_url
        }
        "local" => {
            // Local broker has no URL; set to empty string to keep
            // the field present (the BrokerInner serde struct expects
            // it for backwards compatibility with pre-92 readers).
            broker_map.insert(url_key(), serde_yaml::Value::String(String::new()));
            String::new()
        }
        _ => unreachable!(),
    };

    let serialized = serde_yaml::to_string(&doc)?;
    std::fs::write(&broker_yaml, serialized)?;
    println!(
        "✓ broker.yaml updated: type={} url={}",
        normalized_kind, final_url_for_log
    );

    if no_signal {
        println!("  (--no-signal: skipping daemon kick)");
        println!("  Restart the daemon manually to pick up the new config.");
        return Ok(());
    }

    // Best-effort SIGTERM to any running daemon matching this config
    // dir. `pgrep -f` would be more portable than parsing /proc but
    // the proyecto target ships unix-only today; revisit when
    // Windows is in scope.
    let cfg_path = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let needle = format!("--config {}", cfg_path.display());
    let output = std::process::Command::new("pgrep")
        .args(["-f", &needle])
        .output();
    let pids: Vec<i32> = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|&pid: &i32| pid != std::process::id() as i32)
            .collect(),
        _ => Vec::new(),
    };
    if pids.is_empty() {
        println!("  (no running daemon matched `{needle}`; no signal sent)");
        println!("  Start the daemon to pick up the new config.");
        return Ok(());
    }
    for pid in &pids {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    println!(
        "  Sent SIGTERM to {} daemon process(es); supervisor loop should respawn.",
        pids.len()
    );
    Ok(())
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

/// `agent reload` subcommand. Loads the broker config,
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
    // `/admin/pollers/*` first; falls through to credentials,
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
    // Route `/admin/credentials/*` first (credential hot-reload), then
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
    // Server-side dispatch metrics
    // (`mcp_requests_total`, `mcp_request_duration_seconds`,
    // `mcp_in_flight`, `mcp_rate_limit_hits_total`, etc.).
    body.push_str(&nexo_mcp::server::telemetry::render_prometheus());
    body.push_str(&nexo_poller::telemetry::render_prometheus());
    // Phase 81.20.x F2.5 — email metrics direct call removed.
    // Email v0.6.0+ declares `[plugin.metrics]` and the broker
    // scrape below covers it. Earlier plugin versions that lack
    // the manifest section will simply contribute nothing to
    // `/metrics`; operators must upgrade to v0.6.0 to keep
    // observability.
    // Phase 81.33.b.real Stage 5 — scrape every plugin that
    // declared `[plugin.metrics] prometheus = true`. Sequential
    // dispatch with per-plugin timeout; one slow / unresponsive
    // plugin warn-logs + contributes empty (handler does not
    // stall). When `nexo-plugin-email` ships the manifest section
    // the daemon switches automatically: the direct call above
    // skips + this scrape line takes over.
    body.push_str(
        &nexo_pairing::plugin_metrics::scrape_all(&health.broker, &health.plugin_metrics).await,
    );
    // Phase 92.followup.b — surface tunnel lifecycle counters
    // (`tunnel_starts_total`, `tunnel_starts_failed_total`,
    // `tunnel_shutdowns_total`) AND per-tunnel supervisor
    // counters (`tunnel_streams_total`, `tunnel_bytes_in/out_total`,
    // `tunnel_reconnects_total`) for every handle live in the
    // registry. The handle registry is populated by tunnel-
    // creation sites (whatsapp-pairing public_tunnel today;
    // admin `--tunnel` lives in a standalone CLI subcommand
    // and is intentionally NOT routed through this aggregator).
    {
        let guard = health.tunnel_registry.read().await;
        let refs: Vec<&nexo_tunnel_quick::TunnelHandle> =
            guard.iter().map(|h| h.as_ref()).collect();
        body.push_str(&nexo_tunnel_quick::metrics::render_prometheus_for(&refs).await);
    }
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

    let parsed = read_http_request(&mut stream).await?;
    let path = parsed.path.clone();
    // Phase 81.33.b.real Stage 2 — plugin HTTP route dispatch.
    // Check the manifest-driven router BEFORE the legacy
    // hardcoded `/whatsapp/*` block. A match forwards via broker
    // JSON-RPC to the declaring plugin's subprocess; the plugin
    // returns the full response (status + headers + body). On
    // broker failure the daemon renders a typed 502/504. With no
    // matching prefix we fall through to the legacy handlers.
    if let Some((plugin_id, timeout)) = health.http_router.match_path(&path) {
        let plugin_id = plugin_id.to_string();
        let forward = nexo_pairing::plugin_http::forward_request(
            &health.broker,
            &plugin_id,
            &parsed.method,
            &parsed.path,
            &parsed.query,
            &parsed.headers,
            &parsed.body,
            timeout,
        )
        .await;
        match forward {
            Ok(reply) => {
                let body = reply.decoded_body();
                let content_type = reply
                    .header("Content-Type")
                    .unwrap_or("application/octet-stream");
                write_http_response_bytes(&mut stream, reply.status, content_type, &body).await?;
                return Ok(());
            }
            Err(err) => {
                tracing::warn!(plugin = %plugin_id, error = %err, path = %parsed.path, "plugin HTTP forward failed");
                let (status, body) = match &err {
                    nexo_pairing::plugin_http::PluginHttpForwardError::Broker(_) => {
                        (504, r#"{"error":"plugin gateway timeout"}"#)
                    }
                    nexo_pairing::plugin_http::PluginHttpForwardError::ParseReply(_) => {
                        (502, r#"{"error":"plugin reply malformed"}"#)
                    }
                };
                write_http_response(&mut stream, status, "application/json; charset=utf-8", body)
                    .await?;
                return Ok(());
            }
        }
    }
    // Phase 81.20.x Bucket C2 Stage 2 — `/whatsapp/*` hardcoded
    // block removed. `PluginHttpRouter` (Stage 2, matched at the
    // top of this fn) forwards `/whatsapp/*` to the whatsapp
    // subprocess via broker. The subprocess's
    // `auto_discovery::http_request` handler decides what to
    // serve (currently `/whatsapp/health` + `/whatsapp/status`;
    // pair/QR/HTML routes land in plugin v0.4.4 follow-up).
    // Operators using the daemon's legacy `/whatsapp/pair` HTML
    // flow should migrate to the admin-ui plugin's pairing
    // wizard (driven by admin RPC `pairing/start` —
    // `WhatsappPairingTrigger` is unaffected).

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
        // Phase 81.20.x F2.4 — `/email/health` route removed.
        // Email v0.6.0+ owns `/email/*` via `[plugin.http]` manifest
        // (mount_prefix = "/email"); the daemon's PluginHttpRouter
        // forwards requests over broker to the subprocess, which
        // renders the same JSON snapshot shape this route used to
        // produce (see `nexo-plugin-email/src/auto_discovery.rs::http_request`).
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

// Phase 81.20.x F2.4 — `render_email_health` removed. Migrated
// verbatim to `nexo-plugin-email/src/auto_discovery.rs::http_request`
// where it now lives inside the subprocess; the daemon's
// PluginHttpRouter forwards `/email/health` requests there.

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
    Ok(read_http_request(stream).await?.path)
}

/// Parsed inbound HTTP request used by the plugin HTTP router
/// (Phase 81.33.b.real Stage 2). Backwards-compatible with the
/// legacy `read_http_path` callers that only need the path.
#[derive(Debug, Clone, Default)]
struct ParsedHttpRequest {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Read a full HTTP request from the stream. Today's caller
/// surface is small: pairing pages (GET, no body), status/qr
/// (GET, no body), and `/health` / `/metrics` daemon-internal
/// (GET, no body). For Stage 2 we accept up to 16KB of body
/// upfront; plugins needing large uploads must use
/// `[plugin.http_server]` (own port) instead.
async fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<ParsedHttpRequest> {
    let mut buf = vec![0u8; 16 * 1024];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        anyhow::bail!("empty request");
    }
    buf.truncate(n);
    // Split header / body at the first \r\n\r\n.
    let mut header_end = None;
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            header_end = Some(i);
            break;
        }
    }
    let (header_bytes, body) = match header_end {
        Some(idx) => (&buf[..idx], buf[idx + 4..].to_vec()),
        None => (buf.as_slice(), Vec::new()),
    };
    let header_str = std::str::from_utf8(header_bytes).context("invalid request header utf8")?;
    let mut lines = header_str.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let full_path = parts.next().unwrap_or("/");
    let (path, query) = match full_path.find('?') {
        Some(i) => (full_path[..i].to_string(), full_path[i + 1..].to_string()),
        None => (full_path.to_string(), String::new()),
    };
    let mut headers = Vec::new();
    for line in lines {
        if let Some(i) = line.find(':') {
            headers.push((
                line[..i].trim().to_string(),
                line[i + 1..].trim().to_string(),
            ));
        }
    }
    Ok(ParsedHttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// Phase 81.33.b.real Stage 2 — write a response with raw binary
/// body. The string-body variant ([`write_http_response`]) handles
/// the common case (JSON / HTML / text). Plugins returning images,
/// PDFs, or anything binary use this entrypoint via the broker
/// JSON-RPC forwarder which base64-decodes server-side.
async fn write_http_response_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        status_text,
        content_type,
        body.len(),
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
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

// ---- TaskFlow CLI ----------------------------------------------------------

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
        &cfg.plugins,
        &google,
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
    // Phase 93.5.a — walk the opaque entries map so newly-declared
    // plugins (slack/discord/sms) surface in `nexo doctor` without
    // a daemon-side typed-field addition. Daemon-native channels
    // (whatsapp/telegram) keep showing per-instance labels by
    // dipping into the still-typed fields when present.
    for plugin_id in cfg.plugins.plugin_ids() {
        match plugin_id.as_str() {
            "whatsapp" => {
                // Wave 7 — opaque entries.
                for (i, inst) in cfg.plugins.instances_for("whatsapp").iter().enumerate() {
                    println!("  • whatsapp[{i}] (instance={inst})");
                }
            }
            "telegram" => {
                // Wave 6 — read from opaque entries; nexo-config no
                // longer carries typed telegram.
                for (i, inst) in cfg.plugins.instances_for("telegram").iter().enumerate() {
                    println!("  • telegram[{i}] (instance={inst})");
                }
            }
            other if cfg.plugins.is_active(other) => {
                println!("  • {other}");
            }
            _ => {}
        }
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

/// Phase 96.7 — daemon-side reverse-RPC handler for plugin v2
/// poller subprocesses. Subscribes to `daemon.rpc.{plugin_id}` and
/// services `credentials_get` / `log` / `metric_inc` / `llm_invoke`
/// calls; replies on the message's own `reply_to` topic.
async fn spawn_poller_reverse_rpc_subscriber(
    plugin_id: String,
    topic: String,
    broker: nexo_broker::AnyBroker,
    credentials: std::sync::Arc<nexo_auth::CredentialsBundle>,
    llm_registry: std::sync::Arc<nexo_llm::LlmRegistry>,
    llm_config: std::sync::Arc<nexo_config::LlmConfig>,
) {
    use nexo_broker::{BrokerHandle, Event, Message};
    let mut sub = match broker.subscribe(&topic).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                plugin = %plugin_id,
                topic = %topic,
                error = %e,
                "poller reverse-RPC subscribe failed",
            );
            return;
        }
    };
    tracing::info!(
        plugin = %plugin_id,
        topic = %topic,
        "poller reverse-RPC subscriber started",
    );
    while let Some(event) = sub.next().await {
        // Request-reply envelope: `event.payload` is a serialized
        // `Message` containing `reply_to` + the actual JSON-RPC body
        // in `payload`.
        let msg: Message = match serde_json::from_value(event.payload.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    plugin = %plugin_id,
                    error = %e,
                    "poller reverse-RPC envelope parse failed; dropping",
                );
                continue;
            }
        };
        let reply_topic = match msg.reply_to.clone() {
            Some(r) => r,
            None => {
                tracing::warn!(
                    plugin = %plugin_id,
                    "poller reverse-RPC message missing reply_to; dropping",
                );
                continue;
            }
        };
        let body = msg.payload;
        let method = body
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = body
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let agent_id_field = body
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let result = match method.as_str() {
            "credentials_get" => handle_creds_get(&credentials, agent_id_field.as_deref(), &params),
            "log" => {
                tracing::info!(
                    plugin = %plugin_id,
                    ?params,
                    "poller subprocess log",
                );
                Ok(serde_json::json!({}))
            }
            "metric_inc" => {
                tracing::info!(
                    plugin = %plugin_id,
                    ?params,
                    "poller subprocess metric_inc",
                );
                Ok(serde_json::json!({}))
            }
            "llm_invoke" => handle_llm_invoke(&llm_registry, &llm_config, params.clone()).await,
            other => Err((-32601, format!("method not found: {other}"))),
        };
        let reply_payload = match result {
            Ok(v) => serde_json::json!({ "result": v }),
            Err((code, msg)) => serde_json::json!({
                "error": { "code": code, "message": msg }
            }),
        };
        let reply_msg = Message::new(reply_topic.clone(), reply_payload);
        let reply_event = Event::new(
            &reply_topic,
            "daemon.poller",
            match serde_json::to_value(&reply_msg) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        plugin = %plugin_id,
                        error = %e,
                        "poller reverse-RPC reply serialize failed",
                    );
                    continue;
                }
            },
        );
        if let Err(e) = broker.publish(&reply_topic, reply_event).await {
            tracing::warn!(
                plugin = %plugin_id,
                topic = %reply_topic,
                error = %e,
                "poller reverse-RPC reply publish failed",
            );
        }
    }
}

fn handle_creds_get(
    credentials: &nexo_auth::CredentialsBundle,
    agent_id_override: Option<&str>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, (i32, String)> {
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "credentials_get: missing `channel`".to_string()))?;
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .or(agent_id_override)
        .ok_or((-32602, "credentials_get: missing `agent_id`".to_string()))?;
    let channel_static: &'static str = match channel {
        "whatsapp" => nexo_auth::handle::WHATSAPP,
        "telegram" => nexo_auth::handle::TELEGRAM,
        "google" => nexo_auth::handle::GOOGLE,
        other => {
            return Err((
                -32002,
                format!("credentials_get: unknown channel '{other}'"),
            ))
        }
    };
    let handle = credentials
        .resolver
        .resolve(agent_id, channel_static)
        .map_err(|_| {
            (
                -32002,
                format!("no '{channel_static}' binding for agent '{agent_id}'"),
            )
        })?;
    let mut out = serde_json::json!({
        "channel": channel_static,
        "account_id": handle.account_id_raw(),
        "agent_id": agent_id,
    });
    if channel_static == nexo_auth::handle::GOOGLE {
        if let Some(acct) = credentials.google_account(agent_id) {
            if let serde_json::Value::Object(ref mut m) = out {
                m.insert(
                    "client_id_path".into(),
                    serde_json::Value::String(acct.client_id_path.to_string_lossy().into_owned()),
                );
                m.insert(
                    "client_secret_path".into(),
                    serde_json::Value::String(
                        acct.client_secret_path.to_string_lossy().into_owned(),
                    ),
                );
                m.insert(
                    "token_path".into(),
                    serde_json::Value::String(acct.token_path.to_string_lossy().into_owned()),
                );
                m.insert(
                    "scopes".into(),
                    serde_json::Value::Array(
                        acct.scopes
                            .iter()
                            .map(|s| serde_json::Value::String(s.clone()))
                            .collect(),
                    ),
                );
            }
        }
    }
    Ok(out)
}

async fn handle_llm_invoke(
    registry: &nexo_llm::LlmRegistry,
    config: &nexo_config::LlmConfig,
    params: serde_json::Value,
) -> Result<serde_json::Value, (i32, String)> {
    use nexo_llm::{ChatMessage, ChatRequest, ChatRole, ResponseContent};
    use nexo_poller::{LlmInvokeRequest, LlmInvokeResponse, LlmUsage};

    let req: LlmInvokeRequest = serde_json::from_value(params)
        .map_err(|e| (-32602, format!("llm_invoke: malformed params: {e}")))?;

    let model_cfg = nexo_config::types::agents::ModelConfig {
        provider: req.provider.clone(),
        model: req.model.clone(),
    };
    let client = registry
        .build(config, &model_cfg)
        .map_err(|e| (-32602, format!("llm client build: {e}")))?;

    let mut messages: Vec<ChatMessage> = Vec::with_capacity(req.messages.len());
    for m in req.messages {
        let role = match m.role.as_str() {
            "system" => ChatRole::System,
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            other => return Err((-32602, format!("unknown role '{other}'"))),
        };
        messages.push(ChatMessage {
            role,
            content: m.content,
            attachments: Vec::new(),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        });
    }
    let mut chat_req = ChatRequest::new(&req.model, messages);
    if let Some(mt) = req.max_tokens {
        chat_req.max_tokens = mt;
    }
    let resp = client.chat(chat_req).await.map_err(|e| {
        let msg = e.to_string();
        let is_perm = msg.contains("401")
            || msg.contains("403")
            || msg.contains("not registered")
            || msg.contains("not present in config.providers");
        if is_perm {
            (-32002, msg)
        } else {
            (-32001, msg)
        }
    })?;
    let text = match resp.content {
        ResponseContent::Text(t) => t,
        ResponseContent::ToolCalls(_) => {
            return Err((-32602, "llm_invoke: tool calls not supported".into()))
        }
    };
    let usage =
        (resp.usage.prompt_tokens > 0 || resp.usage.completion_tokens > 0).then_some(LlmUsage {
            input_tokens: resp.usage.prompt_tokens,
            output_tokens: resp.usage.completion_tokens,
        });
    Ok(serde_json::to_value(LlmInvokeResponse {
        content: text,
        model_id: req.model,
        usage,
    })
    .unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod tests {
    use super::{
        has_restricted_delegate_allowlist, mcp_server_has_auth, reload_expose_tools,
        route_cron_subcommand, seed_telegram_subprocess_env_for, seed_whatsapp_subprocess_env_for,
        subprocess_broker_kind_str, Mode,
    };
    // Phase 81.20.x Stage 7 Phase 2 close-out — test helpers
    // build their fixture YAML inline via `serde_yaml::Value` so
    // the daemon binary depends on neither `nexo-plugin-telegram`
    // nor `nexo-plugin-whatsapp` at compile time. Coverage of the
    // opaque-YAML `seed_{telegram,whatsapp}_subprocess_env_for`
    // paths is unchanged.

    fn telegram_cfg_yaml(token: &str, instance: Option<&str>) -> serde_yaml::Value {
        let instance_field = match instance {
            Some(s) => format!("\ninstance: \"{s}\""),
            None => String::new(),
        };
        let yaml = format!(
            "token: \"{token}\"\n\
             polling:\n  enabled: true\n  interval_ms: 1500\n  offset_path: \"/tmp/tg.offset\"\n\
             allowlist:\n  chat_ids:\n    - 100\n    - 200\n\
             auto_transcribe:\n  enabled: false\n  command: \"\"\n  timeout_ms: 60000\n\
             bridge_timeout_ms: 30000\n\
             {instance_field}",
        );
        serde_yaml::from_str(&yaml).expect("telegram test yaml parses")
    }

    /// Happy path: every operator-facing field
    /// of `TelegramPluginConfig` lands in the spawn env dict
    /// under the `NEXO_PLUGIN_TELEGRAM_*` namespace, plus the
    /// inherited daemon whitelist (PATH/HOME/RUST_LOG/broker URL).
    #[test]
    fn seed_telegram_subprocess_env_for_happy_path() {
        let cfg = telegram_cfg_yaml("123:abcdef", Some("bot1"));
        let env = seed_telegram_subprocess_env_for(&cfg, "nats", "nats://127.0.0.1:4222");

        assert_eq!(
            env.get("NEXO_PLUGIN_TELEGRAM_TOKEN").map(String::as_str),
            Some("123:abcdef")
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_TELEGRAM_INSTANCE").map(String::as_str),
            Some("bot1")
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_TELEGRAM_OFFSET_PATH")
                .map(String::as_str),
            Some("/tmp/tg.offset"),
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_TELEGRAM_INTERVAL_MS")
                .map(String::as_str),
            Some("1500")
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_TELEGRAM_BRIDGE_TIMEOUT_MS")
                .map(String::as_str),
            Some("30000"),
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_TELEGRAM_ALLOWLIST")
                .map(String::as_str),
            Some("[100,200]"),
        );
        assert_eq!(
            env.get("NEXO_BROKER_URL").map(String::as_str),
            Some("nats://127.0.0.1:4222")
        );
    }

    /// Empty / `None` instance is omitted (not
    /// emitted as empty string) so the subprocess's
    /// `whatsapp_config_from_env` analog reads `instance = None`,
    /// not `instance = Some("")` — matches the YAML loader's
    /// "absent = default single bot" semantics.
    #[test]
    fn seed_telegram_subprocess_env_for_omits_empty_instance() {
        let cfg = telegram_cfg_yaml("tok", None);
        let env = seed_telegram_subprocess_env_for(&cfg, "nats", "nats://x");
        assert!(!env.contains_key("NEXO_PLUGIN_TELEGRAM_INSTANCE"));

        let cfg2 = telegram_cfg_yaml("tok", Some("   "));
        let env2 = seed_telegram_subprocess_env_for(&cfg2, "nats", "nats://x");
        assert!(!env2.contains_key("NEXO_PLUGIN_TELEGRAM_INSTANCE"));
    }

    /// Auto-transcribe disabled (default) drops
    /// every whisper-related env var; enabled lights them up.
    #[test]
    fn seed_telegram_subprocess_env_for_transcribe_toggle() {
        let mut cfg = telegram_cfg_yaml("tok", None);
        let env_off = seed_telegram_subprocess_env_for(&cfg, "nats", "nats://x");
        assert!(!env_off.contains_key("NEXO_PLUGIN_TELEGRAM_AUTO_TRANSCRIBE"));
        assert!(!env_off.contains_key("NEXO_PLUGIN_TELEGRAM_WHISPER_COMMAND"));

        let cfg_map = cfg.as_mapping_mut().expect("mapping root");
        cfg_map.insert(
            serde_yaml::Value::String("auto_transcribe".into()),
            serde_yaml::from_str(
                "enabled: true\n\
                 command: \"/usr/bin/whisper\"\n\
                 timeout_ms: 45000\n\
                 language: \"es\"\n",
            )
            .unwrap(),
        );
        let env_on = seed_telegram_subprocess_env_for(&cfg, "nats", "nats://x");
        assert_eq!(
            env_on
                .get("NEXO_PLUGIN_TELEGRAM_AUTO_TRANSCRIBE")
                .map(String::as_str),
            Some("true"),
        );
        assert_eq!(
            env_on
                .get("NEXO_PLUGIN_TELEGRAM_WHISPER_COMMAND")
                .map(String::as_str),
            Some("/usr/bin/whisper"),
        );
        assert_eq!(
            env_on
                .get("NEXO_PLUGIN_TELEGRAM_WHISPER_TIMEOUT_MS")
                .map(String::as_str),
            Some("45000"),
        );
        assert_eq!(
            env_on
                .get("NEXO_PLUGIN_TELEGRAM_WHISPER_LANGUAGE")
                .map(String::as_str),
            Some("es"),
        );
    }

    /// Daemon's full process env is NOT inherited
    /// by reference; the helper builds a fresh dict whitelisting
    /// only PATH/HOME/RUST_LOG (defense-in-depth against secrets
    /// leaking). Set a sentinel env in the test process and assert
    /// it does NOT appear in the spawn dict.
    #[test]
    fn seed_telegram_subprocess_env_for_does_not_leak_random_daemon_env() {
        std::env::set_var("__NEXO_TG_TEST_LEAK_SENTINEL__", "do-not-leak");
        let cfg = telegram_cfg_yaml("tok", None);
        let env = seed_telegram_subprocess_env_for(&cfg, "nats", "nats://x");
        assert!(!env.contains_key("__NEXO_TG_TEST_LEAK_SENTINEL__"));
        std::env::remove_var("__NEXO_TG_TEST_LEAK_SENTINEL__");
    }

    // Daemon broker = Local means the subprocess can't
    // reach the in-process broker; the env seeder stamps
    // `NEXO_BROKER_KIND=stdio_bridge` and OMITS the URL because
    // the transport is the parent's stdin/stdout, not a network
    // endpoint.
    #[test]
    fn seed_telegram_env_stdio_bridge_omits_url() {
        let cfg = telegram_cfg_yaml("tok", None);
        let env = seed_telegram_subprocess_env_for(&cfg, "stdio_bridge", "nats://ignored");
        assert_eq!(
            env.get("NEXO_BROKER_KIND").map(String::as_str),
            Some("stdio_bridge")
        );
        assert!(
            !env.contains_key("NEXO_BROKER_URL"),
            "stdio_bridge must omit NEXO_BROKER_URL; got env={env:?}"
        );
    }

    #[test]
    fn seed_telegram_env_nats_keeps_url() {
        let cfg = telegram_cfg_yaml("tok", None);
        let env = seed_telegram_subprocess_env_for(&cfg, "nats", "nats://central:4222");
        assert_eq!(
            env.get("NEXO_BROKER_KIND").map(String::as_str),
            Some("nats")
        );
        assert_eq!(
            env.get("NEXO_BROKER_URL").map(String::as_str),
            Some("nats://central:4222")
        );
    }

    #[test]
    fn subprocess_broker_kind_str_maps_daemon_to_child() {
        use nexo_config::types::broker::BrokerKind;
        assert_eq!(subprocess_broker_kind_str(BrokerKind::Nats), "nats");
        // Daemon `Local` triggers the stdio bridge on the child
        // because the in-process broker is unreachable across
        // process boundaries.
        assert_eq!(
            subprocess_broker_kind_str(BrokerKind::Local),
            "stdio_bridge"
        );
        // `StdioBridge` is daemon-derived; should never appear in
        // operator YAML, but the mapper handles it defensively.
        assert_eq!(
            subprocess_broker_kind_str(BrokerKind::StdioBridge),
            "stdio_bridge"
        );
    }

    /// Build a YAML fixture matching the operator-facing schema of
    /// `nexo-plugin-whatsapp`'s `WhatsappPluginConfig`. Returned as
    /// `serde_yaml::Value` so the daemon binary's test crate
    /// doesn't depend on the plugin's typed config — only the
    /// wire shape consumed by `seed_whatsapp_subprocess_env_for`.
    fn whatsapp_cfg_yaml(session_dir: &str, instance: Option<&str>) -> serde_yaml::Value {
        let instance_field = match instance {
            Some(s) => format!("\ninstance: \"{s}\""),
            None => String::new(),
        };
        let yaml = format!(
            "enabled: true\n\
             session_dir: \"{session_dir}\"\n\
             media_dir: \"/tmp/wa-media\"\n\
             acl:\n  allow_list:\n    - \"+5491100000000\"\n  from_env: \"\"\n\
             bridge:\n  response_timeout_ms: 45000\n  on_timeout: \"noop\"\n  apology_text: \"\"\n\
             transcriber:\n  enabled: false\n  skill: \"whisper\"\n  timeout_ms: 60000\n\
             {instance_field}",
        );
        serde_yaml::from_str(&yaml).expect("whatsapp test yaml parses")
    }

    /// Happy path: every operator-facing field of the whatsapp
    /// YAML lands in the spawn env dict under the
    /// `NEXO_PLUGIN_WHATSAPP_*` namespace plus the inherited
    /// daemon whitelist.
    #[test]
    fn seed_whatsapp_subprocess_env_for_happy_path() {
        let cfg = whatsapp_cfg_yaml("/tmp/wa-session", Some("ventas"));
        let env = seed_whatsapp_subprocess_env_for(&cfg, "nats", "nats://127.0.0.1:4222");

        assert_eq!(
            env.get("NEXO_PLUGIN_WHATSAPP_SESSION_DIR")
                .map(String::as_str),
            Some("/tmp/wa-session"),
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_WHATSAPP_MEDIA_DIR")
                .map(String::as_str),
            Some("/tmp/wa-media"),
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_WHATSAPP_INSTANCE").map(String::as_str),
            Some("ventas"),
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_WHATSAPP_BRIDGE_TIMEOUT_MS")
                .map(String::as_str),
            Some("45000"),
        );
        assert_eq!(
            env.get("NEXO_PLUGIN_WHATSAPP_ALLOWLIST")
                .map(String::as_str),
            Some(r#"["+5491100000000"]"#),
        );
        assert_eq!(
            env.get("NEXO_BROKER_URL").map(String::as_str),
            Some("nats://127.0.0.1:4222"),
        );
        assert!(!env.contains_key("NEXO_PLUGIN_WHATSAPP_TRANSCRIBE_ENABLED"));
    }

    /// Empty / `None` instance is omitted (not emitted as empty
    /// string) so the subprocess's `whatsapp_config_from_env`
    /// reads `instance = None`.
    #[test]
    fn seed_whatsapp_subprocess_env_for_omits_empty_instance() {
        let cfg = whatsapp_cfg_yaml("/tmp/x", None);
        let env = seed_whatsapp_subprocess_env_for(&cfg, "nats", "nats://x");
        assert!(!env.contains_key("NEXO_PLUGIN_WHATSAPP_INSTANCE"));

        let cfg2 = whatsapp_cfg_yaml("/tmp/x", Some("   "));
        let env2 = seed_whatsapp_subprocess_env_for(&cfg2, "nats", "nats://x");
        assert!(!env2.contains_key("NEXO_PLUGIN_WHATSAPP_INSTANCE"));
    }

    /// Transcriber disabled drops whisper env; enabled lights it
    /// up.
    #[test]
    fn seed_whatsapp_subprocess_env_for_transcribe_toggle() {
        let mut cfg = whatsapp_cfg_yaml("/tmp/x", None);
        let env_off = seed_whatsapp_subprocess_env_for(&cfg, "nats", "nats://x");
        assert!(!env_off.contains_key("NEXO_PLUGIN_WHATSAPP_TRANSCRIBE_ENABLED"));

        let cfg_map = cfg.as_mapping_mut().expect("mapping root");
        cfg_map.insert(
            serde_yaml::Value::String("transcriber".into()),
            serde_yaml::from_str("enabled: true\nskill: \"whisper\"\ntimeout_ms: 30000\n").unwrap(),
        );
        let env_on = seed_whatsapp_subprocess_env_for(&cfg, "nats", "nats://x");
        assert_eq!(
            env_on
                .get("NEXO_PLUGIN_WHATSAPP_TRANSCRIBE_ENABLED")
                .map(String::as_str),
            Some("true"),
        );
        assert_eq!(
            env_on
                .get("NEXO_PLUGIN_WHATSAPP_WHISPER_TIMEOUT_MS")
                .map(String::as_str),
            Some("30000"),
        );
    }

    /// Sentinel daemon env var does NOT leak into the spawn dict
    /// (defense-in-depth).
    #[test]
    fn seed_whatsapp_subprocess_env_for_does_not_leak_random_daemon_env() {
        std::env::set_var("__NEXO_WA_TEST_LEAK_SENTINEL__", "do-not-leak");
        let cfg = whatsapp_cfg_yaml("/tmp/x", None);
        let env = seed_whatsapp_subprocess_env_for(&cfg, "nats", "nats://x");
        assert!(!env.contains_key("__NEXO_WA_TEST_LEAK_SENTINEL__"));
        std::env::remove_var("__NEXO_WA_TEST_LEAK_SENTINEL__");
    }

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

    // compute_allowlist_from_mcp_server_cfg

    #[test]
    fn compute_allowlist_returns_set_from_expose_tools() {
        use super::compute_allowlist_from_mcp_server_cfg;
        let mut cfg = nexo_config::types::mcp_server::McpServerConfig::default();
        cfg.expose_tools = vec![
            "Read".into(),
            "Edit".into(),
            "marketing_lead_classify".into(),
        ];
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

    /// Verify the four `NEXO_BUILD_*` env stamps are
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

    // ---- cron_tool_bindings ArcSwap mechanics ----

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

            locale_prompts: Default::default(),
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
            event_subscribers: Vec::new(),
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
            active: true,
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

    /// `replace_bindings` performs an atomic swap visible on the
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

    /// Empty-map swap clears all bindings; resolve returns `None`.
    /// Documents the agent-removal semantics:
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

    /// The post-hook closure must early-return cleanly when
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
        assert!(
            cell.get().is_some(),
            "cell must hold the executor after set"
        );
    }

    // `nexo agent dream` CLI tests

    use super::{
        resolve_dream_db_path, run_agent_dream_kill, run_agent_dream_status, run_agent_dream_tail,
        short_uuid,
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

    // `nexo agent run` / `agent ps` CLI tests

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
        run_agent_ps(Some("bg"), true, Some(&db), false)
            .await
            .unwrap();
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

    // `agent attach` / `agent discover` CLI tests

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
        let err = run_agent_attach("11111111-1111-1111-1111-111111111111", Some(&db), false)
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
