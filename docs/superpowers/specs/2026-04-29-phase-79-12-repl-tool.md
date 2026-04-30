## Spec: Phase 79.12 — REPLTool (stateful code execution sandbox)

### Descripción
A `Repl` tool that spawns persistent Python/Node/bash subprocesses, keeps them alive across LLM turns, and lets the agent execute code and read output. Provider-agnostic. Feature-gated.

### Alcance (IN)
- `spawn` action — launch Python, Node, or bash REPL subprocess with piped stdio
- `exec` action — send code to a running REPL session, read back output
- `read` action — read latest output from a session without sending code
- `kill` action — terminate a session by ID
- `list` action — list all active sessions (id, runtime, uptime, output bytes)
- Sessions persist across turns via `ReplRegistry` stored in `AgentContext`
- Config: `repl.enabled` (bool), `repl.allowed_runtimes` (list), `repl.max_sessions` (u32), `repl.timeout_secs` (u32), `repl.max_output_bytes` (u64)
- Feature flag: `repl-tool` in root `Cargo.toml` (off by default)
- Output buffer capped at `max_output_bytes` — oldest bytes dropped
- Per-session working directory (defaults to agent workspace)
- Graceful timeout: `exec` returns partial output + `"timed_out": true` after `timeout_secs`
- Stderr merged into output stream with `[stderr]` prefix

### Fuera de alcance (OUT)
- Sandbox/container isolation (deferred to 79.12.b)
- PTY allocation / terminal emulation (deferred to 79.12.c)
- Remote REPL bridge (deferred — separate phase)
- Wrapping/ hiding other tools (Claude Code wrapper pattern — rejected)
- Windows support in MVP (Unix pipes only)
- `repl_tool.rs` inside extensions/ — lives in `crates/core/` as a built-in

### Interfaces

#### Config (YAML)
```yaml
repl:
  enabled: false
  allowed_runtimes: ["python", "node", "bash"]
  max_sessions: 3
  timeout_secs: 30
  max_output_bytes: 65536
```

#### Config schema (Rust)
```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_allowed_runtimes")]
    pub allowed_runtimes: Vec<String>,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u32,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u32,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: u64,
}
```

#### ToolDef JSON Schema
```json
{
  "name": "Repl",
  "description": "Run code in a stateful Python, Node.js, or bash REPL session. Sessions persist across turns — spawn once, exec many times. Use for multi-step data analysis, prototyping, or iterative debugging.",
  "parameters": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["spawn", "exec", "read", "kill", "list"]
      },
      "session_id": {
        "type": "string",
        "description": "Session ID returned by spawn. Required for exec, read, kill."
      },
      "runtime": {
        "type": "string",
        "enum": ["python", "node", "bash"],
        "description": "REPL runtime. Required for spawn."
      },
      "code": {
        "type": "string",
        "description": "Code to execute. Required for exec. For Python/Node: a complete statement or block. For bash: a shell command."
      },
      "cwd": {
        "type": "string",
        "description": "Working directory for the session. Defaults to agent workspace. Only valid on spawn."
      }
    },
    "required": ["action"]
  }
}
```

#### Trait / Struct
```rust
// crates/core/src/agent/repl_tool.rs

pub struct ReplTool {
    registry: Arc<ReplRegistry>,
    config: ReplConfig,
}

impl ReplTool {
    pub fn new(registry: Arc<ReplRegistry>, config: ReplConfig) -> Self;
    pub fn tool_def() -> ToolDef;
}

#[async_trait]
impl ToolHandler for ReplTool {
    async fn call(&self, ctx: &AgentContext, args: Value) -> anyhow::Result<Value>;
}

// crates/core/src/agent/repl_registry.rs

pub struct ReplSession {
    pub id: String,
    pub runtime: String,
    pub cwd: String,
    pub spawned_at: chrono::DateTime<chrono::Utc>,
    pub output_len: u64,
    pub exit_code: Option<i32>,
}

pub struct ReplRegistry {
    sessions: DashMap<String, ReplProcess>,
    config: ReplConfig,
}

impl ReplRegistry {
    pub fn new(config: ReplConfig, workspace_dir: PathBuf) -> Self;
    pub async fn spawn(&self, runtime: &str, cwd: Option<&str>) -> Result<String>;
    pub async fn exec(&self, session_id: &str, code: &str) -> Result<ReplOutput>;
    pub async fn read(&self, session_id: &str) -> Result<ReplOutput>;
    pub async fn kill(&self, session_id: &str) -> Result<()>;
    pub fn list(&self) -> Vec<ReplSession>;
}

pub struct ReplOutput {
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
}

struct ReplProcess {
    session: ReplSession,
    child: Mutex<tokio::process::Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout_buf: Mutex<Vec<u8>>,
    stderr_buf: Mutex<Vec<u8>>,
}
```

### Casos de uso
1. **Happy path — Python analysis**: Agent spawns Python session → execs `import json; data = ...` → execs `print(json.dumps(result))` → reads formatted output → kills session
2. **Happy path — Node prototyping**: Agent spawns Node session → execs `const fs = require('fs'); ...` → execs more code referencing previous variables
3. **Timeout recovery**: Agent execs a long-running loop → timeout fires → agent receives partial output + `timed_out: true` → agent can exec again or kill
4. **Session limit**: Agent tries to spawn a 4th session when max_sessions=3 → error: "session limit reached (3)"
5. **Dead process**: Agent execs into a session whose subprocess has died → error: "session terminated (exit code 1)"
6. **Runtime not allowed**: Agent tries to spawn `ruby` when allowed_runtimes=["python", "node"] → error: "runtime 'ruby' not in allowed list"

### Dependencias
- Crates nuevos: none
- Crates del workspace: `nexo-core` (tool lives here), `nexo-config` (config schema)

### Decisiones de diseño
- **`ReplRegistry` en `AgentContext` en vez de singleton global** — cada agente tiene sus propias sesiones, sin leaking entre bindings
- **action-based schema en vez de multi-tool** — un solo tool con `action` enum es más fácil de registrar, clasificar, y exponer vía MCP que 5 tools separados (`repl_spawn`, `repl_exec`, etc.)
- **`tokio::process::Command` sobre `std::process::Command`** — no bloquea el runtime async
- **Output buffer en memoria con cap** — simple, rápido, consistente con el patrón de OpenClaw (`outputCapped`); sin archivo temporal
- **Feature flag `repl-tool` off by default** — herramienta peligrosa (ejecución de código arbitrario), debe ser opt-in explícito
- **Stderr mergeado con prefijo `[stderr]`** — más fácil para el LLM que procesar dos campos separados, consistente con cómo BashTool maneja stderr
- **No sandbox en MVP** — el proceso hijo hereda los permisos del agente. La sandbox (bubblewrap/Docker) se agrega en 79.12.b cuando el caso de uso lo justifique. Esto mantiene MVP simple y funcional en Termux/ARM.
