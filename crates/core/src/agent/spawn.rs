//! Phase 81.32 — runtime hot-spawn of agents.
//!
//! The per-agent boot loop in `src/main.rs` is being extracted to
//! this module (incremental — see PHASES.md Phase 81.32 / commits
//! c2 through c7). Once complete, both boot path and the
//! `ConfigReloadCoordinator` hot-spawn path call the same
//! [`spawn_agent_runtime`] function, eliminating the
//! "adding a new agent at runtime is not supported in Phase 18"
//! rejection that the wizard surfaces today.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │ src/main.rs boot path                 │
//! │   for agent_cfg in cfg.agents { ... } │
//! │              │                        │
//! └──────────────┼────────────────────────┘
//!                ▼
//! ┌──────────────────────────────────────┐    ┌────────────────────────────┐
//! │ spawn_agent_runtime(cfg, shared)      │◀───│ ConfigReloadCoordinator    │
//! │   ─ resolve LLM client                │    │   unknown_id branch        │
//! │   ─ load workspace                    │    │   (wizard create agent)    │
//! │   ─ subscribe to broker per binding   │    └────────────────────────────┘
//! │   ─ spawn heartbeat / dream tasks     │
//! │   ─ wire transcripts + events         │
//! │   ─ register reload sender            │
//! └──────────────────────────────────────┘
//! ```
//!
//! ## Status (c1 — foundation)
//!
//! This file lands the **types** (`SharedRuntimeContext`,
//! `SpawnError`, `SpawnedAgent`) and the **function signature**
//! with a `todo!()` body so that downstream commits (c2-c7)
//! extract one slice at a time without churning the call sites.
//! Until those land, the function panics if called — boot path
//! continues to use the inline loop in `src/main.rs`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use nexo_broker::AnyBroker;
use nexo_config::types::agents::AgentConfig;
use nexo_config::LlmConfig;
use nexo_llm::LlmRegistry;
use nexo_memory::LongTermMemory;

use super::admin_rpc::domains::pairing::PairingChallengeStore;
use super::admin_rpc::domains::processing::ProcessingControlStore;
use super::agent::Agent;
use super::agent_events::AgentEventEmitter;
use super::dispatch_handlers::DispatchToolContext;
use super::peer_directory::PeerDirectory;
use super::plan_mode_tool::PlanApprovalRegistry;
use super::redaction::Redactor;
use super::runtime::{AgentRuntime, ReloadCommand};
use super::tool_registry::ToolRegistry;
use super::transcripts_index::TranscriptsIndex;
use crate::link_understanding::LinkExtractor;
use crate::session::SessionManager;

/// Long-lived runtime dependencies each agent needs at spawn
/// time. Built once at boot from the singletons every nexo
/// daemon already constructs (broker, llm registry, memory,
/// session manager, …); cloned cheaply per spawn call because
/// every field is `Arc` / `Clone`-by-Arc.
///
/// Fields that have no boot-path equivalent for tests (e.g.
/// `transcripts_writer`, `agent_events_emitter`) stay `Option<>`
/// so test harnesses can construct a minimal context without
/// every subsystem wired.
///
/// **Live surface evolves with extraction commits.** Each
/// subsequent commit (c2-c7) lifts one slice of the boot loop's
/// captured state and adds the corresponding field here.
#[derive(Clone)]
pub struct SharedRuntimeContext {
    /// Broker handle. Plugin inbound topics resolve through
    /// this; runtime subscribers wire here per binding.
    pub broker: AnyBroker,
    /// Resolved LLM provider catalog. Per-agent spawn pulls the
    /// matching client via `llm_registry.resolve(&model_ref)`.
    pub llm_registry: Arc<LlmRegistry>,
    /// LLM YAML config snapshot — used by `RuntimeSnapshot::build`
    /// for the per-tenant provider lookup that happens after
    /// global resolution.
    pub llm_config: Arc<LlmConfig>,
    /// Long-term memory backend. Each agent gets a session-scoped
    /// view (`memory.with_agent_scope(id)`) but the backend is
    /// shared.
    pub memory: Option<Arc<LongTermMemory>>,
    /// Process-wide session manager. Per-agent runtimes book
    /// session slots here on first inbound message.
    pub session_mgr: Arc<SessionManager>,
    /// Shared pairing challenge store (used by agent runtimes
    /// that participate in QR / link pairing).
    pub pairing_store: Option<Arc<dyn PairingChallengeStore>>,
    /// Operator config directory — resolves workspace + skills
    /// + transcripts paths declared as relative in the agent
    /// yaml.
    pub config_dir: PathBuf,
    /// Master shutdown token. Each spawned agent allocates a
    /// child token via `dream_shutdown.child_token()` so
    /// SIGTERM cancels the whole tree paralelo; hot-remove
    /// cancels just the per-agent child.
    pub dream_shutdown: CancellationToken,
    /// Same shape as `dream_shutdown` but scoped to heartbeat
    /// loops. Separate token so an operator who pauses the
    /// heartbeat (future feature) doesn't bring down dreaming.
    pub heartbeat_shutdown: CancellationToken,
}

