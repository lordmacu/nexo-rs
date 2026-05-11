# Zero-config quickstart

The fastest path from an installed `nexo` binary to a running
daemon — **no YAML editing required, no NATS server, no API key
needed to boot.** (Install: `curl … install.sh | bash`,
`cargo install nexo-rs`, or a `.deb`/`.rpm` — see
[Installation](./installation.md).) Once running, configure
incrementally via `nexo init` (scaffold sample YAMLs),
`nexo set-broker` (switch broker mode), or the operator UI
(admin RPCs).

Total wall-clock time: **30 seconds** from installed binary
to live daemon serving health + admin RPCs.

> This page reflects the post-Phase-92/93/94/95 ergonomics. For
> the classical full-control walkthrough (manually edit each
> YAML, pair channels, talk to a Telegram bot end-to-end), see
> [Quickstart](./quickstart.md).

---

## 1. Run the daemon

```bash
nexo
```

That's it. The daemon discovers its config dir per
[Phase 92.9 precedence](#config-dir-discovery), falls back to
baked-in defaults for any YAML that's missing (Phase 93), and
starts serving on the default health + admin endpoints.

Expected log:

```
WARN  nexo_config: config dir not found — booting with
                   Default::default() for every YAML
                   (0 agents, BrokerKind::Local, 0 llm
                    providers, sqlite memory at default path)
INFO  nexo: broker ready kind=Local url=
INFO  nexo: long-term memory ready path=./data/memory.db
INFO  plugins.discovery: plugin registry wire complete loaded=0
INFO  nexo: pairing initialised
INFO  nexo: agent ready — waiting for shutdown signal
```

What's happening:

- **0 agents** — daemon waits for `nexo/admin/agents/upsert`
  via the operator UI.
- **`BrokerKind::Local`** — in-process `tokio::mpsc`. No NATS
  server needed. Subprocess plugins bridge through stdio
  (Phase 92).
- **0 LLM providers** — any tool call to an LLM fails loud
  with `provider_not_found`; the daemon stays up.
- **SQLite memory** at `./data/memory.db` — auto-created.

The daemon is now ready to accept admin RPCs to populate
state. The next sections walk through adding config either
via the operator UI (recommended) or by hand-editing YAMLs.

---

## 2. Operator UI (recommended)

If you have the `agent-creator-microapp` extension installed,
it exposes a web UI for managing agents, LLM providers,
channels, and credentials. Every UI action is an admin RPC
behind the scenes:

| UI route                | Admin RPC                              |
|-------------------------|----------------------------------------|
| New agent               | `nexo/admin/agents/upsert`             |
| Add LLM provider + key  | `nexo/admin/llm_providers/upsert`      |
| Pair WhatsApp           | `nexo/admin/pairing/start`             |
| Register credentials    | `nexo/admin/credentials/register`      |
| Set marketing rules     | `nexo/admin/marketing/rules/upsert`    |

Each RPC writes to the corresponding YAML on disk **and**
notifies the running daemon via `config_watch`, so changes
take effect without restart for hot-reloadable subsystems.

If you don't yet have a microapp installed, see
[`agent-creator-microapp`](https://github.com/lordmacu/agent-creator-microapp)
or skip ahead to YAML scaffolding.

---

## 3. Scaffold sample YAMLs

When you want to configure something specific and don't want
to read the source for field shapes, ask the daemon to write
heavily-commented templates:

```bash
nexo init                        # writes 19 sample YAMLs to ~/.config/nexo/
nexo init --yaml broker          # only broker.yaml
nexo init --yaml broker,llm      # comma-separated names
nexo init --yaml plugins         # all plugins/*.yaml templates
nexo init --output /etc/nexo     # custom target dir
nexo init --force                # overwrite existing files
nexo init --yaml llm --stdout    # emit to stdout (for piping)
```

Templates cover the four required configs (`agents`,
`broker`, `llm`, `memory`) plus every optional subsystem
(`extensions`, `mcp`, `runtime`, `pollers`, `taskflow`,
`transcripts`, `pairing`, `webhook_receiver`) and the plugin
+ persona dirs.

Edit any of them, then restart the daemon (or let
`config_watch` pick up the change). Empty fields stay at
their defaults — you only fill in what you actually want.

Example: adding a MiniMax LLM provider after `nexo init`:

```bash
nexo init --yaml llm --output ~/.config/nexo
# Edit ~/.config/nexo/llm.yaml — uncomment the minimax block
# Set MINIMAX_API_KEY in your env (the YAML uses ${MINIMAX_API_KEY})
export MINIMAX_API_KEY=your-key
nexo
```

---

## 4. Switch the broker at runtime

`broker.yaml type:` is the single most operator-tweaked
field. Skip the YAML edit and use the dedicated subcommand:

```bash
nexo set-broker local                            # stdio bridge (default)
nexo set-broker nats --url nats://localhost:4222 # multi-host cluster
nexo set-broker local --no-signal                # edit YAML only, no respawn
```

The subcommand edits `broker.yaml` in your resolved config
dir (auto-creating the file with defaults if missing) and
sends SIGTERM to the running daemon by default. The
supervisor loop (`dev-daemon.sh`, `systemd`, etc.) respawns
and picks up the new config — ~3 second blackout.

`--no-signal` skips the kill; you control the restart
timing yourself.

When to pick which broker shape:
[broker shapes architecture](../architecture/broker-shapes.md).

---

## 5. Override config via env vars (12-factor)

For Docker / Kubernetes / CI deployments where YAMLs live
in secret mounts at non-canonical paths:

```bash
NEXO_BROKER_YAML=/run/secrets/broker.yaml \
NEXO_LLM_YAML=/run/secrets/llm.yaml \
NEXO_AGENTS_YAML=/etc/cfg/agents.yaml \
  nexo
```

Each `NEXO_<NAME>_YAML` env points at an absolute path. If
set, that file **wholesale replaces** the YAML the daemon
would otherwise load from the config dir.

Currently supported (Phase 94): `NEXO_AGENTS_YAML`,
`NEXO_BROKER_YAML`, `NEXO_LLM_YAML`, `NEXO_MEMORY_YAML`.

---

## 6. Layered overrides (Kustomize-style)

For ConfigMap base + Secret overlay deployments where you
want to override specific *fields* (not whole files):

```bash
nexo --config /etc/nexo --override-from /run/secrets
```

The daemon loads each YAML from `/etc/nexo/<name>.yaml`,
then deep-merges the same-named file from `/run/secrets/<name>.yaml`
on top (per-field for mappings; wholesale replace for
sequences and scalars).

Example:

```yaml
# /etc/nexo/broker.yaml  (committed to git, in ConfigMap)
broker:
  type: nats
  url: nats://placeholder
  persistence:
    enabled: true
```

```yaml
# /run/secrets/broker.yaml  (mounted from Kubernetes Secret)
broker:
  url: nats://prod-cluster.example.com:4222
```

Effective config the daemon sees:

```yaml
broker:
  type: nats                                    # from base
  url: nats://prod-cluster.example.com:4222     # from overlay
  persistence:
    enabled: true                               # from base (overlay didn't touch)
```

The same chain applies to all four required YAMLs. Both env
vars and `--override-from` compose with Phase 93 defaults:
when neither layer has a value, the daemon's `Default::default()`
takes over.

---

## Config dir discovery

When `--config <dir>` is not passed explicitly, the daemon
resolves the config dir in this order:

1. `NEXO_CONFIG_DIR` env var
2. `./config` relative to cwd (legacy, only when present)
3. `$XDG_CONFIG_HOME/nexo` or `$HOME/.config/nexo`
4. `./config` as a last-resort error path

Subcommands that read or write config (`nexo init`,
`nexo set-broker`) auto-create the directory and any
missing files when they need to — operators don't run
`mkdir` first.

---

## Composition summary

The four ergonomic phases stack like this:

```
┌────────────────────────────────────────────────────────────┐
│  Phase 95: nexo init                                       │
│    Scaffolds sample YAMLs with field-level docs.           │
│                          ↓                                 │
│  Operator edits YAML  OR  uses admin RPC  OR  set-broker   │
│                          ↓                                 │
│  Phase 94: env / override-from                             │
│    NEXO_<NAME>_YAML overrides; --override-from deep-merges.│
│                          ↓                                 │
│  Phase 92.9: --config / NEXO_CONFIG_DIR / XDG default      │
│    Base config dir resolution.                             │
│                          ↓                                 │
│  Phase 93: zero-config defaults                            │
│    Any YAML still missing → Default::default().            │
│                          ↓                                 │
│  Phase 92: subprocess broker bridge                        │
│    `broker.yaml type: local` works even with extracted     │
│    subprocess plugins. No NATS server required.            │
└────────────────────────────────────────────────────────────┘
```

Each layer is optional; pick the ones your deployment needs.

---

## Where to next

- **Want the classical walkthrough?** → [Quickstart](./quickstart.md)
  shows manual YAML editing + Telegram bot end-to-end.
- **Deploy targets in detail** → [`.deb`](./install-deb.md),
  [`.rpm`](./install-rpm.md), [Termux](./install-termux.md),
  [Nix](./install-nix.md), [native](./install-native.md).
- **Add an agent at runtime** → [agents.yaml](../config/agents.md)
  or the operator UI's `/agents/new` page.
- **Broker shapes** → [broker-shapes.md](../architecture/broker-shapes.md)
  covers when to pick NATS vs local vs embedded.
- **Microapp framework** → [Microapps · getting started](../microapps/getting-started.md)
  for building operator-facing UIs on top of the daemon.
