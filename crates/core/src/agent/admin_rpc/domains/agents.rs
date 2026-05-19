//! `nexo/admin/agents/*` handlers.
//!
//! Yaml mutation is delegated to a [`YamlPatcher`] trait so this
//! crate stays cycle-free vs `nexo-setup` (which holds the
//! concrete `yaml_patch` impl + already depends on `nexo-core`).
//! Production wiring in `src/main.rs` constructs an adapter that
//! forwards each method to `nexo_setup::yaml_patch::*`.
//!
//! After successful mutation the handler calls the dispatcher's
//! reload signal to trigger config hot-reload so the running
//! runtime picks up the change without restart.

use serde_json::Value;

use nexo_tool_meta::admin::agents::{
    AgentDetail, AgentSummary, AgentUpsertInput, AgentsDeleteParams, AgentsDeleteResponse,
    AgentsGetParams, AgentsListFilter, AgentsListResponse, BindingSummary, HeartbeatWire, ModelRef,
};

use crate::agent::admin_rpc::dispatcher::{AdminRpcError, AdminRpcResult};

/// Yaml mutation surface the agents domain
/// handlers consume. Production impl wraps
/// `nexo_setup::yaml_patch`. Tests provide an in-memory mock.
pub trait YamlPatcher: Send + Sync {
    /// List every `agents.yaml.<id>` block in source order.
    fn list_agent_ids(&self) -> anyhow::Result<Vec<String>>;
    /// Read one dotted field (`model.provider`,
    /// `inbound_bindings`, …). `None` when the field is absent.
    fn read_agent_field(&self, agent_id: &str, dotted: &str) -> anyhow::Result<Option<Value>>;
    /// Upsert one dotted field. Atomic via temp+rename in the
    /// production impl.
    fn upsert_agent_field(&self, agent_id: &str, dotted: &str, value: Value) -> anyhow::Result<()>;
    /// Remove the entire `agents.yaml.<id>` block.
    fn remove_agent(&self, agent_id: &str) -> anyhow::Result<()>;
    /// Phase 97.UI — read the entire agent block as a JSON value.
    /// Powers `AgentDetail.raw_config` so admin UIs can render the
    /// long-tail capability gates (`config_tool`, `team`, `repl`,
    /// `proactive`, rate limits, `remote_triggers`, …) without
    /// every one needing a dotted-path read. Default impl returns
    /// `None` so legacy in-memory stubs in tests keep compiling;
    /// the production filesystem impl in nexo-setup overrides.
    fn read_agent_block(&self, _agent_id: &str) -> anyhow::Result<Option<Value>> {
        Ok(None)
    }
}

/// `nexo/admin/agents/list` — return agents matching `filter`.
pub fn list(patcher: &dyn YamlPatcher, params: Value) -> AdminRpcResult {
    let filter: AgentsListFilter = parse_or_default(params);
    let ids = match patcher.list_agent_ids() {
        Ok(ids) => ids,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!("yaml read failed: {e}")));
        }
    };

    let mut summaries: Vec<AgentSummary> = ids
        .into_iter()
        .filter_map(|id| read_summary(patcher, &id).ok().flatten())
        .filter(|s| !filter.active_only || s.active)
        .filter(|s| match &filter.plugin_filter {
            Some(p) => has_plugin_binding(patcher, &s.id, p),
            None => true,
        })
        .filter(|s| match &filter.tenant_id {
            // Multi-tenant filter. Defense-in-
            // depth: an agent without `tenant_id` is treated
            // as `None` and filtered out when caller requests
            // a specific tenant. Cross-tenant returns empty
            // (no leak of existence).
            Some(want) => agent_tenant_id(patcher, &s.id)
                .as_deref()
                .map(|got| got == want)
                .unwrap_or(false),
            None => true,
        })
        .collect();
    // Stable alpha order — operator UIs rely on it for diff
    // displays.
    summaries.sort_by(|a, b| a.id.cmp(&b.id));

    let response = AgentsListResponse { agents: summaries };
    AdminRpcResult::ok(serde_json::to_value(response).unwrap_or(Value::Null))
}

/// `nexo/admin/agents/get` — return full detail for one agent.
pub fn get(patcher: &dyn YamlPatcher, params: Value) -> AdminRpcResult {
    let p: AgentsGetParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };
    match read_detail(patcher, &p.agent_id) {
        Ok(Some(detail)) => AdminRpcResult::ok(serde_json::to_value(detail).unwrap_or(Value::Null)),
        Ok(None) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "not_found: agent `{}` not in yaml",
            p.agent_id
        ))),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("yaml read failed: {e}"))),
    }
}

