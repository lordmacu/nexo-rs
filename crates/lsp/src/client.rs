//! LSP JSON-RPC client over a child process's stdio.
//!
//! Spawns the binary, framed-reads/writes via [`crate::codec::LspCodec`],
//! routes responses to per-request `oneshot` channels, caches
//! `publishDiagnostics` notifications.
//!
//! Reference (PRIMARY):
//!   * `claude-code-leak/src/services/lsp/LSPClient.ts:51-200` —
//!     spawn + spawn-event wait, `vscode-jsonrpc` connection, lazy
//!     handler queue. We mirror the spawn-await behaviour because
//!     `tokio::process::Command::spawn()` returns immediately and
//!     ENOENT (binary missing) shows up as a process-exit event,
//!     not as a spawn error.
//!   * `claude-code-leak/src/services/lsp/LSPServerInstance.ts:165-200`
//!     — `InitializeParams` shape: `processId`, `workspaceFolders`,
//!     `rootUri` (deprecated but tsserver still needs it),
//!     `capabilities.workspace.configuration: false` so servers do
//!     not back-ask us for config.
//!
//! Reference (secondary):
//!   * `research/src/agents/pi-bundle-lsp-runtime.ts:88-157` —
//!     pending-request `Map<id, {resolve, reject}>` pattern. We use
//!     `tokio::sync::oneshot` per pending entry instead.

use crate::codec::LspCodec;
use crate::error::LspError;
use crate::types::ResolvedCapabilities;
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio_util::codec::{FramedRead, FramedWrite};

/// Outgoing message — either a request awaiting a response, or a
/// notification (fire-and-forget).
enum Outgoing {
    Request {
        id: i64,
        body: serde_json::Value,
        responder: oneshot::Sender<Result<serde_json::Value, LspError>>,
    },
    Notification {
        body: serde_json::Value,
    },
}

/// Snapshot of the latest diagnostics for one URI plus the
/// publish timestamp. Sessions consult `published_at` to decide
/// whether to wait for fresher data after `didOpen`.
#[derive(Clone)]
pub struct FileDiagnostics {
    pub items: Vec<lsp_types::Diagnostic>,
    pub published_at: Instant,
}

/// Read-only handle to the diagnostics cache. The session reaches
/// in to read; the I/O loop owns the write side.
#[derive(Default)]
pub struct DiagnosticsCache {
    inner: RwLock<HashMap<url::Url, FileDiagnostics>>,
}

impl DiagnosticsCache {
    pub async fn get(&self, uri: &url::Url) -> Option<FileDiagnostics> {
        self.inner.read().await.get(uri).cloned()
    }

    pub async fn forget(&self, uri: &url::Url) {
        self.inner.write().await.remove(uri);
    }

    async fn put(&self, uri: url::Url, diag: FileDiagnostics) {
        self.inner.write().await.insert(uri, diag);
    }
}

/// LSP client wrapper around one child process. The `request_tx`
/// + reader-task + writer-task topology means callers never block
/// on each other; multiple concurrent `request<R>()` calls are
/// safe.
pub struct LspClient {
    request_tx: mpsc::UnboundedSender<Outgoing>,
    pub diagnostics: Arc<DiagnosticsCache>,
    next_id: AtomicI64,
    child: Mutex<Option<Child>>,
    /// 30 s default; tests override.
    request_timeout: Duration,
}

