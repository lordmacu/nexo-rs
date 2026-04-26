#!/usr/bin/env bash
# Cascading per-user delete bridge for GDPR right-to-be-forgotten
# requests. Bridge until the proper `nexo forget --user <id>`
# subcommand (Phase 50) lands.
#
# Walks every SQLite DB in NEXO_HOME and removes rows that reference
# the target user identifier. JSONL transcripts are filtered per-line
# in place. Produces a manifest of what was deleted so the operator
# has an audit trail to send back to the requester.
#
# CRITICAL: stop the daemon before running. SQLite WAL files don't
# survive parallel writes during the delete pass.
#
# Usage:
#   nexo-forget-user.sh --id <user_id>                 # dry-run
#   nexo-forget-user.sh --id <user_id> --apply         # actually delete
#   nexo-forget-user.sh --id <user_id> --apply --keep-audit
#       (preserves the admin-audit row that records THIS deletion;
#        otherwise it gets nuked too, which is what GDPR requires
#        but breaks operator audit chains)

set -euo pipefail

NEXO_HOME="${NEXO_HOME:-${HOME}/.nexo}"
USER_ID=""
APPLY=0
KEEP_AUDIT=0
MANIFEST_DIR="."

while [ $# -gt 0 ]; do
    case "$1" in
        --id) USER_ID="$2"; shift 2;;
        --apply) APPLY=1; shift;;
        --keep-audit) KEEP_AUDIT=1; shift;;
        --manifest-dir) MANIFEST_DIR="$2"; shift 2;;
        --help|-h)
            sed -n '2,22p' "$0"; exit 0;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

[ -n "$USER_ID" ] || { echo "ERROR: --id <user_id> required" >&2; exit 2; }
[ -d "$NEXO_HOME" ] || { echo "ERROR: NEXO_HOME ($NEXO_HOME) missing" >&2; exit 3; }
command -v sqlite3 >/dev/null || { echo "ERROR: sqlite3 missing" >&2; exit 4; }
command -v jq >/dev/null      || { echo "ERROR: jq missing" >&2; exit 4; }

# Refuse to run against a live daemon.
if pgrep -x nexo >/dev/null 2>&1; then
    echo "ERROR: nexo daemon is running. Stop it before running this script." >&2
    echo "       Suggested: sudo systemctl stop nexo-rs" >&2
    exit 5
fi

TIMESTAMP=$(date -u +%Y%m%dT%H%M%SZ)
MANIFEST="$MANIFEST_DIR/forget-user-${USER_ID}-${TIMESTAMP}.json"
mkdir -p "$MANIFEST_DIR"

if [ $APPLY -eq 0 ]; then
    echo "==> DRY RUN — re-run with --apply to actually delete"
fi

# --------------------------------------------------------------------
# Phase 1: SQLite. Walk every DB, run DELETE statements per known
# user-keyed table. Schemas may evolve; the script targets the column
# names that exist as of the version in this commit.
# --------------------------------------------------------------------

declare -A COUNTS=()
ALL_DBS=$(find "$NEXO_HOME" -type f \
    \( -name '*.db' -o -name '*.sqlite' -o -name '*.sqlite3' \) | sort)

run_count() {
    local db="$1"
    local sql="$2"
    sqlite3 "$db" "$sql" 2>/dev/null || echo 0
}

run_delete() {
    local db="$1"
    local sql="$2"
    [ $APPLY -eq 0 ] && { echo "  would: $sql" >&2; return; }
    sqlite3 "$db" "$sql" 2>/dev/null || true
}

for db in $ALL_DBS; do
    rel="${db#$NEXO_HOME/}"
    echo "==> $rel"

    # Discover tables that have a user-keyed column. Common names: user_id, sender_id, account_id.
    tables=$(sqlite3 "$db" "
        SELECT m.name FROM sqlite_master m
        WHERE m.type='table'
        AND EXISTS (
            SELECT 1 FROM pragma_table_info(m.name) p
            WHERE p.name IN ('user_id','sender_id','account_id','contact_id','peer_id')
        );
    " 2>/dev/null || true)

    for tbl in $tables; do
        # Pick the first matching column.
        col=$(sqlite3 "$db" "
            SELECT name FROM pragma_table_info('$tbl')
            WHERE name IN ('user_id','sender_id','account_id','contact_id','peer_id')
            LIMIT 1;
        " 2>/dev/null)
        [ -z "$col" ] && continue

        # Skip the audit table if --keep-audit and table looks like one.
        if [ $KEEP_AUDIT -eq 1 ] && [ "$tbl" = "admin_audit" ]; then
            echo "  • $tbl.$col [skipped — --keep-audit]"
            continue
        fi

        n=$(run_count "$db" "SELECT COUNT(*) FROM $tbl WHERE $col = '$USER_ID';")
        echo "  • $tbl.$col → $n row(s)"
        COUNTS["$rel/$tbl.$col"]="$n"

        if [ "$n" -gt 0 ]; then
            run_delete "$db" "DELETE FROM $tbl WHERE $col = '$USER_ID';"
        fi
    done

    # VACUUM to reclaim space + scrub free pages so the deleted rows
    # are unrecoverable from on-disk leftovers.
    if [ $APPLY -eq 1 ]; then
        sqlite3 "$db" 'VACUUM;' 2>/dev/null || true
    fi
done

# --------------------------------------------------------------------
# Phase 2: JSONL transcripts. Walk every *.jsonl under NEXO_HOME and
# filter out lines that mention the user id in the canonical fields.
# --------------------------------------------------------------------

JSONL_FILES=$(find "$NEXO_HOME" -type f -name '*.jsonl' 2>/dev/null | sort || true)

for f in $JSONL_FILES; do
    rel="${f#$NEXO_HOME/}"
    before=$(wc -l < "$f")
    after=$(jq -c \
        --arg id "$USER_ID" \
        'select((.user_id // .sender_id // .account_id // "") != $id)' \
        "$f" 2>/dev/null | wc -l || echo "$before")
    removed=$((before - after))
    echo "==> $rel"
    echo "  • $removed line(s) referenced $USER_ID"
    COUNTS["$rel"]="$removed"

    if [ $APPLY -eq 1 ] && [ "$removed" -gt 0 ]; then
        tmp=$(mktemp)
        jq -c \
            --arg id "$USER_ID" \
            'select((.user_id // .sender_id // .account_id // "") != $id)' \
            "$f" > "$tmp"
        mv "$tmp" "$f"
    fi
done

# --------------------------------------------------------------------
# Phase 3: emit manifest.
# --------------------------------------------------------------------

{
    echo "{"
    echo "  \"user_id\": \"$USER_ID\","
    echo "  \"timestamp\": \"$TIMESTAMP\","
    echo "  \"applied\": $([ $APPLY -eq 1 ] && echo true || echo false),"
    echo "  \"deletions\": {"
    first=1
    for k in "${!COUNTS[@]}"; do
        [ $first -eq 1 ] || echo ","
        printf '    "%s": %s' "$k" "${COUNTS[$k]}"
        first=0
    done
    echo
    echo "  }"
    echo "}"
} > "$MANIFEST"

echo
echo "==> manifest: $MANIFEST"
[ $APPLY -eq 0 ] && echo "==> DRY RUN — nothing changed. Re-run with --apply to delete."
