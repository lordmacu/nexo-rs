# Security policy

## Reporting a vulnerability

**Do not** file public GitHub issues for security-sensitive bugs.

Email **informacion@cristiangarcia.co** with:

- A description of the issue and its impact
- Steps to reproduce (proof-of-concept if possible)
- Affected version / commit SHA
- Any suggested mitigation

Expected response:

- Acknowledgment: **within 72 hours**
- Initial assessment: **within 7 days**
- Coordinated disclosure timeline agreed with the reporter before
  any public advisory

If the report is accepted, the reporter will be credited in the
subsequent security advisory (unless they ask to stay anonymous).

## Supported versions

nexo-rs has **not** yet committed to a semver-stable public API.
Only the tip of `main` is supported; security fixes land there and
are not backported.

Once a `v1.0` tag ships this policy will be revised.

## Scope — in

- `crates/*` code shipped from this repository
- The `agent` binary and its default configuration
- CI workflows, the bootstrap script, and the pre-commit hook
- Published documentation when it misleads users into unsafe
  configurations

## Scope — out

- Third-party upstreams (NATS, SQLite, MiniMax / Anthropic / OpenAI /
  Gemini APIs, WhatsApp's Signal Protocol stack, Google APIs, etc.);
  report those upstream
- User-supplied configuration that disables built-in guardrails
  (`outbound_allowlist: []`, `allowed_tools: []`, etc.) — these are
  documented knobs, not vulnerabilities
- Vulnerabilities in extensions that are not shipped in this
  repository

## Safe-harbor

Research done in good faith against:

- A local `nexo-rs` instance you own
- A test account under your control

is explicitly authorized. Do **not** test against third-party
accounts or services without their owner's consent.

## Hardening references

- [Fault tolerance](https://lordmacu.github.io/nexo-rs/architecture/fault-tolerance.html)
- [Per-agent credentials](https://lordmacu.github.io/nexo-rs/config/credentials.html)
- [Skills gating](https://lordmacu.github.io/nexo-rs/skills/gating.html)
- [NATS with TLS + auth](https://lordmacu.github.io/nexo-rs/recipes/nats-tls-auth.html)
