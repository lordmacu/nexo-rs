# 83.8.12.6.runtime + 83.8.12.6.b — SkillLoader fallback + on-disk migration

## Problema

Phase 83.8.12.6 cambió la layout del FsSkillsStore (admin RPC) a
`<root>/{__global__,<tenant_id>}/<name>/SKILL.md`. El runtime
`SkillLoader` (`crates/core/src/agent/skills.rs:191`) sigue
leyendo `<root>/<name>/SKILL.md` (legacy). Resultado:

- Skills creados pre-83.8.12.6 (en `<root>/<name>/`) ya no son
  visibles cuando admin RPC los escribe en `<root>/__global__/<name>/`.
- Tenant-scoped skills (escritos en `<root>/<tenant_id>/<name>/`)
  son completamente ignorados por el loader.

## Refs

- **OpenClaw `research/src/agents/skills.env-path-guidance.test.ts`** —
  patrón "skill resolution chain" donde el lookup intenta varios
  paths en orden de specificity hasta encontrar match. Misma idea
  acá: tenant → global → legacy.
- **`claude-code-leak/`** — sin precedente directo (CLI lookups
  son single-tenant). Absence declarada.

## Diseño

### SkillLoader fallback chain

```rust
pub struct SkillLoader {
    root: PathBuf,
    overrides: BTreeMap<String, SkillDepsMode>,
    tenant_id: Option<String>,  // NEW
}

impl SkillLoader {
    pub fn with_tenant_id(mut self, tenant_id: Option<String>) -> Self { ... }
}
```

Dentro de `load_one(name)`, resolver el path en orden:

1. Si `tenant_id.is_some()`: `<root>/<tenant_id>/<name>/SKILL.md`
2. `<root>/__global__/<name>/SKILL.md`
3. Legacy `<root>/<name>/SKILL.md` (log deprecation warn cuando
   se usa esta rama)

Devuelve la primera que existe.

### `llm_behavior.rs` wire

```rust
SkillLoader::new(skills_dir)
    .with_overrides(ctx.config.skill_overrides.clone())
    .with_tenant_id(ctx.config.tenant_id.clone())
```

### Migración on-disk (83.8.12.6.b)

Helper en `nexo-setup`:

```rust
pub fn migrate_legacy_skills_to_global(root: &Path) -> Result<usize, io::Error>;
```

Algoritmo:
- Si `<root>/<name>/SKILL.md` exists AND `<name>` != `__global__`:
  → move `<root>/<name>/` to `<root>/__global__/<name>/`
- Else (no SKILL.md at top level): es tenant scope, leave alone

Idempotente: si ya está migrado, returns 0. Atomic-ish: usa
`fs::rename`.

CLI sub-command `nexo skills migrate` queda fuera del scope
inmediato (operator runs el helper desde una ops console o boot
hook si quiere).

## Tests

1. SkillLoader sin tenant_id encuentra `<root>/__global__/<name>/SKILL.md`.
2. SkillLoader con tenant_id encuentra `<root>/<tid>/<name>/SKILL.md`
   (precedence sobre global).
3. SkillLoader con tenant_id cae a global cuando tenant scope no
   tiene el skill.
4. SkillLoader cae a legacy `<root>/<name>/SKILL.md` cuando ni
   tenant ni global lo tienen.
5. SkillLoader missing skill → `NotFound` en status.
6. `migrate_legacy_skills_to_global` mueve `<root>/foo/SKILL.md`
   → `<root>/__global__/foo/SKILL.md`, devuelve `1`.
7. Idempotencia: 2da llamada devuelve `0`.
8. No toca tenant-scoped (dirs sin SKILL.md al nivel root).

## Pasos atómicos

1. SkillLoader fallback + `with_tenant_id` + 5 tests + wire en llm_behavior.
2. Migration helper en nexo-setup + 3 tests.
3. Docs + FOLLOWUPS close-out.
