use crate::registry::*;

pub fn defs() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            id: "minimax",
            label: "MiniMax LLM (primario)",
            category: Category::Llm,
            description: Some(
                "Dos variantes de API key reconocidas (mismo fallback que OpenClaw):\n\
                 · `plan` → Coding/Token Plan key (empieza con `sk-cp-…`) → \
                 secrets/minimax_code_plan_key.txt\n\
                 · `api`  → API key general (empieza con `sk-…`) → \
                 secrets/minimax_api_key.txt\n\
                 El cliente prueba CODE_PLAN_KEY → CODING_API_KEY → API_KEY.",
            ),
            fields: vec![
                FieldDef {
                    key: "key_kind",
                    label: "Tipo de key",
                    help: Some("plan = Coding/Token Plan (`sk-cp-…`) · api = API key general (`sk-…`)"),
                    kind: FieldKind::Choice(&["plan", "api"]),
                    required: true,
                    default: Some("plan"),
                    target: FieldTarget::EnvOnly("MINIMAX_KEY_KIND"),
                    validator: None,
                },
                FieldDef {
                    key: "key_value",
                    label: "Key value",
                    help: Some("Pega el string completo tal como lo copiaste del dashboard."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    // Written by a custom branch in writer::persist: the
                    // actual destination file depends on `key_kind`.
                    target: FieldTarget::EnvOnly("MINIMAX_KEY_VALUE"),
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "group_id",
                    label: "MiniMax group ID",
                    help: Some("Dashboard → Account → Group. String numérico."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "minimax_group_id.txt",
                        env_var: "MINIMAX_GROUP_ID",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "region",
                    label: "Región (ajusta base_url)",
                    help: Some("global = api.minimax.io · cn = api.minimaxi.com · chat = api.minimax.chat (legacy)"),
                    kind: FieldKind::Choice(&["global", "cn", "chat"]),
                    required: true,
                    default: Some("global"),
                    target: FieldTarget::EnvOnly("MINIMAX_REGION"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "openai",
            label: "OpenAI LLM",
            category: Category::Llm,
            description: Some("API key OpenAI. Usado por proveedor `openai` en llm.yaml."),
            fields: vec![FieldDef {
                key: "api_key",
                label: "OpenAI API key",
                help: Some("Empieza con `sk-…`"),
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "openai_api_key.txt",
                    env_var: "OPENAI_API_KEY",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "anthropic",
            label: "Anthropic Claude (openai-compat)",
            category: Category::Llm,
            description: Some("API key Anthropic. Requiere configurar base_url apropiado en llm.yaml."),
            fields: vec![FieldDef {
                key: "api_key",
                label: "Anthropic API key",
                help: Some("Empieza con `sk-ant-…`"),
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "anthropic_api_key.txt",
                    env_var: "ANTHROPIC_API_KEY",
                },
                validator: Some(validate_nonempty),
            }],
        },
    ]
}
