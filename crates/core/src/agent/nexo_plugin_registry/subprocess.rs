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

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use nexo_broker::{topic::topic_matches, AnyBroker, BrokerHandle, Event};
use nexo_plugin_manifest::PluginManifest;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex, OnceCell};
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
    /// bridge per subscribed topic, supervisor (Phase 81.21).
    /// Joined on shutdown.
    tasks: Vec<JoinHandle<()>>,

    /// Child process handle. Phase 81.21 wraps in
    /// `Arc<Mutex<...>>` so the supervisor task can `try_wait()`
    /// every 500ms while `shutdown()` can still `take()` for
    /// reaping. Mutex contention is cheap (one try_wait poll per
    /// half-second vs single take on shutdown).
    child: Arc<Mutex<Option<Child>>>,

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
    ///
    /// `broker` is `Some` for the production lifecycle path
    /// (`init()` passes `ctx.broker.clone()`) so the topic bridge
    /// can subscribe outbound topics + forward `broker.publish`
    /// notifications. Tests that exercise the spawn / handshake
    /// shape only (without bridge wiring) pass `None` to skip the
    /// subscription work.
    async fn spawn_and_handshake(
        &self,
        ctx_shutdown: CancellationToken,
        broker: Option<AnyBroker>,
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
            // Phase 81.23 — pipe stderr into the daemon's tracing
            // subsystem instead of discarding. Child code that uses
            // `eprintln!` / `tracing` writing to stderr now becomes
            // operator-visible debug output filtered by `plugin_id`.
            .stderr(Stdio::piped())
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
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("child has no stderr"))?;

        // Phase 81.21 — wrap the Child in Arc<Mutex<Option<...>>>
        // immediately so the supervisor task (spawned after the
        // bridge wires up) can poll `try_wait` while shutdown can
        // still `take()` for reaping. Mutex contention is cheap:
        // one half-second poll vs single take on shutdown.
        let child_handle: Arc<Mutex<Option<Child>>> =
            Arc::new(Mutex::new(Some(child)));

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

        // Phase 81.23 — stderr reader task. Forwards each line
        // the child writes to stderr into the daemon's tracing
        // subsystem at `target = "plugin.stderr"` with the
        // plugin_id captured as a structured field. Operators can
        // then filter `RUST_LOG=plugin.stderr=info` to see
        // child output, or per-plugin via the field.
        // Spawned BEFORE the handshake send so any child boot-time
        // errors land in the operator's log.
        let stderr_cancel = cancel.clone();
        let stderr_plugin_id = self.cached_manifest.plugin.id.clone();
        // Phase 81.21.b — ring buffer of last N stderr lines.
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

        // Phase 81.14.b — broker bridge state.
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
                        // Phase 81.23 — non-JSON stdout lines are
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
        let returned_id = result
            .pointer("/manifest/plugin/id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("initialize reply missing manifest.plugin.id")
            })?;
        if returned_id != self.cached_manifest.plugin.id {
            cancel.cancel();
            kill_handle(&child_handle).await;
            anyhow::bail!(
                "manifest id mismatch: factory expected `{}`, child reported `{}`",
                self.cached_manifest.plugin.id,
                returned_id
            );
        }

        // Phase 81.14.b — wire the broker ↔ child topic bridge.
        // Derives subscribe / publish patterns from
        // `manifest.channels.register[].kind`. Plugins that don't
        // declare any channel kind get no bridge — the connection
        // stays open with just the writer + reader tasks but no
        // broker traffic crosses. Test paths pass `broker = None`
        // to skip subscriptions entirely.
        // Phase 81.21 — supervisor task. Polls `child.try_wait()`
        // every 500ms; on exit detection, publishes a
        // `plugin.lifecycle.<id>.crashed` event on the broker
        // (when one is wired) so operator hooks can observe the
        // crash, then cancels the plugin's tasks. Auto-respawn
        // with backoff is deferred to 81.21.b — this MVP just
        // surfaces the crash so the operator can decide.
        // Captures `broker.clone()` early since the variable is
        // moved into the bridge wiring loop below.
        let supervisor_broker = broker.clone();
        let supervisor_child = child_handle.clone();
        let supervisor_cancel = cancel.clone();
        let supervisor_plugin_id = self.cached_manifest.plugin.id.clone();
        let supervisor_stderr_tail = stderr_tail.clone();
        // Phase 81.21.b — `respawn = true` is parsed + validated
        // but not yet wired to the actual respawn loop. Surface a
        // one-shot reminder so operators know the manifest field
        // landed but the behavior they expected is deferred to
        // 81.21.b.b.
        let supervisor_respawn_requested =
            self.cached_manifest.plugin.supervisor.respawn;
        let supervisor_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = supervisor_cancel.cancelled() => return,
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
                        // job is done.
                        return;
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
                            "subprocess plugin exited unexpectedly"
                        );
                        // Phase 81.21.b — drain the stderr tail
                        // ring buffer into a fresh Vec for the
                        // crashed event payload. Up to
                        // `supervisor.stderr_tail_lines` recent
                        // lines (in chronological order, oldest
                        // first). Empty when the buffer never
                        // received anything OR the manifest set
                        // `stderr_tail_lines = 0`.
                        let stderr_tail_drained: Vec<String> = {
                            let mut buf = supervisor_stderr_tail.lock().await;
                            buf.drain(..).collect()
                        };
                        if supervisor_respawn_requested {
                            tracing::info!(
                                target: "plugin.supervisor",
                                plugin_id = %supervisor_plugin_id,
                                respawn_requested = true,
                                "respawn config not yet wired (Phase 81.21.b.b); operator must restart the daemon to recover"
                            );
                        }
                        if let Some(broker) = supervisor_broker.as_ref() {
                            let topic = format!(
                                "plugin.lifecycle.{}.crashed",
                                supervisor_plugin_id
                            );
                            let payload = json!({
                                "plugin_id": supervisor_plugin_id,
                                "exit_code": exit_code,
                                "stderr_tail": stderr_tail_drained,
                            });
                            let event =
                                Event::new(&topic, "plugin.supervisor", payload);
                            if let Err(e) = broker.publish(&topic, event).await {
                                tracing::warn!(
                                    target: "plugin.supervisor",
                                    plugin_id = %supervisor_plugin_id,
                                    error = %e,
                                    "broker publish of crashed event failed"
                                );
                            }
                        }
                        // Cascade-teardown: cancel the plugin's
                        // tasks (writer / readers / forwarders) so
                        // they don't run against a dead child.
                        supervisor_cancel.cancel();
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "plugin.supervisor",
                            plugin_id = %supervisor_plugin_id,
                            error = %e,
                            "subprocess plugin try_wait failed"
                        );
                        return;
                    }
                }
            }
        });

        let mut tasks =
            vec![writer_handle, reader_handle, stderr_handle, supervisor_handle];
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
            });
        }

        Ok(Inner {
            stdin_tx,
            pending,
            tasks,
            child: child_handle,
            cancel,
        })
    }
}

