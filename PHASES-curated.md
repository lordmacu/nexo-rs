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

## Pickup order — read this first

Sub-phases are tagged with one of these labels. Pick from the
top of the list when starting a sprint; do not skip ahead.

| Tag | Meaning |
|-----|---------|
| **P0 — NEXT** | In-flight or the immediate blocker. One sub-phase carries this at any time; finish before pulling another P0. |
| **P1 — CRITICAL PATH** | Blocks shipping the current product (today: `agent-creator-microapp`). Pull as soon as the active P0 lands. |
| **P2 — PARALLEL** | High-leverage hardening / observability that can run alongside P1 without blocking. Pull when you have spare cycles or a separate contributor. |
| **P3 — POST-CRITICAL** | Waits on P1/P2 predecessors. Don't pull early — you'll re-do work. |
| **DEFER** | Real use case but the trigger has not arrived. Don't touch unless the trigger fires. |
| **DROPPED** | Removed from active scope. Don't touch. |

### Suggested pickup order (top → bottom)

1. **P0** — Phase 84.1 coordinator persona system prompt (in flight: brainstorm + spec + plan approved → `/forge ejecutar 84.1`)
2. **P0** — Phase 84.2 → 84.3 → 84.4 → 84.5 (chain of the current phase)
3. **P1** — Phase 82.12 HTTP server hosting (agent-creator can't bind without it)
4. **P1** — Phase 82.11 agent event firehose (agent-creator UI can't reconstruct conversations without it)
5. **P1** — Phase 82.13 agent processing pause + takeover (operator intervention blocks production use)
6. **P1** — Phase 83.15 microapp testing harness (every author needs it; lands DX value early)
7. **P1** — Phase 83.16 microapp error → operator path (operational visibility)
8. **P1** — Phase 83.17 microapp config schema validation (install-time fail-fast)
9. **P1** — Phase 83.5 compliance primitives (gates 83.8 ventas-etb + serves agent-creator)
10. **P1** — Phase 83.6 microapp contract document (gates Python/TS authors using 83.7-Rust as porting source)
11. **P1** — Phase 83.10 second microapp validation (agent-creator production validation)
12. **P1** — Phase 83.12 → 83.13 → 83.14 React UI scaffold + component library + SDK publish (agent-creator UI shell)
13. **P2** — Phase 85.1 reactive 413 recovery (defensive, always-on benefit, ~1 d)
14. **P2** — Phase 86.1 local memory-shape metrics (ops observability, ~1 d)
15. ✅ ~~Phase 81.5 PluginRegistry::discover~~ (shipped 2026-05-02 library + tests; boot wire + CLI deferred to 81.6)
16. **P2** — Phase 81.9 Mode::Run registry sweep (~500 → 30 LOC critical milestone)
17. **P2** — Phase 81.3 / 81.4 / 81.6 / 81.7 / 81.8 (plug-and-play remainder; order after 81.5/9)
18. **P3** — Phase 85.2 cache-aware micro-compaction (multi-tenant cost optimization, ~3-4 d)
19. **P3** — Phase 87.1 LlmJudgeEvaluator (depends on Phase 84 fully shipped)
20. **P3** — Phase 81.10 / 81.11 / 81.12 (plugin hot-load / doctor / migration — after 81.5/9 land)

Anything not in this list is either ✅ shipped, **DEFER**, or
**DROPPED** — see sections below.

---

## ACTIVE — what we will ship next

Order: priority within each phase × phase numerical order.

### ⭐ Phase 84 — Coordinator agent persona + worker continuation   `P0`

**Status**: brainstorm + spec + plan all approved in
conversation. Next action: `/forge ejecutar 84.1`.

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 84.1 — Coordinator persona system prompt | **P0 NEXT** | ⬜ ready | 1.5 d |
| 84.2 — `<task-notification>` envelope | **P0** | ⬜ | 1 d |
| 84.3 — `SendMessageToWorker` continuation tool | **P0** | ⬜ | 2 d |
| 84.4 — Worker persona system prompt | **P0** | ⬜ | 1 d |
| 84.5 — Docs + admin-ui sync | **P0** | ⬜ | 0.5 d |

**Total**: ~6 dev-days. Critical path 84.1 → 84.2 → 84.3.

---

### Phase 83 — Microapp framework foundation (active for the agent-creator critical path)

The agent-creator microapp at `/home/familia/chat/agent-creator-microapp/`
drives this phase. Critical-path rows flagged `P1`.

| Sub-phase | Priority | Status | Notes |
|-----------|----------|--------|-------|
| 83.1 — Per-agent extension config propagation | **P2** | ⬜ | Microapp wants per-agent config maps; not yet a hard blocker |
| 81.6 — Plugin-side agent registration (library + tests) | **P2** | ✅ shipped 2026-05-02 (merge + init loop + report extension; boot wire + CLI deferred follow-up) |
| 81.7 — Plugin-side `skills_dir` contribution (library + tests) | **P2** | ✅ shipped 2026-05-03 (merge + SkillLoader::with_plugin_roots + report extension; boot wire deferred bundle) |
| 81.8 — `ChannelAdapter` trait + registry (library + tests) | **P2** | ✅ shipped 2026-05-03 (trait + registry + PluginInitContext extension + diagnostic variant; boot wire deferred bundle) |
| 81.9 — `wire_plugin_registry` boot helper + boot wire integration | **P2** | ✅ shipped 2026-05-03 (helper + LlmAgentBehavior.plugin_skill_roots + main.rs replaces 81.5.b block) |
| 81.9.b — `nexo agent doctor plugins` CLI subcommand | **P2** | ✅ shipped 2026-05-03 (Mode::DoctorPlugins variant + parser arm + run_doctor_plugins handler + 8-section TTY/JSON render) |
| 81.10 — Plugin hot-load via Phase 18 reload coord | **P3** | ✅ shipped 2026-05-03 (register_plugin_registry_reload_hook helper + boot wire + 3 unit tests; skill_roots rebuild + live discovery_cfg deferred 81.10.b) |
| 81.11 — Plugin doctor + capability inventory integration | **P3** | ✅ shipped 2026-05-03 (capability_aggregator + 3 new diagnostic variants + report extension + wire_plugin_registry signature; doctor_render sections + DoctorCapabilities envelope mode deferred 81.11.b) |
| 81.12.0 — `PluginFactoryRegistry` foundation (no plugin migrations) | **P3** | ✅ shipped 2026-05-03 (factory module + run_plugin_init_loop_with_factory + wire_plugin_registry 6th param) |
| 81.12.a — Browser plugin migration to NexoPlugin | **P3** | ✅ shipped 2026-05-03 (dual-trait + manifest + factory builder; dormant — main.rs untouched) |
| 81.12.b — Telegram plugin migration to NexoPlugin | **P3** | ⬜ pending |
| 81.12.c — WhatsApp plugin migration to NexoPlugin | **P3** | ⬜ pending |
| 81.12.d — Email plugin migration to NexoPlugin | **P3** | ⬜ pending |
| 81.12.e — Remove legacy registration block from main.rs | **P3** | ⬜ pending (after 81.12.a-d) |
| 83.2 — Extension-contributed skills | **P2** | ⬜ | Microapp ships its own skills; opportunistic |
| 83.3 — Hook interceptor (vote-to-block) | **P1** | ⬜ | Compliance primitives plug in here — gates 83.5 + 83.8 |
| 83.4 — `microapp-sdk-rust` reusable helper | **P1** | 🔄 | Core SDK ✅ 2026-04-30; 83.4.b ✅; 83.4.c Phase 82.x helpers pending |
| 83.5 — `compliance-primitives` reusable library | **P1** | ⬜ | Anti-loop / anti-manipulation / opt-out / PII redact / rate-limit / consent. Provider-agnostic, microapp-foundational |
| 83.6 — Microapp contract document | **P1** | ⬜ | Language-agnostic spec — replaces Python/TS reference templates as the portability story |
| 83.7 — Microapp template (Rust only) | **P2** | ⬜ | **Reduced** from 3 stacks to Rust only. Other stacks port from 83.6 contract |
| 83.8 — `ventas-etb` reference microapp | **P2** | ⬜ | First production microapp built on the framework |
| 83.9 — `ana` cutover | **P3** | ⬜ | Migration from yaml-only to extension-based; depends on 83.8 |
| 83.10 — Second microapp validation | **P1** | ⬜ | agent-creator production validation — proves framework reusability |
| 83.11 — Docs + admin-ui sync | **P3** | ⬜ | Final docs sweep |
| 83.12 — Meta-microapp React UI scaffold | **P1** | ⬜ | agent-creator UI shell |
| 83.13 — `microapp-ui-react` component library | **P1** | ⬜ | WhatsApp-inspired chat helper for microapps that need it |
| 83.14 — Publish SDKs (crates.io + npm) | **P1** | ⬜ | Decouples agent-creator from nexo source |
| **83.15 — Microapp testing harness (mock daemon)** | **P1** | ⬜ NEW | Closes a foundational DX gap — every author re-invents mocks today |
| **83.16 — Microapp error → operator path** | **P1** | ⬜ NEW | Operator visibility into microapp boot/handler failures |
| **83.17 — Microapp config schema validation** | **P1** | ⬜ NEW | Shifts validation to install/boot time so misconfig fails fast |

**3 new gap-closing sub-phases added in this curation pass**
(83.15 / 83.16 / 83.17). They were missing from the original
plan — every microapp author would have hit them.

---

### Phase 82 — Multi-tenant SaaS extension enablement

Critical path for agent-creator: **82.11 / 82.12 / 82.13** all
flagged `P1`. Without these the agent-creator UI cannot stream
transcripts, host its HTTP server, or pause agents.

| Sub-phase | Priority | Status |
|-----------|----------|--------|
| 82.1 — `BindingContext` enrichment | — | ✅ |
| 82.2 — Tool registry + manifest parsing | — | ✅ |
| 82.3 — Plugin.toml [outbound_bindings] schema | — | ✅ |
| 82.4 / 82.5 / 82.7 / 82.10 | — | ✅ |
| 82.6 — Per-extension state_root convention | **P2** | ⬜ |
| 82.8 — Multi-tenant audit log filter | **P2** | ⬜ |
| 82.9 — Reference SaaS template | **P3** | ⬜ |
| 82.11 — Agent event firehose + transcripts | **P1** | ⬜ |
| 82.12 — HTTP server hosting | **P1** | ⬜ |
| 82.13 — Agent processing pause + takeover | **P1** | ⬜ |
| 82.14 — `escalate_to_human` tool + notification | **P2** | ⬜ |

---

### Phase 81 — Plug-and-Play Plugin System

| Sub-phase | Priority | Status |
|-----------|----------|--------|
| 81.1 / 81.2 | — | ✅ |
| 81.3 — Tool namespace runtime enforcement | **P2** | ⬜ |
| 81.4 — Plugin-scoped config dir loader | **P2** | ⬜ |
| 81.5 — `PluginRegistry::discover` filesystem walk | **P2** | ✅ shipped 2026-05-02 (library + tests; boot wire + CLI deferred to 81.6) |
| 81.6 — Plugin-side agent registration | **P3** | ⬜ |
| 81.7 — Plugin-side `skills_dir` | **P3** | ⬜ |
| 81.8 — `ChannelAdapter` trait | **P3** | ⬜ |
| 81.9 — `Mode::Run` registry sweep | **P2** | ⬜ critical milestone (~500 → 30 LOC) |
| 81.10 — Plugin hot-load via reload coord | **P3** | ⬜ |
| 81.11 — Plugin doctor + capability inventory | **P3** | ⬜ |
| 81.12 — Existing plugin migration | **P3** | ⬜ |
| 81.13 — Reference plugin template + CLI | **DEFER** | ⬜ gated on 81.5 + 81.9 |

---

### Phase 85 — Compaction hardening

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 85.1 — Reactive 413 recovery | **P2** | ⬜ | ~1 d |
| 85.2 — Cache-aware micro-compaction | **P3** | ⬜ | ~3-4 d |

---

### Phase 86 — Memory observability

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 86.1 — Local memory-shape Prometheus metrics | **P2** | ⬜ | ~1 d |

---

### Phase 87 — LLM-as-judge verifier

| Sub-phase | Priority | Status | Effort |
|-----------|----------|--------|--------|
| 87.1 — `LlmJudgeEvaluator` impl | **P3** | ⬜ AFTER-PHASE-84 | ~2 d |

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
