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
    let v: Value = serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
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
    let v: Value = serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
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
pub fn list_agent_ids(file: &Path) -> Result<Vec<String>> {
    if !file.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_yaml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let seq = v
        .get("agents")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    Ok(seq
        .into_iter()
        .filter_map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect())
}

/// Remove `item` from `agents[<agent_id>].<list_key>`. Returns `true`
/// when the file was modified, `false` if the item wasn't there.
pub fn remove_list_entry(
    file: &Path,
    agent_id: &str,
    list_key: &str,
    item: &str,
) -> Result<bool> {
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

/// Shared helper: append `item` to `agents[<id>].<list_key>`. Returns
/// `true` when the file was modified, `false` if the item already
/// existed.
fn add_list_entry(
    file: &Path,
    agent_id: &str,
    list_key: &str,
    item: &str,
) -> Result<bool> {
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
    let main = config_dir.join("agents.yaml");
    if main.exists() {
        let ids = list_agent_ids(&main).unwrap_or_default();
        if ids.iter().any(|id| id == agent_id) {
            return Ok(Some(main));
        }
    }
    let drop_dir = config_dir.join("agents.d");
    if !drop_dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&drop_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        if list_agent_ids(&path).unwrap_or_default().iter().any(|id| id == agent_id) {
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
    entry.insert(Value::String("instance".into()), Value::String(instance.into()));
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
    entry.insert(Value::String("instance".into()), Value::String(instance.into()));
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

