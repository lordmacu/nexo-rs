//! Verifies the operator-facing `config/driver/claude.yaml` reference
//! deserializes cleanly into `ClaudeConfig`.

use std::path::PathBuf;

use nexo_driver_claude::ClaudeConfig;

#[test]
fn reference_yaml_parses() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/driver/claude.yaml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let cfg: ClaudeConfig = serde_yaml::from_str(&raw).unwrap();
    assert!(cfg.binary.is_some() || cfg.binary.is_none()); // smoke
    assert!(cfg.turn_timeout.as_secs() > 0);
    assert!(cfg.forced_kill_after.as_secs() > 0);
}
