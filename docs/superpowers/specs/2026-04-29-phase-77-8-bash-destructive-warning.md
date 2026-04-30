## Spec: 77.8 — bashSecurity destructive-command warning

### Descripción
Antes de ejecutar un comando Bash, detecta patrones destructivos conocidos (git reset --hard, rm -rf, DROP TABLE, terraform destroy, etc.) y adjunta una advertencia informativa a la respuesta de permisos. No bloquea — solo informa.

### Alcance (IN)
- 14 patrones regex compilados estáticamente (`LazyLock<[Regex]>`) cubriendo git destructivo, `rm` agresivo, SQL DDL, y herramientas de infra
- Función `check_destructive_command(command: &str) -> Option<&'static str>` — sin alloc, sin async
- Campo `warning: Option<String>` en `PermissionResponse` para transportar la advertencia
- Integración en `PermissionMcpServer::call_tool`: cuando `tool_name == "Bash"`, extrae `command` del input, corre el chequeo, adjunta warning al response
- Feature gate via config `driver/permission/bash_security.destructive_warning: true` (default true)
- Tests unitarios: cada patrón con comando real, negativos (comandos seguros), idempotencia

### Fuera de alcance (OUT)
- NO afecta la decisión allow/deny — puramente informativo
- NO usa parser AST — regex bastan (mismo approach que Claude Code)
- NO cubre PowerShell, cmd.exe, o shells no-Bash
- NO cubre `chmod 777`, `chown`, `dd`, `mkfs`, `shutdown` — scope conservador
- NO emite eventos NATS separados — el warning viaja en el response de permiso existente

### Interfaces

#### Función pública
```rust
/// Checks a bash command string against known destructive patterns.
/// Returns a human-readable warning, or `None` if no pattern matches.
/// The first match wins; subsequent patterns are not checked.
pub fn check_destructive_command(command: &str) -> Option<&'static str>;
```

#### Patrones (port directo de claude-code-leak)
```rust
// Git — data loss / hard to reverse
("git reset --hard",     "may discard uncommitted changes"),
("git push --force",      "may overwrite remote history"),  // also matches --force-with-lease, -f
("git clean.*-.*f",       "may permanently delete untracked files"), // --dry-run excluded
("git checkout\\s+--\\s+\\.", "may discard all working tree changes"),
("git restore\\s+--\\s+\\.",  "may discard all working tree changes"),
("git stash\\s+(drop|clear)",  "may permanently remove stashed changes"),
("git branch\\s+(-D|--delete\\s+--force)", "may force-delete a branch"),

// Git — safety bypass
("git\\s+(commit|push|merge).*--no-verify", "may skip safety hooks"),
("git\\s+commit.*--amend", "may rewrite the last commit"),

// File deletion
("rm\\s+-[a-zA-Z]*[rR][a-zA-Z]*f", "may recursively force-remove files"),
("rm\\s+-[a-zA-Z]*[rR]",           "may recursively remove files"),
("rm\\s+-[a-zA-Z]*f",              "may force-remove files"),

// Database
("(DROP|TRUNCATE)\\s+(TABLE|DATABASE|SCHEMA)", "may drop or truncate database objects"),
("DELETE\\s+FROM\\s+\\w+\\s*(;|\"|'|\n|$)",   "may delete all rows from a table"),

// Infrastructure
("kubectl\\s+delete",      "may delete Kubernetes resources"),
("terraform\\s+destroy",   "may destroy Terraform infrastructure"),
```

#### Cambio en `PermissionResponse`
```rust
pub struct PermissionResponse {
    pub tool_use_id: String,
    pub outcome: PermissionOutcome,
    pub rationale: String,
    /// Phase 77.8 — optional warning surfaced in the permission dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
```

#### Cambio en `PermissionMcpServer::call_tool`
```rust
// After decider.decide(), if tool_name == "Bash":
let warning = if self.bash_destructive_warning_enabled {
    req.input.get("command")
        .and_then(Value::as_str)
        .and_then(check_destructive_command)
        .map(String::from)
} else {
    None
};
// Attach to resp.warning
```

#### Config (YAML)
```yaml
# crates/driver-permission/src/config.rs o similar
bash_security:
  destructive_warning: true   # default true
```

### Casos de uso
1. **Happy path**: Claude propone `rm -rf node_modules/`. PermissionMcpServer adjunta warning "may recursively force-remove files". El usuario ve la advertencia en el diálogo y decide si aprobar.
2. **Comando seguro**: Claude propone `ls -la`. `check_destructive_command` retorna `None`. Sin warning en el response.
3. **Edge case — comando en pipe**: `echo "DROP TABLE users;" | grep DROP` — el regex matchea el string literal, warning se muestra. Falso positivo aceptable (es solo advertencia).
4. **Edge case — feature deshabilitado**: `destructive_warning: false` → nunca se llama `check_destructive_command`. Zero overhead.

### Dependencias
- Crates nuevos: `regex = "1"` (workspace dep, ya existe)
- Crates del workspace: `nexo-driver-permission` (modificado), `nexo-driver-types` (si movemos config ahí)

### Decisiones de diseño
- **Regex vs AST parser**: Regex — mismo approach que Claude Code. AST parser (tree-sitter) sería más preciso pero añade complejidad y deps pesadas. Falsos positivos en strings/heredocs son aceptables porque solo es un warning informativo.
- **Campo en PermissionResponse vs campo en PermissionOutcome**: Va en `PermissionResponse` — es metadata del response completo, no del outcome específico. Si el outcome es Deny, el warning sigue siendo relevante.
- **Feature gate en config vs env var**: Config YAML — consistente con el resto del framework. `NEXO_BASH_SECURITY_DESTRUCTIVE_WARNING=false` sería más rápido pero rompe la convención.
