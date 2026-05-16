pub mod env;
pub mod migrations;
pub mod secrets_source;
pub mod types;

pub use secrets_source::{InMemorySecretsSource, NoSecretsSource, SecretsSource};

pub use types::*;

use anyhow::{Context, Result};
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct AppConfig {
    pub agents: AgentsConfig,
    pub broker: BrokerConfig,
    pub llm: LlmConfig,
    pub memory: MemoryConfig,
    pub plugins: PluginsConfig,
    /// Operator-configured
    /// persona discovery walk knobs. Loaded from
    /// `personas/discovery.yaml`. Always populated; absent
    /// file → empty default (no scan, no installed personas
    /// surface in admin RPC).
    pub personas: PersonasConfig,
    /// Optional extension system config. `None` when the file is
    /// absent; consumers should fall back to `ExtensionsConfig::default()`.
    pub extensions: Option<ExtensionsConfig>,
    /// Optional MCP runtime config. `None` when `mcp.yaml` is
    /// absent; in that case the MCP subsystem is simply not started.
    pub mcp: Option<McpConfig>,
    /// Optional server-side config (expose this agent as an
    /// MCP server). `None` when `mcp_server.yaml` is absent.
    pub mcp_server: Option<McpServerConfig>,
    /// Runtime-level knobs (hot-reload). Always populated;
    /// an absent `runtime.yaml` yields [`RuntimeConfig::default`].
    pub runtime: RuntimeConfig,
    /// Generic poller subsystem. `None` when
    /// `pollers.yaml` is absent (subsystem off).
    pub pollers: Option<PollersConfig>,
    /// TaskFlow runtime knobs. Always populated; absent file → defaults.
    pub taskflow: TaskflowConfig,
    /// Transcripts subsystem (FTS index + redaction). Always populated;
    /// absent file → defaults (FTS on, redaction off).
    pub transcripts: TranscriptsConfig,
    /// Optional pairing config overrides
    /// (`config/pairing.yaml`). `None` keeps the legacy hardcoded
    /// paths (`<memory_dir>/pairing.db`,
    /// `~/.nexo/secret/pairing.key`); each field overrides
    /// selectively when the file is present.
    pub pairing: Option<PairingInner>,
    /// Optional inbound webhook receiver. `None`
    /// when absent; even when present, the listener only spawns
    /// if `webhook_receiver.enabled == true`.
    pub webhook_receiver: Option<WebhookServerConfig>,
}

/// Minimal config bundle for the `agent mcp-server` subcommand.
///
/// The mcp-server bootstrap only needs identity (pick the first agent) and the
/// server config itself. Skipping `llm.yaml` / `broker.yaml` /
/// `memory.yaml` lets the subcommand run on hosts that aren't configured
/// as a full agent runtime — the operator just wants to expose tools.
#[derive(Debug)]
pub struct McpServerBootConfig {
    pub agents: AgentsConfig,
    pub mcp_server: Option<McpServerConfig>,
}

