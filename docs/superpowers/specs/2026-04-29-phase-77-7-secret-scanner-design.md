## Spec: 77.7 — memdir secretScanner + teamMemSecretGuard

### Descripción

Secret scanner pre-commit que bloquea, redacta o advierte cuando contenido destinado a memoria persistente contiene API keys, tokens o private keys reconocibles. Opera en todas las rutas de escritura a memoria (SQLite `remember_typed()`, filesystem `ExtractMemories`, `Dreaming`, git commit). Basado en 25+ reglas regex de alta especificidad con prefijos distintivos — mismo approach que Claude Code `secretScanner.ts`.

### Alcance (IN)

- `SecretScanner` struct con reglas regex compiladas estáticamente (`once_cell::sync::Lazy`)
- `SecretMatch { rule_id, label, offset }` — nunca incluye el valor del secret
- `SecretScanner::scan(content) -> Vec<SecretMatch>` — ordenado por offset, deduplicado por rule_id
- `SecretScanner::has_secrets(content) -> bool` — fast path, short-circuit en primer match
- `SecretGuard` struct con política `Block | Redact | Warn`
- `SecretGuard::check(content) -> Result<String, SecretBlockedError>` — en modo Block, retorna error; en Redact, retorna contenido redactado; en Warn, retorna contenido intacto + emite evento
- `SecretBlockedError { rule_ids, labels, content_hash }` — auditable sin exponer secret
- Integración en `LongTermMemory::remember_typed()` — escanea `content` antes del INSERT
- Integración en `ExtractMemories::extract()` — escanea `MemoryFile.content` antes de `fs::write`
- Integración en `Dreaming::append_to_memory_md()` — escanea `candidate.content` antes de append
- Integración en `workspace_git::commit_all()` — escanea diff staged antes del commit
- Eventos `MemorySecretBlocked`, `MemorySecretRedacted`, `MemorySecretWarned`
- Config YAML `memory.secret_guard` con `enabled`, `on_secret`, `rules`, `exclude_rules`
- Reglas regex porteadas de `claude-code-leak/src/services/teamMemorySync/secretScanner.ts`
- Reforzar path sandbox en ExtractMemories: Unicode traversal, null byte, symlink resolution

### Fuera de alcance (OUT)

- Entropy detection (Shannon) — sin referencia de producción, posponer a 77.7.b
- Escaneo de `save_interaction()` — las interacciones son audit trail, no se inyectan en prompts
- Escaneo de `CompactionStore` — summaries de compaction son texto condensado, riesgo bajo
- Allowlist por archivo — Claude Code no lo tiene, complejidad innecesaria
- gitleaks binary dependency — overkill para las reglas
- Redact en nivel de caracter individual — solo reemplazo completo del match con `[REDACTED:rule_id]`

### Interfaces

#### Struct principal

