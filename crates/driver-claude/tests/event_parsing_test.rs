use std::path::PathBuf;

use nexo_driver_claude::{
    ClaudeError, ClaudeEvent, ContentBlock, EventStream, ResultEvent, SystemEvent,
};
use tokio::io::BufReader;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

async fn drain_fixture(name: &str) -> Vec<ClaudeEvent> {
    let bytes = tokio::fs::read(fixture(name)).await.unwrap();
    let mut s = EventStream::new(BufReader::new(std::io::Cursor::new(bytes)));
    let mut out = Vec::new();
    while let Some(ev) = s.next().await.unwrap() {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn happy_turn_yields_init_assistant_user_assistant_result() {
    let events = drain_fixture("init_assistant_result.jsonl").await;
    assert_eq!(events.len(), 5);
    assert!(matches!(
        events[0],
        ClaudeEvent::System(SystemEvent::Init { .. })
    ));
    assert!(matches!(events[1], ClaudeEvent::Assistant(_)));
    assert!(matches!(events[2], ClaudeEvent::User(_)));
    assert!(matches!(events[3], ClaudeEvent::Assistant(_)));
    assert!(matches!(
        events[4],
        ClaudeEvent::Result(ResultEvent::Success { .. })
    ));

    if let ClaudeEvent::Result(ResultEvent::Success {
        total_cost_usd,
        result,
        ..
    }) = &events[4]
    {
        assert_eq!(*total_cost_usd, Some(0.0234));
        assert_eq!(result.as_deref(), Some("Done."));
    } else {
        panic!("expected Success");
    }
}

#[tokio::test]
async fn error_max_turns_parses() {
    let events = drain_fixture("error_max_turns.jsonl").await;
    let last = events.last().unwrap();
    assert!(matches!(
        last,
        ClaudeEvent::Result(ResultEvent::ErrorMaxTurns { num_turns: 50, .. })
    ));
}

#[tokio::test]
async fn multi_tool_use_keeps_thinking_block_and_tool_uses() {
    let events = drain_fixture("multi_tool_use.jsonl").await;
    let assistant = events
        .iter()
        .find_map(|e| match e {
            ClaudeEvent::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(assistant.message.content.len(), 3);
    assert!(matches!(
        assistant.message.content[0],
        ContentBlock::Thinking { .. }
    ));
    assert!(matches!(
        assistant.message.content[1],
        ContentBlock::ToolUse { .. }
    ));
    assert!(matches!(
        assistant.message.content[2],
        ContentBlock::ToolUse { .. }
    ));
}

#[tokio::test]
async fn unknown_event_lands_in_other_without_aborting_stream() {
    let events = drain_fixture("unknown_event.jsonl").await;
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], ClaudeEvent::System(_)));
    assert!(matches!(events[1], ClaudeEvent::Other));
    assert!(matches!(
        events[2],
        ClaudeEvent::Result(ResultEvent::Success { .. })
    ));
}

#[tokio::test]
async fn invalid_jsonl_line_reports_parse_error_with_line_number() {
    // First line valid, second line broken (truncated JSON).
    let raw = b"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"a\",\"cwd\":\"/x\",\"model\":\"m\",\"permission_mode\":\"default\"}\n{\"type\":\n";
    let mut s = EventStream::new(BufReader::new(std::io::Cursor::new(&raw[..])));
    let _ok = s.next().await.unwrap();
    let err = s.next().await.unwrap_err();
    match err {
        ClaudeError::ParseLine { line_no, raw, .. } => {
            assert_eq!(line_no, 2);
            assert!(raw.starts_with("{\"type\":"));
        }
        other => panic!("expected ParseLine, got {other:?}"),
    }
}

#[tokio::test]
async fn session_id_accessor_recovers_id_from_each_variant() {
    let events = drain_fixture("init_assistant_result.jsonl").await;
    for e in &events {
        assert_eq!(e.session_id(), Some("01HZX-SESSION-1"));
    }
}
