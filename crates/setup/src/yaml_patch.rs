//! In-place YAML upsert at a dotted path.
//!
//! We round-trip through `serde_yaml` — comments are lost, but key
//! order is preserved by `serde_yaml::Mapping` (insertion order) which
//! is good enough for configuration files the wizard creates / touches.
//!
//! If the target file doesn't exist yet it's created from scratch.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde_yaml::{Mapping, Value};

/// Serializes all YAML upserts in this process. `run_service` can
/// invoke `upsert` indirectly from two places during one interactive
/// session (e.g. telegram_link runs while persist() is still writing
/// other services) — a global mutex is cheap and closes the same-
/// process RMW race without a file-lock dep.
static YAML_UPSERT_LOCK: Mutex<()> = Mutex::new(());

pub enum ValueKind {
    String,
    Number,
    Bool,
    /// Comma-separated list of strings.
    List,
    /// Comma-separated list where each item is parsed as i64. Needed
    /// for typed int arrays like `telegram.allowlist.chat_ids`.
    IntList,
}

/// Upsert the value at `dotted_path` inside the YAML at `file`. Creates
/// intermediate maps as needed.
pub fn upsert(file: &Path, dotted_path: &str, raw: &str, kind: ValueKind) -> Result<()> {
    // `.unwrap_or_else` keeps us going after a panic in another thread
    // — the only state the mutex guards is filesystem operations, which
    // don't leave in-memory corruption behind.
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if file.exists() {
        let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?
        }
    } else {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).ok();
        }
        Value::Mapping(Mapping::new())
    };

    let parts: Vec<&str> = dotted_path.split('.').collect();
    let typed = typed_value(raw, kind)?;
    set_path(&mut root, &parts, typed)?;

    // Atomic write.
    let parent = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ok();
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("tempfile in {}", parent.display()))?;
    {
        let mut f = tmp.reopen()?;
        let text = serde_yaml::to_string(&root)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

fn typed_value(raw: &str, kind: ValueKind) -> Result<Value> {
    let raw = raw.trim();
    Ok(match kind {
        ValueKind::String => Value::String(raw.to_string()),
        ValueKind::Bool => {
            let lower = raw.to_ascii_lowercase();
            let b = match lower.as_str() {
                "true" | "yes" | "y" | "1" | "on" => true,
                "false" | "no" | "n" | "0" | "off" | "" => false,
                _ => anyhow::bail!("cannot parse '{raw}' as bool"),
            };
            Value::Bool(b)
        }
        ValueKind::Number => {
            let n: i64 = raw
                .parse()
                .with_context(|| format!("cannot parse '{raw}' as integer"))?;
            Value::Number(n.into())
        }
        ValueKind::List => {
            if raw.is_empty() {
                Value::Sequence(vec![])
            } else {
                let items: Vec<Value> = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .map(Value::String)
                    .collect();
                Value::Sequence(items)
            }
        }
        ValueKind::IntList => {
            if raw.is_empty() {
                Value::Sequence(vec![])
            } else {
                let mut items: Vec<Value> = Vec::new();
                for chunk in raw.split(',') {
                    let trimmed = chunk.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let n: i64 = trimmed
                        .parse()
                        .with_context(|| format!("list item '{trimmed}' is not a valid integer"))?;
                    items.push(Value::Number(n.into()));
                }
                Value::Sequence(items)
            }
        }
    })
}

/// Read a dotted-path string value from a YAML file. `None` if any
/// segment is missing or the final value isn't a string. Used to honor
/// operator overrides (e.g. `whatsapp.session_dir`) instead of
/// silently overwriting them.
pub fn get_string(file: &Path, dotted_path: &str) -> Result<Option<String>> {
    if !file.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let v: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let mut cur = &v;
    for segment in dotted_path.split('.') {
        match cur.get(segment) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(cur.as_str().map(str::to_string))
}

/// Return the `workspace` string for a given agent id from `agents.yaml`.
/// None → agent not found or no `workspace` key.
pub fn get_agent_workspace(file: &Path, agent_id: &str) -> Result<Option<String>> {
    if !file.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let v: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let seq = v
        .get("agents")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    for it in seq {
        if it.get("id").and_then(Value::as_str) == Some(agent_id) {
            return Ok(it
                .get("workspace")
                .and_then(Value::as_str)
                .map(str::to_string));
        }
    }
    Ok(None)
}

/// Return the values under `agents[<agent_id>].<list_key>`. Missing
/// agent or missing list → empty vec.
pub fn get_agent_list(file: &Path, agent_id: &str, list_key: &str) -> Result<Vec<String>> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let v: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let seq = v
        .get("agents")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    for it in seq {
        if it.get("id").and_then(Value::as_str) == Some(agent_id) {
            let items = it
                .get(list_key)
                .and_then(Value::as_sequence)
                .cloned()
                .unwrap_or_default();
            return Ok(items
                .into_iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect());
        }
    }
    Ok(Vec::new())
}

/// Return every `id` under top-level `agents:` sequence in `file`.
/// Missing file / empty list → empty vec. Never errors unless the file
/// is malformed YAML.
/// Enumerate every agent id reachable to the runtime: the canonical
/// `agents.yaml` plus every `agents.d/*.yaml` drop-in (excluding
/// `*.example.yaml`). Mirrors the loader logic in `nexo-config`'s
/// `merge_agents_drop_in` so the wizard sees the same set the
/// daemon will load at boot.
/// Phase 83.8.12 — list every agent id whose
/// `agents.yaml.<id>.tenant_id` matches `tenant_id`. Returns
/// the list in stable order (matches `list_agent_ids`). Used
/// by `TenantsYamlPatcher` to detect orphan agents on tenant
/// delete. Empty list when no agent references the tenant.
pub fn list_agents_by_tenant(file: &Path, tenant_id: &str) -> Result<Vec<String>> {
    let all_ids = list_agent_ids(file)?;
    let mut out = Vec::new();
    for id in all_ids {
        let value = read_agent_field(file, &id, "tenant_id")?;
        if let Some(serde_yaml::Value::String(s)) = value {
            if s == tenant_id {
                out.push(id);
            }
        }
    }
    Ok(out)
}

pub fn list_agent_ids(file: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut push_from = |path: &Path| -> Result<()> {
        for id in agent_ids_in_one_file(path)? {
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        Ok(())
    };

    // 1. The canonical `agents.yaml`.
    push_from(file)?;

    // 2. The `agents.d/*.yaml` drop-in directory next to it.
    if let Some(parent) = file.parent() {
        let drop_in = parent.join("agents.d");
        if drop_in.is_dir() {
            let mut entries: Vec<_> = fs::read_dir(&drop_in)
                .with_context(|| format!("read_dir {}", drop_in.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    n.ends_with(".yaml") && !n.ends_with(".example.yaml")
                })
                .collect();
            entries.sort();
            for p in entries {
                push_from(&p)?;
            }
        }
    }

    Ok(out)
}

fn agent_ids_in_one_file(file: &Path) -> Result<Vec<String>> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let seq = v
        .get("agents")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    Ok(seq
        .into_iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect())
}

/// Remove `item` from `agents[<agent_id>].<list_key>`. Returns `true`
/// when the file was modified, `false` if the item wasn't there.
pub fn remove_list_entry(file: &Path, agent_id: &str, list_key: &str, item: &str) -> Result<bool> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;

    let agents = root
        .get_mut("agents")
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` missing in {}", file.display()))?;
    let target = agents
        .iter_mut()
        .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found"))?;
    let map = target
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` is not a mapping"))?;
    let key = Value::String(list_key.into());
    let Some(entry) = map.get_mut(&key) else {
        return Ok(false);
    };
    let seq = match entry.as_sequence_mut() {
        Some(s) => s,
        None => return Ok(false),
    };
    let item_val = Value::String(item.into());
    let before = seq.len();
    seq.retain(|v| v != &item_val);
    if seq.len() == before {
        return Ok(false);
    }

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(true)
}

