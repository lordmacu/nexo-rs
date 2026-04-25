#!/usr/bin/env bash
# Mock `claude` CLI for tests. Behaviour:
#   * `MOCK_FIXTURE=<path>`      → cat the file to stdout, exit 0.
#   * `MOCK_SLEEP_BEFORE=<secs>` → sleep N seconds before any output.
#   * `MOCK_SLEEP_FOREVER=1`     → sleep 60 (long-living, for kill tests).
#   * `MOCK_NOISY_STDERR=1`      → emit a few warnings to stderr first.
#
# We deliberately ignore `claude`'s real flags here — the tests care
# about subprocess plumbing, not about the CLI's argument grammar.

set -u

if [[ "${MOCK_NOISY_STDERR:-}" == "1" ]]; then
    echo "warn: mock noise 1" >&2
    echo "warn: mock noise 2" >&2
fi

if [[ "${MOCK_SLEEP_FOREVER:-}" == "1" ]]; then
    sleep 60
    exit 0
fi

if [[ -n "${MOCK_SLEEP_BEFORE:-}" ]]; then
    sleep "$MOCK_SLEEP_BEFORE"
fi

if [[ -n "${MOCK_FIXTURE:-}" && -f "$MOCK_FIXTURE" ]]; then
    cat "$MOCK_FIXTURE"
    exit 0
fi

# Nothing to do — exit non-zero so tests detect a misconfigured mock.
echo "mock-claude.sh: no MOCK_FIXTURE set" >&2
exit 2
