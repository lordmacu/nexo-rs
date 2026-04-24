# ADR 0009 — Dual MIT / Apache-2.0 licensing

**Status:** Accepted
**Date:** 2026-04

## Context

Open-sourcing nexo-rs required picking a license. Constraints:

- The Rust ecosystem convention (rustc, tokio, serde, clap, axum…)
  is dual MIT / Apache-2.0
- Downstream projects should be able to pick whichever license fits
  their own project's obligations
- Attribution to the original author must be legally enforceable —
  the author explicitly asked that users "use it, just name me"
- The author doesn't want to ship a custom / restrictive license
  that confuses or scares off contributors

Alternatives considered:

- **MIT alone** — fine, but missing the explicit patent grant that
  Apache-2 gives (relevant to corporate downstream users)
- **Apache-2 alone** — fine, but incompatible with GPLv2 downstream
  (MIT is compatible)
- **AGPL-3** — forces source-release on SaaS; nexo-rs isn't trying
  to prevent cloud forks
- **BSL (Business Source License)** — source-available with
  time-delayed open-source conversion; inappropriate for a framework
  whose value is in wide adoption
- **Custom "use it, name me"** — would need a lawyer for every edge
  case; a solved problem doesn't need a new solution

## Decision

**Dual-license under `MIT OR Apache-2.0`**:

- `LICENSE-MIT` — full text of the MIT License, 2026 Cristian García
- `LICENSE-APACHE` — full text of the Apache-2.0 License
- `Cargo.toml`: `license = "MIT OR Apache-2.0"` (SPDX)
- `NOTICE` file at repo root (required to be preserved by
  Apache-2.0 §4(d)) carries the attribution — author, contact,
  original repo URL
- README links all three + explains the SPDX choice

Downstream users pick whichever they prefer. Attribution is
mandatory under both.

## Consequences

**Positive**

- Fits existing Rust ecosystem tooling (crates.io, rustdoc headers,
  CI scanners)
- Maximum compatibility: GPLv2 projects pick MIT, patent-sensitive
  corporate projects pick Apache-2
- `NOTICE` file gives the author the strongest attribution lever
  available in permissive OSS: removing it is a license violation

**Negative**

- Contributors who want to submit PRs agree (per Apache-2 §5) that
  their contributions are dual-licensed under the same terms. Some
  contributors may require a CLA discussion; none so far
- Trademark on the name "nexo-rs" is **not** covered — this ADR is
  about the code, not the brand. If the brand becomes load-bearing,
  register a trademark separately

## Related

- [License](../license.md) — human-facing version of this decision
- [NOTICE](https://github.com/lordmacu/nexo-rs/blob/main/NOTICE) —
  enforceable attribution block