impl AppConfig {
    /// Backwards-compat entrypoint — `load(dir)` ≡ `load_with_overrides(dir, None)`.
    /// Callers without an override dir (CLI entrypoint without `--override-from`)
    /// route through here unchanged.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_with_overrides(dir, None)
    }

    /// Load with an optional override-dir layer. Each
    /// YAML respects the precedence chain in [`load_with_override`]:
    /// env var > override_dir > base dir > Default. The four
    /// historically-required configs (agents / broker / llm /
    /// memory) ALL participate; optional configs fall through to
    /// the same resolver too so an operator can override
    /// `mcp.yaml` or `transcripts.yaml` from a Kubernetes
    /// ConfigMap without touching the canonical config dir.
    pub fn load_with_overrides(dir: &Path, override_dir: Option<&Path>) -> Result<Self> {
        // Zero-config daemon mode. When the config dir
        // doesn't exist or is missing any of the historically
        // "required" YAMLs, fall through with `Default::default()`
        // for that file and emit a `tracing::warn!` so operators
        // see what's being inferred. Removes the hard requirement
        // for an operator to maintain agents.yaml / broker.yaml /
        // llm.yaml / memory.yaml on disk just to boot the daemon.
        if !dir.exists() && override_dir.map(|o| !o.exists()).unwrap_or(true) {
            tracing::warn!(
                config_dir = %dir.display(),
                "config dir not found — booting with Default::default() for every YAML \
                 (0 agents, BrokerKind::Local, 0 llm providers, sqlite memory at default path)"
            );
            return Ok(AppConfig::default_baked_in());
        }
        maybe_run_yaml_migrations(dir)?;
        let mut agents = load_with_override::<AgentsConfig>(
            dir,
            override_dir,
            "NEXO_AGENTS_YAML",
            "agents.yaml",
        )?;
        merge_agents_drop_in(dir, &mut agents)?;
        resolve_relative_paths(dir, &mut agents);
        let broker = load_with_override::<BrokerConfig>(
            dir,
            override_dir,
            "NEXO_BROKER_YAML",
            "broker.yaml",
        )?;
        let llm = load_with_override::<LlmConfig>(dir, override_dir, "NEXO_LLM_YAML", "llm.yaml")?;
        let memory = load_with_override::<MemoryConfig>(
            dir,
            override_dir,
            "NEXO_MEMORY_YAML",
            "memory.yaml",
        )?;
        let plugins = load_plugins(dir)?;
        let personas = load_personas(dir)?;
        let extensions =
            load_optional::<ExtensionsConfigFile>(dir, "extensions.yaml")?.map(|f| f.extensions);
        let mcp = load_optional::<McpConfigFile>(dir, "mcp.yaml")?.map(|f| f.mcp);
        if let Some(m) = &mcp {
            m.validate().map_err(|e| anyhow::anyhow!(e))?;
        }
        let mcp_server =
            load_optional::<McpServerConfigFile>(dir, "mcp_server.yaml")?.map(|f| f.mcp_server);
        let runtime = load_optional::<RuntimeConfig>(dir, "runtime.yaml")?.unwrap_or_default();
        let pollers = load_optional::<PollersConfigFile>(dir, "pollers.yaml")?.map(|f| f.pollers);
        let taskflow = load_optional::<TaskflowConfig>(dir, "taskflow.yaml")?.unwrap_or_default();
        let transcripts =
            load_optional::<TranscriptsConfig>(dir, "transcripts.yaml")?.unwrap_or_default();
        let pairing = load_optional::<PairingConfig>(dir, "pairing.yaml")?.map(|f| f.pairing);
        let webhook_receiver = load_optional::<WebhookServerConfig>(dir, "webhook_receiver.yaml")?;
        Ok(AppConfig {
            agents,
            broker,
            llm,
            memory,
            plugins,
            personas,
            extensions,
            mcp,
            mcp_server,
            runtime,
            pollers,
            taskflow,
            transcripts,
            pairing,
            webhook_receiver,
        })
    }

    /// Zero-config baked-in defaults. Returned by
    /// [`AppConfig::load`] when the config dir doesn't exist.
    /// Mirrors what `Default::default()` for the top-level
    /// configs produces; every optional + extension subsystem is
    /// off, all "required" types collapse to empty / sane
    /// defaults. Daemon boots, has no agents to spawn, has Local
    /// broker, has no LLM providers (LLM tools fail loudly but
    /// the daemon stays up to serve admin RPCs).
    fn default_baked_in() -> Self {
        AppConfig {
            agents: AgentsConfig::default(),
            broker: BrokerConfig::default(),
            llm: LlmConfig::default(),
            memory: MemoryConfig::default(),
            plugins: PluginsConfig::default(),
            personas: PersonasConfig::default(),
            extensions: None,
            mcp: None,
            mcp_server: None,
            runtime: RuntimeConfig::default(),
            pollers: None,
            taskflow: TaskflowConfig::default(),
            transcripts: TranscriptsConfig::default(),
            pairing: None,
            webhook_receiver: None,
        }
    }

    /// Load only what `agent mcp-server` needs. Tolerant of
    /// missing `llm.yaml` / `broker.yaml` / `memory.yaml` env vars so the
    /// subcommand runs on hosts that don't have a full runtime configured.
    pub fn load_for_mcp_server(dir: &Path) -> Result<McpServerBootConfig> {
        let mut agents = load_required::<AgentsConfig>(dir, "agents.yaml")?;
        merge_agents_drop_in(dir, &mut agents)?;
        resolve_relative_paths(dir, &mut agents);
        let mcp_server =
            load_optional::<McpServerConfigFile>(dir, "mcp_server.yaml")?.map(|f| f.mcp_server);
        Ok(McpServerBootConfig { agents, mcp_server })
    }
}