/// Phase 81.31 — `nexo/admin/agents/get` with persona-locale
/// enrichment. When `snapshot` is `Some`, the response includes
/// `persona_locales` populated from
/// [`crate::agent::admin_rpc::domains::persona::PersonaSnapshotReader`].
/// `None` keeps legacy single-locale behavior identical to
/// [`get`].
pub async fn get_with_persona(
    patcher: &dyn YamlPatcher,
    snapshot: Option<&dyn super::persona::PersonaSnapshotReader>,
    params: Value,
) -> AdminRpcResult {
    let p: AgentsGetParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };
    let mut detail = match read_detail(patcher, &p.agent_id) {
        Ok(Some(d)) => d,
        Ok(None) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!(
                "not_found: agent `{}` not in yaml",
                p.agent_id
            )))
        }
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::Internal(format!("yaml read failed: {e}")))
        }
    };
    if let Some(reader) = snapshot {
        detail.persona_locales = reader.read_locales(&p.agent_id).await;
    }
    AdminRpcResult::ok(serde_json::to_value(detail).unwrap_or(Value::Null))
}

/// `nexo/admin/agents/upsert` — create or update an agent block.
pub fn upsert(
    patcher: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let mut input: AgentUpsertInput = match serde_json::from_value(params) {
        Ok(i) => i,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };

    // Resolve final agent id. `auto_id=true` enables the
    // wizard-friendly path: empty `id` triggers `agent_NNNNNN`
    // generation; a non-empty `id` that collides with an existing
    // agent gets an `_<epoch_ms>` suffix instead of being treated
    // as an upsert. Legacy callers leave `auto_id=false` and keep
    // the upsert-on-collision semantic.
    let existing_ids = patcher.list_agent_ids().unwrap_or_default();
    input.id = match resolve_agent_id(&input.id, input.auto_id, &existing_ids) {
        Ok(id) => id,
        Err(e) => return AdminRpcResult::err(e),
    };

    if let Err(e) = upsert_yaml(patcher, &input) {
        return AdminRpcResult::err(AdminRpcError::Internal(format!("yaml write failed: {e}")));
    }
    reload_signal();

    match read_detail(patcher, &input.id) {
        Ok(Some(d)) => AdminRpcResult::ok(serde_json::to_value(d).unwrap_or(Value::Null)),
        Ok(None) => AdminRpcResult::err(AdminRpcError::Internal(
            "post-upsert read returned None".into(),
        )),
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!(
            "post-upsert read failed: {e}"
        ))),
    }
}

/// `nexo/admin/agents/delete` — soft-delete then remove yaml block.
pub fn delete(
    patcher: &dyn YamlPatcher,
    params: Value,
    reload_signal: &dyn Fn(),
) -> AdminRpcResult {
    let p: AgentsDeleteParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return AdminRpcResult::err(AdminRpcError::InvalidParams(e.to_string()));
        }
    };

    let existed = matches!(
        patcher.list_agent_ids(),
        Ok(ids) if ids.iter().any(|id| id == &p.agent_id)
    );
    if !existed {
        // Idempotent — return removed=false, NOT an error.
        return AdminRpcResult::ok(
            serde_json::to_value(AgentsDeleteResponse { removed: false }).unwrap_or(Value::Null),
        );
    }

    match patcher.remove_agent(&p.agent_id) {
        Ok(()) => {
            reload_signal();
            AdminRpcResult::ok(
                serde_json::to_value(AgentsDeleteResponse { removed: true }).unwrap_or(Value::Null),
            )
        }
        Err(e) => AdminRpcResult::err(AdminRpcError::Internal(format!("yaml remove failed: {e}"))),
    }
}

// ── Internal helpers ────────────────────────────────────────────

fn parse_or_default<T: for<'de> serde::Deserialize<'de> + Default>(v: Value) -> T {
    serde_json::from_value(v).unwrap_or_default()
}

