//! Phase 79.M — conformance suite for `EXPOSABLE_TOOLS`.
//!
//! For every catalog entry we verify:
//! 1. Catalog wiring is consistent — `BootKind::Always` entries with
//!    a missing handle return `SkippedInfraMissing` with a non-empty
//!    label. Hard-denied entries return `SkippedDenied`.
//! 2. When the required handle is provided, the helper returns
//!    `Registered(def, _)` and `def.name == entry.name`.
//! 3. The registered tool's `ToolDef` carries a JSON-object schema —
//!    a basic shape contract MCP clients depend on.
//!
//! These tests do not spawn an MCP server; they drive the boot
//! dispatcher directly. Wire-protocol coverage lives in
//! `crates/mcp/tests/*` (Phase 76.12).

use std::sync::Arc;

use async_trait::async_trait;
use nexo_broker::AnyBroker;
use nexo_config::types::mcp_exposable::{
    lookup_exposable, BootKind, ExposableToolEntry, EXPOSABLE_TOOLS,
};
use nexo_core::agent::context::AgentContext;
use nexo_core::agent::mcp_server_bridge::{boot_exposable, BootResult, McpServerBootContext};
use nexo_core::config_changes_store::{ConfigChangeRow, ConfigChangesError, ConfigChangesStore};
use nexo_core::cron_schedule::{CronEntry, CronStore, CronStoreError};
use nexo_core::session::SessionManager;

