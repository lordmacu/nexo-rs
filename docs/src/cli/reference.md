# CLI reference

Single source of truth for every `agent` subcommand, flag, exit code,
and env var. `agent` is the one binary you'll ever run in production
— this is everything it can do.

Source: `src/main.rs` (Mode enum + parse_args),
`crates/extensions/src/cli/`, `crates/setup/src/`.

## Invocation

```
agent [--config <dir>] [<subcommand> ...]
```

- **Arg parser:** hand-rolled, not `clap`. `--help` / `-h` work;
  `-c` is **not** an alias for `--config` (case-sensitive exact
  match).
- **No subcommand** → run the daemon (default).
- **Global flag:** `--config <dir>` (default `./config`).

## Global environment variables

| Variable | Values | Purpose |
|----------|--------|---------|
| `RUST_LOG` | tracing-subscriber filter | Log level (e.g. `info,agent=debug`). Default `info`. |
| `AGENT_LOG_FORMAT` | `pretty` \| `compact` \| `json` | Log format. Default `pretty`. |
| `AGENT_ENV` | `production` (or `prod`) | Triggers JSON logs unless `AGENT_LOG_FORMAT` overrides. |
| `TASKFLOW_DB_PATH` | file path | Flow CLI DB (default `./data/taskflow.db`). |
| `CONFIG_SECRETS_DIR` | dir path | Whitelists an extra root for `${file:...}` YAML refs. |

## Exit codes (generic)

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | General failure (not found, config invalid, connection refused) |
| `2` | Warnings-only outcome (currently only `--check-config` non-strict) |

Ext subcommand has its own richer code table — see below.

## Subcommand index

