//! Wire layer — turns `AppConfig` + `google-auth.yaml` into the
//! credential stores and resolver the runtime needs. Kept in this
//! crate (not `nexo-config`) so the config crate stays a pure data
//! shape and never pulls `tokio` / `dashmap`.
//!
//! The entry point is [`build_credentials`], called from `main.rs`
//! during boot. Operators can also call it via `--check-config`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use nexo_config::types::agents::AgentConfig;
use nexo_config::types::credentials::{GoogleAccountConfig, GoogleAuthConfig, GoogleAuthFile};
use nexo_config::types::plugins::PluginsConfig;

use crate::email::{load_email_secrets, EmailAccount, EmailCredentialStore};

// Wave 7 — opaque whatsapp cfg slice. Same pattern as telegram /
// email: minimal shape locally so nexo-auth stays decoupled from
// `nexo-plugin-whatsapp`. Reads `cfg.plugins.entries["whatsapp"]`.
#[derive(Debug, Clone, serde::Deserialize)]
struct WhatsappCredEntry {
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub session_dir: String,
    #[serde(default)]
    pub media_dir: String,
    #[serde(default)]
    pub allow_agents: Vec<String>,
    #[serde(flatten)]
    _rest: std::collections::BTreeMap<String, serde_yaml::Value>,
}

fn whatsapp_entries(plugins: &PluginsConfig) -> Vec<WhatsappCredEntry> {
    let Some(value) = plugins.entries.get("whatsapp") else {
        return Vec::new();
    };
    let seq: Vec<serde_yaml::Value> = match value {
        serde_yaml::Value::Sequence(s) => s.clone(),
        serde_yaml::Value::Mapping(_) => vec![value.clone()],
        _ => return Vec::new(),
    };
    seq.into_iter()
        .filter_map(|v| serde_yaml::from_value::<WhatsappCredEntry>(v).ok())
        .collect()
}