impl LspClient {
    /// Spawn the binary as a child process and start the framed
    /// I/O loops. The child inherits the parent's environment plus
    /// any overrides passed in. `kill_on_drop(true)` is set so a
    /// dropped client never leaks a child.
    pub async fn spawn(
        binary: &Path,
        workspace_root: &Path,
        env: &HashMap<String, String>,
        request_timeout: Duration,
    ) -> Result<Self, LspError> {
        let mut cmd = Command::new(binary);
        cmd.current_dir(workspace_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| LspError::Wire(format!("spawn `{}` failed: {e}", binary.display())))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Wire("child stdin not captured".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Wire("child stdout not captured".into()))?;

        // Drain stderr to a tracing target so server logs are not
        // lost. Best-effort; the task ends when the pipe closes.
        if let Some(stderr) = child.stderr.take() {
            let binary_name = binary
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("lsp")
                .to_string();
            tokio::spawn(drain_stderr(stderr, binary_name));
        }

        let diagnostics = Arc::new(DiagnosticsCache::default());
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<serde_json::Value, LspError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (request_tx, request_rx) = mpsc::unbounded_channel::<Outgoing>();

        // Writer task: pulls from the channel, encodes, writes.
        let mut framed_write = FramedWrite::new(BufWriter::new(stdin), LspCodec);
        let pending_for_writer = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut rx = request_rx;
            while let Some(out) = rx.recv().await {
                match out {
                    Outgoing::Request {
                        id,
                        body,
                        responder,
                    } => {
                        // Park the responder before writing so the
                        // reader cannot deliver a response before the
                        // pending entry exists.
                        pending_for_writer.lock().await.insert(id, responder);
                        if let Err(e) = framed_write.send(body).await {
                            // Pull the responder back and signal.
                            if let Some(resp) = pending_for_writer.lock().await.remove(&id) {
                                let _ =
                                    resp.send(Err(LspError::Wire(format!("write failed: {e}"))));
                            }
                        }
                    }
                    Outgoing::Notification { body } => {
                        if let Err(e) = framed_write.send(body).await {
                            tracing::warn!(
                                target: "lsp::client",
                                error = %e,
                                "[lsp] notification write failed"
                            );
                        }
                    }
                }
            }
            // Channel closed → flush + drop.
            let _ = framed_write.flush().await;
        });

        // Reader task: framed-decodes from stdout, dispatches.
        let framed_read = FramedRead::with_capacity(BufReader::new(stdout), LspCodec, 8 * 1024);
        let pending_for_reader = Arc::clone(&pending);
        let diagnostics_for_reader = Arc::clone(&diagnostics);
        tokio::spawn(reader_loop(
            framed_read,
            pending_for_reader,
            diagnostics_for_reader,
        ));

        Ok(Self {
            request_tx,
            diagnostics,
            next_id: AtomicI64::new(1),
            child: Mutex::new(Some(child)),
            request_timeout,
        })
    }

    /// Drive an outgoing JSON-RPC request to completion. Times out
    /// after `request_timeout` — on timeout we fire `$/cancelRequest`
    /// to the server (idempotent fire-and-forget) so it can free
    /// any in-flight work; the session stays alive.
    pub async fn request_raw(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LspError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel();
        self.request_tx
            .send(Outgoing::Request {
                id,
                body,
                responder: tx,
            })
            .map_err(|_| LspError::Wire("client writer task gone".into()))?;
        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(LspError::Wire(
                "responder dropped before the response arrived".into(),
            )),
            Err(_) => {
                // Best-effort cancel — server may already be done.
                let _ = self.notify_raw(
                    "$/cancelRequest",
                    serde_json::json!({ "id": id }),
                );
                Err(LspError::RequestTimeout {
                    method: method.to_string(),
                    timeout_secs: self.request_timeout.as_secs(),
                })
            }
        }
    }

    /// Send a JSON-RPC notification. Returns immediately; failures
    /// surface as `tracing::warn!` from the writer task.
    pub fn notify_raw(&self, method: &str, params: serde_json::Value) -> Result<(), LspError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.request_tx
            .send(Outgoing::Notification { body })
            .map_err(|_| LspError::Wire("client writer task gone".into()))?;
        Ok(())
    }

    /// Run the LSP `initialize` handshake. Sends `initialize`,
    /// awaits the response, sends `initialized` notification, then
    /// returns a normalised [`ResolvedCapabilities`].
    pub async fn initialize(
        &self,
        workspace_root: &Path,
    ) -> Result<ResolvedCapabilities, LspError> {
        let workspace_uri = url::Url::from_directory_path(workspace_root)
            .map_err(|_| LspError::Wire(format!(
                "workspace_root `{}` is not a valid URI base",
                workspace_root.display()
            )))?
            .to_string();
        // Lift from `claude-code-leak/src/services/lsp/LSPServerInstance.ts:165-200`
        // — keep both the deprecated rootUri/rootPath fields and
        // the modern workspaceFolders field; servers vary in which
        // they actually use.
        let init_params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": workspace_uri,
            "rootPath": workspace_root.display().to_string(),
            "workspaceFolders": [
                {
                    "uri": workspace_uri,
                    "name": workspace_root
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("workspace"),
                }
            ],
            "initializationOptions": {},
            "capabilities": {
                "workspace": {
                    // Don't claim configuration we can't serve.
                    "configuration": false,
                    "workspaceFolders": false,
                },
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false },
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "definition": { "linkSupport": true },
                    "references": {},
                    "publishDiagnostics": {},
                }
            }
        });
        let resp = self.request_raw("initialize", init_params).await?;
        // Send `initialized` notification per LSP spec.
        self.notify_raw("initialized", serde_json::json!({}))?;
        Ok(parse_capabilities(&resp))
    }

    /// Send LSP `shutdown` request, wait briefly, then send `exit`
    /// notification and reap the child. Best-effort: timeouts +
    /// errors are absorbed because shutdown is on the cleanup path.
    pub async fn shutdown(&self, grace: Duration) {
        // Send shutdown request — server may take a moment to flush.
        let _ = tokio::time::timeout(
            grace,
            self.request_raw("shutdown", serde_json::Value::Null),
        )
        .await;
        // Send exit notification.
        let _ = self.notify_raw("exit", serde_json::Value::Null);

        // Reap child. `kill_on_drop(true)` covers the rest if grace
        // expires.
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let kill = async {
                let _ = child.kill().await;
                let _ = child.wait().await;
            };
            let _ = tokio::time::timeout(grace, kill).await;
        }
    }
}

