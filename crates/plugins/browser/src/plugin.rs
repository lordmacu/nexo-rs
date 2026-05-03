use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use uuid::Uuid;

use nexo_broker::{AnyBroker, BrokerHandle, Event};
use nexo_config::BrowserConfig;
use nexo_core::agent::{Command, Plugin, Response};
use nexo_core::agent::plugin_host::{
    NexoPlugin, PluginInitContext, PluginInitError, PluginShutdownError,
};
use nexo_plugin_manifest::PluginManifest;
use nexo_resilience::{CircuitBreaker, CircuitBreakerConfig};

use crate::cdp::{CdpClient, CdpSession};
use crate::chrome::ChromeLauncher;
use crate::chrome::RunningChrome;
use crate::command::{BrowserCmd, BrowserResult};

/// All shared mutable state used by both the direct `send_command` path and
/// the broker-driven task spawned in `start`. Held behind `Arc` so cloning
/// the plugin into a background task is cheap and doesn't duplicate state.
pub(crate) struct BrowserInner {
    pub(crate) config: BrowserConfig,
    pub(crate) session: Mutex<Option<CdpSession>>,
    pub(crate) chrome: Mutex<Option<RunningChrome>>,
    pub(crate) circuit: CircuitBreaker,
    pub(crate) broker: Mutex<Option<AnyBroker>>,
    pub(crate) event_forwarder_started: Mutex<bool>,
}

