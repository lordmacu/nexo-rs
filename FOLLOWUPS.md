# Follow-ups — Deuda técnica acumulada

**Regla:** al final de cada `/forge ejecutar`, añadir aquí todo lo que no se hizo. Cada item con: qué falta, por qué se defirió, fase destino.

**Última revisión:** 2026-04-22 — sincronizado contra el código actual (`main.rs`, browser plugin, runtime, tests)

### ~~Phase 4 — `connect_existing` fallaba contra CDP remoto (Host header + WS authority echo)~~ ✅ Resuelto 2026-04-22
- Síntoma original: con el stack compose arriba, el agente registraba el plugin pero cualquier attach contra `http://chrome:9222` habría fallado (500 Internal Server Error en `/json/version` y luego WebSocket 404 al conectar).
- Causas:
  1. Chrome DevTools HTTP rechaza requests con `Host != localhost/IP`. `reqwest` enviaba `Host: chrome:9222`.
  2. Al forzar `Host: localhost`, Chrome devuelve `webSocketDebuggerUrl=ws://localhost/...` — pierde el puerto real y rompe la conexión WS.
- Fix:
  - `crates/plugins/browser/src/cdp/client.rs::discover_ws_url` → `.header(HOST, "localhost")` + nueva helper `rewrite_ws_authority` que sustituye la authority del WS URL por la del cdp_url original.
  - `crates/plugins/browser/src/plugin.rs::get_first_target_id` → mismo header en `/json/list`.
  - `crates/plugins/browser/src/chrome.rs::connect_existing` → ahora delega 1:1 en `CdpClient::discover_ws_url` (una sola ruta canónica).
- Cobertura: 3 unit tests (`rewrite_ws_authority`) + example `examples/e2e_cdp.rs` valida discover → createTarget → navigate → captureScreenshot → evaluate contra el Chrome del compose stack (PNG 4168 bytes, `h1` text "hello").
- Abierto: convertir `e2e_cdp` en test integrado gated por `CDP_URL`, e incluirlo en `scripts/integration_stack_smoke.sh` (9.6).

---

## 🔴 Críticos (bloquean uso real)

### ~~Main binary vacío~~ ✅ Resuelto 2026-04-22
- `src/main.rs` ahora: flag `--config <dir>` (default `./config`), `AppConfig::load`, `AnyBroker::from_config`, `LongTermMemory::open` (con backend="sqlite"), `SessionManager::new(session_ttl, max_history_turns)` parseando TTL con `humantime`, registro de plugins (por ahora solo browser), validación eager de `heartbeat.interval` si `enabled`, construcción de `AnyLlmClient` vía `from_config`, `AgentRuntime` por agente con memoria inyectada, shutdown graceful en SIGTERM/Ctrl+C.
- Validado: `cargo run --bin agent --config ./config` arranca, inicializa todo, para limpio en SIGTERM.

### ~~LongTermMemory nunca se instancia en producción~~ ✅ Resuelto 2026-04-22
- Se abre en `main.rs` si `memory.long_term.backend == "sqlite"` usando `memory.long_term.sqlite.path` (fallback `./data/memory.db`). Se inyecta en cada `AgentRuntime` vía `with_memory(...)` y se expone al `AgentContext` creado por sesión.

### ~~Circuit breaker NO aplicado en plugin browser~~ ✅ Resuelto 2026-04-22
- Extraído `CircuitBreaker` a crate compartido `agent-resilience` (estados Closed/Open/HalfOpen con backoff exponencial y probe automático). `BrowserPlugin` ahora guarda un `CircuitBreaker` por instancia, cada comando CDP pasa por `allow()`/`on_success()`/`on_failure()`. Config por defecto: 5 fallos consecutivos para abrir, 2 éxitos en HalfOpen para cerrar.

### ~~Circuit breaker NO aplicado en LLM clients~~ ✅ Resuelto 2026-04-22
- `MiniMaxClient` y `OpenAiClient` envuelven el bloque retry+HTTP en `CircuitBreaker::call(...)` usando el crate compartido `agent-resilience`. `CircuitError::Open` se mapea a `anyhow::Error` con el nombre del breaker.

---

## 🟡 Wiring incompleto

### Config no consumida por runtime

| Config field | Dónde vive | Quién la lee | Estado |
|---|---|---|---|
| `MemoryConfig.short_term.max_history_turns` | `memory.yaml` | `SessionManager::new` vía `main.rs` | ✅ |
| `MemoryConfig.short_term.session_ttl` ("24h") | `memory.yaml` | `humantime::parse_duration` en `main.rs` | ✅ |
| `AgentConfig.heartbeat.interval` ("5m") | `agents.yaml` | `main.rs` valida eagerly y `crates/core/src/heartbeat.rs` lo ejecuta | ✅ |
| `AgentConfig.heartbeat.enabled` | `agents.yaml` | `main.rs` + `AgentRuntime` | ✅ |
| `BrokerConfig` (nats vs local) | `broker.yaml` | `AnyBroker::from_config` en `main.rs` | ✅ |

### ~~Shutdown / bootstrap: implementado, pero incompleto para cerrar Phase 9~~ ✅ Resuelto 2026-04-22
- `src/main.rs` ahora marca not-ready al iniciar shutdown, detiene plugins primero (corta intake) y luego detiene runtimes para drenar in-flight.
- `AgentRuntime::stop()` cancela intake, cierra colas por sesión, drena mensajes pendientes y espera hasta 30s antes de abortar tareas remanentes.
- Test de runtime reforzado: `runtime_stop_flushes_remaining` exige flush real del mensaje pendiente.

### ~~Logging estructurado: avanzado, no cerrado~~ ✅ Resuelto 2026-04-22
- Runtime y loop LLM mantienen campos estructurados (`agent_id`, `session_id`, `message_id`, `correlation_id`).
- Política de formato implementada: `AGENT_LOG_FORMAT=pretty|compact|json`; si no se define y `AGENT_ENV=production`, usa JSON por defecto.
- Modo JSON agrega salida estructurada con timestamp Unix ms, nivel, target, thread_id, ubicación, fields y spans.

### ~~Metrics + health: baseline implementada~~ ✅ Endurecido 2026-04-22
- **Qué sí existe:** endpoint `:9090/metrics` con formato Prometheus etiquetado: `llm_requests_total{agent,provider,model}`, `llm_latency_ms{agent,provider,model,le}`, `messages_processed_total{agent}`, `circuit_breaker_state{breaker}`. `:8080/health` y `:8080/ready` responden con JSON; `/ready` reporta `agents_running`.
- **Cambios 2026-04-22:** telemetría reescrita sobre `DashMap<LabelKey, ...>` con `LazyLock`; `LlmClient::provider()` añadido al trait (MiniMax/OpenAi/Stub implementan); llm_behavior y runtime pasan `agent_id` + `provider` + `model` en cada observación; render Prometheus ordena series deterministicamente y escapa valores según RFC. Gauge del breaker `nats` se re-muestrea en cada scrape desde `RuntimeHealth`.
- **Tests:** 5 unit tests en `crates/core/src/telemetry.rs` (default series, labels, per-agent, multi-breaker, escaping). `scripts/integration_stack_smoke.sh` valida presencia de TYPE lines + series etiquetadas + `/ready.agents_running > 0`.
- **Migración opcional a stack `metrics`/`axum`:** diferida — la solución actual cumple requisitos sin deps extra.

### ~~Docker runtime: scaffold hecho, validación pendiente~~ ✅ Resuelto 2026-04-22
- `.dockerignore`, `Dockerfile` multi-stage, `docker-compose.yml`, `config/docker/*` y secretos vía archivos quedaron operativos.
- `docker compose up -d` validado con `nats`, `chrome` y `agent` en `healthy`.
- `docker compose down && docker compose up -d` validado con persistencia de `/app/data/memory.db` y volumen `proyecto_nats_data`.
- Ajuste técnico aplicado: builder de Docker movido a `rust:1-bookworm` para compatibilidad con crates que exigen `rustc >= 1.88`.

### Integration suite: avanzando
- **Qué sí existe:** `scripts/integration_stack_smoke.sh` + `make integration-smoke` — ahora con 6 pasos:
  1. Salud de servicios compose (`agent`, `chrome`, `nats` healthy).
  2. `/health` + `/ready` con `agents_running > 0`.
  3. `/metrics` con TYPE lines + series etiquetadas (agent/provider/model, breaker).
  4. NATS `/healthz`.
  5. Browser E2E contra Chrome real (`cargo test -p agent-plugin-browser --test browser_cdp_e2e` vs IP del container, `CDP_URL` env).
  6. **NATS restart recovery** (2026-04-22): `docker compose restart nats`, verifica trip del breaker (`circuit_breaker_state{breaker="nats"}` → 1 o `/ready` → 503), espera NATS healthy, exige reconexión en <30s (`circuit_breaker_state → 0` + `/ready 200`). Ejercita `spawn_state_monitor` en `crates/broker/src/nats.rs` y el drain del disk queue.
  7. **Agent-to-agent delegation** (2026-04-22): `crates/core/tests/delegation_e2e_test.rs` gated por `NATS_URL`. Publica `AgentPayload::Delegate` a `agent.route.kate` con correlation_id único, suscribe `agent.route.<caller>` efímero, espera `AgentPayload::Result` con matching correlation_id. Independiente de credenciales LLM (runtime envuelve error LLM como `{"error": ...}`). Valida routing contract + subscribe topic dinámico + correlation semantics.
  8. **Disk queue drain to real NATS** (2026-04-22): `crates/broker/tests/disk_queue_drain_nats_test.rs` gated por `NATS_URL`. Encola 3 eventos en `DiskQueue` (`:memory:`), suscribe un topic único en NATS real, llama `drain_nats`, verifica entrega en orden + `pending_events` vacío tras drain (segunda corrida retorna 0). Cubre el path recovery que `spawn_state_monitor` dispara en reconexión (`crates/broker/src/nats.rs:97`).
- **Qué falta para cerrar 9.6:** WhatsApp real (Phase 6 aparcada). El resto del scope está cubierto.
- **Fase destino:** 9.6

### Heartbeat reforzado, pero aún simple
- **Qué sí existe:** scheduler por agente, reminders persistidos en SQLite, tool `schedule_reminder(...)`, claim atómico de reminders vencidos para evitar lecturas duplicadas y release de claim para reintento.
- **Qué falta si se quiere llevar más lejos:** parsing natural de fechas, cancel/list/update de reminders, y un scheduler duradero más general si el alcance crece hacia cron.
- **Fase destino:** 9.x o futuro scheduler

### Agent-to-agent comms: baseline implementada
- **Qué sí existe:** `AgentMessage`/`AgentPayload`, `AgentRouter`, routing en `AgentRuntime`, y tool `delegate(...)` con timeout/correlación.
- **Qué falta si se quiere endurecer más:** política de retries/backoff para delegación fallida y límites de concurrencia por agente destino.
- **Fase destino:** 9.x (polish)

---

## 🟠 Promesas incumplidas en fases ✅

### ~~Phase 2.4 — DLQ CLI no existe~~ ✅ Resuelto 2026-04-22
- `DiskQueue` ahora expone `list_dead_letters(limit)`, `replay_dead_letter(id)`, `purge_dead_letters()`. Binario `agent` acepta subcomandos `dlq list | dlq replay <id> | dlq purge`. `replay` mueve el evento de `dead_letters` a `pending_events` con `attempts=0` para que el próximo drain lo reintente. Validado end-to-end con SQLite en `/tmp`.

### ~~Phase 2.6 — Backpressure "adaptive" claimed pero no real~~ ✅ Resuelto 2026-04-22
- `DiskQueue::enqueue` escala el sleep del productor linealmente entre 50% y 100% de `max_pending` (0 → 500ms). A 100% paga el sleep completo **y** dropea el más viejo (cap como guardia de liveness, no truncation silenciosa). Test unitario `disk_queue_applies_backpressure_over_halfway` verifica que fast path es <60ms y near-cap es ≥150ms.

### ~~Phase 3 — `embed()` no existe en `LlmClient`~~ ✅ Trait añadido 2026-04-22
- `async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>` añadido al trait con default que retorna error citando el `provider()`. Firma batch (no single-text) para alinearse con las APIs reales.
- `OpenAiClient::embed` llama `/embeddings` con `model` actual, shortcut para `texts.is_empty()`, ordena por `index` para preservar orden del batch. `AnyLlmClient::embed` delega.
- `MiniMaxClient` y `Stub` mantienen el default (error). Impl real se conecta cuando Phase 10 arranque y se decida si MiniMax se llama vía su endpoint propietario o vía su modo OpenAI-compat.
- Tests: `default_embed_returns_error_with_provider_name` + `openai_embed_empty_input_short_circuits` en `crates/llm/tests/embed_test.rs`.

### ~~Phase 3 — `stream()` no existe~~ ✅ Resuelto 2026-04-23
- `LlmClient::stream(&self, req) -> anyhow::Result<BoxStream<Result<StreamChunk>>>` añadido con default impl que sintetiza un stream desde `chat()` (providers sin SSE siguen funcionando).
- `StreamChunk` enum cerrado: `TextDelta`, `ToolCallStart`, `ToolCallArgsDelta`, `ToolCallEnd`, `Usage`, `End{finish_reason}`.
- `collect_stream()` helper reconstruye `ChatResponse` desde un stream (concatena text deltas, ensambla tool calls con args JSON).
- `OpenAiClient::stream()` override real: POST `/chat/completions` con `"stream": true, "stream_options":{"include_usage":true}` → `parse_openai_sse()` → `BoxStream<StreamChunk>`. Maneja tool_calls parciales (buffer por `index` hasta tener id+name completos antes de emitir `ToolCallStart`).
- `MiniMaxClient::stream()` override para ambos flavors: OpenAI-compat (mismo parser que OpenAiClient) + Anthropic Messages (parsing de `message_start`/`content_block_start`/`content_block_delta[text_delta,input_json_delta]`/`content_block_stop`/`message_delta`/`message_stop`).
- Circuit breaker + rate limiter + retry envuelven el **request-open** (pre-first-byte); mid-stream errors se propagan como `Err(_)` terminal sin reintento para no duplicar prefix entregado.
- SSE parsing vía `eventsource-stream = "0.2"` (maneja `\n\n` boundaries, `data:` multi-línea, comments `:`, marker `[DONE]`).
- Tests: 11 unit tests en `crates/llm/src/stream.rs` (text, tool calls, malformed skip, Anthropic deltas, default impl) + `crates/llm/tests/stream_http_smoke.rs` (wiremock: SSE real sobre HTTP + 400 open-error).
- **Follow-ups abiertos** (ver abajo):
  - Chunker (paragraph/newline/code-fence splitting), coalescing, human-like pacing — son concern de plugin, no del trait.
  - Reconnect mid-stream tras caída de conexión — decisión explícita: no soportado.
  - Reasoning-token deltas — ningún provider soportado los expone estable.

### ~~Phase 3 (follow-up) — métricas Prometheus de streaming~~ ✅ Resuelto 2026-04-24
- `agent-llm` ahora expone módulo propio `crates/llm/src/telemetry.rs` (sin depender de `agent-core`) con:
  - `agent_llm_stream_ttft_seconds{provider}` (histogram).
  - `agent_llm_stream_chunks_total{provider,kind}` (counter).
- Instrumentación via `stream_metrics_tap(...)` en `crates/llm/src/stream.rs`:
  - TTFT se observa al primer chunk contentful (`TextDelta` o `ToolCall*`).
  - Cada chunk incrementa su `kind` (`text_delta`, `tool_call_start`, `tool_call_args_delta`, `tool_call_end`, `usage`, `end`).
- Wiring aplicado en `OpenAiClient`, `MiniMaxClient`, `GeminiClient`, `AnthropicClient` y en el default `stream()` synth de `chat()`.
- `/metrics` concatena `agent_llm::telemetry::render_prometheus()` desde `src/main.rs`.
- Tests: `crates/llm/src/telemetry.rs` + `metrics_tap_records_ttft_and_chunk_kinds` en `crates/llm/src/stream.rs`.

### ~~Phase 3 (follow-up) — wiring del agent loop a `stream()`~~ ✅ Resuelto 2026-04-24
- `crates/core/src/agent/llm_behavior.rs` ya no llama `llm.chat(req)` en el loop principal.
- Ahora abre stream con `llm.stream(req)` y reconstruye respuesta final vía `agent_llm::collect_stream(...)`.
- Efecto: providers con SSE nativo pasan por el mismo camino productivo del runtime; providers sin stream real siguen funcionando por el default `stream()` del trait (fallback a `chat()`).

### ~~Phase 3 (follow-up) — cancelación de streams tested~~ ✅ Resuelto 2026-04-24
- `crates/llm/tests/stream_cancel.rs`: wiremock con 2s delay, cliente abre stream, `drop()` inmediato → verifica que el test termina en <1500ms (la drop cancela el TCP sin esperar el body completo).
- Test tolerante a timing variable en CI (skip-pass si el mock retiene headers >500ms).
- Construcción de `LlmProviderConfig` vía `serde_json::from_value` para sobrevivir los refactors en vuelo que añaden campos.

### ~~Phase 3 (follow-up) — backpressure explícita de streams~~ ✅ Resuelto 2026-04-24
- `crates/llm/src/stream.rs`: `parse_openai_sse`, `parse_anthropic_sse` y `parse_gemini_sse` ahora usan un canal acotado (`tokio::sync::mpsc`, `SSE_CHUNK_BUFFER=128`) entre parser SSE y consumer.
- El parser hace `send().await` sobre el canal; cuando el consumer se retrasa, el canal aplica backpressure y frena el pull de bytes upstream.
- Validado con suites de streaming:
  - `cargo test -p agent-llm parser_tests`
  - `cargo test -p agent-llm --test stream_http_smoke`
  - `cargo test -p agent-llm --test stream_cancel`

### ~~Phase 3 — `AgentConfig.system_prompt` no existe~~ ✅ Resuelto 2026-04-22
- `AgentConfig.system_prompt: String` (default `""`) añadido al struct + YAML loader.
- `LlmAgentBehavior::run_turn` prepende `ChatMessage::system(prompt)` al prefix de mensajes cuando `system_prompt` no está vacío (trim antes). System prompt convive con el prefix cargado desde long-term memory.
- `ChatMessage::system(content)` helper añadido a `crates/llm/src/types.rs`.
- Tests: `system_prompt_prepended_to_llm_request` (asserta role=System como primer mensaje) y `empty_system_prompt_emits_no_system_message` (confirma ausencia cuando queda vacío).
- `config/agents.yaml` + `config/docker/agents.yaml` incluyen campo con comentario; persona real queda para Phase 10 / SOUL.md.

### ~~Phase 4 — `navigate()` usa sleep(500ms) hack~~ ✅ Resuelto 2026-04-22
- `CdpClient` ahora emite un `broadcast::channel<CdpEvent>` para cada notificación sin `id`. `CdpSession::navigate` hace `Page.enable` → subscribe al stream filtrado por `session_id` → envía `Page.navigate` → espera `Page.loadEventFired` con timeout (`command_timeout_ms`).