/// Append `skill_id` to `agents[<agent_id>].skills` if not already
/// there. Creates the `skills:` sequence if absent. No-op when the
/// item is already present (idempotent, safe to call on every setup
/// run).
pub fn add_skill_to_agent(file: &Path, agent_id: &str, skill_id: &str) -> Result<bool> {
    add_list_entry(file, agent_id, "skills", skill_id)
}

/// Same as `add_skill_to_agent` but for `plugins:`. Use for services
/// that surface an in-process plugin (browser, whatsapp, memory).
pub fn add_plugin_to_agent(file: &Path, agent_id: &str, plugin_id: &str) -> Result<bool> {
    add_list_entry(file, agent_id, "plugins", plugin_id)
}

/// Drop `plugin_id` from `agents[<id>].plugins`. Returns `true` when
/// the file was modified, `false` if the plugin wasn't listed.
pub fn remove_plugin_from_agent(file: &Path, agent_id: &str, plugin_id: &str) -> Result<bool> {
    remove_list_entry(file, agent_id, "plugins", plugin_id)
}

/// Shared helper: append `item` to `agents[<id>].<list_key>`. Returns
/// `true` when the file was modified, `false` if the item already
/// existed.
fn add_list_entry(file: &Path, agent_id: &str, list_key: &str, item: &str) -> Result<bool> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;

    let agents = root
        .get_mut("agents")
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", file.display()))?;

    let target = agents
        .iter_mut()
        .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", file.display()))?;

    let map = target
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` is not a mapping"))?;

    let key = Value::String(list_key.into());
    let entry = map
        .entry(key.clone())
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let seq = entry
        .as_sequence_mut()
        .ok_or_else(|| anyhow::anyhow!("`{list_key}:` is not a sequence"))?;
    let item_val = Value::String(item.into());
    if seq.iter().any(|v| v == &item_val) {
        return Ok(false);
    }
    seq.push(item_val);

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(true)
}

/// Insert (or overwrite) the `google_auth` block under the agent with
/// matching `id` inside `agents.yaml`. Uses raw-text injection for the
/// secret references so the output YAML preserves the `${file:...}`
/// placeholder verbatim (serde_yaml would happily round-trip it, but
/// we also want stable ordering — google_auth always appended last).
pub fn patch_agent_google_auth(
    file: &Path,
    agent_id: &str,
    client_id_ref: &str,
    client_secret_ref: &str,
    redirect_port: u16,
    scopes: &[String],
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;

    let agents = root
        .get_mut("agents")
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", file.display()))?;

    let target = agents
        .iter_mut()
        .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", file.display()))?;

    let map = target
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` is not a mapping"))?;

    let mut block = Mapping::new();
    block.insert(
        Value::String("client_id".into()),
        Value::String(client_id_ref.into()),
    );
    block.insert(
        Value::String("client_secret".into()),
        Value::String(client_secret_ref.into()),
    );
    block.insert(
        Value::String("redirect_port".into()),
        Value::Number((redirect_port as i64).into()),
    );
    block.insert(
        Value::String("scopes".into()),
        Value::Sequence(scopes.iter().cloned().map(Value::String).collect()),
    );
    map.insert(Value::String("google_auth".into()), Value::Mapping(block));

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

/// Rewrite `model.provider` and `model.model` for a given agent ID in
/// `agents.yaml`. When `agent_id` is empty or `"*"`, rewrites every
/// agent. Returns the list of agent IDs that were actually patched.
pub fn patch_agent_model(
    file: &Path,
    agent_id: &str,
    provider: &str,
    model: &str,
) -> Result<Vec<String>> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;

    let agents = root
        .get_mut("agents")
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", file.display()))?;

    let mut patched: Vec<String> = Vec::new();
    let match_all = agent_id.is_empty() || agent_id == "*";
    for agent in agents.iter_mut() {
        let id = agent
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !match_all && id != agent_id {
            continue;
        }
        let Some(map) = agent.as_mapping_mut() else {
            continue;
        };
        let mut model_block = Mapping::new();
        model_block.insert(
            Value::String("provider".into()),
            Value::String(provider.into()),
        );
        model_block.insert(Value::String("model".into()), Value::String(model.into()));
        map.insert(Value::String("model".into()), Value::Mapping(model_block));
        patched.push(id);
    }
    if patched.is_empty() {
        anyhow::bail!("no agent matched `{agent_id}` in {}", file.display());
    }

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(patched)
}

/// Patch `docker-compose.yml`:
/// - append `google_client_id` + `google_client_secret` to
///   `services.agent.secrets` (if not already present)
/// - add matching top-level `secrets.<name>.file` entries pointing at
///   `./secrets/<name>.txt`.
///
/// No-op if the compose file is missing.
pub fn patch_compose_google_secrets(file: &Path) -> Result<()> {
    if !file.exists() {
        return Ok(());
    }
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;

    let wanted = ["google_client_id", "google_client_secret"];

    // Append to services.agent.secrets list (create if absent).
    if let Some(agent) = root
        .get_mut("services")
        .and_then(Value::as_mapping_mut)
        .and_then(|m| m.get_mut(Value::String("agent".into())))
        .and_then(Value::as_mapping_mut)
    {
        let key = Value::String("secrets".into());
        let existing = agent
            .entry(key.clone())
            .or_insert_with(|| Value::Sequence(Vec::new()));
        if let Value::Sequence(seq) = existing {
            for name in &wanted {
                let v = Value::String((*name).into());
                if !seq.iter().any(|x| x == &v) {
                    seq.push(v);
                }
            }
        }
    }

    // Top-level secrets.<name>.file = ./secrets/<name>.txt
    let top_secrets = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("compose root is not a mapping"))?
        .entry(Value::String("secrets".into()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let top_map = top_secrets
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("top-level `secrets:` is not a mapping"))?;
    for name in &wanted {
        let key = Value::String((*name).into());
        if top_map.contains_key(&key) {
            continue;
        }
        let mut entry = Mapping::new();
        entry.insert(
            Value::String("file".into()),
            Value::String(format!("./secrets/{name}.txt")),
        );
        top_map.insert(key, Value::Mapping(entry));
    }

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

fn set_path(root: &mut Value, parts: &[&str], value: Value) -> Result<()> {
    if parts.is_empty() {
        anyhow::bail!("empty path");
    }
    if parts.len() == 1 {
        let map = root
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("root is not a mapping"))?;
        map.insert(Value::String(parts[0].to_string()), value);
        return Ok(());
    }
    let head = parts[0];
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("'{head}' parent is not a mapping"))?;
    let key = Value::String(head.to_string());
    if !map.contains_key(&key) {
        map.insert(key.clone(), Value::Mapping(Mapping::new()));
    }
    let next = map
        .get_mut(&key)
        .ok_or_else(|| anyhow::anyhow!("failed to descend into '{head}'"))?;
    set_path(next, &parts[1..], value)
}

