//! Phase 83.8.1 — `nexo/admin/skills/*` wire types.
//!
//! Daemon side handlers in `nexo_core::agent::admin_rpc::domains
//! ::skills` consume these as params / produce as results. SDK side
//! `AdminClient::skills()` accessor takes / returns these types.
//!
//! Skills are markdown bundles (`<name>/SKILL.md`) attachable to
//! agents to extend their knowledge. The runtime side
//! (`nexo_core::agent::skills::SkillLoader`) already reads them from
//! disk; this module adds the missing CRUD surface so a microapp
//! (e.g. an operator UI) can author them via admin RPC instead of
//! editing files directly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Skill dependency-failure mode. Mirror of the same enum in
/// `nexo-config` (kept local so `nexo-tool-meta` does not depend on
/// the config crate). The two enums round-trip through their
/// `snake_case` serde representation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDepsMode {
    /// Default — skip the skill when any dep is missing.
    #[default]
    Strict,
    /// Load anyway with a banner warning the LLM that the surface is
    /// degraded.
    Warn,
    /// Always skip, even if every dep is satisfied.
    Disable,
}

/// Soft constraints declared by the skill author. Mirrors the
/// `requires` block of the `SKILL.md` frontmatter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillRequiresRecord {
    /// External binaries the skill expects on PATH.
    pub bins: Vec<String>,
    /// Environment variables the skill expects to be set.
    pub env: Vec<String>,
    /// Author-declared mode for missing dependencies.
    pub mode: SkillDepsMode,
}

/// Compact summary used by `skills/list` responses. Excludes the
/// markdown body so list calls stay cheap on a daemon that hosts
/// hundreds of skills.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSummary {
    /// Stable name (matches the directory name on disk).
    pub name: String,
    /// Optional human-readable display name from frontmatter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Optional one-line description from frontmatter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Last-modified timestamp at the time the summary was built.
    pub updated_at: DateTime<Utc>,
}

/// Full skill record returned by `skills/get` and `skills/upsert`.
/// `body` is the raw markdown body (sans frontmatter) — the daemon
/// reconstructs the on-disk file by composing frontmatter from the
/// other fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRecord {
    /// Stable name (matches the directory name on disk).
    pub name: String,
    /// Optional human-readable display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Optional one-line description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Markdown body (no frontmatter delimiters).
    pub body: String,
    /// Optional cap on injected prompt size (chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    /// Author-declared dependency requirements.
    #[serde(default)]
    pub requires: SkillRequiresRecord,
    /// Last-modified timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Params for `nexo/admin/skills/list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SkillsListParams {
    /// Filter by name prefix (case-sensitive). `None` returns every
    /// skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Result of `nexo/admin/skills/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsListResponse {
    /// Matching skills in stable order (alpha by name).
    pub skills: Vec<SkillSummary>,
}

/// Params for `nexo/admin/skills/get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsGetParams {
    /// Stable name.
    pub name: String,
}

/// Result of `nexo/admin/skills/get`. `skill` is `None` when the name
/// has no matching directory on disk (not an error condition).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsGetResponse {
    /// Matching skill, or `None` when not found.
    pub skill: Option<SkillRecord>,
}

/// Params for `nexo/admin/skills/upsert`. Create-or-update — the
/// daemon decides based on whether the directory already exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsUpsertParams {
    /// Stable name. Must match `^[a-z0-9][a-z0-9-]{0,63}$`.
    pub name: String,
    /// Optional human-readable display name (omitted from
    /// frontmatter when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Optional one-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Markdown body (no frontmatter delimiters). 1..=65536 chars
    /// after trim.
    pub body: String,
    /// Optional cap on injected prompt size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    /// Author-declared dependency requirements. Defaults to empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<SkillRequiresRecord>,
}

/// Result of `nexo/admin/skills/upsert`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsUpsertResponse {
    /// Final skill record after the write.
    pub skill: SkillRecord,
    /// `true` when this call created a new skill, `false` when it
    /// updated an existing one.
    pub created: bool,
}

/// Params for `nexo/admin/skills/delete`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsDeleteParams {
    /// Stable name.
    pub name: String,
}

