# Phases — curated active scope (2026-05-01)

**Purpose**: single source of truth for what nexo-rs is going to
build vs. what was deliberately dropped or deferred. Use this
file when planning sprints — do not re-derive priorities by
re-reading the full `PHASES.md` / `PHASES-microapps.md` /
`FOLLOWUPS.md` each time.

**Curation principles** (the lens used for every decision below):

1. **Connector framework, not model provider** — nexo-rs connects
   to LLMs (Anthropic / MiniMax / OpenAI / Gemini / DeepSeek /
   xAI / Mistral / future). Anything that assumes nexo *hosts* a
   model is out of scope.
2. **Microapp builder service** — the framework's primary
   consumer is now the microapp author building product features
   (e.g. `agent-creator-microapp`). Features that only serve
   autonomous-agent use cases without a microapp story land
   lower on the queue.
3. **No redundant capability** — if a sub-phase duplicates
   something already shipped (or trivially achievable by chaining
   shipped pieces) it gets dropped.
4. **No scope creep into ecosystems we don't control** — Python
   / TypeScript reference templates, third-party container
   runtimes, push-notification provider integrations all stay
   out unless the microapp framework itself demands them.

---

## ACTIVE — what we will ship next

Order: priority within each phase × phase numerical order.

### ⭐ Phase 84 — Coordinator agent persona + worker continuation

**Status**: brainstorm + spec + plan all approved in
conversation. Next action: `/forge ejecutar 84.1`.

| Sub-phase | Status | Effort |
|-----------|--------|--------|
| 84.1 — Coordinator persona system prompt | ⬜ ready | 1.5 d |
| 84.2 — `<task-notification>` envelope | ⬜ | 1 d |
| 84.3 — `SendMessageToWorker` continuation tool | ⬜ | 2 d |
| 84.4 — Worker persona system prompt | ⬜ | 1 d |
| 84.5 — Docs + admin-ui sync | ⬜ | 0.5 d |

**Total**: ~6 dev-days. Critical path 84.1 → 84.2 → 84.3.

---

### Phase 83 — Microapp framework foundation (active for the agent-creator critical path)

The agent-creator microapp at `/home/familia/chat/agent-creator-microapp/`
drives this phase. The 6 sub-phases on its critical path are
flagged `★`.

| Sub-phase | Status | Notes |
|-----------|--------|-------|
| 83.1 — Per-agent extension config propagation | ⬜ | Microapp wants per-agent config maps |
| 83.2 — Extension-contributed skills | ⬜ | Microapp ships its own skills |
| 83.3 — Hook interceptor (vote-to-block) | ⬜ | Compliance primitives plug in here |
| 83.4 — `microapp-sdk-rust` reusable helper | 🔄 | Core SDK ✅ shipped 2026-04-30; 83.4.b agent-creator migration ✅; 83.4.c Phase 82.x helpers pending |
| 83.5 — `compliance-primitives` reusable library | ⬜ | Anti-loop / anti-manipulation / opt-out / PII redact / rate-limit / consent. KEEP — provider-agnostic, microapp-foundational |
| 83.6 — Microapp contract document | ⬜ | The language-agnostic spec — replaces Python/TS reference templates as the portability story |
| 83.7 — Microapp template (Rust only) | ⬜ | **Reduced** from Rust + Python + TypeScript to Rust only. Other stacks port from 83.6 contract. |
| 83.8 — `ventas-etb` reference microapp | ⬜ | First production microapp |
| 83.9 — `ana` cutover | ⬜ | Migration from yaml-only to extension-based |
| 83.10 — Second microapp validation ★ | ⬜ | agent-creator production validation — proves framework reusability |
| 83.11 — Docs + admin-ui sync | ⬜ | |
| 83.12 — Meta-microapp React UI scaffold ★ | ⬜ | agent-creator UI |
| 83.13 — `microapp-ui-react` component library ★ | ⬜ | WhatsApp-inspired chat helper for microapps that need it |
| 83.14 — Publish SDKs (crates.io + npm) ★ | ⬜ | Decouples agent-creator from nexo source |
| **83.15 — Microapp testing harness (mock daemon)** ★ | ⬜ NEW | Closes a foundational DX gap — every author re-invents mocks today |
| **83.16 — Microapp error → operator path** ★ | ⬜ NEW | Operator visibility into microapp boot/handler failures |
| **83.17 — Microapp config schema validation** | ⬜ NEW | Shifts validation to install/boot time so misconfig fails fast |