/// Resolves the final `id` an `agents/upsert` will commit. See
/// [`AgentUpsertInput::id`] docs for the matrix this implements.
///
/// Bounded 16-attempt re-roll guards against pathological test
/// stubs that always report "exists"; production collisions are
/// statistically negligible (1 in a million per attempt).
fn resolve_agent_id(
    input_id: &str,
    auto_id: bool,
    existing: &[String],
) -> Result<String, AdminRpcError> {
    if !auto_id {
        if input_id.is_empty() {
            return Err(AdminRpcError::InvalidParams(
                "id is required when auto_id=false".into(),
            ));
        }
        if !is_valid_agent_id(input_id) {
            return Err(AdminRpcError::InvalidParams(format!(
                "id `{input_id}` must match ^[a-z][a-z0-9_-]{{1,40}}$"
            )));
        }
        return Ok(input_id.to_string());
    }

    if input_id.is_empty() {
        for _ in 0..16 {
            let candidate = generate_random_agent_id();
            if !existing.iter().any(|e| e == &candidate) {
                return Ok(candidate);
            }
        }
        return Err(AdminRpcError::Internal(
            "could not allocate a unique agent id after 16 attempts".into(),
        ));
    }

    if !is_valid_agent_id(input_id) {
        return Err(AdminRpcError::InvalidParams(format!(
            "id `{input_id}` must match ^[a-z][a-z0-9_-]{{1,40}}$"
        )));
    }
    if !existing.iter().any(|e| e == input_id) {
        return Ok(input_id.to_string());
    }
    Ok(format!("{input_id}_{}", epoch_ms()))
}