fn maybe_run_yaml_migrations(dir: &Path) -> Result<()> {
    let runtime = load_optional::<RuntimeConfig>(dir, "runtime.yaml")?.unwrap_or_default();
    let report = migrations::migrate_config_dir(dir, runtime.migrations.auto_apply)?;
    let pending: Vec<_> = report.files.iter().filter(|f| f.changed).collect();
    if pending.is_empty() {
        return Ok(());
    }
    if runtime.migrations.auto_apply {
        eprintln!(
            "[config] auto-applied {} schema migration(s) to config/*.yaml",
            pending.len()
        );
    } else {
        eprintln!(
            "[config] {} config file(s) have pending schema migrations; run `nexo setup migrate --apply` (or set runtime.migrations.auto_apply: true)",
            pending.len()
        );
        for f in pending {
            eprintln!("  - {}: v{} -> v{}", f.file, f.from_version, f.to_version);
        }
    }
    Ok(())
}

/// Merge private agent definitions from `config/agents.d/*.yaml` into the
/// top-level agents list. Use-case: keep `config/agents.yaml` public-safe
/// in version control while stashing per-customer or business-sensitive
/// agents (full prompts, pricing tables, internal contact lists) in a
/// gitignored drop-in directory that the runtime still loads at boot.
///
/// Each drop-in file has the same shape as `agents.yaml`
/// (`agents: [ ... ]`) so operators can move entries freely between the
/// base file and the directory without restructuring.
/// Resolve agent-level filesystem paths (`skills_dir`, `workspace`,
/// `transcripts_dir`, `extra_docs`) against the config directory when
/// they are relative. Makes boot independent of the process cwd — a
/// config loaded from `/etc/agent/` still points `./skills` at
/// `/etc/agent/skills` instead of whatever the shell happened to
/// launch from. Absolute paths and empty strings pass through
/// unchanged.
fn resolve_relative_paths(dir: &Path, agents: &mut AgentsConfig) {
    let resolve = |p: &str| -> String {
        if p.is_empty() {
            return p.to_string();
        }
        let candidate = Path::new(p);
        if candidate.is_absolute() {
            return p.to_string();
        }
        // Skip leading `./` components so the rendered path looks clean
        // (`/etc/agent/skills` instead of `/etc/agent/./skills`) and
        // matches what downstream consumers expect in log messages and
        // assertions.
        let mut joined = dir.to_path_buf();
        for comp in candidate.components() {
            match comp {
                std::path::Component::CurDir => {}
                c => joined.push(c.as_os_str()),
            }
        }
        joined.to_string_lossy().into_owned()
    };
    for a in &mut agents.agents {
        a.skills_dir = resolve(&a.skills_dir);
        a.workspace = resolve(&a.workspace);
        a.transcripts_dir = resolve(&a.transcripts_dir);
        // `extra_docs` are workspace-relative — `workspace::read_opt`
        // joins each entry against the already-resolved workspace
        // root. Prefixing them with the config dir made the loader
        // chase `<config>/<workspace>/<config>/<doc>`, which never
        // exists. Leave them untouched.
    }
}

