use std::sync::Arc;

use nexo_driver_permission::{
    AllowAllDecider, AllowScope, DenyAllDecider, PermissionMcpServer, PermissionOutcome,
    ScriptedDecider,
};
use nexo_mcp::server::McpServerHandler;
use serde_json::json;

fn args(tool: &str, input: serde_json::Value) -> serde_json::Value {
    json!({
        "tool_name": tool,
        "input": input,
        "tool_use_id": "tu_test",
    })
}

fn parse_behavior(content_text: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(content_text).expect("json text");
    v["behavior"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn list_tools_returns_single_permission_prompt() {
    let server = PermissionMcpServer::new(Arc::new(AllowAllDecider));
    let tools = server.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "permission_prompt");
    assert!(tools[0].description.is_some());
}

#[tokio::test]
async fn call_tool_unknown_returns_protocol_error() {
    let server = PermissionMcpServer::new(Arc::new(AllowAllDecider));
    let err = server
        .call_tool("nope", args("Edit", json!({})))
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("unknown tool nope"));
}

#[tokio::test]
async fn call_tool_invalid_args_returns_protocol_error() {
    let server = PermissionMcpServer::new(Arc::new(AllowAllDecider));
    // Missing required `tool_name`.
    let err = server
        .call_tool("permission_prompt", json!({"input": {}}))
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("invalid arguments"));
}

#[tokio::test]
async fn call_tool_allow_all_returns_allow_value() {
    let server = PermissionMcpServer::new(Arc::new(AllowAllDecider));
    let res = server
        .call_tool("permission_prompt", args("Edit", json!({"path": "x"})))
        .await
        .unwrap();
    assert!(!res.is_error);
    let nexo_mcp::McpContent::Text { text } = &res.content[0] else {
        panic!("expected text content");
    };
    assert_eq!(parse_behavior(text), "allow");
}

#[tokio::test]
async fn call_tool_deny_all_returns_deny_with_message() {
    let server = PermissionMcpServer::new(Arc::new(DenyAllDecider {
        reason: "audit".into(),
    }));
    let res = server
        .call_tool("permission_prompt", args("Bash", json!({"cmd": "ls"})))
        .await
        .unwrap();
    assert!(res.is_error, "deny outcome must set is_error=true");
    let nexo_mcp::McpContent::Text { text } = &res.content[0] else {
        panic!("expected text content");
    };
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["behavior"], "deny");
    assert_eq!(v["message"], "audit");
}

#[tokio::test]
async fn call_tool_scripted_iteration() {
    let decider = ScriptedDecider::new([
        PermissionOutcome::AllowOnce {
            updated_input: None,
        },
        PermissionOutcome::Deny {
            message: "no".into(),
        },
    ]);
    let server = PermissionMcpServer::new(Arc::new(decider));
    let r1 = server
        .call_tool("permission_prompt", args("Edit", json!({"a": 1})))
        .await
        .unwrap();
    let r2 = server
        .call_tool("permission_prompt", args("Edit", json!({"a": 2})))
        .await
        .unwrap();
    assert!(!r1.is_error);
    assert!(r2.is_error);
}

#[tokio::test]
async fn allow_session_caches_subsequent_call() {
    // Script ONE outcome — second call must come from cache, not exhaust.
    let decider = ScriptedDecider::new([PermissionOutcome::AllowSession {
        scope: AllowScope::Session,
        updated_input: None,
    }]);
    let server = PermissionMcpServer::new(Arc::new(decider));
    let input = json!({"file": "src/lib.rs"});
    let r1 = server
        .call_tool("permission_prompt", args("Read", input.clone()))
        .await
        .unwrap();
    assert!(!r1.is_error);
    // Same args → cache hit. If the cache failed, ScriptedDecider would
    // return `Decider("ScriptedDecider exhausted")`.
    let r2 = server
        .call_tool("permission_prompt", args("Read", input))
        .await
        .unwrap();
    assert!(!r2.is_error);
}

#[tokio::test]
async fn allow_once_does_not_cache() {
    // Script TWO outcomes — second call must consume the second one,
    // proving AllowOnce did not cache.
    let decider = ScriptedDecider::new([
        PermissionOutcome::AllowOnce {
            updated_input: None,
        },
        PermissionOutcome::Deny {
            message: "second call".into(),
        },
    ]);
    let server = PermissionMcpServer::new(Arc::new(decider));
    let input = json!({"file": "src/lib.rs"});
    let r1 = server
        .call_tool("permission_prompt", args("Read", input.clone()))
        .await
        .unwrap();
    assert!(!r1.is_error, "first allow_once");
    let r2 = server
        .call_tool("permission_prompt", args("Read", input))
        .await
        .unwrap();
    assert!(r2.is_error, "second call drew the Deny outcome");
}

#[tokio::test]
async fn cache_key_differs_for_same_tool_distinct_input() {
    // First AllowSession caches `{a:1}`; second call has `{a:2}` so it
    // must miss the cache and consume the second outcome.
    let decider = ScriptedDecider::new([
        PermissionOutcome::AllowSession {
            scope: AllowScope::Session,
            updated_input: None,
        },
        PermissionOutcome::Deny {
            message: "different input".into(),
        },
    ]);
    let server = PermissionMcpServer::new(Arc::new(decider));
    let r1 = server
        .call_tool("permission_prompt", args("Edit", json!({"a": 1})))
        .await
        .unwrap();
    let r2 = server
        .call_tool("permission_prompt", args("Edit", json!({"a": 2})))
        .await
        .unwrap();
    assert!(!r1.is_error);
    assert!(r2.is_error);
}
