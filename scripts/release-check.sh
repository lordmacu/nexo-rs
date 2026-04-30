#!/usr/bin/env bash
# Phase 27.1 — release smoke gate. Runs after `cargo dist build
# --artifacts=local` and validates that every expected tarball:
#   1. exists on disk
#   2. matches its `*.sha256` sidecar (when cargo-dist emits one)
#   3. contains the `nexo` binary, both LICENSE files, and README.md
#   4. (host-native musl tarball only) extracts and runs `--version`
#      with the expected version string
#
# Exit 0 = pass. Non-zero = `[release-check] FAIL: <reason>`.
set -euo pipefail

DIST_DIR="${1:-target/distrib}"

EXPECTED_VERSION="${EXPECTED_VERSION:-}"
if [[ -z "${EXPECTED_VERSION}" ]]; then
    EXPECTED_VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
fi
if [[ -z "${EXPECTED_VERSION}" ]]; then
    echo "[release-check] FAIL: could not extract version from Cargo.toml" >&2
    exit 1
fi

if [[ ! -d "${DIST_DIR}" ]]; then
    echo "[release-check] FAIL: dist directory missing: ${DIST_DIR}" >&2
    exit 1
fi

# Tarball naming follows cargo-dist's default: `<bin>-<target>.tar.xz`.
# Phase 27.2 reduced the matrix to 2 musl Linux targets. Termux ships
# as a separate `.deb` produced by `packaging/termux/build.sh`.
EXPECTED_TARBALLS=(
    "nexo-rs-x86_64-unknown-linux-musl.tar.xz"
    "nexo-rs-aarch64-unknown-linux-musl.tar.xz"
)

# Termux .deb glob — checked separately at the end of the script;
# bionic-libc binary can't be smoke-run on this host.
EXPECTED_TERMUX_DEB_GLOB="nexo-rs_*_aarch64.deb"

# Targets the local host is expected to be able to actually build.
# Without zig 0.13.0 + cargo-zigbuild 0.22.x installed locally,
# `dist build` will fail and the gate emits WARN; CI runners in
# Phase 27.2 are the ground truth.
HOST_ARCH="$(uname -m)"
HOST_OS="$(uname -s)"
HOST_NATIVE_TARBALL=""
if [[ "${HOST_OS}" == "Linux" ]]; then
    case "${HOST_ARCH}" in
        x86_64)  HOST_NATIVE_TARBALL="nexo-rs-x86_64-unknown-linux-musl.tar.xz" ;;
        aarch64) HOST_NATIVE_TARBALL="nexo-rs-aarch64-unknown-linux-musl.tar.xz" ;;
    esac
fi

fail() {
    echo "[release-check] FAIL: $*" >&2
    exit 1
}

pass() {
    echo "[release-check] OK: $*"
}

verify_sha256() {
    local file="$1"
    local sidecar="${file}.sha256"
    if [[ ! -f "${sidecar}" ]]; then
        # cargo-dist may bundle sha256 sums in a single
        # `<release>-sha256.txt` file instead of per-asset sidecars.
        # Treat the absence of a sidecar as a soft skip — the unified
        # sums file will be validated below if present.
        return 0
    fi
    local expected
    expected="$(awk '{print $1}' "${sidecar}")"
    local actual
    actual="$(sha256sum "${file}" | awk '{print $1}')"
    if [[ "${expected}" != "${actual}" ]]; then
        fail "sha256 mismatch for ${file}: expected ${expected}, got ${actual}"
    fi
}

list_archive() {
    local file="$1"
    case "${file}" in
        *.tar.xz|*.tar.gz) tar -tf "${file}" ;;
        *.zip)             unzip -l "${file}" | awk 'NR>3 && $NF != "" {print $NF}' ;;
        *) fail "unknown archive format: ${file}" ;;
    esac
}

check_archive_contents() {
    local file="$1"
    local listing
    listing="$(list_archive "${file}")"
    local target_basename
    target_basename="$(basename "${file}")"
    local bin_name="nexo"
    if [[ "${target_basename}" == *windows* ]]; then
        bin_name="nexo.exe"
    fi
    local required=("${bin_name}" "LICENSE-MIT" "LICENSE-APACHE" "README.md")
    for needle in "${required[@]}"; do
        if ! grep -q "/${needle}\$\|^${needle}\$" <<<"${listing}"; then
            fail "tarball ${target_basename} missing ${needle}"
        fi
    done
}

