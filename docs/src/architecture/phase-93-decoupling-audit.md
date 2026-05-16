# Phase 93.11 — Compile-Time Plugin Decoupling Audit

Status: **closed 2026-05-16**. All 47 anchor sites cleared.

## Result

`cargo tree -i nexo-plugin-{whatsapp,telegram,email,browser}`
returns "did not match any packages" against either the
default daemon build or `--no-default-features`. The daemon
binary compiles with zero direct or transitive dependency on
any canonical channel-plugin crate; pairing, outbound
dispatch, HTTP routes, admin RPC, metrics scrape, dashboard
sources, and pairing triggers all flow through
manifest-declared broker contracts:

- `[plugin.pairing.adapter]` (Phase 81.33.b.real Stage 1)
- `[plugin.http]` (Stage 2)
- `[plugin.admin]` (Stage 4)
- `[plugin.metrics]` (Stage 5)
- `[plugin.dashboard]` (Stage 6)
- `[plugin.pairing.trigger]` (Phase 81.20.x Stage 7 Phase 2)
- `[plugin.public_tunnel]` (Phase 81.20.x Stage 7 Phase 2)

Operators wanting an embedded in-process build link the
canonical plugin crate directly from a custom binary — the
daemon's published default ships with zero hardcoded plugin
imports.

## Historical TL;DR (pre-close)

Daemon binary (`nexo-daemon`) cannot build without the four
canonical plugin crates as Cargo dependencies. Phase 93's
opaque-config + `PluginsConfig.entries` work eliminated
runtime YAML coupling but did **not** touch the compile-time
import graph. **47 distinct import sites** (43 in code, 4 in
Cargo) anchor the daemon to `nexo-plugin-whatsapp`,
`nexo-plugin-telegram`, `nexo-plugin-email`,
`nexo-plugin-browser`.

**Recommendation (closed via Phase 81.20.x Stage 7 Phase 2):**
Hybrid path adopted. Cargo feature-gates landed first
(`whatsapp`/`telegram`/`browser` ~Stage 7 Phase 1) and then
the gates themselves were deleted once each plugin shipped
its manifest sections — `cargo tree` returns unmatched for
all four canonical plugins as of 2026-05-16.

## Inventory: 47 sites

### Cargo dependency anchors (4)

| File | Dep |
|---|---|
| `Cargo.toml:376` | `nexo-plugin-whatsapp = "0.1.3"` (root workspace dep) |
| `Cargo.toml:377` | `nexo-plugin-email = "0.1.3"` (root workspace dep) |
| `Cargo.toml:410` | `nexo-plugin-whatsapp = { workspace = true }` (daemon bin) |
| `Cargo.toml:415` | `nexo-plugin-telegram = "0.1.1"` (daemon bin) |
| `Cargo.toml:416` | `nexo-plugin-email = { workspace = true }` (daemon bin) |
| `crates/setup/Cargo.toml:25` | `nexo-plugin-email = { workspace = true }` |
| `crates/setup/Cargo.toml:69` | `nexo-plugin-whatsapp = { workspace = true }` |

`nexo-plugin-browser` is NOT a Cargo dep — only env-var
seeding via copy-pasted `env_config` reference (no compile-
time coupling). Already decoupled in practice.

### Code import sites (43) — by classification bucket

**Bucket A — dies after 81.32.c7.c (full-parity tools registry extract).**
The `register_<channel>_tools(&tools)` blocks at agent boot
+ hot-spawn path. Plugins already expose
`NexoPlugin::register_outbound_tools(&self, &ToolRegistry)`
(trait method exists). Daemon currently hardcodes per-plugin
calls as fallback. Migration: drop the hardcoded calls;
loop over `plugin_handles` and dispatch `register_outbound_tools`.

```
src/main.rs:5128  register_whatsapp_tools     (boot path)
src/main.rs:5132  register_telegram_tools     (boot path)
src/main.rs:5145  filter_from_allowed_patterns
src/main.rs:5146  register_email_tools_filtered
src/main.rs:5150  EMAIL_TOOL_NAMES.len()
src/main.rs:6917  register_whatsapp_tools     (hot-spawn path)
src/main.rs:6920  register_telegram_tools     (hot-spawn path)
src/main.rs:7025  filter_from_allowed_patterns
src/main.rs:7026  register_email_tools_filtered
```

9 sites. **Already tracked by Phase 81.32.c7.c**, not new
debt. Effort: ~4h.

**Bucket B — dies after 81.33.b.real (manifest-driven pairing-adapter).**
Daemon hardcodes `XxxPairingAdapter::new(broker)` because
`SubprocessNexoPlugin::build_pairing_adapter()` still
defaults to `None` — manifest schema for
`[plugin.pairing.adapter]` not yet finalised (see
`crates/core/src/agent/plugin_host.rs:113-119`).

