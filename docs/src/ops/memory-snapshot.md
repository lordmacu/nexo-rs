# Agent memory snapshots

Atomic point-in-time snapshots of an agent's full memory state, packaged
as a single verifiable bundle. Built for rollback after a corrupt dream,
forensic audit ("what did the agent know at T?"), portable export
between hosts, and pre-restore safety nets in autonomous mode.

## What goes in a bundle

| Layer | Source | In-bundle path |
|---|---|---|
| Memory git repo | `<memdir>/.git/` | `git/**` |
| Operator-curated files | `<memdir>/MEMORY.md` + topic files | `memory_files/**` |
| Long-term SQLite | `<sqlite>/long_term.sqlite` | `sqlite/long_term.sqlite` |
| Vector SQLite | `<sqlite>/vector.sqlite` | `sqlite/vector.sqlite` |
| Concepts | `<sqlite>/concepts.sqlite` | `sqlite/concepts.sqlite` |
| Compactions | `<sqlite>/compactions.sqlite` | `sqlite/compactions.sqlite` |
| Extractor cursor | runtime state provider | `state/extract_cursor.json` |
| Last dream run row | agent registry | `state/dream_run.json` |
| Manifest | seal | `manifest.json` |

## Bundle layout on disk

```
<state_root>/tenants/<tenant>/snapshots/<agent_id>/
├── <id>.tar.zst           # bundle body (or .tar.zst.age when encrypted)
└── <id>.tar.zst.sha256    # whole-file SHA-256 sibling
```

Two independent integrity checks ride together:

- **Manifest seal** — `manifest.bundle_sha256` = SHA-256 of every
  per-artifact hex digest concatenated in declared order. Verifiable
  from the manifest alone, no recursion on the tar bytes.
- **File-level seal** — sibling `.sha256` text file = SHA-256 of the
  bundle file as it lives on disk (post-encryption when encrypted).
  Detects bit-flips during transit / cold storage even when the body
  is age-wrapped.

Both must pass for `verify` to report `ok`.

## CLI

```
nexo memory snapshot --agent <id> [--tenant <t>] [--label <s>]
                     [--redact-secrets] [--encrypt age:<recipient>]

nexo memory restore  --agent <id> [--tenant <t>] --from <bundle>
                     [--dry-run] [--no-auto-pre-snapshot]
                     [--decrypt-identity <path>]

nexo memory list     --agent <id> [--tenant <t>] [--json]
nexo memory diff     --agent <id> [--tenant <t>] <id-a> <id-b>
nexo memory export   --agent <id> [--tenant <t>] --id <snapshot-id> --to <path>
nexo memory verify   --bundle <path>
nexo memory delete   --agent <id> [--tenant <t>] --id <snapshot-id>
```

`--tenant` defaults to `default` for single-tenant deployments. Multi-
tenant SaaS deployments require explicit values aligned with the
canonicalized identifier rules described in
[capabilities](./capabilities.md).

`nexo memory restore` is gated on `NEXO_MEMORY_RESTORE_ALLOW=true` (see
[capabilities](./capabilities.md)). Without the flag the subcommand
refuses, even with `--yes`.

## Configuration

Lives in `config/memory.yaml` under `memory.snapshot`:

```yaml
memory:
  snapshot:
    enabled: true
    root: ${NEXO_HOME}/state
    auto_pre_dream: false              # opt-in safety net before autoDream
    auto_pre_restore: true             # always snapshot before restore
    auto_pre_mutating_tool: false      # opt-in: pre-Plan-mode mutating tool
    lock_timeout_secs: 60
    redact_secrets_default: true
    encryption:
      enabled: false
      recipients: []                   # age public keys (age1...)
      identity_path: ${NEXO_HOME}/secret/snapshot-identity.txt
    retention:
      keep_count: 30
      max_age_days: 90
      gc_interval_secs: 3600
    events:
      mutation_subject_prefix: "nexo.memory.mutated"
      lifecycle_subject_prefix: "nexo.memory.snapshot"
      mutation_publish_enabled: true
```

Hot-reload via the standard `ConfigReloadCoordinator` path: edit YAML
and the retention worker picks up the new policy at the next tick.

## Lifecycle events (NATS)

Best-effort published when a broker is wired:

