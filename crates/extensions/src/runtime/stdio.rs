//! Stdio extension runtime — spawns a child process and speaks JSON-RPC 2.0
//! over line-delimited stdio. Supervises the child with restart-on-crash and
//! guards outgoing calls with a circuit breaker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use agent_resilience::{CircuitBreaker, CircuitBreakerConfig};

use crate::manifest::{ExtensionManifest, Transport};

use super::transport::ExtensionTransport;
use super::wire;
use super::{CallError, HandshakeInfo, RpcError, RuntimeState, StartError};

const OUTBOX_CAP: usize = 64;
const DEFAULT_ENV_BLOCKLIST: &[&str] = &[
    // Suffix patterns — match at the trailing edge of the var name.
    "_TOKEN",
    "_KEY",
    "_SECRET",
    "_PASSWORD",
    "_PASSWD",
    "_PWD",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_PAT",
    "_AUTH",
    "_APIKEY",
    "_BEARER",
    "_SESSION",
];

/// Unconditional substring patterns — matched anywhere in the var name.
/// Catches names like `PASSWORD_DB` (trailing table segment) or
/// `AWS_SECRET_ACCESS_KEY` (secret mid-name).
const DEFAULT_ENV_BLOCKLIST_SUBSTRS: &[&str] = &[
    "PASSWORD", "SECRET", "CREDENTIAL", "PRIVATE_KEY",
];

pub struct StdioSpawnOptions {
    pub cwd: PathBuf,
    pub call_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_restart_attempts: u32,
    pub restart_window: Duration,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    pub env_blocklist_suffixes: Vec<String>,
    pub shutdown_grace: Duration,
}

impl Default for StdioSpawnOptions {
    fn default() -> Self {
        Self {
            cwd: PathBuf::from("."),
            call_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(10),
            max_restart_attempts: 5,
            restart_window: Duration::from_secs(300),
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
            env_blocklist_suffixes: DEFAULT_ENV_BLOCKLIST.iter().map(|s| s.to_string()).collect(),
            shutdown_grace: Duration::from_secs(3),
        }
    }
}

type PendingMap = DashMap<u64, oneshot::Sender<Result<serde_json::Value, RpcError>>>;

pub struct StdioRuntime {
    extension_id: String,
    state: Arc<RwLock<RuntimeState>>,
    outbox_tx: mpsc::Sender<String>,
    pending: Arc<PendingMap>,
    next_id: Arc<AtomicU64>,
    breaker: Arc<CircuitBreaker>,
    shutdown: CancellationToken,
    handshake: HandshakeInfo,
    opts: StdioSpawnOptions,
    supervisor_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for StdioRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioRuntime")
            .field("extension_id", &self.extension_id)
            .field(
                "state",
                &self.state.try_read().map(|g| g.clone()).ok(),
            )
            .field("pending_requests", &self.pending.len())
            .field("shutdown_requested", &self.shutdown.is_cancelled())
            .field("handshake", &self.handshake)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod debug_tests {
    use super::*;

    #[test]
    fn debug_renders_extension_id_without_panic() {
        // A minimal runtime constructed without spawning a child — only
        // exercises the `Debug` impl.
        let (outbox_tx, _rx) = mpsc::channel::<String>(1);
        let rt = StdioRuntime {
            extension_id: "weather".into(),
            state: Arc::new(RwLock::new(RuntimeState::Spawning)),
            outbox_tx,
            pending: Arc::new(DashMap::new()),
            next_id: Arc::new(AtomicU64::new(1)),
            breaker: Arc::new(CircuitBreaker::new(
                "ext:weather",
                CircuitBreakerConfig::default(),
            )),
            shutdown: CancellationToken::new(),
            handshake: HandshakeInfo::default(),
            opts: StdioSpawnOptions::default(),
            supervisor_handle: None,
        };
        let s = format!("{rt:?}");
        assert!(s.contains("StdioRuntime"));
        assert!(s.contains("weather"));
        assert!(s.contains("pending_requests: 0"));
    }
}

impl StdioRuntime {
    /// Convenience entry point; pass manifest + cwd, get a supervised runtime.
    pub async fn spawn(
        manifest: &ExtensionManifest,
        cwd: PathBuf,
    ) -> Result<Self, StartError> {
        Self::spawn_with(manifest, StdioSpawnOptions { cwd, ..Default::default() }).await
    }