fn merge_agents_drop_in(dir: &Path, base: &mut AgentsConfig) -> Result<()> {
    let drop_dir = dir.join("agents.d");
    if !drop_dir.exists() {
        return Ok(());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&drop_dir)
        .with_context(|| format!("read_dir {}", drop_dir.display()))?
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
        .collect();
    // Deterministic load order — operators can prefix with `00-`, `10-`, etc.
    files.sort();
    for path in files {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("agents.d entry");
        let resolved = env::resolve_placeholders(&raw, label)?;
        let extra: AgentsConfig = serde_yaml::from_str(&resolved)
            .with_context(|| format!("invalid config in {}", path.display()))?;
        // Drop-in override: an `agents.d/*.yaml` entry with the same
        // `id` as an existing agent REPLACES it. Earlier behavior just
        // appended, leaving two definitions in the loaded config —
        // when the override happened to omit `inbound_bindings`, the
        // truncated copy fell into the runtime's "no bindings →
        // legacy wildcard" branch and silently swallowed every plugin
        // event (e.g. one bot's messages routed to every agent).
        for new_agent in extra.agents {
            match base.agents.iter().position(|a| a.id == new_agent.id) {
                Some(idx) => base.agents[idx] = new_agent,
                None => base.agents.push(new_agent),
            }
        }
    }
    Ok(())
}

/// Load an optional YAML file that historically was
/// required, falling back to `Default::default()` with a WARN log
/// when the file is absent. Same `${ENV_VAR}` placeholder
/// resolution + same `schema_version` stripping as
/// [`load_required`]. The warn is non-fatal: operators that boot
/// with no YAMLs at all are choosing zero-config mode and the
/// daemon's defaults are sensible (empty agents, Local broker,
/// no llm providers, sqlite memory at default path).
pub fn load_optional_or_default<T>(dir: &Path, filename: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    let path = dir.join(filename);
    if !path.exists() {
        tracing::warn!(
            path = %path.display(),
            "config file missing — using Default::default() (zero-config mode)"
        );
        return Ok(T::default());
    }
    load_required::<T>(dir, filename)
}

/// Resolver for the "where does this YAML come from"
/// question. Three sources in priority order:
///
///   1. `NEXO_<NAME>_YAML` env var pointing at an absolute file
///      path (12-factor app style, ideal for Docker / Kubernetes
///      secret mounts where the secret lives outside the
///      canonical config dir).
///   2. `<override_dir>/<filename>` — operator-supplied override
///      that DEEP-MERGES on top of the base config (Kustomize
///      style). Each mapping key wins from the override; nested
///      mappings recurse; scalars / sequences replace wholesale.
///   3. `<base_dir>/<filename>` — the canonical config dir
///      supplied via `--config` (or the XDG default location).
///
/// Returns the resolved value or `T::default()` when none of the
/// three sources turn up a file. Emits ONE tracing line per
/// resolved source so operators can audit which layer won.
pub fn load_with_override<T>(
    base_dir: &Path,
    override_dir: Option<&Path>,
    env_var: &str,
    filename: &str,
) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    // 1) Env var → exact path, wholesale replace.
    if let Ok(env_path) = std::env::var(env_var) {
        if !env_path.is_empty() {
            let path = PathBuf::from(&env_path);
            if !path.exists() {
                anyhow::bail!(
                    "{env_var} = {env_path} but file does not exist; \
                     unset the env or fix the path"
                );
            }
            tracing::info!(
                source = "env_var",
                env_var,
                path = %path.display(),
                "config loaded from env-var override"
            );
            return load_yaml_file::<T>(&path, filename);
        }
    }

    // 2) Override-dir merge layer (when provided).
    let base_path = base_dir.join(filename);
    let override_path = override_dir.map(|d| d.join(filename));
    let has_base = base_path.exists();
    let has_override = override_path.as_ref().map(|p| p.exists()).unwrap_or(false);

    match (has_base, has_override) {
        (false, false) => {
            tracing::warn!(
                path = %base_path.display(),
                "config file missing — using Default::default() (zero-config mode)"
            );
            Ok(T::default())
        }
        (true, false) => {
            tracing::debug!(
                source = "base",
                path = %base_path.display(),
                "config loaded from base config dir"
            );
            load_yaml_file::<T>(&base_path, filename)
        }
        (false, true) => {
            let path = override_path.unwrap();
            tracing::info!(
                source = "override_dir",
                path = %path.display(),
                "config loaded from override dir only (no base present)"
            );
            load_yaml_file::<T>(&path, filename)
        }
        (true, true) => {
            // Deep-merge override on top of base. Operator
            // overrides individual keys without forking the whole
            // file.
            let override_path = override_path.unwrap();
            tracing::info!(
                source = "merge",
                base = %base_path.display(),
                override_ = %override_path.display(),
                "config loaded with override-dir deep-merge on top of base"
            );
            let base_str = read_yaml_resolved(&base_path, filename)?;
            let override_str = read_yaml_resolved(&override_path, filename)?;
            let mut base_v: serde_yaml::Value = serde_yaml::from_str(&base_str)
                .with_context(|| format!("invalid base config in {}", base_path.display()))?;
            let override_v: serde_yaml::Value =
                serde_yaml::from_str(&override_str).with_context(|| {
                    format!("invalid override config in {}", override_path.display())
                })?;
            yaml_deep_merge(&mut base_v, override_v);
            if let Some(map) = base_v.as_mapping_mut() {
                map.remove(serde_yaml::Value::String("schema_version".to_string()));
            }
            serde_yaml::from_value(base_v).with_context(|| {
                format!(
                    "merged config (base={}, override={}) failed to deserialize as {}",
                    base_path.display(),
                    override_path.display(),
                    std::any::type_name::<T>()
                )
            })
        }
    }
}

