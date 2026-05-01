#!/usr/bin/env bash
# Phase 82.2.b.b — manual smoke check of the Tier A publish gates.
#
# Runs build + test + missing-doc gate per crate. Exits non-zero
# if any of the four (now five with `nexo-tool-meta`) crates
# fails. Intended as a local pre-commit aid until the GitHub
# Actions workflow lands in 82.2.b.c.

set -euo pipefail

TIER_A=(
  "nexo-tool-meta"
  "nexo-resilience"
  "nexo-driver-permission"
  "nexo-webhook-receiver"
  "nexo-webhook-server"
)

echo "[tier-a] checking ${#TIER_A[@]} crates..."

for crate in "${TIER_A[@]}"; do
  echo "[tier-a] [$crate] cargo build"
  cargo build -p "$crate" --quiet
  echo "[tier-a] [$crate] cargo test"
  cargo test -p "$crate" --quiet
  echo "[tier-a] [$crate] cargo doc (deny warnings)"
  RUSTDOCFLAGS="-D warnings" cargo doc -p "$crate" --no-deps --quiet
done

echo "[tier-a] all gates green for ${#TIER_A[@]} crates"
