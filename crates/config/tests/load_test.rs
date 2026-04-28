use nexo_config::AppConfig;
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

    write_file(
        dir,
        "agents.yaml",
        r#"
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
    plugins: [whatsapp]
    heartbeat:
      enabled: true
      interval: "5m"
"#,
    );
    write_file(
        dir,
        "broker.yaml",
        r#"
broker:
  type: "nats"
  url: "nats://localhost:4222"
"#,
    );
    write_file(
        dir,
        "llm.yaml",
        r#"
providers:
  minimax:
    api_key: "${MINIMAX_API_KEY}"
    group_id: "${MINIMAX_GROUP_ID}"
    base_url: "https://api.minimax.chat/v1"
"#,
    );
    write_file(
        dir,
        "memory.yaml",
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
  embedding:
    provider: "minimax"
    model: "embo-01"
    dimensions: 1536
"#,
    );

    fs::create_dir_all(dir.join("plugins")).unwrap();
    write_file(
        &dir.join("plugins"),
        "whatsapp.yaml",
        r#"
whatsapp:
  session_dir: "./data/sessions"
  credentials_file: "${WA_CREDENTIALS_FILE}"
"#,
    );
    write_file(
        &dir.join("plugins"),
        "telegram.yaml",
        r#"
telegram:
  token: "${TELEGRAM_BOT_TOKEN}"
"#,
    );
    write_file(
        &dir.join("plugins"),
        "email.yaml",
        r#"
email:
  enabled: true
  accounts:
    - instance: ops
      address: ops@test.com
      provider: custom
      imap: { host: "${IMAP_HOST}", port: 993, tls: implicit_tls }
      smtp: { host: "${SMTP_HOST}", port: 587, tls: starttls }
"#,
    );
}

#[test]
fn happy_path() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    let cfg = AppConfig::load(dir.path()).expect("should load");
    assert_eq!(cfg.agents.agents[0].id, "kate");
    assert_eq!(cfg.llm.providers["minimax"].api_key, "test_key");
    assert_eq!(cfg.plugins.whatsapp.len(), 1);
    assert_eq!(cfg.plugins.telegram.len(), 1);
    assert!(cfg.plugins.email.is_some());
}

#[test]
fn whatsapp_supports_multi_account_list_shape() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    write_file(
        &dir.path().join("plugins"),
        "whatsapp.yaml",
        r#"
whatsapp:
  - session_dir: "./data/sessions/biz"
    credentials_file: "${WA_CREDENTIALS_FILE}"
    instance: "biz"
  - session_dir: "./data/sessions/support"
    credentials_file: "${WA_CREDENTIALS_FILE}"
    instance: "support"
"#,
    );
    let cfg = AppConfig::load(dir.path()).expect("should load");
    assert_eq!(cfg.plugins.whatsapp.len(), 2);
    assert_eq!(cfg.plugins.whatsapp[0].instance.as_deref(), Some("biz"));
    assert_eq!(cfg.plugins.whatsapp[1].instance.as_deref(), Some("support"));
    // Each instance has its own session_dir (the critical per-account
    // isolation requirement for Signal protocol keys).
    assert_ne!(
        cfg.plugins.whatsapp[0].session_dir,
        cfg.plugins.whatsapp[1].session_dir,
    );
}

#[test]
fn telegram_supports_multi_bot_list_shape() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    // Overwrite the telegram fixture with the list-shape variant.
    write_file(
        &dir.path().join("plugins"),
        "telegram.yaml",
        r#"
telegram:
  - token: "${TELEGRAM_BOT_TOKEN}"
    instance: "bot_boss"
  - token: "${TELEGRAM_BOT_TOKEN}"
    instance: "bot_sales"
"#,
    );
    let cfg = AppConfig::load(dir.path()).expect("should load");
    assert_eq!(cfg.plugins.telegram.len(), 2);
    assert_eq!(
        cfg.plugins.telegram[0].instance.as_deref(),
        Some("bot_boss")
    );
    assert_eq!(
        cfg.plugins.telegram[1].instance.as_deref(),
        Some("bot_sales")
    );
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
    write_file(
        dir.path(),
        "agents.yaml",
        r#"
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
    typo_field: "oops"
"#,
    );
    let err = AppConfig::load(dir.path()).unwrap_err();
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("typo_field") || msg.contains("unknown"),
        "got: {msg}"
    );
}

#[test]
fn file_secret_resolved() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());

    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    write!(tmp, "  file_secret_value  ").unwrap();
    let secret_path = tmp.path().to_str().unwrap().to_string();
    let parent = tmp.path().parent().unwrap().to_path_buf();
    // Whitelist the tempfile's directory for the path validator added
    // by the `${file:...}` traversal fix; otherwise /tmp is rejected.
    std::env::set_var("CONFIG_SECRETS_DIR", &parent);

    write_file(
        dir.path(),
        "llm.yaml",
        &format!(
            r#"
providers:
  minimax:
    api_key: "${{file:{secret_path}}}"
    group_id: "${{MINIMAX_GROUP_ID}}"
    base_url: "https://api.minimax.chat/v1"
"#
        ),
    );

    std::env::set_var("MINIMAX_API_KEY", "not_used");
    let cfg = AppConfig::load(dir.path()).unwrap();
    assert_eq!(cfg.llm.providers["minimax"].api_key, "file_secret_value");
    std::env::remove_var("CONFIG_SECRETS_DIR");
}

#[test]
fn defaults_applied() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    write_file(
        dir.path(),
        "agents.yaml",
        r#"
agents:
  - id: "kate"
    model:
      provider: "minimax"
      model: "MiniMax-M2.5"
"#,
    );
    let cfg = AppConfig::load(dir.path()).unwrap();
    assert_eq!(cfg.agents.agents[0].heartbeat.interval, "5m");
    assert_eq!(cfg.agents.agents[0].config.debounce_ms, 2000);
}

#[test]
fn migrations_auto_apply_false_leaves_files_untouched() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    write_file(
        dir.path(),
        "agents.yaml",
        r#"
agent:
  id: "kate"
  model:
    provider: "minimax"
    model: "MiniMax-M2.5"
"#,
    );
    write_file(
        dir.path(),
        "runtime.yaml",
        r#"
migrations:
  auto_apply: false
"#,
    );

    let err = AppConfig::load(dir.path()).expect_err("legacy shape should fail without auto-apply");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("agents") || msg.contains("unknown"),
        "got: {msg}"
    );

    let agents_raw = fs::read_to_string(dir.path().join("agents.yaml")).unwrap();
    assert!(!agents_raw.contains("schema_version:"));
    assert!(agents_raw.contains("agent:"));
}

#[test]
fn migrations_auto_apply_true_rewrites_then_loads() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_fixtures(dir.path());
    write_file(
        dir.path(),
        "agents.yaml",
        r#"
agent:
  id: "kate"
  model:
    provider: "minimax"
    model: "MiniMax-M2.5"
"#,
    );
    write_file(
        dir.path(),
        "runtime.yaml",
        r#"
migrations:
  auto_apply: true
"#,
    );

    let cfg = AppConfig::load(dir.path()).expect("auto-apply should migrate legacy shape");
    assert_eq!(cfg.agents.agents.len(), 1);
    assert_eq!(cfg.agents.agents[0].id, "kate");

    let agents_raw = fs::read_to_string(dir.path().join("agents.yaml")).unwrap();
    assert!(agents_raw.contains("schema_version: 11"));
    assert!(agents_raw.contains("agents:"));
}
