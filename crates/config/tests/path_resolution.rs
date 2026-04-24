//! Regression coverage for `AppConfig::load` canonicalising
//! agent filesystem paths against the config directory.

use agent_config::AppConfig;
use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;

// load_test.rs already uses environment variables; share the lock so
// these tests don't race on MINIMAX_API_KEY / TELEGRAM_BOT_TOKEN etc.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_minimal_config(dir: &std::path::Path) {
    std::env::set_var("MINIMAX_API_KEY", "test_key");
    std::env::set_var("MINIMAX_GROUP_ID", "test_group");
    std::env::set_var("TELEGRAM_BOT_TOKEN", "test_token");
    fs::write(
        dir.join("agents.yaml"),
        r#"
agents:
  - id: ana
    model:
      provider: minimax
      model: MiniMax-M2.5
    system_prompt: "hi"
    workspace: ./data/workspace/ana
    skills_dir: ./skills
    transcripts_dir: ./data/transcripts/ana
    extra_docs:
      - ./RULES.md
"#,
    )
    .unwrap();
    fs::write(
        dir.join("broker.yaml"),
        "broker:\n  type: \"nats\"\n  url: \"nats://localhost:4222\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("llm.yaml"),
        r#"
providers:
  minimax:
    api_key: "${MINIMAX_API_KEY}"
    group_id: "${MINIMAX_GROUP_ID}"
    base_url: "https://api.minimax.chat/v1"
"#,
    )
    .unwrap();
    fs::write(
        dir.join("memory.yaml"),
        r#"
short_term:
  max_history_turns: 50
  session_ttl: "24h"
long_term:
  backend: "sqlite"
  sqlite:
    path: "./data/memory.db"
vector:
  backend: "sqlite-vec"
"#,
    )
    .unwrap();
}

#[test]
fn load_canonicalises_skills_and_workspace_paths() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    write_minimal_config(tmp.path());

    let cfg = AppConfig::load(tmp.path()).expect("config load");
    let ana = &cfg.agents.agents[0];

    let expected_skills = tmp
        .path()
        .join("skills")
        .to_string_lossy()
        .into_owned();
    let expected_workspace = tmp
        .path()
        .join("data/workspace/ana")
        .to_string_lossy()
        .into_owned();
    let expected_transcripts = tmp
        .path()
        .join("data/transcripts/ana")
        .to_string_lossy()
        .into_owned();
    let expected_rules = tmp.path().join("RULES.md").to_string_lossy().into_owned();

    assert_eq!(ana.skills_dir, expected_skills);
    assert_eq!(ana.workspace, expected_workspace);
    assert_eq!(ana.transcripts_dir, expected_transcripts);
    assert_eq!(ana.extra_docs, vec![expected_rules]);
}

#[test]
fn load_preserves_absolute_paths_and_empty_strings() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    // Override the minimal config with absolute paths + empty
    // transcripts_dir.
    std::env::set_var("MINIMAX_API_KEY", "k");
    std::env::set_var("MINIMAX_GROUP_ID", "g");
    std::env::set_var("TELEGRAM_BOT_TOKEN", "t");
    fs::write(
        tmp.path().join("agents.yaml"),
        r#"
agents:
  - id: ana
    model: { provider: minimax, model: MiniMax-M2.5 }
    workspace: /var/lib/agent/ana
    skills_dir: /opt/agent/skills
    transcripts_dir: ""
"#,
    )
    .unwrap();
    fs::write(tmp.path().join("broker.yaml"), "broker:\n  type: \"nats\"\n  url: \"nats://localhost:4222\"\n").unwrap();
    fs::write(
        tmp.path().join("llm.yaml"),
        r#"
providers:
  minimax:
    api_key: "${MINIMAX_API_KEY}"
    group_id: "${MINIMAX_GROUP_ID}"
    base_url: "https://api.minimax.chat/v1"
"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("memory.yaml"),
        r#"
short_term:
  max_history_turns: 50
  session_ttl: "24h"
long_term:
  backend: "sqlite"
  sqlite:
    path: "/tmp/x.db"
vector:
  backend: "sqlite-vec"
"#,
    )
    .unwrap();

    let cfg = AppConfig::load(tmp.path()).expect("config load");
    let ana = &cfg.agents.agents[0];

    assert_eq!(ana.workspace, "/var/lib/agent/ana");
    assert_eq!(ana.skills_dir, "/opt/agent/skills");
    assert_eq!(ana.transcripts_dir, "");
}