### ~~Phase 4 — Element refs storage es dead code~~ ✅ Resuelto 2026-04-22
- Borrados `element_refs: HashMap` y `next_ref: u32` de `CdpSession`. Resolución DOM sigue usando atributo `data-agent-ref` vía JS (verdadera fuente). `click/fill/scroll_to` pasan a `&self` (ya no mutan).

### ~~Phase 4 — CDP events no publicados al broker~~ ✅ Resuelto 2026-04-22
- `BrowserInner::spawn_event_forwarder` se activa una vez tras abrir sesión: subscribe a `client.subscribe_events()` y republica cada evento como `Event` en `plugin.events.browser.{method_suffix_lowercase}` (e.g. `plugin.events.browser.loadeventfired`, `plugin.events.browser.framenavigated`).

### ~~Phase 4 — `BrowserPlugin::send_command` hardcodeado~~ ✅ Resuelto 2026-04-22
- `send_command(Command::Custom { name: "browser", payload })` deserializa `payload` como `BrowserCmd` y llama `BrowserInner::execute(...)` directamente (sin pasar por broker). Otros `Command`s retornan error claro.

### ~~Phase 4 — `BrowserPlugin::start` clona campos internamente~~ ✅ Resuelto 2026-04-22
- Estado compartido extraído a `BrowserInner` (config, session, chrome, circuit, broker, flag forwarder). `BrowserPlugin` mantiene `Arc<BrowserInner>` + `CancellationToken`. El spawn task clona un único `Arc<BrowserInner>` — sin reconstrucción de BrowserPlugin.

---

## 🟣 Deuda menor / cleanup

### Warnings de compilación
```
crates/llm/src/minimax.rs:4   — unused import `Serialize`
crates/llm/src/minimax.rs:13  — unused import `ChatMessage`
crates/broker/src/nats.rs:26  — variant `HalfOpen` never constructed
crates/broker/src/nats.rs:54  — method `half_open` never used
crates/plugins/browser/src/cdp/session.rs:139 — unused_parens
```
- **Acción:** `cargo fix --allow-dirty` — ✅ Resuelto 2026-04-22

### ~~Circuit breaker HalfOpen nunca se usa~~ ✅ Resuelto 2026-04-22
- `agent-resilience::CircuitBreaker` implementa transición real `Open → HalfOpen → Closed`. Tras `initial_backoff`, el próximo `allow()` hace transición lazy a HalfOpen; tras `success_threshold` éxitos consecutivos cierra; si falla, vuelve a Open con backoff duplicado (cap en `max_backoff`).

### ~~`AnyLlmClient::Stub` variant en producción~~ ✅ Resuelto 2026-04-22
- `agent-llm` expone feature `stub`. Variant + ctor + brazos de `match` gateados con `#[cfg(any(test, feature = "stub"))]`. `agent-core` añade `agent-llm` con `features = ["stub"]` en `dev-dependencies` para que tests de integración sigan compilando. Build/test completo verde; prod no incluye el variant.

### Secrets encrypted file backend no implementado
- **Design doc:** "Encrypted secrets file (`secrets.enc.toml` + key from env)"
- **Realidad:** solo env vars y placeholders `${VAR}` en YAML funcionan
- **Acción:** diferir indefinidamente — no necesario para uso personal

### ~~`/run/secrets/` Docker secrets no implementado~~ ✅ Resuelto 2026-04-22
- `resolve_placeholders` en `crates/config/src/env.rs` ya soportaba `${file:/path}` (trim aplicado). Test `resolves_file_secret` cubre el caso.
- Migración de `config/docker/llm.yaml`: `api_key`/`group_id` cambian de `${MINIMAX_*}` a `${file:/run/secrets/minimax_*}`, eliminando el hack de `sh -lc "export ... exec"` en `docker-compose.yml`. `command:` vuelve a ser la invocación directa del binario.

### ~~Quota alert no implementado~~ ✅ Resuelto 2026-04-22
- Duplicado del item "Rate limiter quota tracker" más abajo — `QuotaTracker` consume `quota_alert_threshold` y dispara `tracing::warn!` al cruzar el umbral.

### Rate limiter quota tracker no existe
- **Design doc:** `pub quota_tracker: Option<QuotaTracker>` en `RateLimiter`
- **Realidad:** `RateLimiter` solo hace token bucket, sin quota tracking
- **Acción:** ✅ Resuelto 2026-04-22 — `QuotaTracker` en `crates/llm/src/quota_tracker.rs`, integrado en `MiniMaxClient` y `OpenAiClient` vía `RateLimiter::with_quota`. Registra uso tras cada request, alerta cuando `remaining < quota_alert_threshold`.

---

## 🔵 Phases pendientes completas

| Phase | Estado | Sub-phases |
|---|---|---|
| 6 — WhatsApp Plugin | 0/4 | `crates/plugins/whatsapp/src/lib.rs` tiene 1 línea |
| 7 — Heartbeat | 3/3 | Cerrada |
| 8 — Agent-to-Agent | 3/3 | Cerrada |
| 9 — Polish | 5/6 | Logging + metrics + health + shutdown + docker implementados; queda integration suite |
| 10 — Soul / Identity / Learning | 0/8 | Dreaming, embeddings, transcripts |
| 11 — Extension System | 0/8 | Manifest, stdio runtime, NATS runtime |
| 12 — MCP Support | 0/7 | stdio, HTTP, tool catalog, agent as server |

Plugins telegram/, email/, whatsapp/: cada uno tiene `src/lib.rs` vacío (1 línea).

---

## 🟢 Acciones sugeridas próximo sprint

1. **Continuar Phase 9** — cerrar 9.6 (integration suite).
2. **Mantener Phase 6 aparcada** mientras `whatsapp-rs` siga moviéndose y evitar documentar integración prematuramente.
3. ~~**Aislar tests browser dependientes del entorno**~~ ✅ Resuelto 2026-04-22 — E2E contra Chrome real extraído a `tests/browser_cdp_e2e.rs`, gated por `CDP_URL` (skip limpio sin la var). Los otros tests de integración (`cdp_client_test`, `session_test`) siguen usando loopback sockets contra un mock CDP — corren en cualquier host con TCP loopback; solo fallan en sandboxes que bloquean `127.0.0.1`. El E2E real solo corre vía `scripts/integration_stack_smoke.sh` o con `CDP_URL` explícito.
4. **Revisar diseño contra OpenClaw cron** — si los recordatorios crecen en alcance, el siguiente salto razonable ya no es heartbeat sino scheduler duradero tipo cron.

---

## 🟡 Phase 10.7 — Concept vocabulary (follow-ups)

### Alias maps no implementados
- **Spec original:** `"WhatsApp" → ["wa", "whatsapp", "wapp"]` (alias expansion)
- **Realidad 10.7:** solo derivamos tags desde glosario + segmentación; "wa" queda filtrado por `min_len_for(Latin)=3`. Tag `whatsapp` sí se obtiene si aparece en el texto.
- **Acción:** diferir — introducir alias table `(canonical, alias)` en SQLite cuando haya evidencia de recall fallido en producción (métrica `memory.recall.empty_results`).

### Stop words fr/de/cjk no portados
- **OpenClaw:** 5 bancos lingüísticos (shared, en, es, fr, de, cjk) + pathNoise.
- **Realidad 10.7:** solo shared + en + es + pathNoise. fr/de/cjk no sentidos aún porque Cristian trabaja en es/en.
- **Acción:** portar cuando aparezca primer memory en esos idiomas. Ficheros: `crates/memory/src/concepts.rs` `FRENCH_STOP`/`GERMAN_STOP`/`CJK_STOP`.

### Concept vocabulary stats table no existe
- **Spec original:** `concept_vocab(term, frequency, last_seen)` para reporting y alias resolution.
- **Realidad 10.7:** tags viven solo en `memories.concept_tags`; no hay tabla agregada. 10.8 (self-report) podría necesitar `SELECT term, COUNT(*) FROM memories_unnest_tags GROUP BY term` — si resulta caro, materializar la tabla.
- **Acción:** añadir si 10.8 lo necesita; si no, diferir.

### FTS5 tokenizer default (no unicode61)
- `memories_fts` se creó con tokenizer por defecto (`unicode61` sin parámetros, folding ASCII). Funciona para latín + dígitos pero puede fallar con acentos (`configuración` → `configuracion` en match).
- **Acción:** al primer fallo de recall multilingüe, recrear la tabla con `tokenize='unicode61 remove_diacritics 2'`. Requiere migración de datos.

### Backfill de `concept_tags` para rows pre-10.7 solo en promoción
- Nuevas memorias: `remember()` deriva tags al insertar.
- Memorias viejas: solo obtienen tags cuando la fase de dreaming las promueve (backfill vía `set_concept_tags`).
- Rows que nunca promueven quedan con `[]` → recall expansion sobre ellas solo usa el query crudo.
- **Acción:** script `cargo run --bin concepts_backfill` diferido. Bajo impacto: FTS5 MATCH sobre `content` ya las encuentra si el query es literal.

### Config toggle no existe
- No hay `memory.concepts.enabled: bool`. Feature siempre activa.
- **Acción:** añadir switch solo si se observa regresión en latencia o precisión.

---

## 🟡 Phase 10.8 — Agent self-report (follow-ups)

### `describe_yourself` narration tool no implementado
- **Idea:** tool que combine `who_am_i` + `my_stats` + `what_do_i_know` y devuelva una narración en prosa, no JSON. Requiere sub-call al LLM.
- **Realidad 10.8:** solo 3 tools con JSON estructurado; el propio agente narra cuando el usuario pregunta.
- **Acción:** diferir — el pattern "LLM compone narración sobre tool output" ya funciona sin tool extra.

### DREAMS.md no se expone en `what_do_i_know`
- `what_do_i_know` solo parsea MEMORY.md. DREAMS.md (diario de dreaming) queda invisible al agente en runtime.
- **Acción:** añadir parámetro `source: "memory" | "dreams"` en la tool definition si hay feedback de que el agente quiere narrar sueños.

### `my_stats` no expone trend semana-sobre-semana
- Solo ventana fija 7d actual. No hay comparación con 7d anterior.
- **Acción:** añadir cuando UX lo pida. Requiere query adicional `count_recall_events_since(start, end)`.

### `top_concept_tags_since` no cachea
- Cada llamada a `my_stats` hace JOIN + tally en Rust. Con 200 recall events está en µs, pero si la ventana crece puede importar.
- **Acción:** cachear últimos stats en `DashMap<agent_id, (ts, stats)>` con TTL 60s si se observa latencia >50ms.

### ~~`who_am_i` lee SOUL.md entero cada llamada~~ ✅ Resuelto 2026-04-24
- `WorkspaceLoader::read_opt` ahora usa `File::open + take(limit+1)` en vez de `read_to_string`. Solo lee `limits.max_per_file + 1` bytes (el `+1` permite a `truncate` detectar overflow y emitir `[truncated]`).
- Aplicable a SOUL.md, USER.md, AGENTS.md, MEMORY.md — cualquier archivo que truncate después.
- UTF-8 defensivo: `String::from_utf8` con fallback `from_utf8_lossy` si el cap corta mid-char.
- Tests `per_file_truncation_applied` + 12 más en `agent::workspace::tests` pasan sin cambios.

### No hay endpoint HTTP para self-report externo
- Tools son solo LLM-callable. Operador que quiera dashboard tiene que ir a `metrics` de Prometheus.
- **Acción:** diferir — `/health` ya es suficiente para ops; self-report es para contexto conversacional.

### `memories_stored` no incluye los dream-exclusive (solo MEMORY.md)
- Counter cuenta filas en tabla `memories`. Facts que solo viven en MEMORY.md (editados a mano, no insertados vía `remember`) no se cuentan.
- **Acción:** aceptar como feature — lo que cuenta es "conocimiento explícitamente commiteado al store", no markdown editado.

---

## 🟡 Phase 11.1 — Extension manifest (follow-ups)

### ~~`AGENT_VERSION` viene del propio crate `agent-extensions`, no del binario `agent`~~ ✅ Resuelto 2026-04-24
- `agent_version() -> &'static str` + `set_agent_version(v)` (idempotente via `OnceLock`) en `agent-extensions::manifest`.
- `validate_min_agent_version` ahora consulta `agent_version()` en vez de la constante compile-time.
- Back-compat: `AGENT_VERSION` sigue existiendo como re-export `#[deprecated]` que apunta al fallback crate-version.
- `src/main.rs` llama `set_agent_version(env!("CARGO_PKG_VERSION"))` antes de que discovery/runtime toque manifests — así `plugin.min_agent_version` se compara contra la versión real del binario.

### Config schema JSON-Schema por extensión no implementado
- OpenClaw deriva JSON Schema desde zod para validar `plugins.entries.<id>.config`. Nosotros parseamos solo TOML del manifest; no hay contrato sobre la config *que consume* la extensión.
- **Acción:** evaluar en 11.5 cuando las tools necesiten args validados. Dependencia `schemars` + `jsonschema` es opcional.

### Manifest signing / integridad no implementado
- Cualquier `plugin.toml` se acepta si parsea. No hay checksum ni firma.
- **Acción:** diferir hasta que exista registry remoto (Phase 12+). Para uso local (`extensions/` editado por el usuario), no crítico.

### Legacy ID aliases no soportados
- OpenClaw tiene `legacyPluginIds` para renombrar sin romper usuarios.
- **Acción:** añadir `aliases: Vec<String>` con la misma validación de ID cuando haya primera ruptura.

### ~~Hot-reload de manifest no contemplado~~ ✅ Resuelto 2026-04-23
- `extensions.watch` en `extensions.yaml`: observa `plugin.toml` bajo `search_paths` y logea `warn` en edit/add/remove (`error` en TOML inválido). **No auto-respawn** — restart es la vía explícita. Hash-based diff evita warns duplicados del editor. Módulo `crates/extensions/src/watch.rs` + tests `watcher_logs_manifest_change`, `watcher_logs_invalid_toml`.

### ~~`RESERVED_IDS` es una lista estática hard-coded~~ ✅ Resuelto 2026-04-24
- `register_reserved_ids(ids)` (idempotente via `OnceLock`) + `is_reserved_id(id)` expuestos desde `agent-extensions::manifest`. El binario `agent` puede extender la lista al startup sin recompilar el crate.
- `validate_id` y `cli::install::validate_id_not_reserved` ahora consultan `is_reserved_id(id)` — une lista estática + registro dinámico.
- `RESERVED_IDS` constante sigue existiendo como fallback (útil en tests y cuando el binario no registra nada).
- Follow-up menor: build-script que derive automáticamente desde `crates/plugins/` — diferido, útil solo cuando se añadan más plugins nativos.

### ~~Validación de capability names (`^[a-z][a-z0-9_]*$`) no deja nombres con guión~~ ✅ Resuelto 2026-04-24
- Regla intencional (consistencia con identifiers Rust/Python). Documentada explícitamente en los dos templates (`template-python/README.md`, `template-rust/README.md`) con ejemplo rechazado (`get-weather`) y aceptado (`get_weather`).
- `plugin.id` sí permite `-` (regex `^[a-z][a-z0-9_-]*$`); solo los tool names dentro de `capabilities.tools` están restringidos a snake_case.

---

## 🟡 Phase 11.2 — Extension discovery (follow-ups)

### ~~No hay métrica Prometheus de discovery~~ ✅ Resuelto 2026-04-24
- `crates/core/src/telemetry.rs` agrega `agent_extensions_discovered{status=ok|disabled|invalid}` (counter) y lo renderiza siempre con las 3 labels (0 por defecto cuando no hay datos).
- `run_extension_discovery` en `src/main.rs` reporta:
  - `ok = report.candidates.len()`
  - `disabled = report.disabled_count`
  - `invalid = report.invalid_count`
- `crates/extensions/src/discovery.rs` ahora expone `DiscoveryReport.disabled_count` e `invalid_count` para evitar heurísticas en el wiring.
- Tests:
  - `telemetry::tests::extension_discovery_status_metrics_render` en `agent-core`
  - `discovery::tests::scan_applies_disabled` y `scan_invalid_manifest_becomes_error_diagnostic` validan los nuevos counters en `agent-extensions`.

### ~~Symlinks ignorados por defecto~~ ✅ Resuelto 2026-04-24
- `ExtensionsConfig.follow_links: bool` (default `false` — safe). Propagado a `ExtensionDiscovery` vía `with_follow_links(true)`.
- Los tres `build_discovery` (commands, doctor, install) consumen el flag.
- Guard "manifest path escapes search root via symlink" se relaja cuando `follow_links=true` (explícito opt-in en monorepos). Off-default mantiene la seguridad previa.
- Test unix: `follow_links_flag_discovers_symlinked_plugin` verifica que (a) default=false skip el plugin, (b) con `with_follow_links(true)` el plugin aparece en candidates.

### ~~Allowlist con ID inexistente no avisa~~ ✅ Resuelto 2026-04-24
- `ExtensionDiscovery::discover` ahora añade un `DiagnosticLevel::Warn` por cada entry del `allowlist` que no coincida con ningún candidato descubierto. Mensaje: `allowlist contains \`<id>\` but no extension with that id was discovered`.
- Visible en `agent ext doctor` (ya renderiza WARNINGS) sin flag extra.
- Test `allowlist_with_unknown_id_emits_warn_diagnostic` cubre el happy path (dos ids en allowlist; uno existe, uno genera warn).

### Hot-reload inexistente
- Cambios en `plugin.toml` requieren reinicio del agente. Aceptable mientras extensiones no se usen en caliente.
- **Acción:** 11.7 CLI puede añadir `agent extensions refresh` sin reiniciar el resto del runtime.

### ~~Pruning nested es O(N²)~~ ✅ Resuelto 2026-04-24
- `crates/extensions/src/discovery.rs::prune_nested` ahora es `O(N * depth)`:
  - ordena candidatos por path,
  - mantiene `BTreeSet<PathBuf>` de roots ya aceptados,
  - para cada candidato revisa solo su cadena de ancestros (`parent()`), en lugar de comparar contra todos.
- Mantiene la semántica original (dropear solo descendientes estrictos).
- Validado con el bloque completo `discovery::tests::*` en `agent-extensions` (incluye `scan_prunes_nested_plugin_toml`, `scan_handles_multiple_roots`, `discovery_is_deterministic`).

### ~~`scan_finds_valid_manifest` depende de `starts_with` sobre paths canónicos~~ ✅ Resuelto 2026-04-24
- `crates/extensions/src/discovery.rs` ahora normaliza paths de diagnóstico vía `normalize_path_for_display(...)`:
  - si el path cae bajo el `canonical_root`, lo remapea al `search_path` configurado por el operador,
  - si no, deja el path original.
- Efecto: en hosts donde `canonicalize` devuelve prefijos inesperados (ej. `/private/...`, UNC), los warnings/errors de discovery salen con rutas coherentes con la config.
- Test unix de regresión: `diagnostics_use_configured_search_path_prefix_when_root_is_symlink`.

