# Config hot-reload

Operators rotate per-agent knobs (allowlists, model strings, prompts,
rate limits, delegation gates) without restarting the daemon. Sessions
currently handling a message finish their turn on the old snapshot;
the **next** event picks up the new one (apply-on-next-message). Plugin
configs (`whatsapp.yaml`, `telegram.yaml`, …) are **not** hot-reloadable
yet — see [limitations](#limitations).

## What triggers a reload

| Trigger | Source |
|---|---|
| File save under `config/` | `notify`-based watcher, debounced 500 ms |
| `agent reload` CLI | Publishes `control.reload` on the broker |
| Direct broker publish | Any integration can emit `control.reload` |

## What's reloaded

Files watched by default (paths relative to the config dir):

- `agents.yaml`
- `agents.d/` (recursive)
- `llm.yaml`
- `runtime.yaml`

Extra paths listed under `runtime.reload.extra_watch_paths` are
appended to the list.

The fields that apply live without a restart:

| Field | Location | Effect |
|---|---|---|
| `allowed_tools` (agent + binding) | `agents.d/*.yaml` | Tool list visible to the LLM + per-call guard |
| `outbound_allowlist` | same | Defense-in-depth in `whatsapp_send_*` / `telegram_send_*` |
| `skills` | same | Skill blocks rendered into the system prompt |
| `model.model` (binding-level) | same | LLM model string on next turn |
| `system_prompt` + `system_prompt_extra` | same | System block composition |
| `sender_rate_limit` | same | Per-binding token bucket |
| `allowed_delegates` | same | Delegation ACL |
| `providers.<name>.api_key` | `llm.yaml` | Rotated via a fresh `LlmClient` on next turn |
| `lsp.languages`, `lsp.idle_teardown_secs`, `lsp.prewarm` (agent + binding) | `agents.d/*.yaml` | LSP tool reads policy per call (C2) |
| `team.max_members`, `team.max_concurrent`, `team.idle_timeout_secs`, `team.worktree_per_member` (agent + binding) | same | Team* tools read policy per call (C2) |
| `config_tool.allowed_paths`, `config_tool.approval_timeout_secs` | same | Read on the next ConfigTool call (M11 follow-up promotes the rest) |
| `repl.allowed_runtimes` (agent + binding) | same | ReplTool gates spawn on the per-call allowlist (C2) |
| `remote_triggers` (agent + binding) | same | RemoteTriggerTool reads allowlist per call |
| `cron_*` model fields | same | CronCreateTool reads `effective.model` per call |
| `proactive.tick_interval_secs`, `proactive.jitter_pct`, `proactive.max_idle_secs` | same | Proactive driver reads on the next tick |
| All Phase 16 binding overrides (`allowed_tools`, `outbound_allowlist`, `skills`, `model.model`, `system_prompt_extra`, `sender_rate_limit`, `allowed_delegates`, `language`, `link_understanding`, `web_search`, `pairing_policy`, `dispatch_policy`, `remote_triggers`, `proactive`, `repl`, `lsp`, `team`, `config_tool`) | `agents.d/*.yaml`, `inbound_bindings[].<field>` | Resolved fresh per snapshot build; consumed at handler entry via `ctx.effective_policy()` |

Fields that **require a restart** (logged as `warn` during reload):

- `id`, `plugins`, `workspace`, `skills_dir`, `transcripts_dir`
- `heartbeat.enabled`, `heartbeat.interval`
- `config.debounce_ms`, `config.queue_cap`
- `model.provider` (binding-level provider must match agent provider —
  the `LlmClient` is wired once per agent)
- `broker.yaml`, `memory.yaml`, `mcp.yaml`, `extensions.yaml`
- **Boolean enable flips**: `lsp.enabled`, `team.enabled`,
  `repl.enabled`, `config_tool.self_edit`, `proactive.enabled`
  (any per-binding override of these). Flipping `false → true`
  requires registering the tool in the per-agent `tool_base`
  (immutable post-boot — `Arc<ToolRegistry>`); flipping
  `true → false` would leave a registered-but-refused tool that
  the LLM still sees in its catalogue. The handler refuses with a
  `<feature>Disabled` error in the second case, but operators
  should restart for clean semantics.
- **Subsystem actor lifecycle**: `LspManager` child processes,
  `ReplRegistry` subprocess pool, `TeamMessageRouter` broker
  subscriptions stay alive across reloads. Operator restart is
  required to recycle child processes (e.g. after a toolchain
  update for `rust-analyzer`).

The "boolean enable flips" + "subsystem actor lifecycle"
limitations match prior art: `upstream agent CLI
useManageMCPConnections.ts:624` does invalidate-and-refetch
without killing the MCP child stdio process; OpenClaw
`research/src/plugins/services.ts:33-78` boots plugin services
once per process and keeps them resident across config changes.

Adding or removing an agent also requires a restart in this release;
see [limitations](#limitations).

## Configuration

`config/runtime.yaml` is optional. Defaults:

```yaml
reload:
  enabled: true           # master switch
  debounce_ms: 500        # notify-debouncer-full window
  extra_watch_paths: []   # appended to the built-in list
cron:
  one_shot_retry:
    max_retries: 3
    base_backoff_secs: 30
    max_backoff_secs: 1800
```

Set `enabled: false` to turn off the file watcher + the
`control.reload` subscriber. The CLI `agent reload` still works — the
daemon never opens a privileged socket, it just listens on the shared
broker.

## The reload pipeline

```
file save / CLI / broker
        │
        ▼
  debouncer (500 ms)
        │
        ▼
  AppConfig::load (YAML + env resolution)
        │
        ▼
  validate_agents_with_providers  ──fail──▶  log warn, bump
        │                                    config_reload_rejected_total,
        ▼                                    keep old snapshot
  RuntimeSnapshot::build (per agent)
        │
        ▼
  ArcSwap::store  (atomic per agent)
        │
        ▼
  events.runtime.config.reloaded
```

Validation failure never swaps. The daemon always serves a snapshot
that passed its boot gauntlet.

## CLI

```bash
# Human-readable output
$ agent reload
reload v7: applied=2 rejected=0 elapsed=18ms
  ✓ ana
  ✓ bob

# Machine-readable
$ agent reload --json
{
  "version": 7,
  "applied": ["ana", "bob"],
  "rejected": [],
  "elapsed_ms": 18
}
```

Exit codes:
- `0` — at least one agent reloaded.
- `1` — no `control.reload.ack` within 5 s (daemon not running).
- `2` — every agent rejected (partial-fail signal for CI).

## Broker contract

| Topic | Direction | Payload |
|---|---|---|
| `control.reload` | → daemon | `{requested_by: string}` |
| `control.reload.ack` | ← daemon | serialized `ReloadOutcome` |

`ReloadOutcome` JSON shape:

```json
{
  "version": 7,
  "applied": ["ana", "bob"],
  "rejected": [
    {"agent_id": "ana", "reason": "snapshot build: ..."}
  ],
  "elapsed_ms": 18
}
```

## Telemetry

| Metric | Type | Labels |
|---|---|---|
| `config_reload_applied_total` | counter | — |
| `config_reload_rejected_total` | counter | — |
| `config_reload_latency_ms` | histogram | — |
| `runtime_config_version` | gauge | `agent_id` |

Scrape via the metrics endpoint ([ops/metrics](./metrics.md)).

## Apply-on-next-message semantics

A reload does not interrupt sessions that are currently handling a
message. Specifically:

- The LLM turn in flight keeps its captured `Arc<RuntimeSnapshot>` for
  the life of the turn — tool calls inside that turn all see the same
  policy, even if several reloads land during the turn.
- The **next** event delivered to the agent reads the latest snapshot
  via `snapshot.load()` on the intake hot path.

If you need a "force-apply now" semantic (terminate in-flight sessions,
respawn), use `agent reload --kick-sessions` — **not implemented yet**,
tracked in Phase 19.

## Security model

- **`control.reload` topic has no application-level auth.** Anyone
  with broker publish rights can trigger a reload. In production with
  NATS, restrict the `control.>` subject pattern via NATS account
  permissions; see [NATS with TLS + auth](../recipes/nats-tls-auth.md).
  The local-broker fallback is in-process only — no remote attack
  surface.
- **File-watcher trust = filesystem write.** Whoever can edit
  `config/agents.d/*.yaml` can change capability surface. Treat the
  config dir as a privileged resource: 0600 on YAML files, 0700 on
  the directory.
- **`events.runtime.config.reloaded` payload includes agent ids and
  rejection reasons.** Subscribers see them. Single-process
  deployments are fine; in multi-tenant setups, gate the
  `events.runtime.>` pattern in NATS auth.
- **Outbound allowlist scope.** The Phase 16 outbound allowlist
  governs WhatsApp + Telegram tools only. Google tools are gated by
  the OAuth scopes granted at credential creation (see
  [Per-agent credentials](../config/credentials.md)) — there is no
  per-recipient list for Google.
- **Apply-on-next-message and tightening reloads.** A reload that
  narrows an allowlist for security reasons does **not** affect
  in-flight sessions until they next receive an event. If you need
  the change to take effect immediately, restart the daemon (or wait
  for the upcoming `agent reload --kick-sessions` flag in Phase 19).

## Failure modes

- **Bad YAML**: `AppConfig::load` fails. Old snapshot keeps serving.
  `config_reload_rejected_total` bumps. The warn log names the file +
  line.
- **Validation errors**: aggregate — every problem across every agent
  shows in one warn block. Fix them in one edit instead of
  restart-and-repeat.
- **Unknown provider**: rejected at boot + at reload by
  `KnownProviders` check. Boot validation lists what's registered.
- **Missing tool in binding's `allowed_tools`**: caught by the
  post-registry validation pass during reload.
- **Agent added / removed**: Phase 18 rejects these with a clear
  message; restart the daemon to reshape the fleet.

## Limitations

Intentional scope gaps for Phase 18, tracked for Phase 19:

- **Add / remove agent** at runtime. The coordinator rejects new ids
  and left-over registered handles with an actionable message. Restart
  needed.
- **Plugin config hot-reload** (`whatsapp.yaml`, `telegram.yaml`,
  `browser.yaml`, `email.yaml`). Plugin daemons own I/O (QR pairing,
  long-polling). Reshaping them live requires a dedicated lifecycle
  refactor.
- **`config_reloaded` hook** for extensions to react. Pending.
- **SIGHUP trigger** as an extra UX path. Deferred — use the broker
  topic or the CLI.

## See also

- [Layout](../config/layout.md) — where these files live
- [agents.yaml](../config/agents.md) — the per-agent surface
- [llm.yaml](../config/llm.md) — provider credentials
- [Metrics (Prometheus)](./metrics.md)
