//! Phase 74.1 — Claude Code 2.1 conformance fixture.
//!
//! Drives `nexo-driver-permission-mcp --allow-all` through the
//! exact byte sequence Claude Code 2.1 sends and asserts every
//! response field that Phase 73 had to fix. Future regressions in
//! either Claude's schema or our wire format land as a failing
//! test instead of a silent loop where Cody dispatches goals that
//! never write a single file.
//!
//! Lives next to `bin_smoke_test.rs` (smoke) to keep the test
//! crate discoverable; the smoke test only checks the happy path
//! against the legacy 2024-11-05 protocol, this fixture pins down
//! every wire-shape detail Claude 2.1 enforces.
//!
//! Unix-only for the same reason as the smoke test (tokio Command
//! ergonomics; the bin itself is portable).

#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

const BIN_ENV: &str = env!("CARGO_BIN_EXE_nexo-driver-permission-mcp");

async fn write(stdin: &mut ChildStdin, body: Value) {
    stdin
        .write_all(format!("{body}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();
}

async fn read_id(reader: &mut BufReader<ChildStdout>, id: i64) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut line = String::new();
        let n = tokio::time::timeout_at(deadline, reader.read_line(&mut line))
            .await
            .expect("response timed out")
            .unwrap();
        assert!(n > 0, "stdout closed before response id={id}");
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
            return v;
        }
    }
}

/// Spawn the bin in `--allow-all` mode (no socket dependency) so
/// the fixture is hermetic.
async fn spawn() -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child: Child = tokio::process::Command::new(BIN_ENV)
        .arg("--allow-all")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bin");
    let stdin = child.stdin.take().unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, reader)
}

async fn shutdown(mut child: Child, stdin: ChildStdin) {
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = child.start_kill();
}

#[tokio::test]
async fn initialize_echoes_claude_2_1_protocol_version() {
    // Phase 73 — Claude Code 2.1 sends `protocolVersion: 2025-11-25`.
    // Echoing the legacy `2024-11-05` made Claude register the
    // server but silently drop every tool from the permission
    // registry ("Available MCP tools: none"). The server must
    // negotiate to the client's version when it is supported.
    let (child, mut stdin, mut reader) = spawn().await;
    write(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"roots":{}, "elicitation":{}},
                "clientInfo": {"name":"claude-code","version":"2.1.119"}
            }
        }),
    )
    .await;
    let resp = read_id(&mut reader, 1).await;
    assert_eq!(
        resp["result"]["protocolVersion"], "2025-11-25",
        "server must echo 2025-11-25 when client speaks it; got {resp}"
    );
    assert_eq!(
        resp["result"]["capabilities"]["tools"]["listChanged"], false,
        "tools.listChanged must be advertised for Claude's permission registry"
    );
    assert_eq!(
        resp["result"]["serverInfo"]["name"], "nexo-driver-permission",
        "serverInfo.name must match the config-key Claude uses to namespace tools"
    );
    shutdown(child, stdin).await;
}

#[tokio::test]
async fn initialize_falls_back_for_unknown_protocol_version() {
    // Future-proofing: a hypothetical 2099 client must still
    // negotiate to the version we know.
    let (child, mut stdin, mut reader) = spawn().await;
    write(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2099-01-01",
                "capabilities": {},
                "clientInfo": {"name":"future","version":"x"}
            }
        }),
    )
    .await;
    let resp = read_id(&mut reader, 1).await;
    let v = resp["result"]["protocolVersion"]
        .as_str()
        .expect("string version");
    assert_ne!(
        v, "2099-01-01",
        "server must NOT echo unknown versions; got {v}"
    );
    // Must be one of the known versions.
    assert!(
        ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"].contains(&v),
        "fallback should pick a supported version; got {v}"
    );
    shutdown(child, stdin).await;
}

#[tokio::test]
async fn tools_list_omits_next_cursor_when_no_pagination() {
    // Phase 73 — Claude Code 2.1's Zod validator refused tools
    // when the response contained `nextCursor: null`. The MCP
    // spec treats the field as pagination-only; absence means
    // "no more pages". Confirm we no longer emit the field.
    let (child, mut stdin, mut reader) = spawn().await;
    write(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"x","version":"1"}
        }}),
    )
    .await;
    let _ = read_id(&mut reader, 1).await;
    write(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    let resp = read_id(&mut reader, 2).await;
    assert!(
        resp["result"].get("nextCursor").is_none(),
        "nextCursor must be omitted (Claude rejects null); got {resp}"
    );
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "permission_prompt");
    let schema = &tools[0]["inputSchema"];
    assert_eq!(schema["type"], "object");
    assert!(
        schema["properties"]["tool_name"].is_object(),
        "permission_prompt input schema must declare tool_name"
    );
    // Phase 74.2 — outputSchema declared so Claude 2.1's Zod-style
    // validator runs against our typed shape. Phase 75 dialed the
    // shape back to a permissive object schema after the strict
    // `oneOf` + `additionalProperties:false` form made Claude
    // silently drop the tool. The accepted form names the
    // discriminator (`behavior`) and lists the optional fields
    // without closing the object.
    let out_schema = &tools[0]["outputSchema"];
    assert!(
        out_schema.is_object(),
        "permission_prompt must declare outputSchema; got {out_schema}"
    );
    assert_eq!(out_schema["type"], "object");
    let req = out_schema["required"].as_array().expect("required array");
    assert!(req.iter().any(|v| v == "behavior"));
    let behavior_enum = out_schema["properties"]["behavior"]["enum"]
        .as_array()
        .expect("behavior enum");
    let labels: Vec<&str> = behavior_enum.iter().filter_map(|v| v.as_str()).collect();
    assert!(labels.contains(&"allow"));
    assert!(labels.contains(&"deny"));
    shutdown(child, stdin).await;
}