### Docker volume mount ya incluido, pero config/docker usa `/app/extensions`
- `docker-compose.yml` monta `./extensions:/app/extensions:ro`. Si el usuario no crea `./extensions/` en host, el bind monta un directorio vacío — discovery emite 0 candidates sin warn (es el comportamiento correcto, dir existe pero vacío).

---

## 🟡 Phase 11.3 — stdio runtime (follow-ups)

### Respawn incompleto: outbox no se reconecta tras crash
- El supervisor detecta child exit, drena `pending` con `ChildExited`, pero marca `RuntimeState::Failed` sin volver a lanzar. El `outbox_tx` del `StdioRuntime` público apunta al writer original (muerto); reusarlo con un child nuevo requiere refactor.
- **Opciones:** (a) envolver outbox detrás de `ArcSwap<mpsc::Sender>` para poder cambiar target on-the-fly; (b) reemplazar writer-task con relay que lea de mpsc estable y escriba al stdin actual (mutable a través de `RwLock<Option<ChildStdin>>`).
- **Acción:** diferir a 11.7 o antes si aparece extensión que crashea en producción.

### ~~Sample extension bundled no es ejecutable~~ ✅ Resuelto (ya no aplica 2026-04-24)
- `extensions/sample/` ya no existe en el tree; se reemplazó por extensiones reales bundled (weather, github, translate, etc.) + `template-rust`/`template-python` (11.8). El boot ya no emite el error ruidoso.

### Sin métrica Prometheus para runtime
- Counter `agent_extension_calls_total{ext,method,status}` + histogram duración + gauge state.
- **Acción:** añadir cuando 11.5 wire tools al registry — ahí ya se cuentan llamadas reales.

### Shutdown no espera respuesta a `shutdown`
- Enviamos la notification via `try_send` y dormimos `shutdown_grace`. Mejor sería esperar un ack opcional o timeout explícito.
- **Acción:** bajo impacto; `kill_on_drop` garantiza limpieza.

### ~~Env blocklist por sufijo deja pasar variables sin prefijo típico~~ ✅ Resuelto 2026-04-24
- Suffix list expandida: `_TOKEN|_KEY|_SECRET|_PASSWORD|_PASSWD|_PWD|_CREDENTIAL|_CREDENTIALS|_PAT|_AUTH|_APIKEY|_BEARER|_SESSION`.
- Substring match adicional (`PASSWORD|SECRET|CREDENTIAL|PRIVATE_KEY` en cualquier posición) atrapa `PASSWORD_DB`, `AWS_SECRET_ACCESS_KEY`, `MY_PRIVATE_KEY_PEM` que no terminan en suffix.
- Tests: `blocks_common_suffixes`, `blocks_substring_patterns`, `passes_benign_names` (HOME/PATH/USER/LANG/RUST_LOG quedan).
- Follow-up abierto: allowlist explícita en manifest (`[transport.env]`) para cuando una extension legítima necesite una var con nombre sensible. Diferido.

### ~~Integration tests dependen de `cargo build --example echo_ext` previo~~ ✅ Resuelto 2026-04-24
- `echo_ext_path()` en `tests/stdio_runtime_test.rs` y `tests/doctor_runtime_test.rs` ahora tiene `OnceLock<PathBuf>` que spawnea `cargo build --quiet -p agent-extensions --example echo_ext` en la primera invocación si el binario no existe. `cargo test` alone basta.
- **Acción:** añadir build-helper en `tests/common/mod.rs` cuando se estabilice CI.

### ~~`StdioRuntime` no es `Debug`~~ ✅ Resuelto 2026-04-24
- `impl Debug for StdioRuntime` manual: expone `extension_id`, `state` (vía `try_read`), `pending_requests` count, `shutdown_requested`, `handshake`; `finish_non_exhaustive()` oculta canales/tasks.
- Test `runtime::stdio::debug_tests::debug_renders_extension_id_without_panic`.
- `Clone` sigue sin sentido: runtime es owner de tasks tokio; uso esperado es `Arc<StdioRuntime>`.

### Restart window reset solo tras success nunca se dispara (supervisor retorna antes)
- El `if last_restart.elapsed() > restart_window` del supervisor nunca se evalúa porque el primer fallo sale con `return`. Será relevante cuando el respawn esté funcional.

---

## 🟡 Phase 11.5 — Extension tool registration (follow-ups)

### ~~`args` no se valida contra `input_schema` antes de llamar~~ ✅ Resuelto 2026-04-23
- `ToolArgsValidator` en `crates/core/src/agent/schema_validator.rs` valida args contra `ToolDef.parameters` (JSON Schema). Gated por feature `schema-validation` (default on); compila con `--no-default-features` para opt-out completo. Config `agents[].tool_args_validation.enabled` default true. Cache por sha256 del schema. Errores surface con JSON pointer path al LLM (`outcome="invalid_args"`). Fail-open en compile errors. 6 unit tests.

### ~~Extensiones no reciben `agent_id` / `session_id`~~ ✅ Resuelto 2026-04-23
- Opt-in via `[context] passthrough = true` en `plugin.toml`. Cuando activo, `ExtensionTool::call` inyecta `_meta = { agent_id, session_id }` en args object (scalar args pasan inalterado). `AgentContext.session_id: Option<Uuid>` + builder `with_session_id`; `LlmAgentBehavior::run_turn` pone el session_id del `InboundMessage` antes de invocar el handler. Helper puro `inject_context_meta` para unit tests.

### No hay per-agent extension allowlist
- Todas las extensiones se registran en todos los agentes. Agregar un bloque `extensions:` dentro de `agents.yaml` por agente para filtrar.
- **Acción:** diferir hasta que haya más de 1 agente real.

### ~~`StdioRuntime::shutdown` no se llama al shutdown del proceso~~ ✅ Resuelto 2026-04-23
- `main.rs` ahora ejecuta `futures::future::join_all` sobre todos los `StdioRuntime::shutdown()` tras `mcp_manager.shutdown_all()` y antes de `for rt in &runtimes { rt.stop() }`. Timeout global 5 s; lo que no termine cae a `kill_on_drop`. Orden: plugins → mcp → extensions → agent runtimes.

### ~~Name overflow = skip silencioso (solo warn)~~ ✅ Resuelto 2026-04-23
- `ToolDef::fit_name(prefix, id, tool)` en `agent-llm`: passthrough si cabe en 64, sino trunca tool head + sufijo `_{hash6}` (sha256 determinístico). Aplicado en `ExtensionTool::prefixed_name`, `McpTool::prefixed_name`, `McpResourceListTool/ReadTool::prefixed_name`. Handlers preservan `tool_name` original para routing. Tests: `fit_name_tests::*`, `long_tool_name_hashed`, `long_mcp_name_is_hashed_into_limit`. `validate_name` permanece como `#[deprecated]` no-op por back-compat.

### Sin métrica Prometheus de registrations
- Counter `agent_extension_tools_registered{ext, agent}` ayudaría a ops.
- **Acción:** añadir en la misma iteración que las métricas de runtime (11.3 follow-up).

### ~~`ExtensionTool` no expone `description` ni `input_schema` post-registro~~ ✅ Resuelto 2026-04-24
- `ExtensionTool::with_descriptor_metadata(description, input_schema)` builder + getters `description() -> Option<&str>` y `input_schema() -> Option<&Value>`. Opcionales para no romper call sites legacy.
- `src/main.rs` lo aplica al registrar tools de extensiones: lifecycle hooks / introspection pueden leer lo que la extensión advertió al handshake sin re-solicitar `tools/list`.
- Test `extension_tool_test` valida que los getters reflejen los valores del `ToolDescriptor` tras el registro.

---

## 🟡 Phase 11.8 — Extension templates (follow-ups)

### `python3-minimal` no trae `json` stdlib
- Inicialmente usamos `python3-minimal` (-25MB) pero falta `json`, `time`, etc. Swapped por `python3` full (+~30MB extra).
- **Acción:** si docker image size importa, empacar stdlib mínimo manualmente o usar Python static binary (PyPy portable / python-build-standalone). Diferido.

### ~~Rust template loguea error al boot si no compilado~~ ✅ Resuelto 2026-04-24
- `config/extensions.yaml` ahora incluye `template-rust` en `disabled:` por defecto con comentario explicando cómo habilitar (build + copy binary + `agent ext enable template-rust`).
- README del template actualizado con la nota del flag de disabled.
- Fresh clones ya no emiten el spawn error ruidoso al boot.

### CLI scaffolding `agent ext new` no existe (11.7)
- El flujo actual es `cp -r`. Un comando `agent ext new my-tool --lang rust|python` automatizaría id + name + clean.
- **Acción:** 11.7.

### Templates sin hooks, NATS, HTTP
- Solo stdio + tools. Hooks (11.6), NATS transport (11.4), HTTP transport — quedan para su fase.

### Sin E2E con LLM real llamando a la extensión
- Probado stack hasta registration. No probado: LLM genera tool_call → route → extension → response → LLM consume. Requiere MiniMax quota + prompt.
- **Acción:** smoke manual documentado; no CI (costo API).

### ~~Templates no incluyen ejemplos de error handling avanzado~~ ✅ Resuelto 2026-04-24
- Python template ahora distingue:
  - `InvalidArgs` custom class → JSON-RPC `-32602 Invalid params` (el LLM puede self-correct sin round-trip al server).
  - Cualquier otra excepción → `-32603 Internal error` + `traceback.format_exc()` a stderr para ops debug.
  - Parse error de JSON host → `-32700`.
  - Tool name desconocido → `-32601`.
- `tool_add` valida shape antes de parsear (dict check, required fields, numeric coerce) y lanza `InvalidArgs` con mensaje preciso.
- README actualizado con la tabla de códigos + recomendación de uso.
- Rust template: mismo patrón ya era idiomatico con `Result<T, rpc::Error>`; no requería cambios.

### ~~Python template shebang `#!/usr/bin/env python3`~~ ✅ Resuelto 2026-04-24
- `StdioRuntime::spawn_and_handshake` ahora detecta ENOENT de `Command::spawn()` y llama `diagnose_missing_interpreter(command, cwd)`. Si el script existe + tiene shebang + el interpreter no está en `$PATH` (shebang `/usr/bin/env X`) o no existe en su path absoluto, la función devuelve mensaje: `spawn failed for <script>: shebang interpreter <interp> not found on host (install it or rewrite the shebang)`.
- Helper `which_in_path` minimalista (stdlib-only, Unix gated).
- Si el script mismo no existe, deja pasar el ENOENT original (el usuario necesita ver qué archivo falta).
- Tests: `reports_missing_interpreter_via_env` (fake interp not in PATH → message), `returns_none_when_shebang_interpreter_is_present` (`/bin/sh` exists), `returns_none_when_script_itself_is_missing`.

### ~~`chmod +x` bit depende de filesystem~~ ✅ Resuelto 2026-04-24
- `extensions/template-python/.gitattributes` nuevo pin `main.py text eol=lf` — evita que git autocrlf inyecte `\r` en el shebang (causa `/usr/bin/env python3\r: No such file or directory` en Unix si se checkout en Windows y se copia de vuelta).
- Git no soporta setear el bit de ejecución vía `.gitattributes`; documentado en README: si el filesystem destino lo pierde (Windows sin WSL), `chmod +x main.py` manualmente tras clone.
- Rust template no necesita equivalente (binario compilado, el bit lo pone `cargo` al build).

---

## 🟡 Phase 11.6 — Lifecycle hooks (follow-ups)

### Solo 4 hooks vs 29 de OpenClaw
- Cut intencional. Falta: `on_heartbeat` (requiere Phase 7 wiring), `session_start`/`session_end` (requiere eventos del SessionManager), `before_prompt_build`/`llm_input`/`llm_output` (requieren reach profundo en LLM loop), `before_compaction` (no tenemos compaction), gateway/subagent (N/A).
- **Acción:** añadir `on_heartbeat` cuando runtime heartbeat (Phase 7 follow-up) tenga fire events accesibles desde `LlmAgentBehavior`/`AgentContext`.

### ~~Event payload no se puede modificar, solo abortar~~ ✅ Resuelto 2026-04-24
- `HookResponse.override_event: Option<Value>` nuevo (rename `"override"` en JSON para evitar keyword Rust). Extensiones pueden devolver un JSON object para reescribir campos del event.
- `HookRegistry::fire_with_merge(hook, &mut event)`: itera handlers; cada `override` se shallow-merge sobre el event antes del siguiente. Aborted discards override (abort wins). `fire(...)` legacy delega y descarta el event final.
- Helper `apply_event_override` valida tipos: non-object patch → warn + ignorar; event non-object → warn + ignorar.
- Tests: `override_event_merges_into_next_handler` (h1 rewrites, h2 ve el valor nuevo + caller recibe el merge) y `override_event_ignored_when_abort` (abort descarta override).
- Extension API: extensiones que quieran reescribir añaden `override_event` al JSON-RPC response de `hooks/<name>`.

### ~~Timeout por hook no configurable~~ ✅ Resuelto 2026-04-24
- `StdioRuntime::call_with_timeout(method, params, Option<Duration>)` nuevo — `call()` delega con `None` para back-compat. Override aplica solo a la llamada actual.
- `ExtensionHook::with_timeout(Option<Duration>)` builder permite pinnear per-hook-handler. `on_hook` usa `call_with_timeout(..., self.timeout)`.
- Sigue siendo opcional: si nadie llama `with_timeout`, el comportamiento previo (`opts.call_timeout`) se preserva.
- Config YAML per-extension y per-hook priority no incluidos aquí — wiring queda para cuando el manifest exponga el field.

### Hooks ejecutan secuenciales
- Orden determinista (registro). Si 3 extensiones subscriben `before_message`, latencia = Σ de las 3.
- **Acción:** flag `parallel = true` en manifest para hooks independientes (advisory-only). Cuidado con `abort` semántico cuando son paralelos.

### ~~Ordering depende de orden de discovery~~ ✅ Resuelto 2026-04-24
- `PluginMeta.priority: i32` (default `0`) nuevo en `plugin.toml`. Lower value fires first (`security = -5` antes de `logger = 0` antes de `audit = 10`).
- `HookRegistry::register_with_priority(hook, plugin_id, priority, handler)` — `register(...)` lo delega con priority=0 para back-compat.
- Interno: `HandlerEntry { plugin_id, priority, seq, handler }`; sort por `(priority, seq)` en cada registro. `seq` se asigna monotonic via `AtomicU64` → determinismo en empates.
- `fire` unpack `HandlerEntry` y sigue el flujo normal (abort, advisory after_* warn, etc.).
- Test: `priority_orders_fire_low_first` registra en orden "logger(10), security(-5), audit(10)" y valida que `fire` los llame en `security → logger → audit`.
- Wiring en el binario (leer `manifest.plugin.priority` al registrar ExtensionHook): pendiente — requiere tocar `main.rs` que está en refactor activo. API lista para cuando cierre ese refactor.

### ~~`after_*` ignorados silenciosamente~~ ✅ Resuelto 2026-04-24
- `HookRegistry::fire` ahora detecta `hook_name.starts_with("after_")` y cuando un handler `after_*` devuelve `abort=true`, emite `tracing::warn!` con campos `hook`, `ext`, `reason` y el mensaje `"after_* hook returned abort=true; ignored (after hooks are advisory)"` — luego continúa con el siguiente handler en vez de short-circuitar.
- `before_*` y hooks sin prefijo mantienen el semántico de short-circuit en abort (contrato original intacto).
- Test `after_hook_abort_is_ignored_and_continues` valida que (a) el outcome sigue siendo `Continue`, (b) handlers posteriores sí se ejecutan.

### ~~Hook event no incluye `session_id` en `before_tool_call`/`after_tool_call`~~ ✅ Resuelto 2026-04-24
- Ambos fire sites en `llm_behavior.rs` ahora pasan `session_id: msg.session_id.to_string()` además de `agent_id`. Extensiones ya pueden correlacionar tool calls contra la misma session del `before_message` hook.

### ~~Templates Python solo loguean — no demuestran abort~~ ✅ Resuelto 2026-04-24
- `extensions/template-python/main.py` ahora demuestra dos patrones en el hook `before_message`:
  - **Abort**: texto con `__banned_token__` → `{abort: true, reason: ...}`, agente dropea.
  - **Override**: texto con leading whitespace → `{abort: false, override: {text: <stripped>}}`, agente ve texto normalizado (usa `HookResponse.override_event` que cerramos en el item previo).
- README documenta ambos patrones — los usuarios pueden copiar el template como base de filtros/normalizers sin tener que inventar la semántica.

### ~~Tests integration dependen de echo_ext (require build)~~ ✅ Resuelto 2026-04-24
- `crates/core/tests/extension_tool_test.rs` y `crates/core/tests/extension_hook_test.rs` ahora usan `OnceLock<PathBuf>` + build on-demand del example `echo_ext` (`cargo build --quiet -p agent-extensions --example echo_ext`) cuando el binario no existe.
- `cargo test -p agent-core --test extension_tool_test --test extension_hook_test` corre sin prebuild manual.

---

## 🟡 Phase 11.4 — NATS transport (follow-ups)

### Stdio supervisor no reconecta después de un crash
- Heredado de 11.3 (`runtime/stdio.rs` supervisor deja el runtime en `Failed` tras el primer crash). No se tocó en 11.4 pero la extracción del trait `ExtensionTransport` facilita arreglarlo sin romper a los consumidores de 11.5/11.6.
- **Acción:** implementar relay de `outbox` a un nuevo writer tras respawn, o rediseñar `StdioRuntime` para rebuild completo del Child+tasks manteniendo el `Arc` externo estable.

### ~~Subject `{prefix}.{id}.event` no implementado~~ ✅ Resuelto 2026-04-24
- `ExtensionDirectory` ahora se suscribe a `{prefix}.*.event` y re-emite `DirectoryEvent::Notification { id, payload }` por el canal de eventos.
- Validación de topic: helper `parse_notification_id(topic, prefix)` exige forma `<prefix>.<id>.event` (id no vacío, sin `.`) para evitar parse ambiguo.
- Seguridad básica: se ignoran eventos de extensiones no registradas en `entries` (debug log), evitando ruido/spoofing sin announce previo.
- Tests: `crates/extensions/tests/directory_test.rs::extension_event_is_forwarded_as_notification` + unit tests del parser en `runtime/directory.rs`.

### ~~Announce schema sin `schema_version`~~ ✅ Resuelto 2026-04-24
- `AnnouncePayload.schema_version: u32` con `#[serde(default = "default_schema_version")]` (retorna 1). Legacy payloads sin el campo siguen parseando como v1 — fully back-compat.
- Constante `ANNOUNCE_SCHEMA_VERSION = 1` expuesta en el crate. Bumpear cuando haya un cambio incompatible.
- `ExtensionDirectory` rechaza announcements con `schema_version > ANNOUNCE_SCHEMA_VERSION` con `tracing::warn!` explícito (ext id + version + schema numbers). Esto permite a extensiones futuras degradar limpio en agentes viejos.
- Tests: `announce_defaults_schema_version_to_one` + `announce_round_trip_preserves_schema_version`.