// ─────────────────────────────────────────────────────────────────────
// Agent-centric wizard helpers (read/upsert/remove/list ops scoped to
// a single agent inside `agents.yaml` or `agents.d/*.yaml`). Used by
// the per-agent setup submenu so it can mutate `model`, `language`,
// `plugins`, `inbound_bindings`, `skills` and similar without going
// through the declarative ServiceDef pipeline (which can't model
// per-agent paths).
// ─────────────────────────────────────────────────────────────────────

/// Locate the agent inside the YAML at `path` and return the value at
/// `dotted` relative to that agent. Returns `Ok(None)` when either the
/// agent or any path segment is missing.
pub fn read_agent_field(path: &Path, agent_id: &str, dotted: &str) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let agents = match root.get("agents").and_then(Value::as_sequence) {
        Some(s) => s,
        None => return Ok(None),
    };
    let agent = match agents
        .iter()
        .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
    {
        Some(a) => a,
        None => return Ok(None),
    };
    let mut cur: &Value = agent;
    for segment in dotted.split('.') {
        match cur.get(segment) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur.clone()))
}

/// Upsert `value` at the dotted path inside the matching agent's
/// mapping. Creates intermediate maps as needed; bails when the agent
/// is absent.
pub fn upsert_agent_field(path: &Path, agent_id: &str, dotted: &str, value: Value) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    {
        let agents = root
            .get_mut("agents")
            .and_then(Value::as_sequence_mut)
            .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", path.display()))?;
        let target = agents
            .iter_mut()
            .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
            .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", path.display()))?;
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            anyhow::bail!("empty dotted path");
        }
        set_path(target, &parts, value)?;
    }

    write_atomic(path, &root)
}

/// Remove the dotted path inside the matching agent. No-op when the
/// path is already absent. Bails when the agent itself doesn't exist.
pub fn remove_agent_field(path: &Path, agent_id: &str, dotted: &str) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    {
        let agents = root
            .get_mut("agents")
            .and_then(Value::as_sequence_mut)
            .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", path.display()))?;
        let target = agents
            .iter_mut()
            .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
            .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", path.display()))?;
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            anyhow::bail!("empty dotted path");
        }
        remove_path(target, &parts);
    }

    write_atomic(path, &root)
}

/// Append `item` to the sequence at `dotted` inside the matching
/// agent. Creates the sequence if absent. Idempotent: a no-op when an
/// equal item is already present.
pub fn append_agent_list_item(
    path: &Path,
    agent_id: &str,
    dotted: &str,
    item: Value,
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    {
        let agents = root
            .get_mut("agents")
            .and_then(Value::as_sequence_mut)
            .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", path.display()))?;
        let target = agents
            .iter_mut()
            .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
            .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", path.display()))?;
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            anyhow::bail!("empty dotted path");
        }
        let seq_value = ensure_sequence_at(target, &parts)?;
        let seq = seq_value
            .as_sequence_mut()
            .ok_or_else(|| anyhow::anyhow!("`{dotted}` is not a sequence"))?;
        if !seq.iter().any(|v| v == &item) {
            seq.push(item);
        }
    }

    write_atomic(path, &root)
}

/// Remove every item from the sequence at `dotted` matching
/// `predicate`. No-op when the path or sequence is absent.
pub fn remove_agent_list_item(
    path: &Path,
    agent_id: &str,
    dotted: &str,
    predicate: &dyn Fn(&Value) -> bool,
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    {
        let agents = root
            .get_mut("agents")
            .and_then(Value::as_sequence_mut)
            .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", path.display()))?;
        let target = agents
            .iter_mut()
            .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
            .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", path.display()))?;
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            anyhow::bail!("empty dotted path");
        }
        // Walk to the parent of the leaf; missing intermediates → no-op.
        let mut cur: &mut Value = target;
        for part in &parts {
            let next = match cur.as_mapping_mut() {
                Some(m) => m.get_mut(Value::String((*part).to_string())),
                None => None,
            };
            cur = match next {
                Some(v) => v,
                None => return write_atomic(path, &root),
            };
        }
        if let Some(seq) = cur.as_sequence_mut() {
            seq.retain(|v| !predicate(v));
        }
    }

    write_atomic(path, &root)
}

/// Walk into `root` along `parts`, materialising mappings as needed,
/// and return a `&mut Value` guaranteed to be a sequence at the leaf.
fn ensure_sequence_at<'a>(root: &'a mut Value, parts: &[&str]) -> Result<&'a mut Value> {
    let mut cur: &mut Value = root;
    for (idx, part) in parts.iter().enumerate() {
        let map = cur
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("`{part}` parent is not a mapping"))?;
        let key = Value::String((*part).to_string());
        if !map.contains_key(&key) {
            // Last segment defaults to a sequence; intermediates to maps.
            let placeholder = if idx == parts.len() - 1 {
                Value::Sequence(Vec::new())
            } else {
                Value::Mapping(Mapping::new())
            };
            map.insert(key.clone(), placeholder);
        }
        cur = map
            .get_mut(&key)
            .ok_or_else(|| anyhow::anyhow!("failed to descend into `{part}`"))?;
        if idx == parts.len() - 1 && !matches!(cur, Value::Sequence(_)) {
            *cur = Value::Sequence(Vec::new());
        }
    }
    Ok(cur)
}

/// Walk along `parts` and delete the leaf key from its parent
/// mapping. No-op when any segment is absent.
fn remove_path(root: &mut Value, parts: &[&str]) {
    if parts.is_empty() {
        return;
    }
    let (last, head) = parts.split_last().expect("non-empty");
    let mut cur: &mut Value = root;
    for part in head {
        let next = match cur.as_mapping_mut() {
            Some(m) => m.get_mut(Value::String((*part).to_string())),
            None => None,
        };
        cur = match next {
            Some(v) => v,
            None => return,
        };
    }
    if let Some(map) = cur.as_mapping_mut() {
        map.remove(Value::String((*last).to_string()));
    }
}

/// Atomic-write helper shared by the agent-aware mutators.
fn write_atomic(path: &Path, root: &Value) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ok();
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("tempfile in {}", parent.display()))?;
    {
        let mut f = tmp.reopen()?;
        let text = serde_yaml::to_string(root)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", path.display()))?;
    Ok(())
}

