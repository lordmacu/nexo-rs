# The autonomous agent — capabilities overview

This page is the bird's-eye map of what an `agent` running on
nexo can actually do without you holding its hand. Every
sub-feature has its own page (linked at the end of each
section); this page exists so you can see the whole picture
without piecing it together from individual reference docs.

"Autonomous" here doesn't mean "AGI". It means: the agent runs
in the background, decides when to act on its own schedule,
remembers what it has learned, talks to the user through every
channel the operator wired (Slack, Telegram, iMessage, email,
WhatsApp), approves or escalates risky actions through curated
gates, and survives daemon restarts without losing context.

The agent never executes anything the operator didn't authorise
in YAML. Every autonomous behaviour is a knob the operator
flips on with explicit consent — there are no implicit defaults
that ship a user from "ran nexo for the first time" to "the
agent is texting my boss".

---

## 1. Living in the background

The agent doesn't need a foreground TTY to run.

- **Session kinds** — every running goal carries a `SessionKind`
  enum: `Interactive` (default, attached to a terminal), `Bg`
  (detached background goal — `nexo agent run --bg <prompt>`),
  `Daemon` (a long-running goal supervised by the daemon process
  itself), or `DaemonWorker` (a child of a daemon).
- **`nexo agent run --bg "<prompt>"`** spawns a goal, returns
  the `goal_id` immediately, detaches. The agent keeps running
  even after you close the terminal.
- **`nexo agent ps`** lists running goals filtered by kind;
  `--all` includes `Interactive`. RO SQLite — works without a
  daemon up.
- **`nexo agent attach <goal_id>`** renders a markdown snapshot
  of any goal: kind, status, phase, started_at, finished_at,
  diff_stat, last decision, last event. Useful to check
  progress without interrupting.
- **`nexo agent discover`** lists Running goals filtered to
  detached / daemon kinds. Pass `--include-interactive` to
  broaden.
- **Reattach on restart** — boot flips prior-run `Running` rows
  to `LostOnRestart` and fires `notify_origin` once per goal so
  the originating chat sees a clean `[abandoned]` closure
  instead of silence.
- **Drain on SIGTERM** — `drain_running_goals` runs BEFORE
  plugin teardown so `[shutdown]` `notify_origin` actually leaves
  the channel before the daemon dies. Per-hook 2 s timeout
  prevents stuck publishers from hanging shutdown.

→ See [Background agents (`agent run --bg` / ps / attach)](../cli/agent-bg.md)

---

## 2. Memory + self-improvement

The agent learns. Three tiers, each with a different cost /
durability trade-off.

- **Short-term memory** — per-session, in RAM, scoped to the
  current goal. Cheap; gone on goal completion.
- **Long-term memory** — SQLite + sqlite-vec embeddings
  (`crates/memory/src/long_term.rs`). Survives restarts;
  searchable by semantic + lexical query.
- **Git-backed `MEMORY.md`** — every memory promotion writes a
  markdown file and commits it to a per-agent git repo. Full
  history; operator can `git log MEMORY.md` to audit what the
  agent decided to remember.

Three self-improvement loops the agent runs without operator
intervention:

- **Light-pass dreaming** — scoring-based consolidation runs
  every N turns. Cheap, no LLM call, just promotes warm memories
  via decay × access × recency.
- **Deep-pass autoDream (Phase 80.1)** — heavier consolidation
  via a forked sub-agent with its own 4-phase prompt, runs
  behind 7 gates: kairos active, time-since-last (default 24 h),
  session count ≥ 5 transcripts, scan throttle (10 min), live
  consolidation lock (PID + mtime), force bypass, post-fork
  escape audit. Deferred for fork (`deferred_for_fork: true`)
  when another process holds the lock — promotions land on the
  next turn rather than racing.
- **`extract_memories` (Phase 77.5)** — post-turn LLM-driven
  extraction. After each turn, a small LLM call asks "what
  surprised you, what did you learn, what should we remember?"
  and writes structured memory rows.

