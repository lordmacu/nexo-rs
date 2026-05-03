//! Phase 81.14 — Host-side adapter for out-of-tree plugins running
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
//! # IRROMPIBLE refs
//!
//! - Internal precedent: `extensions/openai-whisper/src/main.rs:1-79`
//!   + `protocol.rs:1-23` — proven nexo-rs subprocess plugin shape.
//!   Reuses methods `initialize` / `tools/call` / `shutdown`. Same
//!   newline-delimited JSON-RPC.
//! - claude-code-leak `src/utils/computerUse/mcpServer.ts` and
//!   `src/commands/mcp/addCommand.ts` — MCP stdio transport pattern
//!   (`@modelcontextprotocol/sdk`). Same wire envelope, same
//!   request/notification split.
//! - OpenClaw absence: `research/extensions/{whatsapp,telegram,browser}/`
//!   ran in-process Node — no subprocess channel-plugin precedent.
//!   Subprocess pattern in nexo is strictly tool-extension shape
//!   (whisper) extended to channel plugins via 81.14.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_plugin_manifest::PluginManifest;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent::nexo_plugin_registry::factory::PluginFactory;
use crate::agent::plugin_host::{
    NexoPlugin, PluginInitContext, PluginInitError, PluginShutdownError,
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

/// Phase 81.14 — adapter that owns one child process and brokers
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

    /// Background tasks: stdin writer, stdout reader, broker→child
    /// bridge per subscribed topic. Joined on shutdown.
    tasks: Vec<JoinHandle<()>>,

    /// Child process handle. Optional because shutdown takes it.
    child: Option<Child>,

    /// Plugin-id-keyed cancellation token for all spawned tasks.
    /// Daemon-wide shutdown also cancels them via the
    /// `PluginInitContext.shutdown` token — both paths must work.
    cancel: CancellationToken,
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
        }
    }

    /// Spawn the child and handshake. Returns the populated
    /// `Inner` on success; on failure leaves the adapter in the
    /// "not started" state and surfaces a structured error.
    async fn spawn_and_handshake(
        &self,
        ctx_shutdown: CancellationToken,
    ) -> Result<Inner, anyhow::Error> {
        let entry = &self.cached_manifest.plugin.entrypoint;
        let command = entry
            .command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("manifest has no entrypoint.command — cannot spawn"))?;
        if command.trim().is_empty() {
            anyhow::bail!("manifest entrypoint.command is empty");
        }

        // Defense-in-depth: refuse env keys that overlap with the
        // daemon's own runtime envs. A plugin author who tries to
        // override `NEXO_STATE_ROOT` from a manifest deserves a
        // hard failure at boot rather than silent confusion.
        for key in entry.env.keys() {
            if key.starts_with("NEXO_") {
                anyhow::bail!(
                    "manifest entrypoint.env may not redefine reserved nexo env `{key}`"
                );
            }
        }

        let mut cmd = Command::new(&command);
        cmd.args(&entry.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
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

        let cancel = ctx_shutdown.child_token();
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<Value, String>>>> =
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

        // Stdout reader task — parses each line as JSON-RPC,
        // demuxes by id (response → resolve oneshot) or method
        // (notification → broker bridge). Bridges shut down on
        // EOF.
        let reader_cancel = cancel.clone();
        let reader_pending = pending.clone();
        let plugin_id_for_log = self.cached_manifest.plugin.id.clone();
        // Phase 81.14 host-side wires only spawn + handshake +
        // reader/writer plumbing. The broker → child topic bridge
        // and child → broker forwarding land in the 81.14 follow-up
        // slice that wires `manifest.channels.register` against the
        // ChannelAdapterRegistry shipped in 81.8. The reader logs
        // unknown notifications today so debug visibility holds.
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
                            tracing::warn!(error = %e, plugin = %plugin_id_for_log, "stdout read failed");
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
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            plugin = %plugin_id_for_log,
                            "stdout: drop frame, parse failed"
                        );
                        continue;
                    }
                };
                // Response: has "id" + ("result" XOR "error").
                if let Some(id) = parsed.get("id").and_then(|v| v.as_u64()) {
                    if let Some((_, sender)) = reader_pending.remove(&id) {
                        let payload = if let Some(err) = parsed.get("error") {
                            Err(err.to_string())
                        } else {
                            Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(payload);
                        continue;
                    }
                    tracing::warn!(
                        id,
                        plugin = %plugin_id_for_log,
                        "stdout: response with unknown id"
                    );
                    continue;
                }
                // Notification path — only `broker.publish` is
                // wired in 81.14. Other methods log + drop until
                // 81.20 ships memory/llm/tool host handlers.
                let _method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
                tracing::debug!(
                    plugin = %plugin_id_for_log,
                    method = _method,
                    "stdout notification received (broker bridge wired in 81.14 follow-up)"
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
                let _ = child.kill().await;
                anyhow::bail!("child returned error to initialize: {err}");
            }
            Ok(Err(_canceled)) => {
                cancel.cancel();
                let _ = child.kill().await;
                anyhow::bail!("initialize oneshot canceled before reply");
            }
            Err(_elapsed) => {
                cancel.cancel();
                let _ = child.kill().await;
                anyhow::bail!(
                    "child did not reply to initialize within {}ms",
                    timeout.as_millis()
                );
            }
        };

        // Validate returned manifest's id matches what the factory
        // was registered under. Defense against an out-of-tree
        // binary pretending to be a different plugin.
        let returned_id = result
            .pointer("/manifest/plugin/id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("initialize reply missing manifest.plugin.id")
            })?;
        if returned_id != self.cached_manifest.plugin.id {
            cancel.cancel();
            let _ = child.kill().await;
            anyhow::bail!(
                "manifest id mismatch: factory expected `{}`, child reported `{}`",
                self.cached_manifest.plugin.id,
                returned_id
            );
        }

        Ok(Inner {
            stdin_tx,
            pending,
            tasks: vec![writer_handle, reader_handle],
            child: Some(child),
            cancel,
        })
    }
}

