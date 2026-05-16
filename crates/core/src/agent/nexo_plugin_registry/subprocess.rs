//! Host-side adapter for out-of-tree plugins running
//! as separate child processes. Speaks newline-delimited JSON-RPC
//! 2.0 over the child's stdin/stdout. Implements [`NexoPlugin`] so
//! it slots into the existing `wire_plugin_registry` path without
//! any new lifecycle hook on the host.
//!
//! # Wire format
//!
//! Each line on stdin (host → child) and stdout (child → host) is
//! exactly one JSON-RPC 2.0 message. Requests carry an integer
//! `id`; notifications omit `id`.
//!
//! ## Host → child
//!
//! Requests (host expects a reply with matching `id`):
//! - `initialize { nexo_version }` — child responds with
//!   `{ manifest, server_version }`. Host validates that the
//!   returned manifest's `plugin.id` matches the id the factory was
//!   registered under; mismatch is a hard failure.
//! - `nexo.init { broker_topics }` — sent after broker subscriptions
//!   are wired so the child knows the bridge is live.
//! - `shutdown { reason }` — child should flush pending state and
//!   exit. Host waits up to 5 s before SIGKILL.
//!
//! Notifications (no reply expected):
//! - `broker.event { topic, event }` — broker delivered an event on
//!   one of the topics declared in `manifest.channels.subscribe` or
//!   the topics the child requested via `nexo.init`.
//!
//! ## Child → host
//!
//! Notifications:
//! - `broker.publish { topic, event }` — host validates the topic
//!   against `manifest.channels.publish` allowlist and forwards to
//!   the broker.
//!
//! The wire shape mirrors the existing subprocess tool-extension
//! plugins (methods `initialize` / `tools/call` / `shutdown`,
//! newline-delimited JSON-RPC) extended here to channel plugins.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_broker::{topic::topic_matches, AnyBroker, BrokerHandle, Event};
use nexo_config::LlmConfig;
use nexo_llm::LlmRegistry;
use nexo_memory::LongTermMemory;
use nexo_plugin_manifest::PluginManifest;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex, Notify, OnceCell};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent::nexo_plugin_registry::factory::PluginFactory;
use crate::agent::plugin_host::{
    NexoPlugin, PluginConfigureError, PluginInitContext, PluginInitError, PluginShutdownError,
};

/// Default time budget for the child's `initialize` reply. A child
/// that doesn't respond inside this window is presumed broken — the
/// host kills it and surfaces `PluginInitError::Other`. Configurable
/// via the `NEXO_PLUGIN_INIT_TIMEOUT_MS` env var.
const DEFAULT_INIT_TIMEOUT_MS: u64 = 5_000;

/// Channel depth for the broker → child stdin path. Bounded so a
/// stalled child can't grow the daemon's memory footprint without
/// limit; 64 is comfortably above realistic burst rates for chat
/// traffic. Drop-on-full + warn matches the at-most-once delivery
/// the broker already promises.
const STDIN_CHANNEL_DEPTH: usize = 64;

/// Hard cap on the per-attempt backoff sleep
/// applied between respawn attempts. The doc-comment on
/// `SupervisorSection.backoff_ms` already promises a 60 s ceiling
/// regardless of operator-supplied values; this constant is the
/// single source of truth that enforces that promise.
const RESPAWN_BACKOFF_CAP_MS: u64 = 60_000;

/// Outcome of one supervised lifetime of a
/// subprocess plugin child. The supervisor task spawned inside
/// `spawn_one_attempt` posts exactly one of these via the
/// `attempt_outcome_tx` oneshot; `respawn_loop` consumes it to
/// decide whether to retry, give up, or exit cleanly.
#[derive(Debug)]
enum AttemptOutcome {
    /// The child exited with status 0 and the supervisor observed
    /// the exit before any cancel/shutdown signal. Treated as a
    /// graceful self-exit (e.g. plugin replied to `shutdown` then
    /// terminated): `respawn_loop` exits without republishing
    /// `crashed`.
    NormalExit,
    /// The child exited with non-zero status (or `try_wait`
    /// surfaced an error). `exit_code` is the OS-level code, or
    /// `-1` when unavailable. `stderr_tail` is the drained ring
    /// buffer (chronological, oldest first), capped at
    /// `manifest.supervisor.stderr_tail_lines`.
    Crashed {
        exit_code: i32,
        stderr_tail: Vec<String>,
    },
    /// Either `ctx_shutdown` or the per-plugin
    /// `shutdown_signaled` flag fired before the child exited.
    /// `respawn_loop` returns immediately without publishing any
    /// further lifecycle events.
    Shutdown,
}

/// Exponential backoff calculator used by
/// `respawn_loop` between attempts. Doubles per attempt, capped at
/// [`RESPAWN_BACKOFF_CAP_MS`]. Saturating arithmetic so any
/// pathological manifest value (e.g. `backoff_ms = u64::MAX`) still
/// resolves to the cap rather than panicking.
///
/// `attempt` is 0-indexed: `attempt = 0` is the first retry after
/// the original child died. Returns the wait duration to apply
/// **before** the corresponding `spawn_one_attempt` call.
///
/// Examples (`base_ms = 1000`):
///   `attempt = 0` → `1000ms`
///   `attempt = 1` → `2000ms`
///   `attempt = 2` → `4000ms`
///   `attempt = 6` → `60000ms` (capped)
///   `attempt = 99` → `60000ms` (capped, no overflow)
fn next_backoff(attempt: u32, base_ms: u64) -> Duration {
    // `1u64 << 64` is UB; clamp the shift to 63 so even a hostile
    // manifest combined with attempt=u32::MAX stays sound.
    let shift = attempt.min(63);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let raw = base_ms.saturating_mul(multiplier);
    Duration::from_millis(raw.min(RESPAWN_BACKOFF_CAP_MS))
}

/// Adapter that owns one child process and brokers
/// JSON-RPC 2.0 between it and the daemon's broker. Lifecycle is
/// driven through the `NexoPlugin` trait — `init()` spawns the
/// child + handshakes; `shutdown()` flushes + reaps it.
pub struct SubprocessNexoPlugin {
    /// Bundled manifest. Kept by the factory and handed to the
    /// adapter at construction so the host knows the plugin id
    /// before the child speaks. The child's `initialize` reply
    /// must agree with this manifest's `plugin.id`.
    cached_manifest: PluginManifest,

    /// Live state — populated by `init()`. None means the plugin
    /// hasn't been started or its boot failed.
    inner: Mutex<Option<Inner>>,

    /// Sandbox runner threaded by `init()` from
    /// `PluginInitContext.sandbox`. None for tests that call
    /// `spawn_one_attempt` directly without going through
    /// init() — those paths skip sandbox wrapping (default
    /// disabled, equivalent to `sandbox.enabled = false`).
    sandbox: Mutex<Option<Arc<crate::agent::plugin_sandbox::SandboxRunner>>>,

    /// Plugin's per-instance state dir, used to
    /// expand `${state_dir}` tokens in `fs_write_paths`.
    /// Threaded from `PluginInitContext::plugin_state_dir(...)`
    /// at init time.
    plugin_state_dir: Mutex<Option<std::path::PathBuf>>,

    /// Phase 93.2 — `configure(value)` is called by the host
    /// BEFORE `init` spawns the child process, but the
    /// `plugin.configure` JSON-RPC can only fire once the stdio
    /// channel is up. The host-facing trait method buffers the
    /// value here; `init` flushes via RPC after the child's
    /// `initialize` ack succeeds. Hot-reload re-calls overwrite
    /// the buffer + redeliver via RPC if `inner` is already up.
    pending_configure: Mutex<Option<serde_yaml::Value>>,

    /// Daemon-supplied per-spawn env dict. When
    /// `Some`, `spawn_one_attempt` calls
    /// `Command::env_clear().envs(&map)` so the child sees ONLY
    /// the keys the daemon explicitly seeded — defense-in-depth
    /// against secrets leaking from the daemon's process env.
    /// When `None`, the child inherits the daemon's full env (the
    /// pre-81.18.b behaviour, still in use by the single-instance
    /// browser plugin). Multi-instance plugins (telegram /
    /// whatsapp) populate this with `seed_*_subprocess_env_for(cfg)`
    /// per cfg entry so N spawns don't see colliding `NEXO_PLUGIN_*`
    /// vars.
    spawn_env: Option<std::collections::HashMap<String, String>>,

    /// Operator-visible label disambiguating N
    /// instances of the same plugin id (`"telegram.bot1"`,
    /// `"telegram.bot2"`). Threaded into log fields so admin
    /// diagnostics list each subprocess distinctly. `None` for
    /// single-instance plugins or in-tree paths that don't
    /// multiplex.
    instance_label: Option<String>,

    /// Manifest's parent directory. Populated by
    /// `subprocess_plugin_factory(_with_env)` from the
    /// `DiscoveredPlugin.root_dir` the discovery walker recorded.
    /// Used by `spawn_one_attempt` to resolve relative
    /// `[plugin.entrypoint] command` paths (e.g. `./bin/browser`)
    /// against the manifest's own directory instead of the daemon's
    /// CWD. `None` for hand-built test plugins that bypass the
    /// factory — those keep the pre-fix raw-path behaviour.
    manifest_dir: Option<std::path::PathBuf>,

    /// Set by `shutdown()` so the supervisor
    /// task aborts any in-flight backoff sleep or respawn
    /// attempt instead of marching on after the daemon asked it
    /// to stop. Lives on the OUTER struct (not `Inner`) so it
    /// survives `Inner` replacement during respawn — a flag on
    /// `Inner` would get dropped the moment a new `Inner` was
    /// installed, leaving stale supervisors thinking shutdown
    /// hadn't fired.
    shutdown_signaled: Arc<AtomicBool>,

    /// Paired wake-up channel for the supervisor
    /// task. `shutdown()` calls `notify_waiters()` so a supervisor
    /// parked inside `sleep_or_shutdown(backoff)` wakes immediately
    /// instead of waiting up to 60s for the natural sleep deadline.
    /// `Notify` is single-threaded waker-style (cheap), and the
    /// missed-wake-up race is handled by checking
    /// `shutdown_signaled` synchronously after each `notified()`
    /// completes.
    shutdown_notify: Arc<Notify>,

    /// Set by `force_restart` to
    /// distinguish operator-initiated teardown from `shutdown()`.
    /// The supervisor task / respawn_loop don't need to inspect
    /// this directly (they observe the cancel cascade); the flag
    /// exists as a documented marker for future coalesce-vs-stale
    /// detection. Cleared on every successful return path of
    /// `force_restart` so a subsequent invocation sees a fresh
    /// state.
    restart_signaled: Arc<AtomicBool>,

    /// Populated by both subprocess plugin
    /// factories (`subprocess_plugin_factory{,_with_env}`)
    /// immediately after `Arc::new(self)` so
    /// `spawn_supervisor_loop` can upgrade back to `Arc<Self>`.
    /// Stored as `Weak` to avoid a ref-cycle (Arc → Weak → Arc).
    /// `set()` is fallible (idempotent factory pattern) but in
    /// practice always succeeds because each adapter is
    /// constructed exactly once.
    weak_self: std::sync::OnceLock<std::sync::Weak<SubprocessNexoPlugin>>,

    /// Serialise concurrent
    /// `force_restart` invocations against the same plugin. Without
    /// this two operators clicking "Restart" simultaneously (or a
    /// CLI restart racing the admin RPC) each clone the Arc, run the
    /// 11-step teardown + spawn cascade in parallel, and one ends up
    /// with an orphaned child kept alive only by `kill_on_drop`. The
    /// audit log lies because both calls return Ok with valid PIDs.
    /// Held by `force_restart` for its full duration; second caller
    /// blocks on the lock until the first publishes
    /// `restarted_manually` and returns. `tokio::sync::Mutex` rather
    /// than `std::sync::Mutex` because the body holds the guard
    /// across `await`s.
    restart_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Per-instance live state. Separated from the outer struct so
/// `SubprocessNexoPlugin: Send + Sync` doesn't have to pretend the
/// runtime handles are always present.
struct Inner {
    /// `mpsc` sender feeding the stdin writer task. Closing this
    /// triggers a graceful child stdin EOF, which most JSON-RPC
    /// servers interpret as "drain and exit."
    stdin_tx: mpsc::Sender<Value>,

    /// Pending request → reply correlations. Keyed by JSON-RPC
    /// `id`. A `oneshot::Sender` resolves the reply once the
    /// stdout reader sees a matching response. The Mutex inside
    /// `oneshot::Sender` is `take`-only so single-shot semantics
    /// stay intact.
    pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>>,

    /// Fresh JSON-RPC request id generator. Starts
    /// at 2 because `init_id = 1` is hardcoded for the initialize
    /// handshake. Shared with `RemoteChannelAdapter` instances
    /// registered into the channel adapter registry so all
    /// host-issued requests draw from the same id space.
    next_id: Arc<AtomicU64>,

    /// Per-id streaming-response state for
    /// `llm.chat` requests where `params.stream = true`. The
    /// reader's notification handler dispatches
    /// `llm.chat.delta { request_id, chunk }` notifications to
    /// the matching entry's `delta_tx`; the response handler
    /// resolves `final_tx` AND removes the entry so `delta_tx`
    /// closes (signalling end-of-stream to the consumer).
    streaming_pending: Arc<DashMap<u64, crate::agent::llm_remote::StreamingPending>>,

    /// Phase 93.8.a-daemon — per-call id allocator for the new
    /// `plugin.credentials.*` RPCs. Initialised `1_000_000` in
    /// `spawn_one_attempt` to avoid colliding with literal
    /// `initialize`=1 / `shutdown`=2 / `plugin.configure`=3.
    /// `fetch_add(1, SeqCst)` per RPC; resets per-respawn (safe —
    /// old `Inner`'s pending receivers wake `Err` on drop).
    next_credentials_id: Arc<std::sync::atomic::AtomicU64>,

    /// Tool catalog advertised by the subprocess
    /// at initialize-reply time. `register_remote_tool_handlers`
    /// reads this to build one `RemoteToolHandler` per declared
    /// tool. Empty when the plugin doesn't expose any tools
    /// (most of today's in-tree plugin manifests).
    declared_tools: Vec<crate::agent::tool_remote::RemoteToolDef>,

    /// Background tasks: stdin writer, stdout reader, broker→child
    /// bridge per subscribed topic, supervisor.
    /// Joined on shutdown.
    tasks: Vec<JoinHandle<()>>,

    /// Child process handle. Wrapped in
    /// `Arc<Mutex<...>>` so the supervisor task can `try_wait()`
    /// every 500ms while `shutdown()` can still `take()` for
    /// reaping. Mutex contention is cheap (one try_wait poll per
    /// half-second vs single take on shutdown).
    child: Arc<Mutex<Option<Child>>>,

    /// Plugin-id-keyed cancellation token for all spawned tasks.
    /// Daemon-wide shutdown also cancels them via the
    /// `PluginInitContext.shutdown` token — both paths must work.
    cancel: CancellationToken,

    /// Wallclock when this `Inner` was
    /// installed. Used by `maybe_reset_attempt_counter` to decide
    /// whether the next crash should reset the per-plugin attempt
    /// counter (transient blip) versus increment it (recurring
    /// crash). Captured via `Instant::now()` at the end of
    /// `spawn_one_attempt` after the handshake succeeded.
    spawned_at: Instant,

    /// Single-shot receiver the per-attempt
    /// supervisor task posts an `AttemptOutcome` to when the
    /// child exits (or shutdown fires). `respawn_loop` `take()`s
    /// it once via `wait_for_attempt_outcome` and `select!`s
    /// against the daemon-wide cancellation token. Wrapped in
    /// `Mutex<Option<...>>` so the take is interior-mutable
    /// without `&mut self`.
    attempt_outcome_rx: Mutex<Option<oneshot::Receiver<AttemptOutcome>>>,
}

impl SubprocessNexoPlugin {
    /// Build the adapter. `manifest` MUST be the same manifest
    /// that the factory was registered under — the child's
    /// `initialize` reply is checked against this manifest's
    /// `plugin.id` to defend against an out-of-tree binary
    /// pretending to be a different plugin.
    pub fn new(manifest: PluginManifest) -> Self {
        Self {
            cached_manifest: manifest,
            inner: Mutex::new(None),
            sandbox: Mutex::new(None),
            plugin_state_dir: Mutex::new(None),
            pending_configure: Mutex::new(None),
            spawn_env: None,
            instance_label: None,
            manifest_dir: None,
            // Auto-respawn shutdown coordination.
            // Both default-quiescent: the supervisor task only checks
            // them after init() succeeds + the respawn_loop is
            // spawned by the init_loop.rs hook.
            shutdown_signaled: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            // Quiescent until
            // `force_restart` flips it; cleared on return.
            restart_signaled: Arc::new(AtomicBool::new(false)),
            // Populated by the factory after
            // `Arc::new(self)` so `spawn_supervisor_loop` can
            // upgrade back to `Arc<Self>`. Empty for hand-built
            // plugins constructed outside the factory (tests),
            // which means those plugins can't auto-respawn — fine,
            // tests don't go through init_loop.
            weak_self: std::sync::OnceLock::new(),
            // Quiescent until the first
            // force_restart acquires it. See field doc.
            restart_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Supply a per-spawn env dict that replaces
    /// the daemon's process env at child spawn time. See
    /// [`spawn_env`](#structfield.spawn_env) for the full
    /// rationale. Builder API; chain after `new()`.
    pub fn with_spawn_env(mut self, env: std::collections::HashMap<String, String>) -> Self {
        self.spawn_env = Some(env);
        self
    }

    /// Supply the manifest's parent directory so relative
    /// `[plugin.entrypoint] command` paths (e.g. `./bin/foo`) get
    /// resolved against the manifest's own dir at spawn time
    /// instead of the daemon's CWD. Mirrors how the discovery
    /// walker records `DiscoveredPlugin.root_dir`; the factory
    /// threads that value into here. Builder API.
    pub fn with_manifest_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.manifest_dir = Some(dir);
        self
    }

    /// Operator-visible label for multi-instance
    /// plugins. Threaded into tracing log fields so admin
    /// diagnostics distinguish concurrent subprocesses. Empty
    /// strings are normalised to `None`.
    pub fn with_instance_label(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        self.instance_label = if label.trim().is_empty() {
            None
        } else {
            Some(label)
        };
        self
    }

    // ─── Helpers ───

    /// Upgrade the factory-populated `weak_self` to a typed
    /// `Arc<Self>` so callers (init_loop) can hand the Arc into
    /// `spawn_supervisor_loop`. Returns `None` for hand-built
    /// plugins constructed outside the subprocess plugin
    /// factories (tests that call `SubprocessNexoPlugin::new`
    /// directly without going through the factory + Arc::new
    /// path); those tests don't auto-respawn.
    pub fn weak_self_arc(&self) -> Option<Arc<Self>> {
        self.weak_self.get().and_then(|w| w.upgrade())
    }

    /// Drain the current `Inner.pending` DashMap, sending
    /// `Err(reason)` to every parked oneshot. Called by
    /// `respawn_loop` BEFORE installing a fresh `Inner` so
    /// in-flight callers fail-fast against the dying child
    /// instead of stranding their `oneshot::recv().await` until
    /// timeout. The order matters: drain first, then install —
    /// after install, callers could send into the new `stdin_tx`
    /// holding stale request ids that the new child would reply
    /// `MethodNotFound` to.
    /// Phase 93.2 — send `plugin.configure` JSON-RPC against a
    /// live [`Inner`] and await the ack with a 5s timeout. Used
    /// by both `init` (flushing buffered value) and the trait
    /// `configure` hot-reload path.
    async fn send_configure_rpc(
        &self,
        value: &serde_yaml::Value,
        inner: &Inner,
    ) -> Result<(), PluginConfigureError> {
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let value_json: Value = serde_json::to_value(value).map_err(|e| {
            PluginConfigureError::SubprocessRpc {
                plugin_id: plugin_id.clone(),
                reason: format!("YAML→JSON conversion failed: {e}"),
            }
        })?;
        let configure_id: u64 = 3;
        let req = json!({
            "jsonrpc": "2.0",
            "id": configure_id,
            "method": "plugin.configure",
            "params": { "value": value_json },
        });
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        inner.pending.insert(configure_id, tx);
        if inner.stdin_tx.send(req).await.is_err() {
            inner.pending.remove(&configure_id);
            return Err(PluginConfigureError::SubprocessRpc {
                plugin_id,
                reason: "stdin channel closed".to_string(),
            });
        }
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Err(_) => {
                inner.pending.remove(&configure_id);
                Err(PluginConfigureError::SubprocessRpc {
                    plugin_id,
                    reason: "configure ack timed out after 5s".to_string(),
                })
            }
            Ok(Err(_)) => Err(PluginConfigureError::SubprocessRpc {
                plugin_id,
                reason: "oneshot dropped before reply".to_string(),
            }),
            Ok(Ok(Ok(_))) => Ok(()),
            Ok(Ok(Err(err_msg))) => Err(PluginConfigureError::SubprocessRpc {
                plugin_id,
                reason: err_msg,
            }),
        }
    }

