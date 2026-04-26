#!/usr/bin/env bash
# Hot backup for a running Nexo deployment.
#
# Snapshots every SQLite DB in NEXO_HOME/ via `sqlite3 .backup`
# (online, no pause, captures a consistent snapshot even with
# concurrent writers), then tars the lot up with a sha256 manifest.
#
# Usage:
#   nexo-backup.sh                     # to ./dist/nexo-backup-<timestamp>.tar.zst
#   nexo-backup.sh --out /backups/     # custom dir
#   nexo-backup.sh --include-secrets   # also include secrets/ (off by default)
#
# Bridge until the proper `nexo backup` CLI subcommand (Phase 36)
# lands. Ships under `scripts/` so cron / systemd can call it
# without depending on the binary being on PATH at the cron user's
# context.

set -euo pipefail

NEXO_HOME="${NEXO_HOME:-${HOME}/.nexo}"
OUT_DIR="."
INCLUDE_SECRETS=0

while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT_DIR="$2"; shift 2;;
        --include-secrets) INCLUDE_SECRETS=1; shift;;
        --help|-h)
            sed -n '2,15p' "$0"
            exit 0;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

[ -d "$NEXO_HOME" ] || {
    echo "ERROR: NEXO_HOME ($NEXO_HOME) does not exist." >&2
    echo "       Set NEXO_HOME explicitly or run as the nexo user." >&2
    exit 3
}
command -v sqlite3 >/dev/null || {
    echo "ERROR: sqlite3 missing. Install with 'apt install sqlite3' or equivalent." >&2
    exit 4
}
command -v zstd >/dev/null || {
    echo "ERROR: zstd missing. Install with 'apt install zstd'." >&2
    exit 5
}

TIMESTAMP=$(date -u +%Y%m%dT%H%M%SZ)
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STAGE="$WORK/nexo-backup-${TIMESTAMP}"
mkdir -p "$STAGE"

echo "==> staging from $NEXO_HOME → $STAGE"

# ---------------------------------------------------------------------
# 1. Hot-snapshot every SQLite DB. `sqlite3 .backup` is the official
#    online-backup mechanism — captures a consistent point-in-time
#    image even with concurrent writers. Faster than dumping SQL and
#    re-importing, preserves the binary format exactly.
# ---------------------------------------------------------------------
mapfile -d '' DB_FILES < <(find "$NEXO_HOME" \
    -type f \
    \( -name '*.db' -o -name '*.sqlite' -o -name '*.sqlite3' \) \
    -print0)

for src in "${DB_FILES[@]}"; do
    rel="${src#$NEXO_HOME/}"
    dst="$STAGE/$rel"
    mkdir -p "$(dirname "$dst")"
    echo "  • snapshot $rel"
    sqlite3 "$src" ".backup '$dst'"
done

# ---------------------------------------------------------------------
# 2. Copy non-DB state. JSONL transcripts, FTS index files (if any
#    live outside the DB), the `secret/` subtree (only when
#    --include-secrets), and the agent workspace git dir if the
#    operator opted into Phase 10.9 git-backed memory.
# ---------------------------------------------------------------------
echo "  • rsync non-DB state"
rsync -a \
    --exclude='*.db' \
    --exclude='*.sqlite' \
    --exclude='*.sqlite3' \
    --exclude='*.tmp' \
    --exclude='*.lock' \
    --exclude='queue/' \
    "$NEXO_HOME/" "$STAGE/"

if [ $INCLUDE_SECRETS -eq 0 ]; then
    rm -rf "$STAGE/secret"
    echo "  • dropped secret/ (re-run with --include-secrets to include)"
fi

# ---------------------------------------------------------------------
# 3. Manifest with sha256 per file so restore can verify integrity.
# ---------------------------------------------------------------------
echo "  • building manifest"
(cd "$STAGE" && find . -type f -print0 | xargs -0 sha256sum) \
    > "$STAGE/MANIFEST.sha256"

# ---------------------------------------------------------------------
# 4. Tar + zstd.
# ---------------------------------------------------------------------
mkdir -p "$OUT_DIR"
ARCHIVE="$OUT_DIR/nexo-backup-${TIMESTAMP}.tar.zst"

echo "==> writing $ARCHIVE"
tar -C "$WORK" -cf - "nexo-backup-${TIMESTAMP}" \
    | zstd -19 -T0 -q -o "$ARCHIVE"

# Final integrity hash for the archive itself.
sha256sum "$ARCHIVE" > "${ARCHIVE}.sha256"

echo "==> done"
echo "    archive: $ARCHIVE"
echo "    size:    $(du -h "$ARCHIVE" | cut -f1)"
echo "    sha256:  $(awk '{print $1}' "${ARCHIVE}.sha256")"

# Restore reminder for operators who pipe this to a backup mailer.
cat <<MSG

To restore:
  zstd -dc $ARCHIVE | tar -xf -
  # validate manifest:
  cd nexo-backup-${TIMESTAMP}
  sha256sum -c MANIFEST.sha256
  # then rsync into NEXO_HOME with the daemon stopped:
  systemctl stop nexo-rs
  rsync -a --delete ./ $NEXO_HOME/
  systemctl start nexo-rs

MSG