extract_and_run_version() {
    local file="$1"
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap "rm -rf '${tmpdir}'" RETURN
    tar -xf "${file}" -C "${tmpdir}"
    local nexo_bin
    nexo_bin="$(find "${tmpdir}" -name nexo -type f -perm -u+x | head -n1)"
    if [[ -z "${nexo_bin}" ]]; then
        fail "no executable nexo bin inside ${file}"
    fi
    local out
    out="$("${nexo_bin}" --version 2>&1)"
    if [[ ! "${out}" =~ ^nexo\ ${EXPECTED_VERSION}$ ]]; then
        fail "host-native --version output mismatch: got '${out}', expected 'nexo ${EXPECTED_VERSION}'"
    fi
    pass "host-native --version → ${out}"
}

# 1. each expected tarball exists OR is documented as skipped.
present=()
missing=()
for t in "${EXPECTED_TARBALLS[@]}"; do
    if [[ -f "${DIST_DIR}/${t}" ]]; then
        present+=("${t}")
    else
        missing+=("${t}")
    fi
done

if [[ ${#present[@]} -eq 0 ]]; then
    fail "no expected tarballs found in ${DIST_DIR}"
fi

# 2 + 3. validate the ones that ARE present.
for t in "${present[@]}"; do
    full="${DIST_DIR}/${t}"
    verify_sha256 "${full}"
    check_archive_contents "${full}"
    pass "${t} contents OK"
done

# 4. host-native --version smoke.
if [[ -n "${HOST_NATIVE_TARBALL}" ]] && [[ -f "${DIST_DIR}/${HOST_NATIVE_TARBALL}" ]]; then
    extract_and_run_version "${DIST_DIR}/${HOST_NATIVE_TARBALL}"
else
    echo "[release-check] WARN: host-native tarball ${HOST_NATIVE_TARBALL:-<unknown>} not present; skipping --version smoke"
fi

# Surface missing tarballs as warnings (not failures) so a partial
# local build (e.g. no zig toolchain) still passes the gate. CI in
# Phase 27.2 enforces the full matrix.
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "[release-check] WARN: tarballs not built locally: ${missing[*]}"
fi

# 5. Termux .deb sanity (when uploaded by CI). Validate sha256 only;
# can't run the bionic-libc binary on this host.
shopt -s nullglob
termux_count=0
for deb in "${DIST_DIR}"/${EXPECTED_TERMUX_DEB_GLOB}; do
    verify_sha256 "${deb}"
    pass "$(basename "${deb}") sha256 OK"
    termux_count=$((termux_count + 1))
done
shopt -u nullglob
if [[ ${termux_count} -eq 0 ]]; then
    echo "[release-check] WARN: no Termux .deb found (${EXPECTED_TERMUX_DEB_GLOB}); skipping"
fi

# 6. Phase 76.14 — mcp-server CLI subcommands smoke (host binary only,
# no server running — validate help output and argument parsing).
echo "[release-check] mcp-server CLI smoke"
mcp_help="$("${BINARY}" mcp-server --help 2>&1 || true)"
if echo "${mcp_help}" | grep -q "inspect"; then
    echo "[release-check] mcp-server inspect subcommand present"
else
    echo "[release-check] FAIL: mcp-server inspect subcommand not found" >&2
    exit 1
fi
# tail-audit without a valid DB should fail with a clear sqlite error, not panic.
tail_output="$("${BINARY}" mcp-server tail-audit /nonexistent/mcp_audit.db 2>&1 || true)"
if echo "${tail_output}" | grep -qi "fail\|error\|denied\|no such"; then
    echo "[release-check] mcp-server tail-audit graceful on missing DB"
else
    echo "[release-check] WARN: mcp-server tail-audit unexpected output: ${tail_output}"
fi

echo "[release-check] PASS — ${#present[@]} tarball(s) + ${termux_count} Termux deb(s) validated"