**3 new gap-closing sub-phases added in this curation pass**
(83.15 / 83.16 / 83.17). They were missing from the original
plan — every microapp author would have hit them.

---

### Phase 82 — Multi-tenant SaaS extension enablement

Critical path for agent-creator: **82.11 / 82.12 / 82.13** all
flagged `★`. Without these the agent-creator UI cannot stream
transcripts, host its HTTP server, or pause agents.

| Sub-phase | Status |
|-----------|--------|
| 82.1 — `BindingContext` enrichment | ✅ |
| 82.2 — Tool registry + manifest parsing | ✅ |
| 82.3 — Plugin.toml [outbound_bindings] schema | ✅ |
| 82.4 / 82.5 / 82.7 / 82.10 | ✅ |
| 82.6 — Per-extension state_root convention | ⬜ |
| 82.8 — Multi-tenant audit log filter | ⬜ |
| 82.9 — Reference SaaS template | ⬜ |
| 82.11 — Agent event firehose + transcripts ★ | ⬜ |
| 82.12 — HTTP server hosting ★ | ⬜ |
| 82.13 — Agent processing pause + takeover ★ | ⬜ |
| 82.14 — `escalate_to_human` tool + notification | ⬜ |

---

### Phase 81 — Plug-and-Play Plugin System

| Sub-phase | Status |
|-----------|--------|
| 81.1 / 81.2 | ✅ |
| 81.3 — Tool namespace runtime enforcement | ⬜ |
| 81.4 — Plugin-scoped config dir loader | ⬜ |
| 81.5 — `PluginRegistry::discover` filesystem walk | ⬜ |
| 81.6 — Plugin-side agent registration | ⬜ |
| 81.7 — Plugin-side `skills_dir` | ⬜ |
| 81.8 — `ChannelAdapter` trait | ⬜ |
| 81.9 — `Mode::Run` registry sweep | ⬜ critical milestone |
| 81.10 — Plugin hot-load via reload coord | ⬜ |
| 81.11 — Plugin doctor + capability inventory | ⬜ |
| 81.12 — Existing plugin migration | ⬜ |
| 81.13 — Reference plugin template + CLI | ⬜ DEFER (until 81.5 + 81.9 ship — the example will be obvious then) |

---

### Phase 85 — Compaction hardening

| Sub-phase | Status | Effort |
|-----------|--------|--------|
| 85.1 — Reactive 413 recovery | ⬜ | ~1 d |
| 85.2 — Cache-aware micro-compaction | ⬜ | ~3-4 d |

---

### Phase 86 — Memory observability

| Sub-phase | Status | Effort |
|-----------|--------|--------|
| 86.1 — Local memory-shape Prometheus metrics | ⬜ | ~1 d |

---

### Phase 87 — LLM-as-judge verifier

| Sub-phase | Status | Effort |
|-----------|--------|--------|
| 87.1 — `LlmJudgeEvaluator` impl | ⬜ AFTER-PHASE-84 | ~2 d |

---

## DROPPED ❌ — explicit no-go

These will not ship. Removed from the active sub-phase tally.

| Phase | Reason |
|-------|--------|
| **80.13** — KAIROS_PUSH_NOTIFICATION (APN/FCM/WebPush tool) | Provider-specific mobile push channel. Generic webhook receiver (Phase 80.12 ✅) covers the use case. Adding APN/FCM/WebPush ties nexo to provider-specific creds + lifecycles for marginal benefit. Microapps that need push wire it themselves. |
| **86.2** — `nexo agent debug break-cache` CLI | Debug-only framework-internal tool. Microapps don't consume it. The automatic cache-break detector (Phase 77.4 ✅) already surfaces the events. Manual force-miss can be added ad-hoc when a real bug demands it, not pre-emptively. |
| **ANTI_DISTILLATION** (was eyed in Phase 87 prior-art batch) | Provider-side defense against model distillation. Nexo is a model **consumer**, has nothing to protect against distillation. Fake-tool injection would only confuse our own agent. Permanent skip. |

---

## DEFERRED ⏸ — gated on a specific trigger