// Historical test module — additional helpers are intentionally
// defined below. Moving this to the end of the file would churn diffs
// across every future helper added to the Phase 17 section.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.yaml");
        upsert(&file, "a.b.c", "hello", ValueKind::String).unwrap();
        let v: Value = serde_yaml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["a"]["b"]["c"], Value::String("hello".into()));
    }

    #[test]
    fn overwrites_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.yaml");
        fs::write(&file, "root:\n  a: old\n  keep: 1\n").unwrap();
        upsert(&file, "root.a", "new", ValueKind::String).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        let v: Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(v["root"]["a"], Value::String("new".into()));
        assert_eq!(v["root"]["keep"], Value::Number(1.into()));
    }

    #[test]
    fn bool_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.yaml");
        upsert(&file, "flag", "true", ValueKind::Bool).unwrap();
        let v: Value = serde_yaml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["flag"], Value::Bool(true));
    }

    #[test]
    fn list_splits_on_comma() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.yaml");
        upsert(&file, "allow", "a, b,  c", ValueKind::List).unwrap();
        let v: Value = serde_yaml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        let seq = v["allow"].as_sequence().unwrap();
        assert_eq!(seq.len(), 3);
        assert_eq!(seq[2], Value::String("c".into()));
    }

    #[test]
    fn empty_list_becomes_empty_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.yaml");
        upsert(&file, "allow", "", ValueKind::List).unwrap();
        let v: Value = serde_yaml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v["allow"].as_sequence().unwrap().is_empty());
    }

    fn write_sample_agents(path: &Path) {
        fs::write(
            path,
            r#"agents:
- id: kate
  model:
    provider: anthropic
    model: claude-haiku-4-5
  plugins:
  - telegram
  skills:
  - weather
- id: ana
  model:
    provider: openai
    model: gpt-4o
  plugins: []
"#,
        )
        .unwrap();
    }

    #[test]
    fn read_agent_field_returns_existing_string() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.yaml");
        write_sample_agents(&file);
        let v = read_agent_field(&file, "kate", "model.provider").unwrap();
        assert_eq!(v.unwrap().as_str(), Some("anthropic"));
    }

    #[test]
    fn read_agent_field_missing_path_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.yaml");
        write_sample_agents(&file);
        assert!(read_agent_field(&file, "kate", "language")
            .unwrap()
            .is_none());
        assert!(read_agent_field(&file, "ghost", "model.provider")
            .unwrap()
            .is_none());
    }

    #[test]
    fn upsert_agent_field_creates_intermediate_maps() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.yaml");
        write_sample_agents(&file);
        upsert_agent_field(&file, "ana", "language", Value::String("es".into())).unwrap();
        let v = read_agent_field(&file, "ana", "language").unwrap().unwrap();
        assert_eq!(v.as_str(), Some("es"));
    }

    #[test]
    fn remove_agent_field_drops_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.yaml");
        write_sample_agents(&file);
        upsert_agent_field(&file, "kate", "language", Value::String("es".into())).unwrap();
        remove_agent_field(&file, "kate", "language").unwrap();
        assert!(read_agent_field(&file, "kate", "language")
            .unwrap()
            .is_none());
    }

    #[test]
    fn append_agent_list_item_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.yaml");
        write_sample_agents(&file);
        append_agent_list_item(&file, "kate", "plugins", Value::String("whatsapp".into())).unwrap();
        // Second call is a no-op.
        append_agent_list_item(&file, "kate", "plugins", Value::String("whatsapp".into())).unwrap();
        let v = read_agent_field(&file, "kate", "plugins").unwrap().unwrap();
        let seq = v.as_sequence().unwrap();
        let count = seq
            .iter()
            .filter(|v| v.as_str() == Some("whatsapp"))
            .count();
        assert_eq!(count, 1);
        assert_eq!(seq.len(), 2);
    }

    #[test]
    fn remove_agent_list_item_by_predicate() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.yaml");
        write_sample_agents(&file);
        remove_agent_list_item(&file, "kate", "plugins", &|v| {
            v.as_str() == Some("telegram")
        })
        .unwrap();
        let v = read_agent_field(&file, "kate", "plugins").unwrap().unwrap();
        assert!(v.as_sequence().unwrap().is_empty());
    }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 17 helpers — multi-instance plugin entries + per-agent
// `credentials:` block + google-auth.yaml account upsert. These exist
// in a separate impl block so the wizard can call them imperatively
// without going through the declarative ServiceDef flow, which cannot
// model array entries.
// ─────────────────────────────────────────────────────────────────────

/// Locate the YAML file where `agent_id` is declared. Checks
/// `<config>/agents.yaml` first, then every `<config>/agents.d/*.yaml`.
/// Returns `None` when the id is unknown.
pub fn find_agent_file(config_dir: &Path, agent_id: &str) -> Result<Option<std::path::PathBuf>> {
    // Use `agent_ids_in_one_file` here, NOT `list_agent_ids` — the
    // latter merges agents.yaml + agents.d/, so it would lie about
    // which physical file actually owns the id.
    let main = config_dir.join("agents.yaml");
    if main.exists()
        && agent_ids_in_one_file(&main)
            .unwrap_or_default()
            .iter()
            .any(|id| id == agent_id)
    {
        return Ok(Some(main));
    }
    let drop_dir = config_dir.join("agents.d");
    if !drop_dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&drop_dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.ends_with(".yaml") || name.ends_with(".example.yaml") {
            continue;
        }
        if agent_ids_in_one_file(&path)
            .unwrap_or_default()
            .iter()
            .any(|id| id == agent_id)
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Write `credentials.<channel>: <instance>` into `agent_id`'s entry
/// inside the given agents YAML file. Creates the `credentials`
/// mapping when absent.
pub fn patch_agent_credentials(
    file: &Path,
    agent_id: &str,
    channel: &str,
    instance: &str,
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;

    let agents = root
        .get_mut("agents")
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", file.display()))?;

    let target = agents
        .iter_mut()
        .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", file.display()))?;

    let map = target
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` is not a mapping"))?;

    let creds = map
        .entry(Value::String("credentials".into()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    let creds_map = creds
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("`credentials` is not a mapping"))?;
    creds_map.insert(
        Value::String(channel.to_string()),
        Value::String(instance.to_string()),
    );

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

/// Ensure the agent's `inbound_bindings` includes a binding for
/// `(plugin, instance)`. Idempotent: if a binding already exists for
/// the pair (or for `(plugin, None)`) it is updated to point at
/// `instance`; otherwise a new entry is appended. Required so the
/// runtime's tightened topic-match rule (binding without `instance`
/// only catches no-instance topics) actually delivers per-bot events
/// to the right agent.
pub fn upsert_agent_inbound_binding(
    file: &Path,
    agent_id: &str,
    plugin: &str,
    instance: &str,
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let agents = root
        .get_mut("agents")
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`agents:` sequence missing in {}", file.display()))?;
    let target = agents
        .iter_mut()
        .find(|it| it.get("id").and_then(Value::as_str) == Some(agent_id))
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` not found in {}", file.display()))?;
    let map = target
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("agent `{agent_id}` is not a mapping"))?;
    let bindings_key = Value::String("inbound_bindings".into());
    let bindings = map
        .entry(bindings_key)
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let seq = bindings
        .as_sequence_mut()
        .ok_or_else(|| anyhow::anyhow!("`inbound_bindings` is not a list"))?;

    // Pass 1: bindings already targeting the right (plugin, instance)
    // are a no-op. Pass 2: a binding for the same plugin without an
    // instance gets the instance attached (operator promoted single
    // bot to multi-bot — preserve their other overrides).
    let mut updated = false;
    for entry in seq.iter_mut() {
        let Some(m) = entry.as_mapping_mut() else {
            continue;
        };
        if m.get(Value::String("plugin".into()))
            .and_then(Value::as_str)
            != Some(plugin)
        {
            continue;
        }
        match m
            .get(Value::String("instance".into()))
            .and_then(Value::as_str)
        {
            Some(existing) if existing == instance => return Ok(()),
            None => {
                m.insert(
                    Value::String("instance".into()),
                    Value::String(instance.to_string()),
                );
                updated = true;
                break;
            }
            _ => continue,
        }
    }
    if !updated {
        let mut entry = Mapping::new();
        entry.insert(
            Value::String("plugin".into()),
            Value::String(plugin.to_string()),
        );
        entry.insert(
            Value::String("instance".into()),
            Value::String(instance.to_string()),
        );
        seq.push(Value::Mapping(entry));
    }

    let parent = file.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

/// Normalise `config/plugins/whatsapp.yaml` to the sequence form and
/// upsert an entry by `instance` label. When the file is absent or the
/// root is an empty mapping, creates a fresh sequence.
pub fn whatsapp_upsert_instance(
    file: &Path,
    instance: &str,
    session_dir: &str,
    media_dir: &str,
    allow_agents: &[String],
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if file.exists() {
        let text = fs::read_to_string(file)?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)?
        }
    } else {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).ok();
        }
        Value::Mapping(Mapping::new())
    };

    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("whatsapp.yaml root is not a mapping"))?;

    // Normalise mapping form → sequence.
    let existing = map.remove(Value::String("whatsapp".into()));
    let mut seq: Vec<Value> = match existing {
        Some(Value::Sequence(s)) => s,
        Some(Value::Mapping(m)) => vec![Value::Mapping(m)],
        _ => Vec::new(),
    };

    let mut entry = Mapping::new();
    entry.insert(
        Value::String("instance".into()),
        Value::String(instance.into()),
    );
    entry.insert(Value::String("enabled".into()), Value::Bool(true));
    entry.insert(
        Value::String("session_dir".into()),
        Value::String(session_dir.into()),
    );
    entry.insert(
        Value::String("media_dir".into()),
        Value::String(media_dir.into()),
    );
    entry.insert(
        Value::String("allow_agents".into()),
        Value::Sequence(allow_agents.iter().cloned().map(Value::String).collect()),
    );

    // Replace-in-place if an entry with this instance already exists.
    let replaced = seq.iter_mut().any(|v| {
        if v.get("instance").and_then(Value::as_str) == Some(instance) {
            *v = Value::Mapping(entry.clone());
            true
        } else {
            false
        }
    });
    if !replaced {
        seq.push(Value::Mapping(entry));
    }

    map.insert(Value::String("whatsapp".into()), Value::Sequence(seq));

    let parent = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ok();
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