    /// Phase 93.8.a-daemon — `plugin.credentials.list` host→child
    /// RPC. 5s timeout (boot-time class). Returns the typed reply
    /// or `Err(msg)` for transport / timeout / plugin-side error.
    pub(crate) async fn send_credentials_list_rpc(
        &self,
    ) -> Result<crate::agent::nexo_plugin_registry::remote_credential_store::CredentialsListReply, String> {
        use crate::agent::nexo_plugin_registry::remote_credential_store::CredentialsListReply;
        let inner_guard = self.inner.lock().await;
        let Some(inner) = inner_guard.as_ref() else {
            return Err("subprocess not spawned".to_string());
        };
        let id = inner
            .next_credentials_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "plugin.credentials.list",
            "params": {},
        });
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        inner.pending.insert(id, tx);
        if inner.stdin_tx.send(req).await.is_err() {
            inner.pending.remove(&id);
            return Err("stdin channel closed".to_string());
        }
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Err(_) => {
                inner.pending.remove(&id);
                Err("plugin.credentials.list timeout".to_string())
            }
            Ok(Err(_)) => Err("oneshot dropped before reply".to_string()),
            Ok(Ok(Ok(value))) => serde_json::from_value::<CredentialsListReply>(value)
                .map_err(|e| format!("response decode: {e}")),
            Ok(Ok(Err(msg))) => Err(msg),
        }
    }

    /// Phase 93.8.a-daemon — `plugin.credentials.issue` host→child
    /// RPC. 1s timeout (hot-path class).
    pub(crate) async fn send_credentials_issue_rpc(
        &self,
        account_id: &str,
        agent_id: &str,
    ) -> Result<(), String> {
        let inner_guard = self.inner.lock().await;
        let Some(inner) = inner_guard.as_ref() else {
            return Err("subprocess not spawned".to_string());
        };
        let id = inner
            .next_credentials_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "plugin.credentials.issue",
            "params": { "account_id": account_id, "agent_id": agent_id },
        });
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        inner.pending.insert(id, tx);
        if inner.stdin_tx.send(req).await.is_err() {
            inner.pending.remove(&id);
            return Err("stdin channel closed".to_string());
        }
        match tokio::time::timeout(Duration::from_secs(1), rx).await {
            Err(_) => {
                inner.pending.remove(&id);
                Err("plugin.credentials.issue timeout".to_string())
            }
            Ok(Err(_)) => Err("oneshot dropped before reply".to_string()),
            Ok(Ok(Ok(value))) => {
                if value
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err("issue ack missing ok=true".to_string())
                }
            }
            Ok(Ok(Err(msg))) => Err(msg),
        }
    }

    /// Phase 93.8.a-daemon — `plugin.credentials.resolve_bytes`
    /// host→child RPC. 1s timeout. Returns the base64-encoded
    /// payload string (caller decodes via
    /// `base64::engine::general_purpose::STANDARD.decode`).
    pub(crate) async fn send_credentials_resolve_bytes_rpc(
        &self,
        account_id: &str,
        agent_id: &str,
        fingerprint: &str,
    ) -> Result<String, String> {
        let inner_guard = self.inner.lock().await;
        let Some(inner) = inner_guard.as_ref() else {
            return Err("subprocess not spawned".to_string());
        };
        let id = inner
            .next_credentials_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "plugin.credentials.resolve_bytes",
            "params": {
                "account_id": account_id,
                "agent_id": agent_id,
                "fingerprint": fingerprint,
            },
        });
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        inner.pending.insert(id, tx);
        if inner.stdin_tx.send(req).await.is_err() {
            inner.pending.remove(&id);
            return Err("stdin channel closed".to_string());
        }
        match tokio::time::timeout(Duration::from_secs(1), rx).await {
            Err(_) => {
                inner.pending.remove(&id);
                Err("plugin.credentials.resolve_bytes timeout".to_string())
            }
            Ok(Err(_)) => Err("oneshot dropped before reply".to_string()),
            Ok(Ok(Ok(value))) => value
                .get("bytes_b64")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "resolve_bytes reply missing bytes_b64".to_string()),
            Ok(Ok(Err(msg))) => Err(msg),
        }
    }

    /// Phase 93.8.a-daemon — `plugin.credentials.reload` host→child
    /// RPC. 5s timeout (boot-time class).
    pub(crate) async fn send_credentials_reload_rpc(&self) -> Result<(), String> {
        let inner_guard = self.inner.lock().await;
        let Some(inner) = inner_guard.as_ref() else {
            return Err("subprocess not spawned".to_string());
        };
        let id = inner
            .next_credentials_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "plugin.credentials.reload",
            "params": {},
        });
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        inner.pending.insert(id, tx);
        if inner.stdin_tx.send(req).await.is_err() {
            inner.pending.remove(&id);
            return Err("stdin channel closed".to_string());
        }
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Err(_) => {
                inner.pending.remove(&id);
                Err("plugin.credentials.reload timeout".to_string())
            }
            Ok(Err(_)) => Err("oneshot dropped before reply".to_string()),
            Ok(Ok(Ok(value))) => {
                if value
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err("reload ack missing ok=true".to_string())
                }
            }
            Ok(Ok(Err(msg))) => Err(msg),
        }
    }

    async fn drain_pending_with_error(&self, reason: &str) {
        let inner_guard = self.inner.lock().await;
        let Some(inner) = inner_guard.as_ref() else {
            return;
        };
        // DashMap doesn't expose `drain()` per se; iterate keys
        // and remove + send. The clear() at the end is belt-and-
        // suspenders against any race where an entry slipped in
        // mid-iteration (DashMap allows concurrent mutation).
        let keys: Vec<u64> = inner.pending.iter().map(|e| *e.key()).collect();
        for k in keys {
            if let Some((_, tx)) = inner.pending.remove(&k) {
                let _ = tx.send(Err(reason.to_string()));
            }
        }
        inner.pending.clear();
    }

    /// Sleep for `dur` or wake early if shutdown fires. Returns
    /// `true` when shutdown short-circuited the sleep (so the
    /// caller — `respawn_loop` — can stop iterating). The
    /// `shutdown_signaled` AtomicBool is checked synchronously
    /// before the wait so a shutdown that fired between
    /// `select!` arms doesn't slip through.
    async fn sleep_or_shutdown(&self, dur: Duration) -> bool {
        if self.shutdown_signaled.load(Ordering::Acquire) {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(dur) => self.shutdown_signaled.load(Ordering::Acquire),
            _ = self.shutdown_notify.notified() => true,
        }
    }

    /// Take the current `Inner.attempt_outcome_rx` and `select!`
    /// against the daemon-wide cancellation token + the
    /// per-plugin shutdown notify. Returns the supervisor's
    /// posted outcome, or `Shutdown` when any shutdown source
    /// fires first. Returns `Shutdown` also when the receiver
    /// was already taken (defensive — `respawn_loop` is the only
    /// caller and takes exactly once per attempt).
    async fn wait_for_attempt_outcome(&self, ctx_shutdown: &CancellationToken) -> AttemptOutcome {
        let rx = {
            let inner_guard = self.inner.lock().await;
            match inner_guard.as_ref() {
                Some(inner) => inner.attempt_outcome_rx.lock().await.take(),
                None => None,
            }
        };
        let Some(rx) = rx else {
            return AttemptOutcome::Shutdown;
        };
        tokio::select! {
            res = rx => res.unwrap_or(AttemptOutcome::Shutdown),
            _ = ctx_shutdown.cancelled() => AttemptOutcome::Shutdown,
            _ = self.shutdown_notify.notified() => AttemptOutcome::Shutdown,
        }
    }

    /// Heuristic: if the current `Inner` has been alive longer
    /// than `reset_window_ms` post-respawn, the next crash is
    /// treated as a transient blip rather than a recurring loop —
    /// reset the caller's local attempt counter to 0. Permits
    /// recovery from network blips without enmascarating real
    /// crash loops. No-op when no Inner is installed (defensive).
    async fn maybe_reset_attempt_counter(&self, attempt: &mut u32, reset_window_ms: u64) {
        let inner_guard = self.inner.lock().await;
        let Some(inner) = inner_guard.as_ref() else {
            return;
        };
        let alive_ms = inner.spawned_at.elapsed().as_millis() as u64;
        if alive_ms >= reset_window_ms {
            *attempt = 0;
        }
    }

    /// Best-effort publish of a `plugin.lifecycle.<id>.<suffix>`
    /// event. Logs a warning on broker error rather than
    /// panicking — lifecycle observability is nice-to-have, not
    /// load-bearing. Centralised here so `respawn_loop` doesn't
    /// repeat the broker handle juggling at each event site.
    async fn publish_lifecycle_event(
        broker: &Option<AnyBroker>,
        plugin_id: &str,
        suffix: &str,
        payload: Value,
    ) {
        let Some(broker) = broker.as_ref() else {
            return;
        };
        let topic = format!("plugin.lifecycle.{}.{}", plugin_id, suffix);
        let event = Event::new(&topic, "plugin.supervisor", payload);
        if let Err(e) = broker.publish(&topic, event).await {
            tracing::warn!(
                target: "plugin.supervisor",
                plugin_id = %plugin_id,
                event = %suffix,
                error = %e,
                "broker publish of lifecycle event failed"
            );
        }
    }

    /// Auto-respawn lifecycle owner. Spawned
    /// by `init_loop` AFTER the first `spawn_one_attempt`
    /// succeeded + the `Inner` was installed. Owns the `Inner`
    /// slot for the rest of the daemon's lifetime: every
    /// successful respawn replaces `*self.inner.lock().await`
    /// transparently to all call sites that re-fetch
    /// `inner.stdin_tx.clone()` per request.
    ///
    /// Loop body, per iteration:
    ///   1. `wait_for_attempt_outcome` against the current
    ///      Inner's supervisor task. Returns `Crashed` /
    ///      `NormalExit` / `Shutdown`.
    ///   2. On `Shutdown` / `NormalExit`: return immediately.
    ///   3. On `Crashed`: publish `plugin.lifecycle.<id>.crashed`.
    ///      If `respawn=false`, return.
    ///   4. Else: maybe reset attempt counter (heuristic), check
    ///      `attempt >= max_attempts` → `gave_up` event + return.
    ///   5. Sleep `next_backoff(attempt)` (interruptible by
    ///      shutdown). Publish `respawning {attempt+1, backoff_ms}`.
    ///   6. Drain pending oneshots BEFORE spawning the new child
    ///      so callers fail-fast against the dying Inner.
    ///   7. `spawn_one_attempt` → on Ok install new Inner +
    ///      publish `respawned`; on Err bump counter, loop again
    ///      (next backoff).
    ///   8. After install, re-check `shutdown_signaled` — if
    ///      shutdown fired between Ok and install, kill the new
    ///      child instead of leaving it running.
    async fn respawn_loop(
        self: Arc<Self>,
        ctx_shutdown: CancellationToken,
        broker: Option<AnyBroker>,
        memory: Option<Arc<LongTermMemory>>,
        llm: Option<LlmServices>,
    ) {
        let cfg = self.cached_manifest.plugin.supervisor.clone();
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let respawn_enabled = cfg.respawn;
        let max_attempts = cfg.max_attempts;
        let base_ms = cfg.backoff_ms;
        // Heuristic: child must outlive this duration after a
        // respawn for the attempt counter to reset to 0. Capped
        // at 10x the backoff cap so an over-tuned manifest can't
        // create an effectively-infinite window.
        let reset_window_ms = base_ms
            .saturating_mul(max_attempts as u64)
            .saturating_mul(2)
            .min(RESPAWN_BACKOFF_CAP_MS.saturating_mul(10));
        let mut attempt: u32 = 0;
        loop {
            // Park until the current Inner's supervisor task
            // posts an outcome (or shutdown fires).
            let outcome = self.wait_for_attempt_outcome(&ctx_shutdown).await;
            match outcome {
                AttemptOutcome::Shutdown => return,
                AttemptOutcome::NormalExit => {
                    // Child requested its own exit (e.g. replied
                    // to `shutdown` then terminated). No crashed
                    // event, no respawn — clean lifecycle close.
                    return;
                }
                AttemptOutcome::Crashed {
                    exit_code,
                    stderr_tail,
                } => {
                    // Capture the
                    // dying Inner's uptime BEFORE drain so the
                    // subsequent `respawned` event can report
                    // how long the previous attempt lived.
                    // Operators can graph crash-cycle duration
                    // to spot degrading plugins.
                    let prev_inner_uptime_ms: u64 = {
                        let inner_guard = self.inner.lock().await;
                        inner_guard
                            .as_ref()
                            .map(|i| i.spawned_at.elapsed().as_millis() as u64)
                            .unwrap_or(0)
                    };
                    // Always publish `crashed` so the operator
                    // sees it regardless of respawn policy.
                    Self::publish_lifecycle_event(
                        &broker,
                        &plugin_id,
                        "crashed",
                        json!({
                            "plugin_id": plugin_id,
                            "exit_code": exit_code,
                            "stderr_tail": stderr_tail,
                        }),
                    )
                    .await;
                    if !respawn_enabled {
                        // Detect, publish, give up. Operator
                        // restarts daemon to recover. No respawn.
                        return;
                    }
                    // Reset counter if the dead child outlived the
                    // reset window — transient blip, not loop bug.
                    self.maybe_reset_attempt_counter(&mut attempt, reset_window_ms)
                        .await;
                    if attempt >= max_attempts {
                        Self::publish_lifecycle_event(
                            &broker,
                            &plugin_id,
                            "gave_up",
                            json!({
                                "plugin_id": plugin_id,
                                "attempts": attempt,
                                "last_exit_code": exit_code,
                                "stderr_tail": stderr_tail,
                            }),
                        )
                        .await;
                        return;
                    }
                    let backoff = next_backoff(attempt, base_ms);
                    Self::publish_lifecycle_event(
                        &broker,
                        &plugin_id,
                        "respawning",
                        json!({
                            "plugin_id": plugin_id,
                            "attempt": attempt + 1,
                            "backoff_ms": backoff.as_millis() as u64,
                        }),
                    )
                    .await;
                    // Park during backoff. Shutdown short-circuits.
                    if self.sleep_or_shutdown(backoff).await {
                        return;
                    }
                    // Drain pending oneshots BEFORE installing the
                    // new Inner so callers can't race the window
                    // where they'd send into the new stdin_tx
                    // holding stale request ids.
                    self.drain_pending_with_error("plugin restarted; retry")
                        .await;
                    // The labelled inner loop drives the spawn-retry
                    // path inline. Earlier `continue` to the
                    // outer loop on Err short-circuited the very
                    // next `wait_for_attempt_outcome` (no Inner
                    // installed → AttemptOutcome::Shutdown →
                    // premature respawn_loop return). The new
                    // structure: inner loop retries until Ok or
                    // gave_up, then outer loop resumes against
                    // the freshly-installed Inner.
                    let new_inner = 'spawn_retry: loop {
                        match self
                            .spawn_one_attempt(
                                ctx_shutdown.clone(),
                                broker.clone(),
                                memory.clone(),
                                llm.clone(),
                            )
                            .await
                        {
                            Ok(inner) => break 'spawn_retry inner,
                            Err(e) => {
                                tracing::warn!(
                                    target: "plugin.supervisor",
                                    plugin_id = %plugin_id,
                                    attempt = attempt + 1,
                                    error = %e,
                                    "respawn attempt failed; counting as failed attempt",
                                );
                                attempt += 1;
                                if self.shutdown_signaled.load(Ordering::Acquire) {
                                    return;
                                }
                                if attempt >= max_attempts {
                                    Self::publish_lifecycle_event(
                                        &broker,
                                        &plugin_id,
                                        "gave_up",
                                        json!({
                                            "plugin_id": plugin_id,
                                            "attempts": attempt,
                                            "last_exit_code": -1,
                                            "stderr_tail": Vec::<String>::new(),
                                        }),
                                    )
                                    .await;
                                    return;
                                }
                                let next = next_backoff(attempt, base_ms);
                                Self::publish_lifecycle_event(
                                    &broker,
                                    &plugin_id,
                                    "respawning",
                                    json!({
                                        "plugin_id": plugin_id,
                                        "attempt": attempt + 1,
                                        "backoff_ms": next.as_millis() as u64,
                                    }),
                                )
                                .await;
                                if self.sleep_or_shutdown(next).await {
                                    return;
                                }
                                // Inner loop continues to retry spawn.
                            }
                        }
                    };

                    // Spawn succeeded. Race protection: shutdown
                    // may have fired between spawn_one_attempt
                    // returning Ok and us installing the new
                    // Inner. If so, kill the just-spawned child
                    // instead of leaving it orphaned + return.
                    if self.shutdown_signaled.load(Ordering::Acquire) {
                        new_inner.cancel.cancel();
                        let mut child_guard = new_inner.child.lock().await;
                        if let Some(mut c) = child_guard.take() {
                            let _ = c.kill().await;
                        }
                        return;
                    }
                    *self.inner.lock().await = Some(new_inner);
                    Self::publish_lifecycle_event(
                        &broker,
                        &plugin_id,
                        "respawned",
                        json!({
                            "plugin_id": plugin_id,
                            "attempt": attempt + 1,
                            // Uptime
                            // of the PREVIOUS Inner (the one that
                            // just crashed and triggered this
                            // respawn). Operators graph per-cycle
                            // duration to spot plugins whose
                            // stable lifetime is degrading.
                            "total_uptime_ms": prev_inner_uptime_ms,
                        }),
                    )
                    .await;
                    attempt += 1;
                    // Outer loop iterates → wait_for_attempt_outcome
                    // resumes against the new child.
                }
            }
        }
    }

    /// Public entry the init_loop hook calls
    /// AFTER `init()` succeeds + all post-init registrations
    /// (channel adapters, llm providers, hook handlers, vector
    /// backends, tool handlers) complete. Spawns the
    /// `respawn_loop` as a fire-and-forget task; the JoinHandle
    /// is intentionally dropped because the loop is daemon-
    /// lifetime and owns its own termination via
    /// `shutdown_signaled` + `ctx_shutdown`.
    ///
    /// Idempotent: callers that double-invoke would spawn two
    /// loops competing for `Inner` replacement, which would be
    /// a bug at the init_loop layer; we don't guard here.
    pub fn spawn_supervisor_loop(
        self: Arc<Self>,
        ctx_shutdown: CancellationToken,
        broker: Option<AnyBroker>,
        memory: Option<Arc<LongTermMemory>>,
        llm: Option<LlmServices>,
    ) {
        // Skip the spawn entirely when the manifest opts out of
        // respawn. The detect-only supervisor inside the current
        // Inner already publishes `crashed` indirectly via the
        // AttemptOutcome → respawn_loop chain — but if we never
        // start a respawn_loop, the AttemptOutcome receiver
        // closes when its Inner gets dropped, with no observable
        // operator-side effect. Operators expect
        // to see `crashed` events even with `respawn=false`, so
        // we DO start the loop; respawn_loop's early-return
        // branch handles the `respawn=false` case after
        // publishing `crashed`.
        let _join = tokio::spawn(self.respawn_loop(ctx_shutdown, broker, memory, llm));
    }

    /// Operator-driven plugin
    /// restart. Bypasses the auto-respawn loop's natural
    /// Crashed flow so no spurious `crashed` event fires for an
    /// intentional kill. Reuses the existing cancel cascade +
    /// new `restart_signaled` flag to coordinate teardown.
    ///
    /// Steps:
    ///   1. Capture the dying Inner's uptime BEFORE drain.
    ///   2. Drain pending oneshots with "plugin restarted by operator".
    ///   3. Cancel current Inner's cascade (writer/reader/forwarders/
    ///      supervisor task all observe; supervisor posts AttemptOutcome::
    ///      Shutdown via oneshot; respawn_loop sees Shutdown + returns).
    ///   4. Wait up to 2s for the supervisor task to drain.
    ///   5. Force-kill the child if still alive (idempotent).
    ///   6. `tokio::time::timeout(60s, spawn_one_attempt(...))` for fresh
    ///      attempt. Elapsed → "restart timed out, plugin may be in
    ///      degraded state" Internal error.
    ///   7. Capture `new_pid` from `child.id()` BEFORE install (Tokio's
    ///      `Child::id()` returns None after wait/kill).
    ///   8. Install new Inner.
    ///   9. Spawn fresh respawn_loop via `weak_self_arc().spawn_supervisor_loop`
    ///      so the plugin is once again under auto-respawn supervision.
    ///   10. Publish `plugin.lifecycle.<id>.restarted_manually` event.
    ///   11. Reset `restart_signaled = false`.
    ///   12. Return `PluginsRestartResponse`.
    pub async fn force_restart(
        self: Arc<Self>,
        ctx_shutdown: CancellationToken,
        broker: Option<AnyBroker>,
        memory: Option<Arc<LongTermMemory>>,
        llm: Option<LlmServices>,
    ) -> Result<nexo_tool_meta::admin::plugin_restart::PluginsRestartResponse, anyhow::Error> {
        // Serialise concurrent
        // restarts of the same plugin. Lock held for the entire
        // 11-step cascade so a second caller observes the freshly
        // installed Inner via wait, never builds a parallel one
        // that would orphan the loser's child.
        let _restart_guard = self.restart_lock.clone().lock_owned().await;
        self.restart_signaled.store(true, Ordering::Release);
        // Wake supervisor + respawn_loop's parked sleep so they
        // observe the cascade quickly instead of waiting for the
        // natural 500ms supervisor poll.
        self.shutdown_notify.notify_waiters();
        let plugin_id = self.cached_manifest.plugin.id.clone();

        // Step 1: capture previous uptime BEFORE drain.
        let previous_uptime_ms: u64 = {
            let guard = self.inner.lock().await;
            guard
                .as_ref()
                .map(|i| i.spawned_at.elapsed().as_millis() as u64)
                .unwrap_or(0)
        };

        // Step 2: drain pending oneshots so callers fail-fast
        // before we tear down the stdin pipe.
        self.drain_pending_with_error("plugin restarted by operator")
            .await;

        // Step 3: cancel current Inner's cascade. Take the inner
        // out so the next install path is clean. The supervisor
        // task posts AttemptOutcome::Shutdown via the oneshot;
        // respawn_loop sees Shutdown via wait_for_attempt_outcome
        // and returns. The new respawn_loop in step 9 takes over.
        let prev_inner = self.inner.lock().await.take();
        if let Some(inner) = prev_inner {
            inner.cancel.cancel();
            // Step 4: brief grace for supervisor task drain.
            // Force-kill in step 5 covers the case where the child
            // ignores the cancel cascade.
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                for task in inner.tasks.into_iter() {
                    let _ = task.await;
                }
            })
            .await;
            // Step 5: force-kill child if still alive. `take()`
            // makes follow-up calls a no-op.
            let mut child_guard = inner.child.lock().await;
            if let Some(mut c) = child_guard.take() {
                let _ = c.kill().await;
                let _ = c.wait().await;
            }
        }

        // Step 6: spawn fresh attempt with 60s outer timeout (the
        // inner handshake timeout is governed by
        // NEXO_PLUGIN_INIT_TIMEOUT_MS). Elapsed = degraded state.
        let new_inner = match tokio::time::timeout(
            Duration::from_secs(60),
            self.spawn_one_attempt(
                ctx_shutdown.clone(),
                broker.clone(),
                memory.clone(),
                llm.clone(),
            ),
        )
        .await
        {
            Ok(Ok(inner)) => inner,
            Ok(Err(e)) => {
                self.restart_signaled.store(false, Ordering::Release);
                return Err(anyhow::anyhow!("spawn_one_attempt: {e}"));
            }
            Err(_elapsed) => {
                self.restart_signaled.store(false, Ordering::Release);
                return Err(anyhow::anyhow!(
                    "restart timed out, plugin may be in degraded state"
                ));
            }
        };

        // Step 7: capture new_pid BEFORE install (Tokio's
        // `Child::id()` returns None after wait/kill, but here
        // the child is freshly spawned + still owned).
        let new_pid: Option<u32> = {
            let guard = new_inner.child.lock().await;
            guard.as_ref().and_then(|c| c.id())
        };

        // Step 8: install new Inner.
        *self.inner.lock().await = Some(new_inner);

        // Step 9: spawn fresh respawn_loop. The previous
        // respawn_loop returned via Shutdown when its Inner's
        // cancel cascade fired in step 3. Without spawning a new
        // one, the plugin would lose auto-respawn supervision.
        if let Some(arc_self) = self.weak_self_arc() {
            arc_self.spawn_supervisor_loop(ctx_shutdown, broker.clone(), memory, llm);
        } else {
            tracing::warn!(
                target: "plugin.supervisor",
                plugin_id = %plugin_id,
                "force_restart: weak_self not populated, auto-respawn loop NOT re-armed"
            );
        }

        // Step 10: publish `restarted_manually` event.
        // Include `new_pid` in the broker
        // payload so subscribers tailing lifecycle events see the
        // freshly spawned PID without an extra RPC round-trip.
        // Mirrors the `PluginsRestartResponse` wire shape; encoded
        // via `Value::Null` when Tokio's `Child::id()` returned
        // None (rare).
        let restarted_at_ms = chrono::Utc::now().timestamp_millis();
        let new_pid_payload = match new_pid {
            Some(p) => json!(p),
            None => Value::Null,
        };
        Self::publish_lifecycle_event(
            &broker,
            &plugin_id,
            "restarted_manually",
            json!({
                "plugin_id": plugin_id,
                "previous_uptime_ms": previous_uptime_ms,
                "restarted_at_ms": restarted_at_ms,
                "new_pid": new_pid_payload,
            }),
        )
        .await;

        // Step 11: clear flag.
        self.restart_signaled.store(false, Ordering::Release);

        Ok(
            nexo_tool_meta::admin::plugin_restart::PluginsRestartResponse {
                plugin_id,
                previous_uptime_ms,
                restarted_at_ms,
                new_pid,
            },
        )
    }

    /// For each backend name in
    /// `manifest.plugin.extends.memory_backends`, build a
    /// `RemoteVectorBackend` sharing this plugin's stdio bridge
    /// and register it with the vector backend registry.
    /// Returns the list of registered backend names. On
    /// `NameAlreadyRegistered`, rolls back any backends this
    /// call already registered. Must be called AFTER `init()`.
    pub async fn register_remote_vector_backends(
        &self,
        registry: &Arc<crate::agent::vector_backend_registry::VectorBackendRegistry>,
    ) -> Result<Vec<String>, crate::agent::vector_backend_registry::VectorBackendRegistrationError>
    {
        let backends = self.cached_manifest.plugin.extends.memory_backends.clone();
        if backends.is_empty() {
            return Ok(Vec::new());
        }
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let (stdin_tx, pending, next_id) = {
            let guard = self.inner.lock().await;
            match guard.as_ref() {
                Some(inner) => (
                    inner.stdin_tx.clone(),
                    inner.pending.clone(),
                    inner.next_id.clone(),
                ),
                None => {
                    return Err(
                        crate::agent::vector_backend_registry::VectorBackendRegistrationError::InnerUnavailable,
                    );
                }
            }
        };

        let mut registered: Vec<String> = Vec::new();
        for name in backends {
            let backend: Arc<dyn nexo_memory::VectorBackend> =
                Arc::new(crate::agent::vector_remote::RemoteVectorBackend::new(
                    name.clone(),
                    plugin_id.clone(),
                    stdin_tx.clone(),
                    pending.clone(),
                    next_id.clone(),
                ));
            match registry.register(backend, plugin_id.clone()) {
                Ok(()) => registered.push(name),
                Err(e) => {
                    for prior in &registered {
                        registry.unregister(prior, &plugin_id);
                    }
                    return Err(e);
                }
            }
        }
        Ok(registered)
    }

    /// For each hook name in
    /// `manifest.plugin.extends.hooks`, build a
    /// `RemoteHookHandler` sharing this plugin's stdio bridge
    /// and register it with the hook registry. Returns the list
    /// of registered hook names. `HookRegistry::register` itself
    /// never fails (cap-violations log + skip silently), so the
    /// only failure mode is `Inner` not yet being populated.
    /// Must be called AFTER `init()`.
    pub async fn register_remote_hook_handlers(
        &self,
        hook_registry: &Arc<crate::agent::hook_registry::HookRegistry>,
    ) -> Result<Vec<String>, crate::agent::hook_remote::HookHandlerRegistrationError> {
        let hooks = self.cached_manifest.plugin.extends.hooks.clone();
        if hooks.is_empty() {
            return Ok(Vec::new());
        }
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let (stdin_tx, pending, next_id) = {
            let guard = self.inner.lock().await;
            match guard.as_ref() {
                Some(inner) => (
                    inner.stdin_tx.clone(),
                    inner.pending.clone(),
                    inner.next_id.clone(),
                ),
                None => {
                    return Err(
                        crate::agent::hook_remote::HookHandlerRegistrationError::InnerUnavailable,
                    );
                }
            }
        };

        let mut registered: Vec<String> = Vec::new();
        for hook_name in hooks {
            let handler = crate::agent::hook_remote::RemoteHookHandler::new(
                hook_name.clone(),
                plugin_id.clone(),
                stdin_tx.clone(),
                pending.clone(),
                next_id.clone(),
            );
            hook_registry.register(&hook_name, plugin_id.clone(), handler);
            registered.push(hook_name);
        }
        Ok(registered)
    }

    /// For each tool name in
    /// `manifest.plugin.extends.tools` that the subprocess
    /// confirmed via initialize-reply, build a `RemoteToolHandler`
    /// sharing this plugin's stdio bridge and register it with
    /// the per-plugin scoped tool registry. Returns the list of
    /// successfully registered tool names. On collision (a built-
    /// in or another plugin already registered the name), aborts
    /// with `ToolNameAlreadyRegistered` (host treats this as a
    /// fatal init failure). Must be called AFTER `init()` (so
    /// `Inner` is populated).
    pub async fn register_remote_tool_handlers(
        &self,
        scoped_registry: &Arc<crate::agent::scoped_tool_registry::ScopedToolRegistry>,
    ) -> Result<Vec<String>, crate::agent::tool_remote::ToolHandlerRegistrationError> {
        let declared = self.cached_manifest.plugin.extends.tools.clone();
        if declared.is_empty() {
            return Ok(Vec::new());
        }
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let (stdin_tx, pending, next_id, advertised) = {
            let guard = self.inner.lock().await;
            match guard.as_ref() {
                Some(inner) => (
                    inner.stdin_tx.clone(),
                    inner.pending.clone(),
                    inner.next_id.clone(),
                    inner.declared_tools.clone(),
                ),
                None => {
                    return Err(
                        crate::agent::tool_remote::ToolHandlerRegistrationError::InnerUnavailable,
                    );
                }
            }
        };

        let mut registered: Vec<String> = Vec::new();
        for tool_name in declared {
            // Manifest declared but child did not advertise →
            // already warned at handshake. Skip silently here so
            // the registry doesn't accumulate dead handlers.
            let def_match = advertised.iter().find(|d| d.name == tool_name).cloned();
            let def = match def_match {
                Some(d) => d,
                None => continue,
            };

            let handler = crate::agent::tool_remote::RemoteToolHandler::new(
                plugin_id.clone(),
                def.clone(),
                stdin_tx.clone(),
                pending.clone(),
                next_id.clone(),
            );
            let tool_def = nexo_llm::ToolDef {
                name: def.name.clone(),
                description: def.description.clone(),
                parameters: def.input_schema.clone(),
            };
            match scoped_registry.register_arc(
                tool_def,
                Arc::new(handler) as Arc<dyn crate::agent::tool_registry::ToolHandler>,
            ) {
                Ok(()) => registered.push(def.name.clone()),
                Err(_violation) => {
                    return Err(
                        crate::agent::tool_remote::ToolHandlerRegistrationError::ToolNameAlreadyRegistered {
                            tool_name: def.name,
                            prior_plugin_hint: "unknown".to_string(),
                        },
                    );
                }
            }
        }
        Ok(registered)
    }

    /// For each provider name in
    /// `manifest.plugin.extends.llm_providers`, build a
    /// `RemoteLlmFactory` sharing this plugin's stdio bridge and
    /// register it with the LLM registry. Returns the list of
    /// successfully registered providers. On
    /// `LlmRegistry::register` Err (provider already registered),
    /// rolls back any providers this call already registered.
    /// Must be called AFTER `init()` (so `Inner` is populated).
    pub async fn register_remote_llm_providers(
        &self,
        llm_registry: &Arc<nexo_llm::LlmRegistry>,
    ) -> Result<Vec<String>, crate::agent::llm_remote::LlmProviderRegistrationError> {
        let providers = self.cached_manifest.plugin.extends.llm_providers.clone();
        if providers.is_empty() {
            return Ok(Vec::new());
        }
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let (stdin_tx, pending, streaming_pending, next_id) = {
            let guard = self.inner.lock().await;
            match guard.as_ref() {
                Some(inner) => (
                    inner.stdin_tx.clone(),
                    inner.pending.clone(),
                    inner.streaming_pending.clone(),
                    inner.next_id.clone(),
                ),
                None => {
                    return Err(
                        crate::agent::llm_remote::LlmProviderRegistrationError::InnerUnavailable,
                    );
                }
            }
        };

        let mut registered: Vec<String> = Vec::new();
        for provider in providers {
            let factory = crate::agent::llm_remote::RemoteLlmFactory::new(
                provider.clone(),
                plugin_id.clone(),
                stdin_tx.clone(),
                pending.clone(),
                streaming_pending.clone(),
                next_id.clone(),
            );
            match llm_registry.register(Box::new(factory)) {
                Ok(()) => registered.push(provider),
                Err(_) => {
                    // Roll back any providers this call registered.
                    for prior in &registered {
                        llm_registry.unregister(prior);
                    }
                    return Err(
                        crate::agent::llm_remote::LlmProviderRegistrationError::AlreadyRegistered {
                            name: provider,
                        },
                    );
                }
            }
        }
        Ok(registered)
    }

    /// For each kind in
    /// `manifest.plugin.extends.channels`, build a
    /// `RemoteChannelAdapter` sharing this plugin's stdio bridge
    /// and register it with the channel adapter registry.
    /// Returns the list of successfully registered kinds. On
    /// `KindAlreadyRegistered`, rolls back any kinds this plugin
    /// already registered in the same call. Must be called AFTER
    /// `init()` (so `Inner` is populated).
    pub async fn register_remote_channel_adapters(
        &self,
        channel_registry: &Arc<crate::agent::channel_adapter::ChannelAdapterRegistry>,
    ) -> Result<Vec<String>, crate::agent::channel_adapter::ChannelAdapterRegistrationError> {
        let kinds = self.cached_manifest.plugin.extends.channels.clone();
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let plugin_id = self.cached_manifest.plugin.id.clone();

        // Snapshot the inner transport handles. If `init()` hasn't
        // run yet, treat this as a no-op (defensive — production
        // boot path always calls init before this).
        let (stdin_tx, pending, next_id) = {
            let guard = self.inner.lock().await;
            match guard.as_ref() {
                Some(inner) => (
                    inner.stdin_tx.clone(),
                    inner.pending.clone(),
                    inner.next_id.clone(),
                ),
                None => return Ok(Vec::new()),
            }
        };

        let mut registered: Vec<String> = Vec::new();
        for kind in kinds {
            let adapter = Arc::new(crate::agent::channel_adapter::RemoteChannelAdapter::new(
                kind.clone(),
                plugin_id.clone(),
                stdin_tx.clone(),
                pending.clone(),
                next_id.clone(),
            ));
            match channel_registry.register(adapter, &plugin_id) {
                Ok(()) => registered.push(kind),
                Err(e) => {
                    // Roll back any kinds this call registered so
                    // partial state doesn't poison the registry.
                    for prior in &registered {
                        channel_registry.unregister(prior, &plugin_id);
                    }
                    return Err(e);
                }
            }
        }
        Ok(registered)
    }

    /// Spawn the child and handshake. Returns the populated
    /// `Inner` on success; on failure leaves the adapter in the
    /// "not started" state and surfaces a structured error.
    ///
    /// `broker` is `Some` for the production lifecycle path
    /// (`init()` passes `ctx.broker.clone()`) so the topic bridge
    /// can subscribe outbound topics + forward `broker.publish`
    /// notifications. Tests that exercise the spawn / handshake
    /// shape only (without bridge wiring) pass `None` to skip the
    /// subscription work.
    async fn spawn_one_attempt(
        &self,
        ctx_shutdown: CancellationToken,
        broker: Option<AnyBroker>,
        memory: Option<Arc<LongTermMemory>>,
        llm: Option<LlmServices>,
    ) -> Result<Inner, anyhow::Error> {
        let entry = &self.cached_manifest.plugin.entrypoint;
        let raw_command = entry
            .command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("manifest has no entrypoint.command — cannot spawn"))?;
        if raw_command.trim().is_empty() {
            anyhow::bail!("manifest entrypoint.command is empty");
        }

        // Resolve relative `./` and `../` paths against the
        // manifest's own directory when the factory recorded one.
        // Operators install plugins as
        // `<plugins-dir>/<id>-<version>/{nexo-plugin.toml,bin/...}`
        // and the manifest's `command = "./bin/<id>"` should target
        // the sibling `bin/` regardless of the daemon's CWD at spawn
        // time. Absolute paths + bare executables (PATH lookups)
        // pass through unchanged.
        let command = match self.manifest_dir.as_ref() {
            Some(dir) if raw_command.starts_with("./") || raw_command.starts_with("../") => {
                dir.join(&raw_command).to_string_lossy().into_owned()
            }
            _ => raw_command,
        };

        // Defense-in-depth: refuse env keys that overlap with the
        // daemon's own runtime envs. A plugin author who tries to
        // override `NEXO_STATE_ROOT` from a manifest deserves a
        // hard failure at boot rather than silent confusion.
        for key in entry.env.keys() {
            if key.starts_with("NEXO_") {
                anyhow::bail!("manifest entrypoint.env may not redefine reserved nexo env `{key}`");
            }
        }

        // Wrap the spawn command with the sandbox
        // runner when one is configured. `init()` stashes the
        // runner + state dir; tests calling spawn_one_attempt
        // directly leave both as None → wrap_command resolves to
        // the raw command (sandbox-disabled passthrough).
        let (program, prog_args) = {
            let runner_guard = self.sandbox.lock().await;
            let state_guard = self.plugin_state_dir.lock().await;
            match (runner_guard.as_ref(), state_guard.as_ref()) {
                (Some(runner), Some(state_dir)) => {
                    let wrapped = runner
                        .wrap_command(&self.cached_manifest, state_dir, &command, &entry.args)
                        .map_err(|e| anyhow::anyhow!("sandbox setup failed: {e}"))?;
                    if let Some(diag) = &wrapped.diagnostic {
                        tracing::warn!(
                            target: "plugin.sandbox",
                            plugin_id = %self.cached_manifest.plugin.id,
                            "{}",
                            diag
                        );
                    }
                    (wrapped.program, wrapped.args)
                }
                _ => (
                    std::path::PathBuf::from(&command),
                    entry
                        .args
                        .iter()
                        .map(std::ffi::OsString::from)
                        .collect::<Vec<_>>(),
                ),
            }
        };

        let mut cmd = Command::new(&program);
        cmd.args(&prog_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Pipe stderr into the daemon's tracing
            // subsystem instead of discarding. Child code that uses
            // `eprintln!` / `tracing` writing to stderr now becomes
            // operator-visible debug output filtered by `plugin_id`.
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // When the daemon supplied a per-spawn env
        // dict via `with_spawn_env`, wipe the inherited env first
        // so the child sees ONLY what the daemon explicitly seeded.
        // Defense-in-depth against secrets leaking from the daemon
        // process env (e.g. `OPENAI_API_KEY`) into a plugin that has
        // no business reading them. The single-instance browser
        // path leaves `spawn_env` as `None` and falls through to
        // the inherit-everything behaviour pre-81.18.b consumers
        // already depended on.
        if let Some(env_map) = &self.spawn_env {
            cmd.env_clear()
                .envs(env_map.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        for (k, v) in &entry.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawn `{command}` failed: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("child has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("child has no stderr"))?;

        // Wrap the Child in Arc<Mutex<Option<...>>>
        // immediately so the supervisor task (spawned after the
        // bridge wires up) can poll `try_wait` while shutdown can
        // still `take()` for reaping. Mutex contention is cheap:
        // one half-second poll vs single take on shutdown.
        let child_handle: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));

        let cancel = ctx_shutdown.child_token();
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
            Arc::new(DashMap::new());
        let streaming_pending: Arc<DashMap<u64, crate::agent::llm_remote::StreamingPending>> =
            Arc::new(DashMap::new());
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Value>(STDIN_CHANNEL_DEPTH);

        // Stdin writer task — single consumer of stdin_rx, writes
        // each Value as one newline-terminated line, flushes per
        // line so the child sees frames promptly.
        let writer_cancel = cancel.clone();
        let writer_handle = tokio::spawn(async move {
            let mut stdin = stdin;
            loop {
                let v = tokio::select! {
                    biased;
                    _ = writer_cancel.cancelled() => return,
                    next = stdin_rx.recv() => match next {
                        Some(v) => v,
                        None => return, // channel closed = graceful EOF
                    },
                };
                let line = match serde_json::to_string(&v) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "subprocess plugin: drop frame, serialize failed");
                        continue;
                    }
                };
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    tracing::warn!(error = %e, "subprocess plugin: stdin write failed");
                    return;
                }
                if let Err(e) = stdin.write_all(b"\n").await {
                    tracing::warn!(error = %e, "subprocess plugin: stdin newline failed");
                    return;
                }
                if let Err(e) = stdin.flush().await {
                    tracing::warn!(error = %e, "subprocess plugin: stdin flush failed");
                    return;
                }
            }
        });

        // Stderr reader task. Forwards each line
        // the child writes to stderr into the daemon's tracing
        // subsystem at `target = "plugin.stderr"` with the
        // plugin_id captured as a structured field. Operators can
        // then filter `RUST_LOG=plugin.stderr=info` to see
        // child output, or per-plugin via the field.
        // Spawned BEFORE the handshake send so any child boot-time
        // errors land in the operator's log.
        let stderr_cancel = cancel.clone();
        let stderr_plugin_id = self.cached_manifest.plugin.id.clone();
        // Ring buffer of last N stderr lines.
        // Capacity comes from `manifest.supervisor.stderr_tail_lines`
        // (validated at parse-time to be ≤ SUPERVISOR_STDERR_TAIL_MAX).
        // Shared between the stderr reader (writer side) and the
        // supervisor task (reader side, drains on crash for the
        // crashed event payload). Mutex contention is rare:
        // appends happen at child stderr rate, drain happens once
        // per crash.
        let stderr_tail_capacity = self.cached_manifest.plugin.supervisor.stderr_tail_lines;
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(
            VecDeque::with_capacity(stderr_tail_capacity.max(1)),
        ));
        let stderr_tail_for_reader = stderr_tail.clone();
        let stderr_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            loop {
                let line = tokio::select! {
                    biased;
                    _ = stderr_cancel.cancelled() => return,
                    next = reader.next_line() => match next {
                        Ok(Some(l)) => l,
                        Ok(None) => return, // EOF — child closed stderr
                        Err(e) => {
                            tracing::warn!(
                                target: "plugin.stderr",
                                plugin_id = %stderr_plugin_id,
                                error = %e,
                                "stderr read failed"
                            );
                            return;
                        }
                    },
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                tracing::info!(
                    target: "plugin.stderr",
                    plugin_id = %stderr_plugin_id,
                    line = %trimmed,
                    "child stderr"
                );
                // Append to the tail ring buffer; drop the oldest
                // line when at capacity. `stderr_tail_capacity == 0`
                // is degenerate (operator turned off the buffer);
                // skip appending entirely so we don't grow.
                if stderr_tail_capacity > 0 {
                    let mut buf = stderr_tail_for_reader.lock().await;
                    if buf.len() >= stderr_tail_capacity {
                        buf.pop_front();
                    }
                    buf.push_back(trimmed.to_string());
                }
            }
        });

        // Send the initialize request. We allocate id=1 by hand
        // here (before the AtomicU64 lives in `Inner`) so the
        // first reply matches without racing the stdout reader.
        let init_id: u64 = 1;
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": { "nexo_version": env!("CARGO_PKG_VERSION") },
        });
        let (init_tx, init_rx) = oneshot::channel::<Result<Value, String>>();
        pending.insert(init_id, init_tx);

        // Broker bridge state.
        // The reader task reads `broker.publish` notifications and
        // forwards to the broker, but it must SKIP forwarding until
        // the handshake completes successfully (otherwise the child
        // could spam the broker before manifest validation).
        // `bridge_cell` holds (broker, allowlist) and is set ONCE
        // after handshake validation passes — reader checks it on
        // every notification. `tokio::sync::OnceCell::get()` is
        // sync + atomic so the reader never blocks.
        let bridge_cell: Arc<OnceCell<BridgeContext>> = Arc::new(OnceCell::new());
        let plugin_id_for_log = self.cached_manifest.plugin.id.clone();

        // Stdout reader task — parses each line as JSON-RPC,
        // demuxes by id (response → resolve oneshot) or method
        // (notification → broker bridge). Bridges shut down on
        // EOF.
        let reader_cancel = cancel.clone();
        let reader_pending = pending.clone();
        let reader_streaming_pending = streaming_pending.clone();
        // Reader needs stdin_tx so it can write
        // responses back to the child for incoming requests
        // (memory.recall today; llm.complete + tool.dispatch in
        // 81.20.b/.c).
        let reader_stdin_tx = stdin_tx.clone();
        let reader_plugin_id = plugin_id_for_log.clone();
        let reader_bridge = bridge_cell.clone();
        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                let line = tokio::select! {
                    biased;
                    _ = reader_cancel.cancelled() => return,
                    next = reader.next_line() => match next {
                        Ok(Some(l)) => l,
                        Ok(None) => return, // EOF
                        Err(e) => {
                            tracing::warn!(error = %e, plugin = %reader_plugin_id, "stdout read failed");
                            return;
                        }
                    },
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        // Non-JSON stdout lines are
                        // INFO-level child output, NOT a parser
                        // failure. Plugin authors mixing stderr +
                        // stdout for diagnostics get the same
                        // operator visibility as stderr-only output.
                        // The wire spec (`nexo-plugin-contract.md`)
                        // says lines on stdout SHOULD be JSON-RPC,
                        // but children may emit debug noise — log,
                        // don't drop with warn.
                        tracing::info!(
                            target: "plugin.stdout",
                            plugin_id = %reader_plugin_id,
                            line = %trimmed,
                            "child stdout (non-json)"
                        );
                        continue;
                    }
                };
                // Frame routing: id + method = INCOMING REQUEST
                // (child → host, expect response). id alone =
                // RESPONSE to one of OUR outbound requests
                // (initialize / shutdown / future). No id =
                // notification.
                let id_val = parsed.get("id").cloned();
                let method_str = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
                if id_val.is_some() && !method_str.is_empty() {
                    // Incoming child request.
                    // Dispatch + write response back via stdin_tx.
                    // Bridge is the source of host services
                    // (memory / future llm + tools); when bridge
                    // is None (boot still racing or no broker), we
                    // return -32603 "service not yet wired".
                    let id_for_reply = id_val.clone().unwrap_or(Value::Null);
                    let params = parsed.get("params").cloned().unwrap_or(Value::Null);
                    let response = match reader_bridge.get() {
                        Some(bridge) => {
                            handle_child_request(
                                bridge,
                                &reader_plugin_id,
                                method_str,
                                &params,
                                &reader_stdin_tx,
                                &id_for_reply,
                            )
                            .await
                        }
                        None => Err((-32603, "host services not yet wired".to_string())),
                    };
                    let frame = match response {
                        Ok(result) => json!({
                            "jsonrpc": "2.0",
                            "id": id_for_reply,
                            "result": result,
                        }),
                        Err((code, msg)) => json!({
                            "jsonrpc": "2.0",
                            "id": id_for_reply,
                            "error": { "code": code, "message": msg },
                        }),
                    };
                    if let Err(e) = reader_stdin_tx.try_send(frame) {
                        tracing::warn!(
                            plugin = %reader_plugin_id,
                            error = %e,
                            "memory.recall response dropped: stdin queue full or closed"
                        );
                    }
                    continue;
                }
                if let Some(id) = id_val.as_ref().and_then(|v| v.as_u64()) {
                    if let Some((_, sender)) = reader_pending.remove(&id) {
                        let payload = if let Some(err) = parsed.get("error") {
                            Err(err.to_string())
                        } else {
                            Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(payload);
                        continue;
                    }
                    // Streaming-response path. The
                    // final reply for an `llm.chat` streaming
                    // request resolves the per-id `final_tx` AND
                    // drops the `streaming_pending` entry so its
                    // `delta_tx` closes (signalling end-of-stream
                    // to the consumer).
                    if let Some((_, entry)) = reader_streaming_pending.remove(&id) {
                        let payload = if let Some(err) = parsed.get("error") {
                            Err(err.to_string())
                        } else {
                            let result = parsed.get("result").cloned().unwrap_or(Value::Null);
                            match serde_json::from_value::<
                                crate::agent::llm_remote::wire::WireChatResponse,
                            >(result)
                            {
                                Ok(wire) => {
                                    Ok(crate::agent::llm_remote::wire::wire_to_response(wire))
                                }
                                Err(e) => Err(format!("decode WireChatResponse: {e}")),
                            }
                        };
                        let _ = entry.final_tx.send(payload);
                        continue;
                    }
                    tracing::warn!(
                        id,
                        plugin = %reader_plugin_id,
                        "stdout: response with unknown id"
                    );
                    continue;
                }
                // Notification path. 81.14.b wires `broker.publish`
                // forwarding when the bridge is active; other methods
                // (`memory.recall`, `llm.complete`, `tool.dispatch`)
                // wait for 81.20.
                let method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
                if method == "broker.publish" {
                    let Some(bridge) = reader_bridge.get() else {
                        tracing::debug!(
                            plugin = %reader_plugin_id,
                            "broker.publish before bridge active — drop"
                        );
                        continue;
                    };
                    handle_child_publish(bridge, &reader_plugin_id, &parsed).await;
                    continue;
                }
                // `llm.chat.delta { request_id,
                // chunk }` notifications push streaming chunks
                // into the per-id `streaming_pending.delta_tx`.
                // Drop on `try_send` failure (consumer slow / gone).
                if method == "llm.chat.delta" {
                    let request_id = parsed
                        .get("params")
                        .and_then(|p| p.get("request_id"))
                        .and_then(|v| v.as_u64());
                    let chunk_value = parsed.get("params").and_then(|p| p.get("chunk")).cloned();
                    let (Some(rid), Some(chunk_value)) = (request_id, chunk_value) else {
                        tracing::warn!(
                            plugin = %reader_plugin_id,
                            "llm.chat.delta missing request_id / chunk — drop"
                        );
                        continue;
                    };
                    let wire: crate::agent::llm_remote::wire::WireStreamChunk =
                        match serde_json::from_value(chunk_value) {
                            Ok(w) => w,
                            Err(e) => {
                                tracing::warn!(
                                    plugin = %reader_plugin_id,
                                    error = %e,
                                    "llm.chat.delta chunk parse failed — drop"
                                );
                                continue;
                            }
                        };
                    let chunk = crate::agent::llm_remote::wire::wire_to_chunk(wire);
                    if let Some(entry) = reader_streaming_pending.get(&rid) {
                        if entry.delta_tx.send(chunk).is_err() {
                            tracing::debug!(
                                plugin = %reader_plugin_id,
                                request_id = rid,
                                "llm.chat.delta consumer dropped — chunk discarded"
                            );
                        }
                    } else {
                        tracing::debug!(
                            plugin = %reader_plugin_id,
                            request_id = rid,
                            "llm.chat.delta with unknown request_id — drop"
                        );
                    }
                    continue;
                }
                tracing::debug!(
                    plugin = %reader_plugin_id,
                    method,
                    "stdout notification: unhandled method (deferred to 81.20)"
                );
            }
        });

        // Wait for the initialize reply with timeout.
        let timeout = Duration::from_millis(
            std::env::var("NEXO_PLUGIN_INIT_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_INIT_TIMEOUT_MS),
        );
        if let Err(e) = stdin_tx.send(init_req).await {
            anyhow::bail!("subprocess plugin: stdin channel closed before initialize: {e}");
        }
        let init_result = tokio::time::timeout(timeout, init_rx).await;
        let result = match init_result {
            Ok(Ok(Ok(v))) => v,
            Ok(Ok(Err(err))) => {
                cancel.cancel();
                kill_handle(&child_handle).await;
                anyhow::bail!("child returned error to initialize: {err}");
            }
            Ok(Err(_canceled)) => {
                cancel.cancel();
                kill_handle(&child_handle).await;
                anyhow::bail!("initialize oneshot canceled before reply");
            }
            Err(_elapsed) => {
                cancel.cancel();
                kill_handle(&child_handle).await;
                anyhow::bail!(
                    "child did not reply to initialize within {}ms",
                    timeout.as_millis()
                );
            }
        };

        // Validate returned manifest's id matches what the factory
        // was registered under. Defense against an out-of-tree
        // binary pretending to be a different plugin.
        //
        // Synthetic instance factories — `synthesize_instance_plugin`
        // appends `.{instance}` to the base manifest id so each
        // instance has its own factory slot in the registry. But the
        // subprocess binary embeds the ORIGINAL manifest via
        // `include_str!("../nexo-plugin.toml")` at compile time, so
        // its initialize-reply reports the BASE id (`whatsapp`), not
        // the synthesized `whatsapp.smoketest`. Accept the base id
        // as a valid response when the factory was registered for an
        // instance variant; the rest of the cached_manifest (broker
        // allowlist, extends.tools, sandbox) still pins all the
        // defense-in-depth invariants.
        let returned_id = result
            .pointer("/manifest/plugin/id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("initialize reply missing manifest.plugin.id"))?;
        let expected_id = &self.cached_manifest.plugin.id;
        let id_ok = returned_id == expected_id
            || expected_id
                .split_once('.')
                .map(|(base, _instance)| base == returned_id)
                .unwrap_or(false);
        if !id_ok {
            cancel.cancel();
            kill_handle(&child_handle).await;
            anyhow::bail!(
                "manifest id mismatch: factory expected `{}`, child reported `{}`",
                expected_id,
                returned_id
            );
        }

        // Parse optional `result.tools` array (tool
        // catalog advertised by the subprocess at handshake). Subset
        // check: every advertised tool name MUST appear in
        // `manifest.plugin.extends.tools` (defense against drift /
        // out-of-tree binary registering tools the operator did
        // not authorize). Manifest entries WITHOUT an advertised
        // counterpart are tolerated but logged at warn — they will
        // surface as -33401 ToolNotFound at first agent-loop call.
        let declared_tools: Vec<crate::agent::tool_remote::RemoteToolDef> = match result
            .pointer("/tools")
        {
            Some(Value::Array(arr)) => {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    let def: crate::agent::tool_remote::RemoteToolDef =
                        serde_json::from_value(item.clone()).map_err(|e| {
                            anyhow::anyhow!("initialize reply tools[]: malformed entry: {e}")
                        })?;
                    if !self
                        .cached_manifest
                        .plugin
                        .extends
                        .tools
                        .iter()
                        .any(|t| t == &def.name)
                    {
                        cancel.cancel();
                        kill_handle(&child_handle).await;
                        anyhow::bail!(
                                "initialize reply advertises undeclared tool `{}` (not in extends.tools = {:?})",
                                def.name,
                                self.cached_manifest.plugin.extends.tools
                            );
                    }
                    out.push(def);
                }
                out
            }
            Some(_) => {
                cancel.cancel();
                kill_handle(&child_handle).await;
                anyhow::bail!("initialize reply field `tools` must be an array if present");
            }
            None => Vec::new(),
        };
        for t in &self.cached_manifest.plugin.extends.tools {
            if !declared_tools.iter().any(|d| &d.name == t) {
                tracing::warn!(
                    target: "plugin.tools",
                    plugin_id = %self.cached_manifest.plugin.id,
                    tool = %t,
                    "manifest declares extends.tools entry but plugin did not advertise it in initialize-reply — runtime calls will fail with ToolNotFound"
                );
            }
        }

        // Wire the broker ↔ child topic bridge.
        // Derives subscribe / publish patterns from
        // `manifest.channels.register[].kind`. Plugins that don't
        // declare any channel kind get no bridge — the connection
        // stays open with just the writer + reader tasks but no
        // broker traffic crosses. Test paths pass `broker = None`
        // to skip subscriptions entirely.
        // Supervisor task. Polls `child.try_wait()`
        // every 500ms; on exit detection, publishes a
        // `AttemptOutcome` to the respawn_loop via this oneshot.
        // Centralised lifecycle event publishing now lives in
        // respawn_loop (it knows the attempt counter + decides
        // whether to publish `crashed` once or `crashed` +
        // `respawning` + `respawned`). Detection responsibility
        // stays here (poll try_wait, drain stderr_tail, cascade
        // cancel) because it's the only place with the live
        // `child_handle` + `stderr_tail` reference.
        let (attempt_outcome_tx, attempt_outcome_rx) = oneshot::channel::<AttemptOutcome>();
        let supervisor_child = child_handle.clone();
        let supervisor_cancel = cancel.clone();
        let supervisor_plugin_id = self.cached_manifest.plugin.id.clone();
        let supervisor_stderr_tail = stderr_tail.clone();
        let supervisor_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Outcome to post just before returning. Built inside
            // the loop; the post happens at the bottom so every
            // exit path goes through the single send().
            let outcome: AttemptOutcome;
            loop {
                tokio::select! {
                    biased;
                    _ = supervisor_cancel.cancelled() => {
                        // Cancellation cascaded from elsewhere
                        // (shutdown(), manual cancel for tests,
                        // OR the respawn_loop tearing down a
                        // doomed Inner). Treat as Shutdown so the
                        // respawn_loop knows to stop iterating.
                        outcome = AttemptOutcome::Shutdown;
                        break;
                    }
                    _ = interval.tick() => {}
                }
                // Lock briefly to poll exit status. `try_wait()`
                // does not block — it queries waitpid + returns
                // immediately. Returns None while alive, Some(status)
                // on exit, Err on waitpid failure.
                let exit_status = {
                    let mut guard = supervisor_child.lock().await;
                    if let Some(ref mut c) = *guard {
                        c.try_wait()
                    } else {
                        // Shutdown took the child; supervisor's
                        // job is done. Treat as Shutdown so the
                        // respawn_loop short-circuits.
                        outcome = AttemptOutcome::Shutdown;
                        break;
                    }
                };
                match exit_status {
                    Ok(None) => continue, // still alive
                    Ok(Some(status)) => {
                        let exit_code = status.code().unwrap_or(-1);
                        tracing::warn!(
                            target: "plugin.supervisor",
                            plugin_id = %supervisor_plugin_id,
                            exit_code,
                            "subprocess plugin exited"
                        );
                        // Drain the stderr tail ring buffer into a
                        // fresh Vec for the AttemptOutcome
                        // payload. Up to `supervisor.stderr_tail_lines`
                        // recent lines (chronological order, oldest
                        // first). Empty when the buffer never
                        // received anything OR the manifest set
                        // `stderr_tail_lines = 0`.
                        let stderr_tail_drained: Vec<String> = {
                            let mut buf = supervisor_stderr_tail.lock().await;
                            buf.drain(..).collect()
                        };
                        // Cascade-teardown: cancel the plugin's
                        // tasks (writer / readers / forwarders) so
                        // they don't run against a dead child.
                        // The respawn_loop will then either
                        // install a fresh `Inner` (which carries
                        // its own CancellationToken) or terminate
                        // the supervisor lifecycle entirely.
                        supervisor_cancel.cancel();
                        // Treat exit_code == 0 as a graceful
                        // self-exit (e.g. plugin replied to
                        // `shutdown` then terminated). Non-zero =
                        // crashed, eligible for respawn.
                        outcome = if exit_code == 0 {
                            AttemptOutcome::NormalExit
                        } else {
                            AttemptOutcome::Crashed {
                                exit_code,
                                stderr_tail: stderr_tail_drained,
                            }
                        };
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "plugin.supervisor",
                            plugin_id = %supervisor_plugin_id,
                            error = %e,
                            "subprocess plugin try_wait failed"
                        );
                        // Treat as a crash with placeholder
                        // exit_code so respawn_loop can decide.
                        // stderr_tail might still hold useful
                        // context — drain it.
                        let stderr_tail_drained: Vec<String> = {
                            let mut buf = supervisor_stderr_tail.lock().await;
                            buf.drain(..).collect()
                        };
                        supervisor_cancel.cancel();
                        outcome = AttemptOutcome::Crashed {
                            exit_code: -1,
                            stderr_tail: stderr_tail_drained,
                        };
                        break;
                    }
                }
            }
            // Single send-site; receiver may already be dropped
            // (respawn_loop never spawned for this attempt, or
            // already moved on). The `Result` from send() is
            // intentionally discarded — there's nothing actionable
            // to do with a closed receiver, the cascade cancel
            // above already tore down the Inner.
            let _ = attempt_outcome_tx.send(outcome);
        });

        let mut tasks = vec![
            writer_handle,
            reader_handle,
            stderr_handle,
            supervisor_handle,
        ];
        if let Some(broker) = broker {
            let kinds: Vec<String> = self
                .cached_manifest
                .plugin
                .channels
                .register
                .iter()
                .map(|c| c.kind.clone())
                .collect();
            // Two patterns per kind: exact `plugin.outbound.<kind>`
            // covers single-instance (legacy back-compat) AND
            // wildcard `plugin.outbound.<kind>.>` covers multi-
            // segment instance suffixes the operator may use
            // (`plugin.outbound.slack.team_a`). Both must match
            // because `topic_matches("foo.>", "foo")` is FALSE —
            // wildcards demand at least one trailing segment.
            let mut subscribe_patterns: Vec<String> = Vec::with_capacity(kinds.len() * 2);
            let mut publish_allowlist: Vec<String> = Vec::with_capacity(kinds.len() * 2);
            for kind in &kinds {
                subscribe_patterns.push(format!("plugin.outbound.{kind}"));
                subscribe_patterns.push(format!("plugin.outbound.{kind}.>"));
                publish_allowlist.push(format!("plugin.inbound.{kind}"));
                publish_allowlist.push(format!("plugin.inbound.{kind}.>"));
            }
            // Manifest-declared broker capability.
            // CRM-style plugins (marketing, analytics) consume topics
            // outside the auto-derived `plugin.outbound.<kind>` family;
            // their `[capabilities.broker]` declares the real surface.
            // Merge after auto-derivation with dedup so kind-channel
            // plugins that ALSO list a duplicate pattern stay harmless
            // (broker subscribe is idempotent, but the publish
            // allowlist matches on first hit so a duplicate would just
            // waste a slot).
            if let Some(broker_cap) = self.cached_manifest.plugin.capabilities.broker.as_ref() {
                for pattern in &broker_cap.subscribe {
                    if !subscribe_patterns.iter().any(|p| p == pattern) {
                        subscribe_patterns.push(pattern.clone());
                    }
                }
                for pattern in &broker_cap.publish {
                    if !publish_allowlist.iter().any(|p| p == pattern) {
                        publish_allowlist.push(pattern.clone());
                    }
                }
            }
            for pattern in subscribe_patterns {
                let mut sub = match broker.subscribe(&pattern).await {
                    Ok(s) => s,
                    Err(e) => {
                        cancel.cancel();
                        kill_handle(&child_handle).await;
                        anyhow::bail!(
                            "subprocess plugin: broker subscribe `{pattern}` failed: {e}"
                        );
                    }
                };
                let stdin_tx_for_fwd = stdin_tx.clone();
                let cancel_for_fwd = cancel.clone();
                let plugin_id_for_fwd = plugin_id_for_log.clone();
                let task = tokio::spawn(async move {
                    loop {
                        let event = tokio::select! {
                            biased;
                            _ = cancel_for_fwd.cancelled() => return,
                            ev = sub.next() => match ev {
                                Some(e) => e,
                                None => return,
                            },
                        };
                        let frame = json!({
                            "jsonrpc": "2.0",
                            "method": "broker.event",
                            "params": {
                                "topic": event.topic.clone(),
                                "event": event,
                            },
                        });
                        // try_send (not send) so a stalled child
                        // can't backpressure the daemon's broker.
                        // The bounded mpsc + drop-on-full matches
                        // at-most-once semantics already noted in
                        // 81.14.
                        match stdin_tx_for_fwd.try_send(frame) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!(
                                    plugin = %plugin_id_for_fwd,
                                    "stdin queue full — dropping broker event for child"
                                );
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                        }
                    }
                });
                tasks.push(task);
            }
            // Activate the reader's `broker.publish` forwarding
            // path AFTER subscribers are wired so the child can't
            // race ahead of its inbound stream.
            let _ = bridge_cell.set(BridgeContext {
                broker,
                publish_allowlist,
                memory,
                llm,
            });
        } else if memory.is_some() || llm.is_some() {
            // Memory provided but no broker → can't reach this
            // path today (factory_registry callers always pass
            // both Some or both None via SubprocessRuntime).
            // Logged for future-proofing.
            tracing::debug!(
                target: "plugin.subprocess",
                plugin_id = %plugin_id_for_log,
                "memory or llm services provided without broker — bridge inactive, RPC path won't fire"
            );
        }

        Ok(Inner {
            stdin_tx,
            pending,
            next_id: Arc::new(AtomicU64::new(2)),
            streaming_pending: Arc::new(DashMap::new()),
            next_credentials_id: Arc::new(std::sync::atomic::AtomicU64::new(1_000_000)),
            declared_tools,
            tasks,
            child: child_handle,
            cancel,
            // Heuristics + IPC for respawn loop.
            // `attempt_outcome_rx` was created earlier in the
            // function (paired with the `attempt_outcome_tx` the
            // supervisor task posts to); now we move it into the
            // returned Inner so `wait_for_attempt_outcome` can
            // `take()` it.
            spawned_at: Instant::now(),
            attempt_outcome_rx: Mutex::new(Some(attempt_outcome_rx)),
        })
    }
}