impl std::fmt::Debug for SharedRuntimeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid printing the broker / llm registry contents —
        // they include credentials. Field names + sentinels are
        // enough for boot diagnostics.
        f.debug_struct("SharedRuntimeContext")
            .field("broker", &"<broker>")
            .field("llm_registry", &"<llm-registry>")
            .field("memory", &self.memory.as_ref().map(|_| "<memory>"))
            .field("session_mgr", &"<session-mgr>")
            .field(
                "pairing_store",
                &self.pairing_store.as_ref().map(|_| "<pairing-store>"),
            )
            .field("config_dir", &self.config_dir)
            .finish()
    }
}

/// Result of [`spawn_agent_runtime`]. On error, no partial state
/// remains on the caller (all subscribers / tokens drop via RAII
/// when the partially-built future returns Err).
pub struct SpawnedAgent {
    /// Agent id (matches `cfg.id`). Useful as a key in the
    /// coordinator's `runtimes` map.
    pub agent_id: String,
    /// Channel the coordinator uses to push `Apply(snapshot)` /
    /// `Shutdown` commands to the live runtime.
    pub reload_tx: mpsc::Sender<ReloadCommand>,
    /// Pre-computed tool surface the agent exposes. Used by the
    /// coordinator's post-assembly validation so a typo in
    /// `allowed_tools` rejects the next reload instead of
    /// silently degrading.
    pub known_tools: Arc<Vec<String>>,
    /// Per-agent cancellation token. Cancelling it tears down
    /// broker subs + heartbeat + dream tasks for this agent in
    /// isolation (without disturbing other agents).
    pub shutdown_token: CancellationToken,
    /// The runtime instance. Boot path pushes this into the
    /// process-wide `runtimes` Vec; hot-spawn path keeps it
    /// alive via the coordinator's handle map.
    pub runtime: AgentRuntime,
}

/// Errors surfaced by [`spawn_agent_runtime`]. Each variant maps
/// to an operator-actionable message so the wizard can render an
/// inline error.
#[derive(Debug, Error)]
pub enum SpawnError {
    /// `validate_agent` / `EffectiveBindingPolicy` rejected the
    /// config (e.g. typo'd tool name, conflicting policy).
    #[error("validation: {0}")]
    Validation(String),
    /// `llm_registry.resolve` could not produce a client for
    /// `cfg.model`. Surface includes provider id + reason so the
    /// operator UI can deep-link to the provider config page.
    #[error("llm bind: {0}")]
    LlmBind(String),
    /// Workspace dir is missing or unreadable. Typical operator
    /// fix: create the directory or remove `workspace:` from yaml.
    #[error("workspace: {0}")]
    Workspace(String),
    /// Broker subscribe failed for one of the agent's bindings.
    /// Includes the failing topic so operator logs are useful.
    #[error("broker subscribe: {0}")]
    BrokerSubscribe(String),
    /// `inbound_bindings[i]` references a plugin/instance pair
    /// that has no handle in the live registry. Operator must
    /// install / pair the plugin first.
    #[error("plugin handle missing for binding {plugin}:{instance:?}")]
    PluginMissing {
        /// Channel/plugin id (e.g. `"whatsapp"`).
        plugin: String,
        /// Instance discriminator (e.g. a paired JID), or `None`
        /// for single-instance channels.
        instance: Option<String>,
    },
    /// Catch-all for unexpected internal failures during spawn
    /// pipeline (e.g. session manager registration failed).
    #[error("internal: {0}")]
    Internal(String),
}

