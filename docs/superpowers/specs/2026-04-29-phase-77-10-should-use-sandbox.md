## Spec: 77.10 — bashSecurity shouldUseSandbox heuristic

### Descripción
Heuristic decides whether to wrap a Bash command in a sandbox (bwrap/firejail).
Probes for sandbox backend at boot; caches result. Pure decision function — actual
command wrapping is out of scope.

### Alcance (IN)

#### 1. `should_use_sandbox` module
- `SandboxMode` enum: `Auto`, `Always`, `Never`
- `SandboxBackend` enum: `Bubblewrap`, `Firejail`, `None`
- `SandboxProbe` — probes for `bwrap`/`firejail` on PATH, caches result
- `should_use_sandbox(command, mode, backend, dangerously_disable, excluded_commands) -> bool`

#### 2. Heuristic rules
- `Never` mode: always false
- `Always` mode: true (even if no backend — callers must handle missing backend)
- `Auto` mode: true only if backend is available
- `dangerously_disable_sandbox` flag: forces false regardless of mode
- Excluded commands: commands matching user-configured patterns skip sandbox

### Fuera de alcance (OUT)
- NO actual bwrap/firejail command wrapping — just the decision function
- NO network/filesystem restriction config
- NO sandbox violation monitoring
- NO sandbox initialization lifecycle

### Interfaces

```rust
// should_use_sandbox.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Bubblewrap,
    Firejail,
    None,
}

pub struct SandboxProbe {
    pub backend: SandboxBackend,
}

impl SandboxProbe {
    pub fn new() -> Self;
    pub fn backend(&self) -> SandboxBackend;
}

pub fn should_use_sandbox(
    command: Option<&str>,
    mode: SandboxMode,
    backend: SandboxBackend,
    dangerously_disable_sandbox: bool,
    excluded_commands: &[String],
) -> bool;
```

### Casos de uso
1. `Never` mode → always false
2. `Always` mode → true (caller responsible for missing-backend error)
3. `Auto` mode + bwrap available + non-excluded command → true
4. `Auto` mode + no backend → false
5. `dangerouslyDisableSandbox` set → false

### Dependencias
- No new crates needed

### Decisiones de diseño
- Probe at construction time, not lazily — caches result, no filesystem access on hot path
- Excluded commands: simple prefix/exact match (not wildcard — that's bashPermissions scope)
- Backend enum exposes which tool was found, so callers can build correct CLI args
