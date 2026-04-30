//! Phase 79.M — boot dispatcher for `EXPOSABLE_TOOLS`.
//!
//! Each named entry maps to a small `boot_*` helper that constructs
//! the tool from handles in `McpServerBootContext`. Missing handles
//! become `BootResult::SkippedInfraMissing` so the rest of the
//! catalog still boots.

use std::sync::Arc;

use nexo_config::types::mcp_exposable::{lookup_exposable, BootKind};
use nexo_llm::ToolDef;

use super::context::McpServerBootContext;
use crate::agent::tool_registry::ToolHandler;

/// Outcome of a single per-tool boot attempt.
pub enum BootResult {
    /// Tool was constructed and is ready to register.
    Registered(ToolDef, Arc<dyn ToolHandler>),
    /// Tool name appears in `EXPOSABLE_TOOLS` but is hard-denied.
    SkippedDenied { reason: &'static str },
    /// Tool name appears but boot wiring is deferred.
    SkippedDeferred {
        phase: &'static str,
        reason: &'static str,
    },
    /// Tool name is feature-gated and the gate is off.
    SkippedFeatureGated { feature: &'static str },
    /// Tool requires a handle that the boot context didn't carry.
    SkippedInfraMissing { handle: &'static str },
    /// Tool name is not in `EXPOSABLE_TOOLS` at all.
    UnknownName,
}

impl BootResult {
    pub fn skip_reason(&self) -> &'static str {
        match self {
            BootResult::Registered(..) => "registered",
            BootResult::SkippedDenied { .. } => "denied_by_policy",
            BootResult::SkippedDeferred { .. } => "deferred",
            BootResult::SkippedFeatureGated { .. } => "feature_gate_off",
            BootResult::SkippedInfraMissing { .. } => "infra_missing",
            BootResult::UnknownName => "unknown_name",
        }
    }
}

/// Boot a single named tool against the catalog.
pub fn boot_exposable(name: &str, ctx: &McpServerBootContext) -> BootResult {
    let entry = match lookup_exposable(name) {
        Some(e) => e,
        None => return BootResult::UnknownName,
    };
    match entry.boot_kind {
        BootKind::Always => boot_always(name, ctx),
        BootKind::DeniedByPolicy { reason } => BootResult::SkippedDenied { reason },
        BootKind::Deferred { phase, reason } => BootResult::SkippedDeferred { phase, reason },
        BootKind::FeatureGated => boot_feature_gated(name, entry, ctx),
    }
}

#[allow(unused_variables)]
fn boot_feature_gated(
    name: &str,
    entry: &nexo_config::types::mcp_exposable::ExposableToolEntry,
    ctx: &McpServerBootContext,
) -> BootResult {
    #[cfg(feature = "config-self-edit")]
    if name == "Config" {
        return boot_config_tool(ctx);
    }
    BootResult::SkippedFeatureGated {
        feature: entry.feature_gate.unwrap_or("unknown"),
    }
}

#[cfg(feature = "config-self-edit")]
fn boot_config_tool(ctx: &McpServerBootContext) -> BootResult {
    use crate::agent::config_tool::ConfigTool;

    // Every handle must be present. Missing one → labelled
    // SkippedInfraMissing pointing at the operator-visible knob.
    let applier = match ctx.config_yaml_applier.as_ref() {
        Some(a) => Arc::clone(a),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_yaml_applier",
            }
        }
    };
    let denylist = match ctx.config_denylist_checker.as_ref() {
        Some(d) => Arc::clone(d),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_denylist_checker",
            }
        }
    };
    let redactor = match ctx.config_secret_redactor.as_ref() {
        Some(r) => Arc::clone(r),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_secret_redactor",
            }
        }
    };
    let correlator = match ctx.config_approval_correlator.as_ref() {
        Some(c) => Arc::clone(c),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_approval_correlator",
            }
        }
    };
    let reload = match ctx.config_reload_trigger.as_ref() {
        Some(r) => Arc::clone(r),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_reload_trigger",
            }
        }
    };
    let policy = match ctx.config_tool_policy.as_ref() {
        Some(p) => p.clone(),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_tool_policy",
            }
        }
    };
    let proposals_dir = match ctx.config_proposals_dir.as_ref() {
        Some(p) => p.clone(),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_proposals_dir",
            }
        }
    };
    let changes_store = match ctx.config_changes_store.as_ref() {
        Some(s) => Arc::clone(s),
        None => {
            return BootResult::SkippedInfraMissing {
                handle: "config_changes_store",
            }
        }
    };

    // Synthetic actor for mcp-server origin. The audit log surfaces
    // every mutation as `(channel="mcp", account_id=<server-name>)`
    // so operators see who touched the YAML.
    let actor_origin = crate::agent::config_tool::ActorOrigin {
        channel: "mcp".to_string(),
        account_id: ctx.agent_id.clone(),
        sender_id: "server".to_string(),
    };

    let cfg_tool = ConfigTool {
        agent_id: ctx.agent_id.clone(),
        binding_id: format!("mcp:{}", ctx.agent_id),
        allowed_paths: policy.allowed_paths.clone(),
        approval_timeout_secs: policy.approval_timeout_secs,
        proposals_dir,
        actor_origin,
        applier,
        denylist,
        redactor,
        changes_store,
        correlator,
        reload,
        pending_receivers: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
    };
    BootResult::Registered(ConfigTool::tool_def(), Arc::new(cfg_tool))
}

