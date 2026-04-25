//! Smoke test for the bin: spawn `nexo-driver-permission-mcp --allow-all`,
//! drive it through `initialize` → `tools/list` → `tools/call`, and
//! assert the response is a Claude `behavior:"allow"`.
//!
//! Unix-only — Windows lacks the spawn ergonomics we lean on (we use
//! `tokio::process::Command` directly, no shell).

#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

const BIN_ENV: &str = env!("CARGO_BIN_EXE_nexo-driver-permission-mcp");

async fn write_request(stdin: &mut ChildStdin, body: serde_json::Value) {
    let line = format!("{}\n", body);
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_response_with_id(reader: &mut BufReader<ChildStdout>, id: i64) -> serde_json::Value {
    // The bin emits notifications too — skip until we see the response
    // matching `id`.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut line = String::new();
        let read = tokio::time::timeout_at(deadline, reader.read_line(&mut line))
            .await
            .expect("response timed out")
            .unwrap();
        assert!(read > 0, "stdout closed before response");
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
            return v;
        }
    }
}

#[tokio::test]
async fn bin_serves_initialize_list_tools_and_permission_prompt() {
    let mut child: Child = tokio::process::Command::new(BIN_ENV)
        .arg("--allow-all")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bin");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize
    write_request(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name":"smoke","version":"0"}
            }
        }),
    )
    .await;
    let init = read_response_with_id(&mut reader, 1).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        "nexo-driver-permission"
    );

    // 2. tools/list
    write_request(
        &mut stdin,
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{}
        }),
    )
    .await;
    let list = read_response_with_id(&mut reader, 2).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "permission_prompt");

    // 3. tools/call permission_prompt
    write_request(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "permission_prompt",
                "arguments": {
                    "tool_name": "Edit",
                    "input": {"file_path":"src/x.rs"},
                    "tool_use_id": "tu_smoke"
                }
            }
        }),
    )
    .await;
    let call = read_response_with_id(&mut reader, 3).await;
    let content = &call["result"]["content"];
    let text = content[0]["text"].as_str().expect("text content");
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        parsed["behavior"], "allow",
        "AllowAllDecider returned: {parsed}"
    );

    // Tear down — close stdin so the bin exits.
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = child.start_kill();
}
