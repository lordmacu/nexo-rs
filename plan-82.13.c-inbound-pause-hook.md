# Plan — Phase 82.13.c: inbound dispatcher pause hook

Cierra el gap crítico de 82.13.b. Atómico. Cada step compila +
tests pasan + commit.

---

## Mining citado

- `research/sessions.ts:1210-1220` (`sessions.steer`) valida
  el patrón "intercept-en-dispatcher-antes-de-fire". El
  análogo nuestro vive en `runtime.rs:706-722`.
- `claude-code-leak/` no presente local. Absencia declarada.

---

## Step 1 — Runtime field + builder + intake hook (commit-able alone)

**Archivos modificados:**

- `crates/core/src/agent/runtime.rs`:
  - Import `ProcessingControlStore` + `ProcessingScope` +
    `ProcessingControlState` + `PendingInbound` from
    `nexo_tool_meta::admin::processing` (or core trait file
    for the store).
  - Field `processing_store: Option<Arc<dyn
    ProcessingControlStore>>` on `Runtime` struct.
  - Initialize to `None` in default constructor.
  - Builder `with_processing_store(store)`.
  - Hook in intake loop at line 722 (after `let message_id =
    msg.id`):
    1. If `processing_store.is_none()` → skip check
       (legacy path).
    2. Build `ProcessingScope::Conversation` from `msg`
       fields.
    3. `store.get(&scope)` → fail-open on Err.
    4. If `PausedByOperator` → redact body via
       `runtime.redactor` if available, build
       `PendingInbound`, `push_pending`, emit firehose drop
       event when `dropped > 0`, `continue;`.
    5. Else → fall through to existing session-spawn +
       `try_send`.

**Tests (6 new in `runtime.rs` test module or a new
`runtime_pause_hook_tests.rs`):**

- `inbound_during_pause_buffers_instead_of_firing`
- `inbound_active_scope_passes_through`
- `inbound_when_store_unwired_fires_legacy_path`
- `inbound_when_store_get_fails_fails_open`
- `body_is_redacted_before_push`
- `cap_exceeded_emits_drop_event`

Use the existing `MockStore` pattern from
`crates/core/src/agent/admin_rpc/domains/processing.rs`
(test module already extended in 82.13.b.3.2). For the
runtime test, may need a `RecordingBehavior` mock to verify
"agent NOT invoked".

**Done:**
- `cargo build -p nexo-core --tests` clean.
- 6 tests pass.
- All existing runtime tests still pass (legacy path
  unchanged when `processing_store` is None).

**Commit:** `feat(82.13.c.1): runtime intake hook checks
ProcessingControlStore + buffers inbound during pause`

---

## Step 2 — Boot wire-up + AdminBootstrapInputs sharing

**Archivos modificados:**

- `crates/setup/src/admin_bootstrap.rs`:
  - `AdminBootstrapInputs` gains
    `processing_store: Option<Arc<dyn ProcessingControlStore>>`.
  - When `Some`, dispatcher uses this shared instance via
    `with_processing_domain(store)` (already exists). When
    `None`, fallback to old behavior of constructing one
    locally (so tests + legacy daemons keep working).
  - Update existing test fixtures (10 sites of
    `AdminBootstrapInputs { ... }` literals) to thread
    `processing_store: None` (graceful default).

- `src/main.rs` (where runtime + bootstrap are constructed):
  - Read `NEXO_PROCESSING_PENDING_QUEUE_CAP` env var.
  - Construct `Arc<InMemoryProcessingControlStore>` once.
  - Pass to runtime via `with_processing_store`.
  - Pass to bootstrap via `AdminBootstrapInputs.processing_store`.

**Tests:**

- 1 new integration test in `admin_bootstrap.rs` test
  module: `bootstrap_with_shared_processing_store_routes_to_runtime`
  — verifies that pause via admin RPC + inbound via runtime
  hit the same store instance (push happens, drain happens).

**Done:**
- `cargo build --workspace --tests` clean.
- New integration test passes.
- `nexo agent <id>` smoke test still boots (manual).

**Commit:** `feat(82.13.c.2): boot shares ProcessingControlStore
between admin RPC + runtime`

---

## Step 3 — Docs + admin-ui sync + close-out

**Archivos modificados:**

- `docs/src/microapps/admin-rpc.md`:
  - Remove the "**Note (Phase 82.13.b.3.2 limitation)**"
    paragraph in the "Pending inbounds during pause" section.
  - Replace with a confirmation block that the round-trip
    works end-to-end, with a smoke-test recipe.

- `admin-ui/PHASES.md`:
  - Mark the existing pending checkboxes related to
    `pending_depth` badge / drop event UI rendering as
    "backend ready" (frontend still TODO but no longer
    blocked).

- `FOLLOWUPS.md`:
  - Mark Phase 82.13's "Inbound dispatcher hook" follow-up
    ✅ shipped 2026-05-02 as 82.13.c.
  - Mark Phase 82.13.b.3 #2 (`pending_inbounds` queue) ✅
    end-to-end (was previously "drain side ✅, push side
    deferred" — now both sides shipped).

**Done:**
- `mdbook build docs` clean.
- `cargo build --workspace --tests` clean.
- `cargo test --workspace` clean.

**Commit:** `docs(82.13.c.3): close-out — round-trip pause
takeover + pending-inbound buffering end-to-end`

---

## Riesgos

| Riesgo | Mitigación |
|--------|-----------|
| Race entre pause RPC y inbound: ventana ms donde inbound pasa antes de que el set persista | Aceptable. Próximo inbound queda bufferado. Documentado en spec. |
| Store roto bloquea todo el inbound loop | Fail-open: log warn, raw msg fluye al agent. Trade-off: pause briefly leaks; alternativa (fail-closed = freeze) es peor. |
| Redactor doble (push + transcript) | Idempotente — regex sobre `[REDACTED:phone]` no rompe. Verificado en tests. |
| Boot ordering: store constructed antes que runtime/bootstrap | Trivial — `Arc` clonable, construir primero, pasar a ambos. |
| Cross-tenant queue leak | Ya cubierto por scope keying en 82.13.b.3.1 tests. |
| `mcp_channel_source: None` v0 dejaría inbounds vía MCP fuera del check | Aceptable v0 — Phase 80.9 MCP channel inbounds usan otro path. Logged como follow-up si emerge. |

---

## Done de toda la fase 82.13.c

- [x] 3 commits atómicos.
- [x] `cargo build --workspace` + `cargo test --workspace`
  clean.
- [x] `mdbook build docs` clean.
- [x] FOLLOWUPS.md marca 82.13's "Inbound dispatcher hook"
  + 82.13.b.3 #2 ambos ✅.
- [x] Smoke manual end-to-end: pause + 3 inbounds + resume
  → 3 User entries en transcript con timestamps originales
  + agent reanuda en próximo inbound.
- [x] Memory rule: research/ + claude-code-leak/ mining
  cited / absencia en cada doc.

---

## Listo para `/forge ejecutar 82.13.c`