### Autenticación / firma del manifest_hash no verificada
- `AnnouncePayload.manifest_hash` se acepta pero no se valida. Extensión hostil podría anunciar id de una legítima.
- **Acción:** integrar verificación contra un allowlist firmado (llave pública por operador) y rechazar announces sin firma en modo `strict`.

### Credenciales NATS per-extension no soportadas
- `NatsRuntime` hereda el `BrokerHandle` del agente. No hay forma de aislar un extension untrusted en un subject-space o creds propios.
- **Acción:** cuando exista multi-tenant necesitamos spawn con un `BrokerHandle` secundario o enforcement de subject-prefix por extension+auth.

### ~~Heartbeat/liveness no aislado entre versiones concurrentes~~ ✅ Resuelto 2026-04-24
- `handle_announce` ahora hace replacement atómico en version-bump:
  1. intenta conectar la nueva versión,
  2. solo si handshake OK reemplaza en `entries` y emite `Removed(v1) + Added(v2)`.
- Si `v2` falla handshake, mantiene `v1` viva (no hay gap de liveness ni pérdida de runtime activo).
- Test de regresión: `crates/extensions/tests/directory_test.rs::failed_version_bump_keeps_previous_runtime_live`.

### ~~Breaker key collision con stdio (`ext:{id}`)~~ ✅ Resuelto 2026-04-24
- Keys ahora transport-prefixed: `ext:stdio:{id}` (en `runtime/stdio.rs`) y `ext:nats:{id}` (en `runtime/nats.rs`). Métricas + estado del breaker quedan aislados por transport, aún cuando el mismo id convive en ambos.

### ~~Tests de integración usan `LocalBroker`, no `NatsBroker` real~~ ✅ Resuelto 2026-04-24
- Nuevo test `crates/extensions/tests/directory_nats_e2e_test.rs` gated por `NATS_URL`: usa `NatsBroker` real + `async_nats` para simular la extensión y valida el flujo `announce -> Added` y `shutdown beacon -> Removed`.
- Mantiene aislamiento por run con `subject_prefix` único (`ext-e2e-<uuid>`) y cola SQLite temporal.
- Integrado en `scripts/integration_stack_smoke.sh` como paso `[9/9]` (`cargo test -p agent-extensions --test directory_nats_e2e_test` con `NATS_URL=nats://127.0.0.1:4222`).

### ~~`NatsRuntime::shutdown` no espera ACK de la extensión~~ ✅ Resuelto 2026-04-24
- Semántica documentada en templates de extensiones:
  - `extensions/template-python/README.md`: `shutdown` es best-effort y no debe ser el único punto de cleanup crítico.
  - `extensions/template-rust/README.md`: mismo warning para autores (ACK no garantizado bajo timeout/teardown).
- El comportamiento runtime no cambia: request best-effort con `shutdown_grace`; esta acción cierra la deuda de documentación para evitar supuestos incorrectos en extensiones nuevas.

---

## 🟡 Phase 11.7 — Extension CLI (follow-ups)

### ~~`agent ext install <path>` no existe~~ ✅ Resuelto 2026-04-23
- Subcomandos añadidos: `agent ext install <path> [--update|--link|--enable|--dry-run|--json]` y `agent ext uninstall <id> --yes [--json]`.
- Source siempre es directorio local (o path directo a `plugin.toml`). Archive + git clone quedan fuera de MVP (requerirían deps `tar`/`flate2`/`zip`/`git2`).
- Pre-validación: parse manifest, reject id reserved/colisión, reject target existente sin `--update`. `--link` requiere source absoluto (rechaza relativo).
- Writes via tmp sibling dir + `rename` atómico (copy y update). EXDEV detectado y reportado con mensaje claro.
- `copy_dir_all` preserva bits Unix de permisos — crítico para `bin/handler` stdio extensions. Test explícito valida `0o755`.
- `--enable` post-install remueve id de `disabled[]` en `extensions.yaml` (best-effort; si falla, install queda válido con warn a stderr).
- `--dry-run` ejecuta validación completa sin tocar filesystem.
- Uninstall borra dir o symlink (sin seguir el link — source externo intacto). `--yes` obligatorio; sin él exit 7.
- Exit codes estables: 0 ok, 1 not-found / update-missing, 2 invalid-manifest/source/link-not-absolute, 3 config-write, 4 invalid-id, 5 already-exists, 6 id-collision, 7 missing-confirmation, 8 copy-failed.
- Tests: 7 unit en `cli/install.rs` + 18 integration en `tests/install_test.rs` (copy/update/link/dry-run/enable/json, conflict rejects, reserved-id reject, collision detect, uninstall copy+symlink+missing-yes+unknown-id).
- Cableado en `src/main.rs`: `Mode::ExtInstall`/`ExtUninstall` + `ExtCmd::Install`/`Uninstall` + `run_ext_cli` dispatcher; `print_help` y `print_usage` actualizados.
- **Follow-ups abiertos**:
  - Archive source (`.tar.gz`/`.zip`) + git clone detrás de feature flags.
  - `--force-copy-swap` fallback si EXDEV aparece en prod.
  - `ext doctor --runtime` (test live de extensiones) sigue pendiente (item L536).
  - Security scan (binarios sospechosos) diferido — heurístico, fuera de MVP personal.

### ~~`ext doctor` no prueba extensiones vivas~~ ✅ Resuelto 2026-04-24
- Flag `--runtime` añadido: por cada extensión descubierta y no-disabled prueba su transport con timeouts acotados (stdio: spawn+handshake, nats: wait beacon, http: HEAD con fallback a GET cuando el server rechaza HEAD).
- Concurrencia limitada (default 4) via `futures::stream::buffer_unordered`.
- Stdio skip `shutdown_grace=1s`. NATS skip cuando broker local. HTTP fail en timeout/no-2xx.
- Trait `BrokerClientForDoctor` en `agent_extensions::cli` para desacoplar (evita dep circular con `agent-broker`); binario implementa `NatsDoctorAdapter` sobre `async_nats`.
- Config `ExtensionsConfig.doctor { stdio_timeout_ms=5000, nats_timeout_ms=5000, http_timeout_ms=3000, concurrency=4 }`.
- Output plain (tabla ID/TRANSPORT/OUTCOME/ELAPSED/ERROR) + `--json` con `{results[], summary{ok,fail,skip}}`.
- Exit codes: 0 ok, 9 uno o más fail, 2 doctor no ejecutable. `CliError::RuntimeCheckFailed(n)` nuevo.
- Tests: 5 unit + 9 integration (stdio happy con `echo_ext`, stdio fail binary missing, nats skip/ok/fail-timeout, http ok/fail vía wiremock, fallback HEAD->GET vía wiremock, disabled skip, JSON shape).
- Cableado en `src/main.rs`: `Mode::ExtDoctor { runtime, json }` + runtime tokio dedicado cuando `--runtime` activo.
- **Follow-ups abiertos**:
  - `--if-not-running` con pidfile para evitar competir con daemon vivo (stdio locks tipo whatsapp-rs session).
  - ~~HTTP GET fallback cuando servidor rechaza HEAD~~ ✅ Resuelto 2026-04-24 (`check_http` en `crates/extensions/src/cli/doctor.rs` hace retry con GET en 405/501).
  - Doctor contracts per-plugin (estilo OpenClaw: cada plugin aporta su checker).
  - ~~Full `tools/list` además del handshake~~ ✅ Resuelto 2026-04-24 (`check_stdio` ahora valida `tools/list` después de spawn+handshake).

### YAML rewrite pierde comentarios
- `serde_yaml::to_string` regenera todo el archivo. Comentarios y orden custom no sobreviven `enable`/`disable`.
- **Acción:** evaluar `yaml-edit` cuando madure, o fallback a regex surgical edit del array `disabled:`.

### ~~Sin file-lock en YAML rewrite~~ ✅ Resuelto 2026-04-24
- Advisory lock vía sibling file `extensions.yaml.lock` con `OpenOptions::create_new(true)`: primer proceso gana, segundo hace polling cada 25ms hasta `LOCK_TIMEOUT=5s`. Portable sin deps extra (sin `fcntl`/`flock`), compatible con tmpfs/NFS.
- `FileLock` con `Drop` remueve el lockfile automáticamente — incluso si el writer panica.
- Timeout claro cuando el lock está pegado: mensaje menciona el path para que el operador pueda borrar stale.
- 2 tests nuevos: `lock_blocks_concurrent_writer_until_released` (foreign lock + background release) y `lock_times_out_when_held_forever` (sanity de cleanup).

### ~~`enable` sobre `disabled` in-memory ≠ on-disk es silente~~ ✅ Resuelto 2026-04-24
- `run_enable` y `run_disable` ahora comparan `ctx.extensions.disabled` (in-memory, cargado al arranque) con el array `disabled[]` leído del yaml on-disk. Si divergen, emiten `tracing::warn!` con `yaml_added` / `yaml_removed` (las diferencias) antes de escribir.
- La operación sigue usando la versión on-disk (comportamiento original); el warn solo avisa al operador que hay ediciones manuales fuera de sync.
- Tests unit: `no_divergence_is_silent` + `divergent_sets_do_not_panic`.

### ~~No hay colorización / emoji~~ ✅ Resuelto 2026-04-24
- `agent ext list` ahora colorea el campo `STATUS` en modo tabla cuando corre en TTY:
  - `enabled` verde, `disabled` amarillo, `error` rojo.
- Auto-detección: respeta `NO_COLOR`; soporta `CLICOLOR=0` (off) y `CLICOLOR_FORCE=1` (force on). Bajo pipe/no-TTY queda monocromo.
- Implementación sin deps extra en `crates/extensions/src/cli/format.rs` (`should_color` + `colorize_status`).

### Parser hand-rolled crece más que `dlq`
- `parse_args` ya tiene 8 ramas Ext*. Mantenible por ahora, pero pasar a `clap` sería barato el día que añadamos `install`/`update`.
- **Acción:** migración a `clap` cuando lleguemos a 12+ subcomandos o requiramos flags compuestos.

### ~~JSON schema sin versionar~~ ✅ Resuelto 2026-04-24
- `agent ext list --json` ahora emite objeto versionado:
  - `{ "schema_version": 1, "rows": [...] }` (antes era array root sin versión).
- `agent ext info --json` ahora incluye `schema_version: 1` top-level junto al resto de campos.
- Implementación: `crates/extensions/src/cli/format.rs` (`CLI_JSON_SCHEMA_VERSION`) + wiring en `commands.rs`.
- Tests actualizados: `crates/extensions/tests/cli_test.rs::list_json_schema_stable` e `info_json_exposes_mcp_servers_key`; unit `cli::format::tests::json_schema_stable`.

### ~~Tests no verifican archivo temporal `.yaml.tmp` cleanup tras crash~~ ✅ Resuelto 2026-04-24
- Nuevo test `cli::yaml_edit::tests::rename_failure_cleans_tmp_file` en `crates/extensions/src/cli/yaml_edit.rs`.
- Fuerza fallo de `rename(tmp, path)` creando `path` como directorio; verifica:
  - retorna `CliError::ConfigWrite` de rename,
  - el `.yaml.tmp` se elimina en cleanup best-effort,
  - el target preexistente queda intacto.

---

## 🟡 Phase 12.1 — MCP client stdio (follow-ups)

### No reconnect automático tras crash del server MCP
- Si el proceso MCP muere, `StdioMcpClient::state` se marca `Failed{reason}`. El caller debe reconstruir el cliente manualmente.
- **Acción:** supervisor con backoff exponencial similar a 11.3/stdio; honrar tope `max_restart_attempts`. Integra con 12.4 session runtime.

### ~~Protocol version hardcoded~~ ✅ Parcialmente resuelto 2026-04-24
- `StdioMcpClient::connect` ahora emite `tracing::warn!` cuando el server responde con `protocolVersion` distinto al hardcoded (`2024-11-05`). Campos visibles: `mcp`, `client`, `server`. Cliente sigue operando con la versión del server (expuesta vía `client.protocol_version()`).
- Constante `PROTOCOL_VERSION` sigue fija; bump requiere commit manual cuando la spec publique 2025-xx-xx.

### ~~Sampling (server → client LLM request) no soportado~~ ✅ Resuelto 2026-04-23
- `sampling/createMessage` implementado: `crates/mcp/src/sampling/` con `SamplingProvider` trait + `DefaultSamplingProvider` (wraps `agent_llm::LlmClient`) + `SamplingPolicy` (rate limit token-bucket + per-server cap + deny list).
- `StdioMcpClient::connect_with_sampling(cfg, Option<Arc<dyn SamplingProvider>>)` — advertise `capabilities.sampling` condicional en `initialize`.
- Reader dispatcha server-originated requests (método+id) a `incoming_handler_task` vía mpsc dedicado; response se escribe al writer existente.
- Mapping: `messages` MCP (text-only) → `ChatMessage::user/assistant`; `modelPreferences.hints` → exact match contra `provider()`/`model_id()` de clientes named + fallback a default; `temperature/maxTokens/stopSequences` → `ChatRequest`; `finish_reason` → `stopReason` (`Stop→endTurn`, `Length→maxTokens`, `ToolUse→endTurn`+warn).
- Rejects: multimodal content → `-32602`, tool_calls response → `-32603`, rate-limited → `-32000`, disabled → `-32000`, no provider → `-32601 method not found`.
- `SamplingPolicy::cap()` trunca `max_tokens` a `min(per_server_cap, global_cap)` sin rechazar.
- Tests: 20 unit (`wire`, `policy`, `default_provider`, `mod::error_codes`) + 3 integration (`tests/sampling_test.rs`: happy path, disabled returns -32601, llm-failure returns -32603). Mock server extendido con `MOCK_MODE=sampling_trigger`.
- **Follow-ups abiertos**:
  - Multimodal content (image/audio) — requiere extender `ChatRequest`.
  - Human-in-the-loop approval via canal de usuario (WhatsApp/Telegram) — nuevo `ConfirmationGatedProvider`.
  - `includeContext: "thisServer"/"allServers"` — hoy se ignora con warn.
  - Hot reload de `sampling_provider` cuando `mcp.yaml` cambia — provider se fija al startup.
- **Original note** (now stale):
- MCP spec permite al server invocar LLM del cliente vía `sampling/createMessage`. 12.1 ignora estas requests con log-debug.
- **Acción:** cuando sea requerido, integrar con `agent_llm::LlmClient` y manejar `sampling/*` requests en reader_task.

### ~~Phase 12.1 (follow-up) — métricas Prometheus de sampling~~ ✅ Resuelto 2026-04-24
- `crates/mcp/src/telemetry.rs` agrega:
  - `agent_mcp_sampling_requests_total{server,outcome}` (counter).
  - `agent_mcp_sampling_duration_ms{server,le}` (histogram).
- `crates/mcp/src/client.rs::handle_incoming` instrumenta `sampling/createMessage`:
  - outcome labels: `ok`, `unsupported`, `invalid_params`, `disabled`, `rate_limited`, `tool_calls_rejected`, `llm_error`, `no_provider`.
  - duración medida end-to-end del request de sampling (parse + provider.sample + encode response/error).
- No requiere wiring extra: `/metrics` ya concatena `agent_mcp::telemetry::render_prometheus()`.

### ~~Phase 12.1 (follow-up) — wiring de `mcp.sampling` en main~~ ✅ Resuelto 2026-04-24
- `crates/config/src/types/mcp.rs` ahora incluye `mcp.sampling`:
  - `enabled`, `default_hint`, `global_max_tokens_cap`, `deny_servers`, `per_server.{enabled,rate_limit_per_minute,max_tokens_cap}`.
- `src/main.rs` construye `DefaultSamplingProvider` desde `LlmRegistry` + modelos de agentes y lo inyecta a `McpRuntimeManager::new_with_sampling(...)`.
- `crates/mcp/src/manager.rs` y `crates/mcp/src/session.rs` propagan ese provider a conexiones stdio (`StdioMcpClient::connect_with_sampling`), habilitando capability `sampling` en handshake cuando aplica.
- `config/mcp.yaml` documenta la sección `sampling` (default off).

### ~~Sin validación client-side del input_schema~~ ✅ Ya cubierto 2026-04-24
- MCP tools se registran como `McpTool` (handler) en el `ToolRegistry` del agente. El `LlmAgentBehavior::execute_one_call` ya pasa cada `args` por `ToolArgsValidator` antes de invocar el handler (gated por `agents[].tool_args_validation.enabled`, default true).
- Resultado: los args del LLM contra tools MCP se validan client-side igual que tools nativas o de extensión. No hace falta duplicar en el cliente MCP.
- Errores se surface como `outcome="invalid_args"` con JSON pointer del campo ofensor, el LLM puede reintentar sin round-trip al server.
- La `Acción` original (flag `strict_schema` + `jsonschema` crate en el cliente) queda innecesaria — el validator actual usa `jsonschema` vía feature `schema-validation` (default on).

### `rmcp` SDK oficial sin evaluar
- Implementación manual cubre lo necesario pero duplica trabajo con el SDK oficial (cuando madure). Re-evaluar cuando rmcp alcance 1.0.
- **Acción:** benchmark rmcp vs actual cuando llegue a beta estable; migración podría simplificar mantenimiento.

### ~~No hot-reload de catálogo en `tools/list_changed`~~ ✅ Resuelto 2026-04-23
- StdioMcpClient + HttpMcpClient emiten `ClientEvent` en broadcast, `SessionMcpRuntime::on_tools_changed` wirea callback con debounce 200 ms. main.rs reacciona con `ToolRegistry::clear_by_prefix` + `register_session_tools`.

### ~~Stderr del server llega siempre como `warn`~~ ✅ Resuelto 2026-04-23
- `notifications/message` con `level` RFC5424-subset se parsea y emite via `tracing::event!` con level mapeado (debug/info/warn/error). Stderr baja a `tracing::debug!` porque ya no es el canal primario. Módulo `crates/mcp/src/logging.rs`. Tests: `log_notification_from_mock_is_observable` + 4 unit tests.

### ~~`logging/setLevel` client request para silenciar servers noisy~~ ✅ Resuelto 2026-04-23
- Per-server `log_level: "warning"` en `mcp.yaml` (stdio + streamable_http + sse). Client hace `set_log_level` post-initialize cuando server declara `capabilities.logging`. Validación local contra spec-set; capability check antes del wire. Mock server mode `logging_capable` + `MOCK_SETLEVEL_LOG`. Tests: `set_log_level_applies_and_server_acks`, `set_log_level_fails_without_capability`, `set_log_level_rejects_invalid_level`. Trait `McpClient::set_log_level` default retorna `Protocol("not supported")`.

### ~~mcp.yaml hot-reload no propaga nuevo `log_level` a clientes vivos~~ ✅ Resuelto 2026-04-23
- `McpRuntimeManager::update_config` calcula diff old/new de `levels_map` por-servidor y spawns `set_log_level` por cada live client que cambió. Timeout 2s per client. Idempotente: mismo level old→new = no-op. None→Some aplica. Test: `mock_server_test::update_config_hot_reloads_log_level_on_live_client`.

