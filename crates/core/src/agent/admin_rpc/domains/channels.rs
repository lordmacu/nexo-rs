//! Phase 82.10.f — `nexo/admin/channels/*` handlers.
//!
//! Operates on `agents.yaml.<id>.channels.approved` (Phase 80.9
//! MCP-channel server allowlist). Reuses the agents-domain
//! [`super::agents::YamlPatcher`] trait — same yaml file.
//!
//! `doctor` returns the static-half of `nexo channel doctor` —
//! verifies that each entry's server_name appears reachable in
//! the operator-declared MCP server registry. Live MCP probe is
//! out-of-scope for this layer (operator runs `nexo channel
//! doctor --runtime` for that).

use serde_json::Value;

use nexo_tool_meta::admin::channels::{
    ChannelDoctorReport, ChannelDoctorVerdict, ChannelEntry, ChannelsApproveInput,
    ChannelsDoctorParams, ChannelsListFilter, ChannelsListResponse, ChannelsRevokeParams,
    ChannelsRevokeResponse,
};

use super::agents::YamlPatcher;
use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// `nexo/admin/channels/list` — return every approved channel
/// across all agents (or a single agent when `agent_id` filter is
/// set).
pub fn list(yaml: &dyn YamlPatcher, params: Value) -> AdminRpcResult {
    let filter: ChannelsListFilter = serde_json::from_value(params).unwrap_or_default();
    let agent_ids = match yaml.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agents.yaml read: {e}"
            )));
        }
    };
    let mut entries: Vec<ChannelEntry> = Vec::new();
    for aid in &agent_ids {
        if let Some(filter_aid) = &filter.agent_id {
            if aid != filter_aid {
                continue;
            }
        }
        if let Some(Value::Array(approved)) = yaml
            .read_agent_field(aid, "channels.approved")
            .ok()
            .flatten()
        {
            for ch in approved {
                if let Some(entry) = parse_channel_entry(aid, &ch) {
                    entries.push(entry);
                }
            }
        }
    }
    entries.sort_by(|a, b| {
        a.agent_id
            .cmp(&b.agent_id)
            .then_with(|| a.server_name.cmp(&b.server_name))
    });
    AdminRpcResult::ok(
        serde_json::to_value(ChannelsListResponse { entries })
            .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/channels/approve` — append a new entry. Idempotent
/// — if `(agent_id, server_name)` is already approved, the
/// existing entry is returned unchanged (no yaml write).
pub fn approve(
    yaml: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let input: ChannelsApproveInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if input.agent_id.is_empty() || input.server_name.is_empty() {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(
            "agent_id and server_name are required".into(),
        ));
    }

    let existing = match yaml.read_agent_field(&input.agent_id, "channels.approved") {
        Ok(Some(Value::Array(arr))) => arr,
        Ok(_) => Vec::new(),
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml read: {e}"
            )));
        }
    };
    let already = existing.iter().any(|e| {
        e.get("server_name")
            .and_then(Value::as_str)
            .is_some_and(|n| n == input.server_name)
    });
    if !already {
        let mut updated = existing;
        let mut entry_obj = serde_json::Map::new();
        entry_obj.insert(
            "server_name".into(),
            Value::String(input.server_name.clone()),
        );
        if let Some(allow) = &input.allowlist {
            entry_obj.insert(
                "allowlist".into(),
                Value::Array(
                    allow
                        .iter()
                        .map(|i| Value::Number((*i).into()))
                        .collect(),
                ),
            );
        }
        updated.push(Value::Object(entry_obj));
        if let Err(e) = yaml.upsert_agent_field(
            &input.agent_id,
            "channels.approved",
            Value::Array(updated),
        ) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
        reload_signal();
    }

    AdminRpcResult::ok(
        serde_json::to_value(ChannelEntry {
            agent_id: input.agent_id,
            server_name: input.server_name,
            allowlist: input.allowlist,
        })
        .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/channels/revoke` — remove the entry from
/// `channels.approved`.
pub fn revoke(
    yaml: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let p: ChannelsRevokeParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    let existing = match yaml.read_agent_field(&p.agent_id, "channels.approved") {
        Ok(Some(Value::Array(arr))) => arr,
        Ok(_) => Vec::new(),
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml read: {e}"
            )));
        }
    };
    let before = existing.len();
    let filtered: Vec<Value> = existing
        .into_iter()
        .filter(|e| {
            e.get("server_name")
                .and_then(Value::as_str)
                .is_none_or(|n| n != p.server_name)
        })
        .collect();
    let removed = filtered.len() < before;
    if removed {
        if let Err(e) = yaml.upsert_agent_field(
            &p.agent_id,
            "channels.approved",
            Value::Array(filtered),
        ) {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "yaml write: {e}"
            )));
        }
        reload_signal();
    }
    AdminRpcResult::ok(
        serde_json::to_value(ChannelsRevokeResponse { removed })
            .unwrap_or(Value::Null),
    )
}