async fn drain_stderr(stderr: tokio::process::ChildStderr, binary_name: String) {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(stderr).lines();
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                tracing::debug!(
                    target: "lsp::server_stderr",
                    binary = %binary_name,
                    "{}",
                    line
                );
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    target: "lsp::server_stderr",
                    binary = %binary_name,
                    error = %e,
                    "stderr read failed"
                );
                break;
            }
        }
    }
}

async fn reader_loop<R>(
    mut framed: FramedRead<R, LspCodec>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<serde_json::Value, LspError>>>>>,
    diagnostics: Arc<DiagnosticsCache>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    while let Some(item) = framed.next().await {
        let value = match item {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "lsp::client",
                    error = %e,
                    "[lsp] framed-read error — bailing out of reader loop"
                );
                break;
            }
        };

        // Three shapes:
        //   * Response: has `id` (number or string) and `result`/`error`.
        //   * Notification: has `method` and no `id`.
        //   * Server-to-client request: has `method` and an `id`.
        //     We only ack `workspace/configuration` (returning
        //     `null`s) per the leak's behaviour — anything else
        //     is logged.
        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            if value.get("method").is_some() {
                // Server-to-client request — minimal ack.
                handle_server_request(&value, &diagnostics).await;
            } else if let Some(responder) =
                pending.lock().await.remove(&id)
            {
                if let Some(error) = value.get("error") {
                    let _ = responder
                        .send(Err(LspError::Wire(format!("{error}"))));
                } else {
                    let result = value
                        .get("result")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let _ = responder.send(Ok(result));
                }
            } else {
                tracing::debug!(
                    target: "lsp::client",
                    id,
                    "[lsp] response for unknown id (already cancelled / timed out)"
                );
            }
        } else if let Some(method) = value.get("method").and_then(|v| v.as_str()) {
            handle_notification(method, &value, &diagnostics).await;
        }
    }

    // Reader exited → cancel any pending requests with a wire
    // error so callers don't hang forever.
    let mut guard = pending.lock().await;
    for (_id, responder) in guard.drain() {
        let _ = responder.send(Err(LspError::Wire("reader closed".into())));
    }
}

async fn handle_notification(
    method: &str,
    value: &serde_json::Value,
    diagnostics: &Arc<DiagnosticsCache>,
) {
    if method == "textDocument/publishDiagnostics" {
        let params = match value.get("params") {
            Some(p) => p,
            None => return,
        };
        let uri = match params
            .get("uri")
            .and_then(|v| v.as_str())
            .and_then(|s| url::Url::parse(s).ok())
        {
            Some(u) => u,
            None => return,
        };
        let items: Vec<lsp_types::Diagnostic> = serde_json::from_value(
            params.get("diagnostics").cloned().unwrap_or_default(),
        )
        .unwrap_or_default();
        diagnostics
            .put(
                uri,
                FileDiagnostics {
                    items,
                    published_at: Instant::now(),
                },
            )
            .await;
    }
}

