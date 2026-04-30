## Spec: 77.9 — bashSecurity sed-in-place + path validation

### Descripción
Valida comandos sed contra allowlist/denylist, advierte sobre ediciones in-place (-i), y extrae paths de 29 comandos comunes con manejo POSIX `--`.

### Alcance (IN)

#### 1. `sed_validator` — port de `sedValidation.ts`
- `sed_command_is_allowed(command: &str, allow_file_writes: bool) -> bool`
- Patrón 1 (line-printing): `sed -n 'Np'` / `sed -n 'N,Mp'` con flags -E/-r/-z
- Patrón 2 (substitution): `sed 's/pattern/replacement/flags'` con flags g/p/i/I/m/M/1-9
- Denylist: comandos w/W (write), e/E (execute), y/ (transliterate), bloques {}, negación !, step ~, backslash tricks, non-ASCII
- `extract_sed_expressions(command: &str) -> Vec<String>`
- `has_sed_file_args(command: &str) -> bool`

#### 2. `sed_in_place_warning` — detección de `sed -i`
- Regex estático: `sed\s+.*-i` (con variantes `--in-place`, `-i.bak`)
- Función: `check_sed_in_place(command: &str) -> Option<&'static str>`
- Integrado en `PermissionMcpServer::call_tool` junto al warning de 77.8
- Warning: "Note: sed -i modifies files in place"

#### 3. `path_extractor` — port de `PATH_EXTRACTORS`
- `PathCommand` enum con 29 comandos (cd/ls/find/mkdir/touch/rm/rmdir/mv/cp/cat/head/tail/sort/uniq/wc/cut/paste/column/tr/file/stat/diff/awk/strings/hexdump/od/base64/nl/grep/rg/sed/git/jq/sha256sum/sha1sum/md5sum)
- `extract_paths(command: PathCommand, args: &[String]) -> Vec<String>`
- `filter_out_flags(args: &[String]) -> Vec<String>` — respeta POSIX `--`
- `parse_command_args(cmd: &str) -> Vec<String>` — shell-quote simplificado (split respetando quotes)
- `classify_command(base_cmd: &str) -> Option<PathCommand>`

### Fuera de alcance (OUT)
- NO validatePath / checkPathConstraints — necesita filesystem + permission context
- NO checkDangerousRemovalPaths — necesita fs checks
- NO validateOutputRedirections
- NO stripWrappersFromArgv (timeout/nice/stdbuf/env) — va en 77.10
- NO AST parser — regex + string parsing bastan

### Interfaces

#### Funciones públicas
```rust
// sed_validator
pub fn sed_command_is_allowed(command: &str, allow_file_writes: bool) -> bool;
pub fn extract_sed_expressions(command: &str) -> Vec<String>;
pub fn has_sed_file_args(command: &str) -> bool;

// sed_in_place_warning (en bash_destructive.rs o nuevo módulo)
pub fn check_sed_in_place(command: &str) -> Option<&'static str>;

// path_extractor
pub fn extract_paths(command: PathCommand, args: &[String]) -> Vec<String>;
pub fn filter_out_flags(args: &[String]) -> Vec<String>;
pub fn classify_command(base_cmd: &str) -> Option<PathCommand>;
```

### Integración en mcp.rs
```rust
// En call_tool, misma zona que 77.8 — después del decider:
let sed_warning = if self.bash_destructive_warning_enabled
    && req.tool_name == "Bash"
{
    bash_command.as_deref().and_then(check_sed_in_place)
} else {
    None
};
// Se mergea con el warning existente si ambos están presentes
```

### Casos de uso
1. **sed -i**: `sed -i 's/foo/bar/g' file.txt` → warning "sed -i modifies files in place"
2. **sed read-only**: `sed -n '1,10p' file.txt` → sin warning, `sed_command_is_allowed` = true
3. **sed dangerous**: `sed 's/foo/bar/e' file.txt` → `sed_command_is_allowed` = false
4. **path extraction**: `rm -rf -- -/tmp/foo` → `extract_paths` captura `-/tmp/foo` (POSIX -- handling)

### Dependencias
- `regex = { workspace = true }` (ya en driver-permission)

### Decisiones de diseño
- **Rust vs port directo**: Adaptación idiomática — `Vec<String>` en vez de arrays mutables, `Option` en vez de null, enums en vez de string unions
- **Shell quote parsing**: Simplificado — split por whitespace respetando `'...'` y `"..."`. No necesitamos el parser completo de shell-quote para extraer paths.
- **Sed denylist**: Conservador — si hay duda, bloquea. Falsos positivos aceptables (requieren approval manual, no bloquean).