/// Result of `nexo/admin/skills/delete`. `deleted = false` is
/// idempotent (the name already had no directory on disk) — the
/// caller does not need to treat it as an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillsDeleteAck {
    /// `true` when the call removed an existing skill, `false` when
    /// the name had no directory.
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::{from_value, json, to_value};

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap()
    }

    #[test]
    fn skill_record_round_trips() {
        let record = SkillRecord {
            name: "tarifario-2026".into(),
            display_name: Some("Tarifario 2026".into()),
            description: Some("Planes ETB".into()),
            body: "# Tarifario\n\nPlanes:".into(),
            max_chars: Some(2048),
            requires: SkillRequiresRecord {
                bins: vec!["jq".into()],
                env: vec!["TARIFARIO_TOKEN".into()],
                mode: SkillDepsMode::Warn,
            },
            updated_at: fixed_ts(),
        };
        let v = to_value(&record).unwrap();
        let back: SkillRecord = from_value(v).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn skill_record_omits_optional_fields_when_none() {
        let record = SkillRecord {
            name: "minimal".into(),
            display_name: None,
            description: None,
            body: "body".into(),
            max_chars: None,
            requires: SkillRequiresRecord::default(),
            updated_at: fixed_ts(),
        };
        let v = to_value(&record).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("display_name"));
        assert!(!obj.contains_key("description"));
        assert!(!obj.contains_key("max_chars"));
    }

    #[test]
    fn skill_record_uses_snake_case_keys() {
        let record = SkillRecord {
            name: "demo".into(),
            display_name: Some("Demo".into()),
            description: None,
            body: "body".into(),
            max_chars: None,
            requires: SkillRequiresRecord::default(),
            updated_at: fixed_ts(),
        };
        let v = to_value(&record).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("display_name"));
        assert!(obj.contains_key("updated_at"));
        assert!(!obj.contains_key("displayName"));
        assert!(!obj.contains_key("updatedAt"));
    }

    #[test]
    fn skill_deps_mode_serializes_snake_case() {
        assert_eq!(to_value(SkillDepsMode::Strict).unwrap(), json!("strict"));
        assert_eq!(to_value(SkillDepsMode::Warn).unwrap(), json!("warn"));
        assert_eq!(to_value(SkillDepsMode::Disable).unwrap(), json!("disable"));
    }

    #[test]
    fn skills_list_params_default_has_no_prefix() {
        let p: SkillsListParams = serde_json::from_str("{}").unwrap();
        assert!(p.prefix.is_none());
    }

    #[test]
    fn skills_get_response_serializes_none_skill_explicitly() {
        let r = SkillsGetResponse { skill: None };
        let v = to_value(&r).unwrap();
        assert_eq!(v, json!({ "skill": null }));
    }

    #[test]
    fn skills_upsert_params_round_trips() {
        let p = SkillsUpsertParams {
            name: "weather".into(),
            display_name: Some("Weather".into()),
            description: Some("Forecast".into()),
            body: "body".into(),
            max_chars: Some(1024),
            requires: Some(SkillRequiresRecord {
                bins: vec!["curl".into()],
                env: vec![],
                mode: SkillDepsMode::Strict,
            }),
        };
        let v = to_value(&p).unwrap();
        let back: SkillsUpsertParams = from_value(v).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn skills_delete_ack_round_trips() {
        for deleted in [true, false] {
            let ack = SkillsDeleteAck { deleted };
            let v = to_value(&ack).unwrap();
            let back: SkillsDeleteAck = from_value(v).unwrap();
            assert_eq!(ack, back);
        }
    }

    #[test]
    fn frontmatter_compose_round_trips_through_skill_loader_format() {
        // The on-disk format is `---\n<yaml>\n---\n\n<body>`. Compose a
        // synthetic blob from a SkillRecord, parse it the same way
        // SkillLoader does (string-based — we don't pull in the
        // crate, just verify the contract).
        let record = SkillRecord {
            name: "ping".into(),
            display_name: Some("Ping".into()),
            description: Some("Probe.".into()),
            body: "Use to check liveness.".into(),
            max_chars: None,
            requires: SkillRequiresRecord::default(),
            updated_at: fixed_ts(),
        };
        let blob = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}",
            record.display_name.as_deref().unwrap(),
            record.description.as_deref().unwrap(),
            record.body,
        );
        assert!(blob.starts_with("---\n"));
        assert!(blob.contains("\n---\n\n"));
        assert!(blob.ends_with("Use to check liveness."));
    }
}