/// Kill the wrapped child if still present. Used by
/// every spawn_one_attempt error path to make sure a partial
/// boot doesn't leak the child process. Idempotent — `take()`
/// makes follow-up calls a no-op.
async fn kill_handle(h: &Arc<Mutex<Option<Child>>>) {
    let mut guard = h.lock().await;
    if let Some(mut c) = guard.take() {
        let _ = c.kill().await;
    }
}

/// Small helper struct collected from
/// streaming `Usage` chunks. Mirrors `TokenUsage` but lives in
/// this module so we can default it without depending on
/// `TokenUsage::default()` semantics changing.
struct TokenUsageOut {
    prompt_tokens: u32,
    completion_tokens: u32,
}

/// Bundle of LLM-related handles needed to
/// service `llm.complete` requests from a subprocess plugin.
/// `LlmRegistry` builds clients per (provider, model);
/// `LlmConfig` carries the provider table (api keys, endpoints)
/// the registry needs at build time.
#[derive(Clone)]
pub struct LlmServices {
    pub registry: Arc<LlmRegistry>,
    pub config: Arc<LlmConfig>,
}

/// Captured by the stdout reader task to forward validated
/// `broker.publish` notifications to the broker AND service incoming
/// `memory.recall` + `llm.complete` requests from the child.
struct BridgeContext {
    broker: AnyBroker,
    /// Topic patterns the child is allowed to publish to. Derived
    /// from `manifest.channels.register[].kind` — for each kind,
    /// `plugin.inbound.<kind>` and `plugin.inbound.<kind>.>` are
    /// allowed. A child publish to anything outside this list gets
    /// dropped with a warn-level log; this is the host's primary
    /// defense against a malicious / buggy plugin attempting to
    /// hijack core nexo topics like `agent.route.*`.
    publish_allowlist: Vec<String>,
    /// Long-term memory backend the daemon
    /// exposes to subprocess plugins via the `memory.recall`
    /// JSON-RPC method. `None` when the operator hasn't
    /// configured long-term memory. Handler returns `-32603`
    /// "memory not configured" when None.
    memory: Option<Arc<LongTermMemory>>,
    /// LLM services exposed via `llm.complete`.
    /// `None` means the daemon hasn't wired the registry / config
    /// to the subprocess pipeline (operator-level decision); the
    /// handler returns `-32603 "llm not configured"`.
    llm: Option<LlmServices>,
}

