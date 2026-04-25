//! Generate the `.nexo-mcp.json` Claude reads via `--mcp-config`.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::DriverError;

/// Write `<workspace>/.nexo-mcp.json` pointing Claude at our
/// permission server. Returns the absolute path of the file written.
pub fn write_mcp_config(
    workspace: &Path,
    bin_path: &Path,
    socket_path: &Path,
) -> Result<PathBuf, DriverError> {
    let cfg = json!({
        "mcpServers": {
            "nexo-driver": {
                "command": bin_path.to_string_lossy(),
                "args": ["--socket", socket_path.to_string_lossy()],
                "env": {}
            }
        }
    });
    let path = workspace.join(".nexo-mcp.json");
    let pretty = serde_json::to_vec_pretty(&cfg)?;
    std::fs::write(&path, pretty)?;
    Ok(path)
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
            parsed["mcpServers"]["nexo-driver"]["command"],
            "/usr/local/bin/nexo-driver-permission-mcp"
        );
        let args = parsed["mcpServers"]["nexo-driver"]["args"]
            .as_array()
            .unwrap();
        assert_eq!(args[0], "--socket");
        assert_eq!(args[1], "/run/nexo-rs/driver.sock");
    }
}