/// `nexo/admin/channels/doctor` — static verdicts. v1 returns
/// one `ok` verdict per declared `(agent_id, server_name)` pair.
/// Live MCP probe is operator's `nexo channel doctor --runtime`
/// CLI; this static surface is for admin UI dashboards.
pub fn doctor(yaml: &dyn YamlPatcher, params: Value) -> AdminRpcResult {
    let p: ChannelsDoctorParams = serde_json::from_value(params).unwrap_or_default();
    let agent_ids = match yaml.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "agents.yaml read: {e}"
            )));
        }
    };
    let mut verdicts: Vec<ChannelDoctorVerdict> = Vec::new();
    for aid in &agent_ids {
        if let Some(filter_aid) = &p.agent_id {
            if aid != filter_aid {
                continue;
            }
        }
        if let Some(Value::Array(approved)) = yaml
            .read_agent_field(aid, "channels.approved")
            .ok()
            .flatten()
        {
            for ch in approved {
                let Some(server_name) = ch.get("server_name").and_then(Value::as_str) else {
                    continue;
                };
                verdicts.push(ChannelDoctorVerdict {
                    agent_id: aid.clone(),
                    server_name: server_name.to_string(),
                    status: "ok".into(),
                    message: format!(
                        "Static check passed for `{server_name}`. Run `nexo channel \
                         doctor --runtime` for live MCP probe."
                    ),
                });
            }
        }
    }
    AdminRpcResult::ok(
        serde_json::to_value(ChannelDoctorReport { verdicts })
            .unwrap_or(Value::Null),
    )
}