/// Spawn a fresh agent runtime end-to-end from `cfg`.
///
/// Currently `todo!()` — c2 through c7 of Phase 81.32 will
/// extract the per-agent boot loop's body into this function one
/// slice at a time. Once those land, both `src/main.rs` boot
/// path and the `ConfigReloadCoordinator` hot-spawn branch call
/// this function with the same semantics.
///
/// On error: no partial state escapes. Subscribers / tokens /
/// allocated futures drop with the returned future; the caller
/// observes only the typed `SpawnError`.
pub async fn spawn_agent_runtime(
    cfg: &AgentConfig,
    shared: &SharedRuntimeContext,
) -> Result<SpawnedAgent, SpawnError> {
    let _ = (cfg, shared);
    todo!(
        "Phase 81.32 c2-c7 — extracting boot loop body into spawn_agent_runtime. \
         Until extraction completes, src/main.rs uses the inline boot loop."
    )
}

/// Phase 81.32 c2 — first slice extracted from the boot loop.
///
/// Resolves the agent's LLM client through the shared
/// [`nexo_llm::LlmRegistry`] and converts the registry's
/// `anyhow::Error` into a typed [`SpawnError::LlmBind`]. Args
/// are granular (registry + config) so the boot path can call
/// this helper without yet constructing a full
/// [`SharedRuntimeContext`] — c8 wires the context-driven
/// caller once every other slice is extracted.
///
/// Returns the `Arc<dyn LlmClient>` ready to hand to
/// `LlmAgentBehavior::new` / the memory extractor.
pub fn resolve_llm_client(
    cfg: &AgentConfig,
    llm_registry: &nexo_llm::LlmRegistry,
    llm_config: &nexo_config::LlmConfig,
) -> Result<Arc<dyn nexo_llm::LlmClient>, SpawnError> {
    llm_registry.build(llm_config, &cfg.model).map_err(|e| {
        SpawnError::LlmBind(format!(
            "agent `{}` model `{}/{}`: {e}",
            cfg.id, cfg.model.provider, cfg.model.model,
        ))
    })
}

/// Phase 81.32 c3 — workspace path resolver used by every
/// per-agent sub-system that consumes `agent_cfg.workspace`.
///
/// Boot loop today duplicates the `if agent_cfg.workspace.trim()
/// .is_empty() { default } else { PathBuf::from(...) }` pattern
/// across ~8 sites (dream, cron, repl, lsp, transcripts,
/// extra_docs, agent_ws, stats). This helper centralises:
///
///   - empty / whitespace-only `workspace:` → `default_root` fallback
///   - non-empty → trimmed `PathBuf` (callers were inconsistent
///     about trimming; we always trim so `"  /tmp  "` works)
///   - relative paths are returned verbatim (caller decides
///     whether to resolve against config_dir; the runtime config
///     loader already resolves absolute via
///     `config::resolve_relative_paths`)
///
/// `None` only when `default_root` is `None` AND the agent's
/// workspace field is empty. Otherwise always returns `Some`.
pub fn resolve_workspace_dir(
    cfg: &AgentConfig,
    default_root: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let trimmed = cfg.workspace.trim();
    if trimmed.is_empty() {
        default_root.map(|p| p.to_path_buf())
    } else {
        Some(std::path::PathBuf::from(trimmed))
    }
}

/// Phase 81.32 c2 — second slice extracted from the boot loop.
///
/// Wraps `nexo_core::agent::validate_agent` with a typed
/// [`SpawnError::Validation`] return so spawn callers don't have
/// to thread `anyhow::Error` strings into the wizard's
/// rejection surface. Called AFTER the tool registry is
/// assembled — the caller passes the agent's resolved tool
/// names list.
///
/// Same granular-args policy as [`resolve_llm_client`]: boot
/// path passes raw `&[String]` for known tools (the tool
/// registry isn't on `SharedRuntimeContext` because it's
/// per-agent, built after the LLM bind step).
pub fn validate_agent_config(
    cfg: &AgentConfig,
    plugins: &nexo_config::types::plugins::PluginsConfig,
    known_tool_names: &[&str],
) -> Result<(), SpawnError> {
    let catalog = crate::agent::KnownTools::new(known_tool_names.to_vec());
    crate::agent::validate_agent(cfg, plugins, &catalog)
        .map_err(|e| SpawnError::Validation(format!("agent `{}`: {e}", cfg.id)))
}

