//! End-to-end tests that spawn `bash mock-claude.sh` and exercise
//! the `TurnHandle` plumbing. Unix-only — Windows lacks the bash
//! interpreter we rely on for the mock.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_driver_claude::{spawn_turn, ClaudeCommand, ClaudeError, ClaudeEvent, ResultEvent};
use nexo_driver_types::CancellationToken;

fn mock_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-claude.sh")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Build a command that runs `bash <mock-claude.sh>`. The first
/// positional arg ("bash") becomes the binary; we pass the script
/// path as the prompt arg slot, which the mock ignores. The flags
/// the builder appends (`-p` etc.) also land as bash positional
/// args, which the mock doesn't read.
///
/// This is intentionally hacky — the mock's *behaviour* is driven
/// purely through env vars set on the builder, not its argv.
fn mock_command() -> ClaudeCommand {
    ClaudeCommand::new("bash", mock_path().display().to_string())
}

#[tokio::test]
async fn spawn_turn_drains_full_stream() {
    let cmd = mock_command().env(
        "MOCK_FIXTURE",
        fixture("init_assistant_result.jsonl").display().to_string(),
    );
    let cancel = CancellationToken::new();
    let mut turn = spawn_turn(
        cmd,
        &cancel,
        Duration::from_secs(10),
        Duration::from_secs(1),
    )
    .await
    .unwrap();

    let events = turn.drain().await.unwrap();
    assert_eq!(events.len(), 5);
    assert!(matches!(
        events.last().unwrap(),
        ClaudeEvent::Result(ResultEvent::Success { .. })
    ));

    let exit = turn.shutdown().await.unwrap();
    assert!(exit.success(), "mock exit was {exit:?}");
}

#[tokio::test]
async fn cancel_during_stream_returns_cancelled_error() {
    // Mock sleeps 5s before any output; cancel fires fast.
    let cmd = mock_command().env("MOCK_SLEEP_BEFORE", "5");
    let cancel = CancellationToken::new();
    let mut turn = spawn_turn(
        cmd,
        &cancel,
        Duration::from_secs(30),
        Duration::from_secs(2),
    )
    .await
    .unwrap();

    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel2.cancel();
    });

    let err = turn.next_event().await.unwrap_err();
    assert!(matches!(err, ClaudeError::Cancelled), "got {err:?}");

    let _ = turn.shutdown().await;
}

#[tokio::test]
async fn turn_timeout_returns_timeout_error() {
    let cmd = mock_command().env("MOCK_SLEEP_BEFORE", "5");
    let cancel = CancellationToken::new();
    let mut turn = spawn_turn(
        cmd,
        &cancel,
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .await
    .unwrap();

    let err = turn.next_event().await.unwrap_err();
    assert!(matches!(err, ClaudeError::Timeout), "got {err:?}");

    let _ = turn.shutdown().await;
}

#[tokio::test]
async fn shutdown_force_kills_long_running_child() {
    let cmd = mock_command().env("MOCK_SLEEP_FOREVER", "1");
    let cancel = CancellationToken::new();
    let turn = spawn_turn(
        cmd,
        &cancel,
        Duration::from_secs(30),
        Duration::from_millis(500),
    )
    .await
    .unwrap();

    let started = Instant::now();
    let _ = turn.shutdown().await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "shutdown took {elapsed:?}; expected <3s"
    );
}