    pub async fn spawn_with(
        manifest: &ExtensionManifest,
        opts: StdioSpawnOptions,
    ) -> Result<Self, StartError> {
        let (command, args) = match &manifest.transport {
            Transport::Stdio { command, args } => (command.clone(), args.clone()),
            _ => return Err(StartError::UnsupportedTransport),
        };

        let extension_id = manifest.id().to_string();

        // Preflight: warn if declared `requires.bins` / `requires.env`
        // are absent. Not a hard fail — some extensions tolerate
        // missing optional deps — but the operator gets a clear
        // breadcrumb when an extension silently degrades.
        let (missing_bins, missing_env) = manifest.requires.missing();
        if !missing_bins.is_empty() || !missing_env.is_empty() {
            tracing::warn!(
                extension = %extension_id,
                missing_bins = ?missing_bins,
                missing_env = ?missing_env,
                "extension preflight: declared requires not satisfied (continuing anyway)"
            );
        }
        let state = Arc::new(RwLock::new(RuntimeState::Spawning));
        let pending: Arc<PendingMap> = Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));
        let breaker = Arc::new(CircuitBreaker::new(
            // Transport-prefixed so the same extension id running on
            // two transports (edge case of misconfig) doesn't collide.
            format!("ext:stdio:{extension_id}"),
            CircuitBreakerConfig::default(),
        ));
        let shutdown = CancellationToken::new();
        let (outbox_tx, outbox_rx) = mpsc::channel::<String>(OUTBOX_CAP);

        // First spawn + handshake. This returns the live Child plus
        // the fresh stdin handle — we hand both to the supervisor so
        // restart cycles can own the full lifecycle.
        let (child, stdin, reader_handle, handshake) = spawn_and_handshake(
            &extension_id,
            &command,
            &args,
            &opts,
            pending.clone(),
            outbox_tx.clone(),
            shutdown.clone(),
        )
        .await?;

        *state.write().unwrap() = RuntimeState::Ready;

        // Supervisor — owns the Child + outbox_rx + stdin from here on
        // so it can kill-then-respawn on crash with a fresh stdin while
        // the caller's outbox_tx keeps routing to whatever child is
        // currently alive.
        let supervisor_handle = {
            let command = command.clone();
            let args = args.clone();
            let opts_clone = clone_opts(&opts);
            let state_clone = state.clone();
            let pending_clone = pending.clone();
            let outbox_tx_clone = outbox_tx.clone();
            let shutdown_clone = shutdown.clone();
            let ext_id = extension_id.clone();
            tokio::spawn(async move {
                supervisor(
                    ext_id,
                    command,
                    args,
                    opts_clone,
                    child,
                    stdin,
                    reader_handle,
                    state_clone,
                    pending_clone,
                    outbox_tx_clone,
                    outbox_rx,
                    shutdown_clone,
                )
                .await;
            })
        };

