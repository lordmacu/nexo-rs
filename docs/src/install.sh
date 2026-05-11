#!/usr/bin/env bash
# nexo-rs installer — served at https://lordmacu.github.io/nexo-rs/install.sh
#
# Usage:
#   curl -fsSL https://lordmacu.github.io/nexo-rs/install.sh | bash
#
# What it does today:
#   - Verifies the Rust toolchain is on PATH (prints rustup hint if not).
#   - Runs `cargo install --git https://github.com/lordmacu/nexo-rs nexo-rs`
#     to build + install the daemon binary into ~/.cargo/bin/.
#
# Why no pre-built binary path yet:
#   The Phase 27.2 multi-platform release pipeline (.deb / .rpm /
#   .tar.gz / MSI / Termux .deb) is wired but every release tag to
#   date (rc1/rc2/rc3) has zero attached assets. Once the first GA
#   tag ships its assets, this script will switch to a binary-first
#   path with cargo install as fallback. Until then `cargo install
#   --git` is the only path that actually produces a working binary.
#   Tracked at https://github.com/lordmacu/nexo-rs/blob/main/FOLLOWUPS.md
#
# Exit codes:
#   0 — install succeeded
#   1 — cargo not on PATH (with rustup install hint printed)
#   2 — cargo install failed (cargo's own error already printed)

set -eu

REPO_URL="https://github.com/lordmacu/nexo-rs"

print_banner() {
    cat <<'EOF'
─────────────────────────────────────────────────────────────
  nexo-rs installer
  https://lordmacu.github.io/nexo-rs/
─────────────────────────────────────────────────────────────
EOF
}

require_cargo() {
    if ! command -v cargo >/dev/null 2>&1; then
        cat >&2 <<'EOF'
error: `cargo` not found on PATH.

The Phase 27.2 pre-built binary pipeline isn't shipping assets
yet, so installing nexo-rs requires the Rust toolchain. Install
it with:

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

Then re-run this script. (Or skip rustup and use Docker:
`docker pull ghcr.io/lordmacu/nexo-rs:latest`.)
EOF
        exit 1
    fi
    echo "✓ cargo found: $(cargo --version)"
}

run_cargo_install() {
    echo "→ Running: cargo install --git ${REPO_URL} nexo-rs"
    echo "  (this compiles ~600 crates; coffee break time on a slow box)"
    if ! cargo install --git "${REPO_URL}" nexo-rs; then
        cat >&2 <<EOF
error: cargo install failed.

If the failure is about missing system deps (libssl, pkg-config,
clang, etc.), see the platform-specific notes at
${REPO_URL}#building-from-source

If the failure is about a Rust version, bump rustup:
    rustup update stable
EOF
        exit 2
    fi
}

print_next_steps() {
    cat <<EOF

✓ nexo installed at \$(which nexo)

Next:
  1. Boot the daemon (Phase 92-95: zero config required):
       nexo

  2. Install a persona pack (one-line ready-to-run agents):
       nexo persona install lordmacu/nexo-persona-cody
       nexo persona install lordmacu/nexo-persona-ana-template
       nexo persona install lordmacu/nexo-persona-marketing-multiclient-template

  3. (Optional) Scaffold documented sample YAMLs:
       nexo init

Docs: https://lordmacu.github.io/nexo-rs/
EOF
}

main() {
    print_banner
    require_cargo
    run_cargo_install
    print_next_steps
}

main "\$@"
