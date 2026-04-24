# GitHub Extension (Rust)

Standalone Rust stdio extension that talks to the GitHub REST API directly via
`reqwest`. No `gh` CLI dependency, no Node, no MCP.

## Tools

- `status` — extension info + authenticated user (if token present)
- `pr_list` — list pull requests (`repo`, optional `state` open|closed|all, `limit` 1–100)
- `pr_view` — get one pull request (`repo`, `number`)
- `pr_checks` — CI check runs for the PR head commit (two-step: PR → check-runs)
- `issue_list` — list issues, filtering out PRs (`repo`, optional `state`, `limit`)

## Auth

- `GITHUB_TOKEN` — required PAT (fine-grained or classic) with `repo` and `read:org` scopes
- `GITHUB_DEFAULT_REPO` — optional `owner/repo` fallback when tools omit `repo`

## Reliability

- `reqwest` blocking, rustls TLS
- Connect timeout 5s, total request timeout 15s (override via `GITHUB_HTTP_TIMEOUT_SECS`)
- Retry: 3 attempts, backoff 500ms / 1s / 2s, only on 5xx and timeouts
- Circuit breaker: 5 fails / 30s open window
- Typed errors: `Unauthorized` (-32011), `Forbidden` (-32012), `RateLimited` with reset epoch (-32013), `NotFound` (-32001)

## Environment

- `GITHUB_TOKEN` (required for any non-`status` call)
- `GITHUB_API_URL` (default `https://api.github.com`)
- `GITHUB_DEFAULT_REPO` (e.g. `lordmacu/agent-rs`)
- `GITHUB_HTTP_TIMEOUT_SECS` (default `15`)

## Build & test

```bash
cargo build --release --manifest-path extensions/github/Cargo.toml
cargo test           --manifest-path extensions/github/Cargo.toml
```

13 tests: 4 unit (breaker) + 9 integration (`wiremock`).

## Smoke

```bash
GITHUB_TOKEN=ghp_xxx GITHUB_DEFAULT_REPO=lordmacu/agent-rs \
  echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"pr_list","arguments":{"limit":5}}}' \
  | ./target/release/github
```