        Ok(Self {
            extension_id,
            state,
            outbox_tx,
            pending,
            next_id,
            breaker,
            shutdown,
            handshake,
            opts,
            supervisor_handle: Some(supervisor_handle),
        })
    }

    pub fn extension_id(&self) -> &str {
        &self.extension_id
    }

    pub fn handshake(&self) -> &HandshakeInfo {
        &self.handshake
    }

    pub fn state(&self) -> RuntimeState {
        self.state.read().unwrap().clone()
    }

    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        self.call_with_timeout(method, params, None).await
    }

    /// Like [`call`] but lets the caller pin a per-invocation timeout
    /// instead of inheriting `StdioSpawnOptions::call_timeout`. Useful
    /// for hook handlers where the operator wants a tight bound on any
    /// single hook without shortening the tool-call budget.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout_override: Option<Duration>,
    ) -> Result<serde_json::Value, CallError> {
        if !matches!(self.state(), RuntimeState::Ready) {
            return Err(CallError::NotReady(format!("{:?}", self.state())));
        }
        if !self.breaker.allow() {
            return Err(CallError::BreakerOpen);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        let line = match wire::encode(method, &params, id) {
            Ok(l) => l,
            Err(e) => {
                self.pending.remove(&id);
                return Err(CallError::Encode(e));
            }
        };

        if self.outbox_tx.send(line).await.is_err() {
            self.pending.remove(&id);
            self.breaker.on_failure();
            return Err(CallError::ChildExited);
        }

        let timeout = timeout_override.unwrap_or(self.opts.call_timeout);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => {
                self.breaker.on_success();
                Ok(v)
            }
            Ok(Ok(Err(e))) => {
                self.breaker.on_failure();
                Err(CallError::Rpc(e))
            }
            Ok(Err(_canceled)) => {
                self.breaker.on_failure();
                Err(CallError::ChildExited)
            }
            Err(_elapsed) => {
                self.pending.remove(&id);
                self.breaker.on_failure();
                Err(CallError::Timeout(timeout))
            }
        }
    }

    pub async fn tools_list(&self) -> Result<Vec<super::ToolDescriptor>, CallError> {
        let v = self.call("tools/list", serde_json::json!({})).await?;
        if let Some(arr) = v.get("tools").cloned() {
            serde_json::from_value(arr).map_err(CallError::Decode)
        } else {
            serde_json::from_value(v).map_err(CallError::Decode)
        }
    }

    pub async fn tools_call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        self.call(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Graceful shutdown: send `shutdown` notification and wait `grace`.
    /// Takes `&self` so callers holding `Arc<StdioRuntime>` can invoke it;
    /// the supervisor task is aborted in `Drop` when the last Arc drops.
    pub async fn shutdown(&self) {
        self.shutdown_with_reason("client shutdown").await
    }

    /// Phase 11.3 follow-up — shutdown variant that threads a
    /// human-readable `reason` into the `shutdown` notification payload
    /// so extensions can differentiate SIGTERM vs config reload vs
    /// explicit user action in their cleanup logic.
    pub async fn shutdown_with_reason(&self, reason: &str) {
        let reason = sanitize_reason(reason);
        self.shutdown.cancel();
        if let Ok(line) = wire::encode(
            "shutdown",
            serde_json::json!({ "reason": reason }),
            0,
        ) {
            let _ = self.outbox_tx.try_send(line);
        }
        tokio::time::sleep(self.opts.shutdown_grace).await;
    }
}

fn sanitize_reason(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

#[async_trait::async_trait]
impl ExtensionTransport for StdioRuntime {
    fn extension_id(&self) -> &str {
        StdioRuntime::extension_id(self)
    }

    fn handshake(&self) -> &HandshakeInfo {
        StdioRuntime::handshake(self)
    }

    fn state(&self) -> RuntimeState {
        StdioRuntime::state(self)
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        StdioRuntime::call(self, method, params).await
    }

    async fn tools_list(&self) -> Result<Vec<super::ToolDescriptor>, CallError> {
        StdioRuntime::tools_list(self).await
    }

    async fn tools_call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, CallError> {
        StdioRuntime::tools_call(self, name, arguments).await
    }

    async fn shutdown(&self) {
        StdioRuntime::shutdown(self).await
    }

    async fn shutdown_with_reason(&self, reason: &str) {
        StdioRuntime::shutdown_with_reason(self, reason).await
    }
}

impl Drop for StdioRuntime {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(h) = self.supervisor_handle.take() {
            h.abort();
        }
        // Child is killed via `kill_on_drop(true)` when the supervisor's
        // owned `Child` is dropped by its task being aborted.
    }
}

// ─── Internals ────────────────────────────────────────────────────────────────

fn clone_opts(o: &StdioSpawnOptions) -> StdioSpawnOptions {
    StdioSpawnOptions {
        cwd: o.cwd.clone(),
        call_timeout: o.call_timeout,
        handshake_timeout: o.handshake_timeout,
        max_restart_attempts: o.max_restart_attempts,
        restart_window: o.restart_window,
        base_backoff: o.base_backoff,
        max_backoff: o.max_backoff,
        env_blocklist_suffixes: o.env_blocklist_suffixes.clone(),
        shutdown_grace: o.shutdown_grace,
    }
}

