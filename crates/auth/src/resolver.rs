//! Agent → credential resolver. Produced by [`AgentCredentialResolver::build`]
//! after the boot-time gauntlet validates every invariant listed in
//! `proyecto/docs/credentials.md`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;

use crate::error::{BuildError, ResolveError};
use crate::google::GoogleCredentialStore;
use crate::handle::{AgentId, Channel, CredentialHandle, GOOGLE, TELEGRAM, WHATSAPP};
use crate::store::CredentialStore;
use crate::telegram::TelegramCredentialStore;
use crate::whatsapp::WhatsappCredentialStore;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum StrictLevel {
    /// Warn-only for soft violations (asymmetric binding, unused
    /// account). Default during migration so back-compat configs
    /// keep booting.
    #[default]
    Lenient,
    /// Promote every warning to a hard error. Intended for CI lane
    /// (`--check-config`) and eventually as the default in V2.
    Strict,
}

/// View of the per-agent config that the resolver needs — keeps this
/// crate independent of `agent-config`. The wiring crate maps from
/// [`AgentConfig`](agent_config::types::agents::AgentConfig) into this
/// shape just before calling [`AgentCredentialResolver::build`].
#[derive(Debug, Clone)]
pub struct AgentCredentialsInput {
    pub agent_id: String,
    /// Outbound binding per channel (`channel -> account_id`).
    pub outbound: HashMap<Channel, String>,
    /// Inbound instance subscriptions per channel (parsed from
    /// `inbound_bindings`). Used for the asymmetric-binding check.
    pub inbound: HashMap<Channel, Vec<String>>,
    /// Per-channel opt-out for the symmetric-binding warning.
    pub asymmetric_allowed: HashMap<Channel, bool>,
}

#[derive(Clone)]
pub struct CredentialStores {
    pub whatsapp: Arc<WhatsappCredentialStore>,
    pub telegram: Arc<TelegramCredentialStore>,
    pub google: Arc<GoogleCredentialStore>,
}

impl CredentialStores {
    pub fn empty() -> Self {
        Self {
            whatsapp: Arc::new(WhatsappCredentialStore::empty()),
            telegram: Arc::new(TelegramCredentialStore::empty()),
            google: Arc::new(GoogleCredentialStore::empty()),
        }
    }
}

/// Hot-reloadable resolver. Bindings + warnings + strict level live in
/// `ArcSwap`/`ArcSwap`/atomic so the runtime can swap them without
/// re-creating the `Arc<AgentCredentialResolver>` every plugin/tool
/// holds.
#[derive(Debug)]
pub struct AgentCredentialResolver {
    bindings: ArcSwap<HashMap<AgentId, HashMap<Channel, CredentialHandle>>>,
    warnings: ArcSwap<Vec<String>>,
    strict: ArcSwap<StrictLevel>,
    version: AtomicU64,
}

impl AgentCredentialResolver {
    pub fn empty() -> Self {
        Self {
            bindings: ArcSwap::from_pointee(HashMap::new()),
            warnings: ArcSwap::from_pointee(Vec::new()),
            strict: ArcSwap::from_pointee(StrictLevel::default()),
            version: AtomicU64::new(0),
        }
    }

