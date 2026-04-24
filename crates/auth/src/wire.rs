//! Wire layer — turns `AppConfig` + `google-auth.yaml` into the
//! credential stores and resolver the runtime needs. Kept in this
//! crate (not `agent-config`) so the config crate stays a pure data
//! shape and never pulls `tokio` / `dashmap`.
//!
//! The entry point is [`build_credentials`], called from `main.rs`
//! during boot. Operators can also call it via `--check-config`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use agent_config::types::agents::AgentConfig;
use agent_config::types::credentials::{GoogleAccountConfig, GoogleAuthConfig, GoogleAuthFile};
use agent_config::types::plugins::{TelegramPluginConfig, WhatsappPluginConfig};
use anyhow::{Context, Result};

use crate::error::BuildError;
use crate::gauntlet::{
    canonicalize_session_dirs, check_duplicate_paths, check_permissions, check_prefix_overlap,
    format_errors, PathClaim,
};
use crate::google::{GoogleAccount, GoogleCredentialStore};
use crate::handle::{Channel, GOOGLE, TELEGRAM, WHATSAPP};
use crate::resolver::{
    AgentCredentialResolver, AgentCredentialsInput, CredentialStores, StrictLevel,
};
use crate::store::CredentialStore;
use crate::telegram::{TelegramAccount, TelegramCredentialStore};
use crate::whatsapp::{WhatsappAccount, WhatsappCredentialStore};

/// Bundle returned by [`build_credentials`] — holds every store plus
/// the resolver. `main.rs` hands this to plugins / tools.
pub struct CredentialsBundle {
    pub stores: CredentialStores,
    pub resolver: Arc<AgentCredentialResolver>,
    /// Per-`(channel, instance)` circuit breakers shared with plugin
    /// tools. Created with default config; failure on one account
    /// never trips another.
    pub breakers: Arc<crate::breaker::BreakerRegistry>,
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for CredentialsBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsBundle")
            .field("whatsapp_instances", &self.stores.whatsapp.list().len())
            .field("telegram_instances", &self.stores.telegram.list().len())
            .field("google_accounts", &self.stores.google.list().len())
            .field("resolver_version", &self.resolver.version())
            .field("warnings", &self.warnings.len())
            .finish()
    }
}