| Subject | Trigger | Payload |
|---|---|---|
| `nexo.memory.snapshot.<agent_id>.created` | snapshot success | `SnapshotMeta` |
| `nexo.memory.snapshot.<agent_id>.restored` | restore success | `RestoreReport` |
| `nexo.memory.snapshot.<agent_id>.deleted` | delete success | `{snapshot_id, ts_ms}` |
| `nexo.memory.snapshot._all.gc` | retention sweep | `{deleted, kept, ts_ms}` |

Mutation events (one per memory write) flow to
`nexo.memory.mutated.<agent_id>` when
`memory.snapshot.events.mutation_publish_enabled = true`. Subscribers
can stream them into an audit log without forking memory writes.

## Encryption

Optional, behind the `snapshot-encryption` Cargo feature:

```bash
cargo build --features snapshot-encryption
nexo memory snapshot --agent ana --encrypt age:age1xyz...
nexo memory restore --agent ana --from <bundle>.tar.zst.age \
                    --decrypt-identity ~/.nexo/secret/snapshot-identity.txt
```

The body is wrapped in an `age` stream; the manifest stays plaintext
inside the encrypted payload but the per-artifact hashes commit to it.
The sibling `.sha256` file always covers the bytes that land on disk
(post-encryption), so transit integrity stays verifiable without the
identity.

## Threat model

- **Loss of identity** → encrypted bundle is unrecoverable. Mirror
  identity files into your operator-credential store with the same
  retention as your other long-lived secrets.
- **Sibling `.sha256` missing** → `verify` reports
  `bundle_sha256_ok = false` but does not error. Operators must treat
  this as a hard fail before restore.
- **Bundle smaller than the live state** → expected: restore overwrites
  whatever was there, including untracked files in the memdir. Use
  `--dry-run` first.
- **Cross-tenant restore** → blocked at path validation. A bundle
  whose tenant string does not match the request errors with
  `CrossTenantError` before any disk mutation.
- **Last snapshot deletion** → `delete` refuses to drop the agent's
  only remaining bundle. Retention sweeps obey the same floor.
- **Auto-pre-snapshot during restore** → on by default. Disable with
  `--no-auto-pre-snapshot` only when the rollback anchor is unwanted
  (e.g. you are restoring into a fresh agent with no prior state).
- **Encrypted bundles + `verify`** → without the identity the
  per-artifact hashes inside the body cannot be checked; the report's
  `manifest_ok` and `per_artifact_ok` are reported as `true` by
  convention while `age_protected` is set. Operators who must verify
  the manifest of an encrypted bundle should run `verify` after a
  decrypt + restore round-trip.

## Retention

A background worker sweeps every `gc_interval_secs`:

1. **Orphan staging cleanup** — any `.staging-<id>/` or
   `.restore-staging-<id>/` directory left behind by a process kill
   is deleted at startup and at every tick.
2. **Per-agent count + age** — bundles older than `max_age_days` or
   exceeding `keep_count` are deleted oldest-first via the same
   `delete()` path the CLI uses, so the "never delete the last
   snapshot" floor is respected.

## Restore mechanics

The full sequence for a real (non-`--dry-run`) restore:

1. `verify` the bundle. Schema-too-new and checksum mismatch fail
   here without touching live state.
2. `auto_pre_snapshot` (default on): take a snapshot labelled
   `auto:pre-restore-<orig_id>` so the operation is reversible.
3. Acquire the per-agent lock. Concurrent snapshot/restore for the
   same agent will fail with `Concurrent`.
4. Unpack to `.restore-staging-<uuid>/`.
5. Tag the live HEAD with `pre-restore-<id>` so prior state stays
   reachable via `git reflog show pre-restore-<id>`.
6. SQLite swap: each live DB is renamed to `<name>.sqlite.pre-restore.bak`
   and the staging copy moves into place. The `.bak` files survive
   the restore for manual recovery.
7. Memdir replace: live memdir is renamed to
   `<memdir>-pre-restore-<id>/` and the staging contents are written
   on top. Failures roll the rename back.
8. State provider replay: extractor cursor + last dream-run row.
9. Drop staging dir + lock.

## See also

- [Backup + restore](./backup.md) — operator backup script (Phase 36.1)
- [Memdir scanner](./memdir-scanner.md) — secret-guard configuration
- [Capabilities](./capabilities.md) — `NEXO_MEMORY_RESTORE_ALLOW`