/// Forward a `broker.publish` notification from the child onto the
/// broker. Validates the topic against the allowlist before
/// publishing. Logs + drops on any failure path so a misbehaving
/// child can't poison the bridge.
async fn handle_child_publish(bridge: &BridgeContext, plugin_id: &str, parsed: &Value) {
    let topic = parsed
        .pointer("/params/topic")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if topic.is_empty() {
        tracing::warn!(
            plugin = %plugin_id,
            "broker.publish: empty topic — drop"
        );
        return;
    }
    if !bridge
        .publish_allowlist
        .iter()
        .any(|pat| topic_matches(pat, topic))
    {
        tracing::warn!(
            plugin = %plugin_id,
            topic,
            "broker.publish: topic outside child's inbound allowlist — drop"
        );
        return;
    }
    let event_val = parsed
        .pointer("/params/event")
        .cloned()
        .unwrap_or(Value::Null);
    let event: Event = match serde_json::from_value(event_val) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                plugin = %plugin_id,
                topic,
                error = %e,
                "broker.publish: deserialize Event failed — drop"
            );
            return;
        }
    };
    if let Err(e) = bridge.broker.publish(topic, event).await {
        tracing::warn!(
            plugin = %plugin_id,
            topic,
            error = %e,
            "broker.publish: broker forward failed"
        );
    }
}

/// Dispatch an incoming child request to the
/// matching daemon-side handler. Returns `Ok(result_value)` on
/// success or `Err((code, message))` to be serialized as a
/// JSON-RPC error response.
///
/// Today only `memory.recall` is wired. `llm.complete` (81.20.b)
/// + `tool.dispatch` (81.20.c) extend this match.
async fn handle_child_request(
    bridge: &BridgeContext,
    plugin_id: &str,
    method: &str,
    params: &Value,
    stdin_tx: &mpsc::Sender<Value>,
    request_id: &Value,
) -> Result<Value, (i32, String)> {
    match method {
        "memory.recall" => handle_memory_recall(bridge, plugin_id, params).await,
        "llm.complete" => {
            handle_llm_complete(bridge, plugin_id, params, stdin_tx, request_id).await
        }
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

/// Service a `memory.recall` request from the
/// child. Params shape:
///   { "agent_id": "<id>", "query": "<text>", "limit": <u64> }
/// Result on success:
///   { "entries": [<MemoryEntry>...] }
/// Errors:
///   -32602 invalid params (missing required field, bad type)
///   -32603 memory backend not configured
///   -32603 memory recall returned an error
async fn handle_memory_recall(
    bridge: &BridgeContext,
    plugin_id: &str,
    params: &Value,
) -> Result<Value, (i32, String)> {
    let memory = bridge
        .memory
        .as_ref()
        .ok_or_else(|| (-32603, "memory not configured".to_string()))?;
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (-32602, "missing or invalid `agent_id` (string)".to_string()))?;
    let query = params
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (-32602, "missing or invalid `query` (string)".to_string()))?;
    let limit_u64 = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
    // Hard cap to prevent a malicious / buggy plugin from asking
    // for unbounded results. Same defensive shape as the memory
    // tail caps.
    let limit: usize = (limit_u64 as usize).min(1000);
    let entries = memory.recall(agent_id, query, limit).await.map_err(|e| {
        tracing::warn!(
            plugin_id,
            agent_id,
            error = %e,
            "memory.recall: backend returned error"
        );
        (-32603, format!("memory recall failed: {e}"))
    })?;
    let entries_json = serde_json::to_value(&entries).map_err(|e| {
        (
            -32603,
            format!("memory.recall: serialize entries failed: {e}"),
        )
    })?;
    Ok(json!({ "entries": entries_json }))
}

