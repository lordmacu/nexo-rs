# Plugin admin UI contribution model

*Phase 99.* How an out-of-tree plugin contributes config screens +
menus to the admin web app without forking it. Section reference:
[Manifest `[plugin.admin_ui]`](../plugins/manifest-admin-ui.md).

## Problem

Before Phase 99, any plugin-specific admin config (Google OAuth,
channel settings, allowlists) required editing YAML by hand or
forking `nexo-rs-plugin-admin` + rebuilding — breaking the Phase 90
axiom "admin out-of-tree, plugin-agnostic" and blocking the Phase 98
marketplace.

## Design — hybrid descriptor

The descriptor is split between a **cold contract** and a **live
descriptor**:

- **Cold:** the `[plugin.admin_ui]` manifest section (validated TOML)
  declares the slots a plugin claims, its screens, and the render
  mode. This drives discovery + trust gating WITHOUT executing the
  plugin.
- **Live:** the admin RPC `nexo/admin/plugin_ui/describe` returns the
  current fields / values / options. The daemon **synthesises** this
  for static plugins (`describe = false`) from the manifest +
  `cfg.plugins.<id>`; it **forwards** to the plugin (over the
  `[plugin.admin]` broker channel) for dynamic ones
  (`describe = true`). The frontend consumes a uniform
  `ScreenDescriptor` either way.

```
admin SPA ──nexo/admin/plugin_ui/list────────▶ daemon (aggregate manifests,
          ◀──menu/submenu structure──────────  slot+trust+visible_when gate)
          ──nexo/admin/plugin_ui/describe────▶ daemon ──(static) synth from cfg
          ◀──ScreenDescriptor─────────────────        └─(dynamic) forward→plugin
          ──nexo/admin/plugin_ui/config_set──▶ daemon (validate merged vs
          ◀──ConfigSetResponse────────────────  config_schema → secrets to
                                                 cred store → write → reload)
```

## Daemon side (`nexo-core`)

- `crates/plugin-manifest/src/admin_ui.rs` — the section + validators
  (reuses `config_schema.rs`; menu/submenu via `Contribution.parent`).
- `crates/plugin-manifest/src/visible_when.rs` — the mini-DSL.
- `crates/core/src/admin_ui/slot_registry.rs` — slot vocabulary +
  trust matrix (`TrustTier` from Phase 98 provenance).
- `crates/core/src/agent/admin_rpc/domains/plugin_ui.rs` — the
  `list` / `describe` / `config_set` domain. Secrets route to the
  Phase 93 credential store; `config_set` validates the **merged**
  config (partial edits keep `required` fields) + fires the Phase 97
  reload signal.
- `AgentEventKind::PluginUiChanged` (firehose) — the admin re-fetches
  on install / uninstall / config change.

## Frontend side (`nexo-rs-plugin-admin`)

Plugins surface under **Settings › Plugins** (`/m/settings/plugins`),
not as top-level rail entries. `usePluginContributions` fetches the
list + subscribes the firehose; `GenericScreen` renders the
descriptor (10 field renderers + actions + a `visible_when` port);
secret fields are write-only.

## Modes

- **Mode A (declarative)** — shipped. Covers the vast majority:
  config forms, dynamic dropdowns, actions.
- **Mode B (embedded iframe + ESM bundle)** — deferred to v2 (see
  `FOLLOWUPS.md`). `mode = "embedded"` is rejected by the v1
  validator with a pointer to the follow-up.

## Adoption requires a manifest-crate publish

`PluginSection` is `#[serde(deny_unknown_fields)]`, so a plugin
pinning an older `nexo-plugin-manifest` from crates.io cannot even
parse a `[plugin.admin_ui]` section. Adopting the model is therefore
gated on publishing the manifest crate (≥ 0.1.8) and bumping each
plugin's dependency. The six canonical plugins (google, telegram,
email, whatsapp, browser, web-search) adopted it once 0.1.8 shipped.
