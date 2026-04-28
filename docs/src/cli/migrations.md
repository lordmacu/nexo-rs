# Migrations CLI

Versioned YAML schema migrations are now available for operator config
files under `config/`.

## Commands

- `nexo setup migrate --dry-run` (default behavior) — reports pending
  file migrations and target schema version without writing files.
- `nexo setup migrate --apply` — applies pending migrations in place.

Each migrated file carries a top-level `schema_version` marker. The
loader tolerates this metadata field and strips it before strict typed
deserialization.

## Boot and hot-reload behavior

- `runtime.yaml` accepts:

```yaml
migrations:
  auto_apply: true
```

- `auto_apply: true` makes boot + Phase 18 hot-reload apply pending
  config schema migrations before loading the runtime snapshot.
- `auto_apply: false` (default) leaves files untouched and prints a
  pending-migrations warning with file/version pairs.

## Notes

- The migration functions are idempotent and versioned.
- `setup migrate --apply` is the safest path for explicit review-driven
  upgrades in production environments.

See also:
- [CLI reference](./reference.md)
- [Backup + restore](../ops/backup.md)
