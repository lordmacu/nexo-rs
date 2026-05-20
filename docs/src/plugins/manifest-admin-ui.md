# Manifest `[plugin.admin_ui]`

*Phase 99 — admin UI contribution model (Mode A, declarative).*

A plugin declares admin-UI **contributions** (menu / submenu /
command-palette entries) plus **screens** (config forms) that the
admin web app (`nexo-rs-plugin-admin`) renders generically — no
per-plugin JavaScript, no admin fork. The plugin's UI lives inside
the plugin (manifest + an optional live `describe` RPC) and the
daemon injects it into the admin via `nexo/admin/plugin_ui/*`.

Requires **`nexo-plugin-manifest >= 0.1.8`** (the version that adds
the `admin_ui` field). Plugins surface in the admin under
**Settings › Plugins** (`/m/settings/plugins`).

## Hybrid descriptor

The section is the **cold contract** — *what* a plugin may
contribute (slots, screens, trust-gated). The *live* descriptor
(current field values, dynamic select options) flows at runtime
through `nexo/admin/plugin_ui/describe`:

- **`describe = false`** (default) — the daemon SYNTHESISES the
  descriptor from `screens.fields` plus the plugin's current
  `cfg.plugins.<id>` values. No plugin code needed for a static
  config form.
- **`describe = true`** — the daemon FORWARDS `describe` to the
  plugin (via `[plugin.admin]` broker dispatch) so the plugin can
  vary fields / values / options by state.

Either way the frontend consumes a uniform `ScreenDescriptor`.

## Shape

```toml
[plugin.admin_ui]
schema_version = 1            # only 1 in v1
mode           = "declarative" # "embedded" (iframe) is v2, rejected in v1
describe       = false         # true ⇒ plugin serves the live descriptor

# Menu / submenu / command-palette entries.
[[plugin.admin_ui.contributions]]
id           = "google"                       # kebab, unique
slot         = "plugin.google.root"           # top-level placement (see Slots)
parent       = "google"                       # OR nest as a submenu (mutually exclusive with slot)
label        = "Google"                       # string OR { en = "…", es = "…" } (BCP-47)
icon         = "mail"                          # lucide-react subset
order        = 1000                            # plugins use 1000+
screen       = "smtp"                          # screen id this entry opens
visible_when = "plugin.enabled"                # optional mini-DSL (see below)

# Declarative screens.
[[plugin.admin_ui.screens]]
id    = "smtp"
title = "SMTP"
load  = "nexo/admin/google/admin_ui/load"      # optional RPC to hydrate values

[[plugin.admin_ui.screens.fields]]
key          = "host"            # maps to a config_schema property
type         = "text"            # see Field types
label        = "SMTP Host"
required     = true
help         = "Outbound mail server."
placeholder  = "smtp.gmail.com"
visible_when = "config.use_tls"

[[plugin.admin_ui.screens.actions]]
id         = "test"
label      = "Test connection"
method     = "nexo/admin/google/smtp_test"     # must start with nexo/admin/
on_success = "toast"                            # toast|inline_json|table|redirect|refresh
```

## Field types

`text` · `number` · `secret` · `toggle` · `select` · `multiselect`
· `list` · `link` · `textarea` · `json`. An unknown/future type
renders a graceful "unsupported" fallback.

- **`secret`** — write-only. The value routes to the generic
  credential store (Phase 93), never to YAML; the descriptor only
  reports `set` / `unset` status.
- **`select` / `multiselect`** — need `options` (inline) or
  `options_source`:

```toml
[plugin.admin_ui.screens.fields.options_source]
kind   = "rpc"                                       # or "static"
method = "nexo/admin/google/admin_ui/options/calendars"
```

## Slots + trust

A contribution targets a **core slot** or the plugin's own
namespace `plugin.<id>.<seg>`. Trust tier is derived from install
provenance (Phase 98), never self-declared:

| Slot | Official | CommunityIndexed | Unverified |
|------|----------|------------------|------------|
| `plugin.<id>.*` (own namespace) | ✅ | ✅ | ✅ |
| `core.sidebar.root` | ✅ | ✅ | ✅ (banner) |
| `core.command_palette.actions` | ✅ | ✅ | ❌ |
| `core.sidebar.{channels,integrations,settings}` | ✅ | ⚠️* | ❌ |
| `core.agent_detail.tabs` | ✅ | ⚠️* | ❌ |

`*` allowed for `CommunityIndexed` only when the operator sets
`NEXO_PLUGIN_ADMIN_UI_ALLOW_COMMUNITY_CORE_SLOTS=1`. Contributions
dropped by trust gating are reported via `hidden_count` (the admin
shows a "N hidden by trust tier" banner).

## `visible_when` mini-DSL

Optional boolean expression that shows/hides a contribution or
field, evaluated server-side (drop) + client-side (reactive):

```text
expr = or; or = and ('||' and)*; and = cmp ('&&' cmp)*;
cmp  = unary (('=='|'!='|'<'|'>'|'<='|'>=') unary)?;
unary = '!'? primary; primary = var | literal | '(' expr ')'
```

Context: `{ plugin: {installed, enabled, healthy}, config: {…},
tenant: {id}, user: {role} }`. Bounds: ≤ 200 chars, AST depth ≤ 5.
A missing variable resolves to false; a type-mismatched comparison
yields false (never panics).

## config_set semantics

The implicit **Save** calls `nexo/admin/plugin_ui/config_set`. The
daemon merges the submitted values into the existing
`cfg.plugins.<id>`, validates the **merged** result against
`[plugin.config_schema]` (so a partial edit keeps `required` fields
the operator already set), routes `secret` fields to the credential
store, persists the rest, and fires the Phase 97 reload signal —
the running agent picks up the new config without a restart.

## What to omit

Channel credentials (bot tokens, Signal sessions, OAuth files) are
managed via **pairing**, not this form. Omit them; a partial save
keeps them via merged-config validation. A `secret` field is for
operator-entered secrets the plugin reads from the credential store.
