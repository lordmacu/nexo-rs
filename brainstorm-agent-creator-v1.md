# Brainstorm — `agent-creator` v1 (SaaS meta-microapp)

**Status:** brainstorm (pre-spec). Output of `/forge brainstorm agent-creator-v1`.

## 1. Producto

`agent-creator` es la **microapp meta-creadora de agentes WhatsApp**
ofrecida como **SaaS multi-tenant**. Cristian (operador del SaaS) crea
manualmente daemons nexo-rs — uno por empresa cliente. Cada empresa
contiene N agentes WhatsApp; cada agente puede compartir o tener su
propio LLM provider; los agentes pueden compartir conocimiento (skills).

Roles:

- **Operador** (Cristian): crea empresas (daemons), ve clientes y chats.
- **Cliente** (tenant): crea/edita sus agentes, ve sus conversaciones,
  toma takeover humano y libera.
- **Usuario final**: persona en WhatsApp que conversa con un agente.

## 2. Topología

```
1 daemon nexo-rs  ==  1 empresa cliente
                       │
                       ├── N agentes WhatsApp (cada uno con su número o
                       │   compartiendo cuenta)
                       ├── M skills (markdown) compartibles entre agentes
                       └── K LLM providers (api keys del cliente, no del
                           operador — cliente paga su consumo)
```

Out-of-tree:

- Repo microapp: `/home/familia/chat/agent-creator-microapp/` (Phase 83.10
  scaffold, ~75 LOC). NO entra en `crates/`.
- Framework `nexo-rs` permanece **multi-channel agnostic** — telegram /
  email pueden añadirse después sin romper la microapp.

## 3. Mining de referencias

### research/ (OpenClaw, TypeScript)

Citas obligatorias (regla `feedback_brainstorm_must_mine_research_and_leak.md`):

- `research/src/channels/conversation-binding-context.ts:1` — modelo de
  binding agente↔canal↔conversación. Equivalente Rust ya existe en
  `crates/core/src/agent/binding.rs::BindingContext` (Phase 82.1
  multi-tenant). **Reutilizable tal cual.**

- `research/extensions/whatsapp/src/channel.ts:65` — bloque `pairing { ... }`
  que define DM-allow vía pairing-store. Análogo Rust = el wrapper
  `crates/plugins/whatsapp/` + `nexo/admin/pairing/*` (Phase 82.10 step 5,
  YA existe). El microapp simplemente expone QR al frontend.

- `research/src/agents/skills.ts:1-50` — skills (markdown) inyectados al
  prompt del agente. Análogo Rust: Phase 83.2 contributed-skills (ya
  existe como concepto framework). **Gap:** en framework actual los skills
  son read-only attach; falta CRUD vía admin RPC para que la microapp los
  edite (`nexo/admin/skills/*`).

- `research/extensions/wacli/backend.ts:1` — backend wacli single-process.
  En nexo-rs el wrapper vive en `crates/plugins/whatsapp/`, el
  `whatsapp-rs` real está fuera del workspace. **No copiar** — Rust
  microservices > TS single-process.

### claude-code-leak/

**Ausente.** `ls /home/familia/chat/claude-code-leak/` retorna "No such
file or directory". Documentado por la regla irrompible — esta
brainstorm cumple con declarar la ausencia.

### Local repos

- `agent-creator-microapp/` — punto de partida out-of-tree (Cargo.toml,
  plugin.toml, src/main.rs stub). v1 lo extiende, no lo reescribe.
- `crates/microapp-sdk/` — SDK ya publica `Microapp::run_stdio`,
  `before_message` hook, `OutboundDispatcher`. v1 reusa todo.
- `crates/compliance-primitives/` — Phase 83.6 (anti-loop, anti-manip,
  opt-out, PII redactor, rate-limit, consent). El microapp registra
  estos hooks dinámicamente según toggle del agente.

## 4. Lo que YA existe en el framework (sin tocar)

