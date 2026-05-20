#!/usr/bin/env bash
# Local pre-push gate that replicates `.github/workflows/ci.yml` 1:1.
# Catches the issues CI catches (rustfmt drift, clippy --all-targets
# under -D warnings, build --locked, test under --features
# config-self-edit + --test-threads=1) without waiting for the runner.
#
# Usage:
#   scripts/ci-local.sh                # all gates, fail-fast
#   scripts/ci-local.sh fmt clippy     # subset (space-separated names)
#
# Gates (in CI execution order):
#   fmt        cargo fmt --all -- --check
#   locale     scripts/lint-locale-list-sync.sh
#   ts-types   scripts/lint-ts-types-sync.sh
#   clippy     cargo clippy --workspace --all-targets --no-deps -- -D warnings
#   build      cargo build --workspace --locked
#   test       cargo test --workspace --locked
#   test-cse   cargo test --workspace --locked --features config-self-edit -- --test-threads=1
#   template-plugin    standalone build of extensions/template-plugin-rust/
#   template-microapp  standalone build of extensions/template-microapp-rust/
#
# Notes:
# - Reuses the same RUSTFLAGS / CARGO_NET_RETRY env CI sets.
# - Sibling repo path-deps (nexo-rs-plugin-{telegram,whatsapp}) are
#   expected to exist as `../nexo-rs-plugin-{telegram,whatsapp}/`.
#   CI checks them out from GitHub; locally they're peers of proyecto.
# - `--locked` forces Cargo.lock fidelity — if you bumped a workspace
#   dep without committing the lock, this catches it.

set -uo pipefail

# Clear the git env vars a parent git operation (e.g. the pre-push
# hook fired by `git push`) exports into this process. Without this,
# the `test` gate's `cargo test --workspace` runs
# crates/driver-loop/tests/workspace_git_worktree_test.rs with an
# inherited GIT_DIR / GIT_INDEX_FILE pointing at the outer repo, and
# its `git worktree add` against a tempdir repo fails with
# `fatal: .git/index: index file open failed: Not a directory`.
# No-op under GitHub Actions (no inherited git env there).
unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_PREFIX \
      GIT_AUTHOR_DATE GIT_COMMITTER_DATE GIT_REFLOG_ACTION 2>/dev/null || true

# Default: run every gate. Override by passing gate names as args.
ALL_GATES=(fmt locale ts-types clippy build test test-cse template-plugin template-microapp)
GATES=("${@:-${ALL_GATES[@]}}")
# When invoked with args we use $@ literally; when none, expand ALL_GATES.
if [[ $# -gt 0 ]]; then
    GATES=("$@")
fi

export CARGO_TERM_COLOR=always
export RUSTFLAGS="-D warnings"
export CARGO_NET_RETRY=10
export CARGO_NET_TIMEOUT=60

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Sanity-check sibling repo path-deps exist. CI checks them out as
# fresh clones; local devs need them present alongside proyecto/.
for sibling in nexo-rs-plugin-telegram nexo-rs-plugin-whatsapp; do
    if [[ ! -d "../$sibling" ]]; then
        echo "[ci-local] WARN: sibling repo missing: ../$sibling" >&2
        echo "[ci-local]       CI checks it out from github.com/lordmacu/$sibling — clone it alongside proyecto/ for local parity." >&2
    fi
done

FAILED=()

run_gate() {
    local name="$1"
    shift
    local started elapsed rc
    echo
    echo "═══════════════════════════════════════════════════════════"
    echo "[ci-local] ▶ $name"
    echo "═══════════════════════════════════════════════════════════"
    started=$(date +%s)
    "$@"
    rc=$?
    elapsed=$(( $(date +%s) - started ))
    if [[ $rc -eq 0 ]]; then
        echo "[ci-local] ✅ $name (${elapsed}s)"
    else
        echo "[ci-local] ❌ $name (${elapsed}s, exit $rc)"
        FAILED+=("$name")
    fi
}

gate_fmt() {
    cargo fmt --all -- --check
}

gate_locale() {
    if [[ -x scripts/lint-locale-list-sync.sh ]]; then
        bash scripts/lint-locale-list-sync.sh
    else
        echo "[ci-local] (skip — scripts/lint-locale-list-sync.sh missing)"
    fi
}

gate_ts_types() {
    if [[ -x scripts/lint-ts-types-sync.sh ]]; then
        bash scripts/lint-ts-types-sync.sh
    else
        echo "[ci-local] (skip — scripts/lint-ts-types-sync.sh missing)"
    fi
}

gate_clippy() {
    cargo clippy --workspace --all-targets --no-deps -- -D warnings
}

gate_build() {
    cargo build --workspace --locked
}

gate_test() {
    cargo test --workspace --locked
}

gate_test_cse() {
    cargo test --workspace --locked --features config-self-edit -- --test-threads=1
}

gate_template_plugin() {
    local tmp
    tmp="$(mktemp -d)"
    trap "rm -rf '$tmp'" RETURN
    cp -r extensions/template-plugin-rust/. "$tmp/"
    if [[ -f "$tmp/_Cargo.toml" ]]; then
        mv "$tmp/_Cargo.toml" "$tmp/Cargo.toml"
    fi
    ( cd "$tmp" && cargo build )
}

gate_template_microapp() {
    local tmp
    tmp="$(mktemp -d)"
    trap "rm -rf '$tmp'" RETURN
    cp -r extensions/template-microapp-rust/. "$tmp/"
    ( cd "$tmp" && cargo build )
}

for g in "${GATES[@]}"; do
    case "$g" in
        fmt)               run_gate "fmt"               gate_fmt ;;
        locale)            run_gate "locale-sync"       gate_locale ;;
        ts-types)          run_gate "ts-types-sync"     gate_ts_types ;;
        clippy)            run_gate "clippy"            gate_clippy ;;
        build)             run_gate "build"             gate_build ;;
        test)              run_gate "test"              gate_test ;;
        test-cse)          run_gate "test-cse"          gate_test_cse ;;
        template-plugin)   run_gate "template-plugin"   gate_template_plugin ;;
        template-microapp) run_gate "template-microapp" gate_template_microapp ;;
        *)
            echo "[ci-local] ERROR: unknown gate '$g'" >&2
            echo "[ci-local]        valid: ${ALL_GATES[*]}" >&2
            exit 64
            ;;
    esac
done

echo
echo "═══════════════════════════════════════════════════════════"
if [[ ${#FAILED[@]} -eq 0 ]]; then
    echo "[ci-local] ✅ ALL GREEN (${#GATES[@]} gates)"
    exit 0
else
    echo "[ci-local] ❌ FAILED: ${FAILED[*]}"
    exit 1
fi
