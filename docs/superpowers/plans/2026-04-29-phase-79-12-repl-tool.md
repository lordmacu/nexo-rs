## Plan: Phase 79.12 — REPLTool

### Crate(s) afectados
- `crates/core/` — new tool module + registry
- `crates/config/` — ReplConfig schema
- Root `Cargo.toml` — feature flag
- `src/main.rs` — registration

### Archivos nuevos
- `crates/core/src/agent/repl_tool.rs` — tool definition + ToolHandler impl
- `crates/core/src/agent/repl_registry.rs` — ReplRegistry + ReplProcess management

### Archivos modificados
- `crates/core/src/agent/mod.rs` — add `pub mod repl_tool;` + `pub mod repl_registry;` + re-exports
- `crates/core/Cargo.toml` — add `repl-tool` feature flag
- `crates/config/src/types/mod.rs` — add `ReplConfig` struct
- `crates/core/src/agent/context.rs` — add `repl_registry: Option<Arc<ReplRegistry>>` field
- `crates/core/src/plan_mode.rs` — add `"Repl"` to `MUTATING_TOOLS` + `classify_tool`
- `src/main.rs` — register ReplTool when feature enabled + config allows
- `Cargo.toml` (root) — add `repl-tool` feature forwarding to `nexo-core/repl-tool`

### Pasos

1. [ ] **ReplConfig schema** — `crates/config/src/types/repl.rs` + register in `mod.rs`
   - Struct with `enabled`, `allowed_runtimes`, `max_sessions`, `timeout_secs`, `max_output_bytes`
   - Default impl: `enabled: false`, `allowed_runtimes: ["python", "node", "bash"]`, `max_sessions: 3`, `timeout_secs: 30`, `max_output_bytes: 65536`
   - Criterion: `cargo build -p nexo-config` compiles

2. [ ] **ReplRegistry** — `crates/core/src/agent/repl_registry.rs`
   - `ReplRegistry::new(config, workspace_dir)` — no sessions on creation
   - `spawn(runtime, cwd)` — validates runtime in allowlist, checks session cap, spawns `tokio::process::Command` with piped stdio, stores in `DashMap<String, ReplProcess>`
   - `exec(session_id, code)` — writes code + newline to stdin, waits timeout_secs, reads stdout/stderr, returns `ReplOutput`
   - `read(session_id)` — reads current stdout/stderr buffers without writing
   - `kill(session_id)` — sends SIGTERM, waits 2s, SIGKILL if still alive
   - `list()` — returns `Vec<ReplSession>` with id, runtime, cwd, spawned_at, output_len, exit_code
   - Output buffer: spawned task reads stdout/stderr line-by-line into `Vec<u8>` with cap
   - Dead process detection: check `child.try_wait()` before exec/read
   - Criterion: `cargo build -p nexo-core` compiles, unit tests in module

3. [ ] **ReplTool + ToolHandler** — `crates/core/src/agent/repl_tool.rs`
   - `ReplTool { registry: Arc<ReplRegistry>, config: ReplConfig }`
   - `ReplTool::tool_def()` — returns `ToolDef` with action-based JSON schema
   - `ToolHandler::call()` dispatches by `action` field: spawn/exec/read/kill/list
   - Error handling: invalid action → error, missing session_id → error, spawn without runtime → error
   - Criterion: `cargo build -p nexo-core` compiles

4. [ ] **Feature flag + mod wiring** — `crates/core/Cargo.toml` + `crates/core/src/agent/mod.rs`
   - Feature `repl-tool` in `nexo-core/Cargo.toml` (no extra deps)
   - `#[cfg(feature = "repl-tool")]` on `pub mod repl_tool;` and `pub mod repl_registry;` in `mod.rs`
   - Re-exports gated on feature
   - Criterion: `cargo build -p nexo-core` (no feature) + `cargo build -p nexo-core --features repl-tool` both compile

5. [ ] **AgentContext wiring** — `crates/core/src/agent/context.rs`
   - Add `repl_registry: Option<Arc<ReplRegistry>>` field, default `None`
   - Add `with_repl_registry(&mut self, registry: Arc<ReplRegistry>)` builder method
   - Criterion: `cargo build -p nexo-core --features repl-tool` compiles

6. [ ] **Plan mode classification** — `crates/core/src/plan_mode.rs`
   - Add `"Repl"` to `MUTATING_TOOLS`
   - Add `"Repl" => ToolKind::Bash` in `classify_tool()` (REPL can exec arbitrary code, similar risk profile to Bash)
   - Criterion: `cargo build -p nexo-core --features repl-tool` compiles

7. [ ] **main.rs registration** — `src/main.rs` + root `Cargo.toml`
   - Root `Cargo.toml`: add `repl-tool = ["nexo-core/repl-tool"]` feature
   - `Cargo.toml`: wire `repl-tool` feature forwarding
   - `main.rs`: when `repl.enabled` in binding config + feature enabled:
     - Create `ReplRegistry::new(repl_config, workspace_dir)`
     - Store `Arc<ReplRegistry>` in `AgentContext` via `with_repl_registry()`
     - Register `ReplTool::new(registry, repl_config)` in tool registry
   - Read `repl` config section from binding YAML
   - Criterion: `cargo build --features repl-tool` compiles, `cargo build` (no feature) compiles

8. [ ] **Unit tests** — `repl_registry.rs` inline `#[cfg(test)]`
   - `test_spawn_python_hello_world` — spawn python, exec `print("hello")`, read stdout contains "hello"
   - `test_spawn_node_eval` — spawn node, exec `console.log(1+1)`, read stdout contains "2"
   - `test_spawn_bash` — spawn bash, exec `echo ok`, read stdout contains "ok"
   - `test_spawn_rejects_disallowed_runtime` — spawn ruby when only python allowed → error
   - `test_session_limit` — spawn 4 sessions with max=3 → 4th fails
   - `test_kill_session` — spawn, kill → exec after kill fails with "session terminated"
   - `test_timeout` — exec code with `sleep(60)` → returns `timed_out: true`
   - `test_list_sessions` — spawn 2 sessions → list returns 2 entries
   - `test_output_buffer_cap` — exec code that prints 200KB → output truncated to max_output_bytes
   - Criterion: `cargo test -p nexo-core --features repl-tool` all pass (skip tests that need python/node if not installed)

9. [ ] **Workspace build gate** — full workspace compilation
   - `cargo build --workspace --features repl-tool`
   - `cargo build --workspace` (no feature)
   - `cargo test --workspace --features repl-tool`
   - Criterion: all pass, no regressions

### Tests a escribir
- `repl_registry.rs` inline — 9 tests covering spawn/exec/read/kill/list/timeout/cap/errors
- `tests/repl_integration.rs` (optional, deferred) — end-to-end with real agent loop

### Riesgos
- **Python/Node/bash no instalados en CI** — mitigación: tests checked `which python3 || which python` y skip si no está; CI ya tiene python3 + node
- **Proceso zombie** — mitigación: `ReplRegistry` implementa `Drop` que hace `kill_all()` y `wait()` en todos los hijos
- **Deadlock en stdin write** — mitigación: `tokio::time::timeout` en `exec`, si el proceso no consume stdin, se cancela
- **Output encoding** — mitigación: leer como bytes, convertir a UTF-8 con `String::from_utf8_lossy`
- **Race entre read background task y exec** — mitigación: `Mutex` en buffers de output, drain antes de cada exec
