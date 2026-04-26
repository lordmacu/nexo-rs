# Privacy toolkit

GDPR-style operator workflows for handling user data requests until
the proper `nexo forget` / `nexo export-user` subcommands ship
(tracked under [Phase 50](#status)).

## Right to be forgotten

`scripts/nexo-forget-user.sh` does cascading delete across every
SQLite DB and JSONL transcript under `NEXO_HOME`, then `VACUUM`s
the databases so the deleted rows don't survive in free pages.

```bash
# Stop the daemon first — SQLite WAL doesn't survive parallel writes
sudo systemctl stop nexo-rs

# DRY RUN — shows what would be deleted, doesn't change anything
NEXO_HOME=/var/lib/nexo-rs sudo -E scripts/nexo-forget-user.sh \
  --id "+5491155556666"

# When the dry-run looks right, re-run with --apply
NEXO_HOME=/var/lib/nexo-rs sudo -E scripts/nexo-forget-user.sh \
  --id "+5491155556666" \
  --apply

# Restart
sudo systemctl start nexo-rs
```

What gets deleted (cascading across all DBs):

| Table column | Match | Source DB |
|---|---|---|
| `user_id` | exact | every DB |
| `sender_id` | exact | every DB (used in pairing, transcripts) |
| `account_id` | exact | every DB (used in WA / TG plugins) |
| `contact_id` | exact | memory + transcripts |
| `peer_id` | exact | agent-to-agent routing |

Plus JSONL transcript lines where any of those keys equals the
target id.

The script emits `forget-user-<id>-<timestamp>.json` with the
exact deletion counts — this is the operator's **GDPR audit
trail**, ship it back to the requester as proof of compliance.

### `--keep-audit` flag

Strict GDPR says even the admin-audit row recording the deletion
should be removed (the user has the right to no trace). But that
breaks operator audit chains. Use `--keep-audit` to opt out of
that single specific erasure:

```bash
nexo-forget-user.sh --id "<id>" --apply --keep-audit
```

The script keeps the `admin_audit` table row showing **that** the
deletion happened (without the user-id field, which is hashed).
Other tables fully wiped either way.

## Right to data export

Until `nexo export-user --id <id>` ships, manual SQL works:

```bash
USER_ID="+5491155556666"
OUT_DIR="export-${USER_ID}-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$OUT_DIR"

# Stop the daemon for a consistent point-in-time export
sudo systemctl stop nexo-rs

# Per-DB extraction
for db in /var/lib/nexo-rs/*.db; do
    name=$(basename "$db" .db)
    sqlite3 "$db" \
        ".headers on" \
        ".mode json" \
        ".output $OUT_DIR/${name}.json" \
        "SELECT * FROM ($(sqlite3 "$db" '
          SELECT GROUP_CONCAT(
            \"SELECT '\" || name || \"' AS table_name, * FROM \" || name ||
            \" WHERE user_id = '\" || ? || \"' OR sender_id = '\" || ? || \"' OR account_id = '\" || ? || \"'\",
            \" UNION ALL \"
          )
          FROM sqlite_master m
          WHERE m.type='table'
            AND EXISTS (
              SELECT 1 FROM pragma_table_info(m.name) p
              WHERE p.name IN ('user_id','sender_id','account_id')
            )
        '))" -- "$USER_ID" "$USER_ID" "$USER_ID"
done

# Per-JSONL extraction
for f in /var/lib/nexo-rs/transcripts/*.jsonl; do
    name=$(basename "$f")
    jq -c \
        --arg id "$USER_ID" \
        'select((.user_id // .sender_id // .account_id // "") == $id)' \
        "$f" > "$OUT_DIR/$name"
done

# Restart
sudo systemctl start nexo-rs

# Tar + zstd, optionally encrypt
tar -C "$(dirname "$OUT_DIR")" -cf - "$(basename "$OUT_DIR")" | \
    zstd -19 -T0 > "${OUT_DIR}.tar.zst"

# (Recommended) age-encrypt before transit
age -r age1xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx \
    -o "${OUT_DIR}.tar.zst.age" \
    "${OUT_DIR}.tar.zst"
shred -u "${OUT_DIR}.tar.zst"
```

The result is a tarball the operator hands to the requester —
JSON files per DB + filtered transcript JSONLs — encrypted with
the requester's `age` public key.

When `nexo export-user --id <id>` ships, this whole shell pipeline
collapses into one command with built-in encryption.

## Retention policy

Operator-defined per deployment. Recommended defaults:

| Surface | Retention | Why |
|---|---|---|
| Transcripts | 90 days | Enough for ops debugging + agent recall |
| Memory (long-term) | indefinite | Agent's working memory; pruned by recall signals |
| TaskFlow finished flows | 30 days | Audit trail for completed work |
| TaskFlow failed flows | 365 days | Forensics |
| Admin audit log | 365 days | Compliance |
| Disk-queue (NATS replay) | 7 days | Disaster recovery |
| Pairing pending requests | 60 min | TTL-enforced by the store |

Apply via cron (until `nexo retention apply` ships):

```bash
# /etc/cron.daily/nexo-retention
#!/bin/sh
set -eu
DB=/var/lib/nexo-rs/transcripts.db

# 90-day rolling window on transcripts
sqlite3 "$DB" "DELETE FROM transcripts
                WHERE timestamp < strftime('%s', 'now', '-90 days');"
sqlite3 "$DB" 'VACUUM;'

# Same for taskflow finished + failed
DB=/var/lib/nexo-rs/taskflow.db
sqlite3 "$DB" "DELETE FROM flows
                WHERE status='Finished'
                  AND finished_at < datetime('now', '-30 days');"
sqlite3 "$DB" "DELETE FROM flows
                WHERE status='Failed'
                  AND finished_at < datetime('now', '-365 days');"
```

## PII detection (deferred)

Phase 50 plans inbound PII flagging — separate from the existing
outbound redactor. The rough shape:

- Regex pre-screen for SSN-shape, credit-card-shape (Luhn-checked),
  phone-number-shape per locale.
- Optional LLM-backed second-pass via the future Phase 68 local
  tier (gemma3-270m).
- Hits land in `data/pii-flags.jsonl` for operator review;
  agent dialog continues unimpeded.

Today: nothing automated. The outbound redactor in
`crates/core/src/redaction.rs` (regex-based) catches the obvious
shapes before they reach long-term memory or the LLM, but doesn't
emit a queue for operator review.

## Encryption at rest

Two roads, both deferred to Phase 50.x:

- **Application-level** — `sqlcipher` build of `libsqlite3-sys` with
  a key fed from `secrets/`. Every page encrypted; backups need
  the same key to restore.
- **Filesystem-level** — `dm-crypt` / LUKS on the volume hosting
  `NEXO_HOME`. Operator does it once at provision, no Nexo
  changes required.

The native install + Hetzner / Fly recipes assume filesystem-level
crypto handled by the host (LUKS on Hetzner, encrypted EBS on AWS,
Fly volumes are encrypted at rest by default). When `sqlcipher` is
ready we'll document switching tiers.

## Status

| Capability | Status |
|---|---|
| `scripts/nexo-forget-user.sh` cascading delete | ✅ shipped |
| Operator data-export shell pipeline (above) | ✅ documented |
| Retention policy + cron template | ✅ documented |
| `nexo forget --user <id>` subcommand | ⬜ deferred |
| `nexo export-user --id <id>` subcommand | ⬜ deferred |
| Inbound PII detection + review queue | ⬜ deferred |
| `sqlcipher` encryption at rest | ⬜ deferred |
| Admin-action audit log (separate from this script's manifest) | ⬜ deferred |

Tracked as [Phase 50 — Privacy toolkit](https://github.com/lordmacu/nexo-rs/blob/main/proyecto/PHASES.md#phase-50).