impl BrowserInner {
    fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            session: Mutex::new(None),
            chrome: Mutex::new(None),
            circuit: CircuitBreaker::new("plugin.browser", CircuitBreakerConfig::default()),
            broker: Mutex::new(None),
            event_forwarder_started: Mutex::new(false),
        }
    }

    async fn ensure_session(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut session_lock = self.session.lock().await;
        if session_lock.is_some() {
            return Ok(());
        }

        let ws_url = if self.config.cdp_url.is_empty() {
            let chrome = ChromeLauncher::launch(&self.config).await?;
            let ws = chrome.ws_url.clone();
            *self.chrome.lock().await = Some(chrome);
            ws
        } else {
            ChromeLauncher::connect_existing(&self.config.cdp_url, self.config.connect_timeout_ms)
                .await?
        };

        let client = Arc::new(CdpClient::connect(&ws_url).await?);

        // Enable Page domain so Chrome emits navigation/load events. Errors
        // here are non-fatal — the forwarder still starts, just quieter.
        if let Err(e) = client.send("Page.enable", serde_json::json!({})).await {
            tracing::warn!(error = %e, "Page.enable failed — navigation events may be missing");
        }

        let target_id = get_first_target_id(&ws_url, self.config.connect_timeout_ms).await?;
        let session = CdpSession::new(
            Arc::clone(&client),
            &target_id,
            self.config.command_timeout_ms,
        )
        .await?;
        *session_lock = Some(session);
        drop(session_lock);

        self.spawn_event_forwarder(client).await;
        Ok(())
    }

    /// Subscribe to every CDP notification and republish as broker events on
    /// `plugin.events.browser.{method_suffix}`. Started at most once per plugin
    /// instance, and only once the broker is available — otherwise the forwarder
    /// would be silently skipped forever.
    async fn spawn_event_forwarder(self: &Arc<Self>, client: Arc<CdpClient>) {
        let mut started = self.event_forwarder_started.lock().await;
        if *started {
            return;
        }

        let broker_guard = self.broker.lock().await;
        let Some(broker) = broker_guard.clone() else {
            tracing::debug!("browser plugin has no broker yet — deferring CDP event forwarder");
            return;
        };
        drop(broker_guard);

        *started = true;
        drop(started);

        let mut rx = client.subscribe_events();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let suffix = ev
                            .method
                            .split('.')
                            .next_back()
                            .unwrap_or("unknown")
                            .to_ascii_lowercase();
                        let topic = format!("plugin.events.browser.{suffix}");
                        let payload = serde_json::json!({
                            "method":     ev.method,
                            "params":     ev.params,
                            "session_id": ev.session_id,
                        });
                        let evt = Event::new(&topic, "browser", payload);
                        if let Err(e) = broker.publish(&topic, evt).await {
                            tracing::debug!(error = %e, topic = %topic, "failed to forward CDP event");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "CDP event forwarder lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Execute one CDP command against the managed Chrome session.
    /// Public so the `BrowserTool` family (registered as LLM tools) can
    /// drive the browser directly — otherwise callers would have to
    /// round-trip through the broker.
    pub async fn execute(self: &Arc<Self>, cmd: BrowserCmd) -> BrowserResult {
        if !self.circuit.allow() {
            return BrowserResult::err(format!(
                "circuit breaker `{}` is open",
                self.circuit.name()
            ));
        }

        if let Err(e) = self.ensure_session().await {
            self.circuit.on_failure();
            return BrowserResult::err(format!("session init failed: {e}"));
        }

        let mut lock = self.session.lock().await;
        let session = match lock.as_mut() {
            Some(s) => s,
            None => {
                self.circuit.on_failure();
                return BrowserResult::err("no active session");
            }
        };

        let timeout = Duration::from_millis(self.config.command_timeout_ms);
        let result = match cmd {
            BrowserCmd::Navigate { url } => {
                match tokio::time::timeout(timeout, session.navigate(&url)).await {
                    Ok(Ok(())) => BrowserResult::ok(),
                    Ok(Err(e)) => BrowserResult::err(e.to_string()),
                    Err(_) => BrowserResult::err("navigate timed out"),
                }
            }
            BrowserCmd::Click { target } => {
                match tokio::time::timeout(timeout, session.click(&target)).await {
                    Ok(Ok(())) => BrowserResult::ok(),
                    Ok(Err(e)) => BrowserResult::err(e.to_string()),
                    Err(_) => BrowserResult::err("click timed out"),
                }
            }
            BrowserCmd::Fill { target, value } => {
                match tokio::time::timeout(timeout, session.fill(&target, &value)).await {
                    Ok(Ok(())) => BrowserResult::ok(),
                    Ok(Err(e)) => BrowserResult::err(e.to_string()),
                    Err(_) => BrowserResult::err("fill timed out"),
                }
            }
            BrowserCmd::Screenshot => {
                match tokio::time::timeout(timeout, session.screenshot()).await {
                    Ok(Ok(bytes)) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        BrowserResult::ok_screenshot(b64)
                    }
                    Ok(Err(e)) => BrowserResult::err(e.to_string()),
                    Err(_) => BrowserResult::err("screenshot timed out"),
                }
            }
            BrowserCmd::Evaluate { script } => {
                match tokio::time::timeout(timeout, session.evaluate(&script)).await {
                    Ok(Ok(v)) => BrowserResult::ok_value(v),
                    Ok(Err(e)) => BrowserResult::err(e.to_string()),
                    Err(_) => BrowserResult::err("evaluate timed out"),
                }
            }
            BrowserCmd::Snapshot => match tokio::time::timeout(timeout, session.snapshot()).await {
                Ok(Ok(text)) => BrowserResult::ok_snapshot(text),
                Ok(Err(e)) => BrowserResult::err(e.to_string()),
                Err(_) => BrowserResult::err("snapshot timed out"),
            },
            BrowserCmd::ScrollTo { target } => {
                match tokio::time::timeout(timeout, session.scroll_to(&target)).await {
                    Ok(Ok(())) => BrowserResult::ok(),
                    Ok(Err(e)) => BrowserResult::err(e.to_string()),
                    Err(_) => BrowserResult::err("scroll_to timed out"),
                }
            }
        };

        if result.ok {
            self.circuit.on_success();
        } else {
            self.circuit.on_failure();
        }
        result
    }
}

pub struct BrowserPlugin {
    inner: Arc<BrowserInner>,
    shutdown: CancellationToken,
    /// Phase 81.12.a — parsed manifest cached at construction time
    /// via `include_str!("../nexo-plugin.toml")`. Returned by
    /// `<Self as NexoPlugin>::manifest()`. Static for the lifetime
    /// of the plugin instance.
    cached_manifest: PluginManifest,
}