fn env_is_blocked(name: &str, suffixes: &[String]) -> bool {
    let upper = name.to_ascii_uppercase();
    if suffixes.iter().any(|suf| upper.ends_with(suf.as_str())) {
        return true;
    }
    DEFAULT_ENV_BLOCKLIST_SUBSTRS
        .iter()
        .any(|sub| upper.contains(sub))
}

#[cfg(test)]
mod shebang_diag_tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn reports_missing_interpreter_via_env() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("main.py");
        let mut f = std::fs::File::create(&script).unwrap();
        writeln!(f, "#!/usr/bin/env definitely-not-an-interpreter-xyz").unwrap();
        writeln!(f, "print('hi')").unwrap();
        let msg = diagnose_missing_interpreter(script.to_str().unwrap(), tmp.path());
        let msg = msg.expect("should detect missing interpreter");
        assert!(msg.contains("definitely-not-an-interpreter-xyz"), "msg: {msg}");
        assert!(msg.contains("shebang"), "msg: {msg}");
    }

    #[test]
    fn returns_none_when_shebang_interpreter_is_present() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("main.sh");
        let mut f = std::fs::File::create(&script).unwrap();
        // `/bin/sh` is effectively universal on Unix hosts we target.
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, "echo hi").unwrap();
        assert!(diagnose_missing_interpreter(script.to_str().unwrap(), tmp.path()).is_none());
    }

    #[test]
    fn returns_none_when_script_itself_is_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(diagnose_missing_interpreter("main.py", tmp.path()).is_none());
    }
}

#[cfg(test)]
mod env_blocklist_tests {
    use super::*;