```rust
/// A secret detected in content. Never contains the secret value itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    /// Rule identifier, e.g. "anthropic-api-key", "github-pat".
    pub rule_id: &'static str,
    /// Human-readable label, e.g. "Anthropic API Key".
    pub label: &'static str,
    /// Byte offset where the match starts.
    pub offset: usize,
}

/// Pre-commit policy for handling detected secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnSecret {
    /// Refuse the write entirely. Returns SecretBlockedError.
    Block,
    /// Replace matched secrets with [REDACTED:rule_id] and write.
    Redact,
    /// Write intact but emit warning event + log.
    Warn,
}

/// Error returned when a secret is detected and policy is Block.
#[derive(Debug, Clone, Error)]
#[error("content contains potential secrets ({labels}) and cannot be written to memory")]
pub struct SecretBlockedError {
    /// Human-readable labels of detected secrets (comma-joined).
    pub labels: String,
    /// Rule IDs that matched (for telemetry).
    pub rule_ids: Vec<&'static str>,
    /// SHA-256 of the content that was blocked (for audit correlation).
    pub content_hash: String,
}

/// The scanner. Holds compiled regexes. Created once, reused.
pub struct SecretScanner {
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    rule_id: &'static str,
    label: &'static str,
    regex: regex::Regex,
}

impl SecretScanner {
    /// Create with all default rules.
    pub fn new() -> Self { /* ... */ }

    /// Create with only the named rules. Unknown names are ignored.
    pub fn with_rules(rule_ids: &[&str]) -> Self { /* ... */ }

    /// Exclude named rules from the default set.
    pub fn without_rules(exclude_ids: &[&str]) -> Self { /* ... */ }

    /// Scan content and return all matches, deduplicated by rule_id,
    /// ordered by offset.
    pub fn scan(&self, content: &str) -> Vec<SecretMatch> { /* ... */ }

    /// Fast check — returns true on first match without collecting all.
    pub fn has_secrets(&self, content: &str) -> bool { /* ... */ }
}

/// The guard. Wraps a scanner with a policy decision.
pub struct SecretGuard {
    scanner: SecretScanner,
    on_secret: OnSecret,
}

impl SecretGuard {
    /// Check content. Returns:
    /// - Ok(content) if no secrets or policy is Warn (content unchanged)
    /// - Ok(redacted) if policy is Redact and secrets were found
    /// - Err(SecretBlockedError) if policy is Block and secrets were found
    pub fn check(&self, content: &str) -> Result<String, SecretBlockedError> { /* ... */ }
}
```

#### Config (YAML)

```yaml
# New block: memory.yaml
memory:
  secret_guard:
    enabled: true           # Master switch. When false, all checks are no-ops.
    on_secret: "block"      # block | redact | warn
    rules: "all"            # "all" | list of rule_ids
    exclude_rules: []       # rule_ids to skip
```

Defaults: `enabled: true`, `on_secret: "block"`, `rules: "all"`, `exclude_rules: []`.

Si el bloque `secret_guard` está ausente, `enabled` es `true` (secure by default). Para deshabilitar, el operador debe explícitamente setear `enabled: false`.

#### Events

| Event | Subject | When |
|-------|---------|------|
| `MemorySecretBlocked` | `agent.memory.secret.blocked` | Write blocked by secret detection |
| `MemorySecretRedacted` | `agent.memory.secret.redacted` | Content redacted before write |
| `MemorySecretWarned` | `agent.memory.secret.warned` | Secret found but write allowed (warn mode) |

Payload: `{ memory_id?, rule_ids: Vec<String>, content_hash: String, source: String }`. `source` identifica la ruta: `"remember"`, `"extract_memories"`, `"dreaming"`, `"git_commit"`.

#### Reglas Regex

Port directo de `claude-code-leak/src/services/teamMemorySync/secretScanner.ts`.