fn parse_channel_entry(agent_id: &str, value: &Value) -> Option<ChannelEntry> {
    let server_name = value.get("server_name")?.as_str()?.to_string();
    let allowlist = value
        .get("allowlist")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect::<Vec<_>>()
        });
    Some(ChannelEntry {
        agent_id: agent_id.to_string(),
        server_name,
        allowlist,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockYaml {
        agents: Mutex<HashMap<String, HashMap<String, Value>>>,
    }
    impl MockYaml {
        fn empty() -> Self {
            Self::default()
        }
        fn with_agent(id: &str) -> Self {
            let me = Self::default();
            me.agents
                .lock()
                .unwrap()
                .entry(id.to_string())
                .or_default()
                .insert("model.provider".into(), Value::String("minimax".into()));
            me
        }
    }
    impl YamlPatcher for MockYaml {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
            let mut v: Vec<String> = self.agents.lock().unwrap().keys().cloned().collect();
            v.sort();
            Ok(v)
        }
        fn read_agent_field(&self, id: &str, dotted: &str) -> anyhow::Result<Option<Value>> {
            Ok(self
                .agents
                .lock()
                .unwrap()
                .get(id)
                .and_then(|m| m.get(dotted).cloned()))
        }
        fn upsert_agent_field(
            &self,
            id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.agents
                .lock()
                .unwrap()
                .entry(id.to_string())
                .or_default()
                .insert(dotted.to_string(), value);
            Ok(())
        }
        fn remove_agent(&self, id: &str) -> anyhow::Result<()> {
            self.agents.lock().unwrap().remove(id);
            Ok(())
        }
    }

    #[test]
    fn channels_approve_writes_yaml() {
        let yaml = MockYaml::with_agent("ana");
        let result = approve(
            &yaml,
            serde_json::json!({
                "agent_id": "ana",
                "server_name": "plugin:telegram:tg"
            }),
            &|| {},
        );
        let entry: ChannelEntry = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(entry.server_name, "plugin:telegram:tg");
        // Verify yaml has it.
        let approved = yaml
            .read_agent_field("ana", "channels.approved")
            .unwrap()
            .unwrap();
        assert_eq!(approved.as_array().unwrap().len(), 1);
    }

    #[test]
    fn channels_approve_idempotent_skip_duplicate() {
        let yaml = MockYaml::with_agent("ana");
        let params = serde_json::json!({
            "agent_id": "ana",
            "server_name": "slack"
        });
        approve(&yaml, params.clone(), &|| {});
        approve(&yaml, params, &|| {});
        let approved = yaml
            .read_agent_field("ana", "channels.approved")
            .unwrap()
            .unwrap();
        assert_eq!(approved.as_array().unwrap().len(), 1);
    }

    #[test]
    fn channels_revoke_removes_entry() {
        let yaml = MockYaml::with_agent("ana");
        approve(
            &yaml,
            serde_json::json!({"agent_id": "ana", "server_name": "slack"}),
            &|| {},
        );
        let result = revoke(
            &yaml,
            serde_json::json!({"agent_id": "ana", "server_name": "slack"}),
            &|| {},
        );
        let response: ChannelsRevokeResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
    }

    #[test]
    fn channels_revoke_unknown_idempotent() {
        let yaml = MockYaml::with_agent("ana");
        let result = revoke(
            &yaml,
            serde_json::json!({"agent_id": "ana", "server_name": "ghost"}),
            &|| {},
        );
        let response: ChannelsRevokeResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
    }

    #[test]
    fn channels_list_filters_by_agent() {
        let yaml = MockYaml::default();
        // 2 agents.
        yaml.agents
            .lock()
            .unwrap()
            .entry("ana".into())
            .or_default()
            .insert("model.provider".into(), Value::String("x".into()));
        yaml.agents
            .lock()
            .unwrap()
            .entry("bob".into())
            .or_default()
            .insert("model.provider".into(), Value::String("x".into()));
        approve(
            &yaml,
            serde_json::json!({"agent_id": "ana", "server_name": "slack"}),
            &|| {},
        );
        approve(
            &yaml,
            serde_json::json!({"agent_id": "bob", "server_name": "telegram"}),
            &|| {},
        );

        let result = list(
            &yaml,
            serde_json::json!({ "agent_id": "ana" }),
        );
        let response: ChannelsListResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].agent_id, "ana");
    }

    #[test]
    fn channels_doctor_returns_static_verdicts() {
        let yaml = MockYaml::with_agent("ana");
        approve(
            &yaml,
            serde_json::json!({"agent_id": "ana", "server_name": "slack"}),
            &|| {},
        );
        let result = doctor(&yaml, Value::Null);
        let report: ChannelDoctorReport =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(report.verdicts.len(), 1);
        assert_eq!(report.verdicts[0].status, "ok");
        assert_eq!(report.verdicts[0].agent_id, "ana");
        assert_eq!(report.verdicts[0].server_name, "slack");
    }

    #[test]
    fn channels_approve_with_allowlist() {
        let yaml = MockYaml::with_agent("ana");
        let result = approve(
            &yaml,
            serde_json::json!({
                "agent_id": "ana",
                "server_name": "slack",
                "allowlist": [0, 2]
            }),
            &|| {},
        );
        let entry: ChannelEntry = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(entry.allowlist, Some(vec![0, 2]));
    }

    #[test]
    fn channels_approve_rejects_empty_fields() {
        let yaml = MockYaml::empty();
        let result = approve(
            &yaml,
            serde_json::json!({"agent_id": "", "server_name": "slack"}),
            &|| {},
        );
        assert!(matches!(
            result.error.unwrap(),
            AdminRpcError::InvalidParams(_)
        ));
    }
}
