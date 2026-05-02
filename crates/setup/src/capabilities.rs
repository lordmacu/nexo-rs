//! Inventory + reporter for write/reveal env-toggles + Cargo
//! feature gates exposed by the bundled extensions and the core
//! framework. Powers `agent doctor capabilities` so an operator can
//! see at a glance which dangerous capabilities are currently armed
//! in their shell environment (or compiled into the binary).
//!
//! The list is hardcoded here on purpose: each entry tracks an env
//! var that lives in some extension's source code, and the source of
//! truth is the code itself. A YAML registry would diverge from
//! reality the moment someone renamed a constant.
//!
//! ## Provider-agnostic by design
//!
//! `extension: &'static str` accepts any identifier — `"core"`,
//! `"auth"`, `"plugin-email"`, `"llm-anthropic"`, `"llm-minimax"`,
//! `"llm-openai"`, `"llm-gemini"`, `"llm-deepseek"`, `"llm-xai"`,
//! `"llm-mistral"`, etc. There is no assumption of a single LLM
//! provider; every provider that introduces a dangerous toggle
//! (insecure-tls, skip-ratelimit, allow-write) gets its own entry.
//!
//! Threshold for inclusion: **dangerous = data destruction OR
//! security bypass OR irreversible side effect**. Things that do
//! NOT qualify and live in [`drift_tests::NON_DANGEROUS_ENV_ALLOWLIST`]
//! instead:
//! - System / path overrides (HOME, PATH, NEXO_HOME, *_DIR).
//! - LLM provider tuning: API version pins (`ANTHROPIC_VERSION`),
//!   cache feature betas (`*_CACHE_BETA`), identity / region routing
//!   (`MINIMAX_GROUP_ID`, hypothetical `OPENAI_ORG_ID`,
//!   `GEMINI_LOCATION`).
//! - Credentials (`ANTHROPIC_API_KEY`, `MINIMAX_API_KEY`,
//!   `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, etc.).
//!   Resolved through `${ENV_VAR}` substitution in YAML; the secret
//!   scanner + audit log cover these.
//!
//! ## Drift prevention
//!
//! [`drift_tests::inventory_covers_known_dangerous_envs`] walks every
//! `crates/**/*.rs` source file and asserts every `env::var("X")`
//! literal is either in INVENTORY or in NON_DANGEROUS_ENV_ALLOWLIST.
//! Adding a new env-driven toggle without classifying it fails the
//! test with an explicit list of offenders, so the operator-facing
//! surface stays in sync with reality.
//!
//! ## Prior art (validated, not copied)
//!
//! - `claude-code-leak/src/utils/envUtils.ts:32-47` — `isEnvTruthy()`
//!   helpers but no master registry; ~160 scattered `CLAUDE_*` env
//!   vars without a single source of truth. We do better.
//! - `claude-code-leak/src/commands/doctor/` — `/doctor` surfaces
//!   env vars hardcoded in UI, not generated from a registry. We
//!   generate from INVENTORY.
//! - `research/src/agents/auth-profiles/doctor.ts:15-42` —
//!   `formatAuthDoctorHint()` scoped to OAuth migration only; no
//!   enumeration of dangerous toggles. We fill that gap.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
            Risk::Critical => "critical",
        }
    }
    fn ansi_open(self) -> &'static str {
        match self {
            Risk::Low => "\x1b[32m",        // green
            Risk::Medium => "\x1b[33m",     // yellow
            Risk::High => "\x1b[31m",       // red
            Risk::Critical => "\x1b[1;31m", // bold red
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleKind {
    /// `true|1|yes` (case-insensitive) enables a single behavior.
    Boolean,
    /// Comma-separated list. `≥1` non-empty entry = enabled.
    Allowlist,
    /// Compile-time Cargo feature. The associated string is the
    /// feature name as declared in `Cargo.toml::[features]`.
    ///
    /// **Limitation**: `cfg!(feature = "X")` is evaluated when this
    /// crate (`nexo-setup`) is compiled, not when the binary that
    /// invoked `agent doctor` was compiled. The feature MUST be
    /// declared in `crates/setup/Cargo.toml::[features]` and
    /// propagated from any consumer that wants it visible here
    /// (the workspace pattern: `nexo-rs/X = ["nexo-setup/X", ...]`).
    /// Otherwise the entry always evaluates to `Disabled` regardless
    /// of the binary's real flag set.
    CargoFeature(&'static str),
}

impl ToggleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToggleKind::Boolean => "boolean",
            ToggleKind::Allowlist => "allowlist",
            ToggleKind::CargoFeature(_) => "cargo_feature",
        }
    }

    /// Feature-flag name for `CargoFeature` variants; `None` otherwise.
    pub fn feature_name(self) -> Option<&'static str> {
        match self {
            ToggleKind::CargoFeature(name) => Some(name),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Enabled,
    Disabled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Enabled => "enabled",
            State::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CapabilityToggle {
    pub extension: &'static str,
    pub env_var: &'static str,
    pub kind: ToggleKind,
    pub risk: Risk,
    pub effect: &'static str,
    pub hint: &'static str,
}

#[derive(Debug, Clone)]
pub struct ToggleStatus {
    pub toggle: CapabilityToggle,
    pub state: State,
    pub raw_value: Option<String>,
    pub items_count: Option<usize>,
}

const INVENTORY: &[CapabilityToggle] = &[
    // ── Phase 82.10 — admin RPC domain enables ───────────────────
    // Per-domain global kill switches. Each is checked BEFORE the
    // operator-granted capability set. Off → all methods in that
    // domain return `-32601 method_not_found` regardless of grant.
    // Default ON (no env var set behaves as `1`); operators flip
    // OFF for hardened deployments that disable a whole domain.
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_ADMIN_AGENTS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Enable `nexo/admin/agents/*` admin RPC domain (microapps can CRUD agents.yaml). Off disables the entire domain regardless of operator grants.",
        hint: "export NEXO_MICROAPP_ADMIN_AGENTS_ENABLED=0  # to disable",
    },
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_ADMIN_CREDENTIALS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Enable `nexo/admin/credentials/*` admin RPC domain (microapps can register/revoke channel credentials).",
        hint: "export NEXO_MICROAPP_ADMIN_CREDENTIALS_ENABLED=0",
    },
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_ADMIN_PAIRING_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::Medium,
        effect: "Enable `nexo/admin/pairing/*` admin RPC domain (microapps can initiate WhatsApp QR pairing flows).",
        hint: "export NEXO_MICROAPP_ADMIN_PAIRING_ENABLED=0",
    },
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_ADMIN_LLM_KEYS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::Critical,
        effect: "Enable `nexo/admin/llm_providers/*` admin RPC domain (microapps can rotate LLM provider keys via ${ENV_VAR} refs).",
        hint: "export NEXO_MICROAPP_ADMIN_LLM_KEYS_ENABLED=0",
    },
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_ADMIN_CHANNELS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::Medium,
        effect: "Enable `nexo/admin/channels/*` admin RPC domain (microapps can approve/revoke MCP-channel servers in agents.yaml).",
        hint: "export NEXO_MICROAPP_ADMIN_CHANNELS_ENABLED=0",
    },
    // Phase 82.11 — agent event firehose + admin RPC backfill.
    // Off → `nexo/admin/agent_events/*` returns -32601 AND the
    // boot-side broadcast subscriber tasks are not spawned (zero
    // overhead, zero PII surface). Operator-global kill switch
    // independent of the per-microapp `transcripts_subscribe` /
    // `agent_events_subscribe_all` capability grants.
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_ADMIN_SKILLS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::Medium,
        effect: "Enable `nexo/admin/skills/*` admin RPC domain (microapps can CRUD markdown skills attachable to agents). Off disables the entire domain regardless of operator grants.",
        hint: "export NEXO_MICROAPP_ADMIN_SKILLS_ENABLED=0",
    },
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_AGENT_EVENTS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Enable `nexo/admin/agent_events/*` backfill + `nexo/notify/agent_event` firehose. Off disables the whole subsystem (microapps see -32601 + receive no notifications) regardless of operator grants.",
        hint: "export NEXO_MICROAPP_AGENT_EVENTS_ENABLED=0  # to disable",
    },
    // Phase 82.12 — microapp HTTP servers (`[capabilities.http_server]`).
    // Off → boot supervisor skips the health probe AND the
    // monitor loop; microapps that declare an http_server still
    // start, but the daemon never marks them `ready` based on
    // health. Per-extension token rotation notifications still
    // fire when the microapp is reachable. Operator killswitch
    // for hardened deployments that ban embedded HTTP servers.
    CapabilityToggle {
        extension: "core",
        env_var: "NEXO_MICROAPP_HTTP_SERVERS_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Enable boot health probe + monitor loop for microapps declaring `[capabilities.http_server]`. Off disables the supervisor; microapps still start but their HTTP-readiness is not gated.",
        hint: "export NEXO_MICROAPP_HTTP_SERVERS_ENABLED=0  # to disable",
    },
    CapabilityToggle {
        extension: "onepassword",
        env_var: "OP_ALLOW_REVEAL",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Reveal raw secret values to the LLM (and through it, transcripts).",
        hint: "export OP_ALLOW_REVEAL=true",
    },
    CapabilityToggle {
        extension: "onepassword",
        env_var: "OP_INJECT_COMMAND_ALLOWLIST",
        kind: ToggleKind::Allowlist,
        risk: Risk::High,
        effect: "Allow `inject_template` to pipe rendered templates to listed commands.",
        hint: "export OP_INJECT_COMMAND_ALLOWLIST=curl,psql",
    },
    CapabilityToggle {
        extension: "cloudflare",
        env_var: "CLOUDFLARE_ALLOW_WRITES",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Create / update / delete DNS records.",
        hint: "export CLOUDFLARE_ALLOW_WRITES=true",
    },
    CapabilityToggle {
        extension: "cloudflare",
        env_var: "CLOUDFLARE_ALLOW_PURGE",
        kind: ToggleKind::Boolean,
        risk: Risk::Critical,
        effect: "Purge zone cache (production-impacting).",
        hint: "export CLOUDFLARE_ALLOW_PURGE=true",
    },
    CapabilityToggle {
        extension: "docker-api",
        env_var: "DOCKER_API_ALLOW_WRITE",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Start / stop / restart Docker containers.",
        hint: "export DOCKER_API_ALLOW_WRITE=true",
    },
    CapabilityToggle {
        extension: "proxmox",
        env_var: "PROXMOX_ALLOW_WRITE",
        kind: ToggleKind::Boolean,
        risk: Risk::Critical,
        effect: "VM / container lifecycle on Proxmox host.",
        hint: "export PROXMOX_ALLOW_WRITE=true",
    },
    CapabilityToggle {
        extension: "ssh-exec",
        env_var: "SSH_EXEC_ALLOWED_HOSTS",
        kind: ToggleKind::Allowlist,
        risk: Risk::High,
        effect: "Allow `ssh_run` against the listed hosts (read-only commands).",
        hint: "export SSH_EXEC_ALLOWED_HOSTS=host1,host2",
    },
    CapabilityToggle {
        extension: "ssh-exec",
        env_var: "SSH_EXEC_ALLOW_WRITES",
        kind: ToggleKind::Boolean,
        risk: Risk::Critical,
        effect: "Allow `scp_upload` (write to remote filesystem).",
        hint: "export SSH_EXEC_ALLOW_WRITES=true",
    },
    CapabilityToggle {
        extension: "project-tracker",
        env_var: "PROGRAM_PHASE_ALLOW_SHELL_HOOKS",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Let `program_phase` register shell-cmd completion hooks that run inside the daemon process.",
        hint: "export PROGRAM_PHASE_ALLOW_SHELL_HOOKS=true",
    },
    CapabilityToggle {
        extension: "plugin-email",
        env_var: "EMAIL_INSECURE_TLS",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Skip IMAP TLS certificate verification (dev / fake servers only). In production this opens MITM attack vectors.",
        hint: "export EMAIL_INSECURE_TLS=1",
    },
    // C3 — auth-wide bypass of the file-permission gauntlet on the
    // secrets directory. Provider-agnostic (applies regardless of
    // which LLM provider's credentials live under `secrets/`).
    CapabilityToggle {
        extension: "auth",
        env_var: "CHAT_AUTH_SKIP_PERM_CHECK",
        kind: ToggleKind::Boolean,
        risk: Risk::High,
        effect: "Skip the file-permission check on the secrets \
                 directory at boot. Permissive perms (group / world \
                 readable) on credential files stop being a startup \
                 error. Dev / CI use only — production runs leak \
                 secrets to anyone with shell access on the host.",
        hint: "export CHAT_AUTH_SKIP_PERM_CHECK=1",
    },
    // C3 — Anthropic OAuth Bearer CLI version stamp override. Low
    // risk (auth metadata, no data destruction) but listed for ops
    // visibility — operators troubleshooting auth want to see if
    // this is set. Provider-specific (Anthropic only); other LLM
    // providers get their own entries when they introduce similar
    // gates.
    CapabilityToggle {
        extension: "llm-anthropic",
        env_var: "NEXO_CLAUDE_CLI_VERSION",
        kind: ToggleKind::Boolean,
        risk: Risk::Low,
        effect: "Override the spoofed `claude-cli` User-Agent / \
                 version header on Anthropic OAuth Bearer requests \
                 (default `2.1.75`). Affects acceptance gating for \
                 Opus / Sonnet 4.x — pin to a specific version when \
                 Anthropic bumps their accepted set. No effect on \
                 API-key (non-OAuth) Anthropic paths or on other \
                 LLM providers (MiniMax / OpenAI / Gemini / DeepSeek \
                 / xAI / Mistral).",
        hint: "export NEXO_CLAUDE_CLI_VERSION=2.2.0",
    },
    // C3 — Cargo feature gate for the self-config-editing tool.
    // Critical: the agent can read + propose + apply edits to its
    // own agents.yaml when this feature is compiled in AND
    // `agents.<id>.config_tool.self_edit = true` AND
    // `mcp_server.auth_token_env` is configured (Phase 79.M.c
    // hardening). Provider-agnostic — gates the same ConfigTool
    // regardless of which LLM provider drives it.
    CapabilityToggle {
        extension: "core",
        env_var: "cfg(feature = \"config-self-edit\")",
        kind: ToggleKind::CargoFeature("config-self-edit"),
        risk: Risk::Critical,
        effect: "Compiles the `Config` tool so an LLM can read + \
                 propose + apply edits to its own agents.yaml. Hard \
                 ship-control: a binary built without this feature \
                 has zero ability to self-edit YAML even if the \
                 per-agent YAML knob is set.",
        hint: "cargo build --features config-self-edit",
    },
    // Phase 80.1.c.b — dream_now LLM tool host-level gate. Per-binding
    // granularity stays in Phase 16 binding policy (`allowed_tools`).
    // Provider-agnostic: gates registration before LLM dispatch, so the
    // env-var read short-circuits regardless of which provider drives
    // the tool (Anthropic / MiniMax / OpenAI / Gemini / DeepSeek / xAI
    // / Mistral). Mirror leak `claude-code-leak/src/services/autoDream/
    // autoDream.ts:95-107` `isGateOpen` composed-flag pattern (we
    // collapse it to a single env var because the per-binding allow/deny
    // already lives in Phase 16).
    CapabilityToggle {
        extension: "dream",
        env_var: "NEXO_DREAM_NOW_ENABLED",
        kind: ToggleKind::Boolean,
        risk: Risk::Medium,
        effect: "Allow the LLM to force a memory-consolidation pass \
                 via the `dream_now` tool. Bypasses time / session / \
                 kairos / remote gates but honors \
                 `<memory_dir>/.consolidate-lock` (one fork at a time). \
                 Each call spawns a forked subagent (up to 30 turns) \
                 with FileEdit / FileWrite scoped to <memory_dir> and \
                 Bash limited to read-only commands. Cost: thousands \
                 of tokens per fire.",
        hint: "export NEXO_DREAM_NOW_ENABLED=true",
    },
    // Memory snapshot restore gate. Critical: a restore replays the
    // bundled SQLite + git memdir + state-provider artifacts on top
    // of the live agent, deleting whatever was there. Operators must
    // opt in explicitly so a stray `nexo memory restore` invocation
    // (typo, automation glitch) cannot silently overwrite production
    // memory. Provider-agnostic: gates the CLI subcommand at
    // dispatch time, irrespective of which LLM drives the agent.
    CapabilityToggle {
        extension: "memory-snapshot",
        env_var: "NEXO_MEMORY_RESTORE_ALLOW",
        kind: ToggleKind::Boolean,
        risk: Risk::Critical,
        effect: "Allow `nexo memory restore` to overwrite the live \
                 agent's SQLite memory + git memdir + state-provider \
                 artifacts with a bundle's contents. Restore always \
                 captures an auto-pre-snapshot first (rollback \
                 anchor) but the swap itself is destructive: tags \
                 dropped, untracked files lost, in-flight tool \
                 results truncated.",
        hint: "export NEXO_MEMORY_RESTORE_ALLOW=true",
    },
];