Defenses:

- **Secret scanner (Phase 77.7)** — regex set blocks Anthropic
  / OpenAI / GitHub / AWS / Stripe / Google / JWT key shapes
  before any memory commit. Fails the commit loud.
- **`AutoMemFilter` (Phase 80.20)** — when a forked sub-agent
  writes memory, the `can_use_tool` whitelist locks `FileEdit` /
  `FileWrite` to paths under `memory_dir`, `Bash` to read-only
  classifier (Phase 77.8/77.9 destructive + sed-in-place
  defenses still apply), `REPL` unrestricted. Defense-in-depth.
- **Memdir relevance scorer (Phase 77.6)** — `relevance × recency
  × access` ranking with age decay so old / unused memories don't
  inflate the working-memory cost.

→ See [Dreaming](../soul/dreaming.md), [Memdir scanner](../ops/memdir-scanner.md)

---

## 3. Self-driving execution loop

When the agent receives work, what runs the loop?

- **Driver-loop (Phase 67)** — replaces a single LLM
  request/response with a multi-turn execution: `read context →
  plan → propose tool calls → run permission gate → execute →
  inspect results → loop`. Goal-scoped, with budget caps on
  turns + time + tokens. Persists to `agent_handles` SQLite so
  every turn survives a daemon restart.
- **Acceptance autodetect (Phase 75)** — at goal completion the
  loop runs an autodetect pass: `cargo build` for Rust,
  `pyproject.toml` build for Python, `npm test` for Node,
  `cmake --build` for CMake, `cargo test --no-run` for cargo.
  Mismatch fails the goal — the agent doesn't claim "done"
  on a broken build.
- **Plan mode (Phase 79.1)** — `EnterPlanMode` toggle puts the
  agent into a read-only mode where it can only call read tools
  + planning advisors (no `Bash`, no `Write`). `ExitPlanMode`
  resolves the plan with operator approval and re-enters the
  full surface.
- **`Sleep { duration_ms, reason }` tool (Phase 77.20)** — the
  agent can decide "no work to do for now, wake me in 20 min"
  without holding a shell process. The runtime intercepts the
  sentinel result, pauses the goal, and schedules a wake-up
  with cache-aware timing (≤ 270 s keeps prompt cache warm,
  ≥ 1200 s amortises a cache miss; avoids the 270-1200 s window
  that pays the miss without benefit).
- **Forked sub-agent infra (Phase 80.19)** — `delegation_tool`
  with `mode: { Sync | ForkAndForget }`. Cache-safe parameters
  (`system_prompt` + `user_context` + `system_context` +
  `tool_use_context` + `fork_context_messages` all five must
  match parent for cache hit). `skipTranscript: true` keeps the
  fork's messages out of the parent's history.

→ See [Acceptance autodetect](../config/acceptance-autodetect.md), [Self-driving guide](../recipes/self-driving.md)

---

## 4. Time-based action

The agent can fire on its own schedule.

- **Heartbeat (Phase 7)** — config-time, per-agent. Every N
  seconds invoke `on_heartbeat()`. Used for proactive messages,
  reminders, periodic state sync.
- **Cron (Phase 79.7)** — LLM-time scheduled fires. The agent
  itself can call `cron_create` to schedule a future task; the
  runtime fires it via `LlmCronDispatcher`. Up to 50 entries
  per binding.
- **Cron jitter cluster (Phase 80.2-80.6)** — six knobs:
  - `enabled` — global killswitch.
  - `recurring_frac` — fraction of next-fire interval used as
    jitter window.
  - `recurring_cap_ms` — absolute cap (5 min default).
  - `one_shot_max_ms` / `one_shot_floor_ms` — backward lead for
    one-shots.
  - `one_shot_minute_mod` — modulus gate (`mod=0` = never jitter
    one-shots).
  - `recurring_max_age_ms` — auto-expire old recurring entries
    (`permanent: true` exempt).
  All hot-reloadable via `Arc<ArcSwap>`. `jitter_frac_from_entry_id`
  derives the offset from the UUID hex prefix so retries don't
  move the target.