/// Same pattern for `config/plugins/telegram.yaml`. Token is written as
/// a `${file:...}` placeholder pointing at the `secrets/` sibling of
/// `config_dir` (so the wizard's existing secret store keeps working).
pub fn telegram_upsert_instance(
    file: &Path,
    instance: &str,
    token_placeholder: &str,
    allow_agents: &[String],
    chat_ids: &[i64],
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if file.exists() {
        let text = fs::read_to_string(file)?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)?
        }
    } else {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).ok();
        }
        Value::Mapping(Mapping::new())
    };

    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("telegram.yaml root is not a mapping"))?;

    let existing = map.remove(Value::String("telegram".into()));
    let mut seq: Vec<Value> = match existing {
        Some(Value::Sequence(s)) => s,
        Some(Value::Mapping(m)) => vec![Value::Mapping(m)],
        _ => Vec::new(),
    };

    let mut entry = Mapping::new();
    entry.insert(
        Value::String("instance".into()),
        Value::String(instance.into()),
    );
    entry.insert(
        Value::String("token".into()),
        Value::String(token_placeholder.into()),
    );
    entry.insert(
        Value::String("allow_agents".into()),
        Value::Sequence(allow_agents.iter().cloned().map(Value::String).collect()),
    );
    let mut allowlist = Mapping::new();
    allowlist.insert(
        Value::String("chat_ids".into()),
        Value::Sequence(
            chat_ids
                .iter()
                .map(|n| Value::Number((*n).into()))
                .collect(),
        ),
    );
    entry.insert(Value::String("allowlist".into()), Value::Mapping(allowlist));

    let replaced = seq.iter_mut().any(|v| {
        if v.get("instance").and_then(Value::as_str) == Some(instance) {
            *v = Value::Mapping(entry.clone());
            true
        } else {
            false
        }
    });
    if !replaced {
        seq.push(Value::Mapping(entry));
    }

    map.insert(Value::String("telegram".into()), Value::Sequence(seq));

    let parent = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ok();
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

/// Append a `chat_id` to the `allowlist.chat_ids` of the telegram
/// instance bound to `agent_id` (i.e. the entry whose `allow_agents`
/// contains it). Falls back to the only instance present when no
/// `agent_id` is given and the file has a single entry. Returns the
/// instance label that was patched, or an error explaining which
/// disambiguation step the caller has to take.
pub fn telegram_append_chat_id(
    file: &Path,
    agent_id: Option<&str>,
    chat_id: i64,
) -> Result<String> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !file.exists() {
        anyhow::bail!(
            "telegram.yaml not found at {} — corre `agent setup telegram` primero",
            file.display()
        );
    }
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut root: Value = if text.trim().is_empty() {
        Value::Mapping(Mapping::new())
    } else {
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?
    };

    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("telegram.yaml root is not a mapping"))?;
    let seq = map
        .get_mut(Value::String("telegram".into()))
        .ok_or_else(|| anyhow::anyhow!("missing `telegram:` key in {}", file.display()))?
        .as_sequence_mut()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "`telegram` must be a list of instances in {} — fix manually",
                file.display()
            )
        })?;

    if seq.is_empty() {
        anyhow::bail!("no telegram instances configured in {}", file.display());
    }

    let idx = match agent_id {
        Some(agent) => {
            let matches: Vec<usize> = seq
                .iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    let allow = v.get("allow_agents")?.as_sequence()?;
                    if allow.iter().any(|a| a.as_str() == Some(agent)) {
                        Some(i)
                    } else {
                        None
                    }
                })
                .collect();
            match matches.len() {
                0 => anyhow::bail!(
                    "no telegram instance lists `{agent}` in `allow_agents` — \
                     edita {} y añade el agente a la instancia correcta",
                    file.display()
                ),
                1 => matches[0],
                _ => anyhow::bail!(
                    "multiple telegram instances list `{agent}` in `allow_agents` — \
                     ambiguous, edita {} manualmente",
                    file.display()
                ),
            }
        }
        None => {
            if seq.len() == 1 {
                0
            } else {
                anyhow::bail!(
                    "multiple telegram instances exist in {}; pasa el agente \
                     (`agent setup telegram-link --agent <id>`) para escoger",
                    file.display()
                );
            }
        }
    };

    let entry = seq[idx]
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("telegram instance at index {idx} is not a mapping"))?;

    let label = entry
        .get(Value::String("instance".into()))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "default".to_string());

    let allowlist_key = Value::String("allowlist".into());
    if !entry.contains_key(&allowlist_key) {
        entry.insert(allowlist_key.clone(), Value::Mapping(Mapping::new()));
    }
    let allowlist = entry
        .get_mut(&allowlist_key)
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| anyhow::anyhow!("`allowlist` in instance `{label}` is not a mapping"))?;

    let chat_ids_key = Value::String("chat_ids".into());
    if !allowlist.contains_key(&chat_ids_key) {
        allowlist.insert(chat_ids_key.clone(), Value::Sequence(Vec::new()));
    }
    let chat_ids = allowlist
        .get_mut(&chat_ids_key)
        .and_then(Value::as_sequence_mut)
        .ok_or_else(|| anyhow::anyhow!("`chat_ids` in instance `{label}` is not a list"))?;

    let already_present = chat_ids.iter().any(|v| v.as_i64() == Some(chat_id));
    if !already_present {
        chat_ids.push(Value::Number(chat_id.into()));
    }

    let parent = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ok();
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("tempfile in {}", parent.display()))?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(label)
}

