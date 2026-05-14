# Backlog Phases

Phases reserved + scoped but **not yet active**. Each phase here
has a brainstorm doc on disk; spec + plan land when the
sequencing trigger documented per phase fires.

Active phases live in [`CLAUDE.md`](CLAUDE.md) (top-of-file
table) + [`PHASES.md`](PHASES.md) (per-phase prose for rows
still 🔄). Shipped phases live in
[`PHASES-archive.md`](PHASES-archive.md).

---

## Phase 93 — Plugin Opaque Config + Generic Credential Store

**Status:** 📋 backlog
**Brainstorm:** [`design-phase-93-plugin-opaque-config.md`](design-phase-93-plugin-opaque-config.md)
**Sub-phases:** 9 (93.1 — 93.9)
**Estimated effort:** ~25 sessions across 3-5 weeks calendar
**Plugin patch publishes required:** 5
  (whatsapp / telegram / email / browser / google)

### Sequencing trigger

Invoke `/forge spec phase-93.1` only when ONE of the following
surfaces:

1. **Imminent batch of 3+ new plugins** (slack, discord, sms,
   instagram, …) — each would otherwise require editing
   `crates/config::PluginsConfig` + adding a new
   `crates/auth::CredentialStores` field + handle constant.
   The marginal cost per plugin compounds; >2 plugins justify
   the refactor.
2. **Blocked community-tier plugin request** — an external
   author wants to ship a plugin that needs daemon-side
   support but the typed coupling rejects it.
3. **Microapp roadmap dependency** — agent-creator-microapp or
   downstream products need plugin discovery / generic config
   that this refactor unlocks.

Without one of these triggers, ship the helper-consolidation
wins (Phase 81.33.b, .e already done) and defer the full
refactor.

### Why this phase exists

Phase 81.33 work surfaced three concrete blockers:

- **81.33.a step 5** — drop daemon-side
  `register_telegram_tools` / `register_whatsapp_tools` library
  calls; replace with manifest-driven `GenericRpcToolHandler`.
  Blocked because `publish_outbound` carries credential +
  breaker + audit + per-account-topic logic that requires
  generic credential store access.
- **81.33.b.real** — manifest-driven `GenericBrokerPairingAdapter`
  replacing the typed `XxxPairingAdapter::new(broker)` library
  call. Blocked by the same credential resolution coupling.
- **81.33.c** — drop daemon `Cargo.toml` deps on
  `nexo-plugin-{whatsapp,telegram,email}`. Blocked because each
  is still imported as a library for the calls above.

All three unblock once Phase 93 lands.

### Sub-phase breakdown

| Sub | Scope | Effort | Blocks |
|-----|-------|--------|--------|
| **93.1** | `[plugin.config_schema]` manifest section + parser + validator (parse JSON object, root type `"object"`) | 1 session | 93.2 |
| **93.2** | `NexoPlugin::configure(&Value)` trait method (default no-op) + `SubprocessNexoPlugin::configure` JSON-RPC `plugin.configure` dispatch | 1 session | 93.3 |
| **93.3** | `PluginsConfig.entries: BTreeMap<String, serde_yaml::Value>` parallel field. Daemon parses BOTH old typed fields AND new map; prefers `entries` when present; warns on typed | 1 session | 93.4 |
| **93.4** | Per-plugin opaque config migration. Each plugin's subprocess `plugin.configure` handler deserialises the value against its own schema. 5 plugins × 2-3 sessions each = 10-15 sessions | 10-15 sessions | 93.5 |
| **93.5** | Drop typed `cfg.plugins.X` fields. End deprecation window after 2-3 release cycles. Operators with stale YAML see a one-line migration command | 1 session | 93.6 |
| **93.6** | `GenericCredentialStore` trait + `CredentialBytes` type-erased value. Plugin-side helper `CredentialBytes::deserialize::<T>()` for typed reinterpretation | 1 session | 93.7 |
| **93.7** | `bundle.stores: DashMap<String, Arc<dyn GenericCredentialStore>>` parallel field. Coexists with typed `bundle.stores.whatsapp` etc. during migration | 1 session | 93.8 |
| **93.8** | Per-plugin store migration. Each plugin's `NexoPlugin::init` registers its store via `bundle.stores.insert(plugin_id, store)`. 4 plugins × 1-2 sessions each = 4-8 sessions | 4-8 sessions | 93.9 |
| **93.9** | Drop typed store fields + `nexo_auth::handle::*` constants + per-handle audit/telemetry switch. Resolver keyed by plugin_id string. Unblocks Phase 81.33.a step 5 | 1 session | (Phase 81.33.a step 5 unblock) |

### Risks captured

See [`design-phase-93-plugin-opaque-config.md`](design-phase-93-plugin-opaque-config.md) §Risks for details:

- Backward-compat YAML during deprecation window
- JSON schema in TOML string loses compile-time guarantees
- Mass refactor of `bundle.stores.X` call sites in plugin crates
- Type-erased credential resolver — plugin-side deserialize helper required
- 25-session pure refactor with zero new feature — high opportunity cost

### Plugin author heads-up cycle

Out-of-tree plugin maintainers
(`nexo-rs-plugin-whatsapp/telegram/email/browser/google`)
receive a heads-up issue **1 release cycle BEFORE 93.4 starts**
so they can spec their plugin-side migration in parallel.
Without this lead time, 93.4 + 93.8 become serial daemon-then-plugin
work and the calendar doubles.

### Resume prompt

> Phase 93.1 — define the `[plugin.config_schema]` manifest
> section. Sub-phase scope: schema field on
> `crates/plugin-manifest::PluginSection`, parser test
> covering a multi-instance plugin (telegram example with two
> `instance` entries), validator that the schema string is a
> valid JSON object with `"type":"object"` at root (mirror of
> Phase 81.33.a's `OutboundToolSpec::input_schema` validator),
> migration note on coexistence with typed `cfg.plugins.X`
> fields during the deprecation window. Run `/forge spec
> phase-93.1` to formalise.