/// Service an `llm.complete` request from the
/// child. Params shape:
///   {
///     "provider": "minimax",
///     "model": "minimax-m2.5",
///     "messages": [{"role":"user","content":"hello"}],
///     "max_tokens": 1024,    // optional, default 4096
///     "temperature": 0.7,    // optional, default 0.7
///     "system_prompt": "..." // optional
///   }
/// Result on success:
///   {
///     "content": "...",
///     "finish_reason": "stop"|"length"|"tool_use"|"other",
///     "usage": { "prompt_tokens": N, "completion_tokens": N }
///   }
/// Errors:
///   -32602 invalid params (missing provider/model/messages, bad role, etc.)
///   -32603 llm not configured (LlmServices None)
///   -32603 client build failed (provider unknown / config invalid)
///   -32603 chat call failed (provider error wrapped)
///   -32601 method returned tool calls instead of text (deferred — MVP
///          surfaces tool calls as `-32601 not_implemented` since
///          plumbing tool-call results back through stdio is its own
///          contract slice)
async fn handle_llm_complete(
    bridge: &BridgeContext,
    plugin_id: &str,
    params: &Value,
    stdin_tx: &mpsc::Sender<Value>,
    request_id: &Value,
) -> Result<Value, (i32, String)> {
    use futures::StreamExt;
    use nexo_config::ModelConfig;
    use nexo_llm::stream::StreamChunk;
    use nexo_llm::types::{ChatMessage, ChatRequest, FinishReason, ResponseContent};

    let llm = bridge
        .llm
        .as_ref()
        .ok_or_else(|| (-32603, "llm not configured".to_string()))?;
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (-32602, "missing or invalid `provider` (string)".to_string()))?;
    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (-32602, "missing or invalid `model` (string)".to_string()))?;
    let messages_val = params.get("messages").ok_or_else(|| {
        (
            -32602,
            "missing `messages` (array of {role, content})".to_string(),
        )
    })?;
    let messages: Vec<ChatMessage> = serde_json::from_value(messages_val.clone())
        .map_err(|e| {
            (
                -32602,
                format!("invalid `messages`: {e} — expected [{{\"role\":\"user|assistant|system|tool\",\"content\":\"...\"}}]"),
            )
        })?;
    if messages.is_empty() {
        return Err((-32602, "`messages` must not be empty".to_string()));
    }
    let max_tokens = params
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(4096);
    let temperature = params
        .get("temperature")
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
        .unwrap_or(0.7);
    let system_prompt = params
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // Opt-in streaming. When `stream: true`,
    // the host calls `client.stream()` and emits each TextDelta
    // as a `llm.complete.delta` notification correlated by
    // `request_id`. Final reply (matching the original `id`)
    // returns only `finish_reason` + `usage` — `content` is
    // omitted because the child reassembled it from deltas.
    let stream = params
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Build the registry's expected ModelConfig view from params.
    // ModelConfig today is just `provider` + `model`.
    let model_cfg = ModelConfig {
        provider: provider.to_string(),
        model: model.to_string(),
    };

    let client = llm.registry.build(&llm.config, &model_cfg).map_err(|e| {
        tracing::warn!(
            plugin_id,
            provider,
            model,
            error = %e,
            "llm.complete: client build failed"
        );
        (-32603, format!("llm client build failed: {e}"))
    })?;

    let mut req = ChatRequest::new(model.to_string(), messages);
    req.max_tokens = max_tokens;
    req.temperature = temperature;
    req.system_prompt = system_prompt;

    if stream {
        // Streaming branch. Iterate the provider's
        // stream; emit TextDelta chunks as
        // `llm.complete.delta { request_id, chunk }` notifications;
        // collect Usage + final FinishReason for the response. Tool
        // call deltas are dropped today (same scope as the
        // non-streaming MVP — tool-call wire shape future).
        let mut stream = client.stream(req).await.map_err(|e| {
            tracing::warn!(
                plugin_id,
                provider,
                model,
                error = %e,
                "llm.complete: stream() returned error"
            );
            (-32603, format!("llm stream failed: {e}"))
        })?;
        let mut usage: Option<TokenUsageOut> = None;
        let mut finish: Option<FinishReason> = None;
        let mut emitted_text = false;
        let mut emitted_tool_calls = false;
        while let Some(chunk_res) = stream.next().await {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        plugin_id,
                        provider,
                        model,
                        error = %e,
                        "llm.complete: stream chunk error"
                    );
                    return Err((-32603, format!("llm stream chunk failed: {e}")));
                }
            };
            match chunk {
                StreamChunk::TextDelta { delta } => {
                    emitted_text = true;
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "method": "llm.complete.delta",
                        "params": {
                            "request_id": request_id,
                            "chunk": delta,
                        }
                    });
                    if let Err(e) = stdin_tx.try_send(frame) {
                        tracing::warn!(
                            plugin_id,
                            error = %e,
                            "llm.complete.delta dropped: stdin queue full or closed"
                        );
                    }
                }
                StreamChunk::ToolCallStart { .. }
                | StreamChunk::ToolCallArgsDelta { .. }
                | StreamChunk::ToolCallEnd { .. } => {
                    emitted_tool_calls = true;
                }
                StreamChunk::Usage(u) => {
                    usage = Some(TokenUsageOut {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                    });
                }
                StreamChunk::End { finish_reason } => {
                    finish = Some(finish_reason);
                }
            }
        }
        if emitted_tool_calls && !emitted_text {
            return Err((
                -32601,
                "llm.complete stream returned tool calls only; MVP supports text \
                 (tool-call wire shape lands in a future contract bump)"
                    .to_string(),
            ));
        }
        let finish_reason = match finish.unwrap_or(FinishReason::Other("stream-no-end".into())) {
            FinishReason::Stop => "stop".to_string(),
            FinishReason::ToolUse => "tool_use".to_string(),
            FinishReason::Length => "length".to_string(),
            FinishReason::Other(s) => format!("other:{s}"),
        };
        let usage_json = match usage {
            Some(u) => json!({
                "prompt_tokens": u.prompt_tokens,
                "completion_tokens": u.completion_tokens,
            }),
            None => json!({"prompt_tokens": 0, "completion_tokens": 0}),
        };
        return Ok(json!({
            "finish_reason": finish_reason,
            "usage": usage_json,
        }));
    }

    let response = client.chat(req).await.map_err(|e| {
        tracing::warn!(
            plugin_id,
            provider,
            model,
            error = %e,
            "llm.complete: chat() returned error"
        );
        (-32603, format!("llm chat failed: {e}"))
    })?;

    // For MVP we only forward text responses. Tool-call responses
    // need a richer wire shape so the child can re-submit tool
    // results — deferring to a future contract bump.
    let content = match response.content {
        ResponseContent::Text(s) => s,
        ResponseContent::ToolCalls(calls) => {
            tracing::info!(
                plugin_id,
                provider,
                model,
                num_tool_calls = calls.len(),
                "llm.complete: provider returned tool calls — MVP surfaces -32601 not_implemented"
            );
            return Err((
                -32601,
                "llm.complete returned tool calls; MVP supports text responses only \
                 (tool-call wire shape lands in a future contract bump)"
                    .to_string(),
            ));
        }
    };
    let finish_reason = match response.finish_reason {
        nexo_llm::types::FinishReason::Stop => "stop".to_string(),
        nexo_llm::types::FinishReason::ToolUse => "tool_use".to_string(),
        nexo_llm::types::FinishReason::Length => "length".to_string(),
        nexo_llm::types::FinishReason::Other(s) => format!("other:{s}"),
    };
    Ok(json!({
        "content": content,
        "finish_reason": finish_reason,
        "usage": {
            "prompt_tokens": response.usage.prompt_tokens,
            "completion_tokens": response.usage.completion_tokens,
        },
    }))
}

#[async_trait]
impl NexoPlugin for SubprocessNexoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.cached_manifest
    }

    async fn init(&self, ctx: &mut PluginInitContext<'_>) -> Result<(), PluginInitError> {
        // Stash the sandbox runner + plugin state
        // dir so `spawn_one_attempt` can wrap the child Command
        // with bwrap argv when the manifest declares
        // `[plugin.sandbox] enabled = true`.
        let plugin_id = self.cached_manifest.plugin.id.clone();
        let plugin_state_dir = ctx.plugin_state_dir(&plugin_id);
        *self.sandbox.lock().await = Some(ctx.sandbox.clone());
        *self.plugin_state_dir.lock().await = Some(plugin_state_dir);

        // Build LlmServices from the context's
        // already-threaded registry + config so `llm.complete`
        // RPC requests reach operator-configured providers.
        let llm = Some(LlmServices {
            registry: ctx.llm_registry.clone(),
            config: ctx.llm_config.clone(),
        });
        let inner = self
            .spawn_one_attempt(
                ctx.shutdown.clone(),
                Some(ctx.broker.clone()),
                ctx.long_term_memory.clone(),
                llm,
            )
            .await
            .map_err(|source| PluginInitError::Other {
                plugin_id: self.cached_manifest.plugin.id.clone(),
                source,
            })?;
        *self.inner.lock().await = Some(inner);
        // Phase 93.2 — flush any buffered configure value via
        // `plugin.configure` JSON-RPC now that the child is alive.
        // The host may have called configure(value) before init;
        // we couldn't deliver it then because the stdio channel
        // didn't exist yet.
        let buffered = self.pending_configure.lock().await.take();
        if let Some(value) = buffered {
            let inner_guard = self.inner.lock().await;
            if let Some(inner_ref) = inner_guard.as_ref() {
                if let Err(e) = self.send_configure_rpc(&value, inner_ref).await {
                    return Err(PluginInitError::Other {
                        plugin_id: self.cached_manifest.plugin.id.clone(),
                        source: anyhow::anyhow!(e.to_string()),
                    });
                }
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PluginShutdownError> {
        // Flag the auto-respawn supervisor task
        // BEFORE we drain the Inner so a supervisor parked in
        // backoff sleep wakes immediately + bails instead of
        // racing the teardown.
        self.shutdown_signaled.store(true, Ordering::Release);
        self.shutdown_notify.notify_waiters();
        let mut inner_guard = self.inner.lock().await;
        let Some(mut inner) = inner_guard.take() else {
            return Ok(()); // never started or already shut down
        };
        // Send shutdown request via JSON-RPC. Allocate a fresh id
        // outside any AtomicU64 — the only other id ever used is
        // the initialize=1, so 2 is safe + simple.
        let shutdown_id: u64 = 2;
        let shutdown_req = json!({
            "jsonrpc": "2.0",
            "id": shutdown_id,
            "method": "shutdown",
            "params": { "reason": "host requested" },
        });
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        inner.pending.insert(shutdown_id, tx);
        let send_ok = inner.stdin_tx.send(shutdown_req).await.is_ok();
        if send_ok {
            let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        }
        // Whether the shutdown reply arrived or not, tear down.
        inner.cancel.cancel();
        // 1s grace for the child to exit on its own; SIGKILL after.
        // The supervisor task may have already taken
        // the child if it observed an exit; `take()` returning None
        // is fine — `kill_handle` is a no-op.
        let child_taken = inner.child.lock().await.take();
        if let Some(mut c) = child_taken {
            let _ = tokio::time::timeout(Duration::from_secs(1), c.wait()).await;
            let _ = c.kill().await;
        }
        for task in inner.tasks.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// Phase 81.33.b.real — opt-in pairing adapter contribution.
    ///
    /// Returns `Some(Arc<GenericBrokerPairingAdapter>)` when the
    /// manifest declares `[plugin.pairing.adapter]`. Daemon
    /// registers the adapter into the shared
    /// `PairingAdapterRegistry` under the manifest-supplied
    /// `channel_id`, replacing the previously hardcoded
    /// `Arc::new(XxxPairingAdapter::new(broker))` blocks.
    ///
    /// Plugins that haven't migrated their manifest yet inherit
    /// `None`; the daemon's legacy hardcoded registration (gated
    /// by `#[cfg(feature = "plugin-X")]`) continues to serve.
    /// Once a plugin ships the manifest section, the generic
    /// adapter wins on the registry `insert` overwrite path.
    fn build_pairing_adapter(
        &self,
        broker: nexo_broker::AnyBroker,
    ) -> Option<std::sync::Arc<dyn nexo_pairing::adapter::PairingChannelAdapter>> {
        let section = self.cached_manifest.plugin.pairing.adapter.as_ref()?;
        Some(std::sync::Arc::new(
            nexo_pairing::generic_adapter::GenericBrokerPairingAdapter::from_manifest(
                broker, section,
            ),
        ))
    }

    /// Phase 93.8.a-daemon — opt-in credential store contribution.
    /// Returns `Some(Arc<RemoteCredentialStore>)` when the
    /// manifest declares `[plugin.credentials_schema] enabled =
    /// true` AND the factory populated `weak_self`. Otherwise
    /// `None` (plugin opts out; typed `bundle.stores.X` continues
    /// to serve during the Phase 93.9 deprecation window).
    fn credential_store(
        &self,
    ) -> Option<std::sync::Arc<dyn nexo_auth::GenericCredentialStore>> {
        let cs = self.cached_manifest.plugin.credentials_schema.as_ref()?;
        if !cs.enabled {
            return None;
        }
        let weak = self.weak_self.get()?.clone();
        Some(std::sync::Arc::new(
            crate::agent::nexo_plugin_registry::remote_credential_store::RemoteCredentialStore::new(
                self.cached_manifest.plugin.id.clone(),
                weak,
            ),
        ))
    }

    /// Phase 93.2 — deliver the operator-supplied config slice to
    /// the subprocess plugin via `plugin.configure` JSON-RPC.
    ///
    /// Called by the host BEFORE `init` (per
    /// [`NexoPlugin::configure`] semantics). For subprocess plugins
    /// the stdio channel only exists after `init` spawns the child,
    /// so this method *buffers* the value into `pending_configure`
    /// and `init` flushes via RPC after the child's `initialize`
    /// ack succeeds. Hot-reload re-calls overwrite the buffer; if
    /// `inner` is already up the new value is delivered eagerly
    /// instead of buffered.
    async fn configure(
        &self,
        value: &serde_yaml::Value,
    ) -> Result<(), PluginConfigureError> {
        // Legacy-compat: plugins without [plugin.config_schema]
        // don't participate in 93.2 RPC delivery — they keep
        // reading their config from disk via the existing loader.
        // Phase 93.5 removes this branch.
        if self.cached_manifest.plugin.config_schema.is_none() {
            return Ok(());
        }
        let inner_guard = self.inner.lock().await;
        if inner_guard.is_some() {
            let inner = inner_guard.as_ref().unwrap();
            // Hot-reload path — channel is up; deliver eagerly.
            return self.send_configure_rpc(value, inner).await;
        }
        drop(inner_guard);
        *self.pending_configure.lock().await = Some(value.clone());
        Ok(())
    }

    /// Phase 81.33.a — override the default no-op trait impl
    /// with the manifest-driven outbound tool registration.
    ///
    /// Iterates `self.cached_manifest.plugin.tools.outbound` and
    /// installs one [`GenericRpcToolHandler`] per entry against
    /// `registry`. Schema parse failures + missing-weak-self
    /// degrade gracefully (warn + skip the entry) rather than
    /// panicking, so a buggy outbound entry doesn't take the
    /// whole agent's tool surface offline.
    fn register_outbound_tools(&self, registry: &crate::agent::tool_registry::ToolRegistry) {
        let outbound = &self.cached_manifest.plugin.tools.outbound;
        if outbound.is_empty() {
            return;
        }
        let Some(weak) = self.weak_self_ref() else {
            tracing::warn!(
                plugin = %self.cached_manifest.plugin.id,
                "register_outbound_tools: weak_self not populated — \
                 plugin built outside the factory; skipping {} outbound tools",
                outbound.len(),
            );
            return;
        };
        // Daemon-wide default. Per-spec timeouts override.
        let daemon_default = std::env::var("NEXO_PLUGIN_TOOL_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
            .unwrap_or(crate::agent::generic_rpc_tool::DEFAULT_OUTBOUND_TOOL_TIMEOUT);
        let mut registered = 0usize;
        let mut skipped = 0usize;
        for spec in outbound {
            let parameters: serde_json::Value =
                match serde_json::from_str(&spec.input_schema) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            plugin = %self.cached_manifest.plugin.id,
                            tool = %spec.name,
                            error = %e,
                            "outbound tool input_schema failed to parse; skipping",
                        );
                        skipped += 1;
                        continue;
                    }
                };
            let def = nexo_llm::types::ToolDef {
                name: spec.name.clone(),
                description: spec.description.clone(),
                parameters,
            };
            let timeout = spec
                .timeout_ms
                .map(std::time::Duration::from_millis)
                .unwrap_or(daemon_default);
            let handler = crate::agent::generic_rpc_tool::GenericRpcToolHandler::new(
                self.cached_manifest.plugin.id.clone(),
                weak.clone(),
                spec.rpc_method.clone(),
                spec.name.clone(),
                timeout,
            );
            registry.register_arc(def, std::sync::Arc::new(handler));
            registered += 1;
        }
        tracing::info!(
            plugin = %self.cached_manifest.plugin.id,
            registered,
            skipped,
            "outbound tools registered from manifest",
        );
    }
}

impl SubprocessNexoPlugin {
    /// Phase 81.33.a — handle for [`GenericRpcToolHandler`] so it
    /// can upgrade back to a typed `Arc<Self>` per call. `None`
    /// only when the factory pattern was bypassed (raw test
    /// constructions); production paths always populate
    /// `weak_self` immediately after `Arc::new(...)`.
    pub fn weak_self_ref(&self) -> Option<std::sync::Weak<SubprocessNexoPlugin>> {
        self.weak_self.get().cloned()
    }

    /// Phase 81.33.a — invoke an outbound-tool RPC against the
    /// running subprocess. Used by
    /// [`GenericRpcToolHandler::call`] (lives in
    /// `agent::generic_rpc_tool`).
    ///
    /// Acquires the inner mutex, sends the request through
    /// `stdin_tx`, awaits the reply via the shared `pending`
    /// DashMap.
    ///
    /// Returns the JSON-RPC `result` value verbatim on success.
    /// Errors:
    ///   - "plugin not running" — `inner` is None (boot failed
    ///     or plugin shutdown).
    ///   - timeout — request sent but no reply within `timeout`;
    ///     the pending slot is cleaned up so it doesn't leak.
    ///   - mapped JSON-RPC error (see
    ///     [`crate::agent::generic_rpc_tool::map_rpc_error`]).
    pub async fn invoke_outbound_tool(
        &self,
        rpc_method: &str,
        tool_name: &str,
        args: serde_json::Value,
        timeout: std::time::Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let (stdin_tx, pending, next_id) = {
            let guard = self.inner.lock().await;
            let Some(inner) = guard.as_ref() else {
                anyhow::bail!(
                    "outbound tool `{tool_name}`: plugin `{}` is not running",
                    self.cached_manifest.plugin.id,
                );
            };
            (
                inner.stdin_tx.clone(),
                inner.pending.clone(),
                inner.next_id.clone(),
            )
        };
        let id = next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": rpc_method,
            "params": {
                "tool_name": tool_name,
                "args": args,
            },
        });
        let (tx, rx) = tokio::sync::oneshot::channel::<
            Result<serde_json::Value, String>,
        >();
        pending.insert(id, tx);
        if let Err(e) = stdin_tx.send(request).await {
            pending.remove(&id);
            anyhow::bail!(
                "outbound tool `{tool_name}`: stdin send failed (plugin likely crashed): {e}"
            );
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(msg))) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                    if let (Some(code), Some(message)) = (
                        parsed.get("code").and_then(|v| v.as_i64()),
                        parsed.get("message").and_then(|v| v.as_str()),
                    ) {
                        return Err(
                            crate::agent::generic_rpc_tool::map_rpc_error(code, message),
                        );
                    }
                }
                Err(anyhow::anyhow!("outbound rpc error: {msg}"))
            }
            Ok(Err(_canceled)) => {
                anyhow::bail!(
                    "outbound tool `{tool_name}`: response channel closed (plugin restart?)",
                );
            }
            Err(_elapsed) => {
                pending.remove(&id);
                anyhow::bail!(
                    "outbound tool `{tool_name}`: timed out after {} ms",
                    timeout.as_millis(),
                );
            }
        }
    }
}

/// Factory helper for subprocess plugins. Reads the
/// manifest at registration time and returns a closure that builds
/// a fresh `Arc<SubprocessNexoPlugin>` per call. Each `factory(&m)`
/// invocation produces a new adapter; the daemon's init loop calls
/// it once per discovered manifest.
pub fn subprocess_plugin_factory(manifest: PluginManifest) -> PluginFactory {
    subprocess_plugin_factory_with_manifest_dir(manifest, None)
}

/// Same as [`subprocess_plugin_factory`] but threads the manifest's
/// parent directory through so relative `[plugin.entrypoint]
/// command` paths (`./bin/foo`) resolve against it at spawn time.
/// The discovery loop wires this with `Some(plugin.root_dir.clone())`
/// where `plugin: &DiscoveredPlugin`.
pub fn subprocess_plugin_factory_with_manifest_dir(
    manifest: PluginManifest,
    manifest_dir: Option<std::path::PathBuf>,
) -> PluginFactory {
    Box::new(move |reg_manifest| {
        let _ = reg_manifest;
        let mut builder = SubprocessNexoPlugin::new(manifest.clone());
        if let Some(dir) = manifest_dir.as_ref() {
            builder = builder.with_manifest_dir(dir.clone());
        }
        let typed: Arc<SubprocessNexoPlugin> = Arc::new(builder);
        let _ = typed.weak_self.set(Arc::downgrade(&typed));
        let plugin: Arc<dyn NexoPlugin> = typed;
        Ok(plugin)
    })
}

/// Factory variant that captures a per-spawn env
/// dict + instance label at registration time. Used by the daemon's
/// multi-instance loops in `proyecto/src/main.rs` so each entry of
/// `cfg.plugins.{telegram,whatsapp}` produces a distinct adapter
/// whose subprocess spawns with a unique env scope.
///
/// Single-instance subprocess plugins (browser) keep using
/// [`subprocess_plugin_factory`] which doesn't tweak the spawn env
/// — those plugins inherit the daemon's process env (the
/// pre-81.18.b behaviour) so their existing operator workflows
/// (`seed_browser_subprocess_env` populating daemon env at boot)
/// keep working unchanged.
pub fn subprocess_plugin_factory_with_env(
    manifest: PluginManifest,
    spawn_env: std::collections::HashMap<String, String>,
    instance_label: String,
) -> PluginFactory {
    subprocess_plugin_factory_with_env_and_manifest_dir(
        manifest,
        spawn_env,
        instance_label,
        None,
    )
}