### ~~`Some→None` log_level reset a default~~ ✅ Resuelto 2026-04-23
- Nuevo flag `mcp.reset_level_on_unset: bool` (default false). Cuando true y un server pasa de `Some(lvl) → None` en hot-reload, `McpRuntimeManager::update_config` emite `set_log_level("info")` al live client en lugar de dejar el previo. Tests: unit `levels_map_diffs_include_reset_when_flag_on` + integration `update_config_resets_level_to_info_when_flag_on_and_unset`.

### ~~`shutdown()` no envía `notifications/cancelled` para in-flight calls~~ ✅ Resuelto 2026-04-23
- Ambos `StdioMcpClient` y `HttpMcpClient` emiten `notifications/cancelled {requestId, reason:"client shutdown"}` por cada request vivo antes de tear-down. Timeout global 100 ms protege contra servers lentos. HTTP tracking de inflight via `DashMap<u64,()>` para que StreamableHttp (que no usa pending map) también participe. Writer task stdio con `biased` + drain para no perder mensajes en el race de cancel. Tests: `mock_server_test::shutdown_sends_cancelled_for_pending`, `http_client_test::shutdown_sends_cancelled_streamable`.

### Mock server como `[[bin]]` en lugar de `[[example]]`
- Compila con `cargo build --workspace`. Overhead ~0.1s. Hecho así porque `env!("CARGO_BIN_EXE_*")` no aplica a examples.
- **Acción:** cuando stable, migrar a `CARGO_TARGET_TMPDIR` + example o a workspace separado de test-utils.

### ~~Rate limiting per-tool ausente~~ ✅ Resuelto 2026-04-23
- `ToolRateLimiter` (token bucket non-blocking) en `crates/core/src/agent/rate_limit.rs`. Config `agents[].tool_rate_limits.patterns` con glob `*` + `_default` reservado. `LlmAgentBehavior::with_rate_limiter` wireado en main.rs; denial surface a LLM via `outcome="rate_limited"` ya tracked por métrica existente. Tests: 5 unit (glob + bucket burst/refill + selection + per-key isolation).

---

## 🟡 Phase 12.3 — MCP tool catalog (follow-ups)

### Dedupe por skip+warn pierde tools en vez de renombrar
- Si dos MCP servers legítimos exponen el mismo `prefixed_name`, el segundo se descarta. OpenClaw usa sufijos numéricos (`_2`, `_3`) para conservar ambos.
- **Acción:** migrar a política de sufijo si aparece un caso real donde el operador quiera ambos.

### ~~Sin hot-reload en `notifications/tools/list_changed`~~ ✅ Resuelto 2026-04-23
- 12.8 añade broadcast `ClientEvent` en ambos transports + `SessionMcpRuntime::on_tools_changed` con debounce 200 ms. main.rs re-registra tools al recibir notif.

### ~~`MAX_TOOL_NAME_LEN` duplicado en `extension_tool` y `mcp_tool`~~ ✅ Resuelto 2026-04-23
- `agent_llm::ToolDef::MAX_NAME_LEN = 64` (docstring cita OpenAI/Anthropic spec). `extension_tool`, `mcp_tool`, `mcp_catalog`, `mcp_resource_tool` todos importan desde `agent-llm`. Re-export removido de `agent_core::agent::mod.rs`. Zero callsites legacy residuales.

### `flatten_content` pierde estructura para LLMs modernos
- Aplanamos a string. LLMs con vision (GPT-4V, Claude Opus+Vision) podrían consumir image blocks nativamente.
- **Acción:** cuando `agent-llm` soporte multi-modal tool outputs, exponer `raw_content()` alterno.

### ~~Sin métricas per-tool~~ ✅ Resuelto 2026-04-23
- `agent_tool_calls_total{agent,tool,outcome}` + `agent_tool_latency_ms{agent,tool}` en `crates/core/src/telemetry.rs`. Wire en `LlmAgentBehavior::run_turn` con outcomes `ok|error|blocked|unknown`. Prefijo `tool` codifica origen (`mcp_`, `ext_`, nativo). Test: `telemetry::tests::tool_metrics_render_correctly`.

### Resources + prompts no participan del catalog
- MCP permite `resources/list` y `prompts/list`. Catalog actual solo maneja tools.
- **Acción:** 12.5 introduce soporte. McpToolCatalog debería extenderse a `McpCapabilityCatalog` o coexistir con un `McpResourceCatalog` separado.

### ~~`register_into` no valida colisión con tools nativos~~ ✅ Resuelto 2026-04-23
- `ToolRegistry::register_if_absent(def, handler) -> bool` + `contains(&str) -> bool` usando `DashMap::Entry` atómico. `McpToolCatalog::register_into` + meta-tools resource log warn estructurado en colisión y suman `skipped_collision` al summary. Tests: `tool_registry::tests::register_if_absent_preserves_original`, `mcp_resource_tool_test::register_into_does_not_overwrite_native`.

### Mock dev-dep adds ~2s to `cargo test` cold start
- `agent-core` tests builden el bin de `agent-mcp` si no existe. Overhead aceptable.
- **Acción:** none; documentado en `tests/mcp_catalog_test.rs`.

### ~~MCP `_meta` simétrico a extensions~~ ✅ Resuelto 2026-04-23
- `mcp.context.passthrough: true` en `mcp.yaml` activa inyección de `params._meta = { agent_id, session_id }` en todo `tools/call`. Trait `McpClient::call_tool_with_meta` default delega a `call_tool` (preserva mocks). `StdioMcpClient` + `HttpMcpClient` override. `McpTool::with_context_passthrough` builder; `McpToolCatalog::build_with_context` + `register_session_tools_with_context`. Mock server `echo_meta` + 2 integration tests.

### ~~MCP `_meta` en `resources/list` y `resources/read`~~ ✅ Resuelto 2026-04-23
- Extensión simétrica: `McpClient::list_resources_with_meta` + `read_resource_with_meta` default deleg a sin-meta. Stdio + HTTP overrides. `McpResourceListTool` + `McpResourceReadTool` gain `with_context_passthrough` builder; `register_resource_meta_tools` recibe flag del catalog. Mock `echo_meta` extendido con `resources/list` y `resources/read` echoing `_meta`. 2 integration tests.

---

## 🟡 Phase 12.4 — Session-scoped MCP runtime (follow-ups)

### ~~File watcher para hot-reload de `mcp.yaml`~~ ✅ Resuelto 2026-04-23
- `agent_mcp::config_watch::spawn_mcp_config_watcher` con `notify-debouncer-full`. Opt-in via `mcp.watch.enabled: true`, debounce configurable (default 500 ms). YAML inválido / env vars missing loggean warn y skip — última config válida sigue corriendo. Re-usa `ExtensionServerDecl` del boot (no re-descubre extensions). Tests: `crates/mcp/tests/config_watch_test.rs`.

### ~~Callback genérico en `SessionManager::delete`~~ ✅ Resuelto 2026-04-23
- `SessionManager::on_expire(Fn(Uuid))` acumula callbacks; `delete()` y el sweeper TTL ambos disparan via `tokio::spawn`. Wireado en `main.rs` con `McpRuntimeManager::dispose_session` (global) y `MemoryGitRepo::commit_all` (per-agent).

### ~~`invalidate_catalog` no wireada con `notifications/tools/list_changed`~~ ✅ Resuelto 2026-04-23
- `SessionMcpRuntime::on_tools_changed<F>` + broadcast `ClientEvent` en stdio/http. main.rs spawnea callback que limpia por prefijo y re-registra tools. invalidate_catalog queda como `touch()` + log (el único cache real vive en ToolRegistry).

### ~~Sin métricas per-session~~ ✅ Resuelto 2026-04-23
- `agent_mcp::telemetry` expone `mcp_sessions_active` (gauge), `mcp_sessions_created_total` (counter), `mcp_sessions_disposed_total{reason}` (counter), `mcp_session_lifetime_secs` (histogram con buckets `[5,30,60,300,900,1800,3600,7200]`). Wire: `build_runtime` (inc created+active), `dispose_with_reason` (dec active + disposed{reason} + observe lifetime). Reap usa reason `"reap"`. `main.rs` concatena `render_prometheus()` al endpoint /metrics. Tests: 3 unit.

### Persistencia del catálogo cacheado
- Cada session rebuild todos sus clients cold. Para agentes con muchas sessions efímeras (chat bots), listar tools N veces es caro.
- **Acción:** compartir un catálogo inmutable por fingerprint entre sessions (second-level cache global). Requiere disciplina de invalidation cross-session.

### Rate limiting per-session
- Breakers son per-server-globales. Una session puede agotar cuota de tools a costa de otras.
- **Acción:** token bucket per `(session_id, server)` en `call_tool`. Default off.

### Sin binding externo `sessionKey → sessionId`
- `SessionManager` ya los resuelve, pero los clientes de `McpRuntimeManager` deben hacer lookup manualmente.
- **Acción:** helper `manager.get_or_create_by_key(session_manager, external_key)` que resuelve sessionKey → sessionId y propaga a mcp.

### `ManagerHandle` indirección fea
- `McpRuntimeManager::clone_arc` crea un mini-struct para pasar al build future y evitar `Arc<Self>` cíclico. Funciona, pero se nota.
- **Acción:** refactor con `Arc::downgrade(&self)` + `Weak::upgrade` dentro del future; menos tipos auxiliares.

### `build_session_catalog` no cachea
- Cada llamada rebuilds el catálogo. El caller debe guardar el `Arc<McpToolCatalog>` si quiere cachear.
- **Acción:** opcional cache en `AgentContext` con fingerprint + catalog guarded por `invalidate_catalog`. Considerar después de implementar `on_notification`.

### ~~Wiring en `main.rs` pendiente~~ ✅ Resuelto 2026-04-23
- 12.4 expone `AgentContext::with_mcp` pero `src/main.rs` no construye el manager ni lo ataca a los contexts por defecto. Falta el paso concreto de "si `AppConfig.mcp` is Some(cfg) → `McpRuntimeManager::new(McpRuntimeConfig::from_yaml(&cfg))` + `ctx = ctx.with_mcp(manager.get_or_create(session_id).await)`".
- **Acción:** integración en `main.rs` cuando 12.6/12.7 demanden MCP en producción.

---

## 🔵 Phase 10.9 — Git-backed memory workspace (deferred)

Formalizada en `PHASES.md` como sub-fase 10.9. Resumen del trade-off para no olvidar por qué se difirió:

- **Beneficio real:** forensics (`git log MEMORY.md`), rollback de dreaming corrupto, blame de memories malas a sesiones concretas, diffs como formato eficiente para re-alimentar al LLM (DiffMem), multi-host via clone.
- **Coste bajo:** ~200 LOC + `git2` crate. Sin shell-out, sin remote por default.
- **Por qué espera:** valor aparece sólo cuando hay deploys productivos que generan bugs reales de memoria. Hoy cero tráfico real. Premature antes de Phase 6 (WhatsApp, desbloquea uso) y antes de integrar MCP en `main.rs`.
- **Trigger para retomar:**
  1. Primer incidente real de "por qué el agente dijo X ayer" sin historial
  2. Primera corrupción de `MEMORY.md` tras dreaming sweep
  3. Demo público donde `git log` vende el diferencial vs otros frameworks
- **Dependencia oculta:** sub-fase 10.9.3 (session-close hook) necesita el callback `on_session_expire` en `SessionManager` — mismo follow-up ya listado en Phase 12.4. Land los dos juntos.

---

## 🟡 Phase 12.2 — MCP HTTP/SSE (follow-ups)

### SSE reconnect automático no implementado
- Si el stream muere, la tarea termina; calls pendientes recibirán `McpError::TransportLost`. No hay retry con backoff.
- **Acción:** spawn un supervisor que re-conecte con backoff exponencial 1s→30s tras N (`sse_reconnect_max_attempts`) intentos; drop pending al reconnect.

### ~~Mcp-Session-Id invalidation mid-call~~ ✅ Resuelto 2026-04-23
- `post_streamable_raw` ahora wrapea `post_streamable_once`: captura `had_session` antes del POST; si response es `HttpStatus { status: 404, .. }` y había session, dropea `session_id`, loggea `warn`, y reintenta sin header. Single retry (no loop). Body reutilizable vía `String::clone()`.
- Tests en `crates/mcp/tests/http_client_test.rs`: `streamable_retries_without_session_on_404` (server retorna 404 con header stale, acepta retry sin header, verifica ambos conteos); `streamable_does_not_retry_404_when_no_session` (404 en initialize sin session surfacea como error, no loop).

### ~~Sin auto-detección de transport (`transport: auto`)~~ ✅ Resuelto 2026-04-23
- Variante `McpServerYaml::Auto` + `HttpTransportMode::Auto`. `HttpMcpClient::connect` intenta `handshake_streamable` primero; si error es `HttpStatus { 404 | 405 | 415 }` (vía helper `is_streamable_unsupported`), dropea session y reintenta vía `handshake_sse`. Transport resuelto queda persistido en `self.transport` (accesor `transport()` nunca retorna `Auto` post-connect).
- Manifest validation: Auto validado como URL http/https (mismo path que StreamableHttp/Sse).
- Tests: yaml parse (`parses_auto_variant`), `auto_picks_streamable_when_available`, `auto_surfaces_non_fallback_errors` (500 no dispara fallback). Test de fallback sobre SSE real está `#[ignore]` por la misma limitación del axum mock SSE ya conocida.

### OAuth 2.1 flow no soportado
- Solo static bearer tokens via headers. Servers que exigen OAuth refresh no funcionan.
- **Acción:** integrar cuando el spec MCP de auth se estabilice; requerirá token store + refresh loop.

### TLS cert pinning + mTLS client certs
- Actualmente confiamos en `rustls-tls` default (system store). Para entornos air-gapped + pinned certs falta config.
- **Acción:** exponer `tls: { ca_cert: <path>, client_cert: <path>, client_key: <path>, pinned_sha256: [...] }` en `McpServerYaml`.

### ~~Sin validación de caracteres en header values~~ ✅ Resuelto 2026-04-23
- `McpConfig::validate()` recorre headers de todos los transports HTTP (StreamableHttp/Sse/Auto) y rechaza cualquier char fuera de RFC 7230 field-value (HTAB + 0x20–0x7E). `AppConfig::load` invoca `validate()` tras parse; non-ASCII o control chars fallan ahora en `agent run` en lugar de drop silencioso en runtime.
- Tests: 3 unit (`validate_rejects_non_ascii_header_value`, `validate_rejects_control_char_header_value`, `validate_accepts_legal_header_values`).

### Tests SSE legacy no cubren response correlation
- `sse_connect_receives_endpoint_event` está `#[ignore]` porque axum no nos da fácil push de responses sobre el stream sin más código.
- **Acción:** implementar `tokio::sync::broadcast` + SSE stream wrapper para simular server real; habilitar el test.

### Streamable HTTP con body SSE no busca notifications antes del response
- Parser consume todo el stream y filtra por id. Semántica correcta, pero un server muy lento puede hacer que el timeout corra antes de que llegue el response.
- **Acción:** reader incremental que salga tan pronto como vea el id esperado, en lugar de drain completo.

### Compression explícita
- reqwest maneja gzip/br transparentemente si sus features están on (no están en nuestra config).
- **Acción:** si algún server lo requiere, habilitar `reqwest` features `gzip`/`brotli`.

### ~~Cookie jar persistence~~ ✅ Resuelto 2026-04-24
- `reqwest::Client::builder().cookie_store(true)` en `HttpMcpClient::connect`. Workspace habilita feature `cookies` en reqwest.
- Spec MCP no requiere cookies; esto cubre servers Streamable HTTP custom que rutean con `Set-Cookie` (CDN sticky sessions).

### Mcp-Session-Id disk persistence entre restarts del agente
- La session se pierde al restart. Algunos servers usan esto para caching.
- **Acción:** follow-up tras 12.4 hot-reload — persist a sqlite/file y re-enviar al reconnect.

---

## 🟡 Phase 12.5 — MCP resources (follow-ups)

### Sin `resources/subscribe` + notifications/resources/updated
- Live resources (DB rows que cambian, files watched) no se reflejan en tiempo real. Cliente debe re-leer para ver updates.
- **Acción:** añadir método `subscribe(uri)` al trait + reader task que despache `notifications/resources/updated` a un canal mpsc. Scope separado.

### ~~Sin auto-refresh en `notifications/resources/list_changed`~~ ✅ Resuelto 2026-04-23
- `SessionMcpRuntime::on_resources_changed<F>` simétrico a `on_tools_changed`. Reusa helper `debounce_event(matcher)`; main.rs wirea un 2do callback que también llama `register_session_tools`. Mock server añadió mode `notify_resources`; integration test en `crates/mcp/tests/hot_reload_test.rs`.

### ~~URI templates no soportadas (`resources/templates/list`)~~ ✅ Resuelto 2026-04-23
- `McpClient::list_resource_templates` en trait + stdio/HTTP con paginación (`MAX_PAGES`). Tipos `McpResourceTemplate` + `ResourceTemplatesPage`. Meta-tool `mcp_{server}_list_resource_templates` registrado cuando `capabilities.resources`; devuelve `[{uri_template, name, description, mime_type}]`. Mock server añadió `resources/templates/list` en mode `resources` (2 templates: `file:///{path}`, `log://{date}/{level}`). Tests: `list_resource_templates_handler_returns_array`, `templates_tool_skipped_when_capability_absent`.
- **Nota:** expansión RFC-6570 `{key}` → URI concreta queda del lado del LLM (usa los templates para construir `uri` y llama `read_resource`). Si se quiere expansión lado-servidor, crear follow-up separado.

### Blob bytes devueltos como marker, no bytes reales
- LLM ve `[blob: image/png, 5 bytes]` en lugar del contenido. Provider-specific multi-modal decoding queda fuera.
- **Acción:** cuando `agent-llm` soporte tool outputs multi-modal (imágenes in-context), devolver blob directo encoded como data URL.

### ~~Sin resource-level caching~~ ✅ Resuelto 2026-04-23
- `agent_mcp::ResourceCache` (LRU via `lru = "0.12"` + TTL) keyed por `(server_name, uri)`. Owned por `SessionMcpRuntime` (accesor `resource_cache()`); `McpToolCatalog::with_resource_cache` inyecta el cache en `McpResourceReadTool`. Bypass automático cuando `context_passthrough=true` (evita reuso cross-agent de contenido session-specific). Solo cachea responses con `text` (skip blob-only para bound memory).
- Config yaml: `mcp.resource_cache.{enabled, ttl, max_entries}` (default off; ttl=30s; max=256).
- Invalidación: main.rs `on_resources_changed` callback llama `rt.resource_cache().invalidate_server(server_id)` antes del re-register (logged como `cache_purged`).
- Métricas Prometheus (2026-04-23): `mcp_resource_cache_hits_total{server}` + `mcp_resource_cache_misses_total{server}` en `agent_mcp::telemetry::render_prometheus()` (main.rs concatena al `/metrics` endpoint).
- Tests: 6 unit en `crates/mcp/src/resource_cache.rs`, 2 unit telemetry (`resource_cache_counters_render`, `resource_cache_counters_empty_default`), 2 integration en `mcp_resource_tool_test.rs`.

