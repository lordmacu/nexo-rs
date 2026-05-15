# `[plugin.dashboard]` — setup wizard surface

> Phase 81.33.b.real Stage 6 (Layer 10 polish of the
> [plugin auto-discovery design](../architecture/plugin-auto-discovery.md)).
> Status: shipped 2026-05-15.

Plugins that want to appear in the setup wizard's channel
dashboard declare how the daemon detects their instances + auth
state via this manifest section. A generic
`ManifestDashboardSource` in `nexo-setup` consumes the section
and runs the right discovery / auth check, eliminating the need
for per-channel hardcoded
`crates/setup/src/services/channels_dashboard.rs` impls (the 3
canonical ones — telegram, whatsapp, email — stay as fallback
until canonical plugin crates ship manifest revisions).

## Manifest section

```toml
[plugin.dashboard]
# Instance enumeration strategy:
[plugin.dashboard.layout]
kind = "single"                          # | "workspace_walk"

# Auth-state probe strategy:
[plugin.dashboard.auth_check]
kind = "file_presence"                   # | "session_dir_files"
path = "telegram_bot_token.txt"          # for file_presence (relative to secrets_dir)
```

### Telegram shape (single instance, file-presence auth)

```toml
[plugin.dashboard.layout]
kind = "single"

[plugin.dashboard.auth_check]
kind = "file_presence"
path = "telegram_bot_token.txt"
```

### Email shape

```toml
[plugin.dashboard.layout]
kind = "single"

[plugin.dashboard.auth_check]
kind = "file_presence"
path = "email_password.txt"
```

### Whatsapp shape (multi-instance via workspace walk, session-dir auth)

```toml
[plugin.dashboard.layout]
kind = "workspace_walk"
subdir = "whatsapp"

[plugin.dashboard.auth_check]
kind = "session_dir_files"
candidates = ["session.db", "state.db", "device.json", "registration.json"]
```

## Field reference

### `layout`

- `kind = "single"` — exactly one instance labelled `"default"`.
  Used by channels with one account per agent (telegram, email).
- `kind = "workspace_walk"`, `subdir = "<name>"` — walk
  `<workspace>/<agent>/<subdir>/<instance>/` for every directory
  entry. Used by channels with multi-instance per-agent layouts
  (whatsapp). `subdir` must be a single segment (no `/`).

### `auth_check`

- `kind = "file_presence"`, `path = "<rel>"` — authenticated if
  `<secrets_dir>/<rel>` exists + is non-empty. Path must be
  relative (no leading `/`). `<secrets_dir>` is the operator's
  secrets root (typically `~/.nexo/secrets` or
  `$NEXO_HOME/secrets`).
- `kind = "session_dir_files"`, `candidates = [...]` —
  authenticated if the per-instance directory contains ANY of
  the listed filenames. Only meaningful with
  `layout.kind = "workspace_walk"`; the per-instance dir is
  `<workspace>/<agent>/<subdir>/<instance>/`. If the directory
  exists with OTHER files (but none of the candidates), reports
  `Stale`; empty dir reports `NotAuthenticated`.

## Daemon-side dispatch

`crates/setup/src/services/channels_dashboard.rs` ships:

- `pub trait ChannelDashboardSource` — `channel_id()` +
  `discover()`.
- `pub struct ManifestDashboardSource` — generic impl that
  consumes a parsed `PluginDashboardSection` + plugin id.
- `pub fn dashboard_sources_from_manifests(manifests) -> Vec<Box<dyn ChannelDashboardSource>>`
  — helper that filters manifests + builds a source per
  declaring plugin.
- `pub fn default_dashboard_sources() -> Vec<Box<dyn ChannelDashboardSource>>`
  — the 3 hardcoded canonical impls (telegram, whatsapp,
  email). Kept for backwards compat until canonical plugin
  crates ship manifest revisions.

Operators combining both:

```rust
let mut sources = default_dashboard_sources();
sources.extend(dashboard_sources_from_manifests(&discovered_manifests));
let entries = detect_channels_with_sources(&sources, &config_dir, &secrets_dir)?;
```

## Migration

Canonical plugin crates currently ship NO `[plugin.dashboard]`
section. The 3 hardcoded sources in `nexo-setup` continue to
serve the wizard. When a plugin ships the section AND the
wizard caller discovers manifests + extends the source list,
the manifest-driven source contributes alongside the hardcoded
one. Once all 3 canonical plugins migrate, the hardcoded sources
can be retired in a Stage 7 cleanup follow-up.

New canonical channels added in the future (signal, sms, …)
ship the manifest section directly + skip the hardcoded path
entirely.

## Validation

- `cargo build --release-fast --bin nexo` (default) — 3m13s.
- `cargo build --release-fast --bin nexo --no-default-features`
  — 2m54s.
- `cargo nextest run --workspace` — 6334/6334 (13 new tests:
  8 in `nexo-plugin-manifest::dashboard::tests`, 5 in
  `nexo-setup::services::channels_dashboard::manifest_dashboard_tests`).
- `mdbook build docs` clean.

## Trade-offs

| Concern | Decision |
|---|---|
| Schema enumerates known shapes | 2 layouts (single / workspace_walk) + 2 auth checks (file_presence / session_dir_files). Covers the 3 canonical channels. New shapes = schema extension (typed enum variant + interpreter branch). |
| Plugin-side auth check via broker (alternative) | Rejected: the wizard runs WITHOUT the plugin subprocess alive in many scenarios (initial setup, secret rotation, plugin-binary-not-yet-installed). The daemon performing the FS check directly is more robust. |
| `channel_id` `'static` lifetime | Process-wide intern table keyed by plugin id; one-time leak per plugin per process. Bounded by plugin count. |
| Workspace walk path resolution | `<config_dir>.parent() / data/workspace` — matches the layout used by canonical plugins today. Manifest does NOT let plugins override the workspace root path (security: prevent arbitrary FS reads via a malicious manifest). |
| Symbol exports | `ManifestDashboardSource` + `dashboard_sources_from_manifests` are public so callers (admin wizard, setup CLI, future microapp) can wire them without re-implementing. |