pub fn inventory() -> &'static [CapabilityToggle] {
    INVENTORY
}

pub fn evaluate_all() -> Vec<ToggleStatus> {
    INVENTORY.iter().copied().map(evaluate_one).collect()
}

pub fn evaluate_one(toggle: CapabilityToggle) -> ToggleStatus {
    // C3 — `CargoFeature` variant short-circuits the env-var read; no
    // env var exists for compile-time gates. Other variants continue
    // to read `toggle.env_var`.
    if let ToggleKind::CargoFeature(name) = toggle.kind {
        let enabled = is_cargo_feature_enabled(name);
        return ToggleStatus {
            toggle,
            state: if enabled {
                State::Enabled
            } else {
                State::Disabled
            },
            raw_value: Some(if enabled { "on" } else { "off" }.to_string()),
            items_count: None,
        };
    }

    let raw = std::env::var(toggle.env_var).ok();
    let (state, items_count) = match toggle.kind {
        ToggleKind::Boolean => {
            let enabled = raw
                .as_deref()
                .map(|s| {
                    let lower = s.trim().to_ascii_lowercase();
                    matches!(lower.as_str(), "true" | "1" | "yes")
                })
                .unwrap_or(false);
            (
                if enabled {
                    State::Enabled
                } else {
                    State::Disabled
                },
                None,
            )
        }
        ToggleKind::Allowlist => {
            let count = raw
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(|t| t.trim())
                        .filter(|t| !t.is_empty())
                        .count()
                })
                .unwrap_or(0);
            (
                if count > 0 {
                    State::Enabled
                } else {
                    State::Disabled
                },
                Some(count),
            )
        }
        // Unreachable — handled by the `if let` above.
        ToggleKind::CargoFeature(_) => unreachable!(),
    };
    ToggleStatus {
        toggle,
        state,
        raw_value: raw,
        items_count,
    }
}

