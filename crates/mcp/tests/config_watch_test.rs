//! Phase 12.8 — integration: file watcher picks up edits to `mcp.yaml`
//! and drives `McpRuntimeManager::update_config`.

use std::io::Write;
use std::time::Duration;

use agent_mcp::config_watch::{
    spawn_mcp_config_watcher, EXTENSIONS_YAML_FILENAME, MCP_YAML_FILENAME,
};
use agent_mcp::{McpRuntimeConfig, McpRuntimeManager};
use tokio_util::sync::CancellationToken;

fn write_yaml(path: &std::path::Path, body: &str) {
    let tmp = path.with_extension("yaml.tmp");
    let mut f = std::fs::File::create(&tmp).expect("create tmp");
    f.write_all(body.as_bytes()).expect("write");
    f.sync_all().expect("sync");
    // Atomic replace — mimics what editors do when saving.
    std::fs::rename(&tmp, path).expect("rename");
}

fn write_manifest(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir -p");
    let mut f = std::fs::File::create(path).expect("create manifest");
    f.write_all(body.as_bytes()).expect("write manifest");
    f.sync_all().expect("sync manifest");
}

async fn wait_for_fingerprint_change(
    mgr: &std::sync::Arc<McpRuntimeManager>,
    initial: &str,
    deadline: Duration,
) -> Option<String> {
    let start = tokio::time::Instant::now();
    loop {
        let fp = mgr.current_fingerprint().await;
        if fp != initial {
            return Some(fp);
        }
        if start.elapsed() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn reload_on_valid_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MCP_YAML_FILENAME);
    write_yaml(
        &path,
        r#"mcp:
  enabled: true
  servers: {}
"#,
    );

    let empty = agent_config::McpConfig::default();
    let mgr = McpRuntimeManager::new(McpRuntimeConfig::from_yaml(&empty));
    let initial_fp = mgr.current_fingerprint().await;

    let shutdown = CancellationToken::new();
    spawn_mcp_config_watcher(
        dir.path().to_path_buf(),
        mgr.clone(),
        Vec::new(),
        None,
        Duration::from_millis(100),
        shutdown.clone(),
    );

    // Give the watcher a tick to register.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Add a server — content change triggers reload.
    write_yaml(
        &path,
        r#"mcp:
  enabled: true
  servers:
    echo:
      transport: stdio
      command: "/bin/true"
"#,
    );

    let new_fp = wait_for_fingerprint_change(&mgr, &initial_fp, Duration::from_secs(3))
        .await
        .expect("fingerprint did not change in time");
    assert_ne!(new_fp, initial_fp);
    shutdown.cancel();
}

#[tokio::test]
async fn invalid_yaml_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(MCP_YAML_FILENAME);
    write_yaml(
        &path,
        r#"mcp:
  enabled: true
  servers: {}
"#,
    );

    let empty = agent_config::McpConfig::default();
    let mgr = McpRuntimeManager::new(McpRuntimeConfig::from_yaml(&empty));
    let initial_fp = mgr.current_fingerprint().await;

    let shutdown = CancellationToken::new();
    spawn_mcp_config_watcher(
        dir.path().to_path_buf(),
        mgr.clone(),
        Vec::new(),
        None,
        Duration::from_millis(100),
        shutdown.clone(),
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Garbage → parse fails → watcher logs warn, no update.
    write_yaml(&path, "mcp: [this, is, not: a, map");

    tokio::time::sleep(Duration::from_millis(800)).await;
    let fp_after = mgr.current_fingerprint().await;
    assert_eq!(
        fp_after, initial_fp,
        "fingerprint must not change on invalid yaml"
    );
    shutdown.cancel();
}

#[tokio::test]
async fn reload_on_extensions_enable_disable_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mcp_path = dir.path().join(MCP_YAML_FILENAME);
    let ext_path = dir.path().join(EXTENSIONS_YAML_FILENAME);
    let ext_root = dir.path().join("extensions").join("weather");
    let search_path = dir.path().join("extensions");

    write_yaml(
        &mcp_path,
        r#"mcp:
  enabled: true
  servers: {}
"#,
    );
    write_manifest(
        &ext_root.join("plugin.toml"),
        r#"[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.local]
transport = "stdio"
command = "/bin/true"
"#,
    );
    write_yaml(
        &ext_path,
        &format!(
            r#"extensions:
  enabled: true
  search_paths:
    - "{}"
  disabled:
    - "weather"
  allowlist: []
  ignore_dirs: []
  max_depth: 3
"#,
            search_path.display()
        ),
    );

    let empty = agent_config::McpConfig::default();
    let mgr = McpRuntimeManager::new(McpRuntimeConfig::from_yaml(&empty));
    let initial_fp = mgr.current_fingerprint().await;

    let shutdown = CancellationToken::new();
    spawn_mcp_config_watcher(
        dir.path().to_path_buf(),
        mgr.clone(),
        Vec::new(),
        None,
        Duration::from_millis(100),
        shutdown.clone(),
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Enable extension MCP declarations at runtime.
    write_yaml(
        &ext_path,
        &format!(
            r#"extensions:
  enabled: true
  search_paths:
    - "{}"
  disabled: []
  allowlist: []
  ignore_dirs: []
  max_depth: 3
"#,
            search_path.display()
        ),
    );

    let enabled_fp = wait_for_fingerprint_change(&mgr, &initial_fp, Duration::from_secs(3))
        .await
        .expect("fingerprint did not change after extensions.yaml edit");
    assert_ne!(enabled_fp, initial_fp);
    shutdown.cancel();
}
