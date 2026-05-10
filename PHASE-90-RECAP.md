# Phase 90 — Plugin Admin Out-of-Tree

**Sesión**: 2026-05-10
**Estado**: 18/18 sub-fases ✅ shipped + 2 follow-ups deferred

Este documento resume **qué se logró** en Phase 90 y **qué queda** (deferred follow-ups + futuras fases relacionadas). Punto de referencia para retomar el trabajo después.

---

## 1. Qué se logró

### 1.1 Pivot estratégico

**Antes**: framework + microapp `agent-creator` como producto primario.
**Ahora**: framework = producto, microapps = demos. Plugin admin oficial out-of-tree gestiona el framework completo.

### 1.2 Repo nuevo: `nexo-rs-plugin-admin`

- 📦 **crates.io**: [`nexo-plugin-admin`](https://crates.io/crates/nexo-plugin-admin) — versiones 0.1.0 → **0.1.9** (9 releases)
- 🌐 **GitHub**: https://github.com/lordmacu/nexo-rs-plugin-admin (público)
- 📝 README + CHANGELOG.md (Keep-a-Changelog format)
- 🧪 3 e2e smoke tests opt-in (`cargo test --tests -- --ignored`)
- 🏷️ 9 release tags (v0.1.0 → v0.1.9)

### 1.3 Backend (`proyecto/`)

| Crate | Antes | Después | Cambio |
|-------|-------|---------|--------|
| `nexo-tool-meta` | 0.1.6 | **0.1.11** | +24 wire types (mcp + plugin_doctor + memory + snapshot) |
| `nexo-core` | 0.1.6 | **0.1.11** | +5 admin RPC dominios + dispatcher routes + capability gates |

**5 nuevos admin RPC dominios** wired en `nexo-core::agent::admin_rpc::domains::`:

1. `mcp` — `nexo/admin/mcp/{list,get,upsert,delete}` + capability `mcp_crud`
2. `plugin_doctor` — `nexo/admin/plugins/doctor` + capability `plugin_doctor`
3. `memory` — `nexo/admin/memory/query` + capability `memory_query`
4. `memory snapshot` — `nexo/admin/memory/{list_snapshots,delete_snapshot}` + capability `memory_snapshot`
5. (plus existing tenants/llm_keys/etc tied via plugin admin)

### 1.4 Frontend del plugin admin (13 módulos LIVE)

| Rail | Módulo | Wire | Estado |
|------|--------|------|--------|
| 10 | dashboard | `agents+llm+audit Promise.allSettled` | ✅ live |
| 20 | agents | `nexo/admin/agents/{list,get,upsert,delete}` | ✅ live (lift from agent-creator) |
| 30 | skills | `nexo/admin/skills/{list,get,delete}` | ✅ live |
| 40 | llm_keys | `nexo/admin/llm_providers/{list,upsert,delete}` + wizard reuse | ✅ live |
| 45 | channels | `nexo/admin/channels/{list,approve,revoke}` | ✅ live (con approve UX) |
| 50 | memory | `nexo/admin/memory/query` + snapshot list+delete panel | ✅ live |
| 60 | audit | `nexo/admin/microapp_audit/tail` | ✅ live (lift) |
| 70 | plugins | `nexo/admin/plugins/doctor` | ✅ live |
| 80 | mcp_servers | `nexo/admin/mcp/{list,get,upsert,delete}` | ✅ live |
| 85 | tenants | `nexo/admin/tenants/{list,upsert,delete}` | ✅ live (con CRUD modal) |
| 90 | settings | `nexo/admin/auth/rotate_token` | ✅ live |
| — | chats | transcript firehose + takeover | ✅ live (lift) |
| — | wizard | first-run setup | ✅ live (lift) |

**Stack frontend**: React 18 + Vite + TS + Tailwind + zustand + react-router + i18n (es/en) + ts-rs codegen pipeline + `@lordmacu/nexo-microapp-ui-react@0.1.0`.

### 1.5 Limpieza del daemon

| Artefacto | Cambio | LOC |
|-----------|--------|-----|
| `proyecto/admin-ui/` | borrado completo | **−5342** |
| `proyecto/src/main.rs::run_admin_web` + helpers + AdminUiAssets | borrado | **−2702** |
| `nexo-rs-plugin-admin/frontend` marketing module + deps | dropped | −884 |
| Plugin admin Rust scaffold | nuevo | +1500 |
| Plugin admin frontend (cloned + 9 módulos nuevos) | nuevo | +3500 |
| `run_admin_via_plugin` shim en `main.rs` | nuevo | +75 |
| **Net** | **−3853 LOC** | |

**Daemon warnings**: 45 → **0**.

### 1.6 Auth + tunnel adapters

- HMAC-SHA256 cookie session 24h TTL (lifted desde legacy `run_admin_web`)
- Random password per-launch impreso en stderr
- 3 tunnel adapters: `none` (default loopback) / `cloudflared` / `tailscale` selectable via `NEXO_ADMIN_TUNNEL` env
- TokenRotated cookie session swap — `nexo/notify/token_rotated` listener wires both `LiveTokenState` (bearer) + `LiveAdminSession` (cookie HMAC) atomically

### 1.7 Drift prevention

- ts-rs codegen wired para 17 nuevos wire structs
- `types.gen.ts` regenerado: 13 → **37 types**, 1285 LOC
- Mirror sync entre `agent-creator-microapp/` y `nexo-rs-plugin-admin/`
- `.gitignore` actualizado (intermediate `crates/tool-meta/bindings/` dir)

### 1.8 Phase 90 follow-ups closed

| # | Item | Cómo |
|---|------|------|
| 1 | tenants CRUD wrappers | `tenantsUpsert`/`tenantsSetActive`/`tenantsDelete` + modal + activate/deactivate buttons |
| 2 | mcp_servers admin RPCs | `McpServerStore` trait + `McpYamlStore` + create modal |
| 3 | plugins doctor admin RPC | `PluginDoctorReader` + `LivePluginDoctorReader` + summary tiles + diagnostics |
| 4 | memory query admin RPC | `MemoryReader` + `LiveMemoryReader` lazy + query form |
| 5 | channel approve UX | `ChannelApproveModal` + agent picker + server picker autocomplete + allowlist editor |
| 6 | TokenRotated cookie swap | listener wires bearer + cookie atomically on auth_rotate |
| 7 | memory snapshot list/delete | `MemorySnapshotReader` + shared cell + delete confirm |
| 8 | GitHub remote push | repo creado vía `gh repo create`, 11 commits + 9 tags pushed |

### 1.9 Comandos para usar lo shipped

```bash
# Operator install
cargo install nexo-plugin-admin   # 0.1.9 from crates.io

# Boot daemon — auto-discovers + spawns the plugin
agent run

# Browse to http://127.0.0.1:18000
# (login con la pass impresa en stderr del daemon)

# Public exposure (opt-in)
NEXO_ADMIN_TUNNEL=cloudflared agent run
NEXO_ADMIN_TUNNEL=tailscale  agent run

# Run e2e smoke tests
cd /path/to/nexo-rs-plugin-admin
cargo build && cargo test --tests -- --ignored
```

---

## 2. Qué falta

### 2.1 Phase 90 follow-ups deferred (1 item, was 2)

#### 2.1.1 ~~Memory snapshot create/restore admin RPCs~~ ✅ shipped 2026-05-10

**Estado**: shipped en `nexo-plugin-admin@0.1.10` + `nexo-tool-meta@0.1.12` + `nexo-core@0.1.12` (sub-fase `90.x.memory-snapshot.create-restore`).

**Lo entregado**:
- 5 nuevos wire types en `crates/tool-meta/src/admin/memory.rs`: `MemorySnapshotsCreateParams/Response`, `MemorySnapshotsRestoreParams/Response`, `RestoreReportWire`.
- Shape change en `MemorySnapshotsListResponse` — añade `encryption_available: bool` (SHAPE CHANGE: `Vec<SnapshotMetaWire>` → `struct { snapshots, encryption_available }`).
- Trait `MemorySnapshotReader` extendido con `create()` + `restore()` (naming hangover documentado, rename diferido al próximo major).
- 2 nuevos handlers en `crates/core/src/agent/admin_rpc/domains/memory.rs` + dispatcher arms (capability `memory_snapshot` reusada).
- `LiveMemorySnapshotReader::new(cell, encryption_section)` extendido en `crates/setup/src/admin_adapters.rs` — clonado de `EncryptionSection` al boot drives `encryption_available` flag, recipient resolution para create, identity resolution para restore.
- Defaults forzados server-side: `redact_secrets=true`, `auto_pre_snapshot=true`, `created_by="admin-ui"`.
- Restore por `snapshot_id` (server resuelve `bundle_path` via `list()` lookup — admin endpoint nunca acepta paths raw).
- Tenant REQUIRED + manifest validation rechaza mismatch antes de tocar disco.
- 8 unit tests nuevos en domains/memory + 4 integration tests en admin_adapters (mock snapshotter).
- Frontend pendiente Fase G: `CreateSnapshotModal` + `RestoreSnapshotModal` + `RestoreReportTable` + i18n keys es+en.
- Docs: `docs/src/ops/memory-snapshot.md` § Admin RPC surface explica defaults + tenant + encryption + dry-run + lock semantics.

#### 2.1.2 Plugin admin daemon-backed e2e test 🟡

**Estado**: 3 binary-only smoke tests shipped. Daemon-backed deferred.

**Por qué deferred**: CI heavy — necesita NATS broker + plugin discovery harness + temp config dir + temp state dir.

**Para shipearlo**:
1. Test harness en `tests/daemon_e2e.rs` que:
   - spawns nexo daemon en port aleatorio
   - copia plugin binary a temp dir + escribe plugin.toml
   - daemon discovery picks up plugin
   - waits for plugin's /healthz to come up
   - drives a real `nexo/admin/agents/list` via HTTP /api/admin
   - verifies wire response shape end-to-end
2. CI gate via `cargo test --features=e2e -- --include-ignored`

**Effort estimado**: 3-4h. **Trigger**: regression triage de bug en daemon↔plugin handshake.

### 2.2 Phase 81 — Community-tier readiness (3 items, heavy)

#### 2.2.1 Phase 81.20.c — Typing presence RPC ⏸

**Estado**: DEFERRED en `PHASES-curated.md`.
**Por qué**: requires `AgentContextRegistry` for per-running-agent state. ~2-3d arch work.
**Trigger**: real use case for daemon-mediated `tool.dispatch` from plugin. No demand yet.

#### 2.2.2 Phase 81.21.b.b — Plugin supervisor auto-respawn loop ⬜

**Estado**: 81.21.b shipped manifest fields (`supervisor.respawn`/`max_attempts`/`backoff_ms`/`stderr_tail_lines`) en 81.21. Loop NO wired.
**Por qué deferred**: 2-3d. Requires supervisor task that owns `SubprocessNexoPlugin` lifecycle OR Inner refactor.
**Para shipearlo**: nuevo `PluginSupervisorTask` que watches child exit + cancela bridge tasks + spawns fresh + redoes handshake con exponential backoff hasta `max_attempts`.

**Gate**: community-tier plugins (external operators). Sin esto un plugin que crashee se queda muerto hasta daemon restart.

#### 2.2.3 Phase 81.21.c — Plugin resource limits ⬜

**Estado**: NOT started. ~3d.
**OS-divergent**: linux cgroup v2 + rlimit / macOS sandbox-exec / fallback monitoring.
**Manifest knobs**: `limits.cpu_pct` / `limits.mem_mb` / `limits.startup_timeout_ms`.
**Gate**: required para community-tier (untrusted plugins).

### 2.3 Phase 83 — Microapp foundation (4 items pending)

#### 2.3.1 83.1 — Per-agent extension config propagation P2 ⬜

Microapp wants per-agent config maps; no es hard blocker.

#### 2.3.2 83.2 — Extension-contributed skills P2 ⬜

Microapp ships skills; opportunistic.

#### 2.3.3 83.9 — `ana` cutover P3 ⬜

Migration de yaml-only to extension-based; depends on 83.10.

#### 2.3.4 83.10 — Second microapp validation P1 ⬜ in-flight

`agent-creator-microapp` production validation, out-of-tree work at `/home/familia/chat/agent-creator-microapp/`. Driver: ¿el framework es realmente reusable por una segunda microapp?

### 2.4 Otros backlog (~75 ⬜ items)

Tracked in `proyecto/FOLLOWUPS.md`. Mayoría:
- Polish (UX details, additional tests)
- Observability (fire-site wiring)
- Phase 22-79 dormant backlog (deployment recipes, telemetry, eval harness, etc.)
- Phase 27 release pipeline (cosign signing / Termux / SBOM)

Ninguno bloquea Phase 90.

---

## 3. Cómo retomar

### 3.1 Si pickup es `Phase 81.21.b.b auto-respawn`

```bash
cd /home/familia/chat/proyecto
grep -rn "supervisor.respawn\|SupervisorConfig" crates/plugin-manifest/src/ crates/core/src/agent/nexo_plugin_registry/
# Read 81.21.b context — manifest fields are wired
# Build a new module: crates/core/src/agent/nexo_plugin_registry/supervisor.rs
# Test against extensions/template-plugin-rust (intentional crash → respawn)
```

### 3.2 Si pickup es `Memory snapshot create/restore`

```bash
cd /home/familia/chat/proyecto
# Read existing list/delete in:
# - crates/tool-meta/src/admin/memory.rs
# - crates/core/src/agent/admin_rpc/domains/memory.rs
# - crates/setup/src/admin_adapters.rs (LiveMemorySnapshotReader)
# Extend with create/restore mirror of list/delete pattern
# Frontend: extend modules/memory/MemoryMain.tsx with create button + restore button
```

### 3.3 Si pickup es `83.10 second microapp validation`

```bash
cd /home/familia/chat/agent-creator-microapp/
# Validation work — not in proyecto/. Verify the published
# nexo-microapp-sdk@0.1.14+ + nexo-plugin-admin@0.1.9 work
# end-to-end against agent-creator's own scope.
```

### 3.4 Si pickup es `Plugin admin daemon-backed e2e test`

```bash
cd /home/familia/chat/nexo-rs-plugin-admin/
# Add tests/daemon_e2e.rs — spawn daemon + plugin in subprocess,
# drive a CRUD through /api/admin, verify response shape.
# Mirror the smoke pattern in tests/handshake_smoke.rs but
# include the daemon-spawning side.
```

---

## 4. Pickup order recomendado

1. **Memory snapshot create/restore** (~4-6h) — completa el módulo memory y cierra el último 🟡 partial de Phase 90.
2. **Daemon-backed e2e test** (~3-4h) — close último Phase 90 follow-up.
3. **Phase 81.21.b.b auto-respawn** (~2-3d) — gates community-tier; high-value framework hardening.
4. **Phase 83.10 second microapp** (open-ended) — out-of-tree, real product validation.

Anything sub-day = 1+2. Anything multi-day = 3 (single-track) or 4 (parallel out-of-tree).

---

## 5. Referencias

- `proyecto/CLAUDE.md` — active phase tracker (Phase 90 row updated)
- `proyecto/PHASES-curated.md` — curated active scope
- `proyecto/FOLLOWUPS.md` — open follow-ups (Phase 90 section)
- `nexo-rs-plugin-admin/CHANGELOG.md` — per-version notes
- `nexo-rs-plugin-admin/README.md` — operator install + dev docs