### Auto-inject opcional en memoria
- Resources que el agente marca como "importante" podrían flow a `MEMORY.md` via dreaming.
- **Acción:** follow-up Phase 10 — dreaming sweep puede llamar `list_resources` + condensar los top N a una sección `## External Resources` en MEMORY.md. Requires design.

### ~~Resource annotations ignored~~ ✅ Resuelto 2026-04-23
- Nuevo struct `McpAnnotations { audience: Vec<String>, priority: Option<f32> }` en `types.rs`; `McpResource.annotations: Option<McpAnnotations>`. `McpResourceListTool` emite `annotations: { audience: [..], priority: f64 }` en el output JSON cuando están presentes (omitido cuando ambos campos están vacíos). Mock server enriquece el recurso `readme` con audience=`["assistant"]`, priority=0.9.
- Tests: 2 unit en `types.rs` (`resource_annotations_deserialize`, `resource_annotations_absent_ok`) + integration `list_resources_surfaces_annotations` en `mcp_resource_tool_test.rs`.

### ~~Sin validación de URI scheme~~ ✅ Resuelto 2026-04-23
- Config yaml `mcp.resource_uri_allowlist: [file, db, ...]` (empty = permissive, default). Threading: `McpConfig → McpRuntimeConfig → SessionMcpRuntime.resource_uri_allowlist → McpToolCatalog → McpResourceReadTool.uri_allowlist`. En `call`, si la lista no está vacía y el scheme del URI solicitado no matchea, emite `tracing::warn!` e incrementa `mcp_resource_uri_allowlist_violations_total{server}` (expuesto en `/metrics`). La llamada **procede** intencionalmente (permisivo) — el LLM recibe la respuesta del server, el operador ve el signal.
- Tests: `read_resource_uri_outside_allowlist_counts_violation` (scheme fuera de set, violación = 1), `read_resource_uri_in_allowlist_no_violation` (scheme OK, sin violation).

### Tool name budget ajustado para resource suffixes
- `mcp_{server}_read_resource` cabe con `server` ≤ 45 chars; `_list_resources` con ≤ 44. Servers con nombre largo pierden el meta-tool silenciosamente (warn).
- **Acción:** abreviar sufijos cuando validate_name falla (`_list` / `_read` como fallback) en lugar de skip.

### Sin integración con `agent ext` CLI para debug
- `agent ext list` muestra extensions; no hay equivalente `agent mcp list` para ver servers y sus capabilities.
- **Acción:** sub-command `agent mcp list/info` paralelo a ext.

---

## 🟡 Phase 12.7 — MCP in extension manifests (follow-ups)

### ~~Dynamic enable/disable no invalida MCP runtime~~ ✅ Resuelto 2026-04-24
- `spawn_mcp_config_watcher` ahora observa también `config/extensions.yaml` (además de `mcp.yaml`) y recompone declaraciones MCP de extensiones en cada cambio estable.
- Al detectar cambio válido, recalcula `McpRuntimeConfig::from_yaml_with_extensions(...)` y ejecuta `McpRuntimeManager::update_config(...)` sin reinicio.
- Si `extensions.yaml` no parsea, emite `warn` y mantiene el último set válido de extension servers (fail-open para no tumbar hot-reload de `mcp.yaml`).
- Cobertura: `reload_on_extensions_enable_disable_edit` en `crates/mcp/tests/config_watch_test.rs`.

### ~~Sin env-expansion en manifest values~~ ✅ Resuelto 2026-04-24
- `McpRuntimeConfig::from_yaml_with_extensions` ahora aplica expansión de placeholders `${ENV}`/`${VAR:-default}`/`${file:...}` sobre valores declarados en `mcp_servers` de extensiones (después de `${EXTENSION_ROOT}`).
- Política fail-open: si un placeholder no resuelve, se emite `warn` y se conserva el literal (no se descarta la declaración completa).
- Cobertura: `from_yaml_with_extensions_expands_manifest_env_placeholders` + `apply_manifest_env_placeholders_fail_open_when_missing_var` en `crates/mcp/src/runtime_config.rs`.

### ~~Solo `${EXTENSION_ROOT}` placeholder~~ ✅ Resuelto 2026-04-24
- `apply_extension_root` ahora también sustituye `${EXTENSION_ID}`, `${EXTENSION_VERSION}` y `${AGENT_VERSION}` (en `command`/`args`/`env`/`cwd`/`log_level` y headers HTTP).
- Wiring completado extremo a extremo:
  - `agent_extensions::ExtensionMcpDecl` incluye `ext_version`.
  - `agent_mcp::runtime_config::ExtensionServerDecl` incluye `ext_version`.
  - `src/main.rs` y tests puentean `ext_version` al merge runtime.
- Tests: `apply_extension_root_substitutes_id_version_and_agent_version` + regresión de `mcp_manifest_test`.

### ~~Path escape es warn, no block~~ ✅ Resuelto 2026-04-24
- `McpConfig.strict_root_paths: bool` (default `false` — back-compat). Cuando `true`, `McpRuntimeConfig::from_yaml_with_extensions` **rechaza** (no incluye en `servers`) cualquier declaración MCP de extensión cuyo `command`/`args`/`cwd` escape su `ext_root`. `tracing::error!` con ext_id + path + root.
- Comportamiento default preservado: warn-only.
- Tests: `strict_root_paths_rejects_escaping_extension_command` (con flag=true, server dropeado; yaml-native intacto) y `strict_root_paths_off_still_loads_with_warn` (regresión guard).

### ~~Sidecar `.mcp.json` no soportado~~ ✅ Resuelto 2026-04-24
- Discovery de extensiones ahora soporta fallback sidecar: si `plugin.toml` no declara `[mcp_servers]` y existe `<ext_root>/.mcp.json`, carga `mcpServers`/`mcp_servers` y lo convierte a `McpServerYaml`.
- Soporta `stdio`/`streamable_http`/`sse`/`auto` (y aliases comunes `streamable-http`, `http`). Si `transport` falta, infiere `stdio` por `command` o `auto` por `url`.
- El sidecar se valida con `manifest.validate()`; si es inválido, emite `warn` y no bloquea el plugin (se ignora solo la parte sidecar).
- Cobertura: `scan_loads_sidecar_mcp_when_manifest_has_none`, `scan_ignores_sidecar_when_manifest_declares_mcp_servers`, `scan_invalid_sidecar_emits_warn_and_keeps_candidate`.

### ~~Breaker key incluye `.` del namespace~~ ✅ No aplica 2026-04-24
- Revisado: Prometheus label **values** aceptan `.` sin problemas (solo los label **names** están restringidos a `[a-zA-Z_][a-zA-Z0-9_]*`).
- `render_prometheus` en `crates/core/src/telemetry.rs` ya escapa valores según RFC (backslash + quote). No hay bug.
- Si eventualmente se necesita sanitizar por interoperabilidad con otra herramienta (Grafana alerts, etc.), añadir helper `sanitize_breaker_key` local. Por ahora no hace falta.

### ~~CLI JSON schema bump~~ ✅ Resuelto 2026-04-24
- `ext info --json` y `ext list --json` ya exponen `schema_version: 1` top-level (contrato explícito para scripts/clients).
- `ext list --json` quedó como objeto `{schema_version, rows}`; `ext info --json` mantiene shape previa + `schema_version`.

### ~~Main.rs wiring pendiente~~ ✅ Resuelto 2026-04-23
- `collect_mcp_declarations` + `from_yaml_with_extensions` listos; `src/main.rs` aún no los wirea al arranque.
- **Acción:** misma acción que 12.4: `McpRuntimeManager::new(McpRuntimeConfig::from_yaml_with_extensions(...))`.

### ~~Extension stdio command relative paths~~ ✅ Resuelto 2026-04-24
- `McpRuntimeConfig::from_yaml_with_extensions` ahora emite:
  - `warn` cuando un `mcp_servers.*` stdio command es relativo y no usa `${EXTENSION_ROOT}`,
  - `debug` con el path resuelto (`ext_root.join(command)`).
- Helper: `detect_relative_stdio_without_root_placeholder`.
- Tests: `detect_relative_stdio_warns_without_extension_root_placeholder` y `detect_relative_stdio_ignores_absolute_and_root_placeholder` en `crates/mcp/src/runtime_config.rs`.

### ~~Collision sin namespace colisiona con yaml no-namespaced~~ ✅ Resuelto 2026-04-24
- Se documentó que keys con `.` en `mcp.yaml` quedan reservadas para shadowing explícito de extensiones (`{ext_id}.{name}`), tanto en:
  - schema (`crates/config/src/types/mcp.rs`, campo `servers`),
  - sample config (`config/mcp.yaml`),
  - comentario del merge runtime (`crates/mcp/src/runtime_config.rs::from_yaml_with_extensions`).

---

## 🟡 Phase 12.6 — Agent as MCP server (follow-ups)

### HTTP/Streamable HTTP transport no implementado
- Solo stdio. Clientes que esperan un endpoint remoto (OAuth, multi-tenant) no pueden usarlo.
- **Acción:** `run_http_server(handler, bind_addr, auth)` con axum; bearer token vía config; rate limit simple.

### ~~Bridge registra sólo `WhoAmITool` por default~~ ✅ Resuelto 2026-04-24
- `agent mcp-server` ahora registra:
  - `who_am_i` + `what_do_i_know` siempre,
  - `my_stats` cuando long-term memory puede inicializarse,
  - `memory` cuando long-term memory está disponible y el agente declara plugin `memory`.
- Bootstrap de memoria en modo mcp-server es best-effort (lee `memory.yaml` opcional; si falta o falla, sigue arrancando con warning y sin memory tools).
- Proxies `ext_*` / `mcp_*` quedan gobernados por `allowlist`/`expose_proxies` (follow-up dedicado también cerrado).

### Sin `chat_with_agent` meta-tool
- El valor principal sería "Claude Desktop pregunta al agente algo" → correr LLM loop completo. Scope grande, follow-up.
- **Acción:** nuevo tool `chat` que dispara turn completo con el agente configurado; requiere AgentRuntime wired.

### ~~Sin resources/prompts server-side~~ ✅ Resuelto 2026-04-24
- `McpServerHandler` ahora soporta métodos opcionales server-side para:
  - `resources/list`, `resources/read`, `resources/templates/list`,
  - `prompts/list`, `prompts/get`.
- `server::stdio::dispatch` enruta esos métodos y responde envelopes MCP válidos (`resources`, `contents`, `prompts`, `messages`), con validación de params requeridos y mapeo de errores `Protocol -> -32602`.
- `ToolRegistryBridge` expone recursos de workspace:
  - `agent://workspace/soul` ↔ `SOUL.md`,
  - `agent://workspace/memory` ↔ `MEMORY.md`,
  junto con prompts templated `workspace_soul_context` / `workspace_memory_context`.
- `initialize.capabilities` del bridge ahora anuncia `tools`, `resources` y `prompts`.
- Cobertura añadida:
  - `server::stdio`: `resources_list_and_read_roundtrip`, `prompts_list_and_get_roundtrip`.
  - `core::mcp_server_bridge`: `list_resources_exposes_workspace_docs`, `read_resource_returns_workspace_markdown`, `prompts_list_and_get_use_workspace_docs`.

### Sin notifications push
- Servidor no emite `tools/list_changed` al añadir extensions en runtime. Cliente ve snapshot del arranque.
- **Acción:** wirear con hot-reload hook (misma follow-up que 12.4 `tools/list_changed`).

### ~~Auth stdio es trusted implícito~~ ✅ Resuelto 2026-04-24
- `mcp_server.yaml` ahora soporta `auth_token_env` (nombre de env var con token esperado).
- `agent mcp-server` lee ese token al arranque y `server::stdio` rechaza `initialize` sin token válido (`params.auth_token` o `params._meta.auth_token`).
- API añadida: `run_stdio_server_with_auth` / `run_with_io_auth`.
- Cobertura:
  - `initialize_rejected_without_auth_token_when_required`
  - `initialize_accepts_auth_token_in_meta`
  - parse test `parses_auth_token_env` en config.

### ~~No expone `ext_*` / `mcp_*` proxies por default~~ ✅ Resuelto 2026-04-24
- `ToolRegistryBridge` ahora filtra por defecto herramientas proxy (`ext_*`, `mcp_*`) cuando `allowlist` está vacío.
- `mcp_server.yaml` añade `expose_proxies: bool` (default `false`) para opt-in global; un `allowlist` explícito sigue pudiendo exponer proxies puntuales aunque `expose_proxies=false`.
- Wiring aplicado en `run_mcp_server` + tests:
  - `list_tools_hides_proxy_tools_by_default_without_allowlist`
  - `explicit_allowlist_can_expose_proxy_tool_even_when_expose_proxies_false`
  - parse test `parses_expose_proxies_flag` en config.

### ~~No `completion/complete`~~ ✅ Resuelto 2026-04-24
- `server::stdio::dispatch` ahora maneja `completion/complete` y devuelve respuesta válida con lista vacía:
  - `{ "completion": { "values": [] } }`
- Evita `method not found` en clientes que llamen autocomplete server-side.
- Cobertura: `completion_complete_returns_empty_values` en `crates/mcp/src/server/stdio.rs`.

### Sin multi-session en stdio
- stdio es 1:1. Si 2 Claude Desktop apuntan al binario, spawneen 2 procesos separados. OK en práctica.

### ~~`AppConfig::load` es strict~~ ✅ Resuelto 2026-04-23
- Nuevo `AppConfig::load_for_mcp_server(dir) -> McpServerBootConfig` que solo lee `agents.yaml` (required) + `mcp_server.yaml` (optional). El subcomando arranca sin depender de `llm.yaml` / `broker.yaml` / `memory.yaml` ni de sus env vars. `load_required` / `load_optional` promovidos a `pub` para reuse futuro.

### ~~Dispatch de `shutdown` no desencadena cleanup~~ ✅ Resuelto 2026-04-24
- `server::stdio::dispatch("shutdown")` ahora marca cierre del loop después de enviar la respuesta JSON-RPC (`result: null`), en vez de quedarse esperando más input.
- Cobertura: `shutdown_request_replies_and_stops_loop` en `crates/mcp/src/server/stdio.rs`.

### ~~Sin tracing para observar requests incoming~~ ✅ Resuelto 2026-04-23
- `crates/mcp/src/server/stdio.rs::dispatch` ahora emite `info` logs en `initialize` (client_name/version) y por cada `tools/call` (tool, duration_ms, is_error); `warn` cuando el call falla (con code + mensaje) o se pide un método desconocido; `debug` para notifications y `tools/list`. También fix secundario: `init_tracing` redirige todo tracing a stderr (antes escribía a stdout y corrompía el wire JSON-RPC del mcp-server).

---

## 🟡 Phase 5.4 — Vector memory (follow-ups)

### Sin local embeddings nativos
- `HttpEmbeddingProvider` requiere un server externo (Ollama, vLLM, LocalAI, llama-server). Sin él, no hay vector.
- **Acción:** añadir `FastembedProvider` (crate `fastembed`) o `CandleProvider` cuando el binary-size tradeoff justifique. Gated por feature flag.

### No hay CLI de reindex
- Cambiar modelo requiere borrar la DB. Un `agent memory reindex --provider X` que recompute todos los vectors sin perder memorias sería útil.
- **Acción:** subcomando que select de `memories`, embed todos, INSERT OR REPLACE en `vec_memories`; validar dim nueva antes.

### Sin multi-vector chunking
- Memorias largas (>500 chars) pierden recall precision al embed completas. Chunking por sentence + agregar múltiples vectores por memoria mejoraría calidad.
- **Acción:** tabla `vec_memories_chunks` con `(memory_id, chunk_idx, embedding)`, cap configurable.

### Sin HNSW ANN index
- sqlite-vec hace escaneo lineal. Para >10k vectores la latency crece. Follow-up con `sqlite-vec`'s `ann_index` cuando estabilice.
- **Acción:** evaluar cuando algún agente cross ese threshold real.

### Sin embedding de concept_tags separado
- Los concept_tags derivados (10.7) no se embeben aparte; sólo content. Queries por concepto se limitan a FTS hoy.
- **Acción:** vector por concept_tag puede boostear recall temático.

### ~~`mode: vector` sin provider errors explícito~~ ✅ Resuelto 2026-04-24
- `LongTermMemory::recall_vector` ahora devuelve mensaje UX-friendly cuando no hay embedding provider:
  - `"the operator hasn't configured semantic memory; use mode 'keyword' instead"`.
- `MemoryTool` propaga ese mensaje directamente al LLM en `action=recall, mode=vector`.

### LRU cache sólo en HTTP provider, no en recall path
- Cache vive en el provider (queries repetidas de embed). El `recall_vector` no cachea resultados.
- **Acción:** opcional segundo LRU por `(agent_id, query, k)` en LongTermMemory si aparece caso repetido.

### No métricas específicas vector vs FTS
- 9.2 Prometheus no tiene counters de `recall_vector_calls_total` ni `hybrid_rrf_overlap`.
- **Acción:** labels en histograms existentes + nuevos counters.

### Sin re-embedding on content update
- Si hay UPDATE en memories (no hay feature hoy, pero podría haber), vector se queda stale.
- **Acción:** trigger SQL o hook app-level cuando llegue update path.

### Empty DB + enabled provider → schema creada pero dim no validada hasta primer insert
- Para DB recién creada, `init_vector_schema` acepta cualquier dim porque no hay filas. Primer INSERT con vector wrong-size es rejected por sqlite-vec. Validación más temprana sería mejor.
- **Acción:** insert a dummy zero-vec al crear, rollback — sondea dim; caro. Probable YAGNI.

### Sin tests contra Ollama real
- Integration tests usan mock provider (determinístico). E2E real contra `ollama serve` + `nomic-embed-text` no existe.
- **Acción:** test gated por env `OLLAMA_URL` similar a `NATS_URL` pattern; smoke CI.

### `mode` default = keyword
- Si operator habilita vector pero no cambia `mode` al tool call, el LLM sigue con FTS.
- **Acción:** opción config `memory.vector.default_recall_mode: "hybrid"` que cambie el default.

---

## 🟡 Phase 10.9 — Git-backed memory workspace (follow-ups)

### ~~Session-close hook ausente (10.9.3)~~ ✅ Resuelto 2026-04-23
- `SessionManager::on_expire` dispara callbacks genéricos; `main.rs` registra uno per-agent que llama `MemoryGitRepo::commit_all("session-close: {sid}", "agent=X")` via `spawn_blocking`.

### Pre-commit validator (10.9.6)
- MVP no filtra PII (emails/teléfonos) ni aborta por patrones prohibidos; sólo skip de archivos >1MiB.
- **Acción:** PII regex opcional + hard cap configurable; política `strict_validation: true` que fail commit si detecta match.