    fn suffixes() -> Vec<String> {
        DEFAULT_ENV_BLOCKLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn blocks_common_suffixes() {
        for v in [
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "STRIPE_SECRET",
            "DB_PASSWORD",
            "AWS_SESSION",
        ] {
            assert!(env_is_blocked(v, &suffixes()), "expected block: {v}");
        }
    }

    #[test]
    fn blocks_substring_patterns() {
        // `PASSWORD_DB` doesn't end in a blocklisted suffix, but contains
        // `PASSWORD` so it's caught.
        assert!(env_is_blocked("PASSWORD_DB", &suffixes()));
        assert!(env_is_blocked("MY_PRIVATE_KEY_PEM", &suffixes()));
        assert!(env_is_blocked("AWS_SECRET_ACCESS_KEY", &suffixes()));
    }

    #[test]
    fn passes_benign_names() {
        for v in ["HOME", "PATH", "USER", "LANG", "RUST_LOG"] {
            assert!(!env_is_blocked(v, &suffixes()), "unexpected block: {v}");
        }
    }
}

/// Inspect `command` for a `#!` shebang and, if the interpreter path
/// it names is absent, return a human-readable message naming the
/// missing interpreter. Caller wraps it in a `std::io::Error` so the
/// `StartError::Spawn` bubble-up mentions `python3` / `bash` instead
/// of the confusing "No such file or directory (os error 2)" that
/// ENOENT on an interpreter normally yields.
fn diagnose_missing_interpreter(command: &str, cwd: &std::path::Path) -> Option<String> {
    let script_path = if std::path::Path::new(command).is_absolute() {
        std::path::PathBuf::from(command)
    } else {
        cwd.join(command)
    };
    if !script_path.exists() {
        // The script itself is missing — let the default ENOENT stand.
        return None;
    }
    let first_line = std::fs::read_to_string(&script_path)
        .ok()?
        .lines()
        .next()?
        .to_string();
    let rest = first_line.strip_prefix("#!")?.trim();
    // Handle `#!/usr/bin/env python3` and plain `#!/usr/bin/python3`.
    let (interpreter, check_in_path) = if let Some(after_env) = rest.strip_prefix("/usr/bin/env ")
    {
        (
            after_env.split_whitespace().next().unwrap_or("").to_string(),
            true,
        )
    } else {
        (
            rest.split_whitespace().next().unwrap_or("").to_string(),
            false,
        )
    };
    if interpreter.is_empty() {
        return None;
    }
    if check_in_path {
        if which_in_path(&interpreter).is_some() {
            return None;
        }
    } else if std::path::Path::new(&interpreter).exists() {
        return None;
    }
    Some(format!(
        "spawn failed for `{}`: shebang interpreter `{}` not found on host (install it or rewrite the shebang)",
        script_path.display(),
        interpreter,
    ))
}

/// Minimal `which` — returns the first entry of `$PATH` that contains
/// an executable named `name`. Stdlib-only.
fn which_in_path(name: &str) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if let Ok(md) = std::fs::metadata(&candidate) {
                if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                    return Some(candidate);
                }
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn build_command(
    extension_id: &str,
    command: &str,
    args: &[String],
    opts: &StdioSpawnOptions,
) -> Command {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(&opts.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Strip secrets by suffix blocklist before passing env.
    let env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !env_is_blocked(k, &opts.env_blocklist_suffixes))
        .collect();
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.env("AGENT_VERSION", env!("CARGO_PKG_VERSION"));
    cmd.env("AGENT_EXTENSION_ID", extension_id);
    cmd
}

/// Spawn the child, wire reader/stderr tasks (NOT writer — the supervisor
/// drives stdin directly so it can swap it on restart without losing
/// callers' outbox_tx), and run the initialize handshake. Returns the
/// live `Child`, a stdin handle for the supervisor to own, the reader
/// task's JoinHandle for cancel-on-restart, and the parsed handshake.
#[allow(clippy::too_many_arguments)]
async fn spawn_and_handshake(
    extension_id: &str,
    command: &str,
    args: &[String],
    opts: &StdioSpawnOptions,
    pending: Arc<PendingMap>,
    outbox_tx: mpsc::Sender<String>,
    shutdown: CancellationToken,
) -> Result<(Child, ChildStdin, JoinHandle<()>, HandshakeInfo), StartError> {
    let mut child = build_command(extension_id, command, args, opts)
        .spawn()
        .map_err(|e| {
            // ENOENT on a script with a shebang often means the
            // *interpreter* is missing, not the script itself — the
            // kernel swallows the shebang line and returns ENOENT with
            // no way to tell which path failed. We re-check the
            // shebang of the target file and, when the interpreter is
            // missing, synthesize a clearer error pointing at it.
            if e.kind() == std::io::ErrorKind::NotFound {
                if let Some(hint) = diagnose_missing_interpreter(command, &opts.cwd) {
                    return StartError::Spawn(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        hint,
                    ));
                }
            }
            StartError::Spawn(e)
        })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        StartError::Spawn(std::io::Error::other("child stdin missing"))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        StartError::Spawn(std::io::Error::other("child stdout missing"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        StartError::Spawn(std::io::Error::other("child stderr missing"))
    })?;

    // Per-child reader task — owned by the supervisor so it can abort
    // it on child exit before respawning a fresh reader for the new
    // child. Without the abort, the old reader would happily consume
    // the new child's stdout (it holds the ChildStdout anyway — scope
    // prevents that, but the pattern is cleaner this way).
    let ext_id_reader = extension_id.to_string();
    let reader_handle = tokio::spawn(reader_task(
        stdout,
        pending.clone(),
        shutdown.clone(),
        ext_id_reader,
    ));
    let ext_id_stderr = extension_id.to_string();
    tokio::spawn(stderr_task(stderr, shutdown.clone(), ext_id_stderr));

    // Handshake: id=0 reserved so regular calls (next_id starts at 1) never collide.
    let (hs_tx, hs_rx) = oneshot::channel();
    pending.insert(0, hs_tx);
    let handshake_line = wire::encode(
        "initialize",
        serde_json::json!({
            "agent_version": env!("CARGO_PKG_VERSION"),
            "extension_id": extension_id,
        }),
        0,
    )
    .map_err(StartError::DecodeHandshake)?;