/// C3 — compile-time feature check. The `cfg!` macro requires a
/// literal argument so we cannot dispatch on `name` at runtime; this
/// fn hard-codes the propagated feature set.
///
/// **Invariant**: every `ToggleKind::CargoFeature("X")` entry in
/// [`INVENTORY`] MUST have a matching arm here AND a corresponding
/// `[features]` entry in `crates/setup/Cargo.toml` propagated from
/// the workspace root (`Cargo.toml::[features]::X = ["nexo-setup/X",
/// ...]`). Missing arm → entry always reports `Disabled`. The drift
/// test `inventory_cargo_features_have_arms` exercises every variant
/// to surface the gap at test time.
fn is_cargo_feature_enabled(name: &str) -> bool {
    match name {
        "config-self-edit" => cfg!(feature = "config-self-edit"),
        _ => false,
    }
}

pub fn render_tty(statuses: &[ToggleStatus]) -> String {
    const HEADERS: [&str; 5] = ["EXT", "ENV VAR", "STATE", "RISK", "EFFECT"];
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(statuses.len() + 1);
    rows.push(HEADERS.map(|s| s.to_string()));
    for s in statuses {
        let state = match s.state {
            State::Enabled => match s.toggle.kind {
                ToggleKind::Allowlist => format!(
                    "enabled ({} {})",
                    s.items_count.unwrap_or(0),
                    if s.items_count == Some(1) {
                        "entry"
                    } else {
                        "entries"
                    }
                ),
                ToggleKind::Boolean => "enabled".to_string(),
                ToggleKind::CargoFeature(_) => "enabled (compiled-in)".to_string(),
            },
            State::Disabled => "disabled".to_string(),
        };
        rows.push([
            s.toggle.extension.to_string(),
            s.toggle.env_var.to_string(),
            state,
            s.toggle.risk.as_str().to_uppercase(),
            s.toggle.effect.to_string(),
        ]);
    }
    let widths: [usize; 5] = std::array::from_fn(|col| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
    });
    let mut out = String::new();
    out.push_str("Capability toggles\n");
    let total_width: usize = widths.iter().sum::<usize>() + 4 * 2;
    out.push_str(&"─".repeat(total_width.min(120)));
    out.push('\n');
    for (idx, row) in rows.iter().enumerate() {
        for col in 0..5 {
            let cell = &row[col];
            let pad = widths[col].saturating_sub(cell.chars().count());
            // Risk column gets ANSI color (data rows only).
            if col == 3 && idx > 0 {
                let risk = statuses[idx - 1].toggle.risk;
                out.push_str(risk.ansi_open());
                out.push_str(cell);
                out.push_str("\x1b[0m");
            } else {
                out.push_str(cell);
            }
            for _ in 0..pad {
                out.push(' ');
            }
            if col < 4 {
                out.push_str("  ");
            }
        }
        out.push('\n');
    }
    out.push_str("\nHint: see `docs/src/ops/capabilities.md` for guidance.\n");
    out
}