/// Upsert an account in `config/plugins/google-auth.yaml`.
pub fn google_auth_upsert_account(
    file: &Path,
    id: &str,
    agent_id: &str,
    client_id_path: &str,
    client_secret_path: &str,
    token_path: &str,
    scopes: &[String],
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if file.exists() {
        let text = fs::read_to_string(file)?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)?
        }
    } else {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).ok();
        }
        Value::Mapping(Mapping::new())
    };

    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("google-auth.yaml root is not a mapping"))?;

    let ga = map
        .entry(Value::String("google_auth".into()))
        .or_insert_with(|| {
            let mut m = Mapping::new();
            m.insert(
                Value::String("accounts".into()),
                Value::Sequence(Vec::new()),
            );
            Value::Mapping(m)
        });
    let ga_map = ga
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("`google_auth` is not a mapping"))?;
    let accounts = ga_map
        .entry(Value::String("accounts".into()))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    let seq = accounts
        .as_sequence_mut()
        .ok_or_else(|| anyhow::anyhow!("`google_auth.accounts` is not a sequence"))?;

    let mut entry = Mapping::new();
    entry.insert(Value::String("id".into()), Value::String(id.into()));
    entry.insert(
        Value::String("agent_id".into()),
        Value::String(agent_id.into()),
    );
    entry.insert(
        Value::String("client_id_path".into()),
        Value::String(client_id_path.into()),
    );
    entry.insert(
        Value::String("client_secret_path".into()),
        Value::String(client_secret_path.into()),
    );
    entry.insert(
        Value::String("token_path".into()),
        Value::String(token_path.into()),
    );
    entry.insert(
        Value::String("scopes".into()),
        Value::Sequence(scopes.iter().cloned().map(Value::String).collect()),
    );

    let replaced = seq.iter_mut().any(|v| {
        if v.get("id").and_then(Value::as_str) == Some(id) {
            *v = Value::Mapping(entry.clone());
            true
        } else {
            false
        }
    });
    if !replaced {
        seq.push(Value::Mapping(entry));
    }

    let parent = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ok();
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut f = tmp.reopen()?;
        f.write_all(serde_yaml::to_string(&root)?.as_bytes())?;
        f.flush()?;
        f.sync_all().ok();
    }
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist yaml {}: {e}", file.display()))?;
    Ok(())
}

// =====================================================================
// Phase 79.10 — `YamlPatch` shape + denylist-aware applier.
//
// `YamlPatch` is the wire shape persisted under
// `.nexo/config-proposals/<patch_id>.yaml` between `propose` and
// `apply`. `apply_patch_with_denylist` runs the denylist gate FIRST
// (defense-in-depth — `propose` already gated; `apply` re-gates because
// the staging file may have been edited externally) and only then
// delegates to the existing `upsert_agent_field` / `remove_agent_field`
// (Phase 69 helpers).
//
// Reference (PRIMARY): leak's `claude-code-leak/src/tools/ConfigTool/
// ConfigTool.ts:310-343` (`updateSettingsForSource('userSettings', update)`)
// — the leak ships no denylist gate; we add it.
// =====================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ActorInfo {
    pub agent_id: String,
    pub binding_id: String,
    pub channel: String,
    pub account_id: String,
    pub sender_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum YamlOp {
    Upsert {
        agent_id: String,
        dotted: String,
        value: Value,
    },
    Remove {
        agent_id: String,
        dotted: String,
    },
}

impl YamlOp {
    pub fn dotted(&self) -> &str {
        match self {
            YamlOp::Upsert { dotted, .. } | YamlOp::Remove { dotted, .. } => dotted.as_str(),
        }
    }

    pub fn agent_id(&self) -> &str {
        match self {
            YamlOp::Upsert { agent_id, .. } | YamlOp::Remove { agent_id, .. } => agent_id.as_str(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct YamlPatch {
    pub patch_id: String,
    pub binding_id: String,
    pub agent_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub actor: ActorInfo,
    pub justification: String,
    pub op: YamlOp,
}

impl YamlPatch {
    pub fn dotted(&self) -> &str {
        self.op.dotted()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Forbidden(#[from] crate::capabilities::ForbiddenKey),
    #[error("yaml apply: io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml apply: {0}")]
    Yaml(String),
}

impl From<anyhow::Error> for ApplyError {
    fn from(e: anyhow::Error) -> Self {
        ApplyError::Yaml(e.to_string())
    }
}

pub fn apply_patch_with_denylist(
    file: &Path,
    patch: &YamlPatch,
) -> std::result::Result<(), ApplyError> {
    if let Some(matched_glob) = crate::capabilities::denylist_match(patch.dotted()) {
        return Err(ApplyError::Forbidden(crate::capabilities::ForbiddenKey {
            path: patch.dotted().to_string(),
            matched_glob,
        }));
    }
    match &patch.op {
        YamlOp::Upsert {
            agent_id,
            dotted,
            value,
        } => {
            upsert_agent_field(file, agent_id, dotted, value.clone()).map_err(ApplyError::from)?;
        }
        YamlOp::Remove { agent_id, dotted } => {
            remove_agent_field(file, agent_id, dotted).map_err(ApplyError::from)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod patch_tests {
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

    fn upsert_patch(agent: &str, dotted: &str, value: Value) -> YamlPatch {
        YamlPatch {
            patch_id: "01J".into(),
            binding_id: "wa:default".into(),
            agent_id: agent.into(),
            created_at: 0,
            expires_at: 0,
            actor: ActorInfo {
                agent_id: agent.into(),
                binding_id: "wa:default".into(),
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "user".into(),
            },
            justification: "test".into(),
            op: YamlOp::Upsert {
                agent_id: agent.into(),
                dotted: dotted.into(),
                value,
            },
        }
    }

    fn remove_patch(agent: &str, dotted: &str) -> YamlPatch {
        YamlPatch {
            patch_id: "01J".into(),
            binding_id: "wa:default".into(),
            agent_id: agent.into(),
            created_at: 0,
            expires_at: 0,
            actor: ActorInfo {
                agent_id: agent.into(),
                binding_id: "wa:default".into(),
                channel: "whatsapp".into(),
                account_id: "default".into(),
                sender_id: "user".into(),
            },
            justification: "test".into(),
            op: YamlOp::Remove {
                agent_id: agent.into(),
                dotted: dotted.into(),
            },
        }
    }

    #[test]
    fn apply_patch_upsert_writes_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let patch = upsert_patch(
            "cody",
            "model.model",
            Value::String("claude-opus-4-7".into()),
        );
        apply_patch_with_denylist(&path, &patch).unwrap();
        let after = read_agent_field(&path, "cody", "model.model")
            .unwrap()
            .unwrap();
        assert_eq!(after, Value::String("claude-opus-4-7".into()));
    }

    #[test]
    fn apply_patch_remove_clears_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let patch = remove_patch("cody", "language");
        apply_patch_with_denylist(&path, &patch).unwrap();
        let after = read_agent_field(&path, "cody", "language").unwrap();
        assert!(after.is_none());
    }

    #[test]
    fn apply_patch_blocks_pairing_glob() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let patch = upsert_patch(
            "cody",
            "pairing.session_token",
            Value::String("sneaky".into()),
        );
        let err = apply_patch_with_denylist(&path, &patch).unwrap_err();
        match err {
            ApplyError::Forbidden(fk) => {
                assert!(
                    fk.matched_glob == "pairing.*" || fk.matched_glob == "*_token",
                    "got `{}`",
                    fk.matched_glob
                );
                assert_eq!(fk.path, "pairing.session_token");
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn apply_patch_idempotent_when_value_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path());
        let patch = upsert_patch("cody", "language", Value::String("en".into()));
        apply_patch_with_denylist(&path, &patch).unwrap();
        apply_patch_with_denylist(&path, &patch).unwrap();
        let after = read_agent_field(&path, "cody", "language")
            .unwrap()
            .unwrap();
        assert_eq!(after, Value::String("en".into()));
    }
}

// ─────────────────────────────────────────────────────────────────────
// Phase 82.10.h.3 helpers — whole-block agent removal + llm.yaml
// `providers.<id>` mapping helpers, consumed by the admin RPC
// production adapters in `crate::admin_adapters`.
// ─────────────────────────────────────────────────────────────────────

/// Drop the entire `agents.yaml.<agent_id>` block. Idempotent — if
/// the id is absent the file is left untouched and `Ok(false)` is
/// returned. Atomic via the same temp+rename path as `upsert`.
pub fn remove_agent_block(path: &Path, agent_id: &str) -> Result<bool> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

    let removed = {
        let agents = match root.get_mut("agents").and_then(Value::as_sequence_mut) {
            Some(s) => s,
            None => return Ok(false),
        };
        let before = agents.len();
        agents.retain(|it| it.get("id").and_then(Value::as_str) != Some(agent_id));
        before != agents.len()
    };
    if removed {
        write_atomic(path, &root)?;
    }
    Ok(removed)
}

/// List every `providers.<id>` key in `llm.yaml` source order.
/// Returns an empty vec when the file is absent or has no
/// `providers` mapping.
pub fn list_llm_provider_ids(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let providers = match root.get("providers").and_then(Value::as_mapping) {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };
    Ok(providers
        .keys()
        .filter_map(|k| k.as_str().map(String::from))
        .collect())
}

/// Read a dotted field under `providers.<provider_id>.*`. Mirrors
/// `read_agent_field` but for the mapping-keyed `llm.yaml` shape.
pub fn read_llm_provider_field(
    path: &Path,
    provider_id: &str,
    dotted: &str,
) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let provider = match root
        .get("providers")
        .and_then(|p| p.get(provider_id))
    {
        Some(p) => p,
        None => return Ok(None),
    };
    let mut cur: &Value = provider;
    for segment in dotted.split('.') {
        match cur.get(segment) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur.clone()))
}

/// Upsert a dotted field under `providers.<provider_id>.*`. Creates
/// the provider mapping (and `providers:` itself) when absent.
pub fn upsert_llm_provider_field(
    path: &Path,
    provider_id: &str,
    dotted: &str,
    value: Value,
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if path.exists() {
        let text =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)
                .with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        Value::Mapping(Mapping::new())
    };

    if !root.is_mapping() {
        anyhow::bail!("{} root is not a mapping", path.display());
    }
    {
        let root_map = root.as_mapping_mut().unwrap();
        let providers_key = Value::String("providers".into());
        if !root_map.contains_key(&providers_key) {
            root_map.insert(providers_key.clone(), Value::Mapping(Mapping::new()));
        }
        let providers = root_map
            .get_mut(&providers_key)
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| anyhow::anyhow!("`providers:` is not a mapping in {}", path.display()))?;
        let provider_key = Value::String(provider_id.into());
        if !providers.contains_key(&provider_key) {
            providers.insert(provider_key.clone(), Value::Mapping(Mapping::new()));
        }
        let provider = providers
            .get_mut(&provider_key)
            .ok_or_else(|| anyhow::anyhow!("provider `{provider_id}` lookup failed"))?;
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            anyhow::bail!("empty dotted path");
        }
        set_path(provider, &parts, value)?;
    }
    write_atomic(path, &root)
}

/// Drop the entire `providers.<provider_id>` mapping. Idempotent —
/// returns `Ok(false)` when the id is already absent.
pub fn remove_llm_provider(path: &Path, provider_id: &str) -> Result<bool> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let removed = {
        let providers = match root
            .get_mut("providers")
            .and_then(Value::as_mapping_mut)
        {
            Some(m) => m,
            None => return Ok(false),
        };
        providers
            .remove(&Value::String(provider_id.into()))
            .is_some()
    };
    if removed {
        write_atomic(path, &root)?;
    }
    Ok(removed)
}

