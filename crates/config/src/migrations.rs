use anyhow::{bail, Context, Result};
use serde_yaml::{Mapping, Number, Value};
use std::path::Path;

pub const LATEST_SCHEMA_VERSION: u64 = 11;

#[derive(Debug, Clone)]
pub struct FileReport {
    pub file: String,
    pub from_version: u64,
    pub to_version: u64,
    pub changed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    pub files: Vec<FileReport>,
}

impl MigrationReport {
    pub fn changed_count(&self) -> usize {
        self.files.iter().filter(|f| f.changed).count()
    }
}

pub fn migrate_config_dir(config_dir: &Path, apply: bool) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();
    for file in candidate_files() {
        let path = config_dir.join(file);
        if !path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let before: Value = serde_yaml::from_str(&raw)
            .with_context(|| format!("invalid YAML {}", path.display()))?;
        let before_version = schema_version(&before);
        let after = apply_migrations(before.clone())?;
        let after_version = schema_version(&after);
        let changed = before != after;

        if apply && changed {
            let serialized = serde_yaml::to_string(&after)
                .with_context(|| format!("cannot serialize migrated YAML {}", path.display()))?;
            std::fs::write(&path, serialized)
                .with_context(|| format!("cannot write {}", path.display()))?;
        }

        report.files.push(FileReport {
            file: file.to_string(),
            from_version: before_version,
            to_version: after_version,
            changed,
        });
    }
    Ok(report)
}

pub fn apply_migrations(mut doc: Value) -> Result<Value> {
    ensure_mapping(&doc)?;
    let mut v = schema_version(&doc);
    while v < LATEST_SCHEMA_VERSION {
        doc = match v + 1 {
            1 => migration_1_set_schema(doc),
            2 => migration_2_agent_to_agents(doc),
            3 => migration_3_inbound_binding_plural(doc),
            4 => migration_4_allowed_tool_plural(doc),
            5 => migration_5_plugins_mapping_to_seq(doc),
            6 => migration_6_proactive_bool_to_object(doc),
            7 => migration_7_default_missing_collections(doc),
            8 => migration_8_trim_plugin_ids(doc),
            9 => migration_9_normalize_worker_tools_alias(doc),
            10 => migration_10_remove_empty_strings(doc),
            11 => migration_11_sort_unique_string_lists(doc),
            _ => unreachable!(),
        };
        set_schema_version(&mut doc, v + 1);
        v += 1;
    }
    Ok(doc)
}

fn candidate_files() -> &'static [&'static str] {
    &[
        "agents.yaml",
        "broker.yaml",
        "llm.yaml",
        "memory.yaml",
        "plugins.yaml",
        "extensions.yaml",
        "mcp.yaml",
        "mcp_server.yaml",
        "runtime.yaml",
        "pollers.yaml",
        "taskflow.yaml",
        "transcripts.yaml",
        "pairing.yaml",
        "telegram.yaml",
        "whatsapp.yaml",
        "google-auth.yaml",
        "email.yaml",
    ]
}

fn schema_version(doc: &Value) -> u64 {
    doc.as_mapping()
        .and_then(|m| m.get(Value::String("schema_version".into())))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn set_schema_version(doc: &mut Value, version: u64) {
    if let Some(map) = doc.as_mapping_mut() {
        map.insert(
            Value::String("schema_version".into()),
            Value::Number(Number::from(version)),
        );
    }
}

fn ensure_mapping(doc: &Value) -> Result<()> {
    if !doc.is_mapping() {
        bail!("top-level YAML document must be a mapping")
    }
    Ok(())
}

fn key(k: &str) -> Value {
    Value::String(k.to_string())
}

fn get_mut_map<'a>(v: &'a mut Value) -> Option<&'a mut Mapping> {
    v.as_mapping_mut()
}

fn migration_1_set_schema(doc: Value) -> Value {
    doc
}

fn migration_2_agent_to_agents(mut doc: Value) -> Value {
    if let Some(map) = get_mut_map(&mut doc) {
        if !map.contains_key(key("agents")) {
            if let Some(agent) = map.remove(key("agent")) {
                map.insert(key("agents"), Value::Sequence(vec![agent]));
            }
        }
    }
    doc
}

fn migration_3_inbound_binding_plural(mut doc: Value) -> Value {
    rename_key_to_seq(&mut doc, "inbound_binding", "inbound_bindings");
    walk_agents(&mut doc, |agent| {
        rename_key_to_seq(agent, "inbound_binding", "inbound_bindings")
    });
    doc
}

fn migration_4_allowed_tool_plural(mut doc: Value) -> Value {
    rename_key_to_seq(&mut doc, "allowed_tool", "allowed_tools");
    walk_agents(&mut doc, |agent| {
        rename_key_to_seq(agent, "allowed_tool", "allowed_tools");
        walk_bindings(agent, |b| {
            rename_key_to_seq(b, "allowed_tool", "allowed_tools")
        });
    });
    doc
}

fn migration_5_plugins_mapping_to_seq(mut doc: Value) -> Value {
    if let Some(map) = get_mut_map(&mut doc) {
        if let Some(existing) = map.get_mut(key("plugins")) {
            if existing.is_mapping() {
                let one = existing.clone();
                *existing = Value::Sequence(vec![one]);
            }
        }
    }
    doc
}