// Wave 6 — opaque telegram cfg slice. Same pattern as email Wave 5:
// minimal shape locally so nexo-auth stays decoupled from
// `nexo-plugin-telegram`. Reads `cfg.plugins.entries["telegram"]`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct TelegramAllowlistSlice {
    #[serde(default)]
    pub chat_ids: Vec<i64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TelegramCredEntry {
    pub token: String,
    #[serde(default)]
    pub instance: Option<String>,
    #[serde(default)]
    pub allow_agents: Vec<String>,
    #[serde(default)]
    pub allowlist: TelegramAllowlistSlice,
    #[serde(flatten)]
    _rest: std::collections::BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum TelegramCredShape {
    Single(TelegramCredEntry),
    Many(Vec<TelegramCredEntry>),
}

impl TelegramCredShape {
    fn into_vec(self) -> Vec<TelegramCredEntry> {
        match self {
            Self::Single(t) => vec![t],
            Self::Many(v) => v,
        }
    }
}

fn telegram_entries(plugins: &PluginsConfig) -> Vec<TelegramCredEntry> {
    let Some(value) = plugins.entries.get("telegram") else {
        return Vec::new();
    };
    match serde_yaml::from_value::<TelegramCredShape>(value.clone()) {
        Ok(shape) => shape.into_vec(),
        Err(e) => {
            tracing::warn!(
                target: "credentials.wire",
                error = %e,
                "failed to deserialize cfg.plugins.entries[\"telegram\"]; falling back \
                 to no telegram accounts"
            );
            Vec::new()
        }
    }
}

// Wave 5 — opaque email cfg slice. nexo_config no longer carries
// typed `EmailPluginConfig` (Phase 93 framework decoupling); we
// deserialize a minimal account-only shape from
// `cfg.plugins.entries["email"]` here. Plugin crate retains the
// full typed config; this auth path only needs (instance, address)
// for credential file loading.
#[derive(Debug, Clone, serde::Deserialize)]
struct EmailCredAccount {
    pub instance: String,
    pub address: String,
    // Catch-all so plugin-specific account fields (imap/smtp/folders/
    // filters/provider/bootstrap_limit) don't fail the deserialize.
    #[serde(flatten)]
    _rest: std::collections::BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct EmailCredTenant {
    #[serde(default)]
    pub accounts: Vec<EmailCredAccount>,
    #[serde(flatten)]
    _rest: std::collections::BTreeMap<String, serde_yaml::Value>,
}

/// `cfg.plugins.entries["email"]` can be either a single map (legacy
/// 0.4.x) or a sequence of tenant maps (0.5.0+). Same untagged shape
/// the plugin's `EmailPluginShape` uses — duplicated here so nexo-auth
/// stays decoupled from `nexo-plugin-email`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum EmailCredShape {
    Single(EmailCredTenant),
    Many(Vec<EmailCredTenant>),
}

impl EmailCredShape {
    fn flat_accounts(self) -> Vec<EmailCredAccount> {
        let tenants = match self {
            Self::Single(t) => vec![t],
            Self::Many(v) => v,
        };
        tenants.into_iter().flat_map(|t| t.accounts).collect()
    }
}

fn email_accounts_from_entries(plugins: &PluginsConfig) -> Vec<EmailCredAccount> {
    let Some(value) = plugins.entries.get("email") else {
        return Vec::new();
    };
    match serde_yaml::from_value::<EmailCredShape>(value.clone()) {
        Ok(shape) => shape.flat_accounts(),
        Err(e) => {
            tracing::warn!(
                target: "credentials.wire",
                error = %e,
                "failed to deserialize cfg.plugins.entries[\"email\"] for credential wiring; \
                 falling back to no email accounts. Plugin will still receive raw entries \
                 via plugin.configure."
            );
            Vec::new()
        }
    }
}
use crate::error::BuildError;
use crate::gauntlet::{
    canonicalize_session_dirs, check_duplicate_paths, check_permissions, check_prefix_overlap,
    format_errors, PathClaim,
};
use crate::generic_store::GenericCredentialStore;
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
    /// Phase 93.9.f — flipped to `pub(crate)` now that every
    /// external caller migrated to the typed accessors
    /// ([`Self::google_account`], [`Self::whatsapp_account`], …)
    /// or to the v2 generic path ([`Self::stores_v2`]). External
    /// code constructs the bundle via [`Self::for_testing`].
    pub(crate) stores: CredentialStores,
    pub resolver: Arc<AgentCredentialResolver>,
    /// Per-`(channel, instance)` circuit breakers shared with plugin
    /// tools. Created with default config; failure on one account
    /// never trips another.
    pub breakers: Arc<crate::breaker::BreakerRegistry>,
    pub warnings: Vec<String>,
    /// Phase 93.7 — opt-in plugin-contributed credential stores
    /// keyed by `manifest.plugin.id`. Empty at boot; populated by
    /// the init-loop after each plugin's `init(ctx)` succeeds via
    /// `NexoPlugin::credential_store()`. Phase 93.9.a hides the
    /// typed `stores` field; .b-.f migrate the remaining runtime
    /// consumers to walk this map exclusively.
    pub stores_v2: DashMap<String, Arc<dyn GenericCredentialStore>>,
}

impl CredentialsBundle {
    /// Phase 93.9.f — public test-only constructor. External
    /// crates (`crates/poller`, `crates/setup`, integration
    /// tests) used to build the bundle directly by initialising
    /// every field; now that [`Self::stores`] is `pub(crate)`,
    /// this helper hands back an empty bundle suitable for fixtures
    /// without exposing the typed `CredentialStores` shape.
    pub fn empty_for_testing() -> Self {
        Self {
            stores: CredentialStores::empty(),
            resolver: Arc::new(AgentCredentialResolver::empty()),
            breakers: Arc::new(crate::breaker::BreakerRegistry::default()),
            warnings: Vec::new(),
            stores_v2: DashMap::new(),
        }
    }

    /// Phase 93.9.b — typed-account accessor for the daemon-owned
    /// Google channel. Plugin extraction is deferred; runtime
    /// consumers (gmail / google-calendar pollers, setup wizard
    /// services) use this method instead of touching
    /// `bundle.stores.google.account(...)` directly. Phase 93.9.f
    /// flips `stores` to `pub(crate)` once every external caller
    /// has migrated to these accessors.
    pub fn google_account(&self, id: &str) -> Option<&crate::google::GoogleAccount> {
        self.stores.google.account(id)
    }