fn is_valid_agent_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 41 {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn generate_random_agent_id() -> String {
    // No `rand` dep in nexo-core; epoch-nanos mod 1M gives a
    // 6-digit decimal that's effectively unique per call when
    // separated by at least one microsecond. The re-roll loop in
    // `resolve_agent_id` covers tight bursts that land in the
    // same nano-window.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("agent_{:06}", (nanos as u32) % 1_000_000)
}

fn epoch_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Read `agents.yaml.<id>.tenant_id`. Returns
/// `None` for legacy agents (no field) and on any read error
/// (defense-in-depth: fail closed for cross-tenant filters).
pub(crate) fn agent_tenant_id(patcher: &dyn YamlPatcher, agent_id: &str) -> Option<String> {
    match patcher
        .read_agent_field(agent_id, "tenant_id")
        .ok()
        .flatten()
    {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

fn read_summary(patcher: &dyn YamlPatcher, agent_id: &str) -> anyhow::Result<Option<AgentSummary>> {
    let provider = match patcher.read_agent_field(agent_id, "model.provider")? {
        Some(Value::String(s)) => s,
        _ => return Ok(None),
    };
    let active = match patcher.read_agent_field(agent_id, "active")? {
        Some(Value::Bool(b)) => b,
        // Default active=true when field absent (legacy yaml).
        _ => true,
    };
    let bindings_count = patcher
        .read_agent_field(agent_id, "inbound_bindings")?
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0);

    Ok(Some(AgentSummary {
        id: agent_id.to_string(),
        active,
        model_provider: provider,
        bindings_count,
    }))
}

fn has_plugin_binding(patcher: &dyn YamlPatcher, agent_id: &str, plugin: &str) -> bool {
    let Some(Value::Array(bindings)) = patcher
        .read_agent_field(agent_id, "inbound_bindings")
        .ok()
        .flatten()
    else {
        return false;
    };
    bindings.iter().any(|b| {
        b.get("plugin")
            .and_then(Value::as_str)
            .is_some_and(|p| p == plugin)
    })
}

fn read_detail(patcher: &dyn YamlPatcher, agent_id: &str) -> anyhow::Result<Option<AgentDetail>> {
    let Some(Value::String(provider)) = patcher.read_agent_field(agent_id, "model.provider")?
    else {
        return Ok(None);
    };
    let model = match patcher.read_agent_field(agent_id, "model.model")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let active = match patcher.read_agent_field(agent_id, "active")? {
        Some(Value::Bool(b)) => b,
        _ => true,
    };
    let allowed_tools: Vec<String> = match patcher.read_agent_field(agent_id, "allowed_tools")? {
        Some(Value::Array(arr)) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let system_prompt = match patcher.read_agent_field(agent_id, "system_prompt")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let language = match patcher.read_agent_field(agent_id, "language")? {
        Some(Value::String(s)) => Some(s),
        _ => None,
    };
    let workspace = match patcher.read_agent_field(agent_id, "workspace")? {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let extra_docs: Vec<String> = match patcher.read_agent_field(agent_id, "extra_docs")? {
        Some(Value::Array(arr)) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    let inbound_bindings: Vec<BindingSummary> =
        match patcher.read_agent_field(agent_id, "inbound_bindings")? {
            Some(Value::Array(arr)) => arr
                .into_iter()
                .filter_map(|b| {
                    let plugin = b.get("plugin")?.as_str()?.to_string();
                    let instance = b.get("instance").and_then(Value::as_str).map(String::from);
                    Some(BindingSummary { plugin, instance })
                })
                .collect(),
            _ => Vec::new(),
        };

    // Heartbeat is `Some(..)` whenever the operator has
    // authored the block at least once. Absent yaml → None so the
    // microapp distinguishes "framework default" from "explicitly
    // configured but disabled".
    let heartbeat_enabled = patcher.read_agent_field(agent_id, "heartbeat.enabled")?;
    let heartbeat_interval = patcher.read_agent_field(agent_id, "heartbeat.interval")?;
    let heartbeat = match (heartbeat_enabled, heartbeat_interval) {
        (None, None) => None,
        (en, iv) => Some(HeartbeatWire {
            enabled: matches!(en, Some(Value::Bool(true))),
            interval: match iv {
                Some(Value::String(s)) => s,
                _ => "5m".to_string(),
            },
        }),
    };

    // Phase 97.UI — slurp the entire agent block as JSON so the
    // admin UI can render every capability gate without per-field
    // dotted reads. Soft-fails to `Value::Null` so the UI keeps
    // working against legacy patchers that don't implement the
    // optional method.
    let raw_config = patcher
        .read_agent_block(agent_id)
        .unwrap_or(None)
        .unwrap_or(Value::Null);

    Ok(Some(AgentDetail {
        id: agent_id.to_string(),
        model: ModelRef { provider, model },
        active,
        allowed_tools,
        inbound_bindings,
        system_prompt,
        language,
        workspace,
        extra_docs,
        heartbeat,
        // Phase 81.31 — populated in c4 (PersonaSnapshotReader
        // injection). Until that wires through, legacy callers see
        // `None` and the admin renders the wizard's single-locale
        // fallback path.
        persona_locales: None,
        raw_config,
    }))
}

fn upsert_yaml(patcher: &dyn YamlPatcher, input: &AgentUpsertInput) -> anyhow::Result<()> {
    // Empty `model.provider` / `model.model` mean "don't touch the
    // existing yaml value" — historically the handler overwrote
    // both unconditionally, so partial updates from callers that
    // only wanted to amend (e.g. `extra_docs`) silently bricked the
    // agent. Treat empty as a no-op write; non-empty is the real
    // upsert path.
    if !input.model.provider.is_empty() {
        patcher.upsert_agent_field(
            &input.id,
            "model.provider",
            Value::String(input.model.provider.clone()),
        )?;
    }
    if !input.model.model.is_empty() {
        patcher.upsert_agent_field(
            &input.id,
            "model.model",
            Value::String(input.model.model.clone()),
        )?;
    }
    if let Some(active) = input.active {
        patcher.upsert_agent_field(&input.id, "active", Value::Bool(active))?;
    }
    if let Some(tools) = &input.allowed_tools {
        let arr = Value::Array(tools.iter().map(|s| Value::String(s.clone())).collect());
        patcher.upsert_agent_field(&input.id, "allowed_tools", arr)?;
    }
    if let Some(prompt) = &input.system_prompt {
        patcher.upsert_agent_field(&input.id, "system_prompt", Value::String(prompt.clone()))?;
    }
    if let Some(language) = &input.language {
        patcher.upsert_agent_field(&input.id, "language", Value::String(language.clone()))?;
    }
    if let Some(transcripts_dir) = &input.transcripts_dir {
        patcher.upsert_agent_field(
            &input.id,
            "transcripts_dir",
            Value::String(transcripts_dir.clone()),
        )?;
    }
    if let Some(workspace) = &input.workspace {
        patcher.upsert_agent_field(&input.id, "workspace", Value::String(workspace.clone()))?;
    }
    if let Some(extra_docs) = &input.extra_docs {
        let arr = Value::Array(
            extra_docs
                .iter()
                .map(|s| Value::String(s.clone()))
                .collect(),
        );
        patcher.upsert_agent_field(&input.id, "extra_docs", arr)?;
    }
    // Heartbeat is replace-whole. `Some` writes both
    // `heartbeat.enabled` + `heartbeat.interval`; `None` leaves
    // the existing yaml block untouched. The empty-string
    // interval guard mirrors the `model.*` no-op semantic above —
    // operators sending an empty literal should not silently
    // brick the daemon at next boot, which would refuse to parse
    // an empty humantime string.
    if let Some(hb) = &input.heartbeat {
        patcher.upsert_agent_field(&input.id, "heartbeat.enabled", Value::Bool(hb.enabled))?;
        if !hb.interval.is_empty() {
            patcher.upsert_agent_field(
                &input.id,
                "heartbeat.interval",
                Value::String(hb.interval.clone()),
            )?;
        }
    }
    if let Some(bindings) = &input.inbound_bindings {
        let arr: Vec<Value> = bindings
            .iter()
            .map(|b| {
                let mut map = serde_json::Map::new();
                map.insert("plugin".into(), Value::String(b.plugin.clone()));
                if let Some(i) = &b.instance {
                    map.insert("instance".into(), Value::String(i.clone()));
                }
                Value::Object(map)
            })
            .collect();
        patcher.upsert_agent_field(&input.id, "inbound_bindings", Value::Array(arr))?;
    }
    // Phase 81.31 follow-up — `locale_prompts` map. `Some({})`
    // clears the block (writes an empty object); `Some(map)`
    // replaces it whole; `None` leaves the existing yaml
    // unchanged. Locale keys validated upstream by the wire-shape
    // parser via `deserialize_locale_prompts`.
    if let Some(prompts) = &input.locale_prompts {
        let mut obj = serde_json::Map::new();
        for (k, v) in prompts {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
        patcher.upsert_agent_field(&input.id, "locale_prompts", Value::Object(obj))?;
    }

    // ── Phase 97.UI — extended capability gates ─────────────────
    //
    // Tier-1 typed scalars / vecs.
    if let Some(tenant_id) = &input.tenant_id {
        patcher.upsert_agent_field(&input.id, "tenant_id", Value::String(tenant_id.clone()))?;
    }
    if let Some(description) = &input.description {
        patcher.upsert_agent_field(&input.id, "description", Value::String(description.clone()))?;
    }
    if let Some(plugins) = &input.plugins {
        let arr = Value::Array(plugins.iter().map(|s| Value::String(s.clone())).collect());
        patcher.upsert_agent_field(&input.id, "plugins", arr)?;
    }
    if let Some(delegates) = &input.allowed_delegates {
        let arr = Value::Array(delegates.iter().map(|s| Value::String(s.clone())).collect());
        patcher.upsert_agent_field(&input.id, "allowed_delegates", arr)?;
    }
    if let Some(delegates) = &input.accept_delegates_from {
        let arr = Value::Array(delegates.iter().map(|s| Value::String(s.clone())).collect());
        patcher.upsert_agent_field(&input.id, "accept_delegates_from", arr)?;
    }
    if let Some(skills) = &input.skills {
        let arr = Value::Array(skills.iter().map(|s| Value::String(s.clone())).collect());
        patcher.upsert_agent_field(&input.id, "skills", arr)?;
    }
    if let Some(skills_dir) = &input.skills_dir {
        patcher.upsert_agent_field(&input.id, "skills_dir", Value::String(skills_dir.clone()))?;
    }

    // Tier-2 / Tier-3 opaque policy blocks. Each is a single
    // dotted-path write replacing the entire block; the patcher
    // serialises the JSON value to YAML preserving nested shapes.
    // `Some(Value::Null)` clears the block (writes `null`);
    // `Some(...)` replaces; `None` leaves yaml unchanged.
    macro_rules! upsert_opaque {
        ($($field:ident as $dotted:literal),* $(,)?) => {
            $(
                if let Some(v) = &input.$field {
                    patcher.upsert_agent_field(&input.id, $dotted, v.clone())?;
                }
            )*
        };
    }
    upsert_opaque!(
        config_tool as "config_tool",
        team as "team",
        repl as "repl",
        proactive as "proactive",
        lsp as "lsp",
        dispatch_policy as "dispatch_policy",
        auto_dream as "auto_dream",
        assistant_mode as "assistant_mode",
        away_summary as "away_summary",
        channels as "channels",
        brief as "brief",
        tool_rate_limits as "tool_rate_limits",
        sender_rate_limit as "sender_rate_limit",
        tool_args_validation as "tool_args_validation",
        remote_triggers as "remote_triggers",
        dreaming as "dreaming",
        workspace_git as "workspace_git",
        context_optimization as "context_optimization",
        outbound_allowlist as "outbound_allowlist",
        pairing_policy as "pairing_policy",
        link_understanding as "link_understanding",
        web_search as "web_search",
        credentials as "credentials",
        google_auth as "google_auth",
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Test-only in-memory `YamlPatcher`. Stores agents as
    /// `agent_id → field_path → Value`. Sufficient for the
    /// admin-domain tests; real yaml-on-disk semantics
    /// (atomicity, ordering) are covered by `nexo-setup`'s own
    /// tests for `yaml_patch::*`.
    #[derive(Debug, Default)]
    struct MockYaml {
        agents: Mutex<HashMap<String, HashMap<String, Value>>>,
    }

    impl MockYaml {
        fn with_fixture() -> Arc<Self> {
            let me = Arc::new(Self::default());
            // ana: active, whatsapp:personal, system_prompt, language=es
            me.set("ana", "model.provider", Value::String("minimax".into()));
            me.set("ana", "model.model", Value::String("MiniMax-M2.5".into()));
            me.set("ana", "active", Value::Bool(true));
            me.set(
                "ana",
                "allowed_tools",
                Value::Array(vec![Value::String("*".into())]),
            );
            me.set(
                "ana",
                "inbound_bindings",
                serde_json::json!([{ "plugin": "whatsapp", "instance": "personal" }]),
            );
            me.set("ana", "system_prompt", Value::String("You are Ana.".into()));
            me.set("ana", "language", Value::String("es".into()));
            // bob: inactive, no bindings
            me.set("bob", "model.provider", Value::String("anthropic".into()));
            me.set(
                "bob",
                "model.model",
                Value::String("claude-opus-4-7".into()),
            );
            me.set("bob", "active", Value::Bool(false));
            me.set("bob", "inbound_bindings", Value::Array(vec![]));
            me
        }

        fn set(&self, agent_id: &str, dotted: &str, value: Value) {
            self.agents
                .lock()
                .unwrap()
                .entry(agent_id.to_string())
                .or_default()
                .insert(dotted.to_string(), value);
        }
    }

    impl YamlPatcher for MockYaml {
        fn list_agent_ids(&self) -> anyhow::Result<Vec<String>> {
            // Source order = insertion order. Use a stable
            // alphabetic sort here so test fixture order matches
            // production yaml read order (which preserves source
            // order for `mapping_for_agent`).
            let mut ids: Vec<String> = self.agents.lock().unwrap().keys().cloned().collect();
            ids.sort();
            Ok(ids)
        }

        fn read_agent_field(&self, agent_id: &str, dotted: &str) -> anyhow::Result<Option<Value>> {
            Ok(self
                .agents
                .lock()
                .unwrap()
                .get(agent_id)
                .and_then(|m| m.get(dotted).cloned()))
        }

        fn upsert_agent_field(
            &self,
            agent_id: &str,
            dotted: &str,
            value: Value,
        ) -> anyhow::Result<()> {
            self.set(agent_id, dotted, value);
            Ok(())
        }

        fn remove_agent(&self, agent_id: &str) -> anyhow::Result<()> {
            self.agents.lock().unwrap().remove(agent_id);
            Ok(())
        }
    }

    fn reload_counter() -> (Arc<AtomicUsize>, impl Fn()) {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        (count, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })
    }

    #[test]
    fn agents_list_returns_yaml_summary_in_alpha_order() {
        let yaml = MockYaml::with_fixture();
        let result = list(&*yaml, Value::Null);
        let response: AgentsListResponse = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.agents.len(), 2);
        assert_eq!(response.agents[0].id, "ana");
        assert!(response.agents[0].active);
        assert_eq!(response.agents[0].model_provider, "minimax");
        assert_eq!(response.agents[0].bindings_count, 1);
        assert_eq!(response.agents[1].id, "bob");
        assert!(!response.agents[1].active);
    }

    #[test]
    fn agents_list_active_only_filters_inactive() {
        let yaml = MockYaml::with_fixture();
        let result = list(&*yaml, serde_json::json!({ "active_only": true }));
        let response: AgentsListResponse = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.agents.len(), 1);
        assert_eq!(response.agents[0].id, "ana");
    }

    #[test]
    fn agents_list_plugin_filter_only_returns_matching_bindings() {
        let yaml = MockYaml::with_fixture();
        let result = list(&*yaml, serde_json::json!({ "plugin_filter": "whatsapp" }));
        let response: AgentsListResponse = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(response.agents.len(), 1);
        assert_eq!(response.agents[0].id, "ana");
    }

    #[test]
    fn agents_get_returns_full_detail() {
        let yaml = MockYaml::with_fixture();
        let result = get(&*yaml, serde_json::json!({ "agent_id": "ana" }));
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(detail.id, "ana");
        assert_eq!(detail.model.provider, "minimax");
        assert_eq!(detail.allowed_tools, vec!["*".to_string()]);
        assert_eq!(detail.inbound_bindings.len(), 1);
        assert_eq!(detail.inbound_bindings[0].plugin, "whatsapp");
        assert_eq!(detail.language.as_deref(), Some("es"));
    }

    #[test]
    fn agents_get_returns_not_found_for_unknown_id() {
        let yaml = MockYaml::with_fixture();
        let result = get(&*yaml, serde_json::json!({ "agent_id": "ghost" }));
        let err = result.error.expect("error");
        match err {
            AdminRpcError::Internal(m) => assert!(m.contains("not_found")),
            other => panic!("expected Internal/not_found, got {other:?}"),
        }
    }

    #[test]
    fn agents_upsert_writes_yaml_and_triggers_reload() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            language: Some("en".into()),
            ..Default::default()
        };
        let result = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert_eq!(detail.language.as_deref(), Some("en"));
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn agents_delete_removes_yaml_block_and_triggers_reload() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let result = delete(&*yaml, serde_json::json!({ "agent_id": "bob" }), &reload);
        let response: AgentsDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(response.removed);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        let listed = list(&*yaml, Value::Null);
        let listed_response: AgentsListResponse =
            serde_json::from_value(listed.result.unwrap()).unwrap();
        assert_eq!(listed_response.agents.len(), 1);
        assert_eq!(listed_response.agents[0].id, "ana");
    }

    #[test]
    fn agents_delete_unknown_id_is_idempotent() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let result = delete(&*yaml, serde_json::json!({ "agent_id": "ghost" }), &reload);
        let response: AgentsDeleteResponse =
            serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(!response.removed);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    /// Agent without `heartbeat.*` fields surfaces
    /// `heartbeat: None` so the microapp shows the framework
    /// default (disabled, 5m).
    #[test]
    fn agents_get_returns_none_heartbeat_when_yaml_omits_block() {
        let yaml = MockYaml::with_fixture();
        let result = get(&*yaml, serde_json::json!({ "agent_id": "ana" }));
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(detail.heartbeat.is_none());
    }

    /// Upsert with `Some(HeartbeatWire { enabled: true,
    /// interval: "30m" })` writes both yaml fields and the next
    /// `get` returns them on the wire.
    #[test]
    fn agents_upsert_writes_heartbeat_block() {
        let yaml = MockYaml::with_fixture();
        let (_count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            heartbeat: Some(HeartbeatWire {
                enabled: true,
                interval: "30m".into(),
            }),
            ..Default::default()
        };
        let _ = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        let detail = read_detail(&*yaml, "ana").unwrap().unwrap();
        let hb = detail.heartbeat.expect("heartbeat persisted");
        assert!(hb.enabled);
        assert_eq!(hb.interval, "30m");
    }

    /// Empty-string interval is a no-op write so the
    /// daemon doesn't brick on next boot. The toggle still flips.
    #[test]
    fn agents_upsert_heartbeat_with_empty_interval_keeps_existing() {
        let yaml = MockYaml::with_fixture();
        // Seed an existing heartbeat block.
        yaml.set("ana", "heartbeat.enabled", Value::Bool(true));
        yaml.set("ana", "heartbeat.interval", Value::String("1h".into()));
        let (_count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            heartbeat: Some(HeartbeatWire {
                enabled: false,
                interval: String::new(),
            }),
            ..Default::default()
        };
        let _ = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        let detail = read_detail(&*yaml, "ana").unwrap().unwrap();
        let hb = detail.heartbeat.expect("heartbeat persisted");
        assert!(!hb.enabled);
        assert_eq!(hb.interval, "1h");
    }

    /// Phase 81.31 follow-up — `agents/upsert` accepts the
    /// `locale_prompts` block and writes the full map verbatim.
    /// `Some(empty)` clears the block; `None` leaves yaml untouched.
    #[test]
    fn agents_upsert_writes_locale_prompts_block() {
        let yaml = MockYaml::with_fixture();
        let (_count, reload) = reload_counter();
        let mut prompts = std::collections::BTreeMap::new();
        prompts.insert("en".to_string(), "english prompt".to_string());
        prompts.insert("es".to_string(), "prompt en espanol".to_string());
        let input = AgentUpsertInput {
            id: "ana".into(),
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            locale_prompts: Some(prompts),
            ..Default::default()
        };
        let _ = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        let raw = yaml
            .read_agent_field("ana", "locale_prompts")
            .unwrap()
            .expect("locale_prompts persisted");
        let obj = raw.as_object().expect("object");
        assert_eq!(obj.len(), 2);
        assert_eq!(
            obj.get("en").and_then(Value::as_str),
            Some("english prompt")
        );
        assert_eq!(
            obj.get("es").and_then(Value::as_str),
            Some("prompt en espanol")
        );
    }

    // ── auto_id resolution tests ────────────────────────────────────

    #[test]
    fn resolve_agent_id_legacy_path_requires_non_empty_id() {
        let err =
            resolve_agent_id("", false, &[]).expect_err("empty id with auto_id=false rejected");
        match err {
            AdminRpcError::InvalidParams(m) => assert!(m.contains("required")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn resolve_agent_id_legacy_path_validates_format() {
        let err = resolve_agent_id("Bad-ID", false, &[]).expect_err("uppercase rejected");
        match err {
            AdminRpcError::InvalidParams(m) => assert!(m.contains("must match")),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn resolve_agent_id_legacy_path_preserves_explicit_id() {
        let id = resolve_agent_id("ana", false, &["ana".into()]).unwrap();
        assert_eq!(id, "ana", "legacy path treats existing id as upsert target");
    }

    #[test]
    fn resolve_agent_id_auto_empty_generates_agent_prefix() {
        let id = resolve_agent_id("", true, &[]).unwrap();
        assert!(id.starts_with("agent_"), "got `{id}`");
        let suffix = &id["agent_".len()..];
        assert_eq!(suffix.len(), 6, "6-digit suffix, got `{suffix}`");
        assert!(suffix.chars().all(|c| c.is_ascii_digit()), "decimal suffix");
    }

    #[test]
    fn resolve_agent_id_auto_explicit_unique_preserved() {
        let id = resolve_agent_id("custom_name", true, &["ana".into(), "bob".into()]).unwrap();
        assert_eq!(id, "custom_name");
    }

    #[test]
    fn resolve_agent_id_auto_explicit_collision_suffixes_timestamp() {
        let id = resolve_agent_id("ana", true, &["ana".into()]).unwrap();
        assert!(id.starts_with("ana_"), "got `{id}`");
        let suffix = &id["ana_".len()..];
        assert!(
            suffix.len() >= 13,
            "epoch-ms is at least 13 digits in 2026, got `{suffix}`"
        );
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn resolve_agent_id_auto_invalid_explicit_rejected() {
        let err = resolve_agent_id("UPPER", true, &[]).expect_err("uppercase rejected");
        match err {
            AdminRpcError::InvalidParams(_) => {}
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn is_valid_agent_id_accepts_canonical_shapes() {
        assert!(is_valid_agent_id("ana"));
        assert!(is_valid_agent_id("a"));
        assert!(is_valid_agent_id("agent_334234"));
        assert!(is_valid_agent_id("multi-word-id"));
        assert!(is_valid_agent_id(&format!("{:a<41}", "a")));
    }

    #[test]
    fn is_valid_agent_id_rejects_bad_shapes() {
        assert!(!is_valid_agent_id(""));
        assert!(!is_valid_agent_id("9starts_with_digit"));
        assert!(!is_valid_agent_id("UPPER"));
        assert!(!is_valid_agent_id("has space"));
        assert!(!is_valid_agent_id("has.dot"));
        assert!(!is_valid_agent_id(&format!("{:a<42}", "a"))); // 42 chars > limit
    }

    #[test]
    fn agents_upsert_auto_id_creates_random_when_empty() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: String::new(),
            auto_id: true,
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            ..Default::default()
        };
        let result = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(detail.id.starts_with("agent_"), "got `{}`", detail.id);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        // The fixture's `ana` and `bob` survive; the new agent joins them.
        let listed = list(&*yaml, Value::Null);
        let listed_response: AgentsListResponse =
            serde_json::from_value(listed.result.unwrap()).unwrap();
        assert_eq!(listed_response.agents.len(), 3);
    }

    #[test]
    fn agents_upsert_auto_id_suffixes_timestamp_on_collision() {
        let yaml = MockYaml::with_fixture();
        let (count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: "ana".into(), // collides — fixture seeds `ana`
            auto_id: true,
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            ..Default::default()
        };
        let result = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        let detail: AgentDetail = serde_json::from_value(result.result.unwrap()).unwrap();
        assert!(detail.id.starts_with("ana_"), "got `{}`", detail.id);
        // Suffix is decimal epoch-ms.
        let suffix = &detail.id["ana_".len()..];
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
        // Original `ana` block untouched.
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let original = read_detail(&*yaml, "ana").unwrap().unwrap();
        assert_eq!(original.system_prompt, "You are Ana.");
    }

    #[test]
    fn agents_upsert_legacy_path_still_upserts_on_collision() {
        let yaml = MockYaml::with_fixture();
        let (_count, reload) = reload_counter();
        let input = AgentUpsertInput {
            id: "ana".into(),
            auto_id: false, // explicit upsert
            model: ModelRef {
                provider: "minimax".into(),
                model: "MiniMax-M2.5".into(),
            },
            language: Some("fr".into()),
            ..Default::default()
        };
        let _ = upsert(&*yaml, serde_json::to_value(&input).unwrap(), &reload);
        // `ana` updated in place — no new agent created.
        let listed = list(&*yaml, Value::Null);
        let listed_response: AgentsListResponse =
            serde_json::from_value(listed.result.unwrap()).unwrap();
        assert_eq!(listed_response.agents.len(), 2);
        let updated = read_detail(&*yaml, "ana").unwrap().unwrap();
        assert_eq!(updated.language.as_deref(), Some("fr"));
    }
}
