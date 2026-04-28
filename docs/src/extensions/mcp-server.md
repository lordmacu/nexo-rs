# MCP server (HTTP + SSE)

The agent can expose its own tools as an [MCP server](https://modelcontextprotocol.io)
so other clients (Claude Desktop, Cursor, Zed, custom IDE plugins,
remote consumers, third-party plugins like the upcoming
`nexo-marketing` extension) can call them. The transport ships in
two flavours, both backed by the same `Dispatcher` and so both
share identical wire-level behaviour:

| Transport | Status | Path | Use case |
|-----------|--------|------|----------|
| **stdio** | shipped (Phase 12.6) | `agent mcp-server` over the process stdio | Local IDE plugins that spawn the agent as a subprocess |
| **HTTP+SSE (Streamable)** | shipped (Phase 76.1) | `POST /mcp`, `GET /mcp`, `DELETE /mcp` | Remote clients, multi-process consumers, browser-based tools |
| **Legacy SSE alias** | optional (Phase 76.1) | `GET /sse`, `POST /messages?sessionId=…` | Older Claude Desktop builds still on the 2024-11-05 spec |

> Phase 76.1 only ships the transport layer. Pluggable auth
> (Phase 76.3), multi-tenant isolation (76.4), per-tool rate-limit
> (76.5), durable sessions + SSE replay (76.8 — see "Session
> resumption" below), and TLS-in-process (76.13) are tracked
> separately. For production today, terminate TLS at
> nginx/caddy/Traefik in front of the loopback bind.

## Enabling HTTP

Edit `config/mcp_server.yaml`:

```yaml
mcp_server:
  enabled: true
  http:
    enabled: true
    bind: "127.0.0.1:7575"
    auth_token_env: "NEXO_MCP_HTTP_TOKEN"
    allow_origins:
      - "http://localhost"
      - "http://127.0.0.1"
    body_max_bytes: 1048576
    request_timeout_secs: 30
    session_idle_timeout_secs: 300
    max_sessions: 1000
    enable_legacy_sse: false
```

Start the daemon as usual; `agent mcp-server` boots both stdio
and the HTTP listener when `http.enabled: true`.

## Authentication (Phase 76.3)

The HTTP transport supports four pluggable authentication modes.
All modes share an anti-enumeration response shape: every rejection
returns the **same** 401 body
(`{"jsonrpc":"2.0","error":{"code":-32001,"message":"unauthorized"}}`)
so a probing client cannot distinguish *missing token*, *wrong
token*, *expired token*, *unknown kid*, etc. The reason is logged
via `tracing::warn!` only.

Configure via `mcp_server.http.auth`. The block is mutually
exclusive with the legacy `auth_token_env`; set one or the other.

### `kind: none`

Disables authentication. The runtime **refuses to boot** if
`bind` is not a loopback address (`127.0.0.0/8` or `::1`). For
local dev only.

### `kind: static_token`

Constant-time-compared bearer token.

```yaml
mcp_server:
  http:
    enabled: true
    auth:
      kind: static_token
      token_env: "NEXO_MCP_TOKEN"
```

The env var must resolve to a non-empty string at boot. Clients
present the token via either `Authorization: Bearer <token>` or
`Mcp-Auth-Token: <token>`. Comparison runs through `subtle::ct_eq`
to defeat timing side-channels; length-mismatch returns false
immediately (the length channel is not protected — pick a
fixed-length token).

### `kind: bearer_jwt`

JWT validated against a remote JWKS endpoint with cache + stale-OK
fallback.

```yaml
mcp_server:
  http:
    enabled: true
    auth:
      kind: bearer_jwt
      jwks_url: "https://idp.example.com/.well-known/jwks.json"
      jwks_ttl_secs: 300
      jwks_refresh_cooldown_secs: 10
      algorithms: ["RS256"]
      issuer: "https://idp.example.com/"
      audiences: ["nexo-mcp"]
      tenant_claim: "tenant_id"
      scopes_claim: "scope"
      leeway_secs: 30
```

Boot-time validation rejects:
* Empty `algorithms` list.
* `algorithms` containing `none`.
* Mixing HMAC (`HS*`) and asymmetric (`RS*`/`ES*`/`PS*`) algorithms
  in the same list — the algorithm-confusion CVE class.

JWKS robustness:
* The cache uses single-flight refresh (one in-flight HTTP fetch
  per `kid`, others wait on `tokio::sync::Notify`).
* Refresh attempts are rate-limited by `jwks_refresh_cooldown_secs`.
* If a refresh fails *and* a previously-cached key for the same
  `kid` exists, the stale key is reused and a `warn!` line is
  emitted (the IdP is allowed transient outages).
* If no usable cached key is available, the request returns
  HTTP **503** (`-32099 authentication backend unavailable`)
  rather than 401, since the failure is on our side.

The `Principal` produced by a successful JWT validation carries
`tenant_id`, `subject`, and `scopes` — those flow into
`DispatchContext.principal` and are available to handlers.

### `kind: mutual_tls` (mode: `from_header`)

mTLS terminated by a reverse proxy (nginx, Caddy, Traefik). The
proxy validates the client cert and forwards the CN/SAN via a
trusted header.

```yaml
mcp_server:
  http:
    enabled: true
    bind: "127.0.0.1:7575"   # MUST be loopback in this mode
    auth:
      kind: mutual_tls
      mode: from_header
      header_name: "X-Client-Cert-Cn"
      cn_allowlist:
        - "agent-1.internal"
        - "agent-2.internal"
```

The runtime **refuses to boot** when `bind` is not loopback in
this mode — without that constraint any internet client could
forge the header. `cn_allowlist` is exact-match (no glob, no
substring).

### Backward compatibility

The legacy `mcp_server.http.auth_token_env` field still works.
When set with no `auth` block, the runtime promotes it to
`AuthConfig::StaticToken` and emits a `tracing::warn!` with a
deprecation hint. Setting both `auth` and `auth_token_env`
simultaneously fails fast at boot.

## Tenant isolation (Phase 76.4)

Every authenticated request carries a validated `TenantId` on its
[`Principal`]. The tenant flows from the auth boundary into
`DispatchContext::tenant()`, and from there into helpers that
namespace filesystem paths and SQLite databases.

### Origin of the tenant id

The tenant id is **always** server-derived from the `Principal`. A
tool **must never** read `tenant_id` from its own arguments — that
would let a caller forge a tenant tag. Pattern ported from
`claude-code-leak/src/services/teamMemorySync/index.ts:163-166`:
the client passes only `repo`, the `organizationId` is validated
on the server side from the Bearer token. Nexo follows the same
discipline.

### How each auth mode derives the tenant

| Mode | Source | Default | Failure |
|------|--------|---------|---------|
| `none` | hardcoded `"local"` | — | — |
| `static_token` | YAML `tenant:` field | `"default"` | invalid id → boot fail |
| `bearer_jwt` | JWT claim named by `tenant_claim` | reject if missing | invalid format → 401 (`TenantClaimMissing`) |
| `mutual_tls` (`from_header`) | `cn_to_tenant` map → CN itself | — | dotted CN without remap → 401 |

```yaml
mcp_server:
  http:
    enabled: true
    auth:
      kind: static_token
      token_env: NEXO_MCP_TOKEN
      tenant: prod-corp     # 76.4 — pin the tenant for this token
```

```yaml
mcp_server:
  http:
    enabled: true
    auth:
      kind: mutual_tls
      mode: from_header
      cn_allowlist: [agent-1.internal, agent-2.internal]
      cn_to_tenant:                       # 76.4 — required for dotted CNs
        agent-1.internal: tenant-a
        agent-2.internal: tenant-b
```

> Dotted CNs (e.g. `agent-1.internal`) cannot be parsed as tenant
> ids on their own — the strict `TenantId` validator rejects `.`.
> Provide `cn_to_tenant` to remap, or rename the CN. We deliberately
> do **not** silently rewrite CNs (no automatic `.` → `-`); silent
> rewrites of identity claims are a security smell.

### `TenantId` validation

`TenantId::parse(raw)` enforces:

1. No NUL bytes (C-syscall truncation vector).
2. Input must already be in NFKC canonical form — fullwidth-form
   bypasses (e.g. `Ｔｅｎａｎｔ`, `．．／`) are rejected.
3. Percent-decode-and-recheck: `%2e%2e%2f` smuggling is rejected.
4. Length: 1–64 bytes.
5. Charset: `[a-z0-9_-]` only (lowercase ASCII; no dot, slash,
   uppercase, or whitespace).
6. No leading or trailing `_` or `-`.

These rules are direct ports of
`claude-code-leak/src/memdir/teamMemPaths.ts:22-64`
(`sanitizePathKey`).

### Path scoping

```rust
use nexo_mcp::server::auth::{tenant_scoped_path, tenant_db_path};

// New writes — non-canonicalising, fast.
let p = tenant_scoped_path(&root, ctx.tenant(), "memory/notes.txt");

// Reads — symlink-aware, ports
// claude-code-leak/src/memdir/teamMemPaths.ts:228-256
// (validateTeamMemWritePath).
let p = tenant_scoped_canonicalize(&root, ctx.tenant(), "memory/notes.txt")?;
```

`tenant_scoped_canonicalize` performs a two-pass containment check:

1. Lexical resolution rejects `..` and absolute suffixes.
2. `realpath()` on the deepest existing ancestor follows symlinks
   and asserts the resolved path is strictly under
   `<root>/tenants/<tenant>/`. Symlink loops (`ELOOP`), dangling
   symlinks, and sibling-tenant traversal (`tenants/t-evil/...`
   trying to pass as `tenants/t/...`) all surface as distinct
   `TenantPathError` variants.

Symlink defense is gated on `cfg(unix)` — Windows
`std::fs::canonicalize` returns UNC paths that break the prefix
check. Phase 76.4 production targets are Linux musl + Termux; full
Windows port is a follow-up.

### `TenantScoped<T>` trip-wire

```rust
use nexo_mcp::server::auth::TenantScoped;

let db = TenantScoped::new(tenant_a.clone(), open_db_for("tenant-a"));
let raw = db.try_into_inner(&tenant_b)?; // → CrossTenantError
```

Thin wrapper that pairs a value with the tenant it was constructed
for. `try_into_inner` is the trip-wire: extracting under a wrong
tenant returns `CrossTenantError` rather than silently leaking. Not
a load-bearing security boundary on its own — the actual isolation
comes from path scoping at construction time — but cheap defense
in depth against future bugs.

### SQLite layout

`tenant_db_path(root, tenant)` returns
`<root>/tenants/<tenant>/state.sqlite3`. One DB per tenant is the
strongest isolation `rusqlite` makes easy: a corrupted DB blasts
exactly one tenant. The production reference at
`claude-code-leak/src/services/teamMemorySync/index.ts` is
file-based + server-side scope enforcement; one-DB-per-tenant in
nexo is a step beyond that, suited to the in-process MCP server
shape.

## Per-principal rate-limit (Phase 76.5)

A second rate-limit layer sits **inside** the dispatcher,
keyed on `(tenant_id, tool_name)`. It complements the per-IP
layer (Phase 76.1, HTTP middleware): the per-IP layer rejects
broad floods at the HTTP level (`429 + Retry-After`); the
per-principal layer protects individual tools from a single
authenticated tenant exhausting them (`200 + JSON-RPC -32099 +
data.retry_after_ms`).

### Wire shape

The per-IP and per-principal layers return **different** wire
shapes — intentional, since they fire at different stack levels:

| Layer | Status | Body |
|-------|--------|------|
| Per-IP (76.1, before parsing) | `429 Too Many Requests` + `Retry-After: <secs>` header | minimal |
| Per-principal (76.5, inside dispatcher) | `200 OK` + JSON-RPC error | `{"jsonrpc":"2.0","error":{"code":-32099,"message":"rate limit exceeded","data":{"retry_after_ms":<n>}},"id":<request_id>}` |

A client that handles both sees one shape (HTTP 429) for "you're
hitting the public IP gate too hard" and another (JSON-RPC -32099)
for "this tenant has used its tool quota". `retry_after_ms` is
the time until one token refills.

The `Retry-After` header parsing pattern (seconds → milliseconds)
is ported from
`claude-code-leak/src/services/api/withRetry.ts:803-812
getRetryAfterMs`.

### Configuration

```yaml
mcp_server:
  http:
    enabled: true
    per_principal_rate_limit:
      enabled: true                         # default
      default: { rps: 100.0, burst: 200.0 } # applies to any tool not in per_tool
      per_tool:
        agent_turn:    { rps: 10.0, burst: 20.0 }   # heavier tool, lower limit
        memory_search: { rps: 50.0, burst: 100.0 }
      max_buckets: 50000                     # hard cap on the bucket map
      stale_ttl_secs: 300                    # prune buckets idle > 5 min
      warn_threshold: 0.8                    # log when utilization ≥ 80%
```

When the `per_principal_rate_limit` block is **omitted entirely**,
the limiter is **not built** (zero overhead in the dispatcher
hot path). When the block is **present** but `enabled: false`,
the limiter is built but `check()` short-circuits.

### What gets rate-limited

| JSON-RPC method | Gated by 76.5? |
|-----------------|----------------|
| `tools/call`    | **yes**        |
| `tools/list`    | no — list calls are cheap, no abuse vector beyond per-IP |
| `initialize`    | no — once per session, gated by auth + per-IP |
| `shutdown`      | no |
| `resources/*`   | no (Phase 76.7 may add a separate gate)  |

Stdio principals (`auth_method: stdio`) **bypass** the limiter
entirely — stdio is single-tenant by construction, so a
self-throttling agent makes no sense.

### Bucket eviction

The bucket map is bounded by `max_buckets` (default 50 000) with
two eviction strategies running in parallel:

* **Hard cap**: when `len() ≥ max_buckets` and a fresh key is
  about to be inserted, the limiter evicts ~1% of the cap from
  the buckets with the smallest `last_seen` timestamp (LRU).
* **Background sweeper**: a `tokio::spawn` task wakes every 60 s
  and prunes any bucket with `last_seen` older than
  `stale_ttl_secs`. The task holds a `Weak<Self>` so it dies
  when the limiter is dropped.

This pattern is ported from OpenClaw
`research/src/gateway/control-plane-rate-limit.ts:6-7,101-110`
(10 k cap + 5-min stale-TTL pruner). The leak (Anthropic Claude
Code CLI) is **client-side only** and does not implement
server-side rate-limiting itself; we port the wire shape from
the leak and the eviction policy from OpenClaw.

### Early-warning log

When a bucket's utilization crosses `warn_threshold` (default
0.8), the limiter emits a `tracing::warn!` with `tenant`, `tool`,
and the current utilization. Useful as an "approaching saturation"
signal so operators can pre-emptively raise a per-tool override
before clients start hitting `-32099`. Pattern from
`claude-code-leak/src/services/claudeAiLimits.ts:53-70
EARLY_WARNING_CONFIGS`, simplified to a single fixed threshold.

## Per-principal concurrency cap + per-call timeout (Phase 76.6)

The third gate in the dispatch path. Sits **after** the rate-limit
layer (76.5) and protects against a different failure mode: not
"too many requests per second" but "too many requests in flight at
once" — typical when handlers are slow and a client keeps firing.

| Layer | Measures | Wire when exceeded |
|-------|----------|-----|
| 76.1 per-IP (HTTP middleware) | requests / second per source IP | HTTP 429 |
| 76.5 per-principal rate-limit | requests / second per (tenant, tool) | JSON-RPC `-32099` |
| **76.6 per-principal concurrency cap** | **in-flight requests per (tenant, tool)** | **JSON-RPC `-32002`** |
| 76.6 per-call timeout | wall-clock duration of a single call | JSON-RPC `-32001` |

A request must clear all four to reach the handler.

### Wire shape

| Outcome | Code | Body `data` |
|---|---|---|
| Concurrency cap exceeded (queue wait expired) | `-32002` | `{"max_in_flight": <n>, "queue_wait_ms_exceeded": <n>}` |
| Per-call timeout exceeded | `-32001` | `{"timeout_ms": <n>}` |

`-32002` is reserved for "operator-side overload" — distinct from
`-32099` which means "you, the client, asked too much".

### Configuration

```yaml
mcp_server:
  http:
    enabled: true
    per_principal_concurrency:
      enabled: true                       # default
      default: { max_in_flight: 10 }      # per-(tenant, tool) default
      per_tool:
        agent_turn:    { max_in_flight: 5,  timeout_secs: 300 }
        memory_search: { max_in_flight: 20, timeout_secs: 5 }
      default_timeout_secs: 30            # fallback when per-tool omits
      queue_wait_ms: 5000                 # how long to wait for a permit
      max_buckets: 50000                  # hard cap on the semaphore map
      stale_ttl_secs: 300                 # prune buckets idle > 5 min
```

When the block is **omitted entirely**, the cap is **not built**
(zero overhead). When `enabled: false`, the cap is built but
`acquire` short-circuits to a no-op permit.

### What gets capped

| JSON-RPC method | Capped by 76.6? |
|-----------------|-----------------|
| `tools/call`    | **yes**         |
| `tools/list`    | no              |
| `initialize`    | no              |
| `shutdown`      | no              |
| `resources/*`   | no              |

Stdio principals (`auth_method: stdio`) **bypass** the cap entirely
(single-tenant by construction).

### How permits work

Each `(tenant, tool)` pair gets a `tokio::sync::Semaphore` with
`max_in_flight` permits. The dispatcher acquires one permit before
calling the handler and drops it (RAII) on:

* successful return,
* handler error,
* per-call timeout firing,
* client/session cancellation.

The permit is **always** released — there is no path that strands
one. Verified by `tests/http_concurrency_load_test.rs` and the
test fixture in PHASES.md (handler sleeps 60 s with timeout 5 s →
returns `-32001` within ~5 s, semaphore back to full permits).

### Queue wait

When all permits are taken, a new request waits up to
`queue_wait_ms` for one to free up. If the wait expires, the
request is rejected with `-32002`. `queue_wait_ms: 0` means "reject
immediately if no permit is available" (no queueing).

Cancellation during the wait (HTTP client disconnect, session
shutdown, `tokio::select!` on the caller side) propagates: the
acquire returns `Cancelled` → dispatcher returns `-32800 request
cancelled` rather than waiting out the full queue interval.

### Per-call timeout

Independent of the concurrency cap. Wraps the handler future in
`tokio::time::timeout(timeout_for(tool), ...)`. On elapse the
inner future is dropped at its next `.await` (cooperative
cancellation), the permit is released, and the dispatcher returns
`-32001` with `data.timeout_ms`. Lookup priority for the timeout:

1. `per_tool[<name>].timeout_secs`
2. `default.timeout_secs`
3. `default_timeout_secs`

Hard cap on any timeout is 600 s (mirrors
`http_config::MAX_REQUEST_TIMEOUT_SECS`).

### Bucket eviction

Same shape as 76.5: a hard cap (`max_buckets`, default 50 000)
with LRU eviction at insert + a background sweeper that runs
every 60 s and prunes entries with `last_seen` older than
`stale_ttl_secs`. The sweeper **only** drops entries whose
semaphore has all permits available — it never strands an
in-flight permit. Worst case: a tenant that always has at least
one call in flight never gets its entry pruned, bounded by the
hard cap LRU at insert time.

### Reference patterns

* **RAII permit + cancel-aware acquire** — in-tree
  `crates/mcp/src/client.rs:873-899` (76.1 client side).
* **DashMap + sweeper + hard-cap eviction** — Phase 76.5
  `per_principal_rate_limit.rs`. We mirror the same shape with
  `Semaphore` in place of `TokenBucket`.
* **`tokio::select!` cancellation** — Phase 76.2
  `dispatch.rs:201-205` (`biased; cancel; do_dispatch`).
* **AbortSignal/AbortController equivalent** —
  `claude-code-leak/src/Task.ts:39` and
  `src/services/tools/toolExecution.ts:415-416`. The leak does
  not implement server-side concurrency caps (it's a client),
  so only the cancellation propagation idea is portable.
* **Anti-pattern (NOT ported)**: OpenClaw
  `research/src/acp/control-plane/session-actor-queue.ts:6-37`
  uses an unbounded keyed-async-queue. Phase 76.6 explicitly
  rejects unbounded queues (`max_buckets` + `queue_wait_ms`
  together bound both memory and tail latency).

## Server-side notifications + streaming (Phase 76.7)

Phase 76.7 closes the server→client notification loop on top of
the per-session SSE channel that Phase 76.1 already wired. Three
JSON-RPC notifications are now emitted by the in-tree dispatcher,
plus a fourth (`notifications/progress`) that tools opt into via
a streaming-aware handler method.

| Notification | Trigger | Wire shape |
|---|---|---|
| `notifications/tools/list_changed` | `HttpServerHandle::notify_tools_list_changed()` | `{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}` |
| `notifications/resources/list_changed` | `HttpServerHandle::notify_resources_list_changed()` | `{"jsonrpc":"2.0","method":"notifications/resources/list_changed"}` |
| `notifications/resources/updated` | `HttpServerHandle::notify_resource_updated(uri, contents)` | `{"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri":<…>,"contents":<…>?}}` |
| `notifications/progress` | tool calls `progress.report(progress, total?, message?)` | `{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":<echoed>,"progress":<n>,"total":<n>?,"message":<…>?}}` |

### Capability advertisement

The default `McpServerHandler::capabilities()` now returns:
```json
{
  "tools":     { "listChanged": true },
  "resources": { "listChanged": true, "subscribe": true }
}
```
Implementors that don't support subscriptions can override the
method.

### Progress reporter

A tool that wants to emit progress overrides `call_tool_streaming`
on its `McpServerHandler` (the default delegates to `call_tool`
and ignores the reporter):

```rust
async fn call_tool_streaming(
    &self,
    name: &str,
    args: Value,
    progress: ProgressReporter,
) -> Result<McpToolResult, McpError> {
    for i in 1..=100 {
        progress.report(i as f64, Some(100.0), Some(format!("step {i}")));
        do_one_step().await;
    }
    Ok(/* result */)
}
```

* `progress.report` is non-blocking. Drop-oldest on broadcast
  overflow; sender never panics if the SSE consumer disconnected.
* A 20 ms coalescing gate (per reporter) collapses storms — a
  tool that calls `report` 1 000 times in a tight loop produces
  ≤ 50 events/sec on the wire, with the most recent values
  emitted on each gate fire.
* The reporter is a noop when the originating request did not
  include `params._meta.progressToken`. Tools call `report`
  unconditionally without branching.

### `resources/subscribe` semantics

```jsonrpc
→ {"jsonrpc":"2.0","method":"resources/subscribe","params":{"uri":"file:///x"},"id":1}
← {"jsonrpc":"2.0","result":{},"id":1}
```

Subscriptions are stored in a `DashSet<String>` on the session,
cleared when the session is removed. The host pushes
`notifications/resources/updated` via
`HttpServerHandle::notify_resource_updated(uri, contents)`; only
sessions whose subscription set contains `uri` receive the event.

### Reference patterns

* `claude-code-leak/src/services/mcp/useManageMCPConnections.ts:618-664`
  — client-side consumption of `tools/list_changed`. The leak is
  client-side and does NOT implement server-side notifications;
  we port the wire shape and build the server-side broadcast
  ourselves on top of the existing
  `broadcast::Sender<SessionEvent>` per session
  (Phase 76.1, `crates/mcp/src/server/http_session.rs:39-46`).
* `crates/mcp/src/server/http_transport.rs:815-820` —
  `Lagged` event handling on SSE overflow. Reused as-is for
  `notifications/progress` storm scenarios.

## Session resumption + SSE replay (Phase 76.8)

The HTTP transport persists every server-pushed SSE frame to a
SQLite event store so a reconnecting client can replay the gap via
the `Last-Event-ID` header instead of re-`initialize`-ing from
scratch.

### Wire contract

* SSE frames carry `id: <seq>` (per-session monotonic, starting at
  1) plus `event: message` / `data: <json-rpc-frame>`.
* Reconnect: `GET /mcp` with `Mcp-Session-Id: <uuid>` +
  `Last-Event-ID: <seq>`. The server replays persisted frames
  with `seq > <Last-Event-ID>` (capped at `max_replay_batch`)
  before the live broadcast loop attaches.
* Header absent → no replay (live only). Header present (any
  numeric value, including `0`) → replay everything above.
* Unknown `Mcp-Session-Id` → HTTP **404** + JSON-RPC body
  `{"error":{"code":-32001,"message":"Session not found"}}`. This
  matches the leaked Claude Code client's
  `isMcpSessionExpiredError` contract — a permanent failure that
  the client must recover by re-`initialize`.

### Configuration

```yaml
mcp_server:
  http:
    session_event_store:
      enabled: true                     # opt-in; default off when block omitted
      db_path: "data/mcp_sessions.db"   # absolute path recommended in prod
      max_events_per_session: 10000     # ring cap; oldest pruned every 1000 emits
      max_replay_batch: 1000            # hard ceiling per replay (max 10000)
      purge_interval_secs: 60           # background prune older than session_max_lifetime_secs
```

The `session_max_lifetime_secs` (default 24 h) gates how long
events live in the store. The background purge worker stops on
parent shutdown; SIGTERM does not block on it.

### What does *not* survive a daemon restart

The in-memory `HttpSession` (broadcast channel + cancellation
token) is gone after a restart. Only **events + subscriptions**
persist on disk. A client that reconnects with its old session-id
gets the 404 + -32001 contract above and is expected to
re-`initialize`. Full session reattach (rehydrating
`HttpSession` entire) is parked as **76.8.b** until a real client
asks for it — the leak's own client treats expired sessions as
permanent failure, so the parity gap is intentional.

### Observability

The same `mcp_requests_total{outcome}` and `mcp_request_duration_seconds`
metrics from 76.10 cover replay path requests transparently.
Replay-specific counters (`mcp_replay_rows_total`,
`mcp_replay_skipped_total{reason="cap"}`) are deferred to a
follow-up — file an issue if you need them sooner.

### Reference patterns

* `claude-code-leak/src/cli/transports/SSETransport.ts:159-266`
  — wire format SSE `id:` + `Last-Event-ID` reconnect.
* `claude-code-leak/src/services/mcp/client.ts:189-206` —
  HTTP 404 + JSON-RPC `-32001` permanent-failure contract.
* `crates/agent-registry/src/turn_log.rs:64-89` — in-tree
  `TurnLogStore` pattern mirrored verbatim for the
  `SessionEventStore` trait shape (Phase 72 alignment).

## Observability + health (Phase 76.10)

The server emits Prometheus metrics for every dispatch path
plus enriched `/healthz` + `/readyz` responses. Metrics are
hand-rolled (`LazyLock<DashMap<Key, AtomicU64>>` module globals)
following the in-tree pattern (`crates/web-search/src/telemetry.rs`,
`crates/llm/src/telemetry.rs`) — render-on-scrape, no
`prometheus` crate dependency.

### Metric inventory

| Metric | Type | Labels | Bumped at |
|---|---|---|---|
| `mcp_requests_total` | counter | `tenant`, `tool`, `outcome` | `Dispatcher` post-call (every `tools/call` outcome) |
| `mcp_request_duration_seconds` | histogram (8 buckets: 50/100/250/500/1k/2.5k/5k/10k ms) | `tenant`, `tool` | `Dispatcher` post-call |
| `mcp_in_flight` | gauge (signed) | `tenant`, `tool` | RAII `InFlightGuard` — increment on entry, decrement on every exit path (incl. panic unwind) |
| `mcp_rate_limit_hits_total` | counter | `tenant`, `tool` | 76.5 rate-limit reject |
| `mcp_timeouts_total` | counter | `tenant`, `tool` | 76.6 per-call timeout reject (-32001) |
| `mcp_concurrency_rejections_total` | counter | `tenant`, `tool` | 76.6 concurrency cap reject (-32002) |
| `mcp_progress_notifications_total` | counter | `outcome` (ok\|drop) | 76.7 reporter emit / drop-oldest overflow |

`outcome` enum (bounded set, byte-stable):
`ok | error | cancelled | timeout | rate_limited | denied | panicked`.

### Cardinality discipline

Tool labels are bounded by `MAX_DISTINCT_TOOLS = 256`. Beyond that,
every new tool name collapses to `"other"`. Pattern ported from
`claude-code-leak/src/services/analytics/datadog.ts:195-217`
(`mcp__*` tools collapsed to `'mcp'`). Tenant labels are bounded
by `TenantId::parse` (`[a-z0-9_-]{1,64}`) — even a misconfigured
deployment can't blow up the metric.

### `correlation_id` propagation

The HTTP transport extracts `X-Request-ID` from request headers
(or generates a UUIDv4 when absent), echoes it in the response
header, and stamps it on `DispatchContext.correlation_id`. The
dispatcher logs it on every `mcp.dispatch` span:

```
INFO mcp.dispatch{tenant=acme tool=agent_turn correlation_id=4d8c...} ...
```

Client-supplied values longer than 128 chars are replaced with a
fresh UUIDv4 — don't trust unbounded headers.

### `/healthz` vs `/readyz`

`/healthz` (port from Phase 9.3): liveness only, returns
`200 {"status":"ok"}` as long as the process is alive.

`/readyz`: structured readiness check with cached snapshot
(TTL 5 s — absorbs scrape thundering-herd):
```json
{
  "ready": true,
  "checks": {
    "broker": true,
    "sessions_capacity_ok": true
  }
}
```
Returns HTTP 200 when `ready` is true, 503 otherwise. Operators
should hit `/readyz` from k8s `readinessProbe` and `/healthz`
from `livenessProbe`.

### Reference patterns

* **Cardinality bounding** —
  `claude-code-leak/src/services/analytics/datadog.ts:195-217`
  (MCP tool collapsing) and `:281-299` (model-name normalisation).
  Direct port: 256-tool allowlist + `"other"` collapse.
* **In-tree precedent** —
  `crates/web-search/src/telemetry.rs:14-260` (8-bucket histogram
  layout), `crates/core/src/telemetry.rs:483-557` (aggregator).
* **Anti-pattern flagged** — `crates/poller/src/telemetry.rs:74-94`
  uses user-provided `job_id: String` as a label, which can grow
  unboundedly. Phase 76.10 deliberately avoids unbounded labels.

## Defaults and hardening

`HttpTransportConfig::validate()` refuses to boot the HTTP
listener when the operator picks an insecure combination:

* Non-loopback `bind` without `auth_token_env`.
* Non-loopback `bind` with empty `allow_origins`.
* Non-loopback `bind` with `allow_origins: ["*"]`.
* `body_max_bytes` above the 16 MiB hard cap.
* `session_idle_timeout_secs` above 86 400 s (24 h hard cap).
* `request_timeout_secs` above 600 s.
* `session_max_lifetime_secs < session_idle_timeout_secs`.

Body parsing is hardened against pathological inputs:

* JSON nesting beyond depth 64 is rejected (`-32600`) BEFORE
  `serde_json` allocates — defends against stack-overflow
  payloads.
* Batch (array) requests are rejected (MCP 2025-11-25 forbids
  them).
* `method` and `params.name` strings beyond 64 KiB are rejected.
* Notifications (`id` absent) yield `202 No Content` and never
  produce a response body.

## Endpoints

### `POST /mcp`

JSON-RPC over HTTP. `initialize` allocates a new session — the
response carries `Mcp-Session-Id: <uuid>`. Every subsequent
request MUST include the same header; missing or unknown
session id returns `404`.

```bash
curl -i -H 'Authorization: Bearer ${TOKEN}' \
     -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}' \
     http://127.0.0.1:7575/mcp
```

### `GET /mcp` (SSE)

Opens a Server-Sent Events stream for unsolicited notifications
(`tools/list_changed`, future `progress` events). Required header
is `Mcp-Session-Id`. Stream events:

* `event: message` — JSON-RPC envelope from server to client.
* `event: lagged` — payload `{"dropped": <n>}` when the per-session
  buffer (default 256) overflows due to a slow consumer.
* `event: shutdown` — payload `{"reason": "<…>"}` on graceful
  daemon shutdown.
* `event: end` — payload `{"reason": "session_closed" | "max_age" | "expired"}`.

### `DELETE /mcp`

Tears down the session referenced by `Mcp-Session-Id`. Returns
`204` on success, `404` if the id is unknown. SSE consumers
listening on the same session receive `event: end` with
`reason: "session_closed"`.

### `GET /healthz` and `GET /readyz`

Always reachable, never authenticated, no origin check.
`/healthz` returns `200 ok` while the listener is alive.
`/readyz` returns `503` until the first successful `initialize`,
then `200` for the rest of the process lifetime.

### Legacy SSE alias (`enable_legacy_sse: true`)

* `GET /sse` — opens an SSE stream and emits a single
  `event: endpoint` whose `data` is the absolute URL the client
  must POST to (`http://<host>/messages?sessionId=<uuid>`).
  Subsequent server→client events come through the same stream.
* `POST /messages?sessionId=X` — equivalent to `POST /mcp`, but
  the JSON-RPC response is delivered on the SSE stream as an
  `event: message` rather than in the HTTP body. The HTTP body
  is `202 No Content`.

## Reverse-proxy guidance

In production, terminate TLS in front of the agent. Example
nginx snippet:

```nginx
server {
    listen 443 ssl http2;
    server_name mcp.example.com;
    ssl_certificate /etc/letsencrypt/live/mcp.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mcp.example.com/privkey.pem;

    location /mcp {
        proxy_pass http://127.0.0.1:7575;
        proxy_http_version 1.1;
        proxy_buffering off;          # keep SSE responsive
        proxy_read_timeout 1h;        # SSE long-poll
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

The agent's per-IP rate limiter trusts `X-Forwarded-For` only when
the listener is bound to loopback (operator behind a proxy);
otherwise the direct peer IP is authoritative.

## Exposing additional tools (Phase 76.16)

By default the MCP server exposes the five agent introspection tools
(`who_am_i`, `what_do_i_know`, `my_stats`, `memory`, `session_logs`).
To surface any subset of the Phase 79 agentic tools to external MCP
clients, add them to `expose_tools` in `config/mcp_server.yaml`:

```yaml
mcp_server:
  expose_tools:
    - EnterPlanMode   # puts the session into read-only plan review mode
    - ExitPlanMode    # lifts plan-mode; requires operator approval
    - ToolSearch      # on-demand schema fetch for deferred tools
    - TodoWrite       # ephemeral intra-turn checklist
    - SyntheticOutput # typed/structured output forcing
    - NotebookEdit    # Jupyter cell-level edits
    - RemoteTrigger   # webhook / NATS publish from inside a turn
```

Unknown names and the two gated tools (`Config`, `Lsp`) are skipped
with a `tracing::warn!` log at startup — the daemon continues
normally. The existing `allowlist` field in `mcp_server.yaml` still
applies on top of `expose_tools`, letting operators further restrict
which of the registered tools each client session may call.

Denied-by-default tools (`Heartbeat`, `delegate`, `RemoteTrigger`)
require an additional safe profile:

1. List the tool in `expose_denied_tools`.
2. Enable `denied_tools_profile.enabled`.
3. Set the matching `denied_tools_profile.allow.* = true`.

Example (safe minimal override for reminders only):

```yaml
mcp_server:
  auth_token_env: MCP_SERVER_TOKEN
  expose_tools: ["Heartbeat"]
  expose_denied_tools: ["Heartbeat"]
  denied_tools_profile:
    enabled: true
    require_auth: true
    require_delegate_allowlist: true
    require_remote_trigger_targets: true
    allow:
      heartbeat: true
      delegate: false
      remote_trigger: false
```

> **Security note:** `Config` (self-config write-back) and `Lsp`
> (in-process rust-analyzer / pylsp) require additional infrastructure
> and are deferred to a later sub-phase. They are intentionally not
> enabled via `expose_tools` today.

## Testing the server

Run the full conformance + fuzz suite (Phase 76.12):

```bash
cargo test -p nexo-mcp --features server-conformance
```

This runs:
- **5 proptest cases** over `parse_jsonrpc_frame` — arbitrary bytes,
  strings, methods, depths, and batch arrays. Invariant: no panic.
- **11 HTTP conformance cases** — MCP 2025-11-25 spec fixtures via HTTP transport.
- **11 stdio conformance cases** — same fixtures via stdio transport,
  verifying transport parity.

For the load smoke test (50 sessions × 200 requests = 10 000 calls,
p99 gate < 500 ms; takes ~5 s):

```bash
cargo test -p nexo-mcp --features server-conformance \
    -- --include-ignored load_smoke
```

## Coming in later sub-phases

* **76.13** — TLS in-process (`rustls` behind `server-tls` feature)
  and nginx/caddy/Traefik reverse-proxy recipes.
* **76.14** — `nexo mcp-server` CLI ops: `inspect`, `bench`,
  `tail-audit`.

Track the rollout in [`PHASES.md`](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/PHASES.md)
and the public surface diff in [`CLAUDE.md`](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/CLAUDE.md).