fn fixture_agent_ctx() -> Arc<AgentContext> {
    use nexo_config::types::agents::{
        AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
        OutboundAllowlistConfig, WorkspaceGitConfig,
    };
    let cfg = AgentConfig {
        id: "exposable-test".into(),
        model: ModelConfig {
            provider: "x".into(),
            model: "y".into(),
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
    Arc::new(AgentContext::new(
        "exposable-test",
        Arc::new(cfg),
        AnyBroker::local(),
        Arc::new(SessionManager::new(std::time::Duration::from_secs(60), 8)),
    ))
}

struct StubCron;
#[async_trait]
impl CronStore for StubCron {
    async fn insert(&self, _e: &CronEntry) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn list_by_binding(&self, _binding_id: &str) -> Result<Vec<CronEntry>, CronStoreError> {
        Ok(Vec::new())
    }
    async fn count_by_binding(&self, _binding_id: &str) -> Result<usize, CronStoreError> {
        Ok(0)
    }
    async fn delete(&self, _id: &str) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn due_at(&self, _now_unix: i64) -> Result<Vec<CronEntry>, CronStoreError> {
        Ok(Vec::new())
    }
    async fn set_paused(&self, _id: &str, _paused: bool) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn get(&self, _id: &str) -> Result<CronEntry, CronStoreError> {
        Err(CronStoreError::NotFound("stub".into()))
    }
    async fn advance_after_fire(
        &self,
        _id: &str,
        _new_next_fire_at: i64,
        _last_fired_at: i64,
    ) -> Result<(), CronStoreError> {
        Ok(())
    }
    async fn schedule_one_shot_retry(
        &self,
        _id: &str,
        _retry_next_fire_at: i64,
        _last_fired_at: i64,
    ) -> Result<u32, CronStoreError> {
        Ok(1)
    }
    async fn sweep_missed_entries(
        &self,
        _now_unix: i64,
        _skew_ms: i64,
    ) -> Result<usize, CronStoreError> {
        Ok(0)
    }
    async fn sweep_expired_recurring(
        &self,
        _now_unix: i64,
        _max_age_ms: i64,
    ) -> Result<usize, CronStoreError> {
        Ok(0)
    }
}

struct StubConfigChanges;
#[async_trait]
impl ConfigChangesStore for StubConfigChanges {
    async fn record(&self, _row: &ConfigChangeRow) -> Result<(), ConfigChangesError> {
        Ok(())
    }
    async fn tail(&self, _n: usize) -> Result<Vec<ConfigChangeRow>, ConfigChangesError> {
        Ok(Vec::new())
    }
    async fn get(&self, _patch_id: &str) -> Result<Option<ConfigChangeRow>, ConfigChangesError> {
        Ok(None)
    }
}

/// Build a fully-stubbed boot context — every optional handle is
/// populated. Tools that read a context-injected handle (e.g. the
/// MCP router tools that read `ctx.mcp`) will not be exercised
/// here for actual call shape — the dispatcher just needs to know
/// `mcp_runtime` is non-`None` to register them.
async fn full_boot_ctx() -> McpServerBootContext {
    use nexo_mcp::SessionMcpRuntime;
    use std::collections::HashMap;
    use uuid::Uuid;

    let mut bc =
        McpServerBootContext::builder("exposable-test", AnyBroker::local(), fixture_agent_ctx())
            .cron_store(Arc::new(StubCron))
            .config_changes_store(Arc::new(StubConfigChanges))
            .build();
    bc.long_term_memory = Some(Arc::new(
        nexo_memory::LongTermMemory::open(":memory:")
            .await
            .expect("memory open"),
    ));
    bc.web_search_router = Some(Arc::new(nexo_web_search::WebSearchRouter::new(
        vec![Arc::new(
            nexo_web_search::providers::duckduckgo::DuckDuckGoProvider::new(12000),
        )],
        None,
    )));
    bc.mcp_runtime = Some(Arc::new(SessionMcpRuntime::new(
        Uuid::new_v4(),
        "exposable-test-fingerprint".into(),
        HashMap::new(),
    )));
    // Workspace-git fixture (temp dir).
    let td = tempfile::tempdir().expect("tempdir");
    bc.memory_git = Some(Arc::new(
        nexo_core::agent::MemoryGitRepo::open_or_init(td.path(), "test", "t@x").unwrap(),
    ));
    // Keep the tempdir alive for the duration of the boot context by
    // leaking it — these are test fixtures, lifetime semantics are
    // tolerable. Real boot wiring uses persistent paths.
    Box::leak(Box::new(td));
    // TaskFlow manager — use the real SqliteFlowStore in a temp file
    // so FlowManager gets a concrete Arc<dyn FlowStore>.
    let _td2 = tempfile::tempdir().expect("tempdir2");
    let path = _td2.path().join("taskflow.db");
    Box::leak(Box::new(_td2));
    let store = nexo_taskflow::SqliteFlowStore::open(&path.to_string_lossy())
        .await
        .expect("flowstore open");
    bc.taskflow_manager = Some(Arc::new(nexo_taskflow::FlowManager::new(Arc::new(store))));

    // LSP manager — empty launcher (no probed binaries) keeps the
    // tool def static + skips real spawn at boot.
    bc.lsp_manager = Some(nexo_lsp::LspManager::with_launcher(
        nexo_lsp::LspLauncher::probe_with(|_| None::<std::path::PathBuf>),
        nexo_lsp::SessionConfig::default(),
    ));

    // Team store — open an in-memory SqliteTeamStore.
    let _td3 = tempfile::tempdir().expect("tempdir3");
    let team_path = _td3.path().join("teams.db");
    Box::leak(Box::new(_td3));
    let team_store = nexo_team_store::SqliteTeamStore::open(&team_path.to_string_lossy())
        .await
        .expect("teams open");
    bc.team_store = Some(Arc::new(team_store) as Arc<dyn nexo_team_store::TeamStore>);
    bc
}

fn empty_boot_ctx() -> McpServerBootContext {
    McpServerBootContext::builder("exposable-test", AnyBroker::local(), fixture_agent_ctx()).build()
}

// ---------------------------------------------------------------------------
// Catalog-level invariants
// ---------------------------------------------------------------------------

#[test]
fn catalog_no_duplicate_names() {
    let mut seen = std::collections::HashSet::new();
    for entry in EXPOSABLE_TOOLS {
        assert!(seen.insert(entry.name), "duplicate: {}", entry.name);
    }
}

#[test]
fn lookup_returns_every_listed_entry() {
    for entry in EXPOSABLE_TOOLS {
        let got: &ExposableToolEntry = lookup_exposable(entry.name).expect(entry.name);
        assert_eq!(got.name, entry.name);
    }
}

#[test]
fn catalog_has_at_least_ten_always_entries() {
    let count = EXPOSABLE_TOOLS
        .iter()
        .filter(|e| matches!(e.boot_kind, BootKind::Always))
        .count();
    assert!(count >= 10, "expected ≥ 10 Always entries, got {count}");
}

// ---------------------------------------------------------------------------
// Per-disposition boot tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_denied_entry_skips_with_reason() {
    let bc = empty_boot_ctx();
    let mut hit = 0;
    for entry in EXPOSABLE_TOOLS {
        if let BootKind::DeniedByPolicy { reason } = entry.boot_kind {
            match boot_exposable(entry.name, &bc) {
                BootResult::SkippedDenied { reason: actual } => {
                    assert_eq!(actual, reason, "name={}", entry.name);
                    hit += 1;
                }
                other => panic!(
                    "expected SkippedDenied for {}, got {}",
                    entry.name,
                    other.skip_reason()
                ),
            }
        }
    }
    assert!(hit >= 3, "expected ≥ 3 denied entries, got {hit}");
}

#[tokio::test]
async fn every_deferred_entry_skips_with_phase() {
    let bc = empty_boot_ctx();
    for entry in EXPOSABLE_TOOLS {
        if let BootKind::Deferred { phase, .. } = entry.boot_kind {
            match boot_exposable(entry.name, &bc) {
                BootResult::SkippedDeferred { phase: p, .. } => {
                    assert_eq!(p, phase, "name={}", entry.name);
                }
                other => panic!(
                    "expected SkippedDeferred for {}, got {}",
                    entry.name,
                    other.skip_reason()
                ),
            }
        }
    }
}

#[tokio::test]
async fn feature_gated_entries_skip_when_default_features() {
    let bc = empty_boot_ctx();
    for entry in EXPOSABLE_TOOLS {
        if matches!(entry.boot_kind, BootKind::FeatureGated) {
            match boot_exposable(entry.name, &bc) {
                BootResult::SkippedFeatureGated { feature } => {
                    assert_eq!(feature, entry.feature_gate.unwrap(), "name={}", entry.name);
                }
                // With the feature compiled in, the empty boot
                // context still skips because handles are missing.
                // Either outcome confirms the gate is enforced.
                BootResult::SkippedInfraMissing { handle } => {
                    assert!(
                        handle.starts_with("config_"),
                        "unexpected handle label: {handle}"
                    );
                }
                other => panic!(
                    "expected SkippedFeatureGated or SkippedInfraMissing for {}, got {}",
                    entry.name,
                    other.skip_reason()
                ),
            }
        }
    }
}

#[tokio::test]
async fn unknown_name_returns_unknown() {
    let bc = empty_boot_ctx();
    assert!(matches!(
        boot_exposable("does_not_exist_anywhere", &bc),
        BootResult::UnknownName
    ));
}

// ---------------------------------------------------------------------------
// Always-bucket round-trip — every entry boots when handles are present.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_always_entry_boots_with_full_context() {
    let bc = full_boot_ctx().await;
    for entry in EXPOSABLE_TOOLS {
        if !matches!(entry.boot_kind, BootKind::Always) {
            continue;
        }
        match boot_exposable(entry.name, &bc) {
            BootResult::Registered(def, _h) => {
                assert_eq!(def.name, entry.name, "tool def name mismatch");
                assert!(
                    def.parameters.is_object(),
                    "tool {} has non-object schema",
                    entry.name
                );
                let obj = def.parameters.as_object().unwrap();
                assert!(
                    obj.get("type").map(|v| v.as_str()) == Some(Some("object")),
                    "tool {} schema missing top-level type=object",
                    entry.name
                );
            }
            other => panic!(
                "expected Registered for {}, got {}",
                entry.name,
                other.skip_reason()
            ),
        }
    }
}

#[tokio::test]
async fn always_entries_skip_with_clear_handle_label_when_handle_missing() {
    let bc = empty_boot_ctx();
    // We expect at least one Always entry per handle category to
    // surface SkippedInfraMissing when the empty context is used.
    let mut handles_seen = std::collections::HashSet::new();
    for entry in EXPOSABLE_TOOLS {
        if !matches!(entry.boot_kind, BootKind::Always) {
            continue;
        }
        if let BootResult::SkippedInfraMissing { handle } = boot_exposable(entry.name, &bc) {
            assert!(!handle.is_empty(), "empty handle label for {}", entry.name);
            handles_seen.insert(handle);
        }
    }
    // We should see all handle categories.
    for expected in [
        "cron_store",
        "mcp_runtime",
        "config_changes_store",
        "web_search_router",
        "long_term_memory",
        "memory_git",
        "taskflow_manager",
        "lsp_manager",
        "team_store",
    ] {
        assert!(
            handles_seen.contains(expected),
            "expected to see handle '{}' missing in empty ctx, saw {:?}",
            expected,
            handles_seen
        );
    }
}