pub fn render_json(statuses: &[ToggleStatus]) -> Value {
    let arr: Vec<Value> = statuses
        .iter()
        .map(|s| {
            json!({
                "extension": s.toggle.extension,
                "env_var": s.toggle.env_var,
                "kind": s.toggle.kind.as_str(),
                "risk": s.toggle.risk.as_str(),
                "state": s.state.as_str(),
                "raw_value": s.raw_value,
                "items_count": s.items_count,
                "effect": s.toggle.effect,
                "hint": s.toggle.hint,
            })
        })
        .collect();
    Value::Array(arr)
}

// =====================================================================
// Phase 79.10 — `ConfigTool` self-edit denylist.
//
// Hard-coded list of dotted-path globs the `Config` tool MUST NEVER
// touch even when the agent's `config.self_edit: true` is set. The
// denylist is the orthogonal counterpart of the per-key whitelist
// (`SUPPORTED_SETTINGS`) and is enforced TWICE — at `propose` (early
// reject) and at `apply` (defense-in-depth).
//
// Source-of-truth lives in code (NOT YAML) so a model that proposes
// a patch widening the denylist cannot succeed: changing this slice
// requires a code change + review.
//
// Reference (PRIMARY): `proyecto/PHASES.md:5277-5293` enumerates the
// 13 globs. The leak's `claude-code-leak/src/tools/ConfigTool/
// supportedSettings.ts:188-190` ships only a whitelist (`isSupported`)
// — we add this orthogonal denylist as defense-in-depth for a
// chat-driven approval flow that lacks the leak's `'ask'` host
// permission prompt.
// =====================================================================