| Subcommand | Purpose |
|------------|---------|
| *(default)* | Run the agent daemon |
| [`setup`](#setup) | Interactive credential wizard |
| [`status`](#status) | Query running agent instances |
| [`dlq`](#dlq) | Dead-letter queue inspection |
| [`ext`](#ext) | Extension management |
| [`flow`](#flow) | TaskFlow operations |
| [`mcp-server`](#mcp-server) | Run as MCP stdio server |
| [`admin`](#admin) | Run the web admin UI behind a Cloudflare quick tunnel |
| [`reload`](#reload) | Trigger config hot-reload on a running daemon |
| `--check-config` | Pre-flight config validation |
| `--dry-run` | Load config and print the plan |

---

## Daemon (default)

```bash
agent [--config ./config]
```

Boots every configured agent runtime, connects to the broker (NATS or
local fallback), starts metrics (`:9090`), health (`:8080`), and admin
(`:9091 loopback`) servers.

**Exit codes:**
- `0` — clean shutdown via SIGTERM / Ctrl+C
- `1` — config load failed, broker unreachable at startup, plugin
  failed to initialize

**Logs to:** stderr. See [Logging](../ops/logging.md).

---

## `setup`

Interactive credential wizard. Launches a prompt-driven flow for
every service you want to enable — LLM keys, WhatsApp QR, Telegram
bot token, Google OAuth, etc.

```bash
agent setup                    # full interactive wizard
agent setup list               # list installable service ids
agent setup <service>          # configure one service (e.g. minimax, whatsapp)
agent setup doctor             # validate every credential / token (also runs the Phase 70.6 pairing-store audit)
agent setup telegram-link      # print Telegram bot link-to-chat URL
```

**Exit codes:** `0` on completion; `1` on error.

See [Setup wizard](../getting-started/setup-wizard.md) for the
step-by-step.

---

## `status`

Query the running daemon via the loopback admin console.

```bash
agent status                                   # every agent, table
agent status ana                               # one agent, table
agent status --json                            # raw JSON
agent status --endpoint http://remote:9091     # override endpoint
```

**Table output columns:** `ID | MODEL | BINDINGS | DELEGATES | DESCRIPTION`

**Exit codes:**
- `0` — query succeeded
- `1` — endpoint unreachable or agent id not found

---

## `dlq`

Dead-letter queue inspection. See [DLQ operations](../ops/dlq.md) for
the full picture.

```bash
agent dlq list                 # plain-text table, up to 1000 entries
agent dlq replay <id>          # move back to pending_events for retry
agent dlq purge                # drop every entry (destructive)
```

**Exit codes:** `0` success; `1` failure (entry not found, DB error).

**`list` columns:** `id | topic | failed_at | reason`.

---

## `ext`

Extension management. See [Extensions — CLI](../extensions/cli.md)
for details and workflows.

```bash
agent ext list                         [--json]
agent ext info <id>                    [--json]
agent ext enable <id>
agent ext disable <id>
agent ext validate <path>
agent ext doctor                       [--runtime] [--json]
agent ext install <path>               [--update] [--enable] [--dry-run] [--link] [--json]
agent ext uninstall <id> --yes         [--json]
```

**Flags:**

| Flag | Where | Purpose |
|------|-------|---------|
| `--json` | list / info / doctor / install / uninstall | Machine-readable output |
| `--runtime` | `doctor` | Also spawn stdio extensions to verify handshake |
| `--update` | `install` | Overwrite if already installed |
| `--enable` | `install` | Flip to `enabled: true` in `extensions.yaml` |
| `--link` | `install` | Symlink source (absolute path required) instead of copy |
| `--dry-run` | `install` | Validate without writing |
| `--yes` | `uninstall` | Required confirmation |

**Exit codes (extension-specific):**

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Extension not found / `--update` target missing |
| 2 | Invalid manifest / invalid source / `--link` needs absolute path |
| 3 | Config write failed |
| 4 | Invalid id (reserved or empty) |
| 5 | Target exists (use `--update`) |
| 6 | Id collision across roots |
| 7 | `uninstall` missing `--yes` confirmation |
| 8 | Copy / atomic swap failed |
| 9 | Runtime check(s) failed (`doctor --runtime`) |

---

## `flow`

TaskFlow operations. See [TaskFlow — FlowManager](../taskflow/manager.md#cli).

```bash
agent flow list                [--json]
agent flow show <id>           [--json]
agent flow cancel <id>
agent flow resume <id>
```

**Env var:** `TASKFLOW_DB_PATH` (default `./data/taskflow.db`).

**Exit codes:** `0` success; `1` on error (flow not found, wrong
state, DB inaccessible).

`list` sorts by `updated_at DESC`; `show` includes every recorded
step; `resume` only works on `Manual` or `ExternalEvent` waits.

---

## `mcp-server`

Run the agent as an MCP stdio server so MCP clients (Claude Desktop,
Cursor, Zed) can consume its tools.

```bash
agent mcp-server
```

- Reads JSON-RPC from stdin, writes responses to stdout
- Does **not** boot a daemon or broker
- Requires `config/mcp_server.yaml` with `enabled: true`

**Exit codes:** `0` on clean exit; `1` if `mcp_server.yaml` disabled.

See [MCP — Agent as MCP server](../mcp/server.md) for deployment
recipes (Claude Desktop config, allowlist, auth token).

---

## `admin`

Run the web admin UI behind a fresh Cloudflare quick tunnel. A new
ephemeral trycloudflare.com URL is minted on every launch — no
account, no DNS, no TLS setup.

```bash
agent admin                  # listen on 127.0.0.1:9099 (default)
agent admin --port 9199      # pick a different loopback port
agent admin --port=9199      # same thing, equals form
```

What happens on launch:

1. **Install cloudflared if missing.** The tunnel crate detects the
   host OS/arch and downloads the matching cloudflared binary into
   the platform data dir. Subsequent launches reuse the cached copy.
2. **Mint a fresh random password.** 24 URL-safe characters from the
   OS RNG. Printed once to stdout — copy it now; there is no
   recovery short of relaunching `agent admin`.
3. **Start a loopback HTTP server.** Listens on
   `127.0.0.1:<port>` and serves the React bundle embedded at Rust
   compile time (see `admin-ui/`) behind HTTP Basic Auth. A
   bundle-missing fallback page is served if `admin-ui/dist/` was
   empty when `cargo build` ran.
4. **Open a quick tunnel.** `cloudflared tunnel --url http://127.0.0.1:<port>`
   returns an ephemeral `https://…trycloudflare.com` URL, which the
   command prints to stdout alongside the username (`admin`) and the
   freshly-minted password.
5. **Wait for Ctrl+C / SIGTERM.** Graceful shutdown kills the
   cloudflared child and stops the HTTP listener.

**Exit codes:**
- `0` — clean shutdown
- `1` — cloudflared install failed, port already bound, or tunnel
  negotiation failed

**Notes:**
- URL is re-generated every launch. If you need a stable URL,
  switch to a named Cloudflare tunnel (requires an account and
  wrangler config — out of scope for this command).
- Auth is **HTTP Basic** for now; the browser prompts for
  `admin` / `<password>` on first load. Username is fixed; password
  is fresh every launch. Keep the shell scrollback if you need to
  re-paste it.
- The password is **never persisted** — losing it means stopping
  `agent admin` and starting again (which also rotates the tunnel
  URL).

---

## `reload`

Triggers a config hot-reload on a running daemon. Publishes
`control.reload` on the broker the daemon is listening to (resolved
from `broker.yaml`), subscribes-before-publish to
`control.reload.ack`, waits up to 5 s, and prints the outcome.

```bash
agent reload                 # human-readable summary
agent reload --json          # serialized ReloadOutcome
```

Example output:

```
$ agent reload
reload v7: applied=2 rejected=0 elapsed=18ms
  ✓ ana
  ✓ bob
```

**Exit codes:**
- `0` — at least one agent reloaded
- `1` — no ack within 5 s (daemon not running)
- `2` — every agent rejected

Full semantics — what's reloaded, apply-on-next-message, failure
modes — in [Config hot-reload](../ops/hot-reload.md).

---

## `--check-config`

Pre-flight validation. Loads every YAML file, resolves env vars,
checks schema, validates credentials. No broker, no daemon. Meant for
CI.

```bash
agent --check-config                    # warnings-only mode
agent --check-config --strict           # warnings become errors
```

**Exit codes:**
- `0` — all clear
- `1` — hard errors (missing required creds, invalid schema)
- `2` — warnings only (non-strict mode)

---

## `--dry-run`

Load the config and print a plan. Doesn't connect to the broker or
start any runtime task.

```bash
agent --dry-run
agent --dry-run --json
```

**Output (plain text):**

- Config directory
- Broker kind (nats | local)
- Plugin list
- Agent directory table (id, model, bindings, delegates, description)

**Exit codes:** `0` valid; `1` on error.

## Daemon admin endpoints

Reference for `status --endpoint` and anyone wiring a custom
dashboard:

| Endpoint | Method | Bind | Purpose |
|----------|--------|------|---------|
| `/admin/agents` | GET | `127.0.0.1:9091` | List every agent (JSON) |
| `/admin/agents/<id>` | GET | `127.0.0.1:9091` | Single agent (JSON) |
| `/admin/tool-policy` | GET | `127.0.0.1:9091` | Tool policy queries |
| `/admin/credentials/reload` | POST | `127.0.0.1:9091` | Phase 17 — re-read agents/plugins YAML and atomically swap the credential resolver. Returns `ReloadOutcome` JSON. See [`config/credentials.md`](../config/credentials.md#hot-reload-no-daemon-restart). |
| `/health` | GET | `0.0.0.0:8080` | Liveness probe |
| `/ready` | GET | `0.0.0.0:8080` | Readiness probe |
| `/metrics` | GET | `0.0.0.0:9090` | Prometheus |
| `/whatsapp/pair*` | GET | `0.0.0.0:8080` | WhatsApp pairing QR (first instance) |
| `/whatsapp/<instance>/pair*` | GET | `0.0.0.0:8080` | Multi-instance WhatsApp pairing |

## Cross-links

- [Setup wizard](../getting-started/setup-wizard.md)
- [Extensions — CLI](../extensions/cli.md)
- [TaskFlow — FlowManager + CLI](../taskflow/manager.md)
- [DLQ operations](../ops/dlq.md)
- [Metrics + health](../ops/metrics.md)

## Gotchas

- **Hand-rolled parser.** Unexpected flag ordering can produce
  "unknown argument" errors that are less forgiving than clap-based
  CLIs. Stick to the form shown in each subcommand.
- **Global `--config` must come before the subcommand.** `agent
  --config ./x ext list` works; `agent ext list --config ./x` does
  not.
- **Admin console is loopback-only.** `status --endpoint` against a
  remote host requires a tunnel; it won't listen publicly.
