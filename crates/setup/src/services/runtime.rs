use crate::registry::*;

pub fn defs() -> Vec<ServiceDef> {
    vec![ServiceDef {
        id: "runtime",
        label: "Runtime / logging",
        category: Category::Runtime,
        description: Some("Variables de observabilidad que lee el bin `agent`."),
        fields: vec![
            FieldDef {
                key: "agent_env",
                label: "AGENT_ENV",
                help: Some("dev / staging / production"),
                kind: FieldKind::Choice(&["dev", "staging", "production"]),
                required: false,
                default: Some("dev"),
                target: FieldTarget::EnvOnly("AGENT_ENV"),
                validator: None,
            },
            FieldDef {
                key: "log_format",
                label: "AGENT_LOG_FORMAT",
                help: Some("pretty | compact | json"),
                kind: FieldKind::Choice(&["pretty", "compact", "json"]),
                required: false,
                default: Some("pretty"),
                target: FieldTarget::EnvOnly("AGENT_LOG_FORMAT"),
                validator: None,
            },
            FieldDef {
                key: "rust_log",
                label: "RUST_LOG",
                help: Some("Ej: info, agent=debug, tokio=warn"),
                kind: FieldKind::Text,
                required: false,
                default: Some("info"),
                target: FieldTarget::EnvOnly("RUST_LOG"),
                validator: None,
            },
        ],
    }]
}