    // Handshake goes through outbox so the supervisor's stdin-drain
    // loop (to be started after this function returns) picks it up
    // immediately once it runs — but we also write it directly here
    // so the child can respond before the supervisor is set up.
    // Direct write keeps the handshake fast and avoids a bootstrapping
    // chicken-and-egg.
    let _ = outbox_tx; // kept for signature symmetry with old call sites
    // Actually we DO still need the supervisor to drain the handshake.
    // The simplest path: write it directly to the child's stdin now,
    // before returning stdin to the supervisor.
    let mut stdin = stdin;
    if let Err(e) = stdin.write_all(handshake_line.as_bytes()).await {
        return Err(StartError::Spawn(std::io::Error::other(format!(
            "handshake write failed: {e}"
        ))));
    }
    if let Err(e) = stdin.flush().await {
        return Err(StartError::Spawn(std::io::Error::other(format!(
            "handshake flush failed: {e}"
        ))));
    }

    let handshake_value = match tokio::time::timeout(opts.handshake_timeout, hs_rx).await {
        Ok(Ok(Ok(v))) => v,
        Ok(Ok(Err(rpc))) => return Err(StartError::Handshake(rpc)),
        Ok(Err(_canceled)) => return Err(StartError::EarlyExit),
        Err(_elapsed) => return Err(StartError::HandshakeTimeout(opts.handshake_timeout)),
    };

    let handshake: HandshakeInfo = serde_json::from_value(handshake_value)
        .map_err(StartError::DecodeHandshake)?;

    Ok((child, stdin, reader_handle, handshake))
}

async fn reader_task(
    stdout: ChildStdout,
    pending: Arc<PendingMap>,
    shutdown: CancellationToken,
    extension_id: String,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = lines.next_line() => {
                match maybe {
                    Ok(Some(line)) => {
                        let response = match wire::decode_response(&line) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!(ext=%extension_id, error=%e, raw=%line, "invalid jsonrpc line");
                                continue;
                            }
                        };
                        let Some(id_val) = response.id.as_ref() else {
                            tracing::debug!(ext=%extension_id, "notification ignored");
                            continue;
                        };
                        let Some(id) = wire::extract_u64_id(id_val) else {
                            tracing::warn!(ext=%extension_id, "non-numeric id, dropping");
                            continue;
                        };
                        if let Some((_, tx)) = pending.remove(&id) {
                            let payload = match (response.result, response.error) {
                                (Some(v), None) => Ok(v),
                                (None, Some(e)) => Err(e),
                                (Some(v), Some(_)) => Ok(v),   // prefer result if both
                                (None, None) => Ok(serde_json::Value::Null),
                            };
                            let _ = tx.send(payload);
                        } else {
                            tracing::debug!(ext=%extension_id, id=%id, "late response, no pending entry");
                        }
                    }
                    Ok(None) => {
                        tracing::info!(ext=%extension_id, "stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(ext=%extension_id, error=%e, "stdout read error");
                        break;
                    }
                }
            }
        }
    }
}

