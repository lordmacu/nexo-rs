use crate::registry::*;

pub fn defs() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            id: "google-auth",
            label: "Google OAuth (loopback flow — Gmail, Calendar, Drive, Sheets, …)",
            category: Category::Skill,
            description: Some(
                "Autentica contra la cuenta Google del usuario con OAuth 2.0 loopback. \
                 Pide client_id + client_secret (Google Cloud Console → Credentials → \
                 OAuth 2.0 Client ID tipo Desktop app, o Web app con redirect URI \
                 http://127.0.0.1:8765/callback). El refresh_token se persiste solo — \
                 el usuario da consent una vez.",
            ),
            fields: vec![
                FieldDef {
                    key: "client_id",
                    label: "Google OAuth client_id",
                    help: Some(
                        "Google Cloud Console → APIs & Services → Credentials → \
                         OAuth 2.0 Client IDs. Tipo: Desktop app (más simple) o \
                         Web app con http://127.0.0.1:8765/callback autorizado.",
                    ),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "google_client_id.txt",
                        env_var: "GOOGLE_CLIENT_ID",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "client_secret",
                    label: "Google OAuth client_secret",
                    help: Some("Misma pantalla de Credentials."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "google_client_secret.txt",
                        env_var: "GOOGLE_CLIENT_SECRET",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "scopes",
                    label: "Scopes (coma-separado, short-form acepta)",
                    help: Some(
                        "Ej: gmail.readonly,calendar.events,drive.readonly. \
                         Short-form se expande a URL canónica. Default: solo userinfo.",
                    ),
                    kind: FieldKind::List,
                    required: false,
                    default: Some(
                        "userinfo.email,userinfo.profile,\
                         gmail.modify,\
                         calendar.events,\
                         drive,\
                         spreadsheets,\
                         tasks,\
                         photoslibrary.readonly,\
                         youtube.readonly",
                    ),
                    target: FieldTarget::EnvOnly("GOOGLE_AUTH_SCOPES"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "google",
            label: "Google extension (Gmail + Calendar + Tasks)",
            category: Category::Skill,
            description: Some(
                "OAuth2 refresh-token flow. Requiere client_id, client_secret y un \
                 refresh_token (obtenido una sola vez con el consent flow). Las flags \
                 ALLOW_* habilitan write scopes — por defecto todo es read-only.",
            ),
            fields: vec![
                FieldDef {
                    key: "client_id",
                    label: "Google OAuth client_id",
                    help: Some("Google Cloud Console → APIs → Credentials → OAuth 2.0 Client IDs"),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "google_client_id.txt",
                        env_var: "GOOGLE_CLIENT_ID",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "client_secret",
                    label: "Google OAuth client_secret",
                    help: None,
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "google_client_secret.txt",
                        env_var: "GOOGLE_CLIENT_SECRET",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "refresh_token",
                    label: "Google OAuth refresh_token",
                    help: Some("Long-lived token del consent inicial (offline scope)."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "google_refresh_token.txt",
                        env_var: "GOOGLE_REFRESH_TOKEN",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "allow_send",
                    label: "Permitir gmail send + modificar labels",
                    help: Some("Write scope de Gmail. Default: off (read-only)."),
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("GOOGLE_ALLOW_SEND"),
                    validator: None,
                },
                FieldDef {
                    key: "allow_calendar_write",
                    label: "Permitir calendar crear/editar/borrar",
                    help: None,
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("GOOGLE_ALLOW_CALENDAR_WRITE"),
                    validator: None,
                },
                FieldDef {
                    key: "allow_tasks_write",
                    label: "Permitir tasks crear/completar/borrar",
                    help: None,
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("GOOGLE_ALLOW_TASKS_WRITE"),
                    validator: None,
                },
                FieldDef {
                    key: "http_timeout_secs",
                    label: "HTTP timeout (segundos)",
                    help: None,
                    kind: FieldKind::Number,
                    required: false,
                    default: Some("15"),
                    target: FieldTarget::EnvOnly("GOOGLE_HTTP_TIMEOUT_SECS"),
                    validator: None,
                },
                FieldDef {
                    key: "oauth_token_url",
                    label: "OAuth token endpoint (override)",
                    help: Some("Por defecto https://oauth2.googleapis.com/token"),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("GOOGLE_OAUTH_TOKEN_URL"),
                    validator: None,
                },
                FieldDef {
                    key: "gmail_url",
                    label: "Gmail API base URL (override)",
                    help: Some("Por defecto https://gmail.googleapis.com/gmail/v1"),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("GOOGLE_GMAIL_URL"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "cloudflare",
            label: "Cloudflare skill",
            category: Category::Skill,
            description: Some(
                "API token de Cloudflare (https://dash.cloudflare.com/profile/api-tokens). \
                 Los flags ALLOW_* habilitan write/purge — default read-only.",
            ),
            fields: vec![
                FieldDef {
                    key: "api_token",
                    label: "Cloudflare API token",
                    help: Some("Scopes típicos: Zone:Read, DNS:Edit, Cache Purge."),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "cloudflare_api_token.txt",
                        env_var: "CLOUDFLARE_API_TOKEN",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "allow_writes",
                    label: "Permitir writes (DNS, settings)",
                    help: None,
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("CLOUDFLARE_ALLOW_WRITES"),
                    validator: None,
                },
                FieldDef {
                    key: "allow_purge",
                    label: "Permitir cache purge",
                    help: None,
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("CLOUDFLARE_ALLOW_PURGE"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "translate",
            label: "Translate skill (DeepL / LibreTranslate)",
            category: Category::Skill,
            description: Some(
                "Dos proveedores soportados: DeepL (comercial, mejor calidad) o \
                 LibreTranslate (open-source, self-host posible).",
            ),
            fields: vec![
                FieldDef {
                    key: "provider",
                    label: "Proveedor por defecto",
                    help: Some("auto = DeepL si hay key, else LibreTranslate"),
                    kind: FieldKind::Choice(&["auto", "deepl", "libretranslate"]),
                    required: true,
                    default: Some("auto"),
                    target: FieldTarget::EnvOnly("TRANSLATE_PROVIDER"),
                    validator: None,
                },
                FieldDef {
                    key: "deepl_api_key",
                    label: "DeepL API key (opcional)",
                    help: Some("https://www.deepl.com/account — free tier soporta 500k chars/mes."),
                    kind: FieldKind::Secret,
                    required: false,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "deepl_api_key.txt",
                        env_var: "DEEPL_API_KEY",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "deepl_url",
                    label: "DeepL API URL (override)",
                    help: Some("Por defecto https://api-free.deepl.com/v2 (free) o https://api.deepl.com/v2 (pro)."),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("DEEPL_URL"),
                    validator: None,
                },
                FieldDef {
                    key: "libretranslate_api_key",
                    label: "LibreTranslate API key (opcional si hostel propio)",
                    help: None,
                    kind: FieldKind::Secret,
                    required: false,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "libretranslate_api_key.txt",
                        env_var: "LIBRETRANSLATE_API_KEY",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "libretranslate_url",
                    label: "LibreTranslate URL",
                    help: Some("Default https://libretranslate.com/translate"),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("LIBRETRANSLATE_URL"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "ssh-exec",
            label: "SSH exec skill",
            category: Category::Skill,
            description: Some(
                "Ejecutar comandos por SSH. Allowlist de hosts + flag de writes son \
                 obligatorios por seguridad — sin allowlist el skill rehúsa operar.",
            ),
            fields: vec![
                FieldDef {
                    key: "allowed_hosts",
                    label: "Hosts permitidos (coma-separado)",
                    help: Some("Ej: homelab.local,pve.local,user@vm.example"),
                    kind: FieldKind::List,
                    required: true,
                    default: None,
                    target: FieldTarget::EnvOnly("SSH_EXEC_ALLOWED_HOSTS"),
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "allow_writes",
                    label: "Permitir comandos que escriben (rm, write, install)",
                    help: Some("Default false → read-only (ls, cat, systemctl status, etc.)"),
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("SSH_EXEC_ALLOW_WRITES"),
                    validator: None,
                },
                FieldDef {
                    key: "timeout_secs",
                    label: "Timeout por comando (segundos)",
                    help: None,
                    kind: FieldKind::Number,
                    required: false,
                    default: Some("30"),
                    target: FieldTarget::EnvOnly("SSH_EXEC_TIMEOUT_SECS"),
                    validator: None,
                },
                FieldDef {
                    key: "ssh_bin",
                    label: "Binario ssh (override)",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("SSH_BIN"),
                    validator: None,
                },
                FieldDef {
                    key: "scp_bin",
                    label: "Binario scp (override)",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("SCP_BIN"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "docker-api",
            label: "Docker API skill",
            category: Category::Skill,
            description: Some(
                "Acceso al socket local de Docker. Write flag abre run/stop/remove; \
                 default read-only (ps, inspect, logs).",
            ),
            fields: vec![FieldDef {
                key: "allow_write",
                label: "Permitir run/stop/remove",
                help: Some("Default false → solo inspección."),
                kind: FieldKind::Bool,
                required: false,
                default: Some("false"),
                target: FieldTarget::EnvOnly("DOCKER_API_ALLOW_WRITE"),
                validator: None,
            }],
        },
        ServiceDef {
            id: "yt-dlp",
            label: "yt-dlp skill",
            category: Category::Skill,
            description: Some("Descarga de videos vía yt-dlp. Allow flag obligatorio para descargar."),
            fields: vec![
                FieldDef {
                    key: "allow_download",
                    label: "Permitir descargas (consume red/disco)",
                    help: None,
                    kind: FieldKind::Bool,
                    required: true,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("YTDLP_ALLOW_DOWNLOAD"),
                    validator: None,
                },
                FieldDef {
                    key: "output_dir",
                    label: "Directorio de salida",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: Some("./data/yt-dlp"),
                    target: FieldTarget::EnvOnly("YTDLP_OUTPUT_DIR"),
                    validator: None,
                },
                FieldDef {
                    key: "bin",
                    label: "Binario yt-dlp (override)",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("YTDLP_BIN"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "brave-search",
            label: "Brave Search skill",
            category: Category::Skill,
            description: Some(
                "API key de Brave Search (https://brave.com/search/api/). \
                 Usada por el MCP de búsqueda web.",
            ),
            fields: vec![
                FieldDef {
                    key: "api_key",
                    label: "Brave Search API key",
                    help: None,
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "brave_search_api_key.txt",
                        env_var: "BRAVE_SEARCH_API_KEY",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "base_url",
                    label: "Base URL (override)",
                    help: Some("Por defecto https://api.search.brave.com/res/v1"),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("BRAVE_SEARCH_URL"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "wolfram",
            label: "Wolfram Alpha skill",
            category: Category::Skill,
            description: Some(
                "App ID de Wolfram Alpha (https://developer.wolframalpha.com/).",
            ),
            fields: vec![
                FieldDef {
                    key: "app_id",
                    label: "Wolfram App ID",
                    help: None,
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "wolfram_app_id.txt",
                        env_var: "WOLFRAM_APP_ID",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "base_url",
                    label: "Base URL (override)",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("WOLFRAM_BASE_URL"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "proxmox",
            label: "Proxmox VE skill",
            category: Category::Skill,
            description: Some(
                "API token de Proxmox VE para gestión de VMs / containers.",
            ),
            fields: vec![
                FieldDef {
                    key: "url",
                    label: "Proxmox URL",
                    help: Some("Ej: https://pve.local:8006/api2/json"),
                    kind: FieldKind::Text,
                    required: true,
                    default: None,
                    target: FieldTarget::EnvOnly("PROXMOX_URL"),
                    validator: Some(validate_https_url),
                },
                FieldDef {
                    key: "token",
                    label: "Proxmox API token",
                    help: Some("Formato: PVEAPIToken=user@realm!tokenid=<uuid>"),
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "proxmox_token.txt",
                        env_var: "PROXMOX_TOKEN",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "insecure_tls",
                    label: "Permitir TLS self-signed",
                    help: Some("Solo si tu Proxmox usa cert autofirmado."),
                    kind: FieldKind::Bool,
                    required: false,
                    default: Some("false"),
                    target: FieldTarget::EnvOnly("PROXMOX_INSECURE_TLS"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "wikipedia",
            label: "Wikipedia skill",
            category: Category::Skill,
            description: Some("Idioma por defecto para búsquedas."),
            fields: vec![FieldDef {
                key: "lang",
                label: "WIKIPEDIA_LANG",
                help: Some("Códigos ISO 639-1: es, en, fr, de, pt…"),
                kind: FieldKind::Text,
                required: false,
                default: Some("es"),
                target: FieldTarget::EnvOnly("WIKIPEDIA_LANG"),
                validator: None,
            }],
        },
        ServiceDef {
            id: "tesseract-ocr",
            label: "Tesseract OCR skill",
            category: Category::Skill,
            description: Some("Path opcional al binario tesseract."),
            fields: vec![FieldDef {
                key: "bin",
                label: "TESSERACT_BIN",
                help: Some("Default: busca `tesseract` en PATH."),
                kind: FieldKind::Text,
                required: false,
                default: None,
                target: FieldTarget::EnvOnly("TESSERACT_BIN"),
                validator: None,
            }],
        },
        ServiceDef {
            id: "video-frames",
            label: "Video frames skill",
            category: Category::Skill,
            description: Some("Extrae frames de video. Setea carpeta de salida + timeout."),
            fields: vec![
                FieldDef {
                    key: "output_root",
                    label: "Carpeta de salida",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: Some("./data/video-frames"),
                    target: FieldTarget::EnvOnly("VIDEO_FRAMES_OUTPUT_ROOT"),
                    validator: None,
                },
                FieldDef {
                    key: "timeout_secs",
                    label: "Timeout (segundos)",
                    help: None,
                    kind: FieldKind::Number,
                    required: false,
                    default: Some("60"),
                    target: FieldTarget::EnvOnly("VIDEO_FRAMES_TIMEOUT_SECS"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "rtsp-snapshot",
            label: "RTSP snapshot skill",
            category: Category::Skill,
            description: Some("Snapshots desde streams RTSP (cámaras IP)."),
            fields: vec![
                FieldDef {
                    key: "output_root",
                    label: "Carpeta de salida",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: Some("./data/rtsp-snapshot"),
                    target: FieldTarget::EnvOnly("RTSP_SNAPSHOT_OUTPUT_ROOT"),
                    validator: None,
                },
                FieldDef {
                    key: "timeout_secs",
                    label: "Timeout (segundos)",
                    help: None,
                    kind: FieldKind::Number,
                    required: false,
                    default: Some("15"),
                    target: FieldTarget::EnvOnly("RTSP_SNAPSHOT_TIMEOUT_SECS"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "openstreetmap",
            label: "OpenStreetMap skill",
            category: Category::Skill,
            description: Some("Geocoding vía Nominatim. Overrides opcionales."),
            fields: vec![
                FieldDef {
                    key: "nominatim_url",
                    label: "URL Nominatim",
                    help: Some("Default https://nominatim.openstreetmap.org"),
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("OSM_NOMINATIM_URL"),
                    validator: None,
                },
                FieldDef {
                    key: "timeout_secs",
                    label: "HTTP timeout (segundos)",
                    help: None,
                    kind: FieldKind::Number,
                    required: false,
                    default: Some("15"),
                    target: FieldTarget::EnvOnly("OSM_HTTP_TIMEOUT_SECS"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "github",
            label: "GitHub skill",
            category: Category::Skill,
            description: Some("Personal Access Token con scopes `repo` + `read:org`."),
            fields: vec![FieldDef {
                key: "token",
                label: "GitHub token",
                help: Some("https://github.com/settings/tokens/new — classic o fine-grained."),
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "github_token.txt",
                    env_var: "GITHUB_TOKEN",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "goplaces",
            label: "Google Places skill",
            category: Category::Skill,
            description: Some(
                "Google Cloud Places API key. Override base URL si usas un proxy.",
            ),
            fields: vec![
                FieldDef {
                    key: "api_key",
                    label: "Google Places API key",
                    help: None,
                    kind: FieldKind::Secret,
                    required: true,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "google_places_api_key.txt",
                        env_var: "GOOGLE_PLACES_API_KEY",
                    },
                    validator: Some(validate_nonempty),
                },
                FieldDef {
                    key: "base_url",
                    label: "Base URL (override)",
                    help: None,
                    kind: FieldKind::Text,
                    required: false,
                    default: None,
                    target: FieldTarget::EnvOnly("GOOGLE_PLACES_BASE_URL"),
                    validator: None,
                },
            ],
        },
        ServiceDef {
            id: "onepassword",
            label: "1Password skill",
            category: Category::Skill,
            description: Some("Service-account token (empieza con `ops_…`). Read-only."),
            fields: vec![FieldDef {
                key: "service_account_token",
                label: "1Password service-account token",
                help: Some("developer.1password.com/docs/service-accounts"),
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "op_service_account_token.txt",
                    env_var: "OP_SERVICE_ACCOUNT_TOKEN",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "whisper",
            label: "OpenAI Whisper skill (audio transcription)",
            category: Category::Skill,
            description: Some("API key para transcripción de voz (conectado al plugin de WhatsApp para voice notes)."),
            fields: vec![FieldDef {
                key: "api_key",
                label: "OpenAI Whisper API key",
                help: None,
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "whisper_openai_api_key.txt",
                    env_var: "WHISPER_OPENAI_API_KEY",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "spotify",
            label: "Spotify skill",
            category: Category::Skill,
            description: Some("User-scoped access token. Atención: no hay refresh flow automático — rota manualmente."),
            fields: vec![FieldDef {
                key: "access_token",
                label: "Spotify access token",
                help: None,
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "spotify_access_token.txt",
                    env_var: "SPOTIFY_ACCESS_TOKEN",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "summarize",
            label: "Summarize skill",
            category: Category::Skill,
            description: Some("OpenAI-compatible API key para resúmenes largos."),
            fields: vec![FieldDef {
                key: "api_key",
                label: "Summarize OpenAI-compat API key",
                help: None,
                kind: FieldKind::Secret,
                required: true,
                default: None,
                target: FieldTarget::Secret {
                    file: "summarize_openai_api_key.txt",
                    env_var: "SUMMARIZE_OPENAI_API_KEY",
                },
                validator: Some(validate_nonempty),
            }],
        },
        ServiceDef {
            id: "loop",
            label: "Loop skill (auto-iteración acotada)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para ejecutar un prompt en ciclo acotado \
                 con contrato `{prompt, max_iters, until_predicate}`. Útil para \
                 retry/refine/verify sin bucles infinitos.",
            ),
            fields: vec![],
        },
        ServiceDef {
            id: "stuck",
            label: "Stuck skill (auto-debug acotado)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para depurar fallos repetidos de \
                 `cargo build`/`cargo test` con contrato \
                 `{failing_command, max_rounds, focus_pattern}` y salida de evidencia.",
            ),
            fields: vec![],
        },
        ServiceDef {
            id: "simplify",
            label: "Simplify skill (refactor acotado)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para simplificar un archivo/hunk con \
                 contrato `{target, scope, max_passes, preserve_behavior}`. Reduce \
                 complejidad (dead code, guards redundantes, duplicación) sin romper \
                 comportamiento por defecto.",
            ),
            fields: vec![],
        },
        ServiceDef {
            id: "verify",
            label: "Verify skill (validación acotada)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para validar criterios de aceptación \
                 con contrato `{acceptance_criterion, candidate_commands, max_rounds, \
                 judge_mode}` ejecutando checks reales + juicio explícito sobre evidencia.",
            ),
            fields: vec![],
        },
        ServiceDef {
            id: "skillify",
            label: "Skillify skill (captura workflow reusable)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para convertir un proceso \
                 repetible en un `SKILL.md` reusable con contrato \
                 `{workflow_name, source_scope, target_location, required_args}`.",
            ),
            fields: vec![],
        },
        ServiceDef {
            id: "remember",
            label: "Remember skill (higiene de memoria)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para revisar capas de memoria \
                 y proponer promociones/limpieza/conflictos con contrato \
                 `{review_scope, apply_changes, priority, target_files}`.",
            ),
            fields: vec![],
        },
        ServiceDef {
            id: "update-config",
            label: "Update-config skill (edición segura de config)",
            category: Category::Skill,
            description: Some(
                "Skill local (sin credenciales) para mapear cambios de \
                 comportamiento a `config/*.yaml` de Nexo y aplicar \
                 merges seguros con awareness de hot-reload vs restart.",
            ),
            fields: vec![],
        },
        // FOLLOWUPS W-3 — Phase 25 in-process `web_search` router.
        // Distinct from the `brave-search` ServiceDef above, which
        // configures the *MCP-based* brave skill. The runtime
        // `web_search` tool (in `crates/web-search/`) reads its
        // provider keys from these env vars / secret files; without
        // them the tool returns "no provider available". Both keys
        // are optional individually — the router falls back across
        // whichever providers are configured. Setting at least one
        // is required for the tool to work.
        ServiceDef {
            id: "web-search",
            label: "Web search router (Phase 25)",
            category: Category::Skill,
            description: Some(
                "API keys for the in-process `web_search` router (distinct from the \
                 MCP-based brave-search skill above). Supports Brave + Tavily; with \
                 either configured the agent can issue web searches. With both \
                 configured the router picks by priority + caches each call.",
            ),
            fields: vec![
                FieldDef {
                    key: "brave_api_key",
                    label: "Brave Search API key (web_search router)",
                    help: Some(
                        "Same key shape as the `brave-search` skill above (public API \
                         at https://brave.com/search/api/). If already configured there \
                         you can copy the value here — they live in separate files so \
                         operators can enable each engine independently.",
                    ),
                    kind: FieldKind::Secret,
                    required: false,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "web_search_brave_api_key.txt",
                        env_var: "BRAVE_SEARCH_API_KEY",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "tavily_api_key",
                    label: "Tavily API key",
                    help: Some(
                        "Tavily AI search (https://tavily.com/). Usually returns more \
                         curated answers for factual questions than Brave; Brave wins \
                         on freshness and direct-link results. Configuring both lets \
                         you pick per-call.",
                    ),
                    kind: FieldKind::Secret,
                    required: false,
                    default: None,
                    target: FieldTarget::Secret {
                        file: "web_search_tavily_api_key.txt",
                        env_var: "TAVILY_API_KEY",
                    },
                    validator: None,
                },
                FieldDef {
                    key: "default_provider",
                    label: "Default provider",
                    help: Some(
                        "`brave` or `tavily`. Which engine the router picks when the \
                         agent does not explicitly request one. Default: brave.",
                    ),
                    kind: FieldKind::Text,
                    required: false,
                    default: Some("brave"),
                    target: FieldTarget::EnvOnly("WEB_SEARCH_DEFAULT_PROVIDER"),
                    validator: None,
                },
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::defs;

    #[test]
    fn loop_skill_is_present_and_secretless() {
        let all = defs();
        let loop_svc = all
            .into_iter()
            .find(|s| s.id == "loop")
            .expect("loop skill service must exist");
        assert!(matches!(
            loop_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            loop_svc.fields.is_empty(),
            "loop skill should not require setup fields/secrets"
        );
    }

    #[test]
    fn stuck_skill_is_present_and_secretless() {
        let all = defs();
        let stuck_svc = all
            .into_iter()
            .find(|s| s.id == "stuck")
            .expect("stuck skill service must exist");
        assert!(matches!(
            stuck_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            stuck_svc.fields.is_empty(),
            "stuck skill should not require setup fields/secrets"
        );
    }

    #[test]
    fn simplify_skill_is_present_and_secretless() {
        let all = defs();
        let simplify_svc = all
            .into_iter()
            .find(|s| s.id == "simplify")
            .expect("simplify skill service must exist");
        assert!(matches!(
            simplify_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            simplify_svc.fields.is_empty(),
            "simplify skill should not require setup fields/secrets"
        );
    }

    #[test]
    fn verify_skill_is_present_and_secretless() {
        let all = defs();
        let verify_svc = all
            .into_iter()
            .find(|s| s.id == "verify")
            .expect("verify skill service must exist");
        assert!(matches!(
            verify_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            verify_svc.fields.is_empty(),
            "verify skill should not require setup fields/secrets"
        );
    }

    #[test]
    fn skillify_skill_is_present_and_secretless() {
        let all = defs();
        let skillify_svc = all
            .into_iter()
            .find(|s| s.id == "skillify")
            .expect("skillify skill service must exist");
        assert!(matches!(
            skillify_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            skillify_svc.fields.is_empty(),
            "skillify skill should not require setup fields/secrets"
        );
    }

    #[test]
    fn remember_skill_is_present_and_secretless() {
        let all = defs();
        let remember_svc = all
            .into_iter()
            .find(|s| s.id == "remember")
            .expect("remember skill service must exist");
        assert!(matches!(
            remember_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            remember_svc.fields.is_empty(),
            "remember skill should not require setup fields/secrets"
        );
    }

    #[test]
    fn update_config_skill_is_present_and_secretless() {
        let all = defs();
        let update_config_svc = all
            .into_iter()
            .find(|s| s.id == "update-config")
            .expect("update-config skill service must exist");
        assert!(matches!(
            update_config_svc.category,
            crate::registry::Category::Skill
        ));
        assert!(
            update_config_svc.fields.is_empty(),
            "update-config skill should not require setup fields/secrets"
        );
    }
}