### Remote push (10.9.7)
- No hay push ni sync. Operadores que quieran backup a git self-hosted deben `git push` manual.
- **Acción:** opcional `workspace_git.remote: "origin"` + credentials via env; feature-gated con `git2/https` re-habilitado; rate limit.

### LLM-generated commit messages
- Dream commits usan subject estático + bullet body literal. Útil pero no descriptivo.
- **Acción:** llamar MiniMax con los bullets como input y pedir subject+body coherentes; feature-flag para no gastar LLM en cada sweep.

### Revert tool expuesto al LLM
- MVP no expone `git revert` al LLM; operator debe hacerlo manual desde el host.
- **Acción:** `forge_memory_revert(oid)` tool; cuidado con undo de dreams legítimos.

### GPG signing
- Commits sin firma. Auditoría criptográfica no disponible.
- **Acción:** opcional `workspace_git.gpg_key: "<fingerprint>"`; requiere `git2/gpg` feature + gpg-agent.

### Multi-agent shared repo
- Cada agent tiene su propio repo. Compartir entre agents (team memory) no soportado.
- **Acción:** diseño futuro; requiere branch-per-agent + merge strategy.

### Post-commit notification events
- `commit_all` no emite evento; otros componentes no saben que ocurrió.
- **Acción:** broker pub `workspace.committed.{agent_id}` con `{oid, subject}`; útil para métricas y hooks.

### Size cap no configurable
- `MAX_COMMIT_FILE_BYTES = 1 MiB` hardcoded. Some workspaces pueden necesitar más.
- **Acción:** expose en `WorkspaceGitConfig.max_file_bytes`.

### Branch-per-experiment
- Sin UX para branch/merge. Operator puede hacerlo con git CLI manual.
- **Acción:** `forge_memory_branch("name")` + `forge_memory_checkout("name")` si hay caso real.

### `git2` con `default-features=false`
- Perdemos `https`/`ssh` features. OK para local; para remote (10.9.7) habrá que re-habilitar con binary-size cost.
- **Acción:** feature flag `remote-push` que activa los features y el subcomando.

### Bootstrap commit incluye `.gitignore` creado
- Init escribe `.gitignore` y `.gitattributes`, luego bootstrap commit los captura. Si el workspace ya tenía esos files, init no sobreescribe pero tampoco valida contenido.
- **Acción:** merge políticas custom del operator con los defaults; por ahora respetamos lo existente.

### Concurrencia cross-process
- Dos procesos agent apuntando al mismo `workspace` simultáneo podrían corromper el index. libgit2 no tiene file lock nativo.
- **Acción:** documentar como no-soportado; un operator con necesidad real usa instancias separadas.

---

## 🟡 Phase 13.2 — weather extension v0.2 (Open-Meteo)

### `WEATHER_HTTP_TIMEOUT_SECS` se lee una sola vez (OnceLock)
- `client.rs` cachea el `reqwest::Client` con `OnceLock`; timeout se fija en la primera llamada.
- **Acción:** si se quiere config dinámica, pasar a `Mutex<Client>` con rebuild on-demand.

### Test de timeout no implementado
- Plan listaba 6 tests; se entregaron 5. Mezclar `reqwest::blocking` + wiremock + OnceLock añade fragilidad.
- **Acción:** añadir cuando exista patrón estable de inyección de timeout por petición.

### `client::reset_state` expuesto sin feature flag
- Marcado `#[doc(hidden)]` en lugar de `#[cfg(feature = "test-utils")]` para evitar añadir feature.
- **Acción:** mover a feature `test-utils` si el crate `weather` se vuelve consumido por otros.

### ~~Skills 13.6 sólo con SKILL.md~~ ✅ Resuelto 2026-04-23
- openai-whisper extension v0.2.0 hecho. Ver entrada Phase 13.6 abajo.

---

## 🟡 Phase 13.3 — openstreetmap extension v0.2 (Nominatim)

### Duplicación de breaker.rs / cache.rs entre weather y openstreetmap
- Copy-paste de los módulos de reliability. cache.rs queda sin uso real en OSM (solo para futuro).
- **Acción:** extraer a un crate `extensions-common` (o `agent-ext-shared`) cuando exista la 3ª extension Rust con el mismo patrón. Por ahora aceptable.

### `cache.rs` declarado en lib.rs pero no usado
- OSM no cachea búsquedas porque GeoCache está atado a `GeoEntry`. Se descartó hack de meter JSON serializado en `country`.
- **Acción:** generalizar GeoCache<T> cuando se extraiga a crate común.

### Rate limiter global, no per-host
- Comparte slot entre `search` y `reverse`. Nominatim cuenta ambos al mismo bucket, así que está bien para ese provider.
- **Acción:** parametrizar si en el futuro se añaden providers OSM alternativos (Photon, MapTiler).

### Sin test de timeout (igual que weather)
- Mismo motivo F-13.2.2.

### `OSM_HTTP_TIMEOUT_SECS` se lee una sola vez (OnceLock)
- Mismo F-13.2.1.

---

## 🟡 Phase 13.4 — github extension v0.2 (REST direct)

### Decisión: REST directo en lugar de MCP
- Plan original 13.4 era usar `@modelcontextprotocol/server-github`. Se descartó por consistencia con weather/osm (mismo stack reqwest+CB+retry+wiremock) y para evitar deps node.
- **Acción:** ninguna; documentado en PHASES 13.4.

### `GITHUB_TOKEN` no se valida en startup
- `status` reporta `token_present` pero no falla si no hay token. Cualquier tool no-status devuelve `Unauthorized` recién al hacer el request.
- **Acción:** considerar fail-fast en `initialize` cuando `token_present=false` y no es modo solo-lectura. Hoy es voluntariamente lazy.

### Sin retry automático tras `RateLimited`
- Devolvemos `-32013` con `reset_at` y dejamos que el LLM decida. Algunos clientes preferirían bloqueo automático hasta el reset.
- **Acción:** flag opcional `GITHUB_AUTO_WAIT_RATE_LIMIT=true` que duerma hasta `reset_at` (cap 5 min) y reintente una vez.

### `pr_checks` es 2 requests
- Una para PR (sacar head SHA), otra para check-runs. Se podría cachear el SHA si el LLM consulta la misma PR varias veces seguidas.
- **Acción:** opcional cache (TTL 60s) si aparece patrón de uso repetitivo.

### Sin paginación
- `pr_list`, `issue_list` cap a 100 (per_page max). No seguimos `Link: rel="next"`.
- **Acción:** si surge necesidad real, añadir `page` arg + helper de paginación.

### Sin write tools
- Solo lectura. Crear/comentar PRs/issues fuera de scope v0.2.
- **Acción:** decidir si entran en v0.3 antes de meterlos — añade riesgo (auth scope, idempotencia).

### ~~Duplicación de breaker.rs ya en 3 extensions (weather/osm/github)~~ ✅ Resuelto 2026-04-23
- `extensions/_common/` crate creado con `Breaker`/`BreakerError`. weather/osm/github/summarize migradas. Net -12 unit tests duplicados.

---

## 🟡 Phase 13.5 — summarize extension v0.2 (OpenAI-compat)

### No reusa LlmRegistry
- Extension corre en proceso separado (stdio); LlmRegistry vive in-process. Por eso reimplementa cliente HTTP propio (~250 LOC).
- **Acción:** si surge necesidad real, exponer un endpoint NATS `llm.completions` que las extensions puedan llamar. Hoy duplicación es aceptable.

### Sin chunking automático
- Texto > 60k chars rechazado con -32602 — el LLM (agent) debe chunkear y combinar manualmente. Skill doc lo documenta.
- **Acción:** opcional helper `chunk_and_summarize` con concat hierarchical map-reduce; complejidad media, espera demanda.

### Sin streaming
- Devuelve summary completo de una sola vez. Para inputs largos (long, ~10 frases) puede tardar 5-10s.
- **Acción:** SSE/streaming requiere cambiar protocolo stdio actual (line-delimited JSON-RPC) — fuera de scope.

### Modelos no enumerados
- `model` configurable por env, sin validación/lookup. Si el endpoint rechaza el modelo, error es transparente (HTTP 4xx).
- **Acción:** none.

### ~~Duplicación breaker.rs ya en 4 extensions~~ ✅ Resuelto 2026-04-23
- Resuelto en mismo sweep. Ver entrada arriba.

---

## 🟡 Phase 13.6 — openai-whisper extension v0.2

### Multipart Form rebuilds en cada retry
- `reqwest::blocking::multipart::Form` no es Clone. Se reconstruye dentro del closure de retry desde owned `Vec<u8>` + Strings. Costo: una clonación extra por reintento.
- **Acción:** none; reintentos son raros y la API es la canónica de reqwest.

### Solo `transcribe_file`, no `transcribe_url`
- Plan original mencionaba `transcribe_url` (descarga + transcribe). Se omitió para limitar surface y evitar SSRF.
- **Acción:** si surge demanda real, añadir con allowlist de hosts o usando un fetcher ya existente (no implementar uno nuevo).

### Sin streaming chunks
- Audio largo (10–30 min) bloquea la stdio call hasta 120s. El agent loop espera.
- **Acción:** considerar split client-side por timestamp (`ffmpeg -ss/-t`) si surge necesidad real.

### `WHISPER_HTTP_TIMEOUT_SECS` se lee una sola vez (OnceLock)
- Mismo F-13.2.1.

---

## 🟡 Phase 13.7 — skill metadata frontmatter

### `requires` es informacional, no bloquea
- Si un skill declara `requires.env: [GITHUB_TOKEN]` y la var no está, el skill se carga igual y solo loggea warn. La extension fallará después con `-32011` cuando el LLM intente usar el tool.
- **Acción:** opcional — añadir flag `agents[].skills_strict: true` que omita skills con dependencias no satisfechas. Hoy preferimos warn-only para no romper agentes en arranque por env vars opcionales.

### `requires.bins` solo PATH walk, no version check
- Si `bins: [ffmpeg]` y existe `ffmpeg` cualquier versión, no warn. Useful pero no detecta versiones obsoletas.
- **Acción:** opcional — añadir `requires.bin_versions: { ffmpeg: ">=4.0" }` parsing semver. Bajo demanda.

### Frontmatter parser asume `---\n` exacto al inicio
- Si el archivo empieza con BOM UTF-8, espacios o `# Heading` directo, el frontmatter no se detecta y se trata como markdown plano. Comportamiento tolerante por diseño.
- **Acción:** none.

### `max_chars` cuenta caracteres Unicode (chars()), no bytes
- Para idiomas con muchos multibyte (chino, emoji), el truncado puede dar tamaños de prompt inesperados. Para hoy aceptable.
- **Acción:** considerar `max_bytes` adicional si surge demanda.

---

## 🟢 ext-common crate (refactor 2026-04-23)

### Decisión
- Tras 4 copias de `breaker.rs` (weather/osm/github/summarize), extraído a `extensions/_common/` (crate path-dep, no parte del workspace principal).
- Cada extension ahora declara `ext-common = { path = "../_common" }` y hace `use ext_common::{Breaker, BreakerError}`.
- Crate intencionalmente minimal — solo `Breaker`. Cache, RateLimiter, retry helpers se quedan locales (semánticas distintas por provider).

### Followup
- Si surge una 6ª–7ª extension con cache/HTTP idéntico, considerar mover `cache.rs` y un `client_helpers.rs` (retry+CB call_with_retries genérico).
- Trade-off: más abstracción reduce DRY pero acopla extensions a la API del crate común. Hoy preferimos LOC vs. acoplamiento del provider HTTP.

---

## 🟢 LLM Provider Registry (refactor 2026-04-23)

### Decisión
- Eliminado `AnyLlmClient` enum (cerrado, hardcoded MiniMax+OpenAi+Stub) → `LlmRegistry` con factories trait-based.
- `LlmProviderFactory` trait + `MiniMaxFactory` / `OpenAiFactory` viven al lado de su client.
- Consumers usan `Arc<dyn LlmClient>` directo. Tests instancian fakes envueltos en `Arc::new(X) as Arc<dyn LlmClient>`.
- Provider desconocido en config ahora falla loud (antes: fallback silencioso a OpenAi).

### Beneficios
- Añadir Anthropic / Gemini / Groq = nuevo `crates/llm/src/X.rs` + 1 línea en `with_builtins()`.
- Tests independientes del enum: cualquier `impl LlmClient + 'static` sirve.

### Followup
- Feature `stub` en `crates/llm/Cargo.toml` quedó como no-op (no se removió por compatibilidad de manifests externos).
- **Acción:** quitar la feature en próxima major bump del crate.

### Pendiente (NO hecho ahora)
- Hot-reload de provider per agent en runtime (cambio de `model.provider` sin restart).
- Provider via extension stdio (descartado: latencia inaceptable para LLM hot path).

---

## 🟡 Phase 14 — TaskFlow runtime (durable workflows)

### WaitEngine no está wireado al heartbeat aún
- `WaitEngine::run` existe y el test verifica shutdown, pero ningún callsite en `src/main.rs` la arranca. La Phase 7 heartbeat podría invocarla.
- **Acción:** cuando exista demanda real (primer flow con `Timer` wait), añadir `wait_engine.spawn_with_heartbeat(interval)` en el bootstrap de main.rs. Hoy no lanzo un task sin consumidor.

### Bridge NATS → `try_resume_external` pendiente
- `ExternalEvent { topic, correlation_id }` ya resumible via API, pero no hay suscriptor NATS que lo dispare automáticamente.
- **Acción:** nuevo módulo `bridge_nats.rs` que subscribe a `taskflow.events.>` y resuelve `correlation_id` → `flow_id`. Requiere dep `agent-broker` en taskflow (opcional feature para preservar el crate broker-agnostic).

### TaskFlowTool no expone `set_waiting`/`finish`/`fail`
- El LLM puede `advance` pero no explicitly marcar `Waiting` ni finalizar. Esos los maneja hoy el host programáticamente.
- **Acción:** añadir actions `wait` (inputs: wait_kind+params), `finish` (inputs: final_state?), `fail` (reason). Decidir primero la UX — pasarle condiciones de wait al LLM puede llevar a bucles de espera mal calibrados.

### `FlowManager` no se instancia en el daemon
- Sólo lo usa el CLI y los tests. Para que los agents lo vean, hay que crearlo una vez en main.rs e inyectar un `Arc<FlowManager>` al `ToolRegistry` junto con `TaskFlowTool`. No implementado para no tocar el bootstrap sin diseño de config.
- **Acción:** añadir `taskflow.yaml` (enabled, db_path) + wiring en main.rs cuando haya una integración real que lo necesite.

### SQLite pool por-CLI-call
- Cada comando CLI abre un pool fresco. Overhead ~100ms por invocación. Aceptable para CLI, no para daemon.
- **Acción:** cuando se integre al daemon, pool se comparte con el runtime.

### `config/taskflow.yaml` no existe
- `AppConfig::load` no mira taskflow; el CLI usa env var. Para el daemon conviene config YAML.
- **Acción:** añadir `TaskflowConfig { enabled: bool, db_path: String }` a `agent-config` cuando se wireé al daemon.

### CLI no muestra eventos
- `agent flow show` no imprime la audit log (`list_events`). Útil para debug operacional.
- **Acción:** añadir flag `--events` con las últimas N entradas.

### Retry budget no es configurable
- `RETRY_ATTEMPTS = 2` hardcoded en `manager.rs`. Para ambientes de alta contención podría subirse.
- **Acción:** parametrizar vía `FlowManager::with_retry_attempts(u32)` si surge caso.

### Phase 13.8 "taskflow" queda como ➡️ promoted
- PHASES.md marca 13.8 como movida a Phase 14. No borrarla para preservar el contexto histórico del plan OPENCLAW-SKILLS-PLAN.md.

---

## 🟡 Phase 13.13 — onepassword extension

### Reveal-on no redactado en transcripts
- Cuando `OP_ALLOW_REVEAL=true`, el valor devuelto entra al tool_result → transcript JSONL (Phase 10.4) → posiblemente memory. TranscriptWriter no sabe que ese payload es sensible.
- **Acción:** añadir flag opcional por-entry `sensitive: true` en `TranscriptEntry` + redaction pass al serializar. Requiere modificar agent-core transcripts.rs. Bajo demanda cuando alguien prenda reveal en serio.

### Sin `op inject` para rellenar templates
- Patrón más seguro: LLM pide "ejecuta `curl $API_URL -H "Authorization: Bearer {{stripe_key}}"`" con `op inject -i tpl.env`; el secret nunca toca el LLM.
- **Acción:** nueva tool `inject_template(template: str, refs: [op://...])` que resuelve y ejecuta una command line concreta (limitada a una allowlist operator-definida). Esto sería el paso *correcto* para evitar reveal.

### No soporta múltiples cuentas 1P
- Una sola service-account token por proceso. Si un operador maneja dos orgs separadas, necesitaría dos instancias del extension.
- **Acción:** `--account` flag / `OP_ACCOUNT` env pass-through si surge caso.

### Fingerprint corto (8 bytes = 64 bits)
- Colisiones teóricamente posibles pero astronómicamente improbables con <10^9 secretos distintos. Suficiente para "verify identity".
- **Acción:** none.

### Sin auditoría local de reads
- El extension no logguea qué referencia leyó cuándo. `op` lo hace del lado de 1Password pero sin contexto de sesión del agente.
- **Acción:** append `flow_event`/`audit_log` JSONL por cada `read_secret` con ref + timestamp + agent_id + session_id. Útil si después hay compliance.

### Fake op script en shell bash para tests
- Portable en Linux/macOS, no Windows. Nuestro target es Linux server → aceptable.
- **Acción:** none.

### `op` no se detecta cacheado
- Cada call resuelve `bin_path()` leyendo PATH — barato pero innecesario. Podría cachear con `OnceLock<PathBuf>`.
- **Acción:** micro-optimización; solo si aparece como hotspot (muy improbable).

### Secretos ≠ tokens cortos: Reading grande
- `op read` puede devolver archivos (certificados, SSH keys) de varios KB. Nuestro `length` + fingerprint funcionan pero el valor (si reveal on) entra al LLM y puede contar muchos tokens.
- **Acción:** `max_bytes` opcional en `read_secret` + flag `truncated`. Bajo demanda.

### Tool no expuesto al agent aún
- No hay wiring en `src/main.rs` que registre esta tool en el ToolRegistry — es solo la extension standalone. Cuando se integre: operator decide por-agente si activar y si `OP_ALLOW_REVEAL=true`.
- **Acción:** discovery ya la va a recoger automáticamente via Phase 11 extension loader cuando el binario exista en el search path. El flag reveal queda per-env.

---

## 🟡 Phase 13.12 — session-logs tool (agent-core)

### Primera skill "backed in-process"
- No es una extension stdio — es una tool directa en `agent-core`. Lee JSONL del filesystem del host.
- Trade-off: no aislada del proceso del agent. Ventajas: zero overhead subprocess, zero dep extra, acceso directo a `ctx.config`.
- **Acción:** none; documentado como patrón válido cuando la skill solo toca recursos del propio agente.