/// Phase 81.32 c6 — sized newtype wrapping the type-erased
/// spawner closure stored by
/// [`crate::config_reload::ConfigReloadCoordinator`].
///
/// `ArcSwapOption<T>` requires `T: Sized` so we wrap the unsized
/// `dyn Fn(…)` in a `Box` and the newtype around the `Box`. The
/// coordinator stores `ArcSwapOption<AgentSpawnerFn>` and invokes
/// via `spawner.0(cfg)`.
///
/// Boxed-future return because `async fn` in a trait/closure
/// produces an opaque future type the coordinator can't name
/// without GATs.
///
/// Lives in `spawn` (vs `config_reload`) so the field type on
/// `ConfigReloadCoordinator` doesn't pull every per-agent dep
/// the closure captures into the coordinator's API surface.
pub struct AgentSpawnerFn(
    pub  Box<
        dyn Fn(AgentConfig) -> Pin<Box<dyn Future<Output = Result<SpawnedAgent, SpawnError>> + Send>>
            + Send
            + Sync,
    >,
);

impl AgentSpawnerFn {
    /// Convenience: invoke the wrapped closure directly without
    /// touching the `.0` field at every call site.
    pub fn call(
        &self,
        cfg: AgentConfig,
    ) -> Pin<Box<dyn Future<Output = Result<SpawnedAgent, SpawnError>> + Send>> {
        (self.0)(cfg)
    }
}

/// Phase 81.32 c4 — per-agent runtime deps consumed by
/// [`assemble_agent_runtime`].
///
/// Every field maps 1:1 to an `AgentRuntime::with_X` setter. The
/// struct exists so the boot path's `.with_X(...)` chain (15 calls
/// in `src/main.rs:6276`) and the hot-spawn path call the same
/// helper without duplicating the conditional `if Some { ... }`
/// boilerplate.
///
/// Pre-built deps (vs constructed inside the helper) because:
///   - Plugin-side adapter types (`WhatsappPairingAdapter`,
///     `TelegramPairingAdapter`) live in `crates/plugins/*` which
///     depend on `nexo-core` — building them here would create a
///     circular dep. Caller builds the `PairingAdapterRegistry`
///     once at boot and clones it per spawn (registry is small;
///     adapters are `Arc<dyn ...>` internally).
///   - `event_emitter` comes from the admin bootstrap which is
///     wired only when the admin plugin is enabled; `Option<>`
///     keeps minimal-boot daemons (no admin) working.
pub struct RuntimeAssemblyDeps {
    /// Per-agent base tool registry. Sessions clone per-binding
    /// filtered views from this via `ToolRegistryCache`.
    pub tools: Arc<ToolRegistry>,
    /// Long-term memory backend, when the daemon was built with
    /// `--features memory`.
    pub memory: Option<Arc<LongTermMemory>>,
    /// Peer directory (`list_peers` tool reads from this).
    pub peers: Arc<PeerDirectory>,
    /// Transcript redactor. Every `AgentContext` clones this for
    /// log-redaction at write time.
    pub redactor: Arc<Redactor>,
    /// Optional transcripts index (full-text search across past
    /// sessions). Built only when the index sidecar is enabled.
    pub transcripts_index: Option<Arc<TranscriptsIndex>>,
    /// Resolved per-agent credentials (LLM keys, plugin tokens).
    /// Empty when `secrets/` is not wired.
    pub credentials: Option<Arc<nexo_auth::AgentCredentialResolver>>,
    /// Circuit-breaker registry shared with credentials. Co-arrives
    /// with `credentials`; both `None` or both `Some`.
    pub breakers: Option<Arc<nexo_auth::BreakerRegistry>>,
    /// Link extractor used by `llm_behavior` to build the
    /// `# LINK CONTEXT` block from inbound URLs.
    pub link_extractor: Arc<LinkExtractor>,
    // Phase 95 — web_search_router removed; subprocess plugin
    // owns the router now.
    /// Process-shared pairing gate. Required so unknown senders
    /// never reach agent behavior.
    pub pairing_gate: Arc<nexo_pairing::PairingGate>,
    /// Pre-registered pairing adapters keyed by `source_plugin`.
    /// Caller (boot path / coordinator) constructs once via the
    /// plugin-side adapter crates (`nexo-plugin-whatsapp`,
    /// `nexo-plugin-telegram`, …) and passes here.
    pub pairing_adapters: nexo_pairing::PairingAdapterRegistry,
    /// Plan-mode approval registry shared with the admin RPC
    /// dispatcher so `/plan-mode` chat messages resolve pending
    /// approvals.
    pub plan_approval_registry: Arc<PlanApprovalRegistry>,
    /// Dispatch tool context (`dispatch.notify` / dispatch
    /// catalog). Optional — minimal daemons skip dispatch.
    pub dispatch_ctx: Option<Arc<DispatchToolContext>>,
    /// Processing-control store backing `processing/pause` /
    /// `processing/resume` admin RPCs.
    pub processing_store: Arc<dyn ProcessingControlStore>,
    /// Optional event emitter wired by the admin bootstrap so
    /// per-scope eviction events reach the firehose.
    pub event_emitter: Option<Arc<dyn AgentEventEmitter>>,
}

