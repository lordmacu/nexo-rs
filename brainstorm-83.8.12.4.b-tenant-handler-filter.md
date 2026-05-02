# Brainstorm 83.8.12.4.b — handler-level tenant filter + TranscriptWriter tenant_id population

## Problema

Phase 83.8.12.4 dejó tres deferreds:

1. `agent_events/list` — filter struct lleva `tenant_id` pero el reader lo ignora; todo evento se devuelve sin importar el tenant.
2. `escalations/list` — mismo síntoma; trait method recibe `tenant_id` pero el adapter no filtra.
3. `TranscriptWriter` emite `AgentEventKind::TranscriptAppended { tenant_id: None, .. }` siempre — el writer no conoce su tenant.

Resultado: cross-tenant leak en el firehose. Un microapp scoped a tenant A puede leer eventos de agentes en tenant B con sólo nombrarlos en `agent_id`. Subscribers que reciben firehose deben re-consultar `agents.yaml` por evento → contradice el campo `tenant_id`.

## Refs

**OpenClaw `research/src/infra/agent-events.ts:99-218`** — patrón canon: cada `emitAgentEvent` enriquece el payload leyendo de un registry indexado por `runId` (`AgentRunContext`). El productor registra contexto una vez al crear el run, y luego cada emit lo lee sin tener que pasarlo por argumento. Mapea limpio a "el `TranscriptWriter` conoce su `tenant_id` por inyección en el constructor" — cada writer es per-agent, y tenant es per-agent, así que el writer puede stampar `tenant_id` desde su propio estado en lugar de re-resolverlo por evento.

**`claude-code-leak/`** — sin precedente directo. La CLI es single-tenant (un único usuario local), no hay enriquecimiento cross-tenant en su path de eventos. Búsqueda en `claude-code-leak/src/utils/hooks/sessionHooks.ts` y `bootstrap/state.ts` no devuelve patrones aplicables. **Absence declarada**.

## Diseño

Tres wire-ups atómicos, cada uno aislable, ningún cambio de wire shape:

1. **`TranscriptWriter.tenant_id: Option<String>`** + `with_tenant_id()` builder. El emit-site reemplaza `tenant_id: None` con `self.tenant_id.clone()`. Boot wire (donde se construye el writer por agent) lee `agent.tenant_id` y lo pasa.
2. **`TranscriptReaderFs.tenant_id: Option<String>`** + ctor variant. `list_recent_events` stampa `tenant_id: self.tenant_id.clone()` en cada `TranscriptAppended` que produce. Defense-in-depth: cuando `filter.tenant_id.is_some() && filter.tenant_id.as_deref() != self.tenant_id.as_deref()` → devuelve `Vec::new()` (no leak of existence, mismo patrón que `agents/list`).
3. **`escalations::list`** signature gains `Option<&dyn YamlPatcher>`. Cuando `filter.tenant_id.is_some()` y patcher disponible, filtra rows post-list usando el helper `agent_tenant_id(patcher, &row.agent_id)` (ya existe en `agents.rs`). Sin patcher disponible (test paths), filter pasa-through (degrada a comportamiento previo).

## Riesgos

- Breaking signature en `TranscriptWriter::new` y similares: muchos call sites en tests. Mitigación: el campo es `Option<String>` con builder `with_tenant_id`, default `None`. Los `new()` existentes siguen compilando.
- Escalations resolver depende del YamlPatcher inyectado al dispatcher. Hoy el patcher ya existe (escalations no lo recibe). Habrá que extender `with_escalations_domain(store)` → `with_escalations_domain(store, Option<patcher>)`.

## Próximo

Listo para `/forge spec 83.8.12.4.b`.