```
src/main.rs:1654  WhatsappPairingAdapter::new(broker)
src/main.rs:1657  TelegramPairingAdapter::new(broker)
```

2 sites. **New follow-up surfaced**: Phase 81.33.b.real
manifest schema + `GenericBrokerPairingAdapter`.
Effort: ~5h.

**Bucket C — dies after 93.5.d (WhatsApp orchestration generalisation).**
Daemon-owned pairing state map + tunnel config + HTTP UI
dispatcher.

```
src/main.rs:710     SharedPairingState (fn sig)
src/main.rs:1038    SharedPairingState (fn sig)
src/main.rs:1126    QrSnapshot
src/main.rs:2492    pairing_trigger::CHANNEL_ID
src/main.rs:2494    WhatsappPairingTrigger::from_configs
src/main.rs:3226    SharedPairingState
src/main.rs:3244    PairingState::new
src/main.rs:15167   dispatch_route + WhatsappRoute
src/main.rs:15174   PAIR_PAGE_HTML
crates/setup/src/admin_adapters.rs:3728   dispatch::TOPIC_OUTBOUND
crates/setup/src/admin_bootstrap.rs:713   WhatsappBotHandle
crates/setup/src/writer.rs:789            session::pair_once
```

12 sites. **Already tracked as 93.5.d DEFERRED-strict**.
Trigger: 2nd pairing-based channel with daemon-owned
tunnel + admin RPC. Effort when triggered: ~5h.

**Bucket D — email in-process integration (autonomous_worker + tool ctx + metrics + wizard validators).**
Email is the only canonical plugin that has NOT undergone
subprocess flip. Daemon holds `Arc<EmailPlugin>` to expose:
- `dispatcher_handle()` for outbound tool routing
- `bounce_store_handle()` for delivery receipts
- `attachments_dir()` for MCP server attachment paths
- `health_map()` for `/metrics` Prometheus rendering
- `WorkerState` enum for `/health` HTML rendering
- MCP autonomous_worker mode embeds `EmailToolContext`
  in-process to share `Arc<HealthMap>`

```
src/main.rs:715,3289,3299                EmailPlugin construction (boot)
src/main.rs:3698,3766,3767               EmailToolContext (boot)
src/main.rs:14129,14153,14154,14174      EmailPlugin (autonomous_worker)
src/main.rs:15117,15273,15286-15289      metrics + render_email_health + WorkerState
crates/setup/src/services/email.rs:28-31 ImapConnection / provider_hint /
                                         SmtpClient / spf_dkim (wizard validators)
```

15 sites. Email is **structurally in-process by design**.
Subprocess-flip would require a Phase 81.20.x plan with:
- Broker RPC for `dispatcher_handle` outbound (lat impact)
- Cross-process `Arc<HealthMap>` sync (state-replication design)
- Subprocess-side MCP server merged into autonomous_worker
  mode (or autonomous_worker stays in-tree as "core service")
- Setup wizard validators (ImapConnection / SmtpClient probe)
  invocable via wizard-only crate or broker RPC

Estimated effort: 20-30h. **No active trigger today** —
email works in-process, autonomous_worker depends on it.

### Imports already dead-after-something (zero today, classify-only)

None. Every import site has a live consumer. Cat C audit
93.5.c (2026-05-15) confirmed zero zombies in the related
typed-config sites; same conclusion holds for plugin imports.

## Decision matrix

### Option 1 — Cargo feature-gates

```toml
[features]
default = ["plugin-whatsapp", "plugin-telegram", "plugin-email", "plugin-browser"]
plugin-whatsapp = ["dep:nexo-plugin-whatsapp"]
plugin-telegram = ["dep:nexo-plugin-telegram"]
plugin-email    = ["dep:nexo-plugin-email"]
plugin-browser  = [] # already no Cargo dep; flag gates env seeding code
```

Each import site wrapped with `#[cfg(feature = "plugin-X")]`.
Build slim binary with `cargo build --no-default-features
--features plugin-whatsapp`.

**Pros.**
- Cheap: ~6h for the three subprocess-flipped plugins
  (whatsapp + telegram + browser). 9-12 `cfg` wrap sites
  (the bucket-A code is already feature-shaped by the
  if-plugin-present check).
- Compile-time enforcement: `cargo build --no-default-features
  --features plugin-whatsapp` proves "daemon compiles without
  telegram crate".
- Unlocks Android/embedded slim builds today
  (whatsapp-only daemon is real demand per
  `project_android_flutter_target` memory).