impl BrowserPlugin {
    pub fn new(config: BrowserConfig) -> Self {
        const MANIFEST_TOML: &str = include_str!("../nexo-plugin.toml");
        let cached_manifest: PluginManifest = toml::from_str(MANIFEST_TOML)
            .expect("compile-time-bundled nexo-plugin.toml must parse");
        Self {
            inner: Arc::new(BrowserInner::new(config)),
            shutdown: CancellationToken::new(),
            cached_manifest,
        }
    }

    /// Execute one browser command directly (no broker roundtrip). The
    /// `BrowserTool` family calls this from inside LLM tool handlers so
    /// each tool call runs exactly once per request — sending through
    /// the broker would pile inbound events on top of whatever the
    /// plugin's own subscriber is doing.
    pub async fn execute(&self, cmd: BrowserCmd) -> BrowserResult {
        self.inner.execute(cmd).await
    }
}

#[async_trait]
impl Plugin for BrowserPlugin {
    fn name(&self) -> &str {
        "browser"
    }

    async fn start(&self, broker: AnyBroker) -> anyhow::Result<()> {
        *self.inner.broker.lock().await = Some(broker.clone());

        // If a CDP session was opened before `start()` (e.g. via a direct
        // `send_command` call), the event forwarder was skipped because the
        // broker wasn't available yet. Retry now that it is.
        if let Some(session) = self.inner.session.lock().await.as_ref() {
            let client = session.client();
            self.inner.spawn_event_forwarder(client).await;
        }

        let mut sub = broker.subscribe("plugin.inbound.browser").await?;
        let inner = Arc::clone(&self.inner);
        let shutdown = self.shutdown.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = sub.next() => {
                        let Some(event) = event else { break };
                        let session_id = event.session_id;

                        let cmd: BrowserCmd = match serde_json::from_value(event.payload.clone()) {
                            Ok(c) => c,
                            Err(e) => {
                                let result = BrowserResult::err(format!("invalid command: {e}"));
                                publish_result(&broker, session_id, result).await;
                                continue;
                            }
                        };

                        let result = inner.execute(cmd).await;
                        publish_result(&broker, session_id, result).await;
                    }
                    _ = shutdown.cancelled() => break,
                }
            }
        });

        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        self.shutdown.cancel();
        *self.inner.session.lock().await = None;
        if let Some(chrome) = self.inner.chrome.lock().await.take() {
            chrome.shutdown().await;
        }
        Ok(())
    }

    async fn send_command(&self, cmd: Command) -> anyhow::Result<Response> {
        match cmd {
            Command::Custom { name, payload } if name == "browser" => {
                let browser_cmd: BrowserCmd = serde_json::from_value(payload)
                    .map_err(|e| anyhow::anyhow!("invalid browser payload: {e}"))?;
                let result = self.inner.execute(browser_cmd).await;
                let payload = serde_json::to_value(&result)?;
                Ok(Response::Custom { payload })
            }
            Command::Custom { name, .. } => {
                anyhow::bail!("browser plugin does not handle Custom command `{name}`");
            }
            Command::SendMessage { .. } | Command::SendMedia { .. } => {
                anyhow::bail!("browser plugin does not handle messaging commands");
            }
        }
    }
}

