use crate::registry::*;

pub fn defs() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            id: "nats",
            label: "NATS broker",
            category: Category::Infra,
            description: Some("URL del broker NATS. Vacío = fallback local in-process."),
            fields: vec![FieldDef {
                key: "url",
                label: "NATS URL",
                help: Some("Ej: nats://127.0.0.1:4222"),
                kind: FieldKind::Text,
                required: false,
                default: Some("nats://127.0.0.1:4222"),
                target: FieldTarget::EnvOnly("NATS_URL"),
                validator: None,
            }],
        },
        ServiceDef {
            id: "tmux-remote",
            label: "tmux-remote extension",
            category: Category::Infra,
            description: Some(
                "Ruta al socket tmux dedicado para la extension. Aislado del tmux del operador.",
            ),
            fields: vec![FieldDef {
                key: "socket",
                label: "TMUX_REMOTE_SOCKET",
                help: Some("Ej: /tmp/agent-rs-tmux.sock"),
                kind: FieldKind::Text,
                required: false,
                default: None,
                target: FieldTarget::EnvOnly("TMUX_REMOTE_SOCKET"),
                validator: None,
            }],
        },
        ServiceDef {
            id: "taskflow",
            label: "TaskFlow DB",
            category: Category::Infra,
            description: Some("Ruta del SQLite que persiste los flujos (Phase 14)."),
            fields: vec![FieldDef {
                key: "db_path",
                label: "TaskFlow DB path",
                help: None,
                kind: FieldKind::Text,
                required: false,
                default: Some("./data/taskflow.db"),
                target: FieldTarget::EnvOnly("TASKFLOW_DB_PATH"),
                validator: None,
            }],
        },
    ]
}