use std::sync::OnceLock;

pub const CONFIG_SELF_EDIT_DENYLIST: &[&str] = &[
    // Generic credential-shaped suffixes. Catch-all for any future
    // YAML key whose name leaks "this is a secret".
    "*_token",
    "*_secret",
    "*_password",
    "*_key",
    // Pairing internals — touching these revokes the operator's
    // grip on the agent.
    "pairing.*",
    // Capability inventory + dispatch policy — model cannot widen
    // its own permissions.
    "capabilities.*",
    // MCP server auth + spawn args — running arbitrary binaries
    // via config self-edit is game-over.
    "mcp.servers.*.auth.*",
    "mcp.servers.*.command",
    // Binding-level role + plan_mode — cannot self-promote to
    // coordinator nor drop plan-mode guardrails.
    "binding.*.role",
    "binding.*.plan_mode.*",
    // Proactive mode changes runtime/cost behavior and can create
    // autonomous turn loops; keep operator-only.
    "binding.*.proactive.enabled",
    "proactive.enabled",
    // Outbound webhooks — URL + secret env both lock-in.
    "remote_triggers[*].url",
    "remote_triggers[*].secret_env",
    // Operator-only knobs.
    "cron.user_max_entries",
    // Changing the registry store mid-run is unsafe.
    "agent_registry.store.*",
];

/// Exact-cause for a denied target.
#[derive(Debug, thiserror::Error)]
#[error("path `{path}` denied by self-edit policy (matched glob `{matched_glob}`)")]
pub struct ForbiddenKey {
    pub path: String,
    pub matched_glob: &'static str,
}

/// Lazy `GlobSet`, built once on first call. Pairs each glob with its
/// position in [`CONFIG_SELF_EDIT_DENYLIST`] so [`denylist_match`] can
/// return the *string* glob that matched (for the error message).
static DENYLIST_SET: OnceLock<globset::GlobSet> = OnceLock::new();

fn denylist_set() -> &'static globset::GlobSet {
    DENYLIST_SET.get_or_init(|| {
        let mut builder = globset::GlobSetBuilder::new();
        for pat in CONFIG_SELF_EDIT_DENYLIST {
            // `[*]` (array index syntax) is not native to globset;
            // we normalise to the equivalent `*` segment.
            let normalised = pat.replace("[*]", ".*");
            let glob = globset::Glob::new(&normalised)
                .expect("CONFIG_SELF_EDIT_DENYLIST entry must be a valid glob");
            builder.add(glob);
        }
        builder
            .build()
            .expect("CONFIG_SELF_EDIT_DENYLIST must compile to a valid GlobSet")
    })
}

