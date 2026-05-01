# Per-extension state directory (Phase 82.6)

Extensions need a stable place to put SQLite databases, vault
files, and per-tenant artifacts. Phase 82.6 formalises the
convention and ships a CLI helper so authors and operators
agree on the path layout.

## Canonical path

```text
$NEXO_HOME/extensions/<extension-id>/state/
```

`NEXO_HOME` falls back to `$HOME/.nexo` when unset, then to
the current working directory if even `$HOME` is missing
(rare; covers minimal CI containers).

For an extension `agent-creator` on a typical install:

```text
~/.nexo/extensions/agent-creator/state/
```

## CLI

```bash
# Print the path (no filesystem touch).
nexo ext state-dir agent-creator
# /home/operator/.nexo/extensions/agent-creator/state

# Create the directory if missing (idempotent).
nexo ext state-dir agent-creator --ensure
```

Operators pipe the output into `cd`, `sqlite3 .backup`, etc.
The base form is pure path resolution — useful in scripts that
want to compute paths without side effects. `--ensure` is the
moral equivalent of `mkdir -p`.

## Programmatic access

`nexo-extensions` exposes:

```rust
use nexo_extensions::{ensure_state_dir, state_dir_for};

// Compute the path without touching disk.
let path = state_dir_for("agent-creator");

// Materialise it (idempotent).
let path = ensure_state_dir("agent-creator")?;
```

The daemon calls `ensure_state_dir` at extension first spawn so
microapps can rely on the directory existing by the time their
`initialize` handshake runs. The path is also exposed via the
`NEXO_EXTENSION_STATE_ROOT` env var injected into the
extension's process environment (constant
`EXTENSION_STATE_ROOT_ENV` in the same module).

## Backup procedure

The state dir is a regular filesystem location — operators
back it up with the same tooling they use for other on-disk
state:

```bash
# Whole-extension snapshot.
tar czf agent-creator-state-$(date +%F).tgz \
    -C "$(nexo ext state-dir agent-creator)" .

# SQLite-aware online backup (preferred for live DBs).
sqlite3 "$(nexo ext state-dir agent-creator)/db.sqlite" \
    ".backup '/var/backups/agent-creator-$(date +%F).db'"
```

## Isolation

Each extension owns its own subtree. `nexo` does not enforce
namespacing inside `state/` — that's the extension's
responsibility. v1 microapps that store per-tenant artifacts
typically sub-divide as `state/tenants/<tenant-id>/…`. The
framework treats the whole subtree as opaque.
