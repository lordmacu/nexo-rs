#!/usr/bin/env bash
# Phase 81.19.b locale follow-up item 2 — verify the operator-facing
# `agent-creator-microapp/frontend/src/data/locales.ts`
# `SUPPORTED_LOCALES` array is a SUBSET of the Rust
# `nexo_microapp_sdk::locale::Locale::from_str` accept-list.
#
# Curated TS list ≤ permissive Rust accept-list (Rust is cross-product
# of all LangCode × RegionCode plus language-only; TS is a hand-picked
# subset shipped to operators). Exit 1 on any TS code Rust would reject.
#
# Mirrors OpenClaw's `research/src/i18n/registry.test.ts:24-39`
# drift-lint pattern but cross-language (Rust ↔ TS).
#
# Run from the proyecto repo root.

set -euo pipefail

cd "$(dirname "$0")/.."

TS_FILE="../agent-creator-microapp/frontend/src/data/locales.ts"
if [[ ! -f "$TS_FILE" ]]; then
    echo "::warning::Microapp frontend not present at $TS_FILE — skipping lint" >&2
    exit 0
fi

# 1. Dump Rust accept-list to a sorted set.
mapfile -t RUST_LIST < <(
    cargo run -q -p nexo-microapp-sdk --bin locale_dump 2>/dev/null \
        | python3 -c "import json,sys;d=json.loads(sys.stdin.read());print('\n'.join(d['supported']))"
)
if [[ "${#RUST_LIST[@]}" -eq 0 ]]; then
    echo "❌ Rust locale dump returned empty — check 'cargo run -p nexo-microapp-sdk --bin locale_dump'" >&2
    exit 1
fi

# 2. Extract every `code: "X-YY"` (or `code: "X"`) literal from the TS file.
mapfile -t TS_LIST < <(
    grep -oE 'code:\s*"[^"]+"' "$TS_FILE" \
        | sed -E 's/^code:\s*"([^"]+)"$/\1/' \
        | sort -u
)
if [[ "${#TS_LIST[@]}" -eq 0 ]]; then
    echo "❌ TS locale list returned empty — check $TS_FILE format ('code: \"...\"' literal expected)" >&2
    exit 1
fi

# 3. Subset check: every TS code must appear in the Rust accept-list.
RUST_SET=$(printf '%s\n' "${RUST_LIST[@]}" | sort -u)
MISSING=()
for code in "${TS_LIST[@]}"; do
    if ! grep -Fxq "$code" <<<"$RUST_SET"; then
        MISSING+=("$code")
    fi
done

if [[ "${#MISSING[@]}" -ne 0 ]]; then
    echo "❌ TS SUPPORTED_LOCALES has codes the Rust parser rejects:" >&2
    printf '   - %s\n' "${MISSING[@]}" >&2
    echo >&2
    echo "Add the missing LangCode / RegionCode variant in" >&2
    echo "  proyecto/crates/microapp-sdk/src/locale.rs" >&2
    echo "or correct the typo in" >&2
    echo "  $TS_FILE" >&2
    exit 1
fi

echo "✅ Locale lists in sync — TS (${#TS_LIST[@]} curated) ⊆ Rust (${#RUST_LIST[@]} accept-list)."