/// Returns the matched glob (verbatim from [`CONFIG_SELF_EDIT_DENYLIST`])
/// when `path` is denied, or `None` when the path is safe.
///
/// `path` is the dotted path the proposal targets (e.g.
/// `pairing.session.token`). Internally we normalise `[*]` segments
/// to `.*` before matching so the array-index glob form survives the
/// `globset` translation.
pub fn denylist_match(path: &str) -> Option<&'static str> {
    // Normalise array-index syntax for the matcher. Patterns and
    // input must use the same form.
    let normalised = path.replace("[*]", ".*");
    let matches = denylist_set().matches(&normalised);
    let idx = matches.first()?;
    Some(CONFIG_SELF_EDIT_DENYLIST[*idx])
}

#[cfg(test)]
mod denylist_tests {
    use super::*;

    #[test]
    fn denylist_compiles() {
        // Building the set forces every glob through `Glob::new` —
        // any malformed pattern would panic.
        let set = denylist_set();
        assert!(set.len() == CONFIG_SELF_EDIT_DENYLIST.len());
    }

    #[test]
    fn denylist_matches_pairing_token() {
        // Pairing keys are blocked even when the suffix matches a
        // different glob (`*_token`). First match wins; either is
        // acceptable.
        let m = denylist_match("pairing.session_token").unwrap();
        assert!(
            m == "pairing.*" || m == "*_token",
            "expected pairing.* or *_token, got `{m}`"
        );
    }

    #[test]
    fn denylist_matches_secret_suffix() {
        assert_eq!(denylist_match("agents.foo.api_token"), Some("*_token"));
        assert_eq!(denylist_match("integrations.x.api_key"), Some("*_key"));
        assert_eq!(
            denylist_match("integrations.x.api_password"),
            Some("*_password")
        );
        assert_eq!(
            denylist_match("integrations.x.app_secret"),
            Some("*_secret")
        );
    }

    #[test]
    fn denylist_matches_array_index_form() {
        assert_eq!(
            denylist_match("remote_triggers[*].url"),
            Some("remote_triggers[*].url")
        );
        // The `.*` normalised form should also match a concrete
        // index since the matcher operates on the normalised input.
        assert_eq!(
            denylist_match("remote_triggers.0.url"),
            Some("remote_triggers[*].url")
        );
    }

    #[test]
    fn denylist_match_returns_none_for_safe_paths() {
        assert_eq!(denylist_match("model.model"), None);
        assert_eq!(denylist_match("language"), None);
        assert_eq!(denylist_match("heartbeat.interval_secs"), None);
        assert_eq!(denylist_match("lsp.enabled"), None);
    }

    #[test]
    fn denylist_blocks_role_and_plan_mode() {
        assert_eq!(
            denylist_match("binding.whatsapp.role"),
            Some("binding.*.role")
        );
        assert_eq!(
            denylist_match("binding.whatsapp.plan_mode.enabled"),
            Some("binding.*.plan_mode.*")
        );
        assert_eq!(
            denylist_match("binding.whatsapp.proactive.enabled"),
            Some("binding.*.proactive.enabled")
        );
        assert_eq!(
            denylist_match("proactive.enabled"),
            Some("proactive.enabled")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests touch process-wide env vars; serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn boolean_toggle() -> CapabilityToggle {
        CapabilityToggle {
            extension: "test",
            env_var: "TEST_BOOL_TOGGLE_XYZ",
            kind: ToggleKind::Boolean,
            risk: Risk::Medium,
            effect: "test",
            hint: "test",
        }
    }

    fn allowlist_toggle() -> CapabilityToggle {
        CapabilityToggle {
            extension: "test",
            env_var: "TEST_ALLOWLIST_TOGGLE_XYZ",
            kind: ToggleKind::Allowlist,
            risk: Risk::Medium,
            effect: "test",
            hint: "test",
        }
    }

    #[test]
    fn inventory_has_expected_entries() {
        let inv = inventory();
        assert!(
            inv.len() >= 14,
            "inventory should not shrink unintentionally"
        );
        let env_vars: Vec<&str> = inv.iter().map(|t| t.env_var).collect();
        assert!(env_vars.contains(&"OP_ALLOW_REVEAL"));
        assert!(env_vars.contains(&"OP_INJECT_COMMAND_ALLOWLIST"));
        assert!(env_vars.contains(&"CLOUDFLARE_ALLOW_WRITES"));
        // Phase 80.1.c.b — dream_now host-level gate.
        assert!(env_vars.contains(&"NEXO_DREAM_NOW_ENABLED"));
        let dream_entry = inv
            .iter()
            .find(|t| t.env_var == "NEXO_DREAM_NOW_ENABLED")
            .expect("dream_now entry present");
        assert_eq!(dream_entry.extension, "dream");
        assert_eq!(dream_entry.risk, Risk::Medium);
        assert!(matches!(dream_entry.kind, ToggleKind::Boolean));

        assert!(env_vars.contains(&"NEXO_MEMORY_RESTORE_ALLOW"));
        let restore_entry = inv
            .iter()
            .find(|t| t.env_var == "NEXO_MEMORY_RESTORE_ALLOW")
            .expect("memory restore entry present");
        assert_eq!(restore_entry.extension, "memory-snapshot");
        assert_eq!(restore_entry.risk, Risk::Critical);
        assert!(matches!(restore_entry.kind, ToggleKind::Boolean));
    }

    #[test]
    fn boolean_true_variants_are_enabled() {
        let _g = ENV_LOCK.lock().unwrap();
        let t = boolean_toggle();
        for v in ["true", "TRUE", "True", "1", "yes", "YES"] {
            std::env::set_var(t.env_var, v);
            let s = evaluate_one(t);
            assert_eq!(s.state, State::Enabled, "{v} should enable");
        }
        std::env::remove_var(t.env_var);
    }

    #[test]
    fn boolean_unset_or_garbage_is_disabled() {
        let _g = ENV_LOCK.lock().unwrap();
        let t = boolean_toggle();
        std::env::remove_var(t.env_var);
        assert_eq!(evaluate_one(t).state, State::Disabled);
        for v in ["false", "0", "no", "garbage", ""] {
            std::env::set_var(t.env_var, v);
            assert_eq!(evaluate_one(t).state, State::Disabled, "{v} should disable");
        }
        std::env::remove_var(t.env_var);
    }

    #[test]
    fn allowlist_with_entries_is_enabled() {
        let _g = ENV_LOCK.lock().unwrap();
        let t = allowlist_toggle();
        std::env::set_var(t.env_var, "curl,psql,send-email");
        let s = evaluate_one(t);
        assert_eq!(s.state, State::Enabled);
        assert_eq!(s.items_count, Some(3));
        std::env::remove_var(t.env_var);
    }

    #[test]
    fn allowlist_empty_or_whitespace_is_disabled() {
        let _g = ENV_LOCK.lock().unwrap();
        let t = allowlist_toggle();
        for v in ["", ", ,", "   ", ",,"] {
            std::env::set_var(t.env_var, v);
            let s = evaluate_one(t);
            assert_eq!(s.state, State::Disabled, "input `{v}` should disable");
            assert_eq!(s.items_count, Some(0));
        }
        std::env::remove_var(t.env_var);
    }

    #[test]
    fn render_json_shape_is_stable() {
        let _g = ENV_LOCK.lock().unwrap();
        let inv = inventory();
        let statuses: Vec<ToggleStatus> = inv.iter().copied().map(evaluate_one).collect();
        let v = render_json(&statuses);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), inv.len());
        for entry in arr {
            assert!(entry["extension"].is_string());
            assert!(entry["env_var"].is_string());
            assert!(entry["state"].is_string());
            assert!(entry["risk"].is_string());
            assert!(entry["effect"].is_string());
            assert!(entry["hint"].is_string());
        }
    }