// ── Phase 83.8.12.5.c.b — tenant-scoped provider helpers ──
//
// Operate on `tenants.<tenant_id>.providers.<provider_id>.*`.
// Mirror the global `*_llm_provider_*` helpers above —
// same root-mapping semantics, same atomic write, same lock
// — just nested under the `tenants.<id>` namespace.

/// List provider ids under `tenants.<tenant_id>.providers`.
/// Empty when the tenant has no providers block (or the
/// tenant or `tenants:` are absent).
pub fn list_llm_tenant_provider_ids(
    path: &Path,
    tenant_id: &str,
) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let providers = match root
        .get("tenants")
        .and_then(|t| t.get(tenant_id))
        .and_then(|t| t.get("providers"))
        .and_then(Value::as_mapping)
    {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };
    Ok(providers
        .keys()
        .filter_map(|k| k.as_str().map(String::from))
        .collect())
}

/// Read a dotted field under
/// `tenants.<tenant_id>.providers.<provider_id>.*`.
pub fn read_llm_tenant_provider_field(
    path: &Path,
    tenant_id: &str,
    provider_id: &str,
    dotted: &str,
) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let provider = match root
        .get("tenants")
        .and_then(|t| t.get(tenant_id))
        .and_then(|t| t.get("providers"))
        .and_then(|p| p.get(provider_id))
    {
        Some(p) => p,
        None => return Ok(None),
    };
    let mut cur: &Value = provider;
    for segment in dotted.split('.') {
        match cur.get(segment) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(Some(cur.clone()))
}

/// Upsert a dotted field under
/// `tenants.<tenant_id>.providers.<provider_id>.*`. Creates
/// every intermediate mapping (`tenants:`, `tenants.<id>:`,
/// `.providers:`, `.providers.<provider_id>:`) on demand.
pub fn upsert_llm_tenant_provider_field(
    path: &Path,
    tenant_id: &str,
    provider_id: &str,
    dotted: &str,
    value: Value,
) -> Result<()> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if path.exists() {
        let text =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)
                .with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        Value::Mapping(Mapping::new())
    };

    if !root.is_mapping() {
        anyhow::bail!("{} root is not a mapping", path.display());
    }
    {
        let root_map = root.as_mapping_mut().unwrap();
        let tenants_key = Value::String("tenants".into());
        if !root_map.contains_key(&tenants_key) {
            root_map.insert(tenants_key.clone(), Value::Mapping(Mapping::new()));
        }
        let tenants = root_map
            .get_mut(&tenants_key)
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| anyhow::anyhow!("`tenants:` is not a mapping in {}", path.display()))?;
        let tenant_key = Value::String(tenant_id.into());
        if !tenants.contains_key(&tenant_key) {
            tenants.insert(tenant_key.clone(), Value::Mapping(Mapping::new()));
        }
        let tenant = tenants
            .get_mut(&tenant_key)
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| {
                anyhow::anyhow!("`tenants.{tenant_id}:` is not a mapping in {}", path.display())
            })?;
        let providers_key = Value::String("providers".into());
        if !tenant.contains_key(&providers_key) {
            tenant.insert(providers_key.clone(), Value::Mapping(Mapping::new()));
        }
        let providers = tenant
            .get_mut(&providers_key)
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "`tenants.{tenant_id}.providers:` is not a mapping in {}",
                    path.display()
                )
            })?;
        let provider_key = Value::String(provider_id.into());
        if !providers.contains_key(&provider_key) {
            providers.insert(provider_key.clone(), Value::Mapping(Mapping::new()));
        }
        let provider = providers
            .get_mut(&provider_key)
            .ok_or_else(|| anyhow::anyhow!("provider `{provider_id}` lookup failed"))?;
        let parts: Vec<&str> = dotted.split('.').collect();
        if parts.is_empty() {
            anyhow::bail!("empty dotted path");
        }
        set_path(provider, &parts, value)?;
    }
    write_atomic(path, &root)
}