async fn stderr_task(
    stderr: ChildStderr,
    shutdown: CancellationToken,
    extension_id: String,
) {
    let mut lines = BufReader::new(stderr).lines();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            maybe = lines.next_line() => {
                match maybe {
                    Ok(Some(line)) => tracing::warn!(ext=%extension_id, stderr=%line, "extension stderr"),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn supervisor(
    extension_id: String,
    command: String,
    args: Vec<String>,
    opts: StdioSpawnOptions,
    mut child: Child,
    mut stdin: ChildStdin,
    mut reader_handle: JoinHandle<()>,
    state: Arc<RwLock<RuntimeState>>,
    pending: Arc<PendingMap>,
    outbox_tx: mpsc::Sender<String>,
    mut outbox_rx: mpsc::Receiver<String>,
    shutdown: CancellationToken,
) {
    let mut attempts: u32 = 0;
    let mut last_restart = Instant::now();

    'outer: loop {
        // Inner loop: drive I/O for the current child. Supervisor owns
        // stdin so restart can swap it atomically; outbox_rx is a
        // single receiver that outlives every child.
        let exit_status = loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    reader_handle.abort();
                    return;
                }
                status = child.wait() => break status,
                maybe_line = outbox_rx.recv() => {
                    match maybe_line {
                        Some(line) => {
                            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                                tracing::warn!(
                                    ext=%extension_id,
                                    error=%e,
                                    "stdin write failed — killing child to trigger restart"
                                );
                                // Kill so `child.wait()` wakes up and we
                                // fall into the respawn path; without
                                // this we'd live-lock sending to a dead
                                // stdin forever.
                                let _ = child.kill().await;
                                continue;
                            }
                            if let Err(e) = stdin.flush().await {
                                tracing::warn!(
                                    ext=%extension_id,
                                    error=%e,
                                    "stdin flush failed — killing child"
                                );
                                let _ = child.kill().await;
                            }
                        }
                        None => {
                            // outbox_tx dropped (StdioRuntime dropped
                            // without calling shutdown) — unusual, but
                            // tear down gracefully.
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            reader_handle.abort();
                            return;
                        }
                    }
                }
            }
        };

        tracing::info!(ext=%extension_id, status=?exit_status, "extension child exited");
        reader_handle.abort();
        drain_pending(&pending);

        if shutdown.is_cancelled() {
            return;
        }

        // Reset attempt window if we ran long enough.
        if last_restart.elapsed() > opts.restart_window {
            attempts = 0;
        }

        if attempts >= opts.max_restart_attempts {
            *state.write().unwrap() =
                RuntimeState::Failed { reason: "max restart attempts exceeded".into() };
            tracing::error!(ext=%extension_id, "extension permanently failed");
            return;
        }

        attempts += 1;
        let backoff = (opts.base_backoff
            * 2u32.saturating_pow(attempts.saturating_sub(1).min(6)))
            .min(opts.max_backoff);
        *state.write().unwrap() = RuntimeState::Restarting { attempt: attempts };
        tracing::warn!(ext=%extension_id, attempt=attempts, ?backoff, "restarting extension");

        // Sleep interruptible by shutdown so operators don't wait 16s
        // to kill a crashed extension.
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }

        // Respawn: reuse the same outbox_rx + outbox_tx so the caller's
        // `StdioRuntime.call()` path keeps working without a stale id
        // space or lost queues. Fresh child, fresh stdin, fresh reader.
        match spawn_and_handshake(
            &extension_id,
            &command,
            &args,
            &opts,
            pending.clone(),
            outbox_tx.clone(),
            shutdown.clone(),
        )
        .await
        {
            Ok((new_child, new_stdin, new_reader, handshake)) => {
                tracing::info!(
                    ext=%extension_id,
                    server_version=?handshake.server_version,
                    "extension respawned"
                );
                child = new_child;
                stdin = new_stdin;
                reader_handle = new_reader;
                last_restart = Instant::now();
                *state.write().unwrap() = RuntimeState::Ready;
                continue 'outer;
            }
            Err(e) => {
                tracing::error!(ext=%extension_id, error=?e, "respawn failed — will retry");
                // Leave state as Restarting; loop iteration will bump
                // attempts and eventually hit max → Failed.
            }
        }
    }
}

fn drain_pending(pending: &Arc<PendingMap>) {
    let ids: Vec<u64> = pending.iter().map(|e| *e.key()).collect();
    for id in ids {
        if let Some((_, tx)) = pending.remove(&id) {
            let _ = tx.send(Err(RpcError {
                code: -32000,
                message: "child exited".into(),
                data: None,
            }));
        }
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_reason;

    #[test]
    fn strips_control_chars() {
        assert_eq!(sanitize_reason("line1\nline2\t"), "line1line2");
    }

    #[test]
    fn truncates_to_200() {
        let raw: String = "x".repeat(500);
        assert_eq!(sanitize_reason(&raw).len(), 200);
    }

    #[test]
    fn passes_typical() {
        assert_eq!(sanitize_reason("sigterm"), "sigterm");
    }
}
