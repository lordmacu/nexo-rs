//! Generate the `.nexo-mcp.json` Claude reads via `--mcp-config`.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::DriverError;

/// Write `<workspace>/.nexo-mcp.json` pointing Claude at our
/// permission server. Returns the absolute path of the file written.
///
/// Phase 73 — both `bin_path` and `socket_path` are canonicalised
/// to absolute form before serialisation. Claude CLI launches the
/// MCP server inside the worktree (the `--mcp-config` file is read
/// with cwd = worktree), so any relative path defined in
/// `config/driver/claude.yaml` (`./data/driver.sock`) would resolve
/// to `<worktree>/data/driver.sock` which does not exist. The
/// driver socket lives at `<daemon-cwd>/data/driver.sock`; the
/// .nexo-mcp.json must spell that out.
pub fn write_mcp_config(
    workspace: &Path,
    bin_path: &Path,
    socket_path: &Path,
) -> Result<PathBuf, DriverError> {
    let bin_abs = absolute_path(bin_path);
    let sock_abs = absolute_path(socket_path);
    // Phase 73 — config-key MUST match `serverInfo.name`
    // returned by the MCP server (`nexo-driver-permission`).
    // Claude Code 2.1 namespaces tools by
    // `mcp__<serverInfo.name>__<tool>` and resolves
    // `--permission-prompt-tool` against that prefix; if the
    // JSON config-key disagrees, Claude registers the server
    // (`status: connected`) but no tool ever lands in the
    // permission registry, surfacing as
    // "Available MCP tools: none".
    let cfg = json!({
        "mcpServers": {
            "nexo-driver-permission": {
                "command": bin_abs.to_string_lossy(),
                "args": ["--socket", sock_abs.to_string_lossy()],
                "env": {}
            }
        }
    });
    let path = workspace.join(".nexo-mcp.json");
    let pretty = serde_json::to_vec_pretty(&cfg)?;
    std::fs::write(&path, pretty)?;
    Ok(path)
}

/// Best-effort absolute form. `canonicalize` requires the file to
/// exist (true for the binary + the active socket); when it fails
/// we fall back to `<daemon-cwd>/<relative>` which is still better
/// than handing Claude a path it would resolve against the worktree.
fn absolute_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Ok(canon) = std::fs::canonicalize(p) {
        return canon;
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.join(p);
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_valid_json_with_paths() {
        let dir = tempfile::tempdir().unwrap();
        let bin = std::path::PathBuf::from("/usr/local/bin/nexo-driver-permission-mcp");
        let sock = std::path::PathBuf::from("/run/nexo-rs/driver.sock");
        let written = write_mcp_config(dir.path(), &bin, &sock).unwrap();
        assert!(written.is_file());
        let raw = std::fs::read_to_string(&written).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["mcpServers"]["nexo-driver-permission"]["command"],
            "/usr/local/bin/nexo-driver-permission-mcp"
        );
        let args = parsed["mcpServers"]["nexo-driver-permission"]["args"]
            .as_array()
            .unwrap();
        assert_eq!(args[0], "--socket");
        assert_eq!(args[1], "/run/nexo-rs/driver.sock");
    }
}