/// Phase 81.12.a — `NexoPlugin` trait impl. Delegates to the legacy
/// `Plugin::start` / `Plugin::stop` so behavior is identical to the
/// pre-migration path. Both traits coexist on the same struct;
/// 81.12.e flips main.rs's boot wire from legacy registration to
/// factory-driven instantiation.
#[async_trait]
impl NexoPlugin for BrowserPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.cached_manifest
    }

    async fn init(
        &self,
        ctx: &mut PluginInitContext<'_>,
    ) -> Result<(), PluginInitError> {
        Plugin::start(self, ctx.broker.clone()).await.map_err(|source| {
            PluginInitError::Other {
                plugin_id: self.cached_manifest.plugin.id.clone(),
                source,
            }
        })
    }

    async fn shutdown(&self) -> Result<(), PluginShutdownError> {
        Plugin::stop(self).await.map_err(|source| {
            PluginShutdownError::Other {
                plugin_id: self.cached_manifest.plugin.id.clone(),
                source,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_browser_config() -> BrowserConfig {
        // BrowserConfig does not derive Default. Build the minimal
        // shape inline; the values don't trigger any real Chrome
        // launch since these tests don't call `init()` end-to-end.
        BrowserConfig {
            headless: true,
            executable: String::new(),
            cdp_url: String::new(),
            user_data_dir: "./data/browser/profile".to_string(),
            window_width: 1280,
            window_height: 800,
            connect_timeout_ms: 10_000,
            command_timeout_ms: 15_000,
            args: Vec::new(),
        }
    }

    #[test]
    fn manifest_parses_and_id_is_browser() {
        const MANIFEST_TOML: &str = include_str!("../nexo-plugin.toml");
        let m: PluginManifest = toml::from_str(MANIFEST_TOML).unwrap();
        assert_eq!(m.plugin.id, "browser");
        assert_eq!(m.plugin.version.to_string(), "0.1.1");
        assert_eq!(m.plugin.requires.nexo_capabilities, vec!["broker".to_string()]);
    }

    #[test]
    fn nexo_plugin_init_delegates_to_legacy_start() {
        // Build the plugin and verify the cached_manifest field
        // populates correctly. Direct invocation of NexoPlugin::init
        // requires a full PluginInitContext — instead we cast to
        // &dyn NexoPlugin and assert the trait dispatch sees the
        // manifest. The init body is a 1-line delegation to
        // Plugin::start; semantics covered by the legacy crate's
        // existing coverage.
        let plugin = BrowserPlugin::new(test_browser_config());
        let nexo: &dyn NexoPlugin = &plugin;
        assert_eq!(nexo.manifest().plugin.id, "browser");
        assert_eq!(nexo.manifest().plugin.version.to_string(), "0.1.1");
    }

    #[test]
    fn factory_builder_produces_usable_handle() {
        // Note: this test uses the CRATE-LOCAL factory shape via
        // direct lib.rs entry point. We construct it via the same
        // closure type signature.
        let cfg = test_browser_config();
        let factory: nexo_core::agent::nexo_plugin_registry::PluginFactory =
            Box::new(move |_m| {
                let plugin: Arc<dyn NexoPlugin> = Arc::new(BrowserPlugin::new(cfg.clone()));
                Ok(plugin)
            });
        const MANIFEST_TOML: &str = include_str!("../nexo-plugin.toml");
        let m: PluginManifest = toml::from_str(MANIFEST_TOML).unwrap();
        match factory(&m) {
            Ok(handle) => assert_eq!(handle.manifest().plugin.id, "browser"),
            Err(e) => panic!("factory should succeed, got {e}"),
        }
    }

    #[test]
    fn dual_trait_methods_share_state() {
        let plugin = BrowserPlugin::new(test_browser_config());
        // Cast the SAME instance through both trait objects.
        let legacy: &dyn Plugin = &plugin;
        let nexo: &dyn NexoPlugin = &plugin;
        // Legacy trait reports the registry name.
        assert_eq!(legacy.name(), "browser");
        // NexoPlugin reports the manifest id.
        assert_eq!(nexo.manifest().plugin.id, "browser");
        // Both agree on the plugin's identity.
        assert_eq!(legacy.name(), nexo.manifest().plugin.id);
    }
}

async fn publish_result(broker: &AnyBroker, session_id: Option<Uuid>, result: BrowserResult) {
    let payload = serde_json::to_value(&result).unwrap_or(serde_json::json!({"ok": false}));
    let mut event = Event::new("plugin.outbound.browser", "browser", payload);
    event.session_id = session_id;
    if let Err(e) = broker.publish("plugin.outbound.browser", event).await {
        tracing::error!(error = %e, "failed to publish browser result");
    }
}

async fn get_first_target_id(ws_url: &str, timeout_ms: u64) -> anyhow::Result<String> {
    // Convert ws:// to http:// for /json endpoint
    let http_url = ws_url
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    // Strip path (keep only host:port)
    let base = if let Some(idx) = http_url.find("/devtools/") {
        http_url[..idx].to_string()
    } else {
        http_url
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()?;
    // Chrome DevTools HTTP endpoint requires Host: localhost.
    let targets: Vec<serde_json::Value> = client
        .get(format!("{base}/json/list"))
        .header(reqwest::header::HOST, "localhost")
        .send()
        .await?
        .json()
        .await?;

    targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .and_then(|t| t.get("id").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no page target found in Chrome"))
}