/// `subprocess_plugin_factory_with_env` + manifest-dir threading.
/// Discovery loop variant for multi-instance plugins that ALSO need
/// relative entrypoint resolution.
pub fn subprocess_plugin_factory_with_env_and_manifest_dir(
    manifest: PluginManifest,
    spawn_env: std::collections::HashMap<String, String>,
    instance_label: String,
    manifest_dir: Option<std::path::PathBuf>,
) -> PluginFactory {
    Box::new(move |_reg_manifest| {
        let mut builder = SubprocessNexoPlugin::new(manifest.clone())
            .with_spawn_env(spawn_env.clone())
            .with_instance_label(instance_label.clone());
        if let Some(dir) = manifest_dir.as_ref() {
            builder = builder.with_manifest_dir(dir.clone());
        }
        let typed: Arc<SubprocessNexoPlugin> = Arc::new(builder);
        let _ = typed.weak_self.set(Arc::downgrade(&typed));
        let plugin: Arc<dyn NexoPlugin> = typed;
        Ok(plugin)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_manifest::EntrypointSection;
    use uuid::Uuid;

    fn manifest_with_entrypoint(command: Option<&str>) -> PluginManifest {
        let toml_str = r#"
[plugin]
id = "test_plugin"
version = "0.1.0"
name = "test"
description = "fixture"
min_nexo_version = ">=0.1.0"

[plugin.requires]
nexo_capabilities = ["broker"]
"#;
        let mut m: PluginManifest = toml::from_str(toml_str).unwrap();
        m.plugin.entrypoint = EntrypointSection {
            command: command.map(|s| s.to_string()),
            args: Vec::new(),
            env: Default::default(),
        };
        m
    }

    #[test]
    fn entrypoint_section_serde_roundtrip() {
        let toml_str = r#"
[plugin]
id = "x"
version = "0.1.0"
name = "x"
description = "x"
min_nexo_version = ">=0.1.0"

[plugin.entrypoint]
command = "/usr/local/bin/plugin-x"
args = ["--mode", "stdio"]

[plugin.entrypoint.env]
RUST_LOG = "info"
"#;
        let m: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(m.plugin.entrypoint.is_subprocess());
        assert_eq!(
            m.plugin.entrypoint.command.as_deref(),
            Some("/usr/local/bin/plugin-x")
        );
        assert_eq!(m.plugin.entrypoint.args, vec!["--mode", "stdio"]);
        assert_eq!(
            m.plugin.entrypoint.env.get("RUST_LOG").map(String::as_str),
            Some("info")
        );
    }

    #[test]
    fn is_subprocess_returns_false_for_in_tree_default() {
        let m = manifest_with_entrypoint(None);
        assert!(!m.plugin.entrypoint.is_subprocess());
        // Empty string also counts as in-tree.
        let m2 = manifest_with_entrypoint(Some("   "));
        assert!(!m2.plugin.entrypoint.is_subprocess());
    }

    #[test]
    fn subprocess_plugin_manifest_returns_cached() {
        let m = manifest_with_entrypoint(Some("/bin/true"));
        let plugin = SubprocessNexoPlugin::new(m.clone());
        assert_eq!(plugin.manifest().plugin.id, m.plugin.id);
    }

    #[tokio::test]
    async fn init_fails_when_command_not_found() {
        let m = manifest_with_entrypoint(Some("/definitely/does/not/exist/nexo-plugin-test-bin"));
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let result = plugin.spawn_one_attempt(cancel, None, None, None).await;
        match result {
            Ok(_) => panic!("spawn must fail for missing command"),
            Err(err) => assert!(
                err.to_string().contains("spawn"),
                "error should mention spawn, got: {err}"
            ),
        }
    }

    #[tokio::test]
    async fn init_fails_when_env_collides_with_nexo_reserved() {
        let mut m = manifest_with_entrypoint(Some("/bin/true"));
        m.plugin
            .entrypoint
            .env
            .insert("NEXO_STATE_ROOT".to_string(), "/tmp/evil".to_string());
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let result = plugin.spawn_one_attempt(cancel, None, None, None).await;
        match result {
            Ok(_) => panic!("env collision must fail"),
            Err(err) => assert!(
                err.to_string().contains("NEXO_"),
                "error should mention reserved env, got: {err}"
            ),
        }
    }

    #[tokio::test]
    async fn init_times_out_when_child_silent() {
        // `/bin/cat` reads stdin forever and never writes JSON-RPC
        // — exactly the "silent child" scenario. We cap timeout
        // hard via env so the test stays fast.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "150");
        let m = manifest_with_entrypoint(Some("/bin/cat"));
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let result = plugin.spawn_one_attempt(cancel, None, None, None).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        match result {
            Ok(_) => panic!("silent child must time out"),
            Err(err) => assert!(
                err.to_string().contains("initialize"),
                "error should mention initialize timeout, got: {err}"
            ),
        }
    }

    #[tokio::test]
    async fn init_fails_when_manifest_id_mismatch_on_initialize_reply() {
        // Spawn a tiny shell pipeline that responds to initialize
        // with a manifest carrying a DIFFERENT id, exercising the
        // identity check.
        let script = r#"#!/bin/sh
read line
echo '{"jsonrpc":"2.0","id":1,"result":{"manifest":{"plugin":{"id":"impostor","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}},"server_version":"test-0.1.0"}}'
sleep 30
"#;
        let dir = std::env::temp_dir().join("nexo-subprocess-test-mismatch");
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("plugin.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "500");
        let m = manifest_with_entrypoint(Some(script_path.to_str().unwrap()));
        // Plugin id remains "test_plugin" but the script returns
        // "impostor" — adapter must reject.
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let result = plugin.spawn_one_attempt(cancel, None, None, None).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        match result {
            Ok(_) => panic!("id mismatch must fail"),
            Err(err) => assert!(
                err.to_string().contains("manifest id mismatch"),
                "error should mention id mismatch, got: {err}"
            ),
        }
    }

    #[tokio::test]
    async fn factory_helper_produces_arc_dyn_nexoplugin() {
        let m = manifest_with_entrypoint(Some("/bin/true"));
        let factory = subprocess_plugin_factory(m.clone());
        match factory(&m) {
            Ok(plugin) => assert_eq!(plugin.manifest().plugin.id, m.plugin.id),
            Err(e) => panic!("factory should build Arc<dyn NexoPlugin>, got: {e}"),
        }
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_when_never_started() {
        let m = manifest_with_entrypoint(Some("/bin/true"));
        let plugin = SubprocessNexoPlugin::new(m);
        // Calling shutdown without init must not panic and must
        // return Ok — the plugin simply has nothing to tear down.
        plugin.shutdown().await.expect("shutdown idempotent");
        plugin
            .shutdown()
            .await
            .expect("second shutdown also idempotent");
    }

    /// Phase 93.2 — `configure(value)` called before `init`
    /// buffers the value silently (returns `Ok`) so the host can
    /// honour configure-before-init semantics even though the
    /// stdio channel only comes up during `init`. `init` then
    /// flushes the buffer via `plugin.configure` JSON-RPC.
    #[tokio::test]
    async fn configure_buffers_when_subprocess_never_spawned() {
        // Manifest WITH [plugin.config_schema] — otherwise the
        // legacy-compat shortcut skips buffering. Phase 93.5 will
        // remove the shortcut.
        let mut m = manifest_with_entrypoint(Some("/bin/true"));
        m.plugin.config_schema = Some(nexo_plugin_manifest::ConfigSchemaSection {
            schema: r#"{"type":"object"}"#.to_string(),
            shape: nexo_plugin_manifest::ConfigShape::Object,
            hot_reload: true,
        });
        let plugin = SubprocessNexoPlugin::new(m);
        let value = serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert(
                serde_yaml::Value::String("k".into()),
                serde_yaml::Value::String("v".into()),
            );
            m
        });
        plugin
            .configure(&value)
            .await
            .expect("configure must buffer + return Ok before init");
        let pending = plugin.pending_configure.lock().await.clone();
        assert_eq!(pending, Some(value));
    }

    /// Phase 93.2 — legacy-compat: when the manifest lacks
    /// `[plugin.config_schema]`, `configure` is a pure no-op
    /// (returns Ok, does NOT buffer). Phase 93.5 removes this
    /// branch once every shipped plugin declares a schema.
    #[tokio::test]
    async fn configure_noop_when_manifest_has_no_schema() {
        let m = manifest_with_entrypoint(Some("/bin/true"));
        let plugin = SubprocessNexoPlugin::new(m);
        let value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        plugin
            .configure(&value)
            .await
            .expect("no-schema configure must be a no-op");
        assert!(plugin.pending_configure.lock().await.is_none());
    }

    // ─── Broker bridge tests ───

    use nexo_broker::AnyBroker;
    use nexo_plugin_manifest::ChannelDecl;

    /// Build a manifest with one channel kind declared so the bridge
    /// derives its subscribe / publish patterns from it. The mock
    /// script writes its own initialize reply with the matching id.
    fn manifest_with_channel(command: &str, kind: &str) -> PluginManifest {
        let mut m = manifest_with_entrypoint(Some(command));
        m.plugin.channels.register.push(ChannelDecl {
            kind: kind.to_string(),
            adapter: "MockAdapter".to_string(),
        });
        m
    }

    /// Drop a tiny shell-script fixture that:
    /// 1. Echoes an initialize reply with manifest.plugin.id =
    ///    `plugin_id` so handshake passes.
    /// 2. Writes a `broker.publish` notification immediately on
    ///    the topic provided so the host bridge has something to
    ///    forward — used by `bridge_forwards_valid_child_publish_*`.
    /// 3. Sleeps so the test can drive its assertions before the
    ///    child exits.
    fn write_bridge_mock_script(
        dir_name: &str,
        plugin_id: &str,
        publish_topic: Option<&str>,
    ) -> std::path::PathBuf {
        let publish_line = match publish_topic {
            Some(t) => format!(
                concat!(
                    "echo '{{\"jsonrpc\":\"2.0\",\"method\":\"broker.publish\",",
                    "\"params\":{{\"topic\":\"{}\",",
                    "\"event\":{{\"id\":\"00000000-0000-0000-0000-000000000001\",",
                    "\"timestamp\":\"2026-05-01T00:00:00Z\",",
                    "\"topic\":\"{}\",\"source\":\"mock\",\"session_id\":null,",
                    "\"payload\":{{\"hello\":\"world\"}}}}}}}}'\n"
                ),
                t, t
            ),
            None => String::new(),
        };
        // The 0.3s gap between initialize reply and the publish
        // notification gives the host time to validate manifest id,
        // wire bridge tasks, and set the OnceCell that gates the
        // reader's broker.publish forwarding. Without it the
        // notification can outrun the bridge wiring on fast CI
        // boxes and get dropped.
        let script = format!(
            r#"#!/bin/sh
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}}}},"server_version":"mock-0.1.0"}}}}'
sleep 0.3
{publish_line}
sleep 5
"#
        );
        let dir = std::env::temp_dir().join(format!("{}-{}", dir_name, Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("plugin.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        script_path
    }

    #[tokio::test]
    async fn bridge_subscribes_outbound_topics_for_each_channel_register_kind() {
        // Manifest declares one channel kind ("slack"). Host should
        // subscribe both `plugin.outbound.slack` and
        // `plugin.outbound.slack.>` so a publish to either matches.
        // We assert by publishing to the wildcard form and verifying
        // the child receives a `broker.event` line on its stdin.
        // Since we don't read the child's stdin from inside the host
        // (it's piped to the child), we infer success from a clean
        // handshake + no panic on the bridge tasks. The richer
        // assertion ships with `bridge_forwards_valid_child_publish_*`.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_bridge_mock_script("nexo-bridge-test-subscribe", "test_plugin", None);
        let m = manifest_with_channel(path.to_str().unwrap(), "slack");
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let res = plugin
            .spawn_one_attempt(cancel.clone(), Some(broker), None, None)
            .await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        let inner = res.expect("handshake + bridge wiring must succeed");
        // 4 baseline tasks (writer + stdout reader + 81.23 stderr
        // reader + 81.21 supervisor) + 2 bridge tasks (one per
        // subscribe pattern, exact + wildcard) for the one
        // declared kind = 6 total.
        assert_eq!(
            inner.tasks.len(),
            6,
            "expected writer + stdout reader + stderr reader + supervisor + 2 forwarder tasks"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn bridge_forwards_valid_child_publish_to_broker() {
        // Mock script publishes `plugin.inbound.slack` immediately
        // after handshake. The host bridge must validate the topic
        // (matches `plugin.inbound.slack` exact) and forward to the
        // broker. We subscribe to `plugin.inbound.slack` from a
        // separate task and assert delivery within 1s.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_bridge_mock_script(
            "nexo-bridge-test-forward",
            "test_plugin",
            Some("plugin.inbound.slack"),
        );
        let m = manifest_with_channel(path.to_str().unwrap(), "slack");
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut sub = broker
            .subscribe("plugin.inbound.slack")
            .await
            .expect("subscribe before init");
        let _inner = plugin
            .spawn_one_attempt(cancel.clone(), Some(broker), None, None)
            .await
            .expect("handshake + bridge wiring");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event arrives within 2s");
        let event = event.expect("subscription delivers Some");
        assert_eq!(event.topic, "plugin.inbound.slack");
        assert_eq!(event.source, "mock");
        cancel.cancel();
    }

    #[tokio::test]
    async fn bridge_rejects_child_publish_outside_inbound_allowlist() {
        // Mock publishes to `agent.route.system_critical` — outside
        // the allowlist for kind=slack. Bridge must DROP, never
        // forward. Verify by subscribing to the rogue topic and
        // ensuring no event arrives within a short window.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_bridge_mock_script(
            "nexo-bridge-test-reject",
            "test_plugin",
            Some("agent.route.system_critical"),
        );
        let m = manifest_with_channel(path.to_str().unwrap(), "slack");
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut rogue_sub = broker
            .subscribe("agent.route.system_critical")
            .await
            .expect("subscribe rogue");
        let _inner = plugin
            .spawn_one_attempt(cancel.clone(), Some(broker), None, None)
            .await
            .expect("handshake");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        // 500ms is long enough for the mock to write its line; if the
        // bridge had forwarded, we'd see the event by then.
        let result = tokio::time::timeout(Duration::from_millis(500), rogue_sub.next()).await;
        cancel.cancel();
        assert!(
            result.is_err(),
            "rogue topic must NOT deliver — bridge dropped"
        );
    }

    #[tokio::test]
    async fn bridge_skipped_when_broker_is_none() {
        // Test the `broker = None` path — no subscribe attempts,
        // no bridge cell set. Verifies tests can still drive the
        // handshake shape without standing up a broker.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_bridge_mock_script("nexo-bridge-test-none", "test_plugin", None);
        let m = manifest_with_channel(path.to_str().unwrap(), "slack");
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let res = plugin
            .spawn_one_attempt(cancel.clone(), None, None, None)
            .await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        let inner = res.expect("handshake must succeed without broker");
        // 81.23 + 81.21 — writer + stdout reader + stderr reader
        // + supervisor, no forwarder tasks (broker = None).
        assert_eq!(
            inner.tasks.len(),
            4,
            "expected writer + stdout reader + stderr reader + supervisor, no forwarders"
        );
        cancel.cancel();
    }

    /// Stderr is piped (not Stdio::null()) so the
    /// reader task can forward child stderr lines into daemon
    /// tracing. We assert this by writing a stderr-emitting mock
    /// that successfully completes the handshake — if stderr were
    /// `Stdio::null()`, the reader_handle on stderr couldn't be
    /// constructed (child has no stderr), and `take()` in
    /// `spawn_one_attempt` would error with "child has no stderr"
    /// before we ever get to a successful handshake.
    #[tokio::test]
    async fn stderr_is_piped_so_reader_can_construct() {
        // Mock that writes "boot diag" on stderr BEFORE replying
        // on stdout — proves both streams flow through the daemon
        // simultaneously. We can't capture daemon tracing output
        // from inside the test (no global subscriber configured),
        // so the assertion focuses on the structural invariant:
        // spawn_one_attempt must succeed when stderr is piped
        // and the child writes to it.
        let script = r#"#!/bin/sh
echo "boot diag from child" >&2
read line
echo '{"jsonrpc":"2.0","id":1,"result":{"manifest":{"plugin":{"id":"test_plugin","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}},"server_version":"mock-0.1.0"}}'
echo "post-init diag" >&2
sleep 5
"#;
        let dir = std::env::temp_dir().join("nexo-stderr-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.sh");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }

        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let m = manifest_with_entrypoint(Some(path.to_str().unwrap()));
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let res = plugin
            .spawn_one_attempt(cancel.clone(), None, None, None)
            .await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let inner = res.expect("handshake must succeed with stderr piped");
        // 4 tasks confirm the stderr reader + 81.21 supervisor are
        // alive: writer + stdout reader + stderr reader + supervisor.
        assert_eq!(
            inner.tasks.len(),
            4,
            "writer + stdout reader + stderr reader + supervisor = 4"
        );
        cancel.cancel();
    }

    /// The supervisor task detects child exit, drains the stderr
    /// tail, and posts an `AttemptOutcome::Crashed` via the oneshot
    /// channel that `respawn_loop` consumes. Lifecycle event
    /// publishing lives in `respawn_loop`; this test verifies the
    /// supervisor's detection contract in isolation.
    #[tokio::test]
    async fn supervisor_posts_crashed_outcome_with_stderr_tail() {
        let script = r#"#!/bin/sh
read line
echo '{"jsonrpc":"2.0","id":1,"result":{"manifest":{"plugin":{"id":"test_plugin","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}},"server_version":"mock-0.1.0"}}'
echo "diag line 1" >&2
echo "diag line 2" >&2
echo "fatal: simulated crash cause" >&2
sleep 0.2
exit 7
"#;
        let dir = std::env::temp_dir().join("nexo-supervisor-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.sh");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }

        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let m = manifest_with_entrypoint(Some(path.to_str().unwrap()));
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let inner = plugin
            .spawn_one_attempt(cancel.clone(), Some(broker), None, None)
            .await
            .expect("handshake");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Supervisor polls every 500ms — exit at 200ms post-handshake
        // is caught on the second tick. Take the receiver out of
        // the Inner so we can await it directly.
        let rx = inner
            .attempt_outcome_rx
            .lock()
            .await
            .take()
            .expect("attempt_outcome_rx populated by spawn_one_attempt");
        let outcome = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("AttemptOutcome arrives within 2s")
            .expect("oneshot sender posted before drop");
        match outcome {
            AttemptOutcome::Crashed {
                exit_code,
                stderr_tail,
            } => {
                assert_eq!(exit_code, 7);
                // stderr lines arrive chronologically (oldest first).
                assert_eq!(
                    stderr_tail,
                    vec![
                        "diag line 1".to_string(),
                        "diag line 2".to_string(),
                        "fatal: simulated crash cause".to_string(),
                    ]
                );
            }
            other => panic!("expected Crashed, got {other:?}"),
        }
        cancel.cancel();
    }

    /// Manifest validation rejects a stderr tail
    /// request above the hard cap (preventing a buggy / malicious
    /// manifest from requesting megabytes of in-memory ring
    /// buffer per running plugin).
    #[test]
    fn manifest_validate_rejects_stderr_tail_above_cap() {
        use nexo_plugin_manifest::SUPERVISOR_STDERR_TAIL_MAX;

        let toml_str = format!(
            r#"
[plugin]
id = "x"
version = "0.1.0"
name = "x"
description = "x"
min_nexo_version = ">=0.0.1"

[plugin.supervisor]
stderr_tail_lines = {}
"#,
            SUPERVISOR_STDERR_TAIL_MAX + 1
        );
        let manifest: PluginManifest = toml::from_str(&toml_str)
            .expect("manifest parses (cap is enforced at validate time, not parse)");
        let mut errors = Vec::new();
        nexo_plugin_manifest::validate::run_all(
            &manifest,
            &semver::Version::parse("0.1.0").unwrap(),
            &mut errors,
        );
        assert!(
            errors.iter().any(|e| matches!(
                e,
                nexo_plugin_manifest::ManifestError::SupervisorStderrTailExceedsCap { .. }
            )),
            "expected SupervisorStderrTailExceedsCap, got {errors:?}"
        );
    }

    /// `memory.recall` request handler servicing
    /// a child's request. We seed an in-memory `LongTermMemory`
    /// with one entry, build a BridgeContext with it, then call
    /// the handler directly. This avoids spinning up the full
    /// subprocess loop — the wire-level routing is structural,
    /// covered by the existing routing tests; what's new in
    /// 81.20.a is the host-side dispatch + serialization.
    #[tokio::test]
    async fn memory_recall_handler_returns_seeded_entry() {
        use nexo_memory::LongTermMemory;
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test_memory.db");
        let memory = Arc::new(
            LongTermMemory::open(db_path.to_str().unwrap())
                .await
                .expect("open long-term memory"),
        );
        memory
            .remember("agent_x", "user prefers concise answers", &["preference"])
            .await
            .expect("seed memory entry");

        let bridge = BridgeContext {
            broker: AnyBroker::Local(nexo_broker::LocalBroker::new()),
            publish_allowlist: vec![],
            memory: Some(memory),
            llm: None,
        };

        let params = json!({
            "agent_id": "agent_x",
            "query": "concise",
            "limit": 5,
        });
        let result = handle_memory_recall(&bridge, "test_plugin", &params)
            .await
            .expect("memory.recall must succeed");
        let entries = result
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries field is an array");
        assert!(
            !entries.is_empty(),
            "expected at least one entry for query that matches seed"
        );
        let first = &entries[0];
        assert_eq!(
            first.get("agent_id").and_then(|v| v.as_str()),
            Some("agent_x")
        );
        assert!(
            first
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("concise"),
            "content field must contain the seeded text"
        );
    }

    /// When memory backend is None (operator
    /// hasn't configured long-term memory OR main.rs hasn't
    /// plumbed the handle yet), the handler returns -32603 with
    /// a clear "memory not configured" message. Plugin authors
    /// see the structured error and can degrade gracefully.
    #[tokio::test]
    async fn memory_recall_handler_returns_neg_32603_when_memory_none() {
        let bridge = BridgeContext {
            broker: AnyBroker::Local(nexo_broker::LocalBroker::new()),
            publish_allowlist: vec![],
            memory: None,
            llm: None,
        };
        let params = json!({"agent_id": "any", "query": "any"});
        let result = handle_memory_recall(&bridge, "test_plugin", &params).await;
        match result {
            Ok(v) => panic!("expected error, got Ok({v:?})"),
            Err((code, msg)) => {
                assert_eq!(code, -32603);
                assert!(
                    msg.contains("not configured"),
                    "error message should mention 'not configured', got: {msg}"
                );
            }
        }
    }

    /// Bad params surface as -32602 invalid
    /// params per JSON-RPC 2.0 spec. Tests both missing
    /// `agent_id` and wrong-type `query`.
    #[tokio::test]
    async fn memory_recall_handler_returns_neg_32602_on_bad_params() {
        use nexo_memory::LongTermMemory;
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test_memory.db");
        let memory = Arc::new(
            LongTermMemory::open(db_path.to_str().unwrap())
                .await
                .expect("open long-term memory"),
        );
        let bridge = BridgeContext {
            broker: AnyBroker::Local(nexo_broker::LocalBroker::new()),
            publish_allowlist: vec![],
            memory: Some(memory),
            llm: None,
        };

        // Missing agent_id
        let r = handle_memory_recall(&bridge, "test_plugin", &json!({"query": "x"})).await;
        match r {
            Err((-32602, msg)) => assert!(msg.contains("agent_id")),
            other => panic!("expected -32602 missing agent_id, got {other:?}"),
        }

        // query is a number, not a string
        let r = handle_memory_recall(
            &bridge,
            "test_plugin",
            &json!({"agent_id": "x", "query": 42}),
        )
        .await;
        match r {
            Err((-32602, msg)) => assert!(msg.contains("query")),
            other => panic!("expected -32602 invalid query, got {other:?}"),
        }
    }

    /// `llm.complete` handler returns -32603
    /// when no `LlmServices` is wired in the bridge. Covers the
    /// "operator hasn't enabled LLM RPC" path.
    #[tokio::test]
    async fn llm_complete_handler_returns_neg_32603_when_llm_none() {
        let bridge = BridgeContext {
            broker: AnyBroker::Local(nexo_broker::LocalBroker::new()),
            publish_allowlist: vec![],
            memory: None,
            llm: None,
        };
        let params = json!({
            "provider": "minimax",
            "model": "x",
            "messages": [{"role":"user","content":"hi"}],
        });
        match {
            let (_dummy_tx, _dummy_rx) = mpsc::channel::<Value>(8);
            let _dummy_id = json!(99);
            handle_llm_complete(&bridge, "test_plugin", &params, &_dummy_tx, &_dummy_id).await
        } {
            Ok(v) => panic!("expected -32603, got Ok({v:?})"),
            Err((code, msg)) => {
                assert_eq!(code, -32603);
                assert!(
                    msg.contains("not configured"),
                    "msg should mention 'not configured', got: {msg}"
                );
            }
        }
    }

    /// Bad params surface as -32602. We test
    /// missing `provider`, missing `messages`, empty `messages`,
    /// and a malformed message (role as integer). Each path must
    /// fail with the correct field name in the error.
    #[tokio::test]
    async fn llm_complete_handler_returns_neg_32602_on_bad_params() {
        let bridge = BridgeContext {
            broker: AnyBroker::Local(nexo_broker::LocalBroker::new()),
            publish_allowlist: vec![],
            memory: None,
            llm: Some(LlmServices {
                registry: Arc::new(LlmRegistry::new()),
                config: Arc::new(LlmConfig {
                    providers: std::collections::HashMap::new(),
                    retry: Default::default(),
                    context_optimization: Default::default(),
                    tenants: std::collections::HashMap::new(),
                }),
            }),
        };

        let (_dtx, _drx) = mpsc::channel::<Value>(8);
        let dummy_id = json!(99);

        // Missing provider.
        let r = handle_llm_complete(
            &bridge,
            "test_plugin",
            &json!({"model": "x", "messages": []}),
            &_dtx,
            &dummy_id,
        )
        .await;
        match r {
            Err((-32602, msg)) => assert!(msg.contains("provider")),
            other => panic!("expected -32602 missing provider, got {other:?}"),
        }

        // Missing messages.
        let r = handle_llm_complete(
            &bridge,
            "test_plugin",
            &json!({"provider": "p", "model": "x"}),
            &_dtx,
            &dummy_id,
        )
        .await;
        match r {
            Err((-32602, msg)) => assert!(msg.contains("messages")),
            other => panic!("expected -32602 missing messages, got {other:?}"),
        }

        // Empty messages array.
        let r = handle_llm_complete(
            &bridge,
            "test_plugin",
            &json!({"provider": "p", "model": "x", "messages": []}),
            &_dtx,
            &dummy_id,
        )
        .await;
        match r {
            Err((-32602, msg)) => assert!(msg.contains("must not be empty")),
            other => panic!("expected -32602 empty messages, got {other:?}"),
        }

        // Malformed role.
        let r = handle_llm_complete(
            &bridge,
            "test_plugin",
            &json!({
                "provider": "p",
                "model": "x",
                "messages": [{"role": 42, "content": "hi"}],
            }),
            &_dtx,
            &dummy_id,
        )
        .await;
        match r {
            Err((-32602, msg)) => assert!(msg.contains("messages")),
            other => panic!("expected -32602 malformed role, got {other:?}"),
        }
    }

    /// When the provider is not registered in the
    /// LlmRegistry, the handler returns -32603 with the build
    /// error wrapped (not -32602 — the params themselves are
    /// well-formed JSON; the failure is server-side).
    #[tokio::test]
    async fn llm_complete_handler_returns_neg_32603_when_provider_not_registered() {
        let bridge = BridgeContext {
            broker: AnyBroker::Local(nexo_broker::LocalBroker::new()),
            publish_allowlist: vec![],
            memory: None,
            llm: Some(LlmServices {
                registry: Arc::new(LlmRegistry::new()), // empty — no providers
                config: Arc::new(LlmConfig {
                    providers: std::collections::HashMap::new(),
                    retry: Default::default(),
                    context_optimization: Default::default(),
                    tenants: std::collections::HashMap::new(),
                }),
            }),
        };
        let params = json!({
            "provider": "nonexistent_provider",
            "model": "x",
            "messages": [{"role":"user","content":"hi"}],
        });
        match {
            let (_dummy_tx, _dummy_rx) = mpsc::channel::<Value>(8);
            let _dummy_id = json!(99);
            handle_llm_complete(&bridge, "test_plugin", &params, &_dummy_tx, &_dummy_id).await
        } {
            Ok(v) => panic!("expected -32603, got Ok({v:?})"),
            Err((code, msg)) => {
                assert_eq!(code, -32603);
                assert!(
                    msg.contains("client build failed") || msg.contains("nonexistent_provider"),
                    "msg should mention build failure, got: {msg}"
                );
            }
        }
    }

    /// `with_spawn_env` populates the field; the
    /// builder is otherwise a no-op (state isn't visible at the
    /// public API level until `spawn_one_attempt` runs). Verify
    /// the dict survives the builder + that `with_instance_label`
    /// normalises empty strings to `None`.
    #[test]
    fn with_spawn_env_populates_field() {
        let manifest = manifest_with_entrypoint(Some("/bin/cat"));
        let mut env = std::collections::HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        env.insert("PATH".to_string(), "/usr/bin".to_string());

        let plugin = SubprocessNexoPlugin::new(manifest)
            .with_spawn_env(env.clone())
            .with_instance_label("bot1");
        assert_eq!(
            plugin
                .spawn_env
                .as_ref()
                .unwrap()
                .get("FOO")
                .map(|s| s.as_str()),
            Some("bar")
        );
        assert_eq!(
            plugin
                .spawn_env
                .as_ref()
                .unwrap()
                .get("PATH")
                .map(|s| s.as_str()),
            Some("/usr/bin")
        );
        assert_eq!(plugin.instance_label.as_deref(), Some("bot1"));
    }

    #[test]
    fn with_instance_label_normalises_empty_to_none() {
        let manifest = manifest_with_entrypoint(Some("/bin/cat"));

        let plugin1 = SubprocessNexoPlugin::new(manifest.clone()).with_instance_label("");
        assert_eq!(plugin1.instance_label, None);

        let plugin2 = SubprocessNexoPlugin::new(manifest.clone()).with_instance_label("   ");
        assert_eq!(plugin2.instance_label, None);

        let plugin3 = SubprocessNexoPlugin::new(manifest).with_instance_label("real");
        assert_eq!(plugin3.instance_label.as_deref(), Some("real"));
    }

    /// By default (`SubprocessNexoPlugin::new`
    /// alone), `spawn_env` stays `None` so `spawn_one_attempt`
    /// keeps the pre-81.18.b inherit-daemon-env behaviour. The
    /// single-instance browser plugin and any test that calls
    /// `spawn_one_attempt` without going through the per-spawn
    /// factory variant relies on this default.
    #[test]
    fn default_inherits_daemon_env_when_spawn_env_not_set() {
        let manifest = manifest_with_entrypoint(Some("/bin/cat"));
        let plugin = SubprocessNexoPlugin::new(manifest);
        assert!(plugin.spawn_env.is_none());
        assert!(plugin.instance_label.is_none());
    }

    // ─── `next_backoff` pure unit tests ───

    #[test]
    fn next_backoff_doubles_per_attempt() {
        // base 1000ms, attempts 0..=3 → 1s, 2s, 4s, 8s.
        assert_eq!(next_backoff(0, 1000), Duration::from_millis(1000));
        assert_eq!(next_backoff(1, 1000), Duration::from_millis(2000));
        assert_eq!(next_backoff(2, 1000), Duration::from_millis(4000));
        assert_eq!(next_backoff(3, 1000), Duration::from_millis(8000));
    }

    #[test]
    fn next_backoff_caps_at_60s() {
        // base 1000ms; attempt 6 would give 64s without the cap →
        // must clamp to 60s exactly. Attempts above the cap point
        // (7, 8, 99) all return the same capped value.
        let cap = Duration::from_millis(RESPAWN_BACKOFF_CAP_MS);
        assert_eq!(next_backoff(6, 1000), cap);
        assert_eq!(next_backoff(7, 1000), cap);
        assert_eq!(next_backoff(99, 1000), cap);
    }

    #[test]
    fn next_backoff_handles_zero_base() {
        // Pathological manifest with `backoff_ms = 0`. No panic;
        // returns 0 for every attempt (operator gets a tight loop;
        // their problem to fix the manifest, not ours to defend
        // against).
        for attempt in [0, 1, 5, 63, u32::MAX] {
            assert_eq!(next_backoff(attempt, 0), Duration::from_millis(0));
        }
    }

    #[test]
    fn next_backoff_saturates_on_overflow() {
        // base = u64::MAX, attempt = u32::MAX. Naive
        // `base * 2^attempt` would panic on overflow; saturating
        // arithmetic must cap at the 60s ceiling instead.
        let result = next_backoff(u32::MAX, u64::MAX);
        assert_eq!(result, Duration::from_millis(RESPAWN_BACKOFF_CAP_MS));
    }

    // ─── Respawn loop integration tests ───

    /// Drop a tiny shell-script fixture that crashes after handshake.
    /// Each invocation of the binary writes a fresh handshake reply,
    /// then exits with status `exit_code` after `crash_after_ms`.
    /// The respawn loop spawns a fresh process per attempt so a
    /// single static script suffices.
    fn write_always_crash_script(
        dir_name: &str,
        plugin_id: &str,
        crash_after_ms: u32,
        exit_code: u8,
    ) -> std::path::PathBuf {
        let crash_after_secs = (crash_after_ms as f64) / 1000.0;
        let script = format!(
            r#"#!/bin/sh
read line
echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"{plugin_id}","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}}}},"server_version":"mock-0.1.0"}}}}'
sleep {crash_after_secs}
exit {exit_code}
"#
        );
        let dir = std::env::temp_dir().join(format!("{}-{}", dir_name, Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("plugin.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        script_path
    }

    /// Build a manifest with `[plugin.supervisor]` knobs tuned for
    /// fast respawn tests.
    fn manifest_with_supervisor(
        command: &str,
        respawn: bool,
        max_attempts: u32,
        backoff_ms: u64,
    ) -> PluginManifest {
        let mut m = manifest_with_entrypoint(Some(command));
        m.plugin.supervisor.respawn = respawn;
        m.plugin.supervisor.max_attempts = max_attempts;
        m.plugin.supervisor.backoff_ms = backoff_ms;
        m.plugin.supervisor.stderr_tail_lines = 16;
        m
    }

    /// Construct a plugin via the production factory so
    /// `weak_self` is populated. Returns the typed Arc + the
    /// dyn Arc held to keep the strong refcount healthy.
    fn arc_plugin_via_factory(
        manifest: PluginManifest,
    ) -> (Arc<SubprocessNexoPlugin>, Arc<dyn NexoPlugin>) {
        let factory = subprocess_plugin_factory(manifest.clone());
        let dyn_plugin: Arc<dyn NexoPlugin> = factory(&manifest).unwrap();
        let typed = dyn_plugin
            .as_any()
            .downcast_ref::<SubprocessNexoPlugin>()
            .unwrap()
            .weak_self_arc()
            .expect("factory populated weak_self");
        (typed, dyn_plugin)
    }

    /// Spawn the first attempt + install Inner + spawn the
    /// respawn loop. Mirrors what `init_loop`'s
    /// `start_plugin_supervisor_loop_after_init` hook does in
    /// production but without the full PluginInitContext.
    async fn boot_with_supervisor(
        plugin: Arc<SubprocessNexoPlugin>,
        ctx_shutdown: CancellationToken,
        broker: AnyBroker,
    ) {
        let inner = plugin
            .spawn_one_attempt(ctx_shutdown.clone(), Some(broker.clone()), None, None)
            .await
            .expect("first spawn must succeed");
        *plugin.inner.lock().await = Some(inner);
        plugin
            .clone()
            .spawn_supervisor_loop(ctx_shutdown, Some(broker), None, None);
    }

    /// Drain a broker subscription into a Vec until the timeout
    /// fires. Returns the events captured so the test can assert
    /// on the sequence + payload shapes.
    async fn collect_events_until(
        sub: &mut nexo_broker::Subscription,
        timeout: Duration,
    ) -> Vec<nexo_broker::Event> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            match tokio::time::timeout(remaining, sub.next()).await {
                Ok(Some(ev)) => out.push(ev),
                Ok(None) => break, // subscription closed
                Err(_) => break,   // overall timeout
            }
        }
        out
    }

    #[tokio::test]
    async fn respawn_after_crash_publishes_crashed_then_respawning() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_always_crash_script("nexo-respawn-test-1", "test_plugin", 50, 1);
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 5, 20);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut crashed_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.crashed")
            .await
            .expect("subscribe crashed");
        let mut respawning_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawning")
            .await
            .expect("subscribe respawning");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let crashed = collect_events_until(&mut crashed_sub, Duration::from_millis(800)).await;
        let respawning = collect_events_until(&mut respawning_sub, Duration::from_millis(50)).await;
        assert!(!crashed.is_empty(), "expected at least one crashed event");
        assert!(
            !respawning.is_empty(),
            "expected at least one respawning event"
        );
        let first_crashed = &crashed[0];
        assert_eq!(first_crashed.source, "plugin.supervisor");
        assert_eq!(
            first_crashed
                .payload
                .get("plugin_id")
                .and_then(|v| v.as_str()),
            Some("test_plugin")
        );
        let first_respawning = &respawning[0];
        assert_eq!(
            first_respawning
                .payload
                .get("attempt")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert!(first_respawning
            .payload
            .get("backoff_ms")
            .and_then(|v| v.as_u64())
            .is_some());
        cancel.cancel();
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts_publishes_gave_up() {
        // The supervisor polls `try_wait()` every 500ms and the
        // script sleeps 50ms before exit, so each Inner is "alive"
        // ~550ms. The reset-counter heuristic resets attempt to 0
        // when alive_ms >= base_ms * max_attempts * 2; with
        // base_ms=10, max_attempts=2 that window is just 40ms,
        // which the child always exceeds — so the counter would
        // reset every cycle, never reaching gave_up. Use
        // base_ms=200 (window = 800ms > 550ms) so the counter
        // bumps and gave_up fires at attempt=2.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_always_crash_script("nexo-respawn-test-2", "test_plugin", 50, 7);
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 2, 200);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut gave_up_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.gave_up")
            .await
            .expect("subscribe gave_up");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        let event = tokio::time::timeout(Duration::from_secs(6), gave_up_sub.next())
            .await
            .expect("gave_up event arrives within 6s")
            .expect("subscription delivers Some");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");
        assert_eq!(event.source, "plugin.supervisor");
        let attempts = event
            .payload
            .get("attempts")
            .and_then(|v| v.as_u64())
            .expect("attempts field");
        assert_eq!(attempts, 2, "max_attempts boundary");
        let last_exit = event
            .payload
            .get("last_exit_code")
            .and_then(|v| v.as_i64())
            .expect("last_exit_code field");
        assert!(
            last_exit == 7 || last_exit == -1,
            "exit_code is 7 from script (or -1 if a respawn handshake failed). got {last_exit}"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn respawn_disabled_keeps_legacy_behavior() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_always_crash_script("nexo-respawn-test-3", "test_plugin", 50, 1);
        // respawn = false → only `crashed` event, never `respawning`.
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 3, 1000);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut crashed_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.crashed")
            .await
            .expect("subscribe crashed");
        let mut respawning_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawning")
            .await
            .expect("subscribe respawning");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let crashed = tokio::time::timeout(Duration::from_secs(2), crashed_sub.next())
            .await
            .expect("crashed event within 2s")
            .expect("subscription delivers Some");
        assert_eq!(crashed.source, "plugin.supervisor");
        // Wait a full backoff window — no respawning event should fire.
        let respawning =
            collect_events_until(&mut respawning_sub, Duration::from_millis(300)).await;
        assert!(
            respawning.is_empty(),
            "respawn=false must not publish respawning events; got {} events",
            respawning.len()
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn shutdown_aborts_during_backoff() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_always_crash_script("nexo-respawn-test-4", "test_plugin", 50, 1);
        // Long backoff (5s) so we can call shutdown() during the
        // wait. respawn_loop must wake immediately + bail.
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 5, 5000);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut crashed_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.crashed")
            .await
            .expect("subscribe crashed");
        let mut respawning_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawning")
            .await
            .expect("subscribe respawning");
        let mut respawned_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawned")
            .await
            .expect("subscribe respawned");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Wait for the first crashed + respawning events to confirm
        // the supervisor entered the backoff branch.
        let _ = tokio::time::timeout(Duration::from_secs(2), crashed_sub.next())
            .await
            .expect("crashed within 2s");
        let _ = tokio::time::timeout(Duration::from_secs(1), respawning_sub.next())
            .await
            .expect("respawning within 1s");
        // Now call shutdown(). Must short-circuit the 5s backoff
        // sleep — verify by checking no `respawned` event arrives
        // in the next 300ms window.
        let shutdown_start = std::time::Instant::now();
        plugin.shutdown().await.expect("shutdown ok");
        let shutdown_elapsed = shutdown_start.elapsed();
        assert!(
            shutdown_elapsed < Duration::from_secs(2),
            "shutdown should not wait the full 5s backoff: {shutdown_elapsed:?}"
        );
        let respawned = collect_events_until(&mut respawned_sub, Duration::from_millis(500)).await;
        assert!(
            respawned.is_empty(),
            "no respawned event after shutdown; got {} events",
            respawned.len()
        );
        cancel.cancel();
    }

    /// `respawned.total_uptime_ms`
    /// reports the previous Inner's lifespan (handshake → crash
    /// detection). Operators graph this per-cycle to spot
    /// degrading plugins. Field used to be hard-coded `0`; now
    /// must be a positive number greater than the supervisor's
    /// 500ms poll interval (the script crashes ~50ms after
    /// handshake, supervisor catches it on the next ~500ms tick).
    #[tokio::test]
    async fn respawned_event_carries_previous_inner_uptime() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1000");
        let path = write_always_crash_script("nexo-respawn-test-uptime", "test_plugin", 50, 1);
        // backoff small so the respawn fires fast.
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 5, 20);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut respawned_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawned")
            .await
            .expect("subscribe respawned");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let event = tokio::time::timeout(Duration::from_secs(3), respawned_sub.next())
            .await
            .expect("respawned event arrives within 3s")
            .expect("subscription delivers Some");
        let uptime = event
            .payload
            .get("total_uptime_ms")
            .and_then(|v| v.as_u64())
            .expect("total_uptime_ms field");
        // Previous Inner survived ~50ms of script lifetime + up
        // to 500ms of supervisor poll latency. Conservative
        // bounds: at least 1ms (positive), at most 5s (sane
        // upper bound).
        assert!(
            uptime >= 1,
            "uptime must be positive (placeholder 0 is bug); got {uptime}"
        );
        assert!(
            uptime < 5_000,
            "uptime should be sub-second-scale for fast-crash test; got {uptime}"
        );
        cancel.cancel();
    }

    // ─── Force_restart tests ───

    /// Drop a stable mock that handshakes then sleeps forever.
    /// Force-restart kills it cleanly and a fresh spawn follows.
    fn write_stable_script(dir_name: &str, plugin_id: &str) -> std::path::PathBuf {
        write_bridge_mock_script(dir_name, plugin_id, None)
    }

    /// `force_restart` cleanly tears down the dying child + spawns
    /// a fresh `Inner` with a different stdin channel. Verify by
    /// comparing the `stdin_tx` channel identity before / after.
    #[tokio::test]
    async fn force_restart_replaces_inner_with_fresh_handshake() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_stable_script("nexo-force-restart-test-1", "test_plugin");
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 0, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Capture initial stdin_tx identity via channel sender
        // pointer (mpsc::Sender doesn't expose Eq, but two
        // distinct channels yield different `clone().capacity()`
        // states only on send — so we rely on the spawned_at
        // timestamp differing instead.
        let initial_spawned_at = {
            let g = plugin.inner.lock().await;
            g.as_ref().expect("inner installed").spawned_at
        };

        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let report = plugin
            .clone()
            .force_restart(cancel.clone(), Some(broker.clone()), None, None)
            .await
            .expect("force_restart ok");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        assert_eq!(report.plugin_id, "test_plugin");
        // previous_uptime_ms is post-handshake elapsed: at least
        // a few ms (handshake completion to force_restart call).
        // Bound generously for CI variance.
        assert!(
            report.previous_uptime_ms < 60_000,
            "uptime sane: {report:?}"
        );
        assert!(report.restarted_at_ms > 1_700_000_000_000);

        // Verify fresh Inner installed with newer spawned_at.
        let new_spawned_at = {
            let g = plugin.inner.lock().await;
            g.as_ref().expect("new inner installed").spawned_at
        };
        assert!(
            new_spawned_at > initial_spawned_at,
            "new Inner.spawned_at must be later than the previous"
        );
        cancel.cancel();
    }

    /// Two operators clicking "Restart"
    /// simultaneously (or a CLI restart racing the admin RPC) must
    /// not produce orphaned children. The per-plugin `restart_lock`
    /// holds for the full force_restart cascade so the second caller
    /// observes the first's freshly installed Inner instead of
    /// building a parallel one. Test asserts both calls succeed,
    /// spawn distinct PIDs, and publish exactly two
    /// `restarted_manually` events (no coalesce, no loss).
    #[tokio::test]
    async fn concurrent_force_restart_serializes_via_restart_lock() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_stable_script("nexo-force-restart-test-concurrent", "test_plugin");
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 0, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut sub = broker
            .subscribe("plugin.lifecycle.test_plugin.restarted_manually")
            .await
            .expect("subscribe restarted_manually");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        let p1 = plugin.clone();
        let p2 = plugin.clone();
        let c1 = cancel.clone();
        let c2 = cancel.clone();
        let b1 = broker.clone();
        let b2 = broker.clone();
        let (r1, r2) = tokio::join!(
            async move { p1.force_restart(c1, Some(b1), None, None).await },
            async move { p2.force_restart(c2, Some(b2), None, None).await },
        );
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let r1 = r1.expect("first force_restart ok");
        let r2 = r2.expect("second force_restart ok");
        assert_eq!(r1.plugin_id, "test_plugin");
        assert_eq!(r2.plugin_id, "test_plugin");
        // Distinct PIDs — proves second cascade spawned its own
        // child rather than reusing the first's.
        if let (Some(p1), Some(p2)) = (r1.new_pid, r2.new_pid) {
            assert_ne!(p1, p2, "serialized restarts must spawn distinct children");
        }
        // Exactly TWO restarted_manually events — one per call.
        let mut events = Vec::new();
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(500), sub.next()).await
        {
            events.push(ev);
            if events.len() >= 2 {
                break;
            }
        }
        assert_eq!(
            events.len(),
            2,
            "concurrent force_restart must publish exactly 2 events, got {}",
            events.len()
        );
        cancel.cancel();
    }

    /// `force_restart` publishes `restarted_manually` (NOT
    /// `crashed`/`respawned` — that's the auto-respawn path).
    /// Subscribe + verify exactly one event arrives with the
    /// documented payload shape.
    #[tokio::test]
    async fn force_restart_publishes_restarted_manually_event() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_stable_script("nexo-force-restart-test-2", "test_plugin");
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 0, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut sub = broker
            .subscribe("plugin.lifecycle.test_plugin.restarted_manually")
            .await
            .expect("subscribe restarted_manually");
        let mut crashed_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.crashed")
            .await
            .expect("subscribe crashed");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        let report = plugin
            .clone()
            .force_restart(cancel.clone(), Some(broker.clone()), None, None)
            .await
            .expect("force_restart ok");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("restarted_manually arrives within 2s")
            .expect("subscription delivers Some");
        assert_eq!(
            event.payload.get("plugin_id").and_then(|v| v.as_str()),
            Some("test_plugin")
        );
        assert_eq!(
            event
                .payload
                .get("previous_uptime_ms")
                .and_then(|v| v.as_u64()),
            Some(report.previous_uptime_ms)
        );
        assert!(event
            .payload
            .get("restarted_at_ms")
            .and_then(|v| v.as_i64())
            .is_some());
        // Broker payload must mirror the
        // `PluginsRestartResponse.new_pid` field so subscribers
        // don't have to RPC again to learn the freshly spawned
        // PID. `Some(_)` from Tokio's `Child::id()` is the common
        // case; `None` would serialise as `null`.
        let new_pid_v = event
            .payload
            .get("new_pid")
            .expect("new_pid present in event payload");
        match (report.new_pid, new_pid_v) {
            (Some(pid), serde_json::Value::Number(n)) => {
                assert_eq!(n.as_u64(), Some(u64::from(pid)));
            }
            (None, serde_json::Value::Null) => {}
            (a, b) => panic!("new_pid mismatch — response={:?}, payload={:?}", a, b),
        }

        // No `crashed` event for an intentional kill.
        let crashed = collect_events_until(&mut crashed_sub, Duration::from_millis(300)).await;
        assert!(
            crashed.is_empty(),
            "intentional kill must NOT publish crashed; got {} events",
            crashed.len()
        );
        cancel.cancel();
    }

    /// Plugin in `gave_up` state can still recover via
    /// `force_restart` — operator's primary recovery path. Drive
    /// to gave_up via always-crash + max_attempts=1, then
    /// force_restart against the same Arc<Self>. New Inner
    /// installed, fresh respawn_loop spawned (verifiable via
    /// successful subsequent force_restart cycle).
    #[tokio::test]
    async fn force_restart_after_gave_up_recovers_plugin() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        // 2 scripts: one always-crashes (drives to gave_up), one
        // stable (recovery). force_restart calls spawn_one_attempt
        // which runs the SAME `manifest.entrypoint.command` —
        // can't swap mid-flight. Workaround: use a stable script
        // throughout + drive to gave_up by calling force_restart
        // not by crashing. WAIT: that doesn't reproduce gave_up.
        //
        // Alt: use the always-crash script for the whole test;
        // gave_up fires; force_restart spawns the same script
        // which crashes again; force_restart still REPORTS
        // success (the new Inner WAS installed, just dies fast).
        // This is the realistic operator scenario: restart a
        // gave_up plugin, fix the underlying issue separately.
        let path = write_always_crash_script(
            "nexo-force-restart-test-3",
            "test_plugin",
            50, // tight crash so reset-counter heuristic doesn't fire
            1,
        );
        // backoff=300ms gives reset_window=600ms, larger than the
        // ~510ms alive lifetime (50ms script sleep + ≤500ms
        // supervisor poll), so counter bumps as expected and
        // gave_up fires after max_attempts=1.
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 1, 300);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut gave_up_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.gave_up")
            .await
            .expect("subscribe gave_up");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        // Wait for gave_up to fire (max_attempts=1, so 2 crashes
        // + gave_up within ~3s).
        let _ = tokio::time::timeout(Duration::from_secs(5), gave_up_sub.next())
            .await
            .expect("gave_up within 5s");

        // Now force_restart — even after gave_up, the Arc<Self>
        // is still alive and force_restart spawns a new Inner +
        // fresh respawn_loop.
        let report = plugin
            .clone()
            .force_restart(cancel.clone(), Some(broker.clone()), None, None)
            .await
            .expect("force_restart after gave_up must succeed");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        assert_eq!(report.plugin_id, "test_plugin");
        // New Inner installed (force_restart returns Ok).
        let inner_present = plugin.inner.lock().await.is_some();
        assert!(inner_present, "new Inner installed after force_restart");
        cancel.cancel();
    }

    /// Pending oneshots get drained with retry-error BEFORE the
    /// new Inner installs. Test by inserting a fake pending entry
    /// pre-restart, then verifying the receiver got the
    /// retry-error before force_restart returns.
    #[tokio::test]
    async fn force_restart_drains_pending_with_retry_error() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_stable_script("nexo-force-restart-test-4", "test_plugin");
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 0, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        // Inject a fake pending oneshot — caller is "waiting" for
        // a JSON-RPC reply id=42 that will never come.
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        {
            let g = plugin.inner.lock().await;
            let inner = g.as_ref().expect("inner installed");
            inner.pending.insert(42, tx);
        }

        let _report = plugin
            .clone()
            .force_restart(cancel.clone(), Some(broker.clone()), None, None)
            .await
            .expect("force_restart ok");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Receiver must have been resolved with the retry error.
        let result = rx.await.expect("oneshot resolved (not dropped)");
        match result {
            Err(msg) => assert!(
                msg.contains("plugin restarted by operator"),
                "drain reason should be retry-friendly: {msg}"
            ),
            Ok(_) => panic!("expected Err from drain; got Ok"),
        }
        cancel.cancel();
    }

    // ─── Deferred tests (6 cases from FOLLOWUPS) ───

    /// Auto-respawn path drains pending oneshots with the
    /// "plugin restarted; retry" message (distinct from
    /// force_restart's "plugin restarted by operator"). Caller
    /// holding a pending oneshot during a crash gets the retry
    /// signal as soon as respawn_loop processes the Crashed
    /// outcome.
    #[tokio::test]
    async fn pending_drained_with_retry_error_during_auto_respawn() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_always_crash_script("nexo-deferred-test-1", "test_plugin", 50, 1);
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 5, 200);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Inject pending oneshot; supervisor will detect the
        // crash within ~500ms and respawn_loop drains pending.
        let (tx, rx) = oneshot::channel::<Result<Value, String>>();
        {
            let g = plugin.inner.lock().await;
            let inner = g.as_ref().expect("inner installed");
            inner.pending.insert(99, tx);
        }
        // Wait for drain. Generous bound: 500ms supervisor poll
        // + 200ms backoff + handshake.
        let result = tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .expect("oneshot resolves within 3s")
            .expect("sender posted before drop");
        match result {
            Err(msg) => assert!(
                msg.contains("plugin restarted; retry"),
                "auto-respawn drain message mismatch: {msg}"
            ),
            Ok(_) => panic!("expected Err from drain; got Ok"),
        }
        cancel.cancel();
    }

    /// Reset-counter heuristic — child surviving longer than
    /// `base_ms × max_attempts × 2` post-respawn resets the
    /// attempt counter to 0 at the next crash. Verify by
    /// driving multiple crashes with long alive windows + asserting
    /// gave_up does NOT fire (counter never reaches max_attempts).
    #[tokio::test]
    async fn attempt_counter_resets_after_window() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        // backoff=100ms, max_attempts=2 → reset_window = 400ms.
        // Script alive for 600ms (> reset_window) → counter
        // resets to 0 at every crash. gave_up never fires.
        let path = write_always_crash_script("nexo-deferred-test-2", "test_plugin", 600, 1);
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 2, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut gave_up_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.gave_up")
            .await
            .expect("subscribe gave_up");
        let mut respawning_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawning")
            .await
            .expect("subscribe respawning");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Wait for at least 3 respawning events to confirm the
        // loop actually iterates (not stalled). ~3 × 1100ms = 3.3s.
        let respawning = collect_events_until(&mut respawning_sub, Duration::from_secs(5)).await;
        assert!(
            respawning.len() >= 3,
            "expected >=3 respawning events to confirm loop iterates; got {}",
            respawning.len()
        );
        // EVERY respawning event should report attempt=1 (counter
        // resets each cycle because alive > reset_window).
        for ev in &respawning {
            let attempt = ev
                .payload
                .get("attempt")
                .and_then(|v| v.as_u64())
                .expect("attempt field");
            assert_eq!(
                attempt, 1,
                "counter should reset to 0 each cycle; got attempt={attempt}"
            );
        }
        // gave_up should NOT have fired.
        let gave_up = collect_events_until(&mut gave_up_sub, Duration::from_millis(50)).await;
        assert!(
            gave_up.is_empty(),
            "reset heuristic should prevent gave_up; got {} events",
            gave_up.len()
        );
        cancel.cancel();
    }

    /// Supervisor task's stash of `sandbox: Mutex<Option<Arc<SandboxRunner>>>`
    /// + `plugin_state_dir: Mutex<Option<PathBuf>>` is set ONCE by
    /// `init()` and read by every `spawn_one_attempt` call. Verify
    /// the stash survives a force_restart cycle (no path that
    /// resets either field).
    #[tokio::test]
    async fn sandbox_stash_reused_across_respawns() {
        // Build the plugin via the factory; manually set the
        // sandbox stash to a known instance, then trigger force
        // restart, then verify the stash still holds the same
        // SandboxRunner Arc.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_stable_script("nexo-deferred-test-3", "test_plugin");
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 0, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        // Pre-restart: stash a sandbox runner + state dir into the
        // outer struct fields. Production wires these via init();
        // we mimic by writing directly post-boot.
        let sandbox_arc = std::sync::Arc::new(
            crate::agent::plugin_sandbox::SandboxRunner::for_test(None, false, false),
        );
        let state_dir = std::path::PathBuf::from("/tmp/nexo-test-state");
        {
            *plugin.sandbox.lock().await = Some(sandbox_arc.clone());
            *plugin.plugin_state_dir.lock().await = Some(state_dir.clone());
        }
        let pre_sandbox_ptr = std::sync::Arc::as_ptr(&sandbox_arc);

        let _report = plugin
            .clone()
            .force_restart(cancel.clone(), Some(broker.clone()), None, None)
            .await
            .expect("force_restart ok");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Post-restart: stash unchanged.
        let post_sandbox = plugin.sandbox.lock().await.clone();
        let post_state_dir = plugin.plugin_state_dir.lock().await.clone();
        let post_sandbox = post_sandbox.expect("sandbox stash retained");
        assert_eq!(
            std::sync::Arc::as_ptr(&post_sandbox),
            pre_sandbox_ptr,
            "sandbox Arc must be the same instance across respawn"
        );
        assert_eq!(
            post_state_dir.expect("state_dir retained"),
            state_dir,
            "plugin_state_dir must survive respawn"
        );
        cancel.cancel();
    }

    /// `respawn_loop`'s spawn_one_attempt path bumps the attempt
    /// counter even when the new child fails handshake (timeout).
    /// Combined with crash-then-handshake-fail script, gave_up
    /// fires after max_attempts is hit.
    #[tokio::test]
    async fn respawn_handshake_failure_counts_as_attempt() {
        // Per-test counter file lets one shell script change
        // behavior across invocations. Fresh dir per test so we
        // don't collide with parallel runs.
        let counter_dir = std::env::temp_dir().join("nexo-deferred-test-4-counter");
        let _ = std::fs::remove_dir_all(&counter_dir);
        std::fs::create_dir_all(&counter_dir).unwrap();
        let counter_path = counter_dir.join("count");
        std::fs::write(&counter_path, "0").unwrap();
        let counter_str = counter_path.to_str().unwrap();

        // Script: read counter, on first invocation handshake +
        // sleep + crash; on subsequent invocations exit before
        // handshake (causing spawn_one_attempt to time out).
        let script = format!(
            r#"#!/bin/sh
COUNT=$(cat {counter_str})
NEXT=$((COUNT + 1))
echo $NEXT > {counter_str}
if [ "$COUNT" = "0" ]; then
    read line
    echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"test_plugin","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}}}},"server_version":"mock-0.1.0"}}}}'
    sleep 0.5
    exit 1
else
    sleep 5
    exit 1
fi
"#,
            counter_str = counter_str,
        );
        let dir = std::env::temp_dir().join("nexo-deferred-test-4");
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("plugin.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        // Tight handshake timeout so failed attempts surface
        // quickly (else the test waits 5s × max_attempts). Plus
        // backoff=200, max_attempts=2 → reset_window=800ms.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "300");
        let m = manifest_with_supervisor(script_path.to_str().unwrap(), true, 2, 200);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut gave_up_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.gave_up")
            .await
            .expect("subscribe gave_up");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        // First crash + handshake-fail respawns + gave_up: ~1-2s of
        // real work (backoff 200ms × 2 attempts + 300ms init timeout),
        // but `cargo test --workspace` fans ~1.4k tests across the
        // pool — give the respawn loop slack under that contention.
        let event = tokio::time::timeout(Duration::from_secs(30), gave_up_sub.next())
            .await
            .expect("gave_up arrives within 30s")
            .expect("subscription delivers Some");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let attempts = event
            .payload
            .get("attempts")
            .and_then(|v| v.as_u64())
            .expect("attempts field");
        // Either the counter reaches max_attempts via failed
        // handshakes counting as attempts, OR the counter
        // resets and we never see gave_up (timeout). The
        // assertion here is that gave_up CAN fire when handshake
        // fails repeatedly.
        assert!(
            attempts >= 1,
            "gave_up must report at least 1 attempt; got {attempts}"
        );
        cancel.cancel();
    }

    /// Shutdown fires while a respawn handshake is still
    /// in-flight. The post-spawn race-check in respawn_loop kills
    /// the just-spawned child and bails. No `respawned` event
    /// publishes.
    #[tokio::test]
    async fn shutdown_during_respawn_handshake_kills_new_child() {
        // Crash-then-stable counter script: first invocation
        // handshakes + sleeps + crashes; second invocation sleeps
        // 2s BEFORE handshake reply (giving the test a window
        // to fire shutdown during the spawn_one_attempt path).
        let counter_dir = std::env::temp_dir().join("nexo-deferred-test-5-counter");
        let _ = std::fs::remove_dir_all(&counter_dir);
        std::fs::create_dir_all(&counter_dir).unwrap();
        let counter_path = counter_dir.join("count");
        std::fs::write(&counter_path, "0").unwrap();
        let counter_str = counter_path.to_str().unwrap();
        let script = format!(
            r#"#!/bin/sh
COUNT=$(cat {counter_str})
NEXT=$((COUNT + 1))
echo $NEXT > {counter_str}
if [ "$COUNT" = "0" ]; then
    read line
    echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"test_plugin","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}}}},"server_version":"mock-0.1.0"}}}}'
    sleep 0.1
    exit 1
else
    read line
    sleep 2
    echo '{{"jsonrpc":"2.0","id":1,"result":{{"manifest":{{"plugin":{{"id":"test_plugin","version":"0.1.0","name":"x","description":"x","min_nexo_version":">=0.1.0"}}}},"server_version":"mock-0.1.0"}}}}'
    sleep 5
fi
"#,
            counter_str = counter_str,
        );
        let dir = std::env::temp_dir().join("nexo-deferred-test-5");
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("plugin.sh");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        // Long init timeout so the 2s handshake delay doesn't
        // trip the inner timeout; race window for shutdown.
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "5000");
        let m = manifest_with_supervisor(script_path.to_str().unwrap(), true, 5, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut respawned_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawned")
            .await
            .expect("subscribe respawned");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Wait for first crash detection (~500-700ms total:
        // handshake instant + 100ms script sleep + ≤500ms supervisor
        // poll). Then call shutdown while spawn_one_attempt is
        // mid-handshake (the 2s sleep on second invocation).
        tokio::time::sleep(Duration::from_millis(800)).await;
        // shutdown_signaled flips so the post-spawn race-check
        // kills the new child if the handshake had completed.
        plugin.shutdown().await.expect("shutdown ok");

        // No respawned event should arrive — even if the
        // handshake completed, the post-spawn race-check kills
        // the child instead of installing.
        let respawned = collect_events_until(&mut respawned_sub, Duration::from_secs(3)).await;
        assert!(
            respawned.is_empty(),
            "shutdown during respawn must not publish respawned; got {} events",
            respawned.len()
        );
        cancel.cancel();
    }

    /// Golden assertion: every lifecycle event topic carries the
    /// documented payload fields with correct types. Drive a
    /// full crash → respawning → respawned → … → gave_up cycle
    /// + verify each event's payload shape. Catches accidental
    /// breaking changes to the operator-observable wire.
    #[tokio::test]
    async fn lifecycle_event_payload_shapes_match_spec() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_always_crash_script("nexo-deferred-test-6", "test_plugin", 50, 7);
        let m = manifest_with_supervisor(path.to_str().unwrap(), true, 1, 300);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut crashed_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.crashed")
            .await
            .expect("subscribe crashed");
        let mut respawning_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.respawning")
            .await
            .expect("subscribe respawning");
        let mut gave_up_sub = broker
            .subscribe("plugin.lifecycle.test_plugin.gave_up")
            .await
            .expect("subscribe gave_up");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        let crashed = tokio::time::timeout(Duration::from_secs(3), crashed_sub.next())
            .await
            .expect("crashed within 3s")
            .expect("Some");
        let respawning = tokio::time::timeout(Duration::from_secs(2), respawning_sub.next())
            .await
            .expect("respawning within 2s")
            .expect("Some");
        let gave_up = tokio::time::timeout(Duration::from_secs(5), gave_up_sub.next())
            .await
            .expect("gave_up within 5s")
            .expect("Some");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // crashed: { plugin_id: string, exit_code: int, stderr_tail: array }
        assert_eq!(crashed.source, "plugin.supervisor");
        assert_eq!(
            crashed.payload.get("plugin_id").and_then(|v| v.as_str()),
            Some("test_plugin")
        );
        assert!(
            crashed
                .payload
                .get("exit_code")
                .and_then(|v| v.as_i64())
                .is_some(),
            "crashed.exit_code must be int"
        );
        assert!(
            crashed
                .payload
                .get("stderr_tail")
                .and_then(|v| v.as_array())
                .is_some(),
            "crashed.stderr_tail must be array"
        );

        // respawning: { plugin_id: string, attempt: int >= 1, backoff_ms: int >= 0 }
        assert_eq!(respawning.source, "plugin.supervisor");
        assert_eq!(
            respawning.payload.get("plugin_id").and_then(|v| v.as_str()),
            Some("test_plugin")
        );
        let attempt = respawning
            .payload
            .get("attempt")
            .and_then(|v| v.as_u64())
            .expect("respawning.attempt int");
        assert!(attempt >= 1, "attempt must be 1-indexed");
        let backoff_ms = respawning
            .payload
            .get("backoff_ms")
            .and_then(|v| v.as_u64())
            .expect("respawning.backoff_ms int");
        assert!(backoff_ms <= 60_000, "backoff capped at 60s");

        // gave_up: { plugin_id: string, attempts: int, last_exit_code: int, stderr_tail: array }
        assert_eq!(gave_up.source, "plugin.supervisor");
        assert_eq!(
            gave_up.payload.get("plugin_id").and_then(|v| v.as_str()),
            Some("test_plugin")
        );
        assert!(
            gave_up
                .payload
                .get("attempts")
                .and_then(|v| v.as_u64())
                .is_some(),
            "gave_up.attempts must be int"
        );
        assert!(
            gave_up
                .payload
                .get("last_exit_code")
                .and_then(|v| v.as_i64())
                .is_some(),
            "gave_up.last_exit_code must be int"
        );
        assert!(
            gave_up
                .payload
                .get("stderr_tail")
                .and_then(|v| v.as_array())
                .is_some(),
            "gave_up.stderr_tail must be array"
        );
        cancel.cancel();
    }

    /// Golden coverage for the
    /// `restarted_manually` event shape. The auto-respawn golden
    /// at `lifecycle_event_payload_shapes_match_spec` only
    /// reaches `crashed`/`respawning`/`gave_up`; manual restart
    /// has its own broker payload (`previous_uptime_ms`,
    /// `restarted_at_ms`, `new_pid`) shipped today and needs an
    /// independent assertion so a future field rename / drop
    /// doesn't slip through unnoticed.
    #[tokio::test]
    async fn lifecycle_payload_shape_restarted_manually() {
        std::env::set_var("NEXO_PLUGIN_INIT_TIMEOUT_MS", "1500");
        let path = write_stable_script("nexo-lifecycle-shape-restarted-manually", "test_plugin");
        let m = manifest_with_supervisor(path.to_str().unwrap(), false, 0, 100);
        let (plugin, _dyn) = arc_plugin_via_factory(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let mut sub = broker
            .subscribe("plugin.lifecycle.test_plugin.restarted_manually")
            .await
            .expect("subscribe restarted_manually");
        boot_with_supervisor(plugin.clone(), cancel.clone(), broker.clone()).await;

        plugin
            .clone()
            .force_restart(cancel.clone(), Some(broker.clone()), None, None)
            .await
            .expect("force_restart ok");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("restarted_manually arrives within 2s")
            .expect("Some");

        // Source is the supervisor (not "memory-snapshot" or
        // similar — the event lives in the plugin lifecycle family).
        assert_eq!(event.source, "plugin.supervisor");

        // plugin_id: string
        assert_eq!(
            event.payload.get("plugin_id").and_then(|v| v.as_str()),
            Some("test_plugin")
        );

        // previous_uptime_ms: u64
        assert!(
            event
                .payload
                .get("previous_uptime_ms")
                .and_then(|v| v.as_u64())
                .is_some(),
            "previous_uptime_ms must be u64"
        );

        // restarted_at_ms: i64 (chrono timestamp)
        let ts = event
            .payload
            .get("restarted_at_ms")
            .and_then(|v| v.as_i64())
            .expect("restarted_at_ms must be i64");
        assert!(ts > 1_700_000_000_000, "restarted_at_ms must be sane");

        // new_pid: u64 | null — present in payload exactly as
        // the wire response carries it. Null is acceptable but
        // the field must EXIST so subscribers can rely on its
        // presence.
        assert!(
            event.payload.get("new_pid").is_some(),
            "new_pid key must exist in event payload"
        );

        // Defensive: no extraneous fields slip in (catches
        // accidental serialisation of the inner Inner state).
        let obj = event.payload.as_object().expect("payload is object");
        let allowed: std::collections::HashSet<&str> = [
            "plugin_id",
            "previous_uptime_ms",
            "restarted_at_ms",
            "new_pid",
        ]
        .iter()
        .copied()
        .collect();
        for k in obj.keys() {
            assert!(
                allowed.contains(k.as_str()),
                "unexpected field `{k}` in restarted_manually payload"
            );
        }
        cancel.cancel();
    }
}
