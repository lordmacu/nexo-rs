//! Phase 67.G.3 — list / status / logs / hooks query tools.

use std::sync::Arc;

use chrono::Utc;
use nexo_agent_registry::{
    AgentHandle, AgentRegistry, AgentRunStatus, AgentSnapshot, LogBuffer, MemoryAgentRegistryStore,
};
use nexo_dispatch_tools::{
    agent_hooks_list, agent_logs_tail, agent_status, list_agents, AgentHooksListInput,
    AgentLogsTailInput, AgentStatusInput, CompletionHook, HookAction, HookRegistry, HookTrigger,
    ListAgentsInput,
};
use nexo_driver_claude::OriginChannel;
use nexo_driver_types::GoalId;
use uuid::Uuid;

fn handle_with(
    phase: &str,
    status: AgentRunStatus,
    origin: Option<OriginChannel>,
    turn_index: u32,
    max_turns: u32,
) -> AgentHandle {
    AgentHandle {
        goal_id: GoalId(Uuid::new_v4()),
        phase_id: phase.into(),
        status,
        origin,
        dispatcher: None,
        started_at: Utc::now(),
        finished_at: None,
        snapshot: AgentSnapshot {
            turn_index,
            max_turns,
            ..AgentSnapshot::default()
        },
        plan_mode: None,
    }
}

#[tokio::test]
async fn list_agents_renders_markdown_table_with_filter() {
    // cap=1 forces the second admission into the queue so the
    // filter assertion below has something to suppress.
    let reg = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        1,
    ));
    let h1 = handle_with("67.10", AgentRunStatus::Running, None, 3, 40);
    let h2 = handle_with("67.11", AgentRunStatus::Queued, None, 0, 0);
    let id1 = h1.goal_id;
    reg.admit(h1, true).await.unwrap();
    reg.admit(h2, true).await.unwrap();
    reg.set_max_turns(id1, 40);

    let out = list_agents(
        ListAgentsInput {
            filter: Some("running".into()),
            phase_prefix: None,
        },
        reg,
    )
    .await;
    assert!(out.starts_with("| status |"));
    assert!(out.contains("67.10"));
    assert!(!out.contains("67.11"));
}

#[tokio::test]
async fn agent_status_includes_origin_when_set() {
    let reg = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let origin = OriginChannel {
        plugin: "telegram".into(),
        instance: "family".into(),
        sender_id: "@cris".into(),
        correlation_id: None,
    };
    let h = handle_with("67.10", AgentRunStatus::Running, Some(origin), 5, 40);
    let id = h.goal_id;
    reg.admit(h, true).await.unwrap();
    reg.set_max_turns(id, 40);

    let out = agent_status(AgentStatusInput { goal_id: id }, reg).await;
    assert!(out.contains("67.10"));
    assert!(out.contains("turn 5/40"));
    assert!(out.contains("telegram:family"));
}

#[tokio::test]
async fn agent_status_unknown_goal() {
    let reg = Arc::new(AgentRegistry::new(
        Arc::new(MemoryAgentRegistryStore::default()),
        4,
    ));
    let out = agent_status(
        AgentStatusInput {
            goal_id: GoalId(Uuid::new_v4()),
        },
        reg,
    )
    .await;
    assert!(out.contains("not in registry"));
}

#[tokio::test]
async fn agent_logs_tail_returns_recent_lines() {
    let buf = Arc::new(LogBuffer::new(5));
    let g = GoalId(Uuid::new_v4());
    for i in 0..3 {
        buf.push(g, "agent.driver.attempt.completed", format!("turn {i}"));
    }
    let out = agent_logs_tail(
        AgentLogsTailInput {
            goal_id: g,
            lines: 10,
        },
        buf,
    )
    .await;
    assert!(out.contains("turn 0"));
    assert!(out.contains("turn 2"));
}

#[tokio::test]
async fn agent_logs_tail_empty_returns_no_logs() {
    let buf = Arc::new(LogBuffer::new(5));
    let out = agent_logs_tail(
        AgentLogsTailInput {
            goal_id: GoalId(Uuid::new_v4()),
            lines: 10,
        },
        buf,
    )
    .await;
    assert_eq!(out, "no logs");
}

#[tokio::test]
async fn agent_hooks_list_renders_attached_hooks() {
    let hooks = Arc::new(HookRegistry::new());
    let g = GoalId(Uuid::new_v4());
    hooks.add(
        g,
        CompletionHook {
            id: "h1".into(),
            on: HookTrigger::Done,
            action: HookAction::NotifyOrigin,
        },
    );
    hooks.add(
        g,
        CompletionHook {
            id: "chain-1".into(),
            on: HookTrigger::Done,
            action: HookAction::DispatchPhase {
                phase_id: "67.11".into(),
                only_if: HookTrigger::Done,
            },
        },
    );
    let out = agent_hooks_list(AgentHooksListInput { goal_id: g }, hooks).await;
    assert!(out.contains("h1"));
    assert!(out.contains("notify_origin"));
    assert!(out.contains("dispatch_phase(67.11"));
}

#[tokio::test]
async fn agent_hooks_list_empty_returns_no_hooks() {
    let hooks = Arc::new(HookRegistry::new());
    let out = agent_hooks_list(
        AgentHooksListInput {
            goal_id: GoalId(Uuid::new_v4()),
        },
        hooks,
    )
    .await;
    assert!(out.contains("no hooks"));
}
