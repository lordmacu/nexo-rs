//! Phase 79.10.b — bridge between nexo-core's `ConfigTool` traits
//! and the YAML helpers in this crate.
//!
//! `nexo-core` cannot depend on `nexo-setup` (the reverse already
//! exists, so a direct dep would form a cycle). The ConfigTool
//! defines `YamlPatchApplier` + `DenylistChecker` traits and lets
//! the call site inject concrete implementations. This module
//! provides those impls for production use:
//!
//! * [`SetupYamlPatchApplier`] forwards to
//!   [`crate::yaml_patch::{read_agent_field, apply_patch_with_denylist}`]
//!   plus a small `snapshot` / `restore` pair that reads + writes
//!   the agents YAML file in one shot.
//! * [`SetupDenylistChecker`] wraps
//!   [`crate::capabilities::denylist_match`].
//!
//! Both gated by the `config-self-edit` feature (propagated from
//! nexo-core).

#![cfg(feature = "config-self-edit")]

use crate::capabilities;
use crate::yaml_patch::{
    apply_patch_with_denylist, read_agent_field, ActorInfo, ApplyError, YamlOp, YamlPatch,
};
use nexo_core::agent::config_tool::{
    DenylistChecker, PatchAppliedError, PatchInfo, PatchOp, YamlPatchApplier,
};
use std::path::PathBuf;

/// Production [`YamlPatchApplier`] bound to one agents YAML file.
pub struct SetupYamlPatchApplier {
    pub agents_yaml_path: PathBuf,
    /// Captured into the staging-file's `actor` block. The
    /// applier stays oblivious to who the actor is at apply time
    /// (audit log already has the row); the field is only kept
    /// here so the conversion `PatchInfo → YamlPatch` produces a
    /// valid YamlPatch envelope.
    pub binding_id: String,
}

impl SetupYamlPatchApplier {
    pub fn new(agents_yaml_path: PathBuf, binding_id: String) -> Self {
        Self {
            agents_yaml_path,
            binding_id,
        }
    }

    fn to_yaml_patch(&self, info: &PatchInfo) -> YamlPatch {
        let op = match &info.op {
            PatchOp::Upsert {
                agent_id,
                dotted,
                value,
            } => YamlOp::Upsert {
                agent_id: agent_id.clone(),
                dotted: dotted.clone(),
                value: value.clone(),
            },
            PatchOp::Remove { agent_id, dotted } => YamlOp::Remove {
                agent_id: agent_id.clone(),
                dotted: dotted.clone(),
            },
        };
        YamlPatch {
            patch_id: info.patch_id.clone(),
            binding_id: self.binding_id.clone(),
            agent_id: info.agent_id.clone(),
            created_at: info.created_at,
            expires_at: info.expires_at,
            actor: ActorInfo {
                agent_id: info.agent_id.clone(),
                binding_id: self.binding_id.clone(),
                channel: String::new(),
                account_id: String::new(),
                sender_id: String::new(),
            },
            justification: info.justification.clone(),
            op,
        }
    }
}

impl YamlPatchApplier for SetupYamlPatchApplier {
    fn read(
        &self,
        agent_id: &str,
        dotted: &str,
    ) -> Result<Option<serde_yaml::Value>, PatchAppliedError> {
        read_agent_field(&self.agents_yaml_path, agent_id, dotted)
            .map_err(|e| PatchAppliedError::Yaml(e.to_string()))
    }

    fn apply(&self, info: &PatchInfo) -> Result<(), PatchAppliedError> {
        let patch = self.to_yaml_patch(info);
        apply_patch_with_denylist(&self.agents_yaml_path, &patch).map_err(|e| match e {
            ApplyError::Forbidden(fk) => PatchAppliedError::Forbidden {
                path: fk.path,
                matched_glob: fk.matched_glob.to_string(),
            },
            ApplyError::Io(io) => PatchAppliedError::Io(io.to_string()),
            ApplyError::Yaml(s) => PatchAppliedError::Yaml(s),
        })
    }

    fn snapshot(&self) -> Result<Vec<u8>, PatchAppliedError> {
        std::fs::read(&self.agents_yaml_path).map_err(|e| {
            PatchAppliedError::Io(format!(
                "snapshot read `{}`: {e}",
                self.agents_yaml_path.display()
            ))
        })
    }

    fn restore(&self, snapshot: &[u8]) -> Result<(), PatchAppliedError> {
        std::fs::write(&self.agents_yaml_path, snapshot).map_err(|e| {
            PatchAppliedError::Io(format!(
                "restore write `{}`: {e}",
                self.agents_yaml_path.display()
            ))
        })
    }
}

/// Production [`DenylistChecker`] forwarding to
/// [`crate::capabilities::denylist_match`].
pub struct SetupDenylistChecker;

impl DenylistChecker for SetupDenylistChecker {
    fn check(&self, dotted: &str) -> Option<&'static str> {
        capabilities::denylist_match(dotted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("agents.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            br#"
agents:
  - id: cody
    model:
      provider: anthropic
      model: claude-sonnet-4-6
    language: en
"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn applier_read_returns_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let applier = SetupYamlPatchApplier::new(path, "wa:default".into());
        let v = applier.read("cody", "model.model").unwrap().unwrap();
        assert_eq!(v, serde_yaml::Value::String("claude-sonnet-4-6".into()));
    }

    #[test]
    fn applier_apply_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let applier = SetupYamlPatchApplier::new(path, "wa:default".into());

        let info = PatchInfo {
            patch_id: "01J7".into(),
            binding_id: "wa:default".into(),
            agent_id: "cody".into(),
            created_at: 0,
            expires_at: 0,
            justification: "test".into(),
            op: PatchOp::Upsert {
                agent_id: "cody".into(),
                dotted: "model.model".into(),
                value: serde_yaml::Value::String("claude-opus-4-7".into()),
            },
        };
        applier.apply(&info).unwrap();

        let v = applier.read("cody", "model.model").unwrap().unwrap();
        assert_eq!(v, serde_yaml::Value::String("claude-opus-4-7".into()));
    }

    #[test]
    fn applier_snapshot_restore_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let applier = SetupYamlPatchApplier::new(path.clone(), "wa:default".into());

        let original = applier.snapshot().unwrap();
        // Modify the file out-of-band.
        std::fs::write(&path, b"agents: []\n").unwrap();
        // Restore.
        applier.restore(&original).unwrap();
        let v = applier.read("cody", "language").unwrap().unwrap();
        assert_eq!(v, serde_yaml::Value::String("en".into()));
    }

    #[test]
    fn applier_apply_blocks_denied_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let applier = SetupYamlPatchApplier::new(path, "wa:default".into());

        let info = PatchInfo {
            patch_id: "01J7".into(),
            binding_id: "wa:default".into(),
            agent_id: "cody".into(),
            created_at: 0,
            expires_at: 0,
            justification: "test".into(),
            op: PatchOp::Upsert {
                agent_id: "cody".into(),
                dotted: "pairing.session_token".into(),
                value: serde_yaml::Value::String("sneaky".into()),
            },
        };
        let err = applier.apply(&info).unwrap_err();
        match err {
            PatchAppliedError::Forbidden { matched_glob, path } => {
                assert_eq!(path, "pairing.session_token");
                assert!(matched_glob == "pairing.*" || matched_glob == "*_token");
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn denylist_checker_matches_pairing() {
        let c = SetupDenylistChecker;
        assert!(c.check("pairing.x").is_some());
        assert!(c.check("model.model").is_none());
    }
}