    #[test]
    fn render_tty_contains_header_and_rows() {
        let _g = ENV_LOCK.lock().unwrap();
        let inv = inventory();
        let statuses: Vec<ToggleStatus> = inv.iter().copied().map(evaluate_one).collect();
        let out = render_tty(&statuses);
        assert!(out.contains("Capability toggles"));
        assert!(out.contains("EXT"));
        assert!(out.contains("ENV VAR"));
        for t in inv {
            assert!(
                out.contains(t.env_var),
                "render_tty should include `{}`",
                t.env_var
            );
        }
    }
}

/// C3 — Drift-prevention tests. Walks the workspace and asserts
/// that every `env::var("UPPER_NAME")` literal is either tracked in
/// [`INVENTORY`] (because it gates dangerous behaviour) or in
/// [`drift_tests::NON_DANGEROUS_ENV_ALLOWLIST`] (because it is a
/// path / version pin / identity router / credential).
///
/// Adding a new env-driven toggle without classifying it fails the
/// test with an explicit list of offenders, so the operator-facing
/// `agent doctor capabilities` surface stays in sync with reality.
#[cfg(test)]
mod drift_tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    /// Env vars that appear in `env::var("X")` reads but are NOT
    /// dangerous toggles by the [`module-level threshold`](super):
    /// **dangerous = data destruction OR security bypass OR
    /// irreversible side effect**.
    ///
    /// Classified by category. When a NEW LLM provider lands and
    /// needs entries, follow this pattern:
    ///   - version pin / API version stamp        → allowlist (here)
    ///   - cache / context-cache feature flag     → allowlist (here)
    ///   - org / project / region / group routing → allowlist (here)
    ///   - timeout / max-tokens override          → allowlist (here)
    ///   - "insecure-tls" / "skip-ratelimit" flag → INVENTORY (high risk)
    ///   - "allow-write" / "allow-purge" flag     → INVENTORY (critical)
    ///   - credential lookup (`*_API_KEY`)        → allowlist (here)
    ///     because credentials are handled by the secret scanner +
    ///     audit log, not as toggles.
    pub(super) const NON_DANGEROUS_ENV_ALLOWLIST: &[&str] = &[
        // System vars (provider-agnostic).
        "HOME",
        "PATH",
        "USER",
        "TMPDIR",
        "TMP",
        "TEMP",
        // Terminal display behaviour.
        "CLICOLOR",
        "CLICOLOR_FORCE",
        // Path / dev overrides.
        "NEXO_HOME",
        "NEXO_SECRETS_DIR",
        "NEXO_DRIVER_BIN",
        "NEXO_DRIVER_CONFIG",
        "NEXO_DRIVER_WORKSPACE_ROOT",
        "NEXO_PROJECT_ROOT",
        "CONFIG_SECRETS_DIR",
        "TELEGRAM_MEDIA_DIR",
        "CLOUDFLARED_BINARY",
        // Plugin endpoint / identity overrides (non-destructive).
        "CDP_URL",     // browser plugin: Chrome DevTools URL.
        "NATS_URL",    // broker URL pin.
        // Test / mock infra (gated by `#[cfg(test)]` in practice; the
        // scanner is regex-only so it picks them up regardless).
        "MOCK_MODE",
        "MOCK_CANCELLED_LOG",
        "MOCK_SAMPLING_LOG",
        "MOCK_SETLEVEL_LOG",
        "DELEGATE_TARGET",     // delegation_e2e_test fixture.
        "WA_LIVE_PEER_JID",    // WhatsApp live integration test.
        "WA_LIVE_SESSION_DIR", // WhatsApp live integration test.
        // ---- LLM provider tuning (non-destructive) ----
        // Anthropic
        "ANTHROPIC_VERSION",
        "ANTHROPIC_CACHE_BETA",
        "ANTHROPIC_CACHE_LONG_TTL_BETA",
        // MiniMax
        "MINIMAX_GROUP_ID",
        // ---- Reserved for future providers (DeepSeek / xAI /
        // Mistral / OpenAI / Gemini): when the first env read for
        // any of them lands the drift test fails, the dev classifies
        // (allowlist vs INVENTORY) and adds here with rationale.
        //
        // ---- Credentials (resolved via `${ENV_VAR}` substitution
        // in YAML; the secret_scanner + audit log handle these,
        // not the toggle inventory) ----
        "ANTHROPIC_API_KEY",
        "MINIMAX_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "DEEPSEEK_API_KEY",
        "XAI_API_KEY",
        "MISTRAL_API_KEY",
    ];

    /// Walks `crates/**/*.rs` regex-matching every
    /// `env::var("UPPER_NAME")` literal and asserts each found name
    /// is either in [`INVENTORY`] or in
    /// [`NON_DANGEROUS_ENV_ALLOWLIST`]. Fails with a sorted list of
    /// offenders so the operator knows exactly what to classify.
    ///
    /// **Limitation**: regex-based detection misses indirect reads
    /// (`let v = "FOO"; env::var(v)`). Almost zero such cases in
    /// this codebase by convention. Switch to `syn` AST if a real
    /// case appears.
    #[test]
    fn inventory_covers_known_dangerous_envs() {
        let inventory_names: HashSet<&str> = INVENTORY
            .iter()
            .filter(|t| !matches!(t.kind, ToggleKind::CargoFeature(_)))
            .map(|t| t.env_var)
            .collect();
        let allowlist: HashSet<&str> = NON_DANGEROUS_ENV_ALLOWLIST.iter().copied().collect();

        let workspace_root = workspace_root();
        let mut found: HashSet<String> = HashSet::new();
        scan_rust_files(&workspace_root.join("crates"), &mut found);

        let mut offenders: Vec<String> = found
            .into_iter()
            .filter(|name| !inventory_names.contains(name.as_str()))
            .filter(|name| !allowlist.contains(name.as_str()))
            .collect();
        offenders.sort();

        assert!(
            offenders.is_empty(),
            "env vars read from code but neither in INVENTORY nor in \
             NON_DANGEROUS_ENV_ALLOWLIST: {:?}.\n\nFix: either add a \
             `CapabilityToggle` entry to `INVENTORY` (if dangerous) \
             or append to `NON_DANGEROUS_ENV_ALLOWLIST` with a \
             comment explaining why it is benign.",
            offenders
        );
    }

    /// Every `ToggleKind::CargoFeature("X")` entry in INVENTORY is
    /// exercised through [`is_cargo_feature_enabled`] to confirm the
    /// branch returns without panic. This catches the case where
    /// someone adds an INVENTORY entry but forgets to wire the
    /// `cfg!` arm — partial detection only (a missing arm falls
    /// through to `_ => false`, which the test cannot distinguish
    /// from a feature genuinely off).
    #[test]
    fn inventory_cargo_features_have_arms() {
        for toggle in INVENTORY {
            if let ToggleKind::CargoFeature(name) = toggle.kind {
                let _ = is_cargo_feature_enabled(name);
            }
        }
    }

    fn workspace_root() -> std::path::PathBuf {
        // `CARGO_MANIFEST_DIR` for `nexo-setup` resolves to
        // `<workspace>/crates/setup`. Climb two parents to land at
        // the workspace root.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/setup parent (= crates/) must exist")
            .parent()
            .expect("crates/ parent (= workspace root) must exist")
            .to_path_buf()
    }

    fn scan_rust_files(root: &Path, found: &mut HashSet<String>) {
        let re = regex::Regex::new(r#"env::var\("([A-Z][A-Z0-9_]+)""#).unwrap();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().extension().is_none_or(|e| e != "rs") {
                continue;
            }
            // Skip this very file — the scanner regex literal +
            // panic-message format string contain `env::var("UPPER_NAME")`
            // shapes that would otherwise self-trigger.
            if entry.path().ends_with("setup/src/capabilities.rs") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                for cap in re.captures_iter(&content) {
                    found.insert(cap[1].to_string());
                }
            }
        }
    }
}
