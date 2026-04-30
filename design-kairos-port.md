# Phase 80 — KAIROS port design appendix

> Single source of truth for sub-phases 80.1–80.21. Each sub-phase
> cites this file rather than re-mining the leak. Update this file
> in lockstep with the sub-phase entries in
> [`PHASES.md`](PHASES.md#phase-80--kairos-autonomous-assistant-mode-parity-).

This appendix catalogues every `claude-code-leak/` file Phase 80
touches (leak path:line, nexo path:line, what it does, what we already
have, what is missing, which sub-phase covers it). Inventory was
produced by deep-mining the leak in two passes — the first identified
the KAIROS feature-flag tree, the second read the surviving file
bodies (KAIROS-gated TypeScript modules where the feature() call
survived dead-code-elimination).

Document version: 1.0 · Created: 2026-04-30

---

## 1 — KAIROS in one paragraph

KAIROS (Greek: "the right time") is the leak's name for an autonomous
"assistant mode" that turns the Claude Code CLI from a foreground REPL
into an always-on background daemon. The daemon wakes on cron, GitHub
webhooks, or ad-hoc inbound messages; runs forked LLM turns that share
the parent prompt cache; consolidates memory while idle (auto-dream);
emits structured push notifications to the operator; routes through
arbitrary MCP "channel" servers (Slack/Discord/SMS); and silences free
text in favour of explicit `SendUserMessage` checkpoints (brief mode).
A second operator can attach to a running daemon via
`claude assistant <session-id>` and resume the conversation as if they
were the original initiator.

## 2 — Out-of-scope (intentional divergences)

| Leak feature | Out-of-scope reason | nexo replacement |
|---|---|---|
| GrowthBook flag namespace (`tengu_kairos_*`) | Anthropic-internal A/B harness | Phase 18 hot-reload + per-binding policy |
| Unix domain sockets (`UDS_INBOX`) | Mac/Linux single-host hack | NATS subjects (`agent.inbox.<goal_id>`) — Phase 8 + 67 already shipped |
| Cloud session-history API (`/v1/sessions/{id}/events`, `ccr-byoc-2025-07-29`) | Requires Anthropic backend | Phase 72 SQLite turn log + `agent_turns_tail` (offline, already shipped) |
| Ink UI components (Spinner, StatusLine, etc.) | TS+Ink, not portable | Companion-tui (Phase 26 deferred) + admin-ui |
| Voice/TTS/STT (`src/voice/`) | Not a KAIROS pillar | Phase 23 backlog |
| GrowthBook itself (`src/services/analytics/growthbook.ts`) | Proprietary | Phase 18 hot-reload covers the same incident-lever shape |

## 3 — KAIROS pillar map

```
KAIROS
├── Activation (assistant module)
│   ├── kairosEnabled runtime flag                ──→ 80.15 + 80.17
│   ├── _pendingAssistantChat argv preprocessing  ──→ 80.16
│   └── getAssistantSystemPromptAddendum()        ──→ 80.15
├── Background work
│   ├── autoDream (forked /dream)                 ──→ 80.1 + 80.18 + 80.19 + 80.20
│   ├── Cron + jitter + durable                   ──→ 80.2-80.7
│   └── Sleep tool ticks (already shipped)        ──→ Phase 77.20 ✅
├── Comms
│   ├── KAIROS_CHANNELS (MCP claude/channel)      ──→ 80.9
│   ├── BG sessions + multi-session inbox         ──→ 80.10 + 80.11
│   ├── KAIROS_GITHUB_WEBHOOKS                    ──→ 80.12
│   └── KAIROS_PUSH_NOTIFICATION                  ──→ 80.13
├── UX
│   ├── Brief mode + SendUserMessage              ──→ 80.8 (re-spec of 79.9)
│   ├── nexo agent attach / discover              ──→ 80.16
│   └── AWAY_SUMMARY                              ──→ 80.14
└── Closeout
    ├── DreamTask audit row                       ──→ 80.18
    └── Docs + admin-ui sync                      ──→ 80.21
```

## 4 — Leak ↔ nexo file inventory

### 4.1 — autoDream

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/services/autoDream/autoDream.ts` | 324 | `crates/core/src/agent/dreaming.rs` | 515 | nexo is scoring-based; needs forked-agent control flow |
| `src/services/autoDream/consolidationLock.ts` | 140 | none | — | new |
| `src/services/autoDream/consolidationPrompt.ts` | 65 | none | — | new (verbatim port) |
| `src/services/autoDream/config.ts` | 22 | embedded in `dreaming.rs` | — | partial |
| `src/utils/forkedAgent.ts` (`runForkedAgent`, `createCacheSafeParams`) | est. 600 | `crates/core/src/agent/delegation_tool.rs` | 115 | delegation is sync-only, needs fire-and-forget + skipTranscript |
| `src/tasks/DreamTask/DreamTask.ts` | est. 250 | none | — | new (audit row) |
| `src/services/extractMemories/extractMemories.ts:171-222` | (slice) | `crates/driver-loop/src/extract_memories.rs` | 1103 | nexo extract is post-turn inline; need to factor out the canUseTool whitelist |

### 4.2 — Cron

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/utils/cronScheduler.ts` | 565 | `crates/core/src/cron_runner.rs` | 639 | runner exists; needs killswitch + missed-task surfacing |
| `src/utils/cronTasks.ts` | 458 | `crates/core/src/cron_runner.rs` (same file) | — | needs `permanent` flag + `recurringMaxAgeMs` exemption + jittered* fns |
| `src/utils/cronJitterConfig.ts` | 78 | `crates/config/src/types/` | — | new — 6-knob hot-reload |
| `src/tools/ScheduleCronTool/prompt.ts` | 135 | `crates/core/src/agent/cron_tool.rs` | 626 | tool MVP shipped (Phase 79.7); needs runtime firing + durable persistence wired |
| `.scheduler-lock` (cwd-scoped) | — | none | — | DEFER until Phase 32 |

### 4.3 — Brief mode

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/tools/BriefTool/BriefTool.ts` | 204 | none | — | Phase 79.9 was opened as terse-toggle, needs re-spec |
| `src/commands/brief.ts` | 130 | none | — | new slash command + state |

### 4.4 — Channels (KAIROS_CHANNELS)

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/services/mcp/channelNotification.ts` | 316 | `crates/mcp/` | — | new capability + 7-step gate |
| `src/components/LogoV2/ChannelsNotice.tsx` | — | none | — | UI-only, skip per out-of-scope |
| `src/bootstrap/state.ts::setAllowedChannels` | — | `crates/core/src/runtime_snapshot.rs` | — | extend with `allowed_channels: Vec<ChannelEntry>` |

### 4.5 — BG sessions + multi-session inbox

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/utils/concurrentSessions.ts` | 204 | `crates/agent-registry/` | — | needs `kind: SessionKind` enum + ps/attach |
| `src/utils/conversationRecovery.ts:480-560` | (slice) | Phase 71 reattach | — | extend with kind-aware filter |
| `src/utils/udsClient.ts` (DCE'd, signature visible) | — | `crates/core/src/team_message_router.rs` | 365 | replaced by NATS subjects |
| `src/tools/SendMessageTool/SendMessageTool.ts:586,631,658,685,742` (`bridge://` scheme) | (slice) | none | — | new SendToPeer + ListPeers tools |

### 4.6 — Webhooks (KAIROS_GITHUB_WEBHOOKS)

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/tools.ts:48` (`SubscribePRTool` gate) | — | none | — | full new plugin |
| `src/commands.ts:101` (`subscribe-pr` slash) | — | none | — | full new |
| `src/tools/SubscribePRTool/` (DCE'd body) | — | `crates/poller/src/builtins/webhook_poll.rs` | 345 | poll-only sibling; need the *receive* side |

### 4.7 — Push notifications (KAIROS_PUSH_NOTIFICATION)

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/tools.ts:45-50` (`PushNotificationTool` gate) | — | none | — | full new |
| `src/tools/PushNotificationTool/` (DCE'd body) | — | none | — | new APN/FCM/WebPush trait |
| `notify_origin` / `notify_channel` hooks | — | `crates/dispatch-tools/` | — | reuse channel of origin; push is distinct |

### 4.8 — AWAY_SUMMARY

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `feature('AWAY_SUMMARY')` integration point (DCE'd) | — | none | — | design from scratch using extractMemories pattern |
| pairing `last_seen_at` | — | `crates/pairing/` | — | needs schema column + on-inbound trigger |

### 4.9 — Assistant module + remote control

| Leak path | LOC | nexo equivalent | LOC | Gap |
|---|---|---|---|---|
| `src/main.tsx:1058-1088` (activation) | (slice) | `crates/driver-loop/`, `crates/core/src/agent/effective.rs` | — | extend binding policy with `assistant_mode: bool` |
| `src/main.tsx:1075` (`isKairosEnabled`) | (slice) | none | — | derive from binding flag (no GrowthBook) |
| `src/main.tsx:2206-2208` (system prompt addendum) | (slice) | `crates/llm/src/prompt_assembly.rs` | — | append addendum when active |
| `src/main.tsx:2916` (`fullRemoteControl`) | (slice) | `crates/driver-permission/` | — | new `kairos_remote_control` flag |
| `src/main.tsx:559,685-694,3259-3340` (`_pendingAssistantChat`) | (slice) | none | — | `nexo agent attach <goal_id>` + `nexo agent discover` CLIs |
| `src/main.tsx:3035` (`assistantTeamContext` precedence) | (slice) | `crates/core/src/team_message_router.rs` | 365 | `initializeAssistantTeam()` equiv |

## 5 — Decisions log

### D-1 · Keep scoring sweep alongside fork

`crates/core/src/agent/dreaming.rs` (515 LOC, Phase 10.6) implements
deterministic recall-signal weighted scoring with promotion ledger.
Leak's autoDream is a forked LLM turn with /dream prompt. **They
complement, not replace.**

- Light pass = scoring sweep, fires every turn, cheap (one SQLite query).
- Deep pass = forked /dream turn, fires every ≥ 24 h when ≥ 5 sessions
  accumulated, expensive (full LLM turn).

The scoring sweep produces the candidate set; the deep pass actually
edits memory files based on the candidates plus transcript scan. We
merge gates: scoring sweep marks "promotion candidate", deep pass
includes those in the prompt extra block alongside the session list.

### D-2 · NATS subjects supersede UDS

KAIROS uses `bridge://` URIs over a Unix domain socket (`UDS_INBOX`).
nexo already has `agent.route.<id>` (Phase 8) + `agent.inbox.<goal_id>`
(Phase 67) over NATS. NATS gives us multi-host, persistence,
fan-out, observability — all things UDS lacks. Sub-phase 80.11
formalises the subject contract and adds the LLM-facing
`ListPeers` + `SendToPeer` tools that ride on top.

### D-3 · No GrowthBook port

Every `tengu_kairos_*` flag in the leak maps to either:

- A **per-binding** YAML knob (already covered by Phase 16).
- A **runtime snapshot** field reloaded by Phase 18 watcher.
- A **boot-time** check in `setup doctor`.

No need for a separate flag service. Operators get the same incident
levers (ramp/kill/jitter) through `nexo setup migrate` + hot-reload.

### D-4 · `kairos_remote_control` is a dial within the gate, not a bypass

KAIROS's `fullRemoteControl` (`main.tsx:2916`) effectively bypasses
approval prompts for a curated tool subset. We refuse to make this a
gate bypass: Phase 16 capability gate stays *authoritative*.
`kairos_remote_control: true` only flips the auto-approve dial within
what the gate already permits. `setup doctor` warns when
`assistant_mode: true` AND `kairos_remote_control: false` (likely
misconfiguration).

### D-5 · DreamTask row joins to goal_id, not a parallel table

KAIROS's `DreamTask` is its own task type alongside `agentTask`. nexo
already has `goal_id` as the universal handle (Phase 67) — adding a
parallel "dream" handle would fork the audit/observability surface.
Instead we add a `dream_runs` row with `FK goal_id` joined to the
forked goal's `agent_handles` row. Same `agent_turns_tail` machinery
surfaces dream turns.

### D-6 · Push is a distinct channel from `notify_origin`

KAIROS `PushNotificationTool` is gated separately from the channel
of origin. We mirror: `notify_origin` re-uses the inbound conversational
channel (WA/TG/email). `notify_push` is a one-way mobile alert via
APN/FCM/WebPush. Operator decides per-binding which one fires when a
goal completes (or both).

### D-7 · Brief mode is a SendUserMessage gate, not a terse toggle

Phase 79.9 was opened with the wrong shape (terse-mode toggle).
Re-spec in 80.8: brief mode hides free-text output entirely; the only
way to render to the operator is the `SendUserMessage` tool. This
matches the leak's pattern and produces *checkpoints* rather than
chatty output — much better fit for long-running daemons.

### D-8 · Forked subagent is fire-and-forget by default

KAIROS `runForkedAgent` waits for completion (sync). We split into two
modes (sub-phase 80.19):

- `Sync` — current `delegate` semantics, parent waits.
- `ForkAndForget` — parent gets `goal_id`, child runs in background,
  child writes own memory commits (with auto-mem whitelist 80.20).

autoDream uses `ForkAndForget`. AWAY_SUMMARY uses `ForkAndForget`.
Future eval harness (Phase 51) uses `ForkAndForget`. Existing
delegation paths stay `Sync` by default.

### D-9 · `assistant_mode: true` implies a configuration bundle

When operator sets `assistant_mode: true` on a binding, the following
defaults flip on automatically (overridable individually):

- `brief.default_on: true`
- `cron.enabled: true`
- `kairos_remote_control: false` (still requires explicit opt-in for
  auto-approve — D-4)
- `proactive.enabled: true` (Phase 77.20)
- `auto_dream.deep.enabled: true` (80.1)
- `team.auto_spawn: true` (initial team from binding's `team:` block)

`setup doctor` lists these implications when the operator toggles
`assistant_mode`.

### D-10 · 80.7 deferred until Phase 32

Per-cwd `.scheduler-lock` matters when multiple daemons share an
agents.yaml. nexo today is single-daemon; multi-host orchestration is
Phase 32. 80.7 is opened with `DEFERRED` so the design is recorded,
but no implementation work happens until Phase 32 is in flight.

## 6 — Per sub-phase appendix

### 80.0 — Surface inventory

This document. Single source of truth. Update before any 80.x
brainstorm.

### 80.1 — autoDream fork-style consolidation

**Leak primary**: `services/autoDream/autoDream.ts:1-324`,
`consolidationLock.ts:1-140`, `consolidationPrompt.ts:1-65`,
`config.ts:1-22`. Cite line ranges in the brainstorm output.

**nexo integration**: Extend `crates/core/src/agent/dreaming.rs` with a
`deep_pass_via_fork()` entry point that the `cron_runner` fires every
`min_hours / 4`. Reuse `crates/memory/` write paths for idempotency.

**Critical invariants**:
- Lock mtime IS `lastConsolidatedAt`. One stat per turn, no extra
  bookkeeping.
- Lock body is the holder's PID. Stale = dead PID OR `now - mtime ≥ 1 h`.
- Rollback is idempotent (kill + fail double-rollback is a no-op).
- Forked goal's tool whitelist is ENFORCED by 80.20; this sub-phase
  consumes the whitelist but doesn't define it.
- `priorMtime: 0` on fresh acquire → rollback unlinks the file. Other
  rollback paths re-set utimes.

**Done criteria highlights** (full list in PHASES.md):
- 4 unit tests + 1 integration covering parallel-acquire race.

### 80.2 — Cron jitter 6-knob hot-reload config

**Leak primary**: `utils/cronJitterConfig.ts:1-78`,
`utils/cronTasks.ts:286-333` (defaults).

**nexo integration**: New
`crates/config/src/types/cron_jitter.rs::CronJitterYaml`.
`agents.yaml::cron.jitter` block. ConfigReloadCoordinator pushes
into `Arc<ArcSwap<CronJitterConfig>>` consumed by `CronRunner` each
tick.

**Knob bounds**:
| Knob | Min | Max | Default |
|---|---|---|---|
| `recurring_frac` | 0.0 | 1.0 | 0.05 |
| `recurring_cap_ms` | 0 | 30 min | 15 min |
| `one_shot_max_ms` | 0 | 30 min | 90 s |
| `one_shot_floor_ms` | 0 | `one_shot_max_ms` | 0 |
| `one_shot_minute_mod` | 1 | 60 | 30 |
| `recurring_max_age_ms` | 0 | 30 days | 30 days |

Validation rejects whole config on any out-of-range value (defence in
depth).

### 80.3 — Cron task-id-derived deterministic jitter

**Leak primary**: `utils/cronTasks.ts:381-398`.

```rust
fn jitter_frac(entry_id: &str) -> f64 {
    let prefix: &str = entry_id.get(..8).unwrap_or(entry_id);
    let n = u32::from_str_radix(prefix, 16).unwrap_or(0);
    n as f64 / u32::MAX as f64
}
```

Important: when `entry_id` is shorter than 8 chars or is non-hex, fall
back to `0` (deterministic, but means same retry path — acceptable
since taskId is a UUID slice in practice).

### 80.4 — Cron one-shot vs recurring jitter modes

**Leak primary**: `utils/cronTasks.ts:381-445`.

```rust
fn jittered_next_recurring(cron: &Cron, from_ms: i64, id: &str, cfg: &CronJitterConfig) -> i64 {
    let t1 = next_cron_run_ms(cron, from_ms);
    let t2 = match next_cron_run_ms_after(cron, t1) {
        Some(t) => t,
        None => return t1,  // pinned date — no herd risk
    };
    let span_ms = (t2 - t1) as u64;
    let jitter = (jitter_frac(id) * cfg.recurring_frac as f64 * span_ms as f64) as u64;
    t1 + jitter.min(cfg.recurring_cap_ms) as i64
}

fn jittered_next_one_shot(cron: &Cron, from_ms: i64, id: &str, cfg: &CronJitterConfig) -> i64 {
    let t1 = next_cron_run_ms(cron, from_ms);
    let minute = (t1 / 60_000) % 60;
    if (minute as u8) % cfg.one_shot_minute_mod != 0 { return t1; }
    let span = (cfg.one_shot_max_ms - cfg.one_shot_floor_ms) as f64;
    let lead = cfg.one_shot_floor_ms + (jitter_frac(id) * span) as u64;
    (t1 - lead as i64).max(from_ms)
}
```

### 80.5 — Cron `permanent` flag + age sweep exemption

**Leak primary**: `utils/cronTasks.ts` (`permanent` field + auto-expiry sweep — line numbers vary; locate in 80.0 inventory pass).

```rust
pub struct CronEntry {
    // existing fields
    pub permanent: bool,  // default false
}
```

`prune_old_entries(now, cfg)` skips rows with `permanent == true`.
Built-ins registered at boot (catch-up, morning-checkin, dream)
set `permanent: true`.

### 80.6 — Cron killswitch + missed-task surfacing

**Leak primary**: `utils/cronScheduler.ts:230-260` + `cronTasks.ts:193-227`.

**Killswitch**: `RuntimeSnapshot::cron_enabled: bool` read each tick.
`false` halts in-flight schedulers (next tick is a no-op). Per-binding
override `agents.<id>.cron.enabled: false`.

**Missed-task surfacing** (boot-only, NOT on file changes):
1. Scan `cron_store::list_one_shots()` for rows with
   `next_fire < now - SAFETY_MARGIN` (default 5 min).
2. Emit `notify_origin` per row: `[catch-up] N tasks missed while offline`.
3. Atomically `update next_fire = i64::MAX` (poison value preventing
   the post-load tick from re-firing).

Use `SAFETY_MARGIN` to avoid catching tasks that fire in the next 5 min
naturally during boot startup.

### 80.7 — Cron per-cwd lock owner [DEFERRED]

**Leak primary**: `utils/cronScheduler.ts:406-436`.

Marked `DEFERRED` until Phase 32 multi-host orchestration. Design
recorded for completeness:

- File: `<cron_store_dir>/.scheduler-lock` with PID body.
- Non-owner polls 5 s for takeover.
- Take over when PID dead OR `now - mtime > 30 min`.
- Lock release on graceful shutdown (Phase 71 SIGTERM drain hook).

### 80.8 — Brief mode + SendUserMessage tool [re-spec of 79.9]

**Leak primary**: `tools/BriefTool/BriefTool.ts:1-204`,
`commands/brief.ts:1-130`.

**Tool API**:
```rust
pub struct SendUserMessageInput {
    message: String,
    attachments: Option<Vec<String>>,    // file paths or URIs
    status: BriefStatus,                  // Normal | Proactive
}

pub enum BriefStatus { Normal, Proactive }

pub struct SendUserMessageOutput {
    message: String,
    attachments: Option<Vec<AttachmentMeta>>,
    sent_at: DateTime<Utc>,
}
```

**Activation predicate**:
```
is_brief_enabled = entitled
                 && (kairos_active || user_msg_opt_in)
```

Where:
- `entitled` = build-time + binding `brief.entitled` (default true if
  feature flag on; lets ops disable per-binding without removing the
  tool).
- `kairos_active` = `assistant_mode: true` on binding (D-9).
- `user_msg_opt_in` = `--brief` CLI flag OR `/brief` slash OR
  `agents.<id>.brief.default_on: true`.

**Refresh interval**: 5 min (binding policy via Phase 18 hot-reload).

**/brief slash command**:
- Toggle `brief_only` on the active goal.
- Inject system reminder on next turn:
  - On: `Use SendUserMessage tool for all user output — plain text is hidden.`
  - Off: `SendUserMessage unavailable — reply with plain text.`

**Channel-adapter integration**: WA/TG `PairingChannelAdapter` checks
`brief_only`: drops free-text messages, forwards `SendUserMessage`
payloads (with attachment thumbnails when supported).

### 80.9 — KAIROS_CHANNELS — MCP channels routing

**Leak primary**: `services/mcp/channelNotification.ts:1-316`.

**Capability discovery**:
```jsonc
// MCP server manifest
{
  "capabilities": {
    "experimental": {
      "claude/channel": {}  // truthy = is a channel server
    }
  }
}
```

**Inbound wrapping**:
```xml
<channel source="slack-prod" thread_id="C123" user="U456">
  Body of the inbound message
</channel>
```
Meta-key validation: `^[a-zA-Z_][a-zA-Z0-9_]*$` (anti
attribute-injection). Unknown keys dropped, not errored.

**Permission flow** (structured, no string parsing):
1. Daemon emits `ChannelPermissionRequestParams` to server.
2. Server formats per platform (Discord embed, iMessage rich text).
3. User taps approve/deny.
4. Server emits `ChannelPermissionNotification` (Approve | Deny + auth
   code echo).
5. Daemon matches auth code against issued request, applies decision.

**7-step gate** (port verbatim from leak — order matters):
1. **Capability**: `capabilities.experimental['claude/channel']` truthy.
2. **Runtime gate**: per-binding `channels.enabled` (defaults true if
   binding has `assistant_mode: true`, else false).
3. **Auth**: OAuth-only path (matches subscription auth from 15.x).
4. **Org policy**: `agents.<id>.channels.allowed_servers` allowlist
   when binding has `org_managed: true`.
5. **Session allowlist**: CLI `--channels plugin:slack@anthropic
   server:foo` parses into `RuntimeSnapshot::allowed_channels`.
6. **Plugin marketplace verification**: declared `marketplace` matches
   installed source (cite Phase 31 marketplace work — gate via that or
   defer to a follow-up).
7. **Allowlist final check**: `allowed_channel_plugins` setting OR dev
   override (`NEXO_DANGEROUSLY_LOAD_DEV_CHANNELS=true`).

### 80.10 — SessionKind + BG sessions

**Leak primary**: `utils/concurrentSessions.ts:1-204`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "kebab-case")]
pub enum SessionKind {
    Interactive,
    Bg,
    Daemon,
    DaemonWorker,
}
```

**Schema migration v3** (Phase 77.17 migration system):
```sql
ALTER TABLE agent_handles ADD COLUMN kind TEXT NOT NULL DEFAULT 'interactive';
ALTER TABLE agent_handles ADD COLUMN name TEXT;
ALTER TABLE agent_handles ADD COLUMN log_path TEXT;
```

**CLI surface**:
- `nexo agent run --bg <prompt>` — detached, prints goal-id, returns 0.
- `nexo agent ps [--all] [--kind=bg]` — list with kind, channel,
  prompt-summary, age.
- `nexo agent attach <goal_id>` — re-attach TTY (80.16).
- `nexo agent kill <goal_id>` — graceful + drain (Phase 71).

### 80.11 — Agent inbox subject + ListPeers + SendToPeer

**Leak primary**: `utils/concurrentSessions.ts:86`,
`tools/SendMessageTool/SendMessageTool.ts:586,631,658,685,742`.

**NATS subject contract**:
- `agent.inbox.<goal_id>` — point-to-point inbox to a specific goal.
- `agent.events.<goal_id>` — stream of goal events (Phase 67).
- `agent.broadcast.<binding_id>` — fan-out to all goals in a binding.

**`ListPeers` LLM tool**:
```jsonc
// returns
[
  {
    "goal_id": "uuid",
    "kind": "bg",
    "channel": "whatsapp:+15551234",
    "name": "morning-checkin",
    "age_secs": 1234,
    "prompt_summary": "Check overnight CI runs"
  }
]
```

**`SendToPeer` LLM tool**:
```jsonc
// args
{ "goal_id": "uuid", "message": "...", "status": "normal" }
// returns
{ "delivered_at": "2026-04-30T...", "ack": true }
```

**`nexo agent peers` CLI**: alias of `agent ps --me=false`.

### 80.12 — KAIROS_GITHUB_WEBHOOKS — github plugin

**Leak primary**: `tools.ts:48`, `commands.ts:101` (gates only — body
DCE'd; design from scratch using GitHub public webhook docs).

**HTTP receiver**: behind tunnel (Phase 26 cloudflared / ngrok).

```
POST /webhooks/github
Headers:
  X-Hub-Signature-256: sha256=<HMAC>
  X-GitHub-Event: pull_request | issue_comment | push | workflow_run
Body: { ... }
```

HMAC verify: `HMAC-SHA256(body, ${GITHUB_WEBHOOK_SECRET})`.

**Event router**:
```yaml
plugins:
  github:
    webhook_secret: ${GITHUB_WEBHOOK_SECRET}
    subscribers:
      - owner: foo
        repo: bar
        events: [pull_request, issue_comment]
        channel: whatsapp:+15551234
        prompt_template: |
          Review PR #{pr.number}: {pr.title}
          Body: {pr.body}
```

**LLM tool**:
```jsonc
github_subscribe { "owner": "foo", "repo": "bar", "pr": 123, "events": ["pull_request"] }
// returns
{ "subscription_id": "uuid", "registered_at": "..." }
```

Persist subscriptions in SQLite. Survive daemon restart.

### 80.13 — KAIROS_PUSH_NOTIFICATION — APN/FCM/WebPush

**Leak primary**: `tools.ts:45-50` + `BriefTool.ts:139` (legacy alias).

**`PushProvider` trait**:
```rust
#[async_trait]
pub trait PushProvider: Send + Sync {
    async fn send(
        &self,
        title: &str,
        body: &str,
        payload: Option<serde_json::Value>,
        recipient: &PushRecipient,
    ) -> Result<PushReceipt, PushError>;
}
```

**Three implementations**:
- **APN** — token-based (.p8 key, key_id, team_id, bundle_id, env=prod|sandbox).
- **FCM** — HTTP v1 with service account JSON.
- **WebPush** — VAPID (subscription endpoint + p256dh + auth + private key).

**LLM tool**:
```jsonc
notify_push { "title": "Goal complete", "body": "...", "payload": {...}, "recipient_id": "device-123" }
```

Per-binding config:
```yaml
push:
  provider: apn   # apn | fcm | webpush
  credentials_ref: secrets/push-prod.json
  default_recipient: <device_id>
```

`nexo agent push test --binding=<id>` smoke command.

### 80.14 — AWAY_SUMMARY

**Leak primary**: `feature('AWAY_SUMMARY')` integration point only.
Design from scratch.

**Trigger**: `pairing_store::record_inbound(channel, account_id)`
computes `gap = now - last_seen_at`. If `gap > threshold` (default 1 h,
binding-overridable), spawn forked goal with:

```text
You are summarising activity that happened while the user was offline.

Time window: <last_seen_at> .. <now>  (gap: <human-readable>)

Sources to summarise (from Phase 72 turn log):
- Goals completed: <list>
- Goals aborted: <list>
- notify_origin events: <list>
- Cron fires: <list>

Produce a single message <= <budget> tokens. Skip if window is empty.
```

Forked goal uses `ForkAndForget` (80.19) + read-only memory whitelist.

**Configuration**:
```yaml
away_summary:
  enabled: false           # off by default
  threshold_secs: 3600     # 1 h
  max_tokens: 800
```

### 80.15 — Assistant module flag

**Leak primary**: `main.tsx:1058-1088, 1075, 2206-2208, 3035`.

**Per-binding YAML**:
```yaml
agents:
  - id: morning-coach
    binding: claude-haiku
    assistant_mode: true
    team:
      auto_spawn: true
      members:
        - { name: research, binding: claude-sonnet }
        - { name: writer,   binding: claude-haiku }
```

**Effects when `assistant_mode: true`** (D-9 implication bundle):
- System-prompt addendum appended (paraphrased from leak — no
  Anthropic-internal wording).
- Initial team auto-spawned at boot.
- Brief mode auto-on (80.8).
- `cron.enabled: true` default.
- `proactive.enabled: true` default (Phase 77.20).
- `auto_dream.deep.enabled: true` default (80.1).
- `kairos_remote_control` REMAINS `false` (D-4 — explicit opt-in).

**System prompt addendum** (drafted, refine in 80.15 brainstorm):
```
You are running in assistant mode — an autonomous background agent.
You may receive periodic <tick> prompts (proactive mode), inbound
messages from peers (SendToPeer), inbound channel notifications, or
cron-fired prompts. Treat each as a turn. Use SendUserMessage for
explicit operator-facing checkpoints; free-text output is hidden.
Use Sleep when idle. Consolidate memory via auto-dream when sessions
accumulate. Delegate to peers via SendToPeer + ListPeers.
```

### 80.16 — `nexo agent attach` + `nexo agent discover`

**Leak primary**: `main.tsx:559, 685-694, 3259-3340`.

**`nexo agent attach <goal_id>`**:
- Subscribe to `agent.events.<goal_id>` — stream events to stdout.
- Read stdin lines → publish to `agent.inbox.<goal_id>`.
- Detach on Ctrl-C (matches isBgSession exit-path semantics).
- Exit on goal completion.

**`nexo agent discover`**:
- List running goals with: kind, channel, age, prompt-summary, last
  event timestamp.
- Default sort: age desc.
- `--kind=bg|daemon|...` filter.

Companion-tui (Phase 26) piggy-backs on the same subjects.

### 80.17 — `kairos_remote_control` mode

**Leak primary**: `main.tsx:2916`.

**Per-binding YAML**:
```yaml
kairos_remote_control: true   # default false
auto_approve_tools:
  - Sleep
  - cron_*
  - notify_*
  - github_*
  # Bash auto-approves only if bash_security::is_read_only(input)
  # (Phase 16 capability gate stays authoritative)
```

When enabled AND tool is in allowlist AND Phase 16 capability gate
passes → permission auto-approves. Otherwise normal flow.

`setup doctor` warning:
```
WARN: agent <id>: assistant_mode=true but kairos_remote_control=false.
Goals will block on approval prompts in unattended runs.
Consider:
  kairos_remote_control: true
  auto_approve_tools: [Sleep, cron_*, notify_*]
```

### 80.18 — DreamTask audit row   ✅

**Status**: shipped. `crates/agent-registry/src/dream_run.rs` (~860 LOC,
26 unit tests). Mirrors Phase 72 turn-log pattern. Three pillars
verified: idempotent insert + transactional append + MAX_TURNS=30
cap (Robusto), shared SqlitePool + JSON columns (Óptimo), zero
LlmClient coupling + flexible `fork_label` String (Transversal).
Concurrent-same-id test omitted due to sqlx 0.8 + SQLite `BEGIN IMMEDIATE`
limitation; production pattern is single-writer-per-row anyway.

**Leak primary**: `tasks/DreamTask/DreamTask.ts:25-130`.

**Schema migration v4**:
```sql
CREATE TABLE dream_runs (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    status TEXT NOT NULL,             -- running | completed | failed | killed
    phase TEXT NOT NULL,              -- starting | updating
    sessions_reviewing INTEGER NOT NULL,
    files_touched TEXT NOT NULL,      -- JSON array
    prior_mtime INTEGER,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    FOREIGN KEY (goal_id) REFERENCES agent_handles(goal_id) ON DELETE CASCADE
);
CREATE INDEX idx_dream_runs_started_at ON dream_runs(started_at DESC);
```

**`dream_runs_tail` LLM tool**: returns last N rows as markdown table
(matches Phase 72 `agent_turns_tail` shape).

**`nexo dream kill <run_id>` admin CLI**: sets abort signal on the
forked goal, rolls back consolidation lock.

### 80.19 — Forked subagent infra   ✅

**Status**: shipped. `crates/fork/` (≈ 1450 LOC + 42 tests). Spec
amended live mid-execution: nexo's `nexo_llm` exposes `ChatMessage` +
`CacheUsage` (not `Message` + `ThinkingConfig`); `DriverOrchestrator`
is goal-flow heavyweight; standalone `turn_loop::run_turn_loop` uses
`LlmClient` directly. 17 KAIROS overrides collapsed to 2
(`agent_id` + `critical_system_reminder`) because Rust's `Arc<...>`
already isolates by construction.

**Leak primary**: `utils/forkedAgent.ts` (`runForkedAgent`,
`createCacheSafeParams`, `createSubagentContext`,
`skipTranscript: true` semantics at `:499, 527-541`).

**Cache-safe params** — five fields must match parent for prompt-cache
hit:
```rust
pub struct CacheSafeParams {
    system_prompt: SystemPrompt,
    user_context: BTreeMap<String, String>,
    system_context: BTreeMap<String, String>,
    tool_use_context: ToolUseContext,
    fork_context_messages: Vec<Message>,  // parent's prefix as cache prefix
}
```

**`fork_subagent` API**:
```rust
pub async fn fork_subagent(
    parent_ctx: &AgentContext,
    params: ForkParams,
) -> Result<ForkHandle, ForkError>;

pub struct ForkParams {
    cache_safe: CacheSafeParams,
    can_use_tool: Arc<dyn ToolFilter>,    // 80.20 whitelist
    on_message: Option<Box<dyn Fn(&Message)>>,
    skip_transcript: bool,
    abort_signal: AbortSignal,
    mode: DelegateMode,                    // Sync | ForkAndForget
}
```

**`skip_transcript: true` invariants**:
- No `agents/agent-*.jsonl` write.
- No `agent_handles` row inserted (forked goal invisible to
  `agent ps`).
- on_message callbacks still fire (caller can record their own).

**Mutation isolation** (`createSubagentContext`):
- `read_file_state` cloned (parent's snapshot, child mutations don't
  leak back unless `share_set_app_state: true`).
- `abort_controller` new child (parent abort cascades; child abort
  doesn't kill parent).
- `content_replacement_state` cloned (matches parent cache decisions
  exactly, deliberate to keep cache hits).

### 80.20 — auto-mem `can_use_tool` whitelist   ✅

**Status**: shipped. `crates/fork/src/auto_mem_filter.rs` (24 unit
tests) + `crates/driver-permission/src/bash_destructive.rs::is_read_only`
(19 unit tests). Three pillars verified:
- **Robusto**: 15 risks enumerated, 4 defense layers (whitelist + bash
  classifier composition + path canonicalize + post-fork audit in 80.1).
- **Óptimo**: static `&'static [&str]` whitelists, single canonicalize
  at construction, reuses Phase 77.8/77.9 classifiers.
- **Transversal**: tool name + JSON args contract, no `LlmClient`
  coupling, 3 explicit provider-shape tests.

**Leak primary**: `services/extractMemories/extractMemories.ts:171-222`.

**Whitelist** (Rust port — exact semantics):

| Tool | Allowed | Conditions |
|---|---|---|
| `FileRead`, `Glob`, `Grep` | yes | unrestricted |
| `REPL` | yes | unrestricted (inner tools re-gated through same filter) |
| `Bash` | conditional | only when `bash_security::is_read_only(input)` (Phase 77.10) |
| `FileEdit`, `FileWrite` | conditional | only when `file_path.starts_with(memory_dir)` |
| anything else | no | denial message below |

**Denial message** (verbatim from leak):
```
only FileRead, Grep, Glob, read-only Bash, and FileEdit/FileWrite within {memory_dir} are allowed
```

**Implementation**: register as a `ToolFilter` strategy attached to the
forked goal at fork time. Same trait used by Phase 77.18
coordinator/worker mode.

### 80.21 — Docs + admin-ui sync

**Standard close-out**:

- New page `docs/src/concepts/kairos-mode.md` (registered in
  `SUMMARY.md`): explains assistant_mode, brief, channels, push,
  github webhooks, away summary, fork-style consolidation.
- New page `docs/src/operations/cron-jitter.md`: 6 knobs + hot-reload
  + how to use as incident shed-load.
- `admin-ui/PHASES.md` Phase A-N entry: new "Assistant mode" panel
  listing per-binding `assistant_mode`, `brief`, `channels.allowed`,
  `push.provider`, `kairos_remote_control`, `cron.enabled`.
- `crates/setup/src/capabilities.rs::INVENTORY` registers any new env
  toggles (80.12 `${GITHUB_WEBHOOK_SECRET}`, 80.13 push provider creds,
  80.17 `NEXO_KAIROS_REMOTE_CONTROL` if exposed as env override).
- `proyecto/FOLLOWUPS.md` cleared of any 80.* deferred items (or each
  item explicitly tracked there).
- `mdbook build docs` passes locally.
- CHANGELOG.md entry.

## 7 — Brainstorm checklist (use for every 80.x sub-phase)

Per project rule (UNBREAKABLE memory): every `/forge brainstorm` for an
80.x sub-phase MUST cite at least one path:line from `claude-code-leak/`
AND from `research/` (or state explicit absence). Use this appendix as
the cite source.

Mandatory citations per sub-phase:

| Sub-phase | Leak cite | OpenClaw cite |
|---|---|---|
| 80.1 | `services/autoDream/autoDream.ts:1-324` + 3 sibling files | `research/docs/concepts/dreaming.md` (existing nexo Phase 10.6 ref) |
| 80.2 | `utils/cronJitterConfig.ts:1-78` | absence (OpenClaw has no cron jitter — confirm in brainstorm) |
| 80.3 | `utils/cronTasks.ts:381-398` | absence |
| 80.4 | `utils/cronTasks.ts:381-445` | absence |
| 80.5 | `utils/cronTasks.ts` (locate `permanent` field) | absence |
| 80.6 | `utils/cronScheduler.ts:230-260` + `cronTasks.ts:193-227` | absence |
| 80.7 | `utils/cronScheduler.ts:406-436` | absence |
| 80.8 | `tools/BriefTool/BriefTool.ts:1-204` + `commands/brief.ts:1-130` | absence |
| 80.9 | `services/mcp/channelNotification.ts:1-316` | `research/src/channels/` (port comparison) |
| 80.10 | `utils/concurrentSessions.ts:1-204` | absence |
| 80.11 | `utils/concurrentSessions.ts:86` + `tools/SendMessageTool/SendMessageTool.ts:586,631,658,685,742` | absence |
| 80.12 | `tools.ts:48` + `commands.ts:101` | absence |
| 80.13 | `tools.ts:45-50` + `BriefTool.ts:139` | absence |
| 80.14 | `feature('AWAY_SUMMARY')` integration point | absence |
| 80.15 | `main.tsx:1058-1088, 1075, 2206-2208, 3035` | `research/src/agents/` (system prompt assembly comparison) |
| 80.16 | `main.tsx:559, 685-694, 3259-3340` | absence |
| 80.17 | `main.tsx:2916` | absence |
| 80.18 | `tasks/DreamTask/DreamTask.ts:25-130` | absence |
| 80.19 | `utils/forkedAgent.ts:499, 527-541` (and full file) | `research/src/agents/sub-agent.ts` (compare delegation contract) |
| 80.20 | `services/extractMemories/extractMemories.ts:171-222` | absence |
| 80.21 | n/a (closeout) | n/a |

"absence" means the brainstorm output must say "OpenClaw has no
equivalent for this — designed from claude-code-leak only." This is
the IRROMPIBLE memory rule: no `/forge brainstorm` may skip this
declaration.

---

End of appendix. Update version and date when materially changed.
