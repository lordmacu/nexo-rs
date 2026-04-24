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
    let _guard = YAML_UPSERT_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut root: Value = if file.exists() {
        let text = fs::read_to_string(file)
            .with_context(|| format!("read {}", file.display()))?;
        if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&text)
                .with_context(|| format!("parse {}", file.display()))?
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
                    let n: i64 = trimmed.parse().with_context(|| {
                        format!("list item '{trimmed}' is not a valid integer")
                    })?;
                    items.push(Value::Number(n.into()));
                }
                Value::Sequence(items)
            }
        }
    })
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