async fn handle_server_request(
    _value: &serde_json::Value,
    _diagnostics: &Arc<DiagnosticsCache>,
) {
    // Server-to-client requests we don't handle — out-of-scope for
    // the MVP. tsserver's `workspace/configuration` is not gated to
    // an explicit ack here because we declared
    // `capabilities.workspace.configuration = false` in
    // `initialize`, which spec-compliant servers respect.
    // If a non-conforming server still asks, the leak's pattern
    // returns an array of nulls (`LSPServerManager.ts:124-135`);
    // we currently log + ignore. Follow-up when we see real
    // breakage.
}

/// Convert raw `initialize` response into our normalised flag set.
/// Tolerant of missing fields.
fn parse_capabilities(resp: &serde_json::Value) -> ResolvedCapabilities {
    let caps = match resp.get("capabilities") {
        Some(c) => c,
        None => return ResolvedCapabilities::default(),
    };

    let bool_or_object_present = |field: &str| -> bool {
        match caps.get(field) {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::Object(_)) => true,
            _ => false,
        }
    };

    ResolvedCapabilities {
        definition: bool_or_object_present("definitionProvider"),
        references: bool_or_object_present("referencesProvider"),
        hover: bool_or_object_present("hoverProvider"),
        workspace_symbol: bool_or_object_present("workspaceSymbolProvider"),
        // Per LSP spec every server can push diagnostics regardless
        // of `initialize` signal. Set true unless the server
        // explicitly declines.
        publish_diagnostics: !matches!(
            caps.get("textDocumentSync"),
            Some(serde_json::Value::Bool(false))
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_capabilities_extracts_bool_providers() {
        let resp = json!({
            "capabilities": {
                "definitionProvider": true,
                "hoverProvider": false,
                "referencesProvider": {},
                "workspaceSymbolProvider": { "workDoneProgress": false },
            }
        });
        let caps = parse_capabilities(&resp);
        assert!(caps.definition);
        assert!(!caps.hover);
        assert!(caps.references);
        assert!(caps.workspace_symbol);
        // textDocumentSync defaults to true in our parser.
        assert!(caps.publish_diagnostics);
    }

    #[test]
    fn parse_capabilities_missing_field_defaults_to_false() {
        let resp = json!({ "capabilities": {} });
        let caps = parse_capabilities(&resp);
        assert!(!caps.definition);
        assert!(!caps.hover);
        assert!(!caps.references);
        assert!(!caps.workspace_symbol);
        assert!(caps.publish_diagnostics);
    }

    #[test]
    fn parse_capabilities_no_field_at_all() {
        let resp = json!({});
        let caps = parse_capabilities(&resp);
        assert_eq!(caps, ResolvedCapabilities::default());
    }

    #[tokio::test]
    async fn diagnostics_cache_get_returns_inserted_value() {
        let cache = DiagnosticsCache::default();
        let uri = url::Url::parse("file:///x/y.rs").unwrap();
        let diag = FileDiagnostics {
            items: vec![],
            published_at: Instant::now(),
        };
        cache.put(uri.clone(), diag.clone()).await;
        let got = cache.get(&uri).await;
        assert!(got.is_some());
    }

    #[tokio::test]
    async fn diagnostics_cache_forget_removes_entry() {
        let cache = DiagnosticsCache::default();
        let uri = url::Url::parse("file:///x/y.rs").unwrap();
        cache
            .put(
                uri.clone(),
                FileDiagnostics {
                    items: vec![],
                    published_at: Instant::now(),
                },
            )
            .await;
        cache.forget(&uri).await;
        assert!(cache.get(&uri).await.is_none());
    }
}

#[cfg(test)]
mod io_loop_tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    /// In-memory mock LSP server: an end-to-end pair where the
    /// "server" side is a tokio task that responds to JSON-RPC
    /// frames synthesised over `tokio::io::duplex`. Avoids spawning
    /// a real binary in unit tests.
    struct MockServer {
        // Server-side framed read/write of the client's stdin/stdout.
        client_to_server_read: FramedRead<
            BufReader<tokio::io::DuplexStream>,
            LspCodec,
        >,
        server_to_client_write: FramedWrite<
            BufWriter<tokio::io::DuplexStream>,
            LspCodec,
        >,
    }

    /// Build a `LspClient` whose stdin/stdout are connected to a
    /// `MockServer` via `tokio::io::duplex`. We bypass `spawn` and
    /// wire the channels manually.
    async fn build_client_and_mock(request_timeout: Duration) -> (LspClient, MockServer) {
        let (client_stdin_w, server_read) = tokio::io::duplex(64 * 1024);
        let (server_write, client_stdout_r) = tokio::io::duplex(64 * 1024);

        let diagnostics = Arc::new(DiagnosticsCache::default());
        let pending: Arc<
            Mutex<HashMap<i64, oneshot::Sender<Result<serde_json::Value, LspError>>>>,
        > = Arc::new(Mutex::new(HashMap::new()));
        let (request_tx, request_rx) = mpsc::unbounded_channel::<Outgoing>();

        // Writer side.
        let mut framed_write =
            FramedWrite::new(BufWriter::new(client_stdin_w), LspCodec);
        let pending_for_writer = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut rx = request_rx;
            while let Some(out) = rx.recv().await {
                match out {
                    Outgoing::Request {
                        id,
                        body,
                        responder,
                    } => {
                        pending_for_writer.lock().await.insert(id, responder);
                        if let Err(e) = framed_write.send(body).await {
                            if let Some(resp) =
                                pending_for_writer.lock().await.remove(&id)
                            {
                                let _ = resp.send(Err(LspError::Wire(format!(
                                    "write failed: {e}"
                                ))));
                            }
                        }
                    }
                    Outgoing::Notification { body } => {
                        let _ = framed_write.send(body).await;
                    }
                }
            }
        });

        // Reader side.
        let framed_read =
            FramedRead::with_capacity(BufReader::new(client_stdout_r), LspCodec, 8 * 1024);
        let pending_for_reader = Arc::clone(&pending);
        let diagnostics_for_reader = Arc::clone(&diagnostics);
        tokio::spawn(reader_loop(
            framed_read,
            pending_for_reader,
            diagnostics_for_reader,
        ));

        let client = LspClient {
            request_tx,
            diagnostics,
            next_id: AtomicI64::new(1),
            child: Mutex::new(None),
            request_timeout,
        };

        let mock = MockServer {
            client_to_server_read: FramedRead::new(BufReader::new(server_read), LspCodec),
            server_to_client_write: FramedWrite::new(BufWriter::new(server_write), LspCodec),
        };

        (client, mock)
    }

    #[tokio::test]
    async fn request_response_routes_to_correct_oneshot() {
        let (client, mut mock) = build_client_and_mock(Duration::from_secs(2)).await;

        // Issue request from client.
        let client_arc = Arc::new(client);
        let c2 = Arc::clone(&client_arc);
        let req = tokio::spawn(async move {
            c2.request_raw("textDocument/hover", json!({})).await
        });

        // Mock server reads the request, replies.
        let req_msg = mock.client_to_server_read.next().await.unwrap().unwrap();
        let id = req_msg["id"].as_i64().unwrap();
        let resp = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "contents": "hello" }
        });
        mock.server_to_client_write.send(resp).await.unwrap();
        mock.server_to_client_write.flush().await.unwrap();

        let got = req.await.unwrap().unwrap();
        assert_eq!(got["contents"], "hello");
    }

    #[tokio::test]
    async fn publish_diagnostics_notification_caches() {
        let (client, mut mock) = build_client_and_mock(Duration::from_secs(2)).await;
        let cache = Arc::clone(&client.diagnostics);

        let uri = "file:///tmp/x.rs";
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [
                    {
                        "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5} },
                        "severity": 1,
                        "message": "test error"
                    }
                ]
            }
        });
        mock.server_to_client_write.send(notif).await.unwrap();
        mock.server_to_client_write.flush().await.unwrap();

        // Give the reader task a chance to pump.
        tokio::task::yield_now().await;
        for _ in 0..20 {
            if cache
                .get(&url::Url::parse(uri).unwrap())
                .await
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let snap = cache
            .get(&url::Url::parse(uri).unwrap())
            .await
            .expect("publishDiagnostics should populate cache");
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].message, "test error");

    }

    #[tokio::test]
    async fn timeout_returns_request_timeout_and_session_alive() {
        let (client, _mock) = build_client_and_mock(Duration::from_millis(50)).await;
        // We never let the mock server respond.
        let err = client
            .request_raw("textDocument/hover", json!({}))
            .await
            .unwrap_err();
        match err {
            LspError::RequestTimeout { method, .. } => {
                assert_eq!(method, "textDocument/hover");
            }
            other => panic!("expected RequestTimeout, got {other:?}"),
        }
        // Issuing a second request should still work — session is
        // alive.
        let err2 = client
            .request_raw("textDocument/hover", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err2, LspError::RequestTimeout { .. }));
    }
}
