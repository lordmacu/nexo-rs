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
            id: "deepseek",
            label: "DeepSeek LLM",
            category: Category::Llm,
            description: Some(
                "API key DeepSeek (https://platform.deepseek.com). \
                 Wire-compatible con OpenAI — el conector reusa el cliente. \
                 Modelos: `deepseek-chat` (general) · `deepseek-reasoner` (razonamiento, sin tools).",
            ),
            fields: vec![FieldDef {
                key: "api_key",
                label: "DeepSeek API key",
                help: Some("Empieza con `sk-…` (ver https://platform.deepseek.com/api_keys)"),
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "deepseek_api_key.txt",
                    env_var: "DEEPSEEK_API_KEY",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "openai_custom",
            label: "OpenAI-compatible (Groq, OpenRouter, Together, Ollama, vLLM, ...)",
            category: Category::Llm,
            description: Some(
                "Slot genérico para cualquier proveedor que hable el wire de OpenAI: \
                 Groq, OpenRouter, Together, Fireworks, LM Studio, vLLM, Ollama, Azure \
                 OpenAI, o tu propio gateway. \n\
                 \n\
                 El wizard guarda la api_key en `secrets/openai_custom_api_key.txt` y \
                 expone `OPENAI_CUSTOM_API_KEY` + `OPENAI_CUSTOM_BASE_URL`. Después \
                 agregá el bloque a `llm.yaml`:\n\
                 \n\
                 ```yaml\n\
                 providers:\n\
                   <tu_nombre>:                  # ej. groq, together, openrouter\n\
                     api_key: ${OPENAI_CUSTOM_API_KEY}\n\
                     base_url: ${OPENAI_CUSTOM_BASE_URL}\n\
                     rate_limit:\n\
                       requests_per_second: 2.0\n\
                 ```\n\
                 \n\
                 Y referencialo desde el agente con `model.provider: <tu_nombre>` \
                 (cualquier nombre — el factory `openai` resuelve por base_url). \
                 Para múltiples gateways simultáneos, edita `llm.yaml` a mano: el \
                 wizard sólo tiene un slot por nombre.",
            ),
            fields: vec![
                FieldDef {
                    key: "api_key",
                    label: "API key",
                    help: Some("Bearer token del gateway elegido."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "openai_custom_api_key.txt",
                        env_var: "OPENAI_CUSTOM_API_KEY",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "base_url",
                    label: "Base URL (incluyendo /v1 si aplica)",
                    help: Some(
                        "Ej. https://api.groq.com/openai/v1 · \
                         https://openrouter.ai/api/v1 · \
                         http://localhost:11434/v1 (Ollama) · \
                         http://localhost:1234/v1 (LM Studio).",
                    ),
                    kind: FieldKind::Text,
                    required: true,
                    default: Some("https://api.openai.com/v1"),
                    target: FieldTarget::EnvOnly("OPENAI_CUSTOM_BASE_URL"),
                    validator: Some(validate_nonempty),
                },
            ],
        },
        ServiceDef {
            id: "anthropic",
            label: "Anthropic Claude (API key / setup-token / Claude CLI / OAuth)",
            category: Category::Llm,
            description: Some(
                "Dos modos de autenticación:\n\
                 · `oauth_login` → flujo browser PKCE (suscripción Claude.ai) ★ recomendado\n\
                 · `setup_token` → pegar token sk-ant-oat01-… de `claude setup-token`\n\
                 El runtime además auto-detecta (mode=auto) bundles OAuth previos \
                 y credenciales `~/.claude/.credentials.json` si existen — no hay \
                 que re-correr wizard tras un `claude login` nuevo.",
            ),
            fields: vec![
                FieldDef {
                    key: "auth_mode",
                    label: "Modo de autenticación",
                    help: Some("oauth_login (browser) · setup_token (paste sk-ant-oat01-…)"),
                    kind: FieldKind::Choice(&["oauth_login", "setup_token"]),
                    required: true,
                    default: Some("oauth_login"),
                    target: FieldTarget::EnvOnly("ANTHROPIC_AUTH_MODE"),
                    validator: None,
                },
                FieldDef {
                    key: "secret_value",
                    label: "Setup-token (solo si auth_mode=setup_token)",
                    help: Some("Pega `sk-ant-oat01-…`. Dejar vacío para oauth_login."),
                    kind: FieldKind::Secret,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("ANTHROPIC_SECRET_VALUE"),
                    validator: None,
                },
                FieldDef {
                    key: "set_as_default",
                    label: "¿Usar Anthropic como proveedor principal en agents.yaml?",
                    help: Some(
                        "Reescribe `model.provider` y `model.model` en config/agents.yaml. \
                         Opciones:\n\
                         · `no`    → no toca agents.yaml (dejás todo como está)\n\
                         · `first` → solo parcha el primer agente (útil si tenés varios \
                           agentes con providers distintos y solo querés cambiar uno)\n\
                         · `yes`   → parcha TODOS los agentes a anthropic/<default_model>",
                    ),
                    kind: FieldKind::Choice(&["no", "first", "yes"]),
                    required: true,
                    default: Some("no"),
                    target: FieldTarget::EnvOnly("ANTHROPIC_SET_AS_DEFAULT"),
                    validator: None,
                },
                FieldDef {
                    key: "default_model",
                    label: "Modelo Anthropic a usar por defecto",
                    help: Some("Solo aplica si `set_as_default` != no."),
                    kind: FieldKind::Choice(&[
                        "claude-sonnet-4-5",
                        "claude-opus-4-5",
                        "claude-haiku-4-5",
                    ]),
                    required: true,
                    default: Some("claude-sonnet-4-5"),
                    target: FieldTarget::EnvOnly("ANTHROPIC_DEFAULT_MODEL"),
                    validator: None,
                },
            ],
        },
    ]
}