/// Drop the
/// `tenants.<tenant_id>.providers.<provider_id>` mapping.
/// Idempotent — returns `Ok(false)` when the id is already
/// absent. Empty `providers` / `tenants.<id>` blocks are
/// LEFT IN PLACE on purpose: deleting a single provider must
/// not delete the whole tenant config (operator's other
/// per-tenant knobs may live there in future).
pub fn remove_llm_tenant_provider(
    path: &Path,
    tenant_id: &str,
    provider_id: &str,
) -> Result<bool> {
    let _guard = YAML_UPSERT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let mut root: Value =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let removed = {
        let providers = match root
            .get_mut("tenants")
            .and_then(|t| t.get_mut(tenant_id))
            .and_then(|t| t.get_mut("providers"))
            .and_then(Value::as_mapping_mut)
        {
            Some(m) => m,
            None => return Ok(false),
        };
        providers
            .remove(&Value::String(provider_id.into()))
            .is_some()
    };
    if removed {
        write_atomic(path, &root)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod admin_adapter_helper_tests {
    use super::*;

    fn write_yaml(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn remove_agent_block_drops_matching_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n  - id: bob\n    model:\n      provider: anthropic\n",
        );
        let removed = remove_agent_block(&path, "ana").unwrap();
        assert!(removed);
        let ids = list_agent_ids(&path).unwrap();
        assert_eq!(ids, vec!["bob".to_string()]);
    }

    #[test]
    fn remove_agent_block_unknown_id_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "agents.yaml",
            "agents:\n  - id: ana\n    model:\n      provider: minimax\n",
        );
        let removed = remove_agent_block(&path, "ghost").unwrap();
        assert!(!removed);
        assert_eq!(list_agent_ids(&path).unwrap(), vec!["ana".to_string()]);
    }

    #[test]
    fn list_llm_provider_ids_returns_mapping_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  minimax:\n    base_url: https://x\n  anthropic:\n    base_url: https://y\n",
        );
        let mut ids = list_llm_provider_ids(&path).unwrap();
        ids.sort();
        assert_eq!(ids, vec!["anthropic".to_string(), "minimax".into()]);
    }

    #[test]
    fn read_and_upsert_llm_provider_field_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  minimax:\n    base_url: https://api.minimax.io\n",
        );
        let v = read_llm_provider_field(&path, "minimax", "base_url")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("https://api.minimax.io"));

        upsert_llm_provider_field(
            &path,
            "minimax",
            "api_key_env",
            Value::String("MINIMAX_API_KEY".into()),
        )
        .unwrap();
        let env = read_llm_provider_field(&path, "minimax", "api_key_env")
            .unwrap()
            .unwrap();
        assert_eq!(env.as_str(), Some("MINIMAX_API_KEY"));
    }

    #[test]
    fn upsert_llm_provider_field_creates_missing_provider_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.yaml");
        upsert_llm_provider_field(
            &path,
            "newp",
            "base_url",
            Value::String("https://new".into()),
        )
        .unwrap();
        let v = read_llm_provider_field(&path, "newp", "base_url")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("https://new"));
    }

    #[test]
    fn remove_llm_provider_drops_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  retired:\n    base_url: https://x\n  active:\n    base_url: https://y\n",
        );
        let removed = remove_llm_provider(&path, "retired").unwrap();
        assert!(removed);
        let ids = list_llm_provider_ids(&path).unwrap();
        assert_eq!(ids, vec!["active".to_string()]);
    }

    #[test]
    fn remove_llm_provider_unknown_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  active:\n    base_url: https://y\n",
        );
        assert!(!remove_llm_provider(&path, "ghost").unwrap());
        assert_eq!(
            list_llm_provider_ids(&path).unwrap(),
            vec!["active".to_string()]
        );
    }

    // ── Phase 83.8.12.5.c.b — tenant-scoped helpers ──

    #[test]
    fn upsert_llm_tenant_provider_creates_nested_blocks_from_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.yaml");
        // File doesn't exist yet — helper builds the whole
        // hierarchy.
        upsert_llm_tenant_provider_field(
            &path,
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme.minimax".into()),
        )
        .unwrap();
        upsert_llm_tenant_provider_field(
            &path,
            "acme",
            "minimax",
            "api_key_env",
            Value::String("ACME_KEY".into()),
        )
        .unwrap();
        // Read back the values.
        let v = read_llm_tenant_provider_field(&path, "acme", "minimax", "base_url")
            .unwrap()
            .unwrap();
        assert_eq!(v.as_str(), Some("https://acme.minimax"));
        let ids = list_llm_tenant_provider_ids(&path, "acme").unwrap();
        assert_eq!(ids, vec!["minimax".to_string()]);
    }

    #[test]
    fn list_llm_tenant_provider_ids_isolates_tenants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.yaml");
        upsert_llm_tenant_provider_field(
            &path,
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        upsert_llm_tenant_provider_field(
            &path,
            "globex",
            "openai",
            "base_url",
            Value::String("https://globex".into()),
        )
        .unwrap();
        assert_eq!(
            list_llm_tenant_provider_ids(&path, "acme").unwrap(),
            vec!["minimax".to_string()]
        );
        assert_eq!(
            list_llm_tenant_provider_ids(&path, "globex").unwrap(),
            vec!["openai".to_string()]
        );
        // Unknown tenant returns empty (not error).
        assert!(list_llm_tenant_provider_ids(&path, "ghost")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn remove_llm_tenant_provider_only_drops_target_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.yaml");
        // Seed: tenant `acme` with `minimax` AND `openai`.
        upsert_llm_tenant_provider_field(
            &path,
            "acme",
            "minimax",
            "base_url",
            Value::String("https://m".into()),
        )
        .unwrap();
        upsert_llm_tenant_provider_field(
            &path,
            "acme",
            "openai",
            "base_url",
            Value::String("https://o".into()),
        )
        .unwrap();
        let removed = remove_llm_tenant_provider(&path, "acme", "minimax").unwrap();
        assert!(removed);
        // Sibling provider intact.
        assert_eq!(
            list_llm_tenant_provider_ids(&path, "acme").unwrap(),
            vec!["openai".to_string()]
        );
    }

    #[test]
    fn tenant_and_global_provider_with_same_id_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_yaml(
            dir.path(),
            "llm.yaml",
            "providers:\n  minimax:\n    base_url: https://global\n",
        );
        upsert_llm_tenant_provider_field(
            &path,
            "acme",
            "minimax",
            "base_url",
            Value::String("https://acme".into()),
        )
        .unwrap();
        // Both reads succeed independently.
        assert_eq!(
            read_llm_provider_field(&path, "minimax", "base_url")
                .unwrap()
                .unwrap()
                .as_str(),
            Some("https://global")
        );
        assert_eq!(
            read_llm_tenant_provider_field(&path, "acme", "minimax", "base_url")
                .unwrap()
                .unwrap()
                .as_str(),
            Some("https://acme")
        );
    }
}