/// Recursively merge `over` on top of `base`. Mappings
/// merge per-key (override wins ties; nested mappings recurse).
/// Scalars + sequences replace wholesale.
fn yaml_deep_merge(base: &mut serde_yaml::Value, over: serde_yaml::Value) {
    match (base, over) {
        (serde_yaml::Value::Mapping(b), serde_yaml::Value::Mapping(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => yaml_deep_merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (base_slot, over_val) => {
            *base_slot = over_val;
        }
    }
}

/// Shared "read file + resolve env placeholders" path
/// for both base and override loads. Mirrors what `load_required`
/// does inline, factored out so [`load_with_override`] can call
/// it twice (once per layer) without re-implementing.
fn read_yaml_resolved(path: &Path, label: &str) -> Result<String> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    env::resolve_placeholders(&raw, label)
}

/// Deserialize a single YAML file with the same
/// schema-version stripping + placeholder resolution as
/// [`load_required`]. Used by [`load_with_override`] when no
/// merge applies (single-source paths).
fn load_yaml_file<T: serde::de::DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    let resolved = read_yaml_resolved(path, label)?;
    deserialize_yaml_with_schema_version(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))
}

/// Load a required YAML file, resolving `${ENV_VAR}` placeholders.
pub fn load_required<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Result<T> {
    let path = dir.join(filename);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = env::resolve_placeholders(&raw, filename)?;
    deserialize_yaml_with_schema_version(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))
}

/// Load an optional YAML file, resolving `${ENV_VAR}` placeholders. Returns
/// `Ok(None)` when the file is absent.
pub fn load_optional<T: serde::de::DeserializeOwned>(
    dir: &Path,
    filename: &str,
) -> Result<Option<T>> {
    let path = dir.join(filename);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let resolved = env::resolve_placeholders(&raw, filename)?;
    let value = deserialize_yaml_with_schema_version(&resolved)
        .with_context(|| format!("invalid config in {}", path.display()))?;
    Ok(Some(value))
}

fn deserialize_yaml_with_schema_version<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    let mut v: Value = serde_yaml::from_str(raw)?;
    if let Some(map) = v.as_mapping_mut() {
        map.remove(Value::String("schema_version".to_string()));
    }
    Ok(serde_yaml::from_value(v)?)
}

/// Load personas/discovery.yaml.
/// Missing file → defaults to a single search path at
/// `$HOME/.nexo/state/personas` so `nexo persona install`
/// results are picked up without manual YAML editing. Restored
/// 2026-05-14 — a prior simplification dropped this fallback,
/// silently breaking persona-installed agents in zero-config
/// dev setups.
fn load_personas(dir: &Path) -> Result<PersonasConfig> {
    let personas_dir = dir.join("personas");
    let mut discovery =
        load_optional::<PersonaDiscoveryConfigFile>(&personas_dir, "discovery.yaml")?
            .map(|f| f.discovery)
            .unwrap_or_default();
    if discovery.search_paths.is_empty() {
        if let Ok(home) = std::env::var("HOME") {
            discovery
                .search_paths
                .push(PathBuf::from(home).join(".nexo/state/personas"));
        }
    }
    Ok(PersonasConfig { discovery })
}