### Sin paginación real
- `read_session.limit` devuelve los primeros N entries; no hay `offset`. Sesiones largas con 500+ turnos pierden el resto.
- **Acción:** añadir `offset` o `after_timestamp` cuando aparezca caso real.

### `search` hace full-scan O(N×M)
- Abre cada archivo JSONL, parsea cada línea, substring match. Cientos de sesiones ~100ms; miles requerirán índice.
- **Acción:** SQLite FTS indexando contenido user/assistant con trigger append-on-write en `TranscriptWriter::append_entry`. Bajo demanda.

### Sin wiring automático al daemon
- Igual que TaskFlowTool en 14.5, `SessionLogsTool` hay que registrarla en el `ToolRegistry` desde `src/main.rs` para que el LLM la vea. Hoy no está wireada.
- **Acción:** bootstrap en main.rs cuando se integre (junto con TaskFlowTool).

### No distingue entries por role en summary
- `list_sessions` devuelve `entry_count` total. Útil pero no separa user/assistant/tool counts.
- **Acción:** agregar counters por role si surge necesidad operacional.

### Privacy: transcripts visibles al LLM
- Cualquier entry (incluyendo PII) puede ser recuperada por `search`.
- **Acción:** flag `pii_redact: bool` opcional con regex PII (emails, phones). Bajo demanda.

---

## 🟡 Phase 13.11 — video-frames extension

### Dep dura de `ffmpeg`/`ffprobe` en PATH
- No hay fallback puro-Rust (codecs pesados). Skill frontmatter declara `requires.bins` → Phase 13.7 loader emite warn si faltan.
- **Acción:** none. Docker image base tendrá que incluir ffmpeg (aceptable — ~80 MB extra).

### SIGKILL on timeout, no graceful shutdown
- Watchdog manda SIGKILL inmediatamente al cumplirse el timeout. No intentamos SIGTERM+wait primero.
- **Acción:** opcional dos-fase (SIGTERM → 2s → SIGKILL) si aparece caso de corrupción de output file por kill abrupto.

### `libc::kill` extern "C" en vez de `libc` crate
- Un syscall sólo; evitar dep completa de `libc` que añade ~0 features pero peso simbólico.
- **Acción:** si aparecen más syscalls (signalfd, etc.) cambiar a `libc` crate.

### Sandbox sólo con env var
- `VIDEO_FRAMES_OUTPUT_ROOT` es global a proceso. Dos sesiones concurrentes comparten sandbox.
- **Acción:** si surge multi-tenant, parametrizar vía config YAML por agente + path template `{sandbox}/{agent_id}/{session_id}`.

### Sin soporte HTTP input
- `path` debe ser local; no acepta `http://` URL directamente (ffmpeg podría, pero abriría SSRF).
- **Acción:** si se necesita, chain explícito: `fetch-url.fetch_url` → disk (operator) → `video-frames.*`. Nunca acepte URLs directas aquí.

### Ventana frames cap a 1000
- Hard cap. Videos largos necesitan muchos frames → LLM nunca debe consumir todos.
- **Acción:** none; es un guard intencional.

### Watchdog no cancela en drop
- Si el test/caller es cancelado mientras ffmpeg corre, el proceso hijo sigue hasta timeout. No es leak grave pero es ruido.
- **Acción:** usar `process-wrap` crate o implementar Drop con kill-on-drop. Bajo demanda.

### serial_test en integration por env var compartida
- Los 10 tests integración corren serializados porque comparten `VIDEO_FRAMES_OUTPUT_ROOT`. Añade ~3s total.
- **Acción:** refactor a API que acepte sandbox root por-call en lugar de env-only. Post-13.11.

### Duración bible_videos no validada contra cap
- User dijo que tiene `bible_videos/` — verificar que cada clip cabe en 500 MB. No hemos corrido probe sobre ellos.
- **Acción:** probar con clip real antes de wirearla al agent kate.

---

## 🟡 Phase 13.10 — fetch-url extension

### OpenClaw `xurl` era Twitter API, no fetch genérico
- Reinterpretado por valor real: fetch URL genérico para alimentar summarize/pdf-extract.
- **Acción:** si aparece necesidad real de Twitter/X API, extension separada `xapi` con auth OAuth; naming distinto para no confundir.

### DNS-based SSRF no bloqueado
- `public-hostname.example.com` que resuelve a `127.0.0.1` pasa el guard (la verificación es sobre el string literal del host).
- **Acción:** pre-resolve DNS antes de la request y validar IPs resultantes. Costo: +1 round-trip DNS. Solo si surge caso real; hoy preferimos performance + `allow_private` explícito.

### Sin `save_to: path` para streams grandes
- Cuerpo se lee entero en memoria (hasta `max_bytes`). Un endpoint 50 MB ocupa 50 MB RAM transient.
- **Acción:** nueva variante `fetch_url_save(url, path, max_bytes?)` con streaming a disco. Restringir a dir configurable + filename sanitize para evitar path traversal. Bajo demanda.

### Sin rate limit per-host
- CB es global. Un host lento consume el budget para todos.
- **Acción:** per-host breaker con `HashMap<String, Breaker>`. Bajo demanda.

### Error -32021 SizeCap definido pero nunca emitido
- Truncamos sin error y devolvemos `truncated: true`. El enum tiene `SizeCap` por consistencia pero no se usa.
- **Acción:** si cambia política a "fail-loud", activar -32021.

---

## 🟡 Phase 13.9 — pdf-extract extension

### OpenClaw `nano-pdf` es editor, no extractor
- Skill original de OpenClaw invoca CLI Python `nano-pdf` para editar PDFs con LLM. Reinterpretado como **extractor** puro Rust porque es el caso de uso realmente necesario (summarize pipeline).
- **Acción:** si en el futuro surge demanda de **edición** de PDFs, crear extension separada (`pdf-edit` wrapping `qpdf` o similar). No mezclar extract+edit en la misma extension — scope + auth + risk profile son distintos.

### Sin reuso de ext-common
- El crate no usa `ext-common::Breaker`: no hay llamadas de red, solo I/O local y parseo. La operación es síncrona y cpu-bound.
- **Acción:** none. Ext-common es para patrones HTTP; esta no aplica.

### Sin paginación / offset
- `extract_text` devuelve prefijo truncado. No hay forma de pedir "páginas 50-100".
- **Acción:** si surge necesidad, añadir args `page_start`/`page_end` y usar la API page-by-page de `lopdf` (actualmente `pdf-extract` solo expone `extract_text(file)` completo).

### Sin OCR
- PDFs escaneados sin text layer devuelven string vacío o -32006. Documentado en SKILL.md.
- **Acción:** skill separada `pdf-ocr` con `tesseract` si aparece caso real.

### Fixture PDF hardcoded
- `tests/fixtures/hello.pdf` checked in (2.4 KB). Generado con ps2pdf.
- **Acción:** none. Fixture regenerable desde comentario en README si hace falta.

### `pdf-extract` crate depende de lopdf 0.34
- Stack completo de dependencias pesado (~160 crates transitive). Binario release es grande (~8 MB).
- **Acción:** none. La funcionalidad compensa.

---

## 🟡 Extensions add-on batch (cloudflare, dns-tools, wikipedia, rss, translate, yt-dlp, ssh-exec, tesseract-ocr) — 2026-04-24

### ~~Manifest schema gap~~ ✅ Resuelto 2026-04-24
- Todos los `plugin.toml` con `[requires]` parsean correctamente en `ExtensionManifest`.
- Cobertura añadida en `crates/extensions/src/manifest.rs`:
  - `requires_defaults_to_empty_lists`
  - `requires_parses_bins_and_env`
  - `requires_rejects_unknown_fields`

### ~~Tests integration wiremock por extension~~ ✅ Resuelto 2026-04-24
- Suites wiremock añadidas para extensiones HTTP del batch:
  - `extensions/cloudflare/tests/cloudflare_mock.rs`
  - `extensions/wikipedia/tests/wikipedia_mock.rs`
  - `extensions/rss/tests/rss_mock.rs`
  - `extensions/translate/tests/translate_mock.rs`
- Validación local:
  - `extensions/cloudflare`: 2 tests ok
  - `extensions/wikipedia`: 2 tests ok
  - `extensions/rss`: 2 tests ok
  - `extensions/translate`: 2 tests ok
- Nota: `dns-tools`, `ssh-exec`, `yt-dlp`, `tesseract-ocr` son principalmente wrappers de binarios/local tools; su cobertura no requiere wiremock.

### cloudflare / yt-dlp / ssh-exec — write gates
- `CLOUDFLARE_ALLOW_WRITES`, `YTDLP_ALLOW_DOWNLOAD`, `SSH_EXEC_ALLOW_WRITES` como env flags sin UI/setup wizard.
- **Acción:** si aparecen fricciones, integrar al setup wizard + doctor.

### ~~tesseract-ocr status no hint install~~ ✅ Resuelto 2026-04-24
- `extensions/tesseract-ocr/src/tools.rs` ahora devuelve `install_hint` cuando `status.ok=false`.
- `status` también expone `status_error` y mejora el probe de versión (`stdout`/`stderr`) para diagnósticos más claros.

## 🟡 Telegram plugin — media + typing + LLM vision — 2026-04-24

### ~~auto_transcribe voz pendiente~~ ✅ Resuelto 2026-04-24
- `TelegramPluginConfig` ya expone `auto_transcribe: { enabled, command, timeout_ms, language }`.
- En el poller (`telegram/src/plugin.rs`), cuando `auto_transcribe.enabled=true` y llega `voice/audio` sin texto, llama `transcribe_voice(...)` antes de publicar y reemplaza `text` con la transcripción.
- El `media_path` original se conserva en `InboundEvent.media` para skills que requieran el archivo crudo.

### ~~Typing indicator solo en bridge path~~ ✅ Resuelto 2026-04-24
- `crates/plugins/telegram/src/plugin.rs` (`spawn_dispatcher`) ya dispara `send_chat_action(...)` en path proactive antes de enviar texto/media.
- Mapea `kind`→acción (`upload_photo`, `record_voice`, `upload_video`, `upload_document`, fallback `typing`).

### ~~Tests dispatcher nuevos~~ ✅ Resuelto 2026-04-24
- Cobertura wiremock agregada en `crates/plugins/telegram/tests/dispatch_test.rs`.
- Incluye los custom commands: `send_photo`, `send_audio`, `send_voice`, `send_video`, `send_document`, `send_animation`, `send_location`, `edit_message`, `reaction`, `send_with_format`, `reply` (además de `chat_action` y `unknown custom`).

### ~~MediaDescriptor solo primer media~~ ✅ Resuelto 2026-04-24
- `download_media` ahora recolecta y descarga **todos** los adjuntos detectables del mensaje (`photo/voice/audio/video/video_note/animation/document/sticker`).
- `InboundEvent::Message.media` cambió de `Option<MediaDescriptor>` a `Vec<MediaDescriptor>`.
- `to_payload` mantiene `media_kind/media_path` top-level usando el primer elemento para compatibilidad con el runtime actual.

### ~~InboundMedia sin reutilización de cache~~ ✅ Ya hecho 2026-04-24
- `telegram/src/plugin.rs:969-975`: antes de llamar `bot.download_file`, hace `tokio::fs::metadata(&dest).await.ok().map(|m| m.len())`. Si existe → debug log `telegram media cache hit` y skip download. Re-deliveries del mismo `(chat, msg, file_id_prefix)` reutilizan.

### ~~MAX_TEXT_LEN conservador~~ ✅ Resuelto 2026-04-24
- `MAX_TEXT_LEN` subió a `4096` en `telegram/src/bot.rs` (alineado al límite real del wire en UTF-16 code units).
- `send_with_format` ahora usa truncado por UTF-16 (`truncate_utf16`) en lugar de `chars().count()`.
- Cobertura añadida: `truncate_respects_utf16_budget`.

### ~~dead_code warning: `pending()` helper en telegram + whatsapp~~ ✅ Resuelto 2026-04-24
- `whatsapp/src/plugin.rs:81` ya lleva `#[allow(dead_code)]` sobre `pub(crate) fn pending()`.
- Telegram ya no tiene el helper (fue removido en algún refactor anterior).
- No hay warning emitido al build de estos crates.

## 🟡 LLM providers — paridad cross-provider — 2026-04-24

### Anthropic sin embeddings
- `LlmClient::embed()` default error. Anthropic no tiene endpoint oficial — usan Voyage como partner.
- **Acción:** crear `voyage.rs` standalone o añadir fallback en `AnthropicClient`.

### ~~MiniMax sin embeddings~~ ✅ Resuelto 2026-04-24
- `crates/llm/src/minimax.rs` ya implementa `embed()` para flavor `openai_compat` contra `POST /embeddings`.
- El flavor `anthropic_messages` devuelve error explícito de no soportado (comportamiento esperado).
- Cobertura agregada: `crates/llm/tests/providers_http_audit.rs` (`minimax_embed_openai_compat_returns_sorted_vectors`, `minimax_embed_rejects_anthropic_flavor`).

### ~~build_media_attachment solo imágenes~~ ✅ Resuelto 2026-04-24
- `crates/core/src/agent/llm_behavior.rs` mapea `voice/audio`→`audio/*` y `video/video_note/animation`→`video/*`, materializando base64 para el wire del LLM.
- Gemini consume esos adjuntos inline; providers que no soportan un tipo lo ignoran sin romper el turno.
- Cobertura agregada: `build_media_attachment_voice_materializes_as_audio`, `build_media_attachment_video_uses_kind_and_guessed_mime`.

### ~~ExtensionContextConfig.passthrough por default off~~ ✅ Resuelto 2026-04-24
- Se activó `[context] passthrough = true` en extensions con valor claro de contexto por sesión/agente:
  `cloudflare`, `dns-tools`, `fetch-url`, `github`, `onepassword`, `openai-whisper`,
  `openstreetmap`, `pdf-extract`, `rss`, `ssh-exec`, `summarize`, `tesseract-ocr`,
  `translate`, `video-frames`, `weather`, `wikipedia`, `yt-dlp`.
- Validación: `cargo test -p agent-extensions context_defaults_to_false` y
  `cargo test -p agent-extensions context_passthrough_parses_true` verdes.

---

## Reglas para mantener este archivo

- **Después de cada `/forge ejecutar`:** añadir aquí lo que quedó fuera del plan
- **Antes de `/forge brainstorm <nueva-fase>`:** revisar esta lista para no duplicar trabajo
- **Al empezar una fase nueva:** mover items aplicables a la sección correspondiente
- **No borrar items resueltos:** mover a sección `## ✅ Resueltos` con fecha


---

## ✅ Resueltos

### 2026-04-24 — Tool optimization bug review (22 items)

Deep review del sistema de `tool_policy` + `tool_filter`. 4 críticos, 5 medios, 4 nice-to-have abordados. Estado final: **8/8 tests tool_policy + 6/6 tool_filter verdes**, build limpio.

Críticos (#1, #3, #6, #7):
- Cache particionada por `agent_id` (antes colisionaba entre agentes).
- `stringify_tool_result` fix para `Value::String` (antes `v.to_string()` rompía strings con comillas).
- `FuturesUnordered` + ventana + timeout reemplazan `join_all` sin cap (antes 20-tool batch tumbaba downstream).
- Filtro relevance con fallback a catálogo completo cuando prompt tokeniza vacío.

Medios (#2, #9, #10, #11, #12):
- Filter construido **una sola vez** al boot (`with_tool_policy`), no por turno. Expuesto `rebuild_tool_filter()` para hot-reload.
- LRU real: escanea `stored_at` mínimo en vez de "primer iterator entry" (evita churn).
- Telemetría `agent_tool_cache_events_total{event=hit|miss|put|evict|skip_size|invalidate}` Prometheus.
- Background sweep cada 60s en `main.rs` (tokio::spawn sobre `registry.sweep_expired()`).
- Query de filter ahora concatena prompt actual + últimas 3 interacciones del history para contexto.

Tier 3 (#17, #19, #20 + admin HTTP):
- `CacheConfig::max_value_bytes` (default 256KB) — skip cache si payload excede cap.
- `cache_invalidate(agent, tool)` + `cache_clear()` con telemetry.
- `ToolPolicyRegistry` con `per_agent: HashMap<String, AgentPolicyOverride>` en YAML. Cada agente recibe su `Arc<ToolPolicy>` propio.
- Admin HTTP loopback `127.0.0.1:9091`: `GET /admin/tool-cache/stats`, `POST /admin/tool-cache/clear`, `POST /admin/tool-cache/invalidate?agent=X&tool=Y`.

#16 strict allow-list: **no requiere código nuevo** — set `top_k: 0` + `always_include: [...]` da ese comportamiento. Test `strict_allow_list_via_top_k_zero` lo documenta.

No abordado (low ROI):
- #18 cache persistence a disco (TTL 60s lo hace marginal).
- Auth header en admin HTTP (loopback-only mitigación suficiente por ahora).

---

## Phase 15 — claude-subscription-auth (deferred items)

- **Multi-profile round-robin**: OpenClaw keeps several Anthropic
  profiles (`anthropic:manual`, `anthropic:default-oauth`, …) with
  cooldowns and `order`. Not ported — one provider = one auth in V1.
  Revisit only if a real failover case appears.
- **Device-code OAuth flow**: we rely on `claude login` or a pasted
  bundle. A native device-code flow would mean pinning Anthropic's
  client_id + endpoints ourselves. Deferred; YAGNI for now.
- **macOS Keychain write**: we only read. User rotates by re-running
  `claude login` and `agent setup anthropic`.
- **Live smoke test**: `anthropic_live.rs` gated by
  `CHAT_LIVE_ANTHROPIC_OAUTH=1` was sketched in the plan but not
  landed — CI has no Anthropic creds. Add later if we get a service
  account for regression.
- **Refresh endpoint / client_id hardcoded default**: the Claude Code
  CLI's public client_id and `console.anthropic.com/v1/oauth/token`
  are baked in as defaults. Both are overridable via YAML, but if
  Anthropic rotates them upstream we'll need to ship a new default
  release.

## Per-binding capability override

Phase 16 complete. Review + polish landed across several commits:
aggregate validation errors, wildcard/specific overlap warning,
post-assembly tool-name check, known-provider validation against
the LLM registry, config-dir-relative path resolution, and
Option<usize> binding_index (no more `usize::MAX` sentinel). The
only items left are structural choices, not bugs.

- **`agents_directory` default-spread fragility**: `..Default::default()`
  in test-only struct literals means any future InboundBinding field
  silently defaults. Acceptable for tests; revisit if the schema
  gains a field whose default has semantic weight.
- **Hot-reload of per-binding config**: the effective policy cache,
  tool registry cache, and rate-limiter slots are built at
  `runtime::new`. Config changes need a process restart. A hot-
  reload path would have to invalidate all three plus the LLM
  registry catalogue used for boot-time validation.
- **session_id cross-binding collision (theoretical)**: a session is
  tied to its first binding for the life of `session_id`. Platform
  ids don't collide today, so this is a paper cut — documented for
  future auditors.