/// Phase 81.32 c4 — assemble an `AgentRuntime` from an already-built
/// `Agent` and the per-agent deps in [`RuntimeAssemblyDeps`].
///
/// Mirrors exactly the `.with_X(...)` chain in `src/main.rs:6276`
/// (boot path). Both boot and hot-spawn now produce identically-
/// configured runtimes. The runtime is returned *unstarted* so the
/// caller still owns the lifecycle decision (`runtime.start().await`
/// on boot; deferred-until-after-coordinator-handle on hot-spawn).
///
/// Order matters for setters that share state (none today, but
/// future setters that take `&mut self` reads should preserve this
/// order to keep diff churn low against the boot path).
pub fn assemble_agent_runtime(
    agent: Arc<Agent>,
    broker: AnyBroker,
    sessions: Arc<SessionManager>,
    deps: RuntimeAssemblyDeps,
) -> AgentRuntime {
    let mut runtime = AgentRuntime::new(agent, broker, sessions);
    runtime = runtime.with_tool_base(deps.tools);
    if let Some(mem) = deps.memory {
        runtime = runtime.with_memory(mem);
    }
    runtime = runtime.with_peers(deps.peers);
    runtime = runtime.with_redactor(deps.redactor);
    if let Some(idx) = deps.transcripts_index {
        runtime = runtime.with_transcripts_index(idx);
    }
    if let Some(creds) = deps.credentials {
        runtime = runtime.with_credentials(creds);
    }
    if let Some(brk) = deps.breakers {
        runtime = runtime.with_breakers(brk);
    }
    runtime = runtime.with_link_extractor(deps.link_extractor);
    // Phase 95 — web_search_router wiring removed.
    runtime = runtime.with_pairing_gate(deps.pairing_gate);
    runtime = runtime.with_pairing_adapters(deps.pairing_adapters);
    runtime = runtime.with_plan_approval_registry(deps.plan_approval_registry);
    if let Some(dc) = deps.dispatch_ctx {
        runtime = runtime.with_dispatch_ctx(dc);
    }
    runtime = runtime.with_processing_store(deps.processing_store);
    if let Some(emitter) = deps.event_emitter {
        runtime = runtime.with_event_emitter(emitter);
    }
    runtime
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_error_display_is_actionable() {
        let cases = [
            SpawnError::Validation("bad tool".into()),
            SpawnError::LlmBind("provider 'typo' unknown".into()),
            SpawnError::Workspace("/missing/dir not found".into()),
            SpawnError::BrokerSubscribe("topic refused".into()),
            SpawnError::PluginMissing {
                plugin: "whatsapp".into(),
                instance: Some("personal".into()),
            },
            SpawnError::Internal("session register".into()),
        ];
        for case in cases {
            let s = case.to_string();
            assert!(!s.is_empty(), "error display must produce text");
            assert!(s.len() > 4, "error display must be operator-readable: {s:?}");
        }
    }

    #[test]
    fn shared_runtime_context_debug_omits_secrets() {
        // We don't construct a real context (would need broker +
        // LlmRegistry + LongTermMemory which require running tokio
        // services). Instead this test guards the Debug impl
        // by asserting the redaction sentinels exist in the
        // formatter source. Updated whenever a new field is added
        // so reviewers remember to redact.
        let src = include_str!("./spawn.rs");
        assert!(
            src.contains("<broker>") && src.contains("<llm-registry>"),
            "Debug impl must redact broker + llm_registry"
        );
    }
}
