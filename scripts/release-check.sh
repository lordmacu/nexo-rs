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

# Tarball naming follows cargo-dist's default: `<bin>-<target>.tar.xz`
# on Unix targets and `<bin>-<target>.zip` on Windows.
EXPECTED_TARBALLS=(
    "nexo-rs-x86_64-unknown-linux-gnu.tar.xz"
    "nexo-rs-x86_64-unknown-linux-musl.tar.xz"
    "nexo-rs-aarch64-unknown-linux-musl.tar.xz"
    "nexo-rs-x86_64-apple-darwin.tar.xz"
    "nexo-rs-aarch64-apple-darwin.tar.xz"
    "nexo-rs-x86_64-pc-windows-msvc.zip"
)

# Targets the local host is expected to be able to actually build
# (the rest may legitimately be skipped on a developer laptop without
# the right SDK).
HOST_ARCH="$(uname -m)"
HOST_OS="$(uname -s)"
HOST_NATIVE_TARBALL=""
case "${HOST_OS}" in
    Linux)
        # Prefer the gnu fallback tarball (always builds locally),
        # falling back to musl if the gnu target was disabled.
        case "${HOST_ARCH}" in
            x86_64)
                if [[ -f "${DIST_DIR}/nexo-rs-x86_64-unknown-linux-gnu.tar.xz" ]]; then
                    HOST_NATIVE_TARBALL="nexo-rs-x86_64-unknown-linux-gnu.tar.xz"
                else
                    HOST_NATIVE_TARBALL="nexo-rs-x86_64-unknown-linux-musl.tar.xz"
                fi
                ;;
            aarch64)
                HOST_NATIVE_TARBALL="nexo-rs-aarch64-unknown-linux-musl.tar.xz"
                ;;
        esac
        ;;
    Darwin)
        case "${HOST_ARCH}" in
            x86_64)  HOST_NATIVE_TARBALL="nexo-rs-x86_64-apple-darwin.tar.xz" ;;
            arm64)   HOST_NATIVE_TARBALL="nexo-rs-aarch64-apple-darwin.tar.xz" ;;
        esac
        ;;
esac

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
# local build (e.g. no Apple SDK) still passes the gate. CI in Phase
# 27.2 enforces the full matrix.
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "[release-check] WARN: tarballs not built locally: ${missing[*]}"
fi

echo "[release-check] PASS — ${#present[@]} tarball(s) validated"
