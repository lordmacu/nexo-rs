//! Phase 10.8 — end-to-end test of self-report tools against a seeded
//! LongTermMemory + a temp workspace with IDENTITY/SOUL/MEMORY fixtures.

use std::sync::Arc;
use std::time::Duration;

use nexo_broker::AnyBroker;
use nexo_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use nexo_core::agent::{AgentContext, MyStatsTool, ToolHandler, WhatDoIKnowTool, WhoAmITool};
use nexo_core::session::SessionManager;
use nexo_memory::LongTermMemory;
use serde_json::json;
use uuid::Uuid;

fn agent_cfg() -> Arc<AgentConfig> {
    Arc::new(AgentConfig {
        id: "kate".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "MiniMax-M2.5".into(),
        },
        plugins: vec![],
        heartbeat: HeartbeatConfig::default(),
        config: AgentRuntimeConfig::default(),
        system_prompt: String::new(),
        workspace: String::new(),
        skills: vec![],
        skills_dir: "./skills".into(),
        skill_overrides: Default::default(),
        transcripts_dir: String::new(),
        dreaming: Default::default(),
        workspace_git: Default::default(),
        tool_rate_limits: None,
        tool_args_validation: None,
        extra_docs: Vec::new(),
        inbound_bindings: Vec::new(),
        allowed_tools: Vec::new(),
        sender_rate_limit: None,
        allowed_delegates: Vec::new(),
        accept_delegates_from: Vec::new(),
        description: String::new(),
        outbound_allowlist: Default::default(),
        google_auth: None,
        credentials: Default::default(),
        link_understanding: serde_json::Value::Null,
        web_search: serde_json::Value::Null,
        pairing_policy: serde_json::Value::Null,
        language: None,
        context_optimization: None,
        dispatch_policy: Default::default(),
        plan_mode: Default::default(),
        remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
    })
}

async fn ctx_with_memory(memory: Arc<LongTermMemory>) -> AgentContext {
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    AgentContext::new("kate", agent_cfg(), broker, sessions).with_memory(memory)
}

#[tokio::test]
async fn who_am_i_returns_identity_from_workspace() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    tokio::fs::write(
        dir.join("IDENTITY.md"),
        "- **Name:** Kate\n- **Creature:** cat\n- **Vibe:** playful\n",
    )
    .await?;
    tokio::fs::write(dir.join("SOUL.md"), "I believe in short answers.\n").await?;

    let memory = Arc::new(LongTermMemory::open(":memory:").await?);
    let ctx = ctx_with_memory(memory).await;
    let tool = WhoAmITool::new("kate", "MiniMax-M2.5", Some(dir.to_path_buf()));
    let out = tool.call(&ctx, json!({})).await?;

    assert_eq!(out["agent_id"], "kate");
    assert_eq!(out["model"], "MiniMax-M2.5");
    assert_eq!(out["identity"]["name"], "Kate");
    assert_eq!(out["identity"]["creature"], "cat");
    assert!(out["soul_excerpt"]
        .as_str()
        .unwrap()
        .contains("short answers"));
    Ok(())
}

#[tokio::test]
async fn who_am_i_handles_missing_workspace() -> anyhow::Result<()> {
    let memory = Arc::new(LongTermMemory::open(":memory:").await?);
    let ctx = ctx_with_memory(memory).await;
    let tool = WhoAmITool::new("solo", "stub-model", None);
    let out = tool.call(&ctx, json!({})).await?;
    assert_eq!(out["agent_id"], "solo");
    assert!(out["workspace_dir"].is_null());
    assert!(out["identity"].is_null());
    assert!(out["soul_excerpt"].is_null());
    Ok(())
}

#[tokio::test]
async fn what_do_i_know_parses_memory_md() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    let memory_md = "\
# MEMORY

## User preferences
- short answers
- spanish conversations

## Facts
- lives in madrid
- likes coffee
";
    tokio::fs::write(dir.join("MEMORY.md"), memory_md).await?;

    let memory = Arc::new(LongTermMemory::open(":memory:").await?);
    let ctx = ctx_with_memory(memory).await;
    let tool = WhatDoIKnowTool::new(Some(dir.to_path_buf()));
    let out = tool.call(&ctx, json!({})).await?;

    let sections = out["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0]["heading"], "User preferences");
    assert_eq!(sections[0]["bullets"].as_array().unwrap().len(), 2);
    assert_eq!(out["truncated"], false);
    Ok(())
}

#[tokio::test]
async fn what_do_i_know_applies_section_filter() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    let memory_md = "## User preferences\n- a\n\n## Facts\n- b\n";
    tokio::fs::write(dir.join("MEMORY.md"), memory_md).await?;

    let memory = Arc::new(LongTermMemory::open(":memory:").await?);
    let ctx = ctx_with_memory(memory).await;
    let tool = WhatDoIKnowTool::new(Some(dir.to_path_buf()));
    let out = tool.call(&ctx, json!({ "section": "fact" })).await?;
    let sections = out["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0]["heading"], "Facts");
    Ok(())
}

#[tokio::test]
async fn my_stats_aggregates_counts_and_last_dream() -> anyhow::Result<()> {
    let memory = Arc::new(LongTermMemory::open(":memory:").await?);

    // Seed memories + sessions + promotions + recall events.
    let m1 = memory
        .remember("kate", "OpenAI quota monitoring", &[])
        .await?;
    let m2 = memory.remember("kate", "Router VLAN config", &[]).await?;
    let _other = memory.remember("other", "unrelated", &[]).await?;

    let s1 = Uuid::new_v4();
    let s2 = Uuid::new_v4();
    memory.save_interaction(s1, "kate", "user", "hi").await?;
    memory
        .save_interaction(s2, "kate", "user", "follow-up")
        .await?;

    memory.mark_promoted("kate", m1, 0.5, "deep").await?;
    memory.record_recall_event("kate", m1, "q1", 1.0).await?;
    memory.record_recall_event("kate", m1, "q2", 0.8).await?;
    memory.record_recall_event("kate", m2, "q3", 0.4).await?;

    let ctx = ctx_with_memory(memory.clone()).await;
    let tool = MyStatsTool::new(memory.clone(), None);
    let out = tool.call(&ctx, json!({})).await?;

    assert_eq!(out["agent_id"], "kate");
    assert_eq!(out["memories_stored"], 2);
    assert_eq!(out["sessions_total"], 2);
    assert_eq!(out["memories_promoted"], 1);
    assert_eq!(out["recall_events_7d"], 3);
    assert!(out["last_dream_ts"].is_string());
    let top = out["top_concept_tags_7d"].as_array().unwrap();
    assert!(
        !top.is_empty(),
        "expected at least one concept tag, got {:?}",
        top
    );
    Ok(())
}