fn migration_6_proactive_bool_to_object(mut doc: Value) -> Value {
    coerce_bool_object(&mut doc, "proactive", "enabled");
    walk_agents(&mut doc, |agent| {
        coerce_bool_object(agent, "proactive", "enabled");
        walk_bindings(agent, |b| coerce_bool_object(b, "proactive", "enabled"));
    });
    doc
}

fn migration_7_default_missing_collections(mut doc: Value) -> Value {
    walk_agents(&mut doc, |agent| {
        ensure_seq(agent, "plugins");
        ensure_seq(agent, "allowed_tools");
        ensure_seq(agent, "inbound_bindings");
    });
    doc
}

fn migration_8_trim_plugin_ids(mut doc: Value) -> Value {
    walk_seq_strings(&mut doc, "plugins", |s| s.trim().to_string());
    walk_agents(&mut doc, |agent| {
        walk_seq_strings(agent, "plugins", |s| s.trim().to_string())
    });
    doc
}

fn migration_9_normalize_worker_tools_alias(mut doc: Value) -> Value {
    walk_agents(&mut doc, |agent| {
        walk_bindings(agent, |b| {
            if let Some(map) = b.as_mapping_mut() {
                if !map.contains_key(key("allowed_tools")) {
                    if let Some(v) = map.remove(key("worker_tools")) {
                        map.insert(key("allowed_tools"), v);
                    }
                }
            }
        })
    });
    doc
}

fn migration_10_remove_empty_strings(doc: Value) -> Value {
    // Keep this migration intentionally conservative: removing empty
    // strings at arbitrary paths can break required string fields in
    // existing configs. We reserve destructive normalization for
    // field-specific migrations only.
    doc
}

fn migration_11_sort_unique_string_lists(mut doc: Value) -> Value {
    dedup_sort_seq_strings(&mut doc, "plugins");
    dedup_sort_seq_strings(&mut doc, "allowed_tools");
    walk_agents(&mut doc, |agent| {
        dedup_sort_seq_strings(agent, "plugins");
        dedup_sort_seq_strings(agent, "allowed_tools");
        walk_bindings(agent, |b| dedup_sort_seq_strings(b, "allowed_tools"));
    });
    doc
}

fn rename_key_to_seq(doc: &mut Value, from: &str, to: &str) {
    if let Some(map) = doc.as_mapping_mut() {
        if map.contains_key(key(to)) {
            return;
        }
        if let Some(v) = map.remove(key(from)) {
            let seq = match v {
                Value::Sequence(s) => Value::Sequence(s),
                other => Value::Sequence(vec![other]),
            };
            map.insert(key(to), seq);
        }
    }
}

fn coerce_bool_object(doc: &mut Value, field: &str, bool_key: &str) {
    if let Some(map) = doc.as_mapping_mut() {
        if let Some(v) = map.get_mut(key(field)) {
            if let Value::Bool(b) = v {
                let mut m = Mapping::new();
                m.insert(key(bool_key), Value::Bool(*b));
                *v = Value::Mapping(m);
            }
        }
    }
}

fn ensure_seq(doc: &mut Value, field: &str) {
    if let Some(map) = doc.as_mapping_mut() {
        map.entry(key(field))
            .or_insert_with(|| Value::Sequence(vec![]));
    }
}

fn walk_agents<F>(doc: &mut Value, mut f: F)
where
    F: FnMut(&mut Value),
{
    if let Some(map) = doc.as_mapping_mut() {
        if let Some(Value::Sequence(agents)) = map.get_mut(key("agents")) {
            for agent in agents {
                f(agent);
            }
        }
    }
}

fn walk_bindings<F>(agent: &mut Value, mut f: F)
where
    F: FnMut(&mut Value),
{
    if let Some(map) = agent.as_mapping_mut() {
        if let Some(Value::Sequence(bindings)) = map.get_mut(key("inbound_bindings")) {
            for binding in bindings {
                f(binding);
            }
        }
    }
}

fn walk_seq_strings<F>(doc: &mut Value, field: &str, mut map_fn: F)
where
    F: FnMut(&str) -> String,
{
    if let Some(m) = doc.as_mapping_mut() {
        if let Some(Value::Sequence(items)) = m.get_mut(key(field)) {
            for v in items {
                if let Value::String(s) = v {
                    *s = map_fn(s);
                }
            }
        }
    }
}

fn dedup_sort_seq_strings(doc: &mut Value, field: &str) {
    if let Some(m) = doc.as_mapping_mut() {
        if let Some(Value::Sequence(items)) = m.get_mut(key(field)) {
            let mut vals: Vec<String> = items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect();
            vals.sort();
            vals.dedup();
            *items = vals.into_iter().map(Value::String).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent_when_latest() {
        let input: Value = serde_yaml::from_str(
            "schema_version: 11\nagents: []\nplugins: [a, b, a]\nallowed_tools: [bash, bash]\n",
        )
        .unwrap();
        let once = apply_migrations(input.clone()).unwrap();
        let twice = apply_migrations(once.clone()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn upgrades_and_sets_latest_version() {
        let input: Value = serde_yaml::from_str(
            "agent: {id: one}\nallowed_tool: bash\ninbound_binding: {channel: telegram}\nproactive: true\n",
        )
        .unwrap();
        let out = apply_migrations(input).unwrap();
        assert_eq!(schema_version(&out), LATEST_SCHEMA_VERSION);
        let m = out.as_mapping().unwrap();
        assert!(m.contains_key(key("agents")));
        assert!(m.contains_key(key("allowed_tools")));
    }
}
