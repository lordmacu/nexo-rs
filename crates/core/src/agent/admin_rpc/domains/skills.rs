//! Phase 83.8.2 — `nexo/admin/skills/*` handlers.
//!
//! CRUD surface for markdown skills (`<root>/<name>/SKILL.md`).
//! The runtime side
//! (`crate::agent::skills::SkillLoader`) already reads them from
//! disk; this domain adds the missing write side so a microapp
//! (e.g. an operator UI) can author them via admin RPC.
//!
//! Backed by a [`SkillsStore`] trait so this crate stays
//! cycle-free vs `nexo-setup` (which holds the concrete
//! `FsSkillsStore` adapter introduced in 83.8.3).

use async_trait::async_trait;
use serde_json::Value;

use nexo_tool_meta::admin::skills::{
    SkillRecord, SkillSummary, SkillsDeleteAck, SkillsDeleteParams, SkillsGetParams,
    SkillsGetResponse, SkillsListParams, SkillsListResponse, SkillsUpsertParams,
    SkillsUpsertResponse,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Validation cap on skill body size — keeps a hostile or sloppy
/// microapp from streaming a multi-MiB blob into the prompt.
pub const MAX_SKILL_BODY_BYTES: usize = 64 * 1024;

/// Write-side surface the skills admin handlers consume.
/// Production wires `nexo_setup::admin_adapters::FsSkillsStore`
/// against an on-disk `<root>/<name>/SKILL.md` layout. Tests
/// inject in-memory mocks.
#[async_trait]
pub trait SkillsStore: Send + Sync + std::fmt::Debug {
    /// List skills, optionally filtered by name prefix. Returns an
    /// empty `Vec` (NOT an error) when the store is empty.
    async fn list(&self, prefix: Option<&str>) -> anyhow::Result<Vec<SkillSummary>>;
    /// Read one skill. Returns `Ok(None)` when the name has no
    /// matching directory (NOT an error).
    async fn get(&self, name: &str) -> anyhow::Result<Option<SkillRecord>>;
    /// Create or update one skill. The boolean is `true` when the
    /// call created a new directory, `false` when it overwrote an
    /// existing one (idempotent retry).
    async fn upsert(&self, params: SkillsUpsertParams) -> anyhow::Result<(SkillRecord, bool)>;
    /// Delete one skill. Returns `Ok(false)` when the name had no
    /// matching directory (idempotent — not an error).
    async fn delete(&self, name: &str) -> anyhow::Result<bool>;
}

/// Reject names that would escape the skills root or otherwise
/// produce surprising on-disk layouts. Mirrors the production
/// adapter contract; surfaced here so handler-level errors come
/// back as `-32602 invalid_params` regardless of which adapter is
/// installed.
pub fn validate_skill_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("name is empty");
    }
    if name.len() > 64 {
        return Err("name longer than 64 chars");
    }
    let bytes = name.as_bytes();
    let first = bytes[0];
    let starts_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
    if !starts_ok {
        return Err("name must start with [a-z0-9]");
    }
    for &b in bytes {
        let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
        if !ok {
            return Err("name must match [a-z0-9-]");
        }
    }
    Ok(())
}

fn validate_body(body: &str) -> Result<(), &'static str> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("body is empty");
    }
    if body.len() > MAX_SKILL_BODY_BYTES {
        return Err("body exceeds 64 KiB cap");
    }
    Ok(())
}

/// `nexo/admin/skills/list` — list skills with optional prefix filter.
pub async fn list(store: &dyn SkillsStore, params: Value) -> AdminRpcResult {
    let p: SkillsListParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    let skills = match store.list(p.prefix.as_deref()).await {
        Ok(v) => v,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "skills_store.list: {e}"
            )))
        }
    };
    let response = SkillsListResponse { skills };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/skills/get` — read one skill. Unknown name returns
/// `{ "skill": null }`, NOT an error.
pub async fn get(store: &dyn SkillsStore, params: Value) -> AdminRpcResult {
    let p: SkillsGetParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_skill_name(&p.name) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    let skill = match store.get(&p.name).await {
        Ok(s) => s,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "skills_store.get: {e}"
            )))
        }
    };
    let response = SkillsGetResponse { skill };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/skills/upsert` — create or update one skill.
