//! Inventory + reporter for write/reveal env-toggles exposed by the
//! bundled extensions. Powers `agent doctor capabilities` so an
//! operator can see at a glance which dangerous capabilities are
//! currently armed in their shell environment.
//!
//! The list is hardcoded here on purpose: each entry tracks an env
//! var that lives in some extension's source code, and the source of
//! truth is the code itself. A YAML registry would diverge from
//! reality the moment someone renamed a constant.

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
}

impl ToggleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToggleKind::Boolean => "boolean",
            ToggleKind::Allowlist => "allowlist",
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
];

pub fn inventory() -> &'static [CapabilityToggle] {
    INVENTORY
}

pub fn evaluate_all() -> Vec<ToggleStatus> {
    INVENTORY.iter().copied().map(evaluate_one).collect()
}

pub fn evaluate_one(toggle: CapabilityToggle) -> ToggleStatus {
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
    };
    ToggleStatus {
        toggle,
        state,
        raw_value: raw,
        items_count,
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
            inv.len() >= 7,
            "inventory should not shrink unintentionally"
        );
        let env_vars: Vec<&str> = inv.iter().map(|t| t.env_var).collect();
        assert!(env_vars.contains(&"OP_ALLOW_REVEAL"));
        assert!(env_vars.contains(&"OP_INJECT_COMMAND_ALLOWLIST"));
        assert!(env_vars.contains(&"CLOUDFLARE_ALLOW_WRITES"));
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