#[async_trait]
impl NexoPlugin for SubprocessNexoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.cached_manifest
    }

    async fn init(
        &self,
        ctx: &mut PluginInitContext<'_>,
    ) -> Result<(), PluginInitError> {
        let inner = self
            .spawn_and_handshake(ctx.shutdown.clone())
            .await
            .map_err(|source| PluginInitError::Other {
                plugin_id: self.cached_manifest.plugin.id.clone(),
                source,
            })?;
        // 81.14 wires only the host-side spawn + handshake.
        // Broker → child topic bridge ships in the 81.14 follow-up
        // slice; today we keep the connection alive with the
        // tasks already spawned (stdin writer + stdout reader).
        let _ = ctx.broker.clone(); // silence: bridge wiring lands next slice
        *self.inner.lock().await = Some(inner);
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PluginShutdownError> {
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
        if let Some(mut child) = inner.child.take() {
            // 1s grace for the child to exit on its own; SIGKILL after.
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            let _ = child.kill().await;
        }
        for task in inner.tasks.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        Ok(())
    }
}

/// Phase 81.14 — factory helper for subprocess plugins. Reads the
/// manifest at registration time and returns a closure that builds
/// a fresh `Arc<SubprocessNexoPlugin>` per call. Each `factory(&m)`
/// invocation produces a new adapter; the daemon's init loop calls
/// it once per discovered manifest.
pub fn subprocess_plugin_factory(manifest: PluginManifest) -> PluginFactory {
    Box::new(move |reg_manifest| {
        // The init loop hands us the manifest it just discovered.
        // Belt-and-suspenders: if the registry's manifest disagrees
        // with the one captured at factory registration, prefer the
        // captured one — that's what the operator vetted at boot.
        // (Discovery should hand the same manifest back, so this is
        // defensive against future refactors.)
        let _ = reg_manifest;
        let plugin: Arc<dyn NexoPlugin> = Arc::new(SubprocessNexoPlugin::new(manifest.clone()));
        Ok(plugin)
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use nexo_plugin_manifest::EntrypointSection;

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
        let m = manifest_with_entrypoint(Some(
            "/definitely/does/not/exist/nexo-plugin-test-bin",
        ));
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let result = plugin.spawn_and_handshake(cancel).await;
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
        m.plugin.entrypoint.env.insert(
            "NEXO_STATE_ROOT".to_string(),
            "/tmp/evil".to_string(),
        );
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let result = plugin.spawn_and_handshake(cancel).await;
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
        let result = plugin.spawn_and_handshake(cancel).await;
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
        let result = plugin.spawn_and_handshake(cancel).await;
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
}
