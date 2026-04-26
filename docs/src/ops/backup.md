# Backup + restore

Nexo state lives under `NEXO_HOME` (default `~/.nexo/` for native
installs, `/var/lib/nexo-rs/` for the systemd package, `/app/data/`
in the Docker image). Backing it up + restoring it is the operator's
responsibility today; a proper `nexo backup` / `nexo restore`
subcommand is tracked under [Phase 36](#status).

## Quickest path — `scripts/nexo-backup.sh`

The repo ships a shell script that does the right thing without
stopping the daemon:

```bash
# Single-shot, output to ./
NEXO_HOME=/var/lib/nexo-rs sudo -E scripts/nexo-backup.sh

# Custom output dir, exclude secrets (default)
scripts/nexo-backup.sh --out /backups/

# Include secrets/ for full recovery (encrypt the archive yourself)
scripts/nexo-backup.sh --include-secrets
```

What it does:

1. **Hot snapshot every SQLite DB** via `sqlite3 .backup` — the
   official online-backup mechanism. Captures a consistent
   point-in-time image even with concurrent writers; no daemon
   stop required.
2. **rsync non-DB state** — JSONL transcripts, the agent
   workspace-git dir if Phase 10.9 is enabled, any operator
   files dropped under `NEXO_HOME`. Skips `*.tmp`, `*.lock`,
   and the `queue/` disk-queue dir (replays on next boot from
   NATS, no need to back up).
3. **`secret/` excluded by default.** Re-run with
   `--include-secrets` to include them; **encrypt the resulting
   tarball before transit** (use `age`, `gpg`, or push to an
   encrypted bucket).
4. **sha256 manifest** at `MANIFEST.sha256` inside the archive
   so restore can verify integrity.
5. **zstd-19 compression** — typical 10× ratio over raw SQLite.
6. **Sidecar `<archive>.sha256`** with the archive's outer hash
   so backup pipelines can detect transit corruption.

## Restore

```bash
# Pull the archive locally first
scp ops@host:/backups/nexo-backup-20260426T121500Z.tar.zst .

# Extract
zstd -dc nexo-backup-20260426T121500Z.tar.zst | tar -xf -

# Verify the manifest
cd nexo-backup-20260426T121500Z
sha256sum -c MANIFEST.sha256

# Stop the daemon (state must not be mid-write)
sudo systemctl stop nexo-rs

# Replace state
sudo rsync -a --delete --chown=nexo:nexo \
  ./ /var/lib/nexo-rs/

# Start
sudo systemctl start nexo-rs
sudo journalctl -u nexo-rs -f
```

The daemon **must** be stopped during the rsync — SQLite WAL
files do not survive a parallel-write replacement.

## Cron schedule

Drop in `/etc/cron.daily/nexo-backup`:

```bash
#!/bin/sh
set -eu
ARCHIVE_DIR=/backups/nexo
mkdir -p "$ARCHIVE_DIR"

# Snapshot, retain locally
NEXO_HOME=/var/lib/nexo-rs \
    /opt/nexo-rs/scripts/nexo-backup.sh --out "$ARCHIVE_DIR"

# Push to remote (Backblaze, S3, Wasabi, etc.)
rclone copy --include '*.tar.zst*' "$ARCHIVE_DIR" remote:nexo-backups/

# Retain 30 days locally + 90 days remote
find "$ARCHIVE_DIR" -name 'nexo-backup-*.tar.zst*' -mtime +30 -delete
rclone delete --min-age 90d remote:nexo-backups/
```

`chmod +x /etc/cron.daily/nexo-backup`. Single-host operators get
a tested daily backup pipeline in 6 lines.

## What survives a backup

| Component | In backup | Notes |
|---|---|---|
| Long-term memory (vector + relational) | ✅ | `memory.db` |
| Transcripts | ✅ | `transcripts/` JSONL + `transcripts.db` FTS |
| TaskFlow state | ✅ | `taskflow.db` |
| Pairing store + setup-code key | ⚠️ | DB included; key only with `--include-secrets` |
| LLM credentials | ⚠️ | `secret/` only with `--include-secrets` |
| Per-agent SOUL.md + MEMORY.md | ✅ | rsync from workspace |
| Agent workspace git | ✅ | full `.git` dir included if Phase 10.9 is on |
| Disk-queue (NATS replay buffer) | ❌ | regenerates from NATS on boot |
| Process logs | ❌ | journalctl handles those separately |

## Migrations

Schema migrations across Nexo versions are still ad-hoc — `ALTER
TABLE … .ok()` patterns inside the runtime. Phase 36 adds:

- `nexo migrate status` — show the applied vs available migration set
- `nexo migrate up [target]` — apply pending migrations forward
- `nexo migrate down [target]` — roll back if a release ships
  reversible migrations
- A `migrations/` dir with versioned, checksummed SQL files

Until then, pin to a specific Nexo version per deployment and
test upgrades on a copy of the backup before applying to
production.

## Status

Tracked as [Phase 36 — Backup, restore, migrations](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/PHASES.md#phase-36).

| Sub-phase | Status |
|---|---|
| `scripts/nexo-backup.sh` shell bridge | ✅ shipped |
| Operator doc (this page) | ✅ shipped |
| `nexo backup --out <dir>` subcommand | ⬜ deferred |
| `nexo restore --from <archive>` subcommand | ⬜ deferred |
| `nexo migrate up/down/status` versioned migrations | ⬜ deferred |
| Encrypted archive output (age / gpg) | ⬜ deferred |
| CI test that backup → restore round-trips on a fixture | ⬜ deferred |

The shell script + this doc are the bridge. Once the runtime
subcommands ship, this page rewrites to point at them and the
script gets retired.
