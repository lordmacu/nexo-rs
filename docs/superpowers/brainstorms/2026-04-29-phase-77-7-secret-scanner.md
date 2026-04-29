## Brainstorm: 77.7 — memdir secretScanner + teamMemSecretGuard

### Lo que hizo Claude Code

**Archivo principal:** `claude-code-leak/src/services/teamMemorySync/secretScanner.ts` (324 líneas)

25 reglas regex de alta especificidad, organizadas por familia:
- **Cloud**: AWS, GCP, Azure AD, DigitalOcean PAT/access-token
- **AI APIs**: Anthropic sk-ant-api03/sk-ant-admin01, OpenAI sk-proj/sk-svcacct/sk-admin, HuggingFace hf_
- **VCS**: GitHub (ghp_, github_pat_, ghu_/ghs_, gho_, ghr_), GitLab (glpat-, gldt-)
- **Comms**: Slack (xoxb-, xoxp/xoxe, xapp-), Twilio SK, SendGrid SG.
- **Dev tools**: npm_, PyPI pypi-AgEIcHlwaS5vcmc, Databricks dapi, HashiCorp atlasv1, Pulumi pul-, Postman PMAK-
- **Observability**: Grafana (eyJrIjoi, glc_, glsa_), Sentry (sntryu_, sntrys_)
- **Commerce**: Stripe sk/rk, Shopify shpat_/shpss_
- **Crypto**: `-----BEGIN ... PRIVATE KEY-----` (multiline, case-insensitive)
- **Genérico**: JWT `eyJ` — presente en diccionario de labels pero **sin regla regex activa**
- **Entropía**: NO implementan Shannon entropy — solo prefijos distintivos

**Dos integration points:**
1. **Pre-write guard** (`teamMemSecretGuard.ts:19`): `checkTeamMemSecrets(filePath, content)` en `validateInput` de FileWriteTool + FileEditTool → bloquea **antes** de escribir a disco. Gateado por `feature('TEAMMEM')`.
2. **Push sync** (`index.ts:567-673`): `readLocalTeamMemory()` escanea cada archivo antes de subir → archivos con secrets se excluyen del delta de subida. Emite warning + analytics event.

**Sin allowlists.** Sin skip markers. Sin entropy. Filosofía: solo reglas con prefijos distintivos que tienen near-zero false-positive rate. Reglas genéricas (`password =`, `secret =`) omitidas a propósito.

**Formato de error exacto:**
```
Content contains potential secrets ({labels}) and cannot be written to team memory. Team memory is shared with all repository collaborators. Remove the sensitive content and try again.
```

### Lo que NO hizo Claude Code — limitaciones explotables

- **Sin entropy detection**. Strings como `dGhpcyBpcyBhIHNlY3JldA==` (base64 de "this is a secret") pasan limpias si no matchean prefijo conocido.
- **Sin JWT regex**. La regla JWT nunca se implementó — solo está el label en el diccionario.
- **Sin escaneo de código fuente**. No detecta `const API_KEY = "..."` en snippets de código dentro de memorias.
- **Sin allowlist**. Si un archivo legítimo contiene un string que casualmente matchea (ej: documentación con example keys `sk-1234...`), se bloquea sin escape.
- **Team memory solo**. El guard solo aplica a archivos en paths de team memory — `remember()` a SQLite NO tiene scanner.

### Ideas para el framework Rust

1. **Scanner en `remember_typed()` y `ExtractMemories` — no solo en filesystem.**
   Claude Code solo escanea archivos de team memory en disco. Nosotros tenemos SQLite + filesystem + git. El scanner debe estar en TODAS las rutas de escritura a memoria persistente:
   - `LongTermMemory::remember_typed()` → escanear `content` antes del INSERT
   - `ExtractMemories::extract()` → escanear cada `MemoryFile.content` antes de `fs::write`
   - `Dreaming::append_to_memory_md()` → escanear `candidate.content` antes de escribir a MEMORY.md
   - `save_interaction()` → probablemente NO (demasiado volumen, y las interacciones son audit trail, no inyectadas en prompts)

2. **`SecretScanner` como struct con `scan(&self, content: &str) -> Vec<SecretMatch>`.**
   - `SecretMatch { rule_id: &'static str, label: &'static str, offset: usize }` — nunca incluye el valor del secret.
   - `SecretScanner::default()` carga las 25 reglas compiladas (regex estáticos con `once_cell::Lazy`).
   - `scan()` devuelve matches ordenados por offset, deduplicados por `rule_id`.
   - `has_secrets(&self, content: &str) -> bool` — fast path, short-circuits en primer match.

3. **`SecretGuard` como wrapper pre-commit.**
   - `SecretGuard::check(content: &str) -> Result<(), SecretBlocked>` — usa `SecretScanner`.
   - `SecretBlocked` error con `labels: Vec<String>` y `rule_ids: Vec<&'static str>`.
   - Quien llama (MemoryTool, ExtractMemories, Dreaming) decide si bloquear o warn+skip.

4. **Evento `MemorySecretBlocked` en el bus.**
   - `agent.memory.secret.blocked` con `{ memory_id?, rule_ids, content_hash }` — auditable sin exponer el secret.
   - Permite telemetría: ¿cuántos secrets está intentando escribir el LLM? ¿De qué tipo?

5. **Redact + write como alternativa a block.**
   - Claude Code tiene `redactSecrets()` pero no lo usa en el sync path.
   - Nosotros podemos ofrecerlo como opción: `on_secret: "block" | "redact" | "warn"` en config.
   - `redact`: reemplaza el match con `[REDACTED:rule_id]`, escribe el contenido redactado.
   - `warn`: escribe igual pero emite evento + log warning.

6. **Config YAML — sin feature flags de compilación.**
   ```yaml
   memory:
     secret_guard:
       enabled: true          # master switch (default true)
       on_secret: "block"     # block | redact | warn
       rules: "all"           # all | custom list of rule_ids
       exclude_rules: []      # rule_ids to skip (ej: false positive known)
   ```
   No usamos `feature('TEAMMEM')` — es runtime config, hot-reloadable.

7. **Path sandbox para extractMemories ya existe — reforzar.**
   `extract_memories.rs` ya tiene `resolve_memory_path()` con rechazo de `..` y paths absolutos. Agregar los checks adicionales de `teamMemPaths.ts`:
   - URL-encoded traversal (`%2e%2e%2f`)
   - Unicode normalization attacks (fullwidth characters)
   - Null byte rejection
   - Symlink resolution via `canonicalize()`

8. **Escaneo en git commit path (Phase 10.9).**
   `workspace_git.rs::commit_all()` debe escanear el diff antes de commitear. Si hay secrets en archivos staged, bloquear el commit con error claro. Esto es defensa en profundidad: si algo pasó el scanner pre-write, el git hook lo agarra.

### Ideas descartadas
- **Entropy detection (Shannon)** — Claude Code no lo hace. Agrega complejidad sin referencia de producción. Posponer a 77.7.b si hay falsos negativos.
- **gitleaks binary dependency** — overkill. Las 25 reglas regex cubren el 95% de los casos reales. No necesitamos un binary externo.
- **Escaneo de `save_interaction()`** — las interacciones son audit trail, no se inyectan en prompts. Escanear cada mensaje user/assistant agregaría overhead innecesario. Solo escanear rutas que alimentan el context window futuro.
- **Allowlist por archivo** — Claude Code no lo tiene. Agregaríamos complejidad sin evidencia de necesidad. Si un patrón da false positive, se excluye la regla completa via config.
