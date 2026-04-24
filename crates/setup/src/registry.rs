//! Declarative service / field catalog.
//!
//! Each [`ServiceDef`] describes a unit the user can configure —
//! typically one LLM provider, plugin, or skill. [`FieldDef`] enumerates
//! the pieces of data we need to ask for, where each value lives
//! (secrets file, YAML path, or just an env var), and how to validate
//! it before writing.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Agent,
    Llm,
    Memory,
    Plugin,
    Skill,
    Infra,
    Runtime,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "Agente",
            Self::Llm => "LLM",
            Self::Memory => "Memory",
            Self::Plugin => "Plugin",
            Self::Skill => "Skill",
            Self::Infra => "Infra",
            Self::Runtime => "Runtime",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// Hidden input, persisted to `secrets/<secret_file>`.
    Secret,
    /// Free-form text.
    Text,
    /// Comma-separated list.
    List,
    /// Integer.
    Number,
    /// y/N prompt → "true" / "false".
    Bool,
    /// Pick one from the provided options.
    Choice(&'static [&'static str]),
}

#[derive(Debug, Clone)]
pub enum FieldTarget {
    /// Write the value to `secrets/<file>` and reference it via
    /// `${file:/run/secrets/<file_stem>}` or `${<env_var>}`.
    Secret {
        file: &'static str,
        env_var: &'static str,
    },
    /// Upsert into a YAML file at a dotted path
    /// (e.g. `plugins/whatsapp.yaml::whatsapp.enabled`).
    Yaml {
        file: &'static str,
        path: &'static str,
    },
    /// Only expose as an env var — no disk state from the wizard.
    EnvOnly(&'static str),
}

pub type FieldValidator = fn(&str) -> Result<(), String>;

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub key: &'static str,
    pub label: &'static str,
    pub help: Option<&'static str>,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<&'static str>,
    pub target: FieldTarget,
    pub validator: Option<FieldValidator>,
}

#[derive(Debug, Clone)]
pub struct ServiceDef {
    pub id: &'static str,
    pub label: &'static str,
    pub category: Category,
    pub description: Option<&'static str>,
    pub fields: Vec<FieldDef>,
}

/// Captured user input for a run of one service form. Keys mirror
/// [`FieldDef::key`].
#[derive(Debug, Default, Clone)]
pub struct ServiceValues {
    inner: BTreeMap<String, String>,
}

impl ServiceValues {
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.inner.insert(key.into(), value.into());
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.inner.get(key).map(String::as_str)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.inner.iter()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

// ── Shared validators ──────────────────────────────────────────────────────

pub fn validate_nonempty(s: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        Err("value cannot be empty".into())
    } else {
        Ok(())
    }
}

pub fn validate_https_url(s: &str) -> Result<(), String> {
    let s = s.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        Ok(())
    } else {
        Err("must start with http:// or https://".into())
    }
}

pub fn validate_host(s: &str) -> Result<(), String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("host cannot be empty".into());
    }
    if s.contains(' ') {
        return Err("host cannot contain spaces".into());
    }
    Ok(())
}

pub fn validate_telegram_token(s: &str) -> Result<(), String> {
    let s = s.trim();
    // Telegram bot tokens are "<bot_id>:<35 char base64url-ish>"
    if !s.contains(':') || s.len() < 35 {
        Err("expected format: 123456:ABC-DEF...".into())
    } else {
        Ok(())
    }
}

pub fn validate_port(s: &str) -> Result<(), String> {
    match s.trim().parse::<u32>() {
        Ok(n) if (1..=65535).contains(&n) => Ok(()),
        _ => Err("port must be 1..=65535".into()),
    }
}