- **Boot-time missed-task quarantine** —
  `sweep_missed_entries(skew_ms)` rewrites overdue
  `next_fire_at` to `i64::MAX` so a long-down daemon doesn't
  stampede on the next tick.
- **`agent_turn` poller (Phase 20)** — config-time scheduled LLM
  turn → channel publish. Provider-agnostic; primary use case
  is "every morning at 7am, summarise the inbox and post to
  Slack".
- **Proactive mode (Phase 77.20)** — `proactive: { enabled: true,
  tick_interval_secs, jitter_pct, max_idle_secs }` injects a
  periodic `<tick>` message into the agent's session. The agent
  decides whether to act on it or call `Sleep`. Mutually
  exclusive with `role: coordinator`.

→ See [Cron jitter](../ops/cron-jitter.md), [Proactive mode](../agents/proactive-mode.md)

---

## 5. Communication — every surface the agent can reach

### 5.1. Inbound from the user

- **Pairing (Phase 26)** — every `(channel, account_id)`
  inbound goes through a pairing gate. Senders that haven't
  been allowlisted via `nexo pair seed` get a pairing
  challenge. Per-binding `pairing_policy` + `auto_challenge`
  knobs. Seeded senders survive daemon restarts via
  `PairingStore::list_allow`.
- **WhatsApp / Telegram / email / browser** — first-party
  plugins (Phases 6, 22, plus email + browser CDP). Each is a
  `Channel` impl that maps inbound platform events to
  `agent.intake.<binding>` broker subjects.
- **MCP channels (Phase 80.9)** — any MCP server that declares
  `experimental['nexo/channel']` can push user messages into
  the agent. Provider-agnostic: write a Slack adapter as an MCP
  server and the agent gets Slack inbound for free.
  - 5-step gate: capability + killswitch + per-binding
    allowlist + plugin source verification + approved allowlist.
  - SQLite-backed session registry — Slack threads survive
    daemon restarts.
  - Token bucket rate limit per server.
  - Audit marker `source: "channel:<server>"` in the turn-log.
  - Operator CLI `nexo channel list / doctor / test`.

### 5.2. Outbound to the user

- **`notify_origin` / `notify_channel` hooks** — `Phase 67.F`
  callback shape so the agent can surface mid-goal updates back
  to the originating channel without holding the request open.
- **`send_user_message` tool (Phase 80.8)** — when brief mode
  is active, the agent's visible output flows through this
  tool. `status: "normal"` for replies, `"proactive"` for
  unsolicited surfacings. Free text outside the tool stays
  visible in the detail view.
- **`channel_send` tool (Phase 80.9)** — invoke any MCP channel
  server's outbound tool by name. Configurable
  `outbound_tool_name` per server (default `send_message`).
- **Reminder tool (Phase 7.3)** — schedule a future message to
  any channel.

### 5.3. Inbound from the world

- **Generic webhook receiver (Phase 80.12)** — HTTP receiver
  behind a tunnel. Configure each source by YAML:
  `signature_spec` (HMAC-SHA256/SHA1/raw token) + `event_kind_from`
  (header or body json-path) + `publish_to` (subject NATS).
  Constant-time signature compare via `subtle::ConstantTimeEq`.
  Provider-agnostic: GitHub, Stripe, Calendly, Zapier all in
  YAML.
- **Pollers (Phase 19)** — config-time external endpoint polls.
  Fan-out to per-source NATS subjects.

### 5.4. Multi-agent coordination

- **Peer inbox (Phase 80.11)** — every running goal has a NATS
  subject `agent.inbox.<goal_id>`. `list_peers` returns
  reachable peers (filtered by `allowed_delegates`); `send_to_peer`
  sends a typed `InboxMessage` with `correlation_id`.