    /// Same shape as [`Self::google_account`] but keyed by
    /// `agent_id` — Google enforces a 1:1 account-to-agent rule, so
    /// gmail-poller jobs that only know the agent can still recover
    /// the account.
    pub fn google_account_for_agent(
        &self,
        agent_id: &str,
    ) -> Option<&crate::google::GoogleAccount> {
        self.stores.google.account_for_agent(agent_id)
    }

    /// Phase 93.9.b — daemon-owned WhatsApp typed accessor. The
    /// subprocess plugin contributes a `RemoteCredentialStore` to
    /// `stores_v2` but legacy in-process consumers (gauntlet,
    /// observability surface) still need typed access.
    pub fn whatsapp_account(&self, instance: &str) -> Option<&crate::whatsapp::WhatsappAccount> {
        self.stores.whatsapp.account(instance)
    }

    /// Phase 93.9.b — daemon-owned Telegram typed accessor.
    pub fn telegram_account(&self, instance: &str) -> Option<&crate::telegram::TelegramAccount> {
        self.stores.telegram.account(instance)
    }

    /// Phase 93.9.b — daemon-owned Email typed accessor. Wizard +
    /// poller migrators in 93.9.c-e replace direct
    /// `bundle.stores.email.account()` calls with this method.
    pub fn email_account(&self, instance: &str) -> Option<&crate::email::EmailAccount> {
        self.stores.email.account(instance)
    }

    /// Phase 93.9.b — refresh-lock accessor for Google's
    /// concurrent-refresh guard. Hides the typed
    /// `stores.google.refresh_lock(...)` call.
    pub fn google_refresh_lock(
        &self,
        handle: &crate::handle::CredentialHandle,
    ) -> Option<Arc<tokio::sync::Mutex<()>>> {
        self.stores.google.refresh_lock(handle)
    }

    /// Phase 93.9.f — typed `Arc<GoogleCredentialStore>` accessor
    /// for plugin constructors that need the whole store (not a
    /// single account). Hides the aggregate `CredentialStores`
    /// shape from external code while preserving the typed-Arc
    /// surface daemon-internal plugins still rely on.
    pub fn google_store(&self) -> Arc<crate::google::GoogleCredentialStore> {
        Arc::clone(&self.stores.google)
    }

    /// Phase 93.9.f — typed `Arc<EmailCredentialStore>` accessor.
    pub fn email_store(&self) -> Arc<crate::email::EmailCredentialStore> {
        Arc::clone(&self.stores.email)
    }

    /// Phase 93.9.f — typed `Arc<WhatsappCredentialStore>` accessor.
    pub fn whatsapp_store(&self) -> Arc<crate::whatsapp::WhatsappCredentialStore> {
        Arc::clone(&self.stores.whatsapp)
    }

    /// Phase 93.9.f — typed `Arc<TelegramCredentialStore>` accessor.
    pub fn telegram_store(&self) -> Arc<crate::telegram::TelegramCredentialStore> {
        Arc::clone(&self.stores.telegram)
    }

    /// Account count for `channel`. Reads from `stores_v2` first
    /// (plugin-contributed `GenericCredentialStore`); falls back to
    /// the internal typed `CredentialStores` so boot-time consumers
    /// see correct counts before plugin init lands.
    ///
    /// `stores_v2.list()` is async; this accessor blocks via
    /// [`tokio::task::block_in_place`]. Callers must be on a
    /// multi-thread Tokio runtime. The daemon's `#[tokio::main]`
    /// satisfies this; unit tests should use
    /// `#[tokio::test(flavor = "multi_thread")]`.
    pub fn account_count(&self, channel: Channel) -> usize {
        if let Some(store) = self.stores_v2.get(channel).map(|e| e.value().clone()) {
            let list = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(store.list())
            });
            return list.len();
        }
        match channel {
            WHATSAPP => self.stores.whatsapp.list().len(),
            TELEGRAM => self.stores.telegram.list().len(),
            GOOGLE => self.stores.google.list().len(),
            c if c == crate::handle::EMAIL => self.stores.email.list().len(),
            _ => 0,
        }
    }
}