pub async fn upsert(store: &dyn SkillsStore, params: Value) -> AdminRpcResult {
    let p: SkillsUpsertParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_skill_name(&p.name) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    if let Err(msg) = validate_body(&p.body) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    let (skill, created) = match store.upsert(p).await {
        Ok(pair) => pair,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "skills_store.upsert: {e}"
            )))
        }
    };
    let response = SkillsUpsertResponse { skill, created };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/skills/delete` — remove one skill. Idempotent: a
/// missing name returns `deleted: false` rather than an error.
pub async fn delete(store: &dyn SkillsStore, params: Value) -> AdminRpcResult {
    let p: SkillsDeleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string())),
    };
    if let Err(msg) = validate_skill_name(&p.name) {
        return AdminRpcResult::err(AdminRpcError::InvalidParams(msg.into()));
    }
    let deleted = match store.delete(&p.name).await {
        Ok(b) => b,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "skills_store.delete: {e}"
            )))
        }
    };
    let ack = SkillsDeleteAck { deleted };
    AdminRpcResult::ok(serde_json::to_value(ack).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::sync::Mutex;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[derive(Debug, Default, Clone)]
    struct InMemoryStore {
        rows: Arc<Mutex<BTreeMap<String, SkillRecord>>>,
    }

    #[async_trait]
    impl SkillsStore for InMemoryStore {
        async fn list(&self, prefix: Option<&str>) -> anyhow::Result<Vec<SkillSummary>> {
            let rows = self.rows.lock().unwrap();
            Ok(rows
                .iter()
                .filter(|(name, _)| match prefix {
                    Some(p) => name.starts_with(p),
                    None => true,
                })
                .map(|(_, r)| SkillSummary {
                    name: r.name.clone(),
                    display_name: r.display_name.clone(),
                    description: r.description.clone(),
                    updated_at: r.updated_at,
                })
                .collect())
        }
        async fn get(&self, name: &str) -> anyhow::Result<Option<SkillRecord>> {
            Ok(self.rows.lock().unwrap().get(name).cloned())
        }
        async fn upsert(
            &self,
            params: SkillsUpsertParams,
        ) -> anyhow::Result<(SkillRecord, bool)> {
            let mut rows = self.rows.lock().unwrap();
            let created = !rows.contains_key(&params.name);
            let record = SkillRecord {
                name: params.name.clone(),
                display_name: params.display_name,
                description: params.description,
                body: params.body,
                max_chars: params.max_chars,
                requires: params.requires.unwrap_or_default(),
                updated_at: Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
            };
            rows.insert(params.name, record.clone());
            Ok((record, created))
        }
        async fn delete(&self, name: &str) -> anyhow::Result<bool> {
            Ok(self.rows.lock().unwrap().remove(name).is_some())
        }
    }

    fn store() -> Arc<InMemoryStore> {
        Arc::new(InMemoryStore::default())
    }

    #[tokio::test]
    async fn list_empty_returns_empty_vec() {
        let s = store();
        let r = list(s.as_ref(), json!({})).await;
        assert!(r.error.is_none());
        let v = r.result.unwrap();
        assert_eq!(v["skills"], json!([]));
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let s = store();
        let up = upsert(
            s.as_ref(),
            json!({
                "name": "weather",
                "display_name": "Weather",
                "description": "Forecasts.",
                "body": "Use for forecasts."
            }),
        )
        .await;
        assert!(up.error.is_none());
        let v = up.result.unwrap();
        assert_eq!(v["created"], json!(true));
        assert_eq!(v["skill"]["name"], json!("weather"));

        let g = get(s.as_ref(), json!({ "name": "weather" })).await;
        assert!(g.error.is_none());
        let v = g.result.unwrap();
        assert_eq!(v["skill"]["body"], json!("Use for forecasts."));
    }

    #[tokio::test]
    async fn upsert_twice_reports_created_false_second() {
        let s = store();
        let _ = upsert(s.as_ref(), json!({"name":"w","body":"a"})).await;
        let up2 = upsert(s.as_ref(), json!({"name":"w","body":"b"})).await;
        assert_eq!(up2.result.unwrap()["created"], json!(false));
    }

    #[tokio::test]
    async fn get_missing_returns_null_not_error() {
        let s = store();
        let g = get(s.as_ref(), json!({ "name": "absent" })).await;
        assert!(g.error.is_none());
        assert_eq!(g.result.unwrap(), json!({ "skill": null }));
    }

    #[tokio::test]
    async fn delete_missing_returns_false_not_error() {
        let s = store();
        let d = delete(s.as_ref(), json!({ "name": "absent" })).await;
        assert!(d.error.is_none());
        assert_eq!(d.result.unwrap()["deleted"], json!(false));
    }

    #[tokio::test]
    async fn invalid_name_rejected_with_invalid_params() {
        let s = store();
        let cases = vec!["", "ABC", "foo bar", "../etc/passwd", "foo/bar", "1foo--ok"];
        for name in cases {
            let r = get(s.as_ref(), json!({ "name": name })).await;
            // Last case "1foo--ok" is actually valid; assert pass.
            if name == "1foo--ok" {
                assert!(r.error.is_none(), "name `{name}` should be valid");
            } else {
                let e = r.error.expect("expected validation error");
                assert_eq!(e.code(), -32602, "name `{name}` should be invalid_params");
            }
        }
    }

    #[tokio::test]
    async fn body_too_large_rejected() {
        let s = store();
        let big = "x".repeat(MAX_SKILL_BODY_BYTES + 1);
        let r = upsert(
            s.as_ref(),
            json!({ "name": "big", "body": big }),
        )
        .await;
        let e = r.error.expect("body cap should reject");
        assert_eq!(e.code(), -32602);
    }

    #[tokio::test]
    async fn empty_body_rejected() {
        let s = store();
        let r = upsert(s.as_ref(), json!({ "name": "x", "body": "   " })).await;
        let e = r.error.expect("empty body should reject");
        assert_eq!(e.code(), -32602);
    }

    #[tokio::test]
    async fn list_prefix_filter() {
        let s = store();
        let _ = upsert(s.as_ref(), json!({"name":"alpha","body":"a"})).await;
        let _ = upsert(s.as_ref(), json!({"name":"beta","body":"b"})).await;
        let _ = upsert(s.as_ref(), json!({"name":"alpine","body":"c"})).await;
        let r = list(s.as_ref(), json!({"prefix":"alp"})).await;
        let v = r.result.unwrap();
        let names: Vec<&str> = v["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "alpine"]);
    }

    #[tokio::test]
    async fn delete_then_list_drops_record() {
        let s = store();
        let _ = upsert(s.as_ref(), json!({"name":"gone","body":"x"})).await;
        let _ = delete(s.as_ref(), json!({"name":"gone"})).await;
        let r = list(s.as_ref(), json!({})).await;
        assert_eq!(r.result.unwrap()["skills"], json!([]));
    }

    #[test]
    fn validate_skill_name_accepts_kebab_lowercase() {
        for name in ["weather", "lead-capture", "v1-2-3", "0test"] {
            assert!(validate_skill_name(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn validate_skill_name_rejects_dots_slashes_uppercase_underscores() {
        for name in ["", "Weather", "../escape", "foo/bar", "foo_bar", "foo.bar", "-foo"] {
            assert!(validate_skill_name(name).is_err(), "{name} should be invalid");
        }
    }
}