| Capacidad | Phase | Ubicación |
|-----------|-------|-----------|
| Multi-tenant `account_id` keying | 82.1 | `BindingContext` |
| Admin RPC: agents CRUD | 82.10 step 1-4 | `nexo/admin/agents/*` |
| Admin RPC: pairing | 82.10 step 5 | `nexo/admin/pairing/*` |
| Admin RPC: LLM providers | 82.10 step 6 | `nexo/admin/llm_providers/*` |
| Admin RPC: channels | 82.10 step 7 | `nexo/admin/channels/*` |
| Transcripts firehose | 82.11 | `nexo/admin/transcripts/stream` |
| HTTP server + auth tokens | 82.12 | listener + 2-token gate |
| Processing pause/intervention | 82.13 | `InterventionAction` |
| Escalation events | 82.14 | escalation bus |
| Skills attach (read-only) | 83.2 | `agent.skills: [...]` YAML |
| Compliance toggles per-agent | 83.1 | `extensions_config` field |
| Hook interceptor (block/transform) | 83.5 | `HookOutcome` |
| Microapp SDK (stdio JSON-RPC) | 83.3 | `nexo-microapp-sdk` |

## 5. Gaps a agregar al framework (agnostic, on-demand)

Cumplen el test "¿lo usaría OTRA microapp distinta?" — sí, son primitivos.

### 5.1 — `nexo/admin/skills/*` (CRUD admin RPC)

- **Por qué:** hoy skills son read-only attach (configurados en YAML).
  Microapp necesita crear/editar markdown skills desde la UI cliente.
  Otra microapp futura (ej. CRM, soporte) también querrá skills CRUD.
- **Shape:** `skills.list`, `skills.get`, `skills.create`, `skills.update`,
  `skills.delete` — capability-gated `manage_skills`.
- **Storage:** mismo dir que Phase 83.2, persistido en disk;
  re-inyectable en prompt al recargar agente.

### 5.2 — Cerrar end-to-end `InterventionAction::Reply`

- **Por qué:** Phase 82.13 ya ofrece pause/resume; falta wire del
  `Reply` action para que el operador escriba un mensaje *en nombre del
  agente* sin reanudar la IA. Microapp lo necesita para el modo takeover
  humano completo. Otra microapp (ej. soporte ticketing) lo reusa igual.
- **Shape:** `nexo/admin/agents/{id}/intervene` con
  `{ action: "reply", body: "..." }` → outbound directo al canal.

### 5.3 — SDK helper `HumanTakeover`

- **Por qué:** patrón takeover IA→manual→liberar es transversal.
  Empaquetarlo como helper en `nexo-microapp-sdk` evita que cada
  microapp re-inventeé la coordinación pause+reply+resume.
- **API:** `HumanTakeover::engage(agent_id)` → pause; `.send(body)` →
  reply; `.release()` → resume + opcional resumen-de-conversación al
  agente para retomar contexto.

### 5.4 — SDK helper `TranscriptStream::filter_by_agent`

- **Por qué:** Phase 82.11 firehose retorna TODOS los transcripts del
  daemon. Cliente de la microapp solo debe ver los de SUS agentes
  (multi-tenant defense-in-depth). Helper SDK lo hace una línea, no
  copy-paste por microapp.
- **API:** `TranscriptStream::firehose().filter_by_agent(agent_ids)`.

### 5.5 — Notificación "agente no sabe" (UI)

- **Por qué:** requisito producto: cuando agente no encuentra respuesta
  en skills/knowledge, notificar UI. Hoy framework ya emite
  escalation events (Phase 82.14); falta marker semántico
  `EscalationReason::UnknownQuery`.
- **Shape:** nuevo variant en enum existente. Cero breaking change si se
  hace non-exhaustive (ya lo es).

## 6. Tools v1 de la microapp

Orientadas a CRUD del cliente — todos backed por admin RPC framework:

| Tool microapp | Backed by |
|---|---|
| `create_agent`, `list_agents`, `update_agent`, `delete_agent` | `nexo/admin/agents/*` |
| `pair_whatsapp` (devuelve QR) | `nexo/admin/pairing/*` |
| `add_llm_key`, `list_llm_keys`, `delete_llm_key` | `nexo/admin/llm_providers/*` |
| `create_skill`, `list_skills`, `update_skill`, `delete_skill`, `attach_skill_to_agent` | `nexo/admin/skills/*` (gap 5.1) |
| `set_compliance_toggles` (per-agent: anti_loop / opt_out / pii / rate_limit) | `nexo/admin/agents/{id}/extensions_config` |
| `list_conversations`, `get_transcript` | `nexo/admin/transcripts/*` + filter helper (gap 5.4) |
| `human_takeover_start`, `human_takeover_send`, `human_takeover_release` | `HumanTakeover` SDK helper (gap 5.3) |

## 7. UI scope (React frontend, repo aparte)

- Dos vistas autenticadas con tokens HTTP (Phase 82.12):
  - **Cliente**: ve sus agentes, conversaciones, takeover, LLM keys,
    skills propias.
  - **Operador**: ve TODOS los clientes (1 daemon = 1 cliente, así que
    operador alterna entre múltiples daemons; multi-empresa-en-una-pantalla
    se difiere a Phase 2 — v1 usa pestañas múltiples del browser).
- Comunica vía WebSocket a transcripts firehose + REST a admin RPC.
- React fuera del repo Rust. Empaquetado: TBD spec.

## 8. Per-agent configurability

Cada agente almacena en `agents.yaml` (managed por admin RPC, NUNCA
manual):

```yaml
agents:
  - id: agent-ventas-acme
    llm_profile: minimax-acme    # qué LLM keys usar
    skills:                       # cuáles skills inyectar
      - skill-tarifario-2026
      - skill-coverage-bogota
    extensions_config:
      compliance:
        anti_loop: { enabled: true, max_repeat: 3 }
        opt_out:   { enabled: true }
        pii:       { enabled: true, redact: ["phone", "email"] }
        rate_limit:{ enabled: true, per_min: 5 }
```

Microapp ofrece checkboxes UI; toggle on/off → admin RPC `update_agent`
→ daemon hot-reload del hook registry (Phase 83.5 ya soporta).

## 9. Onboarding

v1 = manual. Cristian provisiona daemon por cliente (`docker compose up`
con env-vars del tenant). NO self-service register. Diferido a Phase 2
post-validación de mercado.

## 10. Riesgos / open questions

- **Cuotas LLM por cliente:** ¿quién paga? v1 = cliente pone su API key
  → cliente paga directo al provider. Operador no factura uso, factura
  suscripción flat. **Pricing TBD.**
- **WhatsApp account isolation:** ¿1 cuenta WhatsApp por agente o varios
  agentes comparten cuenta con routing por contexto? v1 simple = 1
  cuenta por agente. Multi-agente-en-1-cuenta diferido.
- **Skill markdown editor:** ¿WYSIWYG o textarea? Diferir a spec UI.
- **Backup transcripts:** ¿retention policy? Diferir.
- **Audit log de cambios cliente:** quién creó/borró agente, cuándo.
  Necesario por compliance pero diferible a Phase 2.

## 11. Cortes propuestos para spec

Particionar la spec así:

1. **Framework gaps** (5.1–5.5) — ship primero, agnostic, otras microapps
   se benefician.
2. **SDK helpers** (5.3, 5.4) — siguen al gap.1 con tests.
3. **Microapp expansión** — extender `agent-creator-microapp/` con tools
   v1 de §6.
4. **UI React** — frontend separado, conectado a HTTP + WS.

Cada corte = sub-fase atómica con cargo build clean.

---

**Listo para `/forge spec agent-creator-v1`.**