| Rule ID | Label | Regex (simplified) |
|---------|-------|--------|
| `anthropic-api-key` | Anthropic API Key | `\bsk-ant-api03-[a-zA-Z0-9_\-]{93}AA\b` |
| `anthropic-admin-api-key` | Anthropic Admin API Key | `\bsk-ant-admin01-[a-zA-Z0-9_\-]{93}AA\b` |
| `openai-api-key` | OpenAI API Key | `\bsk-(?:proj\|svcacct\|admin)-[A-Za-z0-9_-]+T3BlbkFJ[A-Za-z0-9_-]+\b` |
| `aws-access-token` | AWS Access Token | `\b(?:A3T[A-Z0-9]\|AKIA\|ASIA\|ABIA\|ACCA)[A-Z2-7]{16}\b` |
| `gcp-api-key` | GCP API Key | `\bAIza[\w-]{35}\b` |
| `azure-ad-client-secret` | Azure AD Client Secret | `[a-zA-Z0-9_~.]{3}\dQ~[a-zA-Z0-9_~.-]{31,34}` |
| `digitalocean-pat` | DigitalOcean PAT | `\bdop_v1_[a-f0-9]{64}\b` |
| `digitalocean-access-token` | DigitalOcean Access Token | `\bdoo_v1_[a-f0-9]{64}\b` |
| `github-pat` | GitHub PAT | `\bghp_[0-9a-zA-Z]{36}\b` |
| `github-fine-grained-pat` | GitHub Fine Grained PAT | `\bgithub_pat_\w{82}\b` |
| `github-app-token` | GitHub App Token | `\b(?:ghu\|ghs)_[0-9a-zA-Z]{36}\b` |
| `github-oauth` | GitHub OAuth | `\bgho_[0-9a-zA-Z]{36}\b` |
| `github-refresh-token` | GitHub Refresh Token | `\bghr_[0-9a-zA-Z]{36}\b` |
| `gitlab-pat` | GitLab PAT | `\bglpat-[\w-]{20}\b` |
| `gitlab-deploy-token` | GitLab Deploy Token | `\bgldt-[0-9a-zA-Z_\-]{20}\b` |
| `slack-bot-token` | Slack Bot Token | `\bxoxb-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*\b` |
| `slack-user-token` | Slack User Token | `\bxox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}\b` |
| `slack-app-token` | Slack App Token | `\bxapp-\d-[A-Z0-9]+-\d+-[a-z0-9]+\b` |
| `twilio-api-key` | Twilio API Key | `\bSK[0-9a-fA-F]{32}\b` |
| `sendgrid-api-token` | SendGrid API Token | `\bSG\.[a-zA-Z0-9=_\-.]{66}\b` |
| `npm-access-token` | NPM Access Token | `\bnpm_[a-zA-Z0-9]{36}\b` |
| `pypi-upload-token` | PyPI Upload Token | `\bpypi-AgEIcHlwaS5vcmc[\w-]{50,1000}\b` |
| `databricks-api-token` | Databricks API Token | `\bdapi[a-f0-9]{32}(?:-\d)?\b` |
| `hashicorp-tf-api-token` | HashiCorp TF API Token | `[a-zA-Z0-9]{14}\.atlasv1\.[a-zA-Z0-9\-_=]{60,70}` |
| `pulumi-api-token` | Pulumi API Token | `\bpul-[a-f0-9]{40}\b` |
| `postman-api-token` | Postman API Token | `\bPMAK-[a-fA-F0-9]{24}-[a-fA-F0-9]{34}\b` |
| `grafana-api-key` | Grafana API Key | `\beyJrIjoi[A-Za-z0-9+/]{70,400}={0,3}\b` |
| `grafana-cloud-api-token` | Grafana Cloud API Token | `\bglc_[A-Za-z0-9+/]{32,400}={0,3}\b` |
| `grafana-service-account-token` | Grafana Service Account Token | `\bglsa_[A-Za-z0-9]{32}_[A-Fa-f0-9]{8}\b` |
| `sentry-user-token` | Sentry User Token | `\bsntryu_[a-f0-9]{64}\b` |
| `sentry-org-token` | Sentry Org Token | `\bsntrys_eyJpYXQiO[a-zA-Z0-9+/]{10,200}(?:LCJyZWdpb25fdXJs\|InJlZ2lvbl91cmwi\|cmVnaW9uX3VybCI6)[a-zA-Z0-9+/]{10,200}={0,2}_[a-zA-Z0-9+/]{43}` |
| `stripe-access-token` | Stripe Access Token | `\b(?:sk\|rk)_(?:test\|live\|prod)_[a-zA-Z0-9]{10,99}\b` |
| `shopify-access-token` | Shopify Access Token | `\bshpat_[a-fA-F0-9]{32}\b` |
| `shopify-shared-secret` | Shopify Shared Secret | `\bshpss_[a-fA-F0-9]{32}\b` |
| `huggingface-access-token` | HuggingFace Access Token | `\bhf_[a-zA-Z]{34}\b` |
| `private-key` | Private Key | `(?i)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----[\s\S]{64,}?-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----` |