#[tokio::test]
async fn permission_prompt_allow_response_includes_updated_input_record() {
    // Phase 73 — Claude Code 2.1 schema requires `updatedInput`
    // to be a record (object) on every `behavior:"allow"`
    // response. Earlier we omitted it when the decider had no
    // override and Claude rejected the tool with a Zod
    // `invalid_union` / `invalid_type` error, surfacing as
    // "Hook malformed. Returns neither valid update nor deny."
    // and the operator-visible Edit/Bash/Write call failing.
    let (child, mut stdin, mut reader) = spawn().await;
    write(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"claude-code","version":"2.1.119"}
        }}),
    )
    .await;
    let _ = read_id(&mut reader, 1).await;
    write(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;

    let original_input = json!({"file_path":"src/lib.rs","content":"hi"});
    write(
        &mut stdin,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{
                "name":"permission_prompt",
                "arguments":{
                    "tool_name":"Write",
                    "input": original_input,
                    "tool_use_id":"tu_phase74"
                }
            }
        }),
    )
    .await;
    let resp = read_id(&mut reader, 2).await;
    let content = resp["result"]["content"].as_array().expect("content array");
    let text = content[0]["text"].as_str().expect("text content");
    let parsed: Value =
        serde_json::from_str(text).expect("permission_prompt text is JSON for Claude to validate");
    assert_eq!(parsed["behavior"], "allow");
    let updated = &parsed["updatedInput"];
    assert!(
        updated.is_object(),
        "updatedInput MUST be a record (Zod expected: 'record'); got {updated}"
    );
    // When the decider has no override, the original input is
    // echoed verbatim — that's what makes the response satisfy
    // Claude without dropping caller-supplied fields.
    assert_eq!(
        updated, &original_input,
        "AllowOnce echoes the caller's input by default"
    );
    // Phase 74.3 — same payload also surfaces in structuredContent
    // so Claude 2.1's typed validator can run against the object
    // directly instead of re-parsing the text.
    let structured = &resp["result"]["structuredContent"];
    assert!(
        structured.is_object(),
        "structuredContent must be present alongside text; got {structured}"
    );
    assert_eq!(structured["behavior"], "allow");
    assert_eq!(structured["updatedInput"], original_input);
    shutdown(child, stdin).await;
}

#[tokio::test]
async fn permission_prompt_deny_response_includes_message() {
    // Mirror of the allow-shape test: the Zod schema's other
    // branch is `behavior:"deny"` + `message:string`. Use the
    // `--deny-all` mode so the bin produces a deny without
    // needing a daemon-side decider.
    let mut child: Child = tokio::process::Command::new(BIN_ENV)
        .arg("--deny-all")
        .arg("destructive op blocked by fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bin");
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    write(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"x","version":"1"}
        }}),
    )
    .await;
    let _ = read_id(&mut reader, 1).await;
    write(
        &mut stdin,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{
                "name":"permission_prompt",
                "arguments":{
                    "tool_name":"Bash",
                    "input":{"command":"rm -rf /"},
                    "tool_use_id":"tu_deny"
                }
            }
        }),
    )
    .await;
    let resp = read_id(&mut reader, 2).await;
    let content = resp["result"]["content"].as_array().expect("content array");
    let text = content[0]["text"].as_str().expect("text content");
    // Phase 77.8 — text may carry bash security warnings prefix.
    // Use structured_content for the machine-readable shape.
    assert!(
        text.contains("WARNING — bash security"),
        "destructive Bash should carry warning; got {text}"
    );
    let parsed = resp["result"]["structuredContent"]
        .as_object()
        .expect("structured_content must be an object");
    assert_eq!(parsed["behavior"], "deny");
    assert!(
        parsed["message"].as_str().is_some(),
        "deny must carry message; got {parsed:?}"
    );
    assert!(
        !parsed.contains_key("updatedInput"),
        "deny must NOT include updatedInput (Zod's union picks the deny branch when message is set)"
    );
    shutdown(child, stdin).await;
}

#[tokio::test]
async fn unknown_tool_returns_protocol_error() {
    // Defensive: a typoed tool name from the operator (or a
    // misconfigured `--permission-prompt-tool` flag) should fail
    // loudly with an error response, not a silent empty allow.
    let (child, mut stdin, mut reader) = spawn().await;
    write(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"x","version":"1"}
        }}),
    )
    .await;
    let _ = read_id(&mut reader, 1).await;
    write(
        &mut stdin,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{ "name":"not_a_real_tool", "arguments":{} }
        }),
    )
    .await;
    let resp = read_id(&mut reader, 2).await;
    assert!(
        resp.get("error").is_some() || resp["result"]["isError"] == true,
        "unknown tool must surface as JSON-RPC error or isError=true; got {resp}"
    );
    shutdown(child, stdin).await;
}