- **`InboxRouter` (Phase 80.11.b)** — single broker subscriber
  on `agent.inbox.>`, dashmap per-goal buffers (MAX_QUEUE=64,
  FIFO eviction). Renders `<peer-message from="..." sent_at="..."
  correlation_id="...">` block into the agent's next turn.
- **Teams (Phase 79.6)** — N parallel coordinated agents with a
  shared scratchpad directory. Distinct from `Agent` 1-to-1
  delegation — suited to research fan-out + massive refactors.
- **Delegation tool (Phase 8)** — agent-to-agent routing on
  `agent.route.{target_id}` with `correlation_id`. Sync mode
  awaits the response; ForkAndForget (Phase 80.19) fires the
  delegate without blocking.

→ See [MCP channels](../mcp/channels.md), [Multi-agent coordination](../agents/multi-agent-coordination.md), [AWAY_SUMMARY](../agents/away-summary.md)

---

## 6. Permission + safety

The agent has powerful tools. The safety story is layered.

- **Per-binding capability override (Phase 16)** — each binding
  has its own `EffectiveBindingPolicy` that filters
  `allowed_tools`, rate limits, outbound allowlists, and
  capability gates. Same agent can have a public
  WhatsApp binding (locked-down tool set) AND a private
  Telegram binding (full power).
- **Auto-approve dial (Phase 80.17)** — `auto_approve: true`
  flips skipping the prompt for read-only / scoped-write tools
  while destructive Bash + writes outside workspace +
  ConfigTool + REPL + remote_trigger always ask.
  `is_curated_auto_approve` decision table 25 entries with
  symlink-escape defense + parent-canonicalize fallback for new
  files. `mcp_/ext_` prefix default-ask. Default arm `_ => false`.
- **Capability inventory** — `crates/setup/src/capabilities.rs::INVENTORY`
  registers every dangerous env toggle (`NEXO_DREAM_NOW_ENABLED`,
  `NEXO_KAIROS_REMOTE_CONTROL`, etc). `nexo doctor capabilities`
  surfaces every armed knob.
- **Bash safety (Phase 77.8-77.10)**:
  - Destructive command warning — flags `rm -rf /`-shaped
    invocations.
  - Sed-in-place + path validation — rejects `sed -i` against
    paths outside the workspace.
  - `shouldUseSandbox` heuristic with `bwrap` / `firejail`
    probe.