/// Phase 81.21 — kill the wrapped child if still present. Used by
/// every spawn_and_handshake error path to make sure a partial
/// boot doesn't leak the child process. Idempotent — `take()`
/// makes follow-up calls a no-op.
async fn kill_handle(h: &Arc<Mutex<Option<Child>>>) {
    let mut guard = h.lock().await;
    if let Some(mut c) = guard.take() {
        let _ = c.kill().await;
    }
}

/// Phase 81.14.b — captured by the stdout reader task to forward
/// validated `broker.publish` notifications to the broker.
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
            .spawn_and_handshake(ctx.shutdown.clone(), Some(ctx.broker.clone()))
            .await
            .map_err(|source| PluginInitError::Other {
                plugin_id: self.cached_manifest.plugin.id.clone(),
                source,
            })?;
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
        // 1s grace for the child to exit on its own; SIGKILL after.
        // The supervisor task (Phase 81.21) may have already taken
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
        let result = plugin.spawn_and_handshake(cancel, None).await;
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
        let result = plugin.spawn_and_handshake(cancel, None).await;
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
        let result = plugin.spawn_and_handshake(cancel, None).await;
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
        let result = plugin.spawn_and_handshake(cancel, None).await;
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

    // ─── Phase 81.14.b — broker bridge tests ────────────────────────

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
        let dir = std::env::temp_dir().join(dir_name);
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
        let path = write_bridge_mock_script(
            "nexo-bridge-test-subscribe",
            "test_plugin",
            None,
        );
        let m = manifest_with_channel(path.to_str().unwrap(), "slack");
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let broker = AnyBroker::Local(nexo_broker::LocalBroker::new());
        let res = plugin
            .spawn_and_handshake(cancel.clone(), Some(broker))
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
            .spawn_and_handshake(cancel.clone(), Some(broker))
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
            .spawn_and_handshake(cancel.clone(), Some(broker))
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
        let path = write_bridge_mock_script(
            "nexo-bridge-test-none",
            "test_plugin",
            None,
        );
        let m = manifest_with_channel(path.to_str().unwrap(), "slack");
        let plugin = SubprocessNexoPlugin::new(m);
        let cancel = CancellationToken::new();
        let res = plugin.spawn_and_handshake(cancel.clone(), None).await;
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

    /// Phase 81.23 — stderr is piped (not Stdio::null()) so the
    /// reader task can forward child stderr lines into daemon
    /// tracing. We assert this by writing a stderr-emitting mock
    /// that successfully completes the handshake — if stderr were
    /// `Stdio::null()`, the reader_handle on stderr couldn't be
    /// constructed (child has no stderr), and `take()` in
    /// `spawn_and_handshake` would error with "child has no stderr"
    /// before we ever get to a successful handshake.
    #[tokio::test]
    async fn stderr_is_piped_so_reader_can_construct() {
        // Mock that writes "boot diag" on stderr BEFORE replying
        // on stdout — proves both streams flow through the daemon
        // simultaneously. We can't capture daemon tracing output
        // from inside the test (no global subscriber configured),
        // so the assertion focuses on the structural invariant:
        // spawn_and_handshake must succeed when stderr is piped
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
        let res = plugin.spawn_and_handshake(cancel.clone(), None).await;
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

    /// Phase 81.21 — supervisor task detects child exit and
    /// publishes a `plugin.lifecycle.<id>.crashed` event. The mock
    /// also writes a few stderr lines so the test verifies
    /// 81.21.b's stderr-tail capture: those lines must show up in
    /// the event payload's `stderr_tail` field.
    #[tokio::test]
    async fn supervisor_publishes_crashed_event_on_child_exit() {
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
        let mut sub = broker
            .subscribe("plugin.lifecycle.test_plugin.crashed")
            .await
            .expect("subscribe to crash topic");
        let _inner = plugin
            .spawn_and_handshake(cancel.clone(), Some(broker))
            .await
            .expect("handshake");
        std::env::remove_var("NEXO_PLUGIN_INIT_TIMEOUT_MS");

        // Supervisor polls every 500ms — exit at 200ms post-handshake
        // is caught on the second tick. 2s timeout is plenty.
        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("crashed event arrives within 2s");
        let event = event.expect("subscription delivers Some");
        assert_eq!(event.topic, "plugin.lifecycle.test_plugin.crashed");
        assert_eq!(event.source, "plugin.supervisor");
        assert_eq!(
            event.payload.get("plugin_id").and_then(|v| v.as_str()),
            Some("test_plugin")
        );
        assert_eq!(
            event.payload.get("exit_code").and_then(|v| v.as_i64()),
            Some(7)
        );
        // Phase 81.21.b — stderr_tail must contain the 3 diag
        // lines the mock wrote before exiting. Order is
        // chronological (oldest first). The mock wrote the lines
        // BEFORE the 0.2s sleep, so the stderr reader has plenty
        // of time to populate the buffer before the supervisor's
        // 500ms poll catches the exit.
        let stderr_tail = event
            .payload
            .get("stderr_tail")
            .and_then(|v| v.as_array())
            .expect("stderr_tail field must be an array");
        let lines: Vec<&str> = stderr_tail
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            lines,
            vec![
                "diag line 1",
                "diag line 2",
                "fatal: simulated crash cause"
            ]
        );
        cancel.cancel();
    }

    /// Phase 81.21.b — manifest validation rejects a stderr tail
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
        let manifest: PluginManifest =
            toml::from_str(&toml_str).expect("manifest parses (cap is enforced at validate time, not parse)");
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
}