fn boot_always(name: &str, ctx: &McpServerBootContext) -> BootResult {
    use crate::agent::config_changes_tail_tool::ConfigChangesTailTool;
    use crate::agent::cron_tool::{
        CronCreateTool, CronDeleteTool, CronListTool, CronPauseTool, CronResumeTool,
    };
    use crate::agent::followup_tool::{CancelFollowupTool, CheckFollowupTool, StartFollowupTool};
    use crate::agent::mcp_router_tool::{ListMcpResourcesTool, ReadMcpResourceTool};
    use crate::agent::memory_checkpoint_tool::MemoryCheckpointTool;
    use crate::agent::memory_history_tool::MemoryHistoryTool;
    use crate::agent::notebook_edit_tool::NotebookEditTool;
    use crate::agent::plan_mode_tool::{EnterPlanModeTool, ExitPlanModeTool, PlanModeResolveTool};
    use crate::agent::synthetic_output_tool::SyntheticOutputTool;
    use crate::agent::taskflow_tool::TaskFlowTool;
    use crate::agent::todo_write_tool::TodoWriteTool;
    use crate::agent::tool_search_tool::ToolSearchTool;
    use crate::agent::web_fetch_tool::WebFetchTool;
    use crate::agent::web_search_tool::WebSearchTool;

    match name {
        // --- Step 5 — handle-free tools shipped by Phase 79.1–.4/.13 ---
        "EnterPlanMode" => {
            BootResult::Registered(EnterPlanModeTool::tool_def(), Arc::new(EnterPlanModeTool))
        }
        "ExitPlanMode" => {
            BootResult::Registered(ExitPlanModeTool::tool_def(), Arc::new(ExitPlanModeTool))
        }
        "ToolSearch" => {
            BootResult::Registered(ToolSearchTool::tool_def(), Arc::new(ToolSearchTool::new()))
        }
        "TodoWrite" => BootResult::Registered(TodoWriteTool::tool_def(), Arc::new(TodoWriteTool)),
        "SyntheticOutput" => BootResult::Registered(
            SyntheticOutputTool::tool_def(),
            Arc::new(SyntheticOutputTool),
        ),
        "NotebookEdit" => {
            BootResult::Registered(NotebookEditTool::tool_def(), Arc::new(NotebookEditTool))
        }

        // --- Step 6 — cron_* (require ctx.cron_store) ---
        "cron_create" => match ctx.cron_store.as_ref() {
            Some(s) => BootResult::Registered(
                CronCreateTool::tool_def(),
                Arc::new(CronCreateTool::new(Arc::clone(s))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "cron_store",
            },
        },
        "cron_list" => match ctx.cron_store.as_ref() {
            Some(s) => BootResult::Registered(
                CronListTool::tool_def(),
                Arc::new(CronListTool::new(Arc::clone(s))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "cron_store",
            },
        },
        "cron_delete" => match ctx.cron_store.as_ref() {
            Some(s) => BootResult::Registered(
                CronDeleteTool::tool_def(),
                Arc::new(CronDeleteTool::new(Arc::clone(s))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "cron_store",
            },
        },
        "cron_pause" => match ctx.cron_store.as_ref() {
            Some(s) => BootResult::Registered(
                CronPauseTool::tool_def(),
                Arc::new(CronPauseTool::new(Arc::clone(s))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "cron_store",
            },
        },
        "cron_resume" => match ctx.cron_store.as_ref() {
            Some(s) => BootResult::Registered(
                CronResumeTool::tool_def(),
                Arc::new(CronResumeTool::new(Arc::clone(s))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "cron_store",
            },
        },

        // --- Step 7 — mcp_router (handle-free; reads ctx.mcp at call time) ---
        "ListMcpResources" => match ctx.mcp_runtime.as_ref() {
            Some(_) => BootResult::Registered(
                ListMcpResourcesTool::tool_def(),
                Arc::new(ListMcpResourcesTool),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "mcp_runtime",
            },
        },
        "ReadMcpResource" => match ctx.mcp_runtime.as_ref() {
            Some(_) => BootResult::Registered(
                ReadMcpResourceTool::tool_def(),
                Arc::new(ReadMcpResourceTool),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "mcp_runtime",
            },
        },

        // --- Step 8 — config_changes_tail ---
        "config_changes_tail" => match ctx.config_changes_store.as_ref() {
            Some(s) => BootResult::Registered(
                ConfigChangesTailTool::tool_def(),
                Arc::new(ConfigChangesTailTool::new(Arc::clone(s))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "config_changes_store",
            },
        },

        // --- Step 9 — web_search + web_fetch ---
        "web_search" => match ctx.web_search_router.as_ref() {
            Some(r) => BootResult::Registered(
                WebSearchTool::tool_def(),
                Arc::new(WebSearchTool::new(Arc::clone(r))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "web_search_router",
            },
        },
        "web_fetch" => {
            BootResult::Registered(WebFetchTool::tool_def(), Arc::new(WebFetchTool::new()))
        }

        // --- Gap A — workspace-git audit log ---
        "forge_memory_checkpoint" => match ctx.memory_git.as_ref() {
            Some(g) => BootResult::Registered(
                MemoryCheckpointTool::tool_def(),
                Arc::new(MemoryCheckpointTool::new(Arc::clone(g))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "memory_git",
            },
        },
        "memory_history" => match ctx.memory_git.as_ref() {
            Some(g) => BootResult::Registered(
                MemoryHistoryTool::tool_def(),
                Arc::new(MemoryHistoryTool::new(Arc::clone(g))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "memory_git",
            },
        },

        // --- Gap A — taskflow ---
        "taskflow" => match ctx.taskflow_manager.as_ref() {
            Some(m) => BootResult::Registered(
                TaskFlowTool::tool_def(),
                Arc::new(TaskFlowTool::new((**m).clone())),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "taskflow_manager",
            },
        },
        "start_followup" => match ctx.long_term_memory.as_ref() {
            Some(mem) => BootResult::Registered(
                StartFollowupTool::tool_def(),
                Arc::new(StartFollowupTool::new(Arc::clone(mem))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "long_term_memory",
            },
        },
        "check_followup" => match ctx.long_term_memory.as_ref() {
            Some(mem) => BootResult::Registered(
                CheckFollowupTool::tool_def(),
                Arc::new(CheckFollowupTool::new(Arc::clone(mem))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "long_term_memory",
            },
        },
        "cancel_followup" => match ctx.long_term_memory.as_ref() {
            Some(mem) => BootResult::Registered(
                CancelFollowupTool::tool_def(),
                Arc::new(CancelFollowupTool::new(Arc::clone(mem))),
            ),
            None => BootResult::SkippedInfraMissing {
                handle: "long_term_memory",
            },
        },

        // --- Gap A — plan_mode_resolve (no separate handle; reads
        // ctx.plan_approval_registry which AgentContext::new
        // already initialises) ---
        "plan_mode_resolve" => BootResult::Registered(
            PlanModeResolveTool::tool_def(),
            Arc::new(PlanModeResolveTool),
        ),

        // --- 79.M.b — Lsp ---
        // C2 — `LspTool::new` no longer takes `policy`; the handler
        // pulls the per-call `LspPolicy` from `ctx.effective_policy()`
        // so a hot-reload of `lsp.languages` is observed on the next
        // call without re-registration.
        "Lsp" => match ctx.lsp_manager.as_ref() {
            Some(mgr) => {
                use crate::agent::lsp_tool::LspTool;
                let workspace_root = if ctx.agent_context.config.workspace.is_empty() {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                } else {
                    std::path::PathBuf::from(&ctx.agent_context.config.workspace)
                };
                let tool = LspTool::new(Arc::clone(mgr), workspace_root);
                BootResult::Registered(LspTool::tool_def_static(), Arc::new(tool))
            }
            None => BootResult::SkippedInfraMissing {
                handle: "lsp_manager",
            },
        },

        // --- 79.M.d.1 — TeamList + TeamStatus (read-only) ---
        // Construction needs a `TeamTools` shared inner. The router
        // is constructed unspawned because read-only ops never
        // touch DM channels. Mutating Team* tools stay deferred —
        // they need a spawned router + a real `current_goal_id`.
        "TeamList" => match ctx.team_store.as_ref() {
            Some(s) => {
                use crate::agent::team_tools::{TeamListTool, TeamTools};
                use crate::team_message_router::TeamMessageRouter;
                let router = TeamMessageRouter::new(Arc::new(ctx.broker.clone()));
                let tools = TeamTools::new(
                    Arc::clone(s),
                    router,
                    ctx.broker.clone(),
                    ctx.agent_context.config.team.clone(),
                    ctx.agent_id.clone(),
                    ctx.agent_id.clone(),
                );
                BootResult::Registered(TeamListTool::tool_def(), Arc::new(TeamListTool::new(tools)))
            }
            None => BootResult::SkippedInfraMissing {
                handle: "team_store",
            },
        },
        "TeamStatus" => match ctx.team_store.as_ref() {
            Some(s) => {
                use crate::agent::team_tools::{TeamStatusTool, TeamTools};
                use crate::team_message_router::TeamMessageRouter;
                let router = TeamMessageRouter::new(Arc::new(ctx.broker.clone()));
                let tools = TeamTools::new(
                    Arc::clone(s),
                    router,
                    ctx.broker.clone(),
                    ctx.agent_context.config.team.clone(),
                    ctx.agent_id.clone(),
                    ctx.agent_id.clone(),
                );
                BootResult::Registered(
                    TeamStatusTool::tool_def(),
                    Arc::new(TeamStatusTool::new(tools)),
                )
            }
            None => BootResult::SkippedInfraMissing {
                handle: "team_store",
            },
        },

        // --- 79.M.d.2 — Team* mutating (Create/Delete/SendMessage) ---
        // Same TeamTools shared inner. Router is constructed via
        // `Arc::new(ctx.broker.clone())` and the caller is expected
        // to spawn the router subscriber out-of-band (run_mcp_server
        // does this when any Team* mutating tool is requested).
        "TeamCreate" => match ctx.team_store.as_ref() {
            Some(s) => {
                use crate::agent::team_tools::{TeamCreateTool, TeamTools};
                use crate::team_message_router::TeamMessageRouter;
                let router = TeamMessageRouter::new(Arc::new(ctx.broker.clone()));
                let tools = TeamTools::new(
                    Arc::clone(s),
                    router,
                    ctx.broker.clone(),
                    ctx.agent_context.config.team.clone(),
                    ctx.agent_id.clone(),
                    ctx.agent_id.clone(),
                );
                BootResult::Registered(
                    TeamCreateTool::tool_def(),
                    Arc::new(TeamCreateTool::new(tools)),
                )
            }
            None => BootResult::SkippedInfraMissing {
                handle: "team_store",
            },
        },
        "TeamDelete" => match ctx.team_store.as_ref() {
            Some(s) => {
                use crate::agent::team_tools::{TeamDeleteTool, TeamTools};
                use crate::team_message_router::TeamMessageRouter;
                let router = TeamMessageRouter::new(Arc::new(ctx.broker.clone()));
                let tools = TeamTools::new(
                    Arc::clone(s),
                    router,
                    ctx.broker.clone(),
                    ctx.agent_context.config.team.clone(),
                    ctx.agent_id.clone(),
                    ctx.agent_id.clone(),
                );
                BootResult::Registered(
                    TeamDeleteTool::tool_def(),
                    Arc::new(TeamDeleteTool::new(tools)),
                )
            }
            None => BootResult::SkippedInfraMissing {
                handle: "team_store",
            },
        },
        "TeamSendMessage" => match ctx.team_store.as_ref() {
            Some(s) => {
                use crate::agent::team_tools::{TeamSendMessageTool, TeamTools};
                use crate::team_message_router::TeamMessageRouter;
                let router = TeamMessageRouter::new(Arc::new(ctx.broker.clone()));
                let tools = TeamTools::new(
                    Arc::clone(s),
                    router,
                    ctx.broker.clone(),
                    ctx.agent_context.config.team.clone(),
                    ctx.agent_id.clone(),
                    ctx.agent_id.clone(),
                );
                BootResult::Registered(
                    TeamSendMessageTool::tool_def(),
                    Arc::new(TeamSendMessageTool::new(tools)),
                )
            }
            None => BootResult::SkippedInfraMissing {
                handle: "team_store",
            },
        },

        // Catalog has the entry as Always but no boot helper exists yet —
        // should never trigger if EXPOSABLE_TOOLS and this match stay in
        // sync. Fail loud at runtime so a missing arm shows up in tests.
        other => BootResult::SkippedInfraMissing {
            handle: leak_arm(other),
        },
    }
}

// Stable string label for the "no helper for this Always entry"
// case. Returning a `&'static str` keeps the BootResult variant
// trivially `Copy`-shaped without leaking the missing-name string.
fn leak_arm(_name: &str) -> &'static str {
    "boot_helper_missing"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::context::AgentContext;
    use crate::config_changes_store::{ConfigChangeRow, ConfigChangesError, ConfigChangesStore};
    use crate::cron_schedule::{CronEntry, CronStore, CronStoreError};
    use async_trait::async_trait;
    use nexo_broker::AnyBroker;

    fn fixture_ctx() -> McpServerBootContext {
        use nexo_config::types::agents::{
            AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
            OutboundAllowlistConfig, WorkspaceGitConfig,
        };
        let cfg = AgentConfig {
            id: "a".into(),
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
        };
        let ctx = Arc::new(AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(crate::session::SessionManager::new(
                std::time::Duration::from_secs(60),
                8,
            )),
        ));
        McpServerBootContext::builder("a", AnyBroker::local(), ctx).build()
    }

    /// In-memory CronStore stub for tests.
    struct StubCron;
    #[async_trait]
    impl CronStore for StubCron {
        async fn insert(&self, _e: &CronEntry) -> Result<(), CronStoreError> {
            Ok(())
        }
        async fn list_by_binding(
            &self,
            _binding_id: &str,
        ) -> Result<Vec<CronEntry>, CronStoreError> {
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
    }

    /// In-memory ConfigChangesStore stub for tests.
    struct StubConfigChanges;
    #[async_trait]
    impl ConfigChangesStore for StubConfigChanges {
        async fn record(&self, _row: &ConfigChangeRow) -> Result<(), ConfigChangesError> {
            Ok(())
        }
        async fn tail(&self, _n: usize) -> Result<Vec<ConfigChangeRow>, ConfigChangesError> {
            Ok(Vec::new())
        }
        async fn get(
            &self,
            _patch_id: &str,
        ) -> Result<Option<ConfigChangeRow>, ConfigChangesError> {
            Ok(None)
        }
    }

    // --- skeleton dispatcher tests (denied / deferred / feature_gate / unknown) ---

    #[tokio::test]
    async fn unknown_name_returns_unknown() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("nope", &bc),
            BootResult::UnknownName
        ));
    }

    #[tokio::test]
    async fn delegate_is_denied_by_policy() {
        let bc = fixture_ctx();
        match boot_exposable("delegate", &bc) {
            BootResult::SkippedDenied { reason } => {
                assert!(reason.contains("a2a"));
            }
            other => panic!("expected SkippedDenied, got {}", other.skip_reason()),
        }
    }

    #[tokio::test]
    async fn lsp_skips_without_manager() {
        let bc = fixture_ctx();
        match boot_exposable("Lsp", &bc) {
            BootResult::SkippedInfraMissing { handle } => {
                assert_eq!(handle, "lsp_manager");
            }
            other => panic!("expected SkippedInfraMissing, got {}", other.skip_reason()),
        }
    }

    #[cfg(feature = "config-self-edit")]
    #[tokio::test]
    async fn config_registers_with_full_handles() {
        use crate::agent::approval_correlator::{ApprovalCorrelator, ApprovalCorrelatorConfig};
        use crate::agent::config_tool::{
            DefaultSecretRedactor, DenylistChecker, PatchAppliedError, PatchInfo, ReloadTrigger,
            YamlPatchApplier,
        };
        use std::path::PathBuf;

        // Lightweight stubs so we don't depend on nexo-setup in tests.
        struct StubApplier;
        impl YamlPatchApplier for StubApplier {
            fn read(
                &self,
                _: &str,
                _: &str,
            ) -> Result<Option<serde_yaml::Value>, PatchAppliedError> {
                Ok(None)
            }
            fn apply(&self, _: &PatchInfo) -> Result<(), PatchAppliedError> {
                Ok(())
            }
            fn snapshot(&self) -> Result<Vec<u8>, PatchAppliedError> {
                Ok(Vec::new())
            }
            fn restore(&self, _: &[u8]) -> Result<(), PatchAppliedError> {
                Ok(())
            }
        }
        struct StubDenylist;
        impl DenylistChecker for StubDenylist {
            fn check(&self, _: &str) -> Option<&'static str> {
                None
            }
        }
        struct StubReload;
        #[async_trait]
        impl ReloadTrigger for StubReload {
            async fn reload(&self) -> Result<(), String> {
                Ok(())
            }
        }

        let bc_minimal = fixture_ctx();
        let ctx_arc = bc_minimal.agent_context.clone();
        let bc = McpServerBootContext::builder("a", AnyBroker::local(), ctx_arc)
            .config_changes_store(Arc::new(StubConfigChanges))
            .config_handles(
                Arc::new(StubApplier),
                Arc::new(StubDenylist),
                Arc::new(DefaultSecretRedactor),
                ApprovalCorrelator::new(ApprovalCorrelatorConfig::default()),
                Arc::new(StubReload),
                nexo_config::types::config_tool::ConfigToolPolicy::default(),
                PathBuf::from("/tmp/proposals"),
            )
            .build();
        match boot_exposable("Config", &bc) {
            BootResult::Registered(def, _) => assert_eq!(def.name, "Config"),
            other => panic!("expected Registered, got {}", other.skip_reason()),
        }
    }

    #[tokio::test]
    async fn config_is_feature_gated() {
        let bc = fixture_ctx();
        // When feature is OFF, Config returns SkippedFeatureGated.
        // When feature is ON, the boot helper still returns
        // SkippedInfraMissing because the empty fixture context
        // didn't carry any of the 7 Config handles. Both outcomes
        // confirm the gate is enforced.
        match boot_exposable("Config", &bc) {
            BootResult::SkippedFeatureGated { feature } => {
                assert_eq!(feature, "config-self-edit");
            }
            #[cfg(feature = "config-self-edit")]
            BootResult::SkippedInfraMissing { handle } => {
                assert!(
                    handle.starts_with("config_"),
                    "unexpected handle label: {handle}"
                );
            }
            other => panic!(
                "expected SkippedFeatureGated or SkippedInfraMissing(config_*), got {}",
                other.skip_reason()
            ),
        }
    }

    // --- step 5 — Always entries with no handle ---

    #[tokio::test]
    async fn enter_plan_mode_registers() {
        let bc = fixture_ctx();
        let r = boot_exposable("EnterPlanMode", &bc);
        match r {
            BootResult::Registered(def, _) => assert_eq!(def.name, "EnterPlanMode"),
            other => panic!("expected Registered, got {}", other.skip_reason()),
        }
    }

    #[tokio::test]
    async fn exit_plan_mode_registers() {
        let bc = fixture_ctx();
        let r = boot_exposable("ExitPlanMode", &bc);
        assert!(matches!(r, BootResult::Registered(..)));
    }

    #[tokio::test]
    async fn tool_search_registers() {
        let bc = fixture_ctx();
        let r = boot_exposable("ToolSearch", &bc);
        assert!(matches!(r, BootResult::Registered(..)));
    }

    #[tokio::test]
    async fn todo_write_registers() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("TodoWrite", &bc),
            BootResult::Registered(..)
        ));
    }

    #[tokio::test]
    async fn synthetic_output_registers() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("SyntheticOutput", &bc),
            BootResult::Registered(..)
        ));
    }

    #[tokio::test]
    async fn notebook_edit_registers() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("NotebookEdit", &bc),
            BootResult::Registered(..)
        ));
    }

    // --- step 6 — cron_* ---

    #[tokio::test]
    async fn cron_create_skips_when_no_store() {
        let bc = fixture_ctx();
        match boot_exposable("cron_create", &bc) {
            BootResult::SkippedInfraMissing { handle } => assert_eq!(handle, "cron_store"),
            other => panic!("expected SkippedInfraMissing, got {}", other.skip_reason()),
        }
    }

    #[tokio::test]
    async fn cron_create_registers_with_store() {
        use nexo_config::types::agents::{
            AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
            OutboundAllowlistConfig, WorkspaceGitConfig,
        };
        let cfg = AgentConfig {
            id: "a".into(),
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
        };
        let actx = Arc::new(AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(crate::session::SessionManager::new(
                std::time::Duration::from_secs(60),
                8,
            )),
        ));
        let bc = McpServerBootContext::builder("a", AnyBroker::local(), actx)
            .cron_store(Arc::new(StubCron))
            .build();
        match boot_exposable("cron_create", &bc) {
            BootResult::Registered(def, _) => assert_eq!(def.name, "cron_create"),
            other => panic!("expected Registered, got {}", other.skip_reason()),
        }
    }

    #[tokio::test]
    async fn cron_list_skips_when_no_store() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("cron_list", &bc),
            BootResult::SkippedInfraMissing {
                handle: "cron_store"
            }
        ));
    }

    #[tokio::test]
    async fn cron_delete_pause_resume_all_skip_without_store() {
        let bc = fixture_ctx();
        for name in ["cron_delete", "cron_pause", "cron_resume"] {
            assert!(
                matches!(
                    boot_exposable(name, &bc),
                    BootResult::SkippedInfraMissing {
                        handle: "cron_store"
                    }
                ),
                "expected skip for {name}"
            );
        }
    }

    // --- step 7 — mcp_router ---

    #[tokio::test]
    async fn mcp_router_skips_without_runtime() {
        let bc = fixture_ctx();
        for name in ["ListMcpResources", "ReadMcpResource"] {
            assert!(
                matches!(
                    boot_exposable(name, &bc),
                    BootResult::SkippedInfraMissing {
                        handle: "mcp_runtime"
                    }
                ),
                "expected skip for {name}"
            );
        }
    }

    // --- step 8 — config_changes_tail ---

    #[tokio::test]
    async fn config_changes_tail_skips_without_store() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("config_changes_tail", &bc),
            BootResult::SkippedInfraMissing {
                handle: "config_changes_store"
            }
        ));
    }

    #[tokio::test]
    async fn config_changes_tail_registers_with_store() {
        use nexo_config::types::agents::{
            AgentConfig, AgentRuntimeConfig, DreamingYamlConfig, HeartbeatConfig, ModelConfig,
            OutboundAllowlistConfig, WorkspaceGitConfig,
        };
        let cfg = AgentConfig {
            id: "a".into(),
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
        };
        let actx = Arc::new(AgentContext::new(
            "a",
            Arc::new(cfg),
            AnyBroker::local(),
            Arc::new(crate::session::SessionManager::new(
                std::time::Duration::from_secs(60),
                8,
            )),
        ));
        let bc = McpServerBootContext::builder("a", AnyBroker::local(), actx)
            .config_changes_store(Arc::new(StubConfigChanges))
            .build();
        match boot_exposable("config_changes_tail", &bc) {
            BootResult::Registered(def, _) => assert_eq!(def.name, "config_changes_tail"),
            other => panic!("expected Registered, got {}", other.skip_reason()),
        }
    }

    // --- step 9 — web_search + web_fetch ---

    #[tokio::test]
    async fn web_search_skips_without_router() {
        let bc = fixture_ctx();
        assert!(matches!(
            boot_exposable("web_search", &bc),
            BootResult::SkippedInfraMissing {
                handle: "web_search_router"
            }
        ));
    }

    #[tokio::test]
    async fn web_fetch_registers_standalone() {
        let bc = fixture_ctx();
        match boot_exposable("web_fetch", &bc) {
            BootResult::Registered(def, _) => assert_eq!(def.name, "web_fetch"),
            other => panic!("expected Registered, got {}", other.skip_reason()),
        }
    }
}
