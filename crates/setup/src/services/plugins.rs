use crate::registry::*;

pub fn defs() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            id: "whatsapp",
            label: "WhatsApp plugin",
            category: Category::Plugin,
            description: Some(
                "Phase 6 plugin. No hay token — el primer arranque emite QR. \
                 Aquí configuras paths, allow-list y toggles de config/plugins/whatsapp.yaml.",
            ),
            fields: vec![
                FieldDef {
                    key: "enabled",
                    label: "Habilitar plugin",
                    help: Some("Si es false el plugin no abre WebSocket en boot."),
                    kind: FieldKind::Bool,
                    required: true,
                    default: Some("true"),
                    target: FieldTarget::Yaml {
                        file: "plugins/whatsapp.yaml",
                        path: "whatsapp.enabled",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "session_dir",
                    label: "Session dir (opcional — vacío = per-agent bajo <workspace>/whatsapp/default)",
                    help: Some(
                        "Dejar vacío para que cada agente use su propio workspace. \
                         Llenar solo para forzar una ruta compartida (bind mount, volumen cifrado, etc).",
                    ),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/whatsapp.yaml",
                        path: "whatsapp.session_dir",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "media_dir",
                    label: "Media dir (descargas inbound)",
                    help: None,
                    kind: FieldKind::Text,
                    required: true,
                    default: Some("./data/media/whatsapp"),
                    target: FieldTarget::Yaml {
                        file: "plugins/whatsapp.yaml",
                        path: "whatsapp.media_dir",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "allow_list",
                    label: "Allow-list (JIDs separados por coma, vacío = open)",
                    help: Some("Ej: 573111111111@s.whatsapp.net,573222222222@s.whatsapp.net"),
                    kind: FieldKind::List,
                    required: false,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/whatsapp.yaml",
                        path: "whatsapp.acl.allow_list",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "ignore_groups",
                    label: "Ignorar chats de grupo",
                    help: None,
                    kind: FieldKind::Bool,
                    required: true,
                    default: Some("false"),
                    target: FieldTarget::Yaml {
                        file: "plugins/whatsapp.yaml",
                        path: "whatsapp.behavior.ignore_groups",
                    },
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "telegram",
            label: "Telegram plugin",
            category: Category::Plugin,
            description: Some("BotFather token + opcional allowlist de chat_ids."),
            fields: vec![
                FieldDef {
                    key: "bot_token",
                    label: "Bot token (@BotFather)",
                    help: Some("Formato: 123456789:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "telegram_bot_token.txt",
                        env_var: "TELEGRAM_BOT_TOKEN",
                    },
                    validator: Some(validate_telegram_token),
                },
                FieldDef {
                    key: "allow_chat_ids",
                    label: "Chat IDs permitidos (coma-separado, vacío = abierto)",
                    help: Some("Los chat IDs negativos son grupos."),
                    kind: FieldKind::List,
                    required: false,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/telegram.yaml",
                        path: "telegram.allowlist.chat_ids",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "polling_enabled",
                    label: "Polling habilitado",
                    help: None,
                    kind: FieldKind::Bool,
                    required: true,
                    default: Some("true"),
                    target: FieldTarget::Yaml {
                        file: "plugins/telegram.yaml",
                        path: "telegram.polling.enabled",
                    },
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "email",
            label: "Email plugin (SMTP/IMAP)",
            category: Category::Plugin,
            description: Some("Credenciales SMTP para salida. IMAP opcional para polling entrante."),
            fields: vec![
                FieldDef {
                    key: "smtp_host",
                    label: "SMTP host",
                    help: Some("Ej: smtp.gmail.com"),
                    kind: FieldKind::Text,
                    required: true,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/email.yaml",
                        path: "email.smtp.host",
                    },
                    validator: Some(validate_host),
                },
                FieldDef {
                    key: "smtp_port",
                    label: "SMTP port",
                    help: None,
                    kind: FieldKind::Number,
                    required: true,
                    default: Some("587"),
                    target: FieldTarget::Yaml {
                        file: "plugins/email.yaml",
                        path: "email.smtp.port",
                    },
                    validator: Some(validate_port),
                },
                FieldDef {
                    key: "smtp_user",
                    label: "SMTP user",
                    help: Some("Típicamente el email completo."),
                    kind: FieldKind::Text,
                    required: true,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/email.yaml",
                        path: "email.smtp.username",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "smtp_password",
                    label: "SMTP password (o app password)",
                    help: Some("Gmail: genera App Password; no uses tu clave humana."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "smtp_password.txt",
                        env_var: "SMTP_PASSWORD",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "imap_host",
                    label: "IMAP host (opcional — vacío para skip inbound)",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/email.yaml",
                        path: "email.imap.host",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "imap_port",
                    label: "IMAP port",
                    help: None,
                    kind: FieldKind::Number,
                    required: false,
                    default: Some("993"),
                    target: FieldTarget::Yaml {
                        file: "plugins/email.yaml",
                        path: "email.imap.port",
                    },
                    validator: Some(validate_port),
                },
            ],
        },
        ServiceDef {
            id: "browser",
            label: "Browser plugin (CDP)",
            category: Category::Plugin,
            description: Some(
                "Chrome DevTools Protocol. Vacío = lanza nuevo Chrome; URL = attach a uno existente.",
            ),
            fields: vec![
                FieldDef {
                    key: "cdp_url",
                    label: "CDP URL (vacío = spawn)",
                    help: Some("Ej: http://127.0.0.1:9222"),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::Yaml {
                        file: "plugins/browser.yaml",
                        path: "browser.cdp_url",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "headless",
                    label: "Headless",
                    help: None,
                    kind: FieldKind::Bool,
                    required: true,
                    default: Some("true"),
                    target: FieldTarget::Yaml {
                        file: "plugins/browser.yaml",
                        path: "browser.headless",
                    },
                    validator: None,
                },
            ],
        },
    ]
}
