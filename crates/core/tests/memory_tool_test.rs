//! Phase 10.5 integration test — MemoryTool.recall auto-logs recall events
//! so the signal store can drive the dreaming sweep later.

use std::sync::Arc;
use std::time::Duration;

use nexo_broker::AnyBroker;
use nexo_config::types::agents::{AgentConfig, AgentRuntimeConfig, HeartbeatConfig, ModelConfig};
use nexo_core::agent::{AgentContext, MemoryTool, ToolHandler};
use nexo_core::session::SessionManager;
use nexo_memory::LongTermMemory;
use serde_json::json;

#[tokio::test]
async fn memory_recall_records_events_for_every_hit() -> anyhow::Result<()> {
    let memory = Arc::new(LongTermMemory::open(":memory:").await?);
    let a = memory
        .remember("kate", "Cristian likes dark mode", &[])
        .await?;
    let b = memory
        .remember("kate", "Cristian prefers dark colors", &[])
        .await?;
    let _c = memory.remember("kate", "Kate runs on MiniMax", &[]).await?;

    let cfg = Arc::new(AgentConfig {
        id: "kate".into(),
        model: ModelConfig {
            provider: "stub".into(),
            model: "m1".into(),
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
    });
    let broker = AnyBroker::local();
    let sessions = Arc::new(SessionManager::new(Duration::from_secs(60), 20));
    let ctx = AgentContext::new("kate", cfg, broker, sessions).with_memory(Arc::clone(&memory));

    let tool = MemoryTool::new(Arc::clone(&memory));
    let out = tool
        .call(
            &ctx,
            json!({ "action": "recall", "query": "dark", "limit": 5 }),
        )
        .await?;
    let results = out["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "two memories match 'dark'");

    // Both surfaced memories must have one recall event each.
    let sig_a = memory.recall_signals("kate", a, None).await?;
    let sig_b = memory.recall_signals("kate", b, None).await?;
    assert_eq!(sig_a.recall_count, 1);
    assert_eq!(sig_b.recall_count, 1);
    // Position 1 → score 1.0, position 2 → 0.5. Exact order depends on FTS5 bm25,
    // so just assert the relevance values sum to 1.5 (top + second hit).
    let total = sig_a.relevance + sig_b.relevance;
    assert!(
        (total - 1.5).abs() < 1e-4,
        "expected 1.0+0.5 total, got {total}"
    );

    // Second recall with a different query → signals accumulate.
    tool.call(
        &ctx,
        json!({ "action": "recall", "query": "Cristian", "limit": 5 }),
    )
    .await?;
    let sig_a2 = memory.recall_signals("kate", a, None).await?;
    assert_eq!(sig_a2.recall_count, 2);
    // Two distinct queries → diversity must be > 0.
    assert!(sig_a2.diversity > 0.0);
    Ok(())
}