fn load_plugins(dir: &Path) -> Result<PluginsConfig> {
    let plugins_dir = dir.join("plugins");
    // Wave 7 — whatsapp loads via opaque `entries["whatsapp"]` map only.
    // Wave 6 — telegram loads via opaque `entries["telegram"]` map only.
    // Wave 5 — email loads via opaque `entries["email"]` map only.
    // No typed `email` field in PluginsConfig anymore.
    let browser =
        load_optional::<BrowserConfigFile>(&plugins_dir, "browser.yaml")?.map(|f| f.browser);
    // Optional discovery knobs at plugins/discovery.yaml.
    // Missing file => default (empty search_paths => nothing scanned).
    let discovery = load_optional::<PluginDiscoveryConfigFile>(&plugins_dir, "discovery.yaml")?
        .map(|f| f.discovery)
        .unwrap_or_default();
    // Phase 93.3 — opaque per-plugin entries map. Same files;
    // additive view keyed by plugin id.
    let entries = load_plugin_entries(&plugins_dir);
    Ok(PluginsConfig {
        entries,
        browser,
        discovery,
    })
}

/// Phase 93.3 — walk `<plugins_dir>/*.yaml` and produce an opaque
/// `plugin_id → serde_yaml::Value` map. Outer wrapper key is
/// stripped when the single top-level key matches the filename
/// stem (so `whatsapp.yaml` containing `{whatsapp: [...]}` yields
/// `entries["whatsapp"] = [...]`). `discovery.yaml` is filtered
/// (framework-internal — typed loader handles it). Per-file parse
/// failures emit `tracing::warn!` on the `plugins.config` target
/// and are skipped; a single bad file doesn't abort the whole
/// config load.
///
/// `.yml` extension is ignored (parity with the typed loaders).
/// Returns the empty map when `plugins_dir` doesn't exist.
pub fn load_plugin_entries(plugins_dir: &Path) -> BTreeMap<String, serde_yaml::Value> {
    let mut out: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    let read = match std::fs::read_dir(plugins_dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let ext_yaml = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("yaml"))
            .unwrap_or(false);
        if !ext_yaml {
            continue;
        }
        if stem == "discovery" {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    target: "plugins.config",
                    file = %path.display(),
                    error = %e,
                    "skipping unreadable plugins/*.yaml",
                );
                continue;
            }
        };
        let parsed: serde_yaml::Value = match serde_yaml::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "plugins.config",
                    file = %path.display(),
                    error = %e,
                    "skipping unparseable plugins/*.yaml",
                );
                continue;
            }
        };
        // Strip outer wrapper key only when (a) value is a Mapping,
        // (b) Mapping has exactly one entry, (c) key matches stem.
        let value = match parsed {
            serde_yaml::Value::Mapping(ref m) if m.len() == 1 => {
                let key = serde_yaml::Value::String(stem.to_string());
                match m.get(&key) {
                    Some(inner) => inner.clone(),
                    None => parsed.clone(),
                }
            }
            other => other,
        };
        out.insert(stem.to_string(), value);
    }
    out
}