- Composable with subprocess-flip: a feature-disabled plugin
  can still run as a discovered subprocess if its manifest
  is in `search_paths/`. The daemon never imports the crate;
  the subprocess does.

**Cons.**
- Doesn't help 3rd-party plugins (still need to be Cargo
  deps of daemon or run as subprocesses).
- Doesn't decouple email (in-process by design).
- Adds `#[cfg]` noise in main.rs (~9-12 blocks).
- Workspace default-features quirks already biting
  (per `feedback_rustls_default_features_off`); each new
  feature needs `default-features = false` discipline.

**Effort.** ~6h for whatsapp + telegram + browser gates +
test matrix (`build --no-default-features` per single-channel
combo). Email NOT gated (keeps current shape).

### Option 2 — Full dynamic-loading via manifest + broker

Daemon drops all `nexo_plugin_X::` imports. Every integration
becomes:
- Pairing: `NexoPlugin::build_pairing_adapter()` →
  `GenericBrokerPairingAdapter` (81.33.b.real manifest).
- Tools: `NexoPlugin::register_outbound_tools(&tool_registry)`
  (already trait, just drop hardcoded fallbacks — 81.32.c7.c).
- Pairing trigger map: `NexoPlugin::pairing_trigger()` →
  `dyn PairingTrigger` (new opt-in trait method).
- Tunnel + session_dir: `NexoPlugin::orchestration_descriptor()`
  → typed `OrchestrationRequirements` (new — covers tunnel
  port, pairing-state HTTP route, instance loop driver).
- Email in-process: broker RPC OR keep gated in-tree.

**Pros.**
- True self-describing: drop plugin in `search_paths/`, it
  works without daemon recompile.
- Android-friendly (no compile-time plugin baggage on lib
  target).
- Eliminates the 30+ import-site debt entirely.
- Forces clean trait surfaces (good architectural pressure).

**Cons.**
- Cost: ~30-50h across multiple sub-phases (manifest schema
  design, GenericBrokerPairingAdapter, OrchestrationDescriptor,
  email broker-RPC bridge OR feature-gate, wizard validator
  extraction).
- Speculative trait shapes without driver: 93.5.d locked
  DEFERRED-strict for exactly this reason. 93.10
  channels_dashboard same shape.
- Email subprocess-flip is 20-30h on its own with real
  latency tradeoffs (in-process `Arc<HealthMap>` → broker
  cross-process sync).
- Migration risk: each trait method that mis-fits the
  real-world 2nd-channel driver = churn.

**Effort.** 30-50h. No discrete trigger today (no 3rd-party
plugin, no email subprocess driver).

### Option 3 — Hybrid (recommended)

1. **Ship feature-gates now** (~6h) for whatsapp + telegram +
   browser. Email stays default-on. This delivers the
   Android/embedded slim-daemon win NOW.
2. **Defer dynamic-loading trait surface** until trigger:
   3rd-party plugin demand OR email subprocess driver lands.
3. **Land 81.32.c7.c** (tool registry full-parity extract,
   already tracked, ~4h) — kills 9 bucket-A imports
   regardless of which long-term path wins.
4. **Land 81.33.b.real** (manifest-driven pairing adapter
   schema, already implied by L113-119 comment in
   `plugin_host.rs`, ~5h) — kills 2 bucket-B imports.

**Combined immediate work** ~15h, drops ~11 of 43 code
imports + adds compile-time enforcement that
`whatsapp|telegram|browser` are optional. Leaves the
email-in-process bucket (15 sites) intact as documented
design choice, not silent debt.

## Recommendation

**Ship Option 3 Hybrid.** Phase 93 closes with:
- 93.5.d DEFERRED-strict (already done)
- 93.10 DEFERRED until non-canon dashboard driver (already done)
- 93.11 = this memo (concrete data + decision)
- 93.12 (new) = ship feature-gates per above
- Bucket-B follow-up = 81.33.b.real (existing implied)

Phase 93 then status = **shipped + audit-complete**. Email
in-process is acknowledged design (not bug). Daemon can ship
slim builds for embedded targets immediately. Dynamic-loading
becomes a Phase 100+ candidate triggered by real 3rd-party
demand, not speculative.

## Trigger watchlist (re-open this audit if)

- 3rd-party plugin (slack/discord/sms) requests to ship as
  pure subprocess without daemon recompile.
- Android Flutter integration finalised + daemon binary
  size becomes a release-blocker.
- Email subprocess-flip plan opens (would invalidate the
  "email stays in-process" recommendation).
- 2nd pairing-based channel lands and 93.5.d unblocks —
  forces revisit of trait shape consensus.