/// Load optional `google-auth.yaml` from `<dir>/plugins/google-auth.yaml`.
/// Returns an empty config when the file is absent so the caller does
/// not have to branch on `None`.
pub fn load_google_auth(dir: &Path) -> Result<GoogleAuthConfig> {
    let path = dir.join("plugins").join("google-auth.yaml");
    if !path.exists() {
        return Ok(GoogleAuthConfig::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = agent_config::env::resolve_placeholders(&raw, "google-auth.yaml")?;
    let file: GoogleAuthFile = serde_yaml::from_str(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))?;
    Ok(file.google_auth)
}

/// Run the boot gauntlet and build stores + resolver. Every error is
/// accumulated; a single `anyhow::Error` with a multi-line body is
/// returned so operators see every misconfiguration at once.
pub fn build_credentials(
    agents: &[AgentConfig],
    whatsapp: &[WhatsappPluginConfig],
    telegram: &[TelegramPluginConfig],
    google: &GoogleAuthConfig,
    strict: StrictLevel,
) -> Result<CredentialsBundle, Vec<BuildError>> {
    let mut errors: Vec<BuildError> = Vec::new();

    // ── 1. Path claims (session_dir WA + credential files Google) ──
    // Only labelled instances participate in the per-agent resolver.
    // Unlabelled (instance=None) accounts keep using the legacy single
    // outbound topic `plugin.outbound.whatsapp` as back-compat.
    let session_claims: Vec<PathClaim> = whatsapp
        .iter()
        .filter_map(|c| {
            c.instance.as_ref().map(|ins| PathClaim {
                channel: WHATSAPP,
                instance: ins.clone(),
                path: c.session_dir.clone().into(),
            })
        })
        .collect();

    let (canonical, canon_errs) = canonicalize_session_dirs(&session_claims);
    errors.extend(canon_errs);
    errors.extend(check_duplicate_paths(&canonical));
    errors.extend(check_prefix_overlap(&canonical));

    // Google file permission check (client_id / client_secret; token is
    // optional — setup wizard writes it on first consent).
    let mut perm_paths: Vec<(Channel, String, std::path::PathBuf)> = Vec::new();
    for a in &google.accounts {
        perm_paths.push((GOOGLE, a.id.clone(), a.client_id_path.clone()));
        perm_paths.push((GOOGLE, a.id.clone(), a.client_secret_path.clone()));
        if a.token_path.exists() {
            perm_paths.push((GOOGLE, a.id.clone(), a.token_path.clone()));
        }
    }
    let perm_errs = check_permissions(&perm_paths);
    let insecure_count = perm_errs.len() as u64;
    errors.extend(perm_errs);

    crate::telemetry::set_insecure_paths(insecure_count);

    // ── 2. Build per-channel stores ──
    // Skip unlabelled instances — they stay on the legacy outbound
    // topic and do not appear in the resolver's binding surface.
    let wa_accounts: Vec<WhatsappAccount> = whatsapp
        .iter()
        .filter_map(|c| {
            let instance = c.instance.as_ref()?.clone();
            Some(WhatsappAccount {
                instance,
                session_dir: c.session_dir.clone().into(),
                media_dir: c.media_dir.clone().into(),
                allow_agents: c.allow_agents.clone(),
            })
        })
        .collect();
    let tg_accounts: Vec<TelegramAccount> = telegram
        .iter()
        .filter_map(|c| {
            let instance = c.instance.as_ref()?.clone();
            Some(TelegramAccount {
                instance,
                token: c.token.clone(),
                allow_agents: c.allow_agents.clone(),
                allowed_chat_ids: c.allowlist.chat_ids.clone(),
            })
        })
        .collect();
    let mut goog_accounts: Vec<GoogleAccount> = google
        .accounts
        .iter()
        .map(|a: &GoogleAccountConfig| GoogleAccount {
            id: a.id.clone(),
            agent_id: a.agent_id.clone(),
            client_id_path: a.client_id_path.clone(),
            client_secret_path: a.client_secret_path.clone(),
            token_path: a.token_path.clone(),
            scopes: a.scopes.clone(),
        })
        .collect();

    // Migrate legacy inline `agents[].google_auth` into the store with
    // a warning. The account id is the agent id — 1:1 per agent. In
    // Strict mode the legacy form is an error: Phase 17 V2 forces the
    // move to google-auth.yaml.
    let mut legacy_warnings: Vec<String> = Vec::new();
    for agent in agents {
        let Some(g) = &agent.google_auth else { continue };
        if goog_accounts.iter().any(|a| a.agent_id == agent.id) {
            continue; // already declared explicitly in google-auth.yaml
        }
        let msg = format!(
            "agent '{}': inline google_auth is deprecated — migrate to config/plugins/google-auth.yaml (id: {0})",
            agent.id
        );
        match strict {
            StrictLevel::Strict => {
                errors.push(BuildError::LegacyInlineGoogleAuth {
                    agent: agent.id.clone(),
                });
                // Skip the synthetic migration — we want the operator
                // to fix the YAML, not run on a ghost entry.
                continue;
            }
            StrictLevel::Lenient => {
                legacy_warnings.push(msg);
            }
        }
        // `google_auth` uses `client_id` / `client_secret` as literal
        // strings, so emit synthetic in-memory paths. The gmail-poller
        // legacy path uses files; this synthetic path is marked by the
        // `inline:` prefix so the store knows to read the value
        // directly rather than load from disk. (Consumer logic lives
        // in step 16 of the plan; V1 ignores these accounts if the
        // files do not exist.)
        goog_accounts.push(GoogleAccount {
            id: agent.id.clone(),
            agent_id: agent.id.clone(),
            client_id_path: std::path::PathBuf::from(format!(
                "inline:{}",
                g.client_id
            )),
            client_secret_path: std::path::PathBuf::from(format!(
                "inline:{}",
                g.client_secret
            )),
            token_path: std::path::PathBuf::from(&g.token_file),
            scopes: g.scopes.clone(),
        });
    }

    let stores = CredentialStores {
        whatsapp: Arc::new(WhatsappCredentialStore::new(wa_accounts.clone())),
        telegram: Arc::new(TelegramCredentialStore::new(tg_accounts.clone())),
        google: Arc::new(GoogleCredentialStore::new(goog_accounts.clone())),
    };

    // Per-store self-check (missing scopes / empty token etc).
    let wa_report = stores.whatsapp.validate();
    let tg_report = stores.telegram.validate();
    let g_report = stores.google.validate();
    errors.extend(wa_report.errors);
    errors.extend(tg_report.errors);
    errors.extend(g_report.errors);
    let mut warnings: Vec<String> = wa_report
        .warnings
        .into_iter()
        .chain(tg_report.warnings.into_iter())
        .chain(g_report.warnings.into_iter())
        .chain(legacy_warnings)
        .collect();

    // Counter for dashboards.
    crate::telemetry::set_accounts_total(WHATSAPP, wa_accounts.len() as u64);
    crate::telemetry::set_accounts_total(TELEGRAM, tg_accounts.len() as u64);
    crate::telemetry::set_accounts_total(GOOGLE, goog_accounts.len() as u64);

    // ── 3. Build resolver inputs from agent configs ──
    let inputs: Vec<AgentCredentialsInput> =
        agents.iter().map(agent_to_input).collect();

    // ── 4. If any path / store-level error was collected, stop now ──
    if !errors.is_empty() {
        for e in &errors {
            let kind = match e {
                BuildError::DuplicatePath { .. } => "duplicate_path",
                BuildError::PathPrefixOverlap { .. } => "prefix_overlap",
                BuildError::MissingInstance { .. } => "missing_instance",
                BuildError::AmbiguousOutbound { .. } => "ambiguous_outbound",
                BuildError::AllowAgentsExcludes { .. } => "allow_agents_excludes",
                BuildError::AsymmetricBinding { .. } => "asymmetric_binding",
                BuildError::Credential { .. } => "credential_io",
                BuildError::LegacyInlineGoogleAuth { .. } => "legacy_inline_google_auth",
            };
            crate::telemetry::inc_boot_error(kind);
        }
        return Err(errors);
    }

    // ── 5. Build resolver (adds MissingInstance / Ambiguous / …) ──
    match AgentCredentialResolver::build(&inputs, &stores, strict) {
        Ok(resolver) => {
            warnings.extend(resolver.warnings().iter().cloned());
            // Export 0/1 binding gauge for dashboards.
            for agent in agents {
                for channel in [WHATSAPP, TELEGRAM, GOOGLE] {
                    let bound = resolver.resolve(&agent.id, channel).is_ok();
                    crate::telemetry::set_binding(channel, &agent.id, bound);
                }
            }
            Ok(CredentialsBundle {
                stores,
                resolver: Arc::new(resolver),
                breakers: Arc::new(crate::breaker::BreakerRegistry::default()),
                warnings,
            })
        }
        Err(errs) => {
            for e in &errs {
                let kind = match e {
                    BuildError::MissingInstance { .. } => "missing_instance",
                    BuildError::AmbiguousOutbound { .. } => "ambiguous_outbound",
                    BuildError::AllowAgentsExcludes { .. } => "allow_agents_excludes",
                    BuildError::AsymmetricBinding { .. } => "asymmetric_binding",
                    BuildError::Credential { .. } => "credential_io",
                    _ => "other",
                };
                crate::telemetry::inc_boot_error(kind);
            }
            Err(errs)
        }
    }
}

fn agent_to_input(agent: &AgentConfig) -> AgentCredentialsInput {
    let mut outbound: HashMap<Channel, String> = HashMap::new();
    if let Some(v) = agent.credentials.whatsapp.clone() {
        outbound.insert(WHATSAPP, v);
    }
    if let Some(v) = agent.credentials.telegram.clone() {
        outbound.insert(TELEGRAM, v);
    }
    if let Some(v) = agent.credentials.google.clone() {
        outbound.insert(GOOGLE, v);
    }

    let mut inbound: HashMap<Channel, Vec<String>> = HashMap::new();
    for binding in &agent.inbound_bindings {
        let channel: Channel = match binding.plugin.as_str() {
            "whatsapp" => WHATSAPP,
            "telegram" => TELEGRAM,
            _ => continue,
        };
        if let Some(ins) = &binding.instance {
            inbound
                .entry(channel)
                .or_insert_with(Vec::new)
                .push(ins.clone());
        }
    }

    let asymmetric_raw = agent.credentials.asymmetric_flags();
    let mut asymmetric: HashMap<Channel, bool> = HashMap::new();
    for (k, v) in asymmetric_raw {
        let channel: Channel = match k.as_str() {
            "whatsapp" => WHATSAPP,
            "telegram" => TELEGRAM,
            "google" => GOOGLE,
            _ => continue,
        };
        asymmetric.insert(channel, v);
    }

    AgentCredentialsInput {
        agent_id: agent.id.clone(),
        outbound,
        inbound,
        asymmetric_allowed: asymmetric,
    }
}

/// Convenience for `--check-config` / CLI: pretty-print either the
/// warnings or the accumulated error list to stderr and return an
/// exit code (0 = clean, 1 = errors, 2 = warnings-only).
pub fn print_report(bundle: &Result<CredentialsBundle, Vec<BuildError>>) -> i32 {
    match bundle {
        Ok(b) if b.warnings.is_empty() => {
            eprintln!("credentials: OK");
            0
        }
        Ok(b) => {
            eprintln!("credentials: OK with {} warning(s):", b.warnings.len());
            for w in &b.warnings {
                eprintln!("  - {w}");
            }
            2
        }
        Err(errs) => {
            eprintln!("credentials: FAILED with {} error(s):", errs.len());
            eprint!("{}", format_errors(errs));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_config::types::agents::{
        AgentConfig, HeartbeatConfig, ModelConfig, OutboundAllowlistConfig,
    };
    use agent_config::types::credentials::AgentCredentialsConfig;
    use tempfile::TempDir;

    fn minimal_agent(id: &str, wa_cred: Option<&str>) -> AgentConfig {
        let mut creds = AgentCredentialsConfig::default();
        if let Some(v) = wa_cred {
            creds.whatsapp = Some(v.to_string());
        }
        AgentConfig {
            id: id.into(),
            model: ModelConfig {
                provider: "stub".into(),
                model: "stub".into(),
            },
            plugins: vec![],
            heartbeat: HeartbeatConfig::default(),
            config: Default::default(),
            system_prompt: String::new(),
            workspace: String::new(),
            skills: vec![],
            skills_dir: "./skills".into(),
            transcripts_dir: String::new(),
            dreaming: Default::default(),
            workspace_git: Default::default(),
            tool_rate_limits: None,
            tool_args_validation: None,
            extra_docs: vec![],
            inbound_bindings: vec![],
            allowed_tools: vec![],
            sender_rate_limit: None,
            allowed_delegates: vec![],
            accept_delegates_from: vec![],
            description: String::new(),
            google_auth: None,
            outbound_allowlist: OutboundAllowlistConfig::default(),
            credentials: creds,
        }
    }

    fn wa_cfg(instance: Option<&str>, dir: &Path, allow: &[&str]) -> WhatsappPluginConfig {
        use agent_config::types::plugins::*;
        WhatsappPluginConfig {
            enabled: true,
            session_dir: dir.to_string_lossy().into_owned(),
            media_dir: format!("{}/media", dir.display()),
            credentials_file: None,
            acl: WhatsappAclConfig::default(),
            behavior: WhatsappBehaviorConfig::default(),
            rate_limit: WhatsappRateLimitConfig::default(),
            bridge: WhatsappBridgeConfig::default(),
            transcriber: WhatsappTranscriberConfig::default(),
            daemon: WhatsappDaemonConfig::default(),
            public_tunnel: Default::default(),
            instance: instance.map(|s| s.to_string()),
            allow_agents: allow.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn happy_path_one_agent_one_instance() {
        let dir = TempDir::new().unwrap();
        let wa_dir = dir.path().join("ana");
        std::fs::create_dir_all(&wa_dir).unwrap();
        let wa = vec![wa_cfg(Some("personal"), &wa_dir, &["ana"])];
        let agent = minimal_agent("ana", Some("personal"));
        let bundle = build_credentials(
            &[agent],
            &wa,
            &[],
            &GoogleAuthConfig::default(),
            StrictLevel::Strict,
        )
        .unwrap();
        assert!(bundle.resolver.resolve("ana", WHATSAPP).is_ok());
    }

    #[test]
    fn missing_instance_surfaces_with_available() {
        let dir = TempDir::new().unwrap();
        let wa_dir = dir.path().join("work");
        std::fs::create_dir_all(&wa_dir).unwrap();
        let wa = vec![wa_cfg(Some("work"), &wa_dir, &[])];
        let agent = minimal_agent("ana", Some("personal"));
        let err = build_credentials(
            &[agent],
            &wa,
            &[],
            &GoogleAuthConfig::default(),
            StrictLevel::Lenient,
        )
        .unwrap_err();
        assert!(err
            .iter()
            .any(|e| matches!(e, BuildError::MissingInstance { .. })));
    }

    #[test]
    fn duplicate_session_dir_is_caught() {
        let dir = TempDir::new().unwrap();
        let wa_dir = dir.path().join("shared");
        std::fs::create_dir_all(&wa_dir).unwrap();
        let wa = vec![
            wa_cfg(Some("a"), &wa_dir, &[]),
            wa_cfg(Some("b"), &wa_dir, &[]),
        ];
        let agent = minimal_agent("ana", Some("a"));
        let err = build_credentials(
            &[agent],
            &wa,
            &[],
            &GoogleAuthConfig::default(),
            StrictLevel::Lenient,
        )
        .unwrap_err();
        assert!(err
            .iter()
            .any(|e| matches!(e, BuildError::DuplicatePath { .. })));
    }
}