#[cfg(test)]
mod phase94_tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    fn get_str<'a>(v: &'a serde_yaml::Value, path: &[&str]) -> Option<&'a str> {
        let mut cur = v;
        for k in path {
            cur = cur.get(*k)?;
        }
        cur.as_str()
    }

    fn get_bool(v: &serde_yaml::Value, path: &[&str]) -> Option<bool> {
        let mut cur = v;
        for k in path {
            cur = cur.get(*k)?;
        }
        cur.as_bool()
    }

    #[test]
    fn yaml_deep_merge_overrides_scalar_in_nested_mapping() {
        let mut base: serde_yaml::Value = serde_yaml::from_str(
            "broker:\n  type: nats\n  url: nats://base:4222\n  persistence:\n    enabled: true\n",
        )
        .unwrap();
        let over: serde_yaml::Value = serde_yaml::from_str(
            "broker:\n  url: nats://override:4222\n  persistence:\n    enabled: false\n",
        )
        .unwrap();
        yaml_deep_merge(&mut base, over);
        // Override-only keys win.
        assert_eq!(
            get_str(&base, &["broker", "url"]),
            Some("nats://override:4222")
        );
        assert_eq!(
            get_bool(&base, &["broker", "persistence", "enabled"]),
            Some(false)
        );
        // Base-only key survives.
        assert_eq!(get_str(&base, &["broker", "type"]), Some("nats"));
    }

    #[test]
    fn yaml_deep_merge_replaces_sequence_wholesale() {
        let mut base: serde_yaml::Value = serde_yaml::from_str("items:\n- a\n- b\n- c\n").unwrap();
        let over: serde_yaml::Value = serde_yaml::from_str("items:\n- x\n").unwrap();
        yaml_deep_merge(&mut base, over);
        let items = base.get("items").unwrap().as_sequence().unwrap();
        assert_eq!(items.len(), 1, "sequences replace, not concatenate");
        assert_eq!(items[0].as_str(), Some("x"));
    }

    #[test]
    fn env_var_wholesale_replaces_base() {
        let base = TempDir::new().unwrap();
        write(
            base.path(),
            "broker.yaml",
            "broker:\n  type: nats\n  url: nats://base\n",
        );
        let other = TempDir::new().unwrap();
        write(
            other.path(),
            "broker.yaml",
            "broker:\n  type: local\n  url: \"\"\n",
        );
        // SAFETY: tests serialised by env-var contention via the
        // ENV_TEST_LOCK below. Setting unique-named vars keeps
        // unrelated tests unaffected.
        let _lock = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(
            "NEXO_TEST_BROKER_YAML_X",
            other.path().join("broker.yaml").to_str().unwrap(),
        );
        let cfg: BrokerConfig =
            load_with_override(base.path(), None, "NEXO_TEST_BROKER_YAML_X", "broker.yaml")
                .unwrap();
        std::env::remove_var("NEXO_TEST_BROKER_YAML_X");
        assert_eq!(cfg.broker.kind, crate::types::broker::BrokerKind::Local);
    }

    #[test]
    fn override_dir_deep_merges_on_base() {
        let base = TempDir::new().unwrap();
        write(
            base.path(),
            "broker.yaml",
            "broker:\n  type: nats\n  url: nats://base\n  persistence:\n    enabled: true\n",
        );
        let overlay = TempDir::new().unwrap();
        write(
            overlay.path(),
            "broker.yaml",
            "broker:\n  url: nats://override\n",
        );
        let cfg: BrokerConfig = load_with_override(
            base.path(),
            Some(overlay.path()),
            "NEXO_TEST_NONEXISTENT_ENV_FOR_MERGE",
            "broker.yaml",
        )
        .unwrap();
        assert_eq!(cfg.broker.kind, crate::types::broker::BrokerKind::Nats);
        assert_eq!(cfg.broker.url, "nats://override");
        // Persistence stayed from base (override didn't touch it).
        assert!(cfg.broker.persistence.enabled);
    }

    #[test]
    fn missing_everywhere_returns_default() {
        let base = TempDir::new().unwrap();
        let cfg: BrokerConfig = load_with_override(
            base.path(),
            None,
            "NEXO_TEST_NONEXISTENT_ENV_FOR_DEFAULT",
            "broker.yaml",
        )
        .unwrap();
        // Default = Local.
        assert_eq!(cfg.broker.kind, crate::types::broker::BrokerKind::Local);
    }

    #[test]
    fn env_var_pointing_at_missing_file_errors() {
        let _lock = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(
            "NEXO_TEST_BROKER_YAML_BAD",
            "/this/path/does/not/exist/broker.yaml",
        );
        let result: anyhow::Result<BrokerConfig> = load_with_override(
            Path::new("/tmp/does-not-matter"),
            None,
            "NEXO_TEST_BROKER_YAML_BAD",
            "broker.yaml",
        );
        std::env::remove_var("NEXO_TEST_BROKER_YAML_BAD");
        assert!(result.is_err(), "missing env-pointed file must fail");
        assert!(format!("{:?}", result.unwrap_err()).contains("file does not exist"));
    }

    // Env-var tests serialise so they don't observe each other's
    // sets / unsets. Cargo runs unit tests in parallel by default.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