These have a real use case but the trigger has not arrived.
Listed here so the design pointer is not lost.

| Phase | Trigger (when to revisit) |
|-------|----------------------------|
| **80.7** — Cron scheduler per-cwd lock owner (multi-instance) | Phase 32 (multi-host orchestration) becoming active. Single-daemon deploys do not need it. |
| **81.13** — Reference plugin template + `nexo plugin new` CLI | After 81.5 (discover) + 81.9 (registry sweep) ship. Authors clone the reference example shipped by 81.5; the CLI ergonomics layer is value-add only after the discovery story is operational. |
| **87.2** — Container runtime dispatcher (BYOC) | **Either** Phase 32 multi-host **or** Phase 82 multi-tenant SaaS hardening demanding stronger-than-worktree isolation. Until then, the existing `WorkspaceManager` git-worktree boundary is sufficient. |

---

## Phase 80 — autonomous assistant mode (mostly ✅, residual)

22 sub-phases in original plan. 20 ✅ shipped. 1 DEFER (80.7
above), 1 DROPPED (80.13 above). **Phase 80 is effectively
closed at MVP** for the autonomous-agent core; remaining items
are not gating microapp work.

Open follow-ups against shipped Phase 80 items live in
`FOLLOWUPS.md` § Phase 36.2 + § Audit 2026-04-30 — these are
tactical hardening completions, not promotion-worthy
sub-phases.

---

## Curation pass — what was promoted from FOLLOWUPS.md

Reviewed the open `⬜` and `🟡` items in `FOLLOWUPS.md`. None
warranted promotion to a top-level sub-phase. Reasoning:

- **Phase 36.2 compactions tail** — tiny slice (`CompactionStore`
  schema decision); stays in followups.
- **C4.b.b YAML config bash safety schema** / **C4.c.b
  notify_origin wire** — surgical wiring tasks; stays.
- **Audit 2026-04-30 M-series (M1–M10)** — most are partial /
  shipped slices with tail items. Tail work is still tactical.
  Stays.
- **Phase 67.A–H residuals (PT-1 / PT-2 / PT-3 / PT-6 / PT-7 /
  PT-8)** — these *together* would be a sub-phase-sized effort
  (driver-binary unification + dispatch-telemetry wire-up +
  multi-agent integration test). Flagged here for future
  promotion **if** the user wants to formally schedule it.
  Currently fragmented across followup notes.
- **Phase 79.M MCP server follow-ups** / **Phase 19 V2 pollers**
  / **Phase 21 link / 25 web-search / 26 pairing** — domain-
  specific tactical hardening; stays.

Recommendation: leave followups alone. The signal-to-noise of
the open items is fine where they are. Promote only if a
specific item starts blocking microapp work.

---

## Effort summary

| Bucket | Active dev-days |
|--------|------------------|
| Phase 84 (coordinator persona) | ~6 |
| Phase 83 — agent-creator critical path (★ rows: 82.11/12/13 + 83.10/12/13/14 + 83.15/16/17) | sized in PHASES-microapps.md, ~30 d aggregate |
| Phase 83 non-critical (83.1–83.9 + 83.11) | sized in PHASES-microapps.md |
| Phase 81 plug-and-play (excluding 81.13 DEFER) | unestimated, ~10-15 d |
| Phase 85 compaction hardening | ~5 |
| Phase 86 memory observability (86.1 only) | ~1 |
| Phase 87 LLM-as-judge (87.1 only, after 84) | ~2 |
| **Active total (excl. Phase 83 detail)** | ~14 + Phase 83 critical path |

DEFER pile (~14-22 d if all activated) and DROPPED items are
not counted.

---

## Update protocol

1. When a sub-phase ships, mark it ✅ in the source `PHASES.md`
   / `PHASES-microapps.md` AND update the corresponding row
   here in the same commit.
2. When a new sub-phase is added (after a `/forge brainstorm`
   approval), record it in source AND add a row here under
   the right phase, with a one-line rationale tying it to the
   curation principles above.
3. When a sub-phase is dropped or deferred, move its row from
   ACTIVE to DROPPED ❌ or DEFERRED ⏸ here AND apply the
   marker in the source file.
4. Do not let this file drift — `CLAUDE.md` cites it as the
   single source of truth for active scope, so a stale view
   here mis-leads sprint planning.