Total: 34 reglas. La regla Anthropic ensambla el prefijo `sk-ant-api` en runtime para evitar el literal en el binario.

### Casos de uso

1. **Happy path — sin secrets:** `remember_typed("agent-1", "user likes dark mode", &[], Some(User), &guard)` → `guard.check()` retorna `Ok(content)` intacto → INSERT procede.

2. **Block — Anthropic API key en ExtractMemories:** LLM genera memoria con `sk-ant-api03-...` → `guard.check()` retorna `Err(SecretBlockedError { labels: "Anthropic API Key", .. })` → el archivo NO se escribe, se emite `MemorySecretBlocked`.

3. **Redact — GitHub PAT en remember:** `remember_typed("agent-1", "token: ghp_abc123...", &[], None, &guard)` → `guard.check()` retorna `Ok("token: [REDACTED:github-pat]")` → INSERT con contenido redactado, se emite `MemorySecretRedacted`.

4. **Warn — AWS key en dreaming candidate:** Dreaming promueve memoria que contiene `AKIA...` → `guard.check()` retorna `Ok(content)` intacto → se escribe a MEMORY.md, se emite `MemorySecretWarned` + log warning.

5. **Múltiples secrets:** Contenido con Anthropic key + GitHub PAT → error lista ambos labels: `"Anthropic API Key, GitHub PAT"`.

6. **Falso positivo:** `exclude_rules: ["github-pat"]` → el patrón `github-pat` se omite del scanner → contenido con `ghp_...` pasa limpio.

7. **Disabled:** `enabled: false` → `SecretGuard::check()` es no-op, retorna `Ok(content)` siempre.

8. **Path traversal hardening:** `resolve_memory_path("../../../etc/passwd")` → rechazado. `resolve_memory_path("foo/%2e%2e%2f/bar")` → rechazado (URL-encoded traversal). Path con null byte → rechazado. Unicode fullwidth dots → rechazado.

9. **Symlink escape:** `resolve_memory_path("memory/legit.md")` donde `legit.md` es symlink a `/etc/passwd` → `canonicalize()` revela path real fuera de `memory_dir` → rechazado.

10. **Git commit con secret:** `workspace_git::commit_all()` escanea el diff staged → encuentra `sk-ant-api03-...` en `memory/some-file.md` → commit bloqueado, evento `MemorySecretBlocked { source: "git_commit" }`.

### Dependencias
- Crates nuevos: `regex` (ya en workspace), `sha2` (para `content_hash`)
- Crates del workspace: `nexo-core` (eventos), `nexo-memory` (scanner + integración), `nexo-driver-loop` (integración ExtractMemories)
- No nuevos deps externos

### Decisiones de diseño
- **`regex` crate** — las reglas usan features estándar, `regex` es la más rápida y ya está en el workspace.
- **`once_cell::sync::Lazy` para reglas compiladas** — compilar en cada `new()` es barato pero innecesario. `Lazy` compila una vez.
- **`SecretGuard` separado de `SecretScanner`** — scanner es puro (detecta), guard es política (decide). Permite testear scanner sin mockear políticas.
- **`OnSecret::Block` por default** — secure by default. Si el bloque `secret_guard` está ausente, `enabled` es `true` y `on_secret` es `block`.
- **`content_hash` (SHA-256) en eventos, nunca el secret value** — permite correlacionar incidentes sin exponer el secreto.
- **No allowlist por archivo** — Claude Code no lo tiene. Si un patrón da false positive, se excluye la regla completa via `exclude_rules`.
- **Redact reemplaza el match completo con `[REDACTED:rule_id]`** — simple, trazable.
- **Path sandbox en ExtractMemories ya existe, solo reforzar** — `resolve_memory_path()` ya rechaza `..` y paths absolutos. Agregar Unicode normalization defense, URL-encoded traversal, null byte, y symlink check.