    /// Lookup the outbound handle for `(agent, channel)`. Never panics
    /// on missing bindings — returns [`ResolveError::Unbound`] so the
    /// calling tool can surface a clean error to the LLM.
    pub fn resolve(
        &self,
        agent_id: &str,
        channel: Channel,
    ) -> Result<CredentialHandle, ResolveError> {
        self.bindings
            .load()
            .get(agent_id)
            .and_then(|m| m.get(channel))
            .cloned()
            .ok_or(ResolveError::Unbound {
                agent: agent_id.to_string(),
                channel,
            })
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    pub fn warnings(&self) -> Vec<String> {
        self.warnings.load().as_ref().clone()
    }

    pub fn strict(&self) -> StrictLevel {
        **self.strict.load()
    }

    /// Atomic hot-reload — replaces bindings, warnings, and strict
    /// level in one swap. Existing `CredentialHandle`s already issued
    /// to in-flight tool calls keep working (handles are by-value
    /// clones, the resolver only mediates lookup of *future* calls).
    pub fn replace_state(
        &self,
        new_bindings: HashMap<AgentId, HashMap<Channel, CredentialHandle>>,
        new_warnings: Vec<String>,
        new_strict: StrictLevel,
    ) {
        self.bindings.store(Arc::new(new_bindings));
        self.warnings.store(Arc::new(new_warnings));
        self.strict.store(Arc::new(new_strict));
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Validate every agent binding against the given stores. Returns
    /// [`Ok`] with the resolver plus soft warnings, or [`Err`] with
    /// every accumulated invariant violation so the operator can fix
    /// them in one edit.
    pub fn build(
        agents: &[AgentCredentialsInput],
        stores: &CredentialStores,
        strict: StrictLevel,
    ) -> Result<Self, Vec<BuildError>> {
        let mut errors: Vec<BuildError> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut bindings: HashMap<AgentId, HashMap<Channel, CredentialHandle>> =
            HashMap::new();

        for agent in agents {
            let mut per_channel: HashMap<Channel, CredentialHandle> = HashMap::new();
            for channel in [WHATSAPP, TELEGRAM, GOOGLE] {
                let outbound = agent.outbound.get(channel).cloned();
                let inbound = agent.inbound.get(channel).cloned().unwrap_or_default();
                let asymmetric_ok =
                    *agent.asymmetric_allowed.get(channel).unwrap_or(&false);

                // Back-compat inference: no explicit outbound + single
                // inbound instance → use that one.
                let account_id = match outbound {
                    Some(a) => Some(a),
                    None => match inbound.len() {
                        0 => None,
                        1 => Some(inbound[0].clone()),
                        _ => {
                            errors.push(BuildError::AmbiguousOutbound {
                                channel,
                                agent: agent.agent_id.clone(),
                                instances: inbound.clone(),
                            });
                            continue;
                        }
                    },
                };

                let Some(account_id) = account_id else {
                    continue;
                };

                // Account must exist in the store.
                let available = store_list(stores, channel);
                if !available.iter().any(|a| a == &account_id) {
                    errors.push(BuildError::MissingInstance {
                        channel,
                        agent: agent.agent_id.clone(),
                        account: account_id.clone(),
                        available,
                    });
                    continue;
                }

                // Account's allow_agents must accept this agent.
                let allow = store_allow_agents(stores, channel, &account_id);
                if !allow.is_empty() && !allow.iter().any(|a| a == &agent.agent_id) {
                    errors.push(BuildError::AllowAgentsExcludes {
                        channel,
                        instance: account_id.clone(),
                        agent: agent.agent_id.clone(),
                    });
                    continue;
                }

                // Asymmetric-binding warning (outbound ≠ inbound).
                if !inbound.is_empty()
                    && !inbound.iter().any(|i| i == &account_id)
                    && !asymmetric_ok
                {
                    let msg = BuildError::AsymmetricBinding {
                        channel,
                        agent: agent.agent_id.clone(),
                        outbound: account_id.clone(),
                        inbound: inbound.join(","),
                    };
                    match strict {
                        StrictLevel::Strict => errors.push(msg),
                        StrictLevel::Lenient => warnings.push(msg.to_string()),
                    }
                }

                // Issue the handle through the store — re-runs the
                // allow_agents check and enforces Google's 1:1 rule.
                match store_issue(stores, channel, &account_id, &agent.agent_id) {
                    Ok(handle) => {
                        per_channel.insert(channel, handle);
                    }
                    Err(source) => {
                        errors.push(BuildError::Credential {
                            channel,
                            instance: account_id.clone(),
                            source,
                        });
                    }
                }
            }
            if !per_channel.is_empty() {
                bindings.insert(Arc::from(agent.agent_id.as_str()), per_channel);
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self {
            bindings: ArcSwap::from_pointee(bindings),
            warnings: ArcSwap::from_pointee(warnings),
            strict: ArcSwap::from_pointee(strict),
            version: AtomicU64::new(1),
        })
    }

    /// Reload entry point — runs `build` against fresh inputs and
    /// atomically swaps the new state into `self`. Used by the
    /// admin endpoint and integration tests.
    pub fn rebuild(
        &self,
        agents: &[AgentCredentialsInput],
        stores: &CredentialStores,
        strict: StrictLevel,
    ) -> Result<(), Vec<BuildError>> {
        let fresh = Self::build(agents, stores, strict)?;
        // Move state out of the freshly-built resolver into self.
        let new_bindings = fresh.bindings.load_full();
        let new_warnings = fresh.warnings.load_full();
        let new_strict = **fresh.strict.load();
        self.replace_state(
            (*new_bindings).clone(),
            (*new_warnings).clone(),
            new_strict,
        );
        Ok(())
    }

    /// Test-only constructor that takes raw bindings. Not intended for
    /// production code — [`Self::build`] is the only validated path.
    #[doc(hidden)]
    pub fn from_raw(
        bindings: HashMap<AgentId, HashMap<Channel, CredentialHandle>>,
    ) -> Self {
        Self {
            bindings: ArcSwap::from_pointee(bindings),
            warnings: ArcSwap::from_pointee(Vec::new()),
            strict: ArcSwap::from_pointee(StrictLevel::default()),
            version: AtomicU64::new(1),
        }
    }
}

fn store_list(stores: &CredentialStores, channel: Channel) -> Vec<String> {
    match channel {
        WHATSAPP => stores.whatsapp.list(),
        TELEGRAM => stores.telegram.list(),
        GOOGLE => stores.google.list(),
        _ => Vec::new(),
    }
}

fn store_allow_agents(
    stores: &CredentialStores,
    channel: Channel,
    account_id: &str,
) -> Vec<String> {
    match channel {
        WHATSAPP => stores.whatsapp.allow_agents(account_id),
        TELEGRAM => stores.telegram.allow_agents(account_id),
        GOOGLE => stores.google.allow_agents(account_id),
        _ => Vec::new(),
    }
}

fn store_issue(
    stores: &CredentialStores,
    channel: Channel,
    account_id: &str,
    agent_id: &str,
) -> Result<CredentialHandle, crate::error::CredentialError> {
    match channel {
        WHATSAPP => stores.whatsapp.issue(account_id, agent_id),
        TELEGRAM => stores.telegram.issue(account_id, agent_id),
        GOOGLE => stores.google.issue(account_id, agent_id),
        _ => Err(crate::error::CredentialError::NotFound {
            channel,
            account: account_id.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::GoogleAccount;
    use crate::telegram::TelegramAccount;
    use crate::whatsapp::WhatsappAccount;
    use std::path::PathBuf;

    fn wa(instance: &str, allow: &[&str]) -> WhatsappAccount {
        WhatsappAccount {
            instance: instance.into(),
            session_dir: PathBuf::from(format!("/tmp/wa-{instance}")),
            media_dir: PathBuf::from(format!("/tmp/wa-{instance}/media")),
            allow_agents: allow.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn tg(instance: &str, allow: &[&str]) -> TelegramAccount {
        TelegramAccount {
            instance: instance.into(),
            token: "t".into(),
            allow_agents: allow.iter().map(|s| s.to_string()).collect(),
            allowed_chat_ids: vec![],
        }
    }

    fn ga(id: &str, agent: &str) -> GoogleAccount {
        GoogleAccount {
            id: id.into(),
            agent_id: agent.into(),
            client_id_path: PathBuf::from("/tmp/cid"),
            client_secret_path: PathBuf::from("/tmp/csec"),
            token_path: PathBuf::from("/tmp/tok"),
            scopes: vec![],
        }
    }

    fn stores(wa_list: Vec<WhatsappAccount>, tg_list: Vec<TelegramAccount>, g_list: Vec<GoogleAccount>) -> CredentialStores {
        CredentialStores {
            whatsapp: Arc::new(WhatsappCredentialStore::new(wa_list)),
            telegram: Arc::new(TelegramCredentialStore::new(tg_list)),
            google: Arc::new(GoogleCredentialStore::new(g_list)),
        }
    }

    fn input(id: &str, out: &[(Channel, &str)], inb: &[(Channel, &[&str])]) -> AgentCredentialsInput {
        let mut outbound = HashMap::new();
        for (c, a) in out {
            outbound.insert(*c, a.to_string());
        }
        let mut inbound = HashMap::new();
        for (c, ins) in inb {
            inbound.insert(*c, ins.iter().map(|s| s.to_string()).collect());
        }
        AgentCredentialsInput {
            agent_id: id.into(),
            outbound,
            inbound,
            asymmetric_allowed: HashMap::new(),
        }
    }

    #[test]
    fn happy_path_binds_all_three_channels() {
        let s = stores(
            vec![wa("personal", &["ana"])],
            vec![tg("ana_bot", &["ana"])],
            vec![ga("ana@x", "ana")],
        );
        let inp = input(
            "ana",
            &[(WHATSAPP, "personal"), (TELEGRAM, "ana_bot"), (GOOGLE, "ana@x")],
            &[],
        );
        let r = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Strict).unwrap();
        assert!(r.resolve("ana", WHATSAPP).is_ok());
        assert!(r.resolve("ana", TELEGRAM).is_ok());
        assert!(r.resolve("ana", GOOGLE).is_ok());
    }

    #[test]
    fn missing_instance_rejected_with_available_list() {
        let s = stores(vec![wa("work", &[])], vec![], vec![]);
        let inp = input("ana", &[(WHATSAPP, "personal")], &[]);
        let err = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Lenient).unwrap_err();
        assert_eq!(err.len(), 1);
        match &err[0] {
            BuildError::MissingInstance {
                agent, account, available, ..
            } => {
                assert_eq!(agent, "ana");
                assert_eq!(account, "personal");
                assert_eq!(available, &vec!["work".to_string()]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn ambiguous_inbound_rejected() {
        let s = stores(
            vec![wa("a", &[]), wa("b", &[])],
            vec![],
            vec![],
        );
        let inp = input("ana", &[], &[(WHATSAPP, &["a", "b"])]);
        let err = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Lenient).unwrap_err();
        assert!(matches!(err[0], BuildError::AmbiguousOutbound { .. }));
    }

    #[test]
    fn single_inbound_infers_outbound() {
        let s = stores(vec![wa("personal", &[])], vec![], vec![]);
        let inp = input("ana", &[], &[(WHATSAPP, &["personal"])]);
        let r = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Strict).unwrap();
        assert_eq!(
            r.resolve("ana", WHATSAPP).unwrap().account_id_raw(),
            "personal"
        );
    }

    #[test]
    fn allow_agents_excludes_agent() {
        let s = stores(vec![wa("work", &["kate"])], vec![], vec![]);
        let inp = input("ana", &[(WHATSAPP, "work")], &[]);
        let err = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Lenient).unwrap_err();
        assert!(matches!(err[0], BuildError::AllowAgentsExcludes { .. }));
    }

    #[test]
    fn asymmetric_binding_warns_in_lenient() {
        let s = stores(vec![wa("a", &[]), wa("b", &[])], vec![], vec![]);
        let inp = input("ana", &[(WHATSAPP, "a")], &[(WHATSAPP, &["b"])]);
        let r = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Lenient).unwrap();
        assert_eq!(r.warnings().len(), 1);
    }

    #[test]
    fn asymmetric_binding_errors_in_strict() {
        let s = stores(vec![wa("a", &[]), wa("b", &[])], vec![], vec![]);
        let inp = input("ana", &[(WHATSAPP, "a")], &[(WHATSAPP, &["b"])]);
        let err = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Strict).unwrap_err();
        assert!(matches!(err[0], BuildError::AsymmetricBinding { .. }));
    }

    #[test]
    fn boot_reports_all_errors_in_one_pass() {
        let s = stores(
            vec![wa("work", &["kate"])],
            vec![], // telegram empty — missing instance
            vec![],
        );
        let inp = input(
            "ana",
            &[(WHATSAPP, "work"), (TELEGRAM, "nope")],
            &[],
        );
        let err = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Lenient).unwrap_err();
        assert_eq!(err.len(), 2, "both errors should surface: {err:#?}");
    }

    #[test]
    fn google_1to1_rule_enforced_via_store_issue() {
        let s = stores(vec![], vec![], vec![ga("ana@x", "ana")]);
        let inp = input("kate", &[(GOOGLE, "ana@x")], &[]);
        let err = AgentCredentialResolver::build(&[inp], &s, StrictLevel::Lenient).unwrap_err();
        // allow_agents for google returns the bound agent, so the
        // mismatch is caught as AllowAgentsExcludes.
        assert!(matches!(err[0], BuildError::AllowAgentsExcludes { .. }));
    }

    #[test]
    fn no_bindings_when_config_empty() {
        let s = stores(vec![], vec![], vec![]);
        let r = AgentCredentialResolver::build(&[], &s, StrictLevel::Strict).unwrap();
        assert!(r.resolve("ana", WHATSAPP).is_err());
    }
}
