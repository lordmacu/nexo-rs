//! Configuration-status audit. Reads the secrets directory + config
//! YAMLs and reports which service fields are satisfied vs missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::registry::{FieldDef, FieldTarget, ServiceDef};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldStatus {
    Configured,
    Missing,
    NotRequired,
}

#[derive(Debug, Clone)]
pub struct FieldReport {
    pub key: String,
    pub label: String,
    pub required: bool,
    pub status: FieldStatus,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub service_id: String,
    pub service_label: String,
    pub category: crate::registry::Category,
    pub fields: Vec<FieldReport>,
}

impl ServiceStatus {
    pub fn is_fully_configured(&self) -> bool {
        self.fields
            .iter()
            .all(|f| matches!(f.status, FieldStatus::Configured | FieldStatus::NotRequired))
    }
    pub fn is_partially_configured(&self) -> bool {
        self.fields.iter().any(|f| f.status == FieldStatus::Configured)
    }
}

#[derive(Debug, Clone, Default)]
pub struct StatusReport {
    pub services: Vec<ServiceStatus>,
}

impl StatusReport {
    pub fn missing_required(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.services {
            for f in &s.fields {
                if f.required && f.status == FieldStatus::Missing {
                    out.push(format!("{}::{}", s.service_id, f.key));
                }
            }
        }
        out
    }
}

pub fn audit(services: &[ServiceDef], secrets_dir: &Path, config_dir: &Path) -> StatusReport {
    let mut yaml_cache: HashMap<PathBuf, serde_yaml::Value> = HashMap::new();
    let mut services_out = Vec::with_capacity(services.len());
    for svc in services {
        // `minimax` has a bespoke audit: the form transports `key_kind`
        // / `key_value` / `region` via EnvOnly slots that are never set
        // in the runtime env, so the generic field audit would always
        // report them missing. Check for the actual on-disk secrets
        // + YAML patch instead.
        if svc.id == "minimax" {
            services_out.push(audit_minimax(svc, secrets_dir, config_dir));
            continue;
        }
        let mut fields_out = Vec::with_capacity(svc.fields.len());
        for field in &svc.fields {
            fields_out.push(audit_field(field, secrets_dir, config_dir, &mut yaml_cache));
        }
        services_out.push(ServiceStatus {
            service_id: svc.id.to_string(),
            service_label: svc.label.to_string(),
            category: svc.category,
            fields: fields_out,
        });
    }
    StatusReport { services: services_out }
}

fn audit_minimax(svc: &ServiceDef, secrets_dir: &Path, _config_dir: &Path) -> ServiceStatus {
    let plan = secrets_dir.join("minimax_code_plan_key.txt");
    let api = secrets_dir.join("minimax_api_key.txt");
    let group = secrets_dir.join("minimax_group_id.txt");

    let any_key = plan_configured_or(&plan) || plan_configured_or(&api);
    let group_ok = plan_configured_or(&group);

    let key_report = FieldReport {
        key: "key".into(),
        label: "Key (plan o api)".into(),
        required: true,
        status: if any_key { FieldStatus::Configured } else { FieldStatus::Missing },
        source: "secrets/minimax_{code_plan,api}_key.txt".into(),
    };
    let group_report = FieldReport {
        key: "group_id".into(),
        label: "MiniMax group ID".into(),
        required: true,
        status: if group_ok { FieldStatus::Configured } else { FieldStatus::Missing },
        source: "secrets/minimax_group_id.txt".into(),
    };
    ServiceStatus {
        service_id: svc.id.to_string(),
        service_label: svc.label.to_string(),
        category: svc.category,
        fields: vec![key_report, group_report],
    }
}

fn plan_configured_or(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}

fn audit_field(
    field: &FieldDef,
    secrets_dir: &Path,
    config_dir: &Path,
    yaml_cache: &mut HashMap<PathBuf, serde_yaml::Value>,
) -> FieldReport {
    let (configured, source) = match &field.target {
        FieldTarget::Secret { file, env_var } => {
            let path = secrets_dir.join(file);
            if std::fs::metadata(&path)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
            {
                (true, format!("secrets/{file}"))
            } else if std::env::var(env_var).map(|v| !v.is_empty()).unwrap_or(false) {
                (true, format!("env:{env_var}"))
            } else {
                (false, format!("secrets/{file} or env:{env_var}"))
            }
        }
        FieldTarget::Yaml { file, path } => {
            let yaml_file = config_dir.join(file);
            let present = yaml_cache
                .entry(yaml_file.clone())
                .or_insert_with(|| read_yaml_or_null(&yaml_file))
                .clone();
            let found = resolve_dotted(&present, path).is_some();
            (found, format!("{file}::{path}"))
        }
        FieldTarget::EnvOnly(env_var) => {
            let set = std::env::var(env_var).map(|v| !v.is_empty()).unwrap_or(false);
            (set, format!("env:{env_var}"))
        }
    };

    let status = if configured {
        FieldStatus::Configured
    } else if field.required {
        FieldStatus::Missing
    } else {
        FieldStatus::NotRequired
    };

    FieldReport {
        key: field.key.to_string(),
        label: field.label.to_string(),
        required: field.required,
        status,
        source,
    }
}

fn read_yaml_or_null(path: &Path) -> serde_yaml::Value {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_yaml::from_str(&text).unwrap_or(serde_yaml::Value::Null),
        Err(_) => serde_yaml::Value::Null,
    }
}

fn resolve_dotted<'a>(v: &'a serde_yaml::Value, path: &str) -> Option<&'a serde_yaml::Value> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    if cur.is_null() {
        None
    } else if let Some(s) = cur.as_str() {
        if s.is_empty() || s.starts_with("${") {
            None
        } else {
            Some(cur)
        }
    } else {
        Some(cur)
    }
}

pub fn print_report(report: &StatusReport) {
    use crate::registry::Category;
    let mut by_cat: std::collections::BTreeMap<&str, Vec<&ServiceStatus>> =
        std::collections::BTreeMap::new();
    for s in &report.services {
        by_cat.entry(s.category.label()).or_default().push(s);
    }
    let ordered: &[Category] = &[
        Category::Llm,
        Category::Memory,
        Category::Plugin,
        Category::Skill,
        Category::Infra,
        Category::Runtime,
    ];
    for cat in ordered {
        let Some(list) = by_cat.get(cat.label()) else { continue };
        println!();
        println!("▎{}", cat.label());
        for s in list {
            let marker = if s.is_fully_configured() {
                "✔"
            } else if s.is_partially_configured() {
                "·"
            } else {
                "✗"
            };
            println!("  {marker} {:<40} ({})", s.service_label, s.service_id);
            for f in &s.fields {
                let m = match f.status {
                    FieldStatus::Configured => "✔",
                    FieldStatus::Missing => "✗",
                    FieldStatus::NotRequired => "·",
                };
                let req = if f.required { "" } else { " (opt)" };
                println!("      {m} {:<34} [{}]{req}", f.label, f.source);
            }
        }
    }
    println!();
    let missing = report.missing_required();
    if missing.is_empty() {
        println!("Todo requerido configurado ✔");
    } else {
        println!("Pendiente requerido: {}", missing.join(", "));
    }
}