- **Channel permission relay (Phase 80.9.b)** — `ChannelRelayDecider`
  decorator races the local approval prompt against any channel
  reply (`yes <id>` / `no <id>` from the user's phone). First
  decision wins. 5-letter ID alphabet a-z minus `l`
  (anti-confusable); substring blocklist for offensive combos.
  Local prompt always runs in parallel — channel approval is a
  **second** surface, never a replacement.
- **Setup doctor** — `nexo setup doctor` audits `(channel,
  account_id)` tuples, capability gates, dispatch policy
  consistency, pairing allowlist coverage.

→ See [Auto-approve dial](../agents/auto-approve.md), [Capability toggles](../ops/capabilities.md), [Bash safety knobs](../ops/bash-safety.md)

---

## 7. Audit + observability

Everything the agent does leaves a trail.

- **Turn-level audit log (Phase 72)** — every driver-loop
  `AttemptResult` writes a row to `goal_turns` SQLite table:
  outcome, decision text, summary, diff_stat, error,
  raw_json, plus the channel `source` marker. 1000-row tail
  cap. Idempotent on `(goal_id, turn_index)` so a replay
  doesn't corrupt history.
- **`agent_turns_tail goal_id=<uuid> [n=20]` tool** — read
  tool that surfaces the last N turns of a goal as a markdown
  table. Post-mortem debug surface.
- **DreamTask audit (Phase 80.18)** — `dream_runs` SQLite table
  joined to `goal_id` with `status`, `phase`, `sessions_reviewing`,
  `files_touched (JSON)`, `prior_mtime_ms`, `started_at`,
  `ended_at`. `dream_runs_tail` LLM tool. `nexo agent dream
  tail/status/kill` CLI.
- **Agent registry persistence (Phase 71)** — `agent_handles`
  SQLite table tracks every Running / completed / aborted goal.
  Survives daemon restarts.
- **Channel turn-log marker (Phase 80.9.h)** — channel-driven
  turns write `source: "channel:<server>"`. Single SQL filter
  answers "what came in via Slack today?".
- **Prometheus metrics (Phase 9.2)** — counters + gauges per
  agent / per binding / per tool / per channel. `health.bind`
  YAML key wires the scrape endpoint.
- **Tracing logs** — every gate / every dispatch / every retry
  emits a `tracing::info!` or `warn!` with structured fields
  (server, binding, kind, reason, error). Operator-readable.
- **Config-changes log (Phase 79.10)** — when ConfigTool
  mutates YAML, a row lands in `config_changes` table with
  patch_id, actor_origin, allowed paths.

→ See [Logging](../ops/logging.md), [Metrics](../ops/metrics.md), [Turn-level audit log](../architecture/turn-log.md)

---

## 8. Operator surface

The CLI commands a human runs to drive / debug / observe the
agent:

| Command | What it does |
|---------|-------------|
| `nexo run --config config/agents.yaml` | Daemon entrypoint |
| `nexo agent run [--bg] "<prompt>"` | Spawn a goal |
| `nexo agent ps [--all] [--kind=...]` | List running goals |
| `nexo agent attach <goal_id>` | Snapshot of a goal |
| `nexo agent discover [--include-interactive]` | List discoverable goals |
| `nexo agent dream tail/status/kill` | DreamTask audit + control |
| `nexo channel list/doctor/test` | MCP channels surface |
| `nexo pair list/seed/start/revoke` | Pairing gate management |
| `nexo flow list/show/cancel/resume` | TaskFlow runtime |
| `nexo setup` | Interactive wizard |
| `nexo setup doctor` | Configuration audit |
| `nexo setup migrate --dry-run/--apply` | Schema migrations |
| `nexo doctor capabilities` | Env toggle inventory |
| `nexo ext install/list/uninstall/run` | Extension management |
| `nexo mcp-server` | Run nexo as an MCP server |

→ See [CLI reference](../cli/reference.md)

---

## 9. End-to-end use case

This is the kind of workflow the autonomous agent is built for.

**Scenario**: a marketing-agent named `kate` runs as a daemon
process, paired with the operator's Slack workspace + Telegram
account. It manages the editorial calendar and replies to user
queries during business hours.

```yaml
agents:
  - id: kate
    model:
      provider: anthropic
      model: claude-sonnet-4-5
    plugins: [memory, browser, web_search]
    assistant_mode:
      enabled: true
    auto_approve: true
    proactive:
      enabled: true
      tick_interval_secs: 1800   # check in every 30 min
      max_idle_secs: 86400
    auto_dream:
      enabled: true
    channels:
      enabled: true
      approved:
        - server: slack
        - server: telegram
    inbound_bindings:
      - plugin: telegram
        instance: kate_tg
        allowed_channel_servers: [slack, telegram]
        auto_approve: true
        dispatch_policy:
          mode: full
```

What happens at runtime:

1. **Boot** — `nexo run` spawns `kate` as a daemon. The daemon
   reads the YAML, validates, opens broker, opens SQLite stores
   (memory, agent registry, dream runs, turn log, channel
   sessions, pairing). Connects the configured MCP servers.
   Spawns a `ChannelInboundLoop` per `(binding, server)` plus a
   single `ChannelBridge` per process. Wraps the inner
   permission decider in `ChannelRelayDecider`.
2. **First Slack DM** — `alice` writes "¿qué publicamos hoy?"
   in Slack thread `1700000000.000`. The Slack MCP server emits
   `notifications/nexo/channel`. The runtime parses, derives
   `session_key = "slack|thread_ts=1700000000.000"`, resolves a
   fresh `session_uuid`, persists it in
   `mcp_channel_sessions.sqlite`, hands off the
   `<channel source="slack" thread_ts="1700000000.000">` to the
   intake. Pairing gate verifies `alice` is allowlisted (or
   challenges her).
3. **Agent decides** — the LLM reads recent context (long-term
   memory + transcripts), decides to look up the calendar.
   Calls `Bash(python check_calendar.py)`. Auto-approve flips
   the prompt away because the path is read-only and inside the
   workspace.
4. **Reply** — agent calls `channel_send(server: "slack",
   content: "Tenemos pendiente el blog post de Q2",
   arguments: { thread_ts: "1700000000.000" })`. The runtime
   resolves the outbound tool name from the registered server's
   snapshot and invokes it through the MCP runtime. Slack MCP
   server posts to the Slack API.
5. **Cron fires at 8 PM** — `cron_create` from a previous turn
   scheduled a daily summary. Cron runner picks it up,
   dispatches an LLM turn through `LlmCronDispatcher`. Output
   goes to the operator's Telegram via `notify_channel`.
6. **Risky tool prompt** — the agent decides to schedule an
   email blast. The local approval prompt opens; in parallel
   the runtime emits `notifications/nexo/channel/permission_request`
   to both Slack and Telegram. Operator's phone shows
   `Approve "Schedule email blast?" — yes abcde / no abcde`.
   Operator types `yes abcde` in Telegram; Telegram MCP server
   parses, emits `notifications/nexo/channel/permission`.
   `ChannelRelayDecider` wins the race, returns `AllowOnce`.
   Email sends.
7. **Operator sleeps** — agent keeps running. Receives Slack
   DMs from team members; replies through the same threads.
   Cron tasks fire on schedule. Memory consolidates at midnight
   via `auto_dream`.
8. **Daemon restart** — operator pushes a new YAML, the watcher
   detects, validates, swaps via Phase 18 `ArcSwap`. The
   `ChannelRegistry::reevaluate` pass evicts handlers that no
   longer pass the gate. SQLite stores survive. When alice
   writes again in the same Slack thread, the agent reattaches
   to the same session — the bot doesn't re-introduce itself.
9. **Operator returns after 12 h silence** — first inbound
   triggers the AWAY_SUMMARY digest. Agent composes a markdown
   report of the past 12 h: 14 channel messages handled, 2
   permission prompts approved, 1 cron fire completed. Sent
   before processing the operator's actual message.
10. **Operator audits** — `agent_turns_tail goal_id=<uuid>
    n=50` shows every decision the agent made in the last 50
    turns. `nexo channel doctor` validates the YAML against
    the gate. `nexo agent dream tail` shows last consolidations.

The operator never sat at a terminal during steps 5-9. The
agent is autonomous within the bounds of the YAML.

---

## 10. Provider-agnostic by design

Every autonomous behaviour works against any LLM provider:

- **MiniMax M2.5** (primary)
- **Anthropic Claude** (subscription OAuth, API key, or Claude
  Code import)
- **OpenAI-compat** providers
- **Gemini**
- **Local llama.cpp** (Phase 68 backlog — model-agnostic GGUF
  loader for tier-0 inference)

The `LlmClient` trait is the abstraction. No autonomous feature
hard-codes a provider; everything routes through the registry +
binding-level provider selection.

Channels work the same way: any MCP server that follows the
protocol becomes a channel, regardless of which platform it
adapts.

Pollers, webhooks, and channel adapters are all data-driven via
YAML — operators don't write per-provider Rust to add a new
external surface.

---

## 11. Code map — where each capability lives

| Capability | Crate / file | Tests |
|------------|-------------|-------|
| Driver-loop | `crates/driver-loop/` | + integration tests |
| Permission decider | `crates/driver-permission/src/decider.rs` | inline |
| Auto-approve dial | `crates/driver-permission/src/auto_approve.rs` | 27 |
| Channel relay decorator | `crates/driver-permission/src/channel_relay.rs` | 8 |
| Bash safety | `crates/driver-permission/src/bash_destructive.rs` | 19 |
| Long-term memory | `crates/memory/src/long_term.rs` | inline |
| Memdir relevance scorer | `crates/memory/src/memdir/` | inline |
| Secret guard | `crates/memory/src/secret_guard.rs` | inline |
| autoDream runner | `crates/dream/` | 67 |
| Cron schedule + jitter | `crates/core/src/cron_schedule.rs` | 80 |
| Channels gate + parser + bridge | `crates/mcp/src/channel*.rs` | 109 |
| Channel session store | `crates/mcp/src/channel_session_store.rs` | 9 |
| Channel permission relay | `crates/mcp/src/channel_permission.rs` | 27 |
| Channel boot helpers | `crates/mcp/src/channel_boot.rs` | 5 |
| Channel LLM tools | `crates/core/src/agent/channel_*_tool.rs` | 21 |
| Pairing | `crates/pairing/` | inline |
| TaskFlow | `crates/taskflow/` | inline |
| Agent registry persistence | `crates/agent-registry/` | 51 |
| Turn-level audit log | `crates/agent-registry/src/turn_log.rs` | 9 |
| Inbox router | `crates/core/src/agent/inbox*.rs` | 17 |
| Webhook receiver | `crates/webhook-receiver/` | 33 |
| Forked sub-agent | `crates/fork/` | 42 |
| Driver / runtime hookup | `src/main.rs` | smoke |

Total channel-related lib tests: **168 verde** spread across 5
crates. Workspace-wide tests count is much larger; see the
phase-specific docs for the per-feature breakdown.

---

## 12. What's NOT done yet

Honest list of polish items still backlogged:

- **Sample MCP channel server fixture** — `extensions/sample-channel-server/`
  reference impl so operators can wire a fake channel quickly
  without writing an MCP server from scratch. ~200 LOC,
  high educational value, no functional impact.
- **Setup wizard panel for channels** — `nexo setup → Configurar
  agente → Channels` interactive opt-in. UX nice-to-have.
- **Live-runtime channel doctor** — current `nexo channel
  doctor` is static against YAML. Live version that consults
  the active `ChannelRegistry` via NATS to show what's actually
  registered in the running daemon.
- **`channel_history` LLM tool** — tail of the turn-log
  filtered by `source: "channel:<server>"`, useful for the
  agent to ask itself "what did Slack send today".
- **Phase 67.10–67.13** — escalation-to-channel paths for
  driver-loop are largely subsumed by `notify_origin` /
  `notify_channel` already. Remaining tickets in `PHASES.md`.
- **Phase 68 Local LLM tier (llama.cpp)** — 15 sub-phases for
  tier-0 inference (PII / embeddings / poller pre-filter /
  classifiers / fallback). Planned to run on Termux ARM CPU +
  desktop CPU/GPU.

None of these block the autonomous agent's current capabilities.

---

## 13. Where to go next

- **Setting up your first autonomous agent** →
  [Quick start](../getting-started/quickstart.md) +
  [Setup wizard](../getting-started/setup-wizard.md).
- **Deep dive on assistant mode + auto-approve** →
  [Assistant mode overview](./assistant-mode.md).
- **MCP channels specifically** → [MCP channels](../mcp/channels.md).
- **Multi-agent coordination patterns** →
  [Multi-agent coordination](./multi-agent-coordination.md).
- **Audit + observability stack** → [Logging](../ops/logging.md)
  + [Metrics](../ops/metrics.md) + [Turn-level audit log](../architecture/turn-log.md).
- **Phase tracking** — `PHASES.md` at repo root has the
  exhaustive sub-phase status (✅ MVP / ⬜ open / DEFERRED).
