//! Phase 12.7 — integration: extension manifest declares MCP servers,
//! discovery surfaces them, runtime config merges them namespaced.

use std::fs;
use std::io::Write;
use std::path::Path;

use agent_extensions::{collect_mcp_declarations, ExtensionDiscovery};
use agent_mcp::runtime_config::{ExtensionServerDecl, McpRuntimeConfig, McpServerRuntimeConfig};

fn write_manifest(dir: &Path, body: &str) {
    fs::create_dir_all(dir).unwrap();
    let mut f = fs::File::create(dir.join("plugin.toml")).unwrap();
    f.write_all(body.as_bytes()).unwrap();
}

fn discovery_for(root: &Path) -> ExtensionDiscovery {
    ExtensionDiscovery::new(vec![root.to_path_buf()], vec![], vec![], vec![], 3)
}

#[test]
fn discovery_collects_mcp_declarations() {
    let td = tempfile::tempdir().unwrap();

    // Candidate A — declares two MCP servers.
    write_manifest(
        &td.path().join("weather"),
        r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.api]
transport = "streamable_http"
url = "https://example.com/mcp"

[mcp_servers.local]
transport = "stdio"
command = "${EXTENSION_ROOT}/bin/geo"
"#,
    );

    // Candidate B — no mcp_servers section.
    write_manifest(
        &td.path().join("nothing"),
        r#"
[plugin]
id = "nothing"
version = "0.1.0"

[capabilities]
tools = ["x"]

[transport]
kind = "stdio"
command = "/bin/true"
"#,
    );

    let report = discovery_for(td.path()).discover();
    assert_eq!(report.candidates.len(), 2);
    let decls = collect_mcp_declarations(&report, &[]);
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].ext_id, "weather");
    assert_eq!(decls[0].servers.len(), 2);
}

#[test]
fn disabled_extensions_are_skipped() {
    let td = tempfile::tempdir().unwrap();
    write_manifest(
        &td.path().join("weather"),
        r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.api]
transport = "streamable_http"
url = "https://example.com/mcp"
"#,
    );

    let report = discovery_for(td.path()).discover();
    let decls = collect_mcp_declarations(&report, &["weather".to_string()]);
    assert!(decls.is_empty());
}

#[test]
fn from_yaml_with_extensions_expands_root_and_namespaces() {
    let td = tempfile::tempdir().unwrap();
    write_manifest(
        &td.path().join("weather"),
        r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["ping"]

[transport]
kind = "stdio"
command = "/bin/true"

[mcp_servers.local-geo]
transport = "stdio"
command = "${EXTENSION_ROOT}/bin/geo"
args = ["--db", "${EXTENSION_ROOT}/data/cities.db"]
"#,
    );

    let report = discovery_for(td.path()).discover();
    let ext_decls = collect_mcp_declarations(&report, &[]);
    // bridge agent-extensions ExtensionMcpDecl → agent-mcp ExtensionServerDecl
    let decls: Vec<ExtensionServerDecl> = ext_decls
        .into_iter()
        .map(|d| ExtensionServerDecl {
            ext_id: d.ext_id,
            ext_version: d.ext_version,
            ext_root: d.ext_root,
            servers: d.servers,
        })
        .collect();

    let yaml = agent_config::McpConfig::default();
    let rt = McpRuntimeConfig::from_yaml_with_extensions(&yaml, &decls);
    assert_eq!(rt.servers.len(), 1);
    let entry = &rt.servers[0];
    match entry {
        McpServerRuntimeConfig::Stdio(cfg) => {
            assert_eq!(cfg.name, "weather.local-geo");
            let ext_root = td.path().join("weather");
            // Discovery canonicalizes; use the canonical form.
            let ext_root = std::fs::canonicalize(&ext_root).unwrap();
            let expected_cmd = format!("{}/bin/geo", ext_root.display());
            assert_eq!(cfg.command, expected_cmd);
            assert_eq!(
                cfg.args[1],
                format!("{}/data/cities.db", ext_root.display())
            );
        }
        _ => panic!("expected stdio"),
    }
}
