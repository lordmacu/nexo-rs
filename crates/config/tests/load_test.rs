use agent_config::AppConfig;
use std::fs;
use std::io::Write;
use std::sync::Mutex;

// Serialize tests that mutate env vars to avoid races.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

fn write_fixtures(dir: &std::path::Path) {
    std::env::set_var("MINIMAX_API_KEY", "test_key");
    std::env::set_var("MINIMAX_GROUP_ID", "test_group");
    std::env::set_var("WA_CREDENTIALS_FILE", "/tmp/wa_creds.json");
    std::env::set_var("TELEGRAM_BOT_TOKEN", "test_token");
    std::env::set_var("SMTP_HOST", "smtp.test.com");
    std::env::set_var("SMTP_USER", "user@test.com");
    std::env::set_var("SMTP_PASSWORD", "pass");
    std::env::set_var("IMAP_HOST", "imap.test.com");

    write_file(dir, "agents.yaml", r#"
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
    plugins: [whatsapp]
    heartbeat:
      enabled: true
      interval: "5m"
"#);
    write_file(dir, "broker.yaml", r#"
broker:
  type: "nats"
  url: "nats://localhost:4222"
"#);
    write_file(dir, "llm.yaml", r#"
providers:
  minimax:
    api_key: "${MINIMAX_API_KEY}"
    group_id: "${MINIMAX_GROUP_ID}"
    base_url: "https://api.minimax.chat/v1"
"#);
    write_file(dir, "memory.yaml", r#"
short_term:
  max_history_turns: 50
  session_ttl: "24h"
long_term:
  backend: "sqlite"
  sqlite:
    path: "./data/memory.db"
vector:
  backend: "sqlite-vec"
  embedding:
    provider: "minimax"
    model: "embo-01"
    dimensions: 1536
"#);

    fs::create_dir_all(dir.join("plugins")).unwrap();
    write_file(&dir.join("plugins"), "whatsapp.yaml", r#"
whatsapp:
  session_dir: "./data/sessions"
  credentials_file: "${WA_CREDENTIALS_FILE}"
"#);
    write_file(&dir.join("plugins"), "telegram.yaml", r#"
telegram:
  token: "${TELEGRAM_BOT_TOKEN}"
"#);
    write_file(&dir.join("plugins"), "email.yaml", r#"
email:
  smtp:
    host: "${SMTP_HOST}"
    username: "${SMTP_USER}"
    password: "${SMTP_PASSWORD}"
  imap:
    host: "${IMAP_HOST}"
"#);
}

#[test]
fn happy_path() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    let cfg = AppConfig::load(dir.path()).expect("should load");
    assert_eq!(cfg.agents.agents[0].id, "kate");
    assert_eq!(cfg.llm.providers["minimax"].api_key, "test_key");
    assert!(cfg.plugins.whatsapp.is_some());
    assert!(cfg.plugins.telegram.is_some());
    assert!(cfg.plugins.email.is_some());
}

#[test]
fn missing_env_var() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    std::env::remove_var("MINIMAX_API_KEY");
    let err = AppConfig::load(dir.path()).unwrap_err();
    let msg = format!("{:#}", err);
    assert!(msg.contains("MINIMAX_API_KEY"), "got: {msg}");
}

#[test]
fn optional_plugin_absent() {
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    fs::remove_file(dir.path().join("plugins/email.yaml")).unwrap();
    let cfg = AppConfig::load(dir.path()).unwrap();
    assert!(cfg.plugins.email.is_none());
}

#[test]
fn unknown_field_in_agents_yaml() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    write_file(dir.path(), "agents.yaml", r#"
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
    typo_field: "oops"
"#);
    let err = AppConfig::load(dir.path()).unwrap_err();
    let msg = format!("{:#}", err);
    assert!(msg.contains("typo_field") || msg.contains("unknown"), "got: {msg}");
}

#[test]
fn file_secret_resolved() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());

    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    write!(tmp, "  file_secret_value  ").unwrap();
    let secret_path = tmp.path().to_str().unwrap().to_string();

    write_file(dir.path(), "llm.yaml", &format!(r#"
providers:
  minimax:
    api_key: "${{file:{secret_path}}}"
    group_id: "${{MINIMAX_GROUP_ID}}"
    base_url: "https://api.minimax.chat/v1"
"#));

    std::env::set_var("MINIMAX_API_KEY", "not_used");
    let cfg = AppConfig::load(dir.path()).unwrap();
    assert_eq!(cfg.llm.providers["minimax"].api_key, "file_secret_value");
}

#[test]
fn defaults_applied() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    write_file(dir.path(), "agents.yaml", r#"
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
"#);
    let cfg = AppConfig::load(dir.path()).unwrap();
    assert_eq!(cfg.agents.agents[0].heartbeat.interval, "5m");
    assert_eq!(cfg.agents.agents[0].config.debounce_ms, 2000);
}