impl std::fmt::Debug for CredentialsBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsBundle")
            .field("whatsapp_instances", &self.stores.whatsapp.list().len())
            .field("telegram_instances", &self.stores.telegram.list().len())
            .field("google_accounts", &self.stores.google.list().len())
            .field("email_accounts", &self.stores.email.list().len())
            .field("stores_v2_count", &self.stores_v2.len())
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
    let resolved = nexo_config::env::resolve_placeholders(&raw, "google-auth.yaml")?;
    let file: GoogleAuthFile = serde_yaml::from_str(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))?;
    Ok(file.google_auth)
}

/// Run the boot gauntlet and build stores + resolver. Every error is
/// accumulated; a single `anyhow::Error` with a multi-line body is
/// returned so operators see every misconfiguration at once.
///
/// Phase 93.5.b — takes `&PluginsConfig` (instead of three explicit
/// per-channel parameters) so callers stop knowing the typed
/// per-channel shape. Internal slicing pulls `whatsapp`, `telegram`,
/// and `email` from the bundle. `GoogleAuthConfig` stays separate
/// because it ships from a different YAML file
/// (`plugins/google-auth.yaml`, loaded by [`load_google_auth`]).
pub fn build_credentials(
    agents: &[AgentConfig],
    plugins: &PluginsConfig,
    google: &GoogleAuthConfig,
    secrets_dir: &Path,
    strict: StrictLevel,
) -> Result<CredentialsBundle, Vec<BuildError>> {
    // Wave 7 — whatsapp cfg via opaque entries.
    let whatsapp = whatsapp_entries(plugins);
    // Wave 6 — telegram cfg flows through opaque entries; same
    // pattern as email Wave 5. nexo-config no longer carries typed
    // telegram. Local minimal shape suffices for credential wiring.
    let telegram_entries_vec = telegram_entries(plugins);
    // Wave 5 — opaque email cfg. Flatten across all declared tenants
    // (multi-tenant credential aggregation). The full plugin config
    // stays inside `nexo-plugin-email`; we only need account-level
    // (instance, address) here for secrets loading.
    let email_accounts_decl = email_accounts_from_entries(plugins);
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
    let tg_accounts: Vec<TelegramAccount> = telegram_entries_vec
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
    // Strict mode the legacy form is an error: forces the
    // move to google-auth.yaml.
    let mut legacy_warnings: Vec<String> = Vec::new();
    for agent in agents {
        let Some(g) = &agent.google_auth else {
            continue;
        };
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
        // directly rather than load from disk. (Older consumers
        // ignore these accounts if the files do not exist.)
        goog_accounts.push(GoogleAccount {
            id: agent.id.clone(),
            agent_id: agent.id.clone(),
            client_id_path: std::path::PathBuf::from(format!("inline:{}", g.client_id)),
            client_secret_path: std::path::PathBuf::from(format!("inline:{}", g.client_secret)),
            token_path: std::path::PathBuf::from(&g.token_file),
            scopes: g.scopes.clone(),
        });
    }

    // ── Email accounts: load secrets/email/<inst>.toml for every
    //    declared instance. Missing files / malformed TOML accumulate
    //    into `errors` like every other gauntlet branch.
    let (email_accounts, email_warnings, email_errors) = if email_accounts_decl.is_empty() {
        (Vec::<EmailAccount>::new(), Vec::new(), Vec::new())
    } else {
        let declared: Vec<(String, String)> = email_accounts_decl
            .iter()
            .map(|a| (a.instance.clone(), a.address.clone()))
            .collect();
        load_email_secrets(secrets_dir, &declared)
    };
    errors.extend(email_errors);

    // Cross-store check: every `OAuth2Google` email account must point
    // at an existing google_account_id. Strict in both modes — a typo
    // here would silently fall through to a runtime NotFound.
    if !email_accounts_decl.is_empty() {
        let google_ids: std::collections::HashSet<&str> =
            goog_accounts.iter().map(|a| a.id.as_str()).collect();
        for acct in &email_accounts {
            if let crate::email::EmailAuth::OAuth2Google {
                google_account_id, ..
            } = &acct.auth
            {
                if !google_ids.contains(google_account_id.as_str()) {
                    errors.push(BuildError::Credential {
                        channel: crate::handle::EMAIL,
                        instance: acct.instance.clone(),
                        source: crate::error::CredentialError::OrphanedGoogleRef {
                            account: acct.instance.clone(),
                            google_account_id: google_account_id.clone(),
                        },
                    });
                }
            }
        }
        // Permission check on TOML files (mode 0o600).
        let mut email_perm_paths: Vec<(Channel, String, std::path::PathBuf)> = Vec::new();
        for acct in &email_accounts_decl {
            let p = secrets_dir
                .join("email")
                .join(format!("{}.toml", acct.instance));
            if p.exists() {
                email_perm_paths.push((crate::handle::EMAIL, acct.instance.clone(), p));
            }
        }
        let email_perm_errs = check_permissions(&email_perm_paths);
        errors.extend(email_perm_errs);
    }

    let stores = CredentialStores {
        whatsapp: Arc::new(WhatsappCredentialStore::new(wa_accounts.clone())),
        telegram: Arc::new(TelegramCredentialStore::new(tg_accounts.clone())),
        google: Arc::new(GoogleCredentialStore::new(goog_accounts.clone())),
        email: Arc::new(EmailCredentialStore::new(email_accounts.clone())),
    };

    // Per-store self-check (missing scopes / empty token etc).
    let wa_report = stores.whatsapp.validate();
    let tg_report = stores.telegram.validate();
    let g_report = stores.google.validate();
    let e_report = stores.email.validate();
    errors.extend(wa_report.errors);
    errors.extend(tg_report.errors);
    errors.extend(g_report.errors);
    errors.extend(e_report.errors);
    let mut warnings: Vec<String> = wa_report
        .warnings
        .into_iter()
        .chain(tg_report.warnings)
        .chain(g_report.warnings)
        .chain(e_report.warnings)
        .chain(email_warnings)
        .chain(legacy_warnings)
        .collect();

    // Counter for dashboards.
    crate::telemetry::set_accounts_total(WHATSAPP, wa_accounts.len() as u64);
    crate::telemetry::set_accounts_total(TELEGRAM, tg_accounts.len() as u64);
    crate::telemetry::set_accounts_total(GOOGLE, goog_accounts.len() as u64);
    crate::telemetry::set_accounts_total(crate::handle::EMAIL, email_accounts.len() as u64);

    // ── 3. Build resolver inputs from agent configs ──
    let inputs: Vec<AgentCredentialsInput> = agents.iter().map(agent_to_input).collect();

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
                for channel in [WHATSAPP, TELEGRAM, GOOGLE, crate::handle::EMAIL] {
                    let bound = resolver.resolve(&agent.id, channel).is_ok();
                    crate::telemetry::set_binding(channel, &agent.id, bound);
                }
            }
            Ok(CredentialsBundle {
                stores,
                resolver: Arc::new(resolver),
                breakers: Arc::new(crate::breaker::BreakerRegistry::default()),
                warnings,
                stores_v2: DashMap::new(),
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
            inbound.entry(channel).or_default().push(ins.clone());
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

/// Hot-reload the credential resolver in-place. Re-reads YAML from
/// `config_dir`, runs the gauntlet, and atomically swaps the new
/// bindings into the existing `Arc<AgentCredentialResolver>` held by
/// every plugin tool. Returns the per-channel account counts and any
/// warnings the rebuild surfaced.
pub fn reload_resolver(
    config_dir: &Path,
    secrets_dir: &Path,
    bundle: &CredentialsBundle,
    strict: StrictLevel,
) -> Result<ReloadOutcome, Vec<BuildError>> {
    let cfg = match nexo_config::AppConfig::load(config_dir) {
        Ok(c) => c,
        Err(e) => {
            return Err(vec![BuildError::Credential {
                channel: crate::handle::WHATSAPP,
                instance: "<config>".into(),
                source: crate::error::CredentialError::Unreadable {
                    path: config_dir.to_path_buf(),
                    source: std::io::Error::other(e.to_string()),
                },
            }])
        }
    };
    let google = match load_google_auth(config_dir) {
        Ok(g) => g,
        Err(e) => {
            return Err(vec![BuildError::Credential {
                channel: crate::handle::GOOGLE,
                instance: "<google-auth.yaml>".into(),
                source: crate::error::CredentialError::Unreadable {
                    path: config_dir.join("plugins/google-auth.yaml"),
                    source: std::io::Error::other(e.to_string()),
                },
            }])
        }
    };

    // Build fresh stores so removed/added accounts surface immediately.
    // Existing plugin instances keep using their old session_dir until
    // the daemon restarts; this reload only affects the resolver +
    // tool-side ACL, which is the V1 invariant we promised.
    let fresh = build_credentials(
        &cfg.agents.agents,
        &cfg.plugins,
        &google,
        secrets_dir,
        strict,
    )?;

    // Drive the resolver's atomic swap from the freshly-built bindings.
    let inputs: Vec<crate::resolver::AgentCredentialsInput> = cfg
        .agents
        .agents
        .iter()
        .map(crate::wire::agent_to_input_pub)
        .collect();
    bundle.resolver.rebuild(&inputs, &fresh.stores, strict)?;

    use crate::store::CredentialStore;
    Ok(ReloadOutcome {
        accounts_wa: fresh.stores.whatsapp.list().len(),
        accounts_tg: fresh.stores.telegram.list().len(),
        accounts_google: fresh.stores.google.list().len(),
        accounts_email: fresh.stores.email.list().len(),
        warnings: fresh.warnings,
        version: bundle.resolver.version(),
    })
}

#[derive(Debug, serde::Serialize)]
pub struct ReloadOutcome {
    pub accounts_wa: usize,
    pub accounts_tg: usize,
    pub accounts_google: usize,
    pub accounts_email: usize,
    pub warnings: Vec<String>,
    pub version: u64,
}

/// Internal helper exposed for `reload_resolver`. Mirrors the private
/// `agent_to_input` defined inside this module.
pub fn agent_to_input_pub(agent: &AgentConfig) -> crate::resolver::AgentCredentialsInput {
    agent_to_input(agent)
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
    use nexo_config::types::agents::{
        AgentConfig, HeartbeatConfig, ModelConfig, OutboundAllowlistConfig,
    };
    use nexo_config::types::credentials::AgentCredentialsConfig;
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
            language: None,
            locale_prompts: Default::default(),
            skill_overrides: Default::default(),
            link_understanding: serde_json::Value::Null,
            web_search: serde_json::Value::Null,
            pairing_policy: serde_json::Value::Null,
            context_optimization: None,
            dispatch_policy: Default::default(),
            plan_mode: Default::default(),
            remote_triggers: Vec::new(),
            lsp: nexo_config::types::lsp::LspPolicy::default(),
            config_tool: nexo_config::types::config_tool::ConfigToolPolicy::default(),
            team: nexo_config::types::team::TeamPolicy::default(),
            proactive: Default::default(),
            repl: Default::default(),
            auto_dream: None,
            assistant_mode: None,
            away_summary: None,
            brief: None,
            channels: None,
            auto_approve: false,
            extract_memories: None,
            event_subscribers: Vec::new(),
            tenant_id: None,
            extensions_config: std::collections::BTreeMap::new(),
            active: true,
        }
    }

    /// Wave 7 — opaque whatsapp fixture built as a serde_yaml::Value
    /// directly. nexo-auth no longer depends on `nexo_config::Whatsapp*`
    /// typed structs.
    fn wa_cfg(instance: Option<&str>, dir: &Path, allow: &[&str]) -> serde_yaml::Value {
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            serde_yaml::Value::String("enabled".into()),
            serde_yaml::Value::Bool(true),
        );
        map.insert(
            serde_yaml::Value::String("session_dir".into()),
            serde_yaml::Value::String(dir.to_string_lossy().into_owned()),
        );
        map.insert(
            serde_yaml::Value::String("media_dir".into()),
            serde_yaml::Value::String(format!("{}/media", dir.display())),
        );
        if let Some(inst) = instance {
            map.insert(
                serde_yaml::Value::String("instance".into()),
                serde_yaml::Value::String(inst.into()),
            );
        }
        let allow_seq: Vec<serde_yaml::Value> = allow
            .iter()
            .map(|s| serde_yaml::Value::String((*s).into()))
            .collect();
        map.insert(
            serde_yaml::Value::String("allow_agents".into()),
            serde_yaml::Value::Sequence(allow_seq),
        );
        serde_yaml::Value::Mapping(map)
    }

    fn plugins_with_whatsapp(wa: Vec<serde_yaml::Value>) -> PluginsConfig {
        let mut entries: std::collections::BTreeMap<String, serde_yaml::Value> =
            std::collections::BTreeMap::new();
        if !wa.is_empty() {
            entries.insert("whatsapp".to_string(), serde_yaml::Value::Sequence(wa));
        }
        PluginsConfig {
            entries,
            ..PluginsConfig::default()
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
            &plugins_with_whatsapp(wa),
            &GoogleAuthConfig::default(),
            std::path::Path::new("/nonexistent"),
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
            &plugins_with_whatsapp(wa),
            &GoogleAuthConfig::default(),
            std::path::Path::new("/nonexistent"),
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
            &plugins_with_whatsapp(wa),
            &GoogleAuthConfig::default(),
            std::path::Path::new("/nonexistent"),
            StrictLevel::Lenient,
        )
        .unwrap_err();
        assert!(err
            .iter()
            .any(|e| matches!(e, BuildError::DuplicatePath { .. })));
    }

    /// Phase 93.9.b — `Bundle::google_account` accessor returns
    /// the typed account when present and `None` when absent.
    #[test]
    fn google_account_accessor_returns_known_id() {
        let dir = TempDir::new().unwrap();
        let wa_dir = dir.path().join("ana");
        std::fs::create_dir_all(&wa_dir).unwrap();
        let wa = vec![wa_cfg(Some("personal"), &wa_dir, &["ana"])];
        let agent = minimal_agent("ana", Some("personal"));
        let bundle = build_credentials(
            &[agent],
            &plugins_with_whatsapp(wa),
            &GoogleAuthConfig::default(),
            std::path::Path::new("/nonexistent"),
            StrictLevel::Strict,
        )
        .unwrap();
        assert!(bundle.google_account("nonexistent").is_none());
        assert!(bundle.whatsapp_account("personal").is_some());
        assert!(bundle.whatsapp_account("ghost").is_none());
        assert!(bundle.telegram_account("anything").is_none());
        assert!(bundle.email_account("anything").is_none());
    }

    /// Phase 93.9.a — `account_count` walks `stores_v2` first
    /// and falls back to the internal typed stores for channels
    /// with no plugin contribution yet.
    #[tokio::test(flavor = "multi_thread")]
    async fn account_count_falls_back_to_typed_when_v2_empty() {
        let dir = TempDir::new().unwrap();
        let wa_dir = dir.path().join("ana");
        std::fs::create_dir_all(&wa_dir).unwrap();
        let wa = vec![wa_cfg(Some("personal"), &wa_dir, &["ana"])];
        let agent = minimal_agent("ana", Some("personal"));
        let bundle = build_credentials(
            &[agent],
            &plugins_with_whatsapp(wa),
            &GoogleAuthConfig::default(),
            std::path::Path::new("/nonexistent"),
            StrictLevel::Strict,
        )
        .unwrap();
        assert_eq!(bundle.account_count(WHATSAPP), 1);
        assert_eq!(bundle.account_count(TELEGRAM), 0);
        assert_eq!(bundle.account_count(GOOGLE), 0);
    }

    /// Phase 93.7 — `build_credentials` initialises `stores_v2`
    /// as an empty `DashMap`. Plugin contributions land later in
    /// the init-loop; no daemon-side auto-wrap of typed stores.
    #[test]
    fn build_credentials_initialises_empty_stores_v2() {
        let dir = TempDir::new().unwrap();
        let wa_dir = dir.path().join("ana");
        std::fs::create_dir_all(&wa_dir).unwrap();
        let wa = vec![wa_cfg(Some("personal"), &wa_dir, &["ana"])];
        let agent = minimal_agent("ana", Some("personal"));
        let bundle = build_credentials(
            &[agent],
            &plugins_with_whatsapp(wa),
            &GoogleAuthConfig::default(),
            std::path::Path::new("/nonexistent"),
            StrictLevel::Strict,
        )
        .unwrap();
        assert_eq!(
            bundle.stores_v2.len(),
            0,
            "Phase 93.7: stores_v2 is empty at boot — plugin contributions land via NexoPlugin::credential_store()",
        );
    }
}
