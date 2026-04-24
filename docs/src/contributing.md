# Contributing

PRs welcome. A few ground rules keep the codebase coherent.

## Workflow

All feature work follows the `/forge` pipeline:

```
/forge brainstorm <topic>  →  /forge spec <topic>  →  /forge plan <topic>  →  /forge ejecutar <topic>
```

Per-sub-phase done criteria live in
[`PHASES.md`](https://github.com/lordmacu/nexo-rs/blob/main/PHASES.md).

## Rules of the road

- **All code and code comments in English.** User-facing prose can be
  Spanish or English depending on context.
- **No hardcoded secrets.** Use `${ENV_VAR}` or `${file:...}` in YAML.
- **Every external call goes through `CircuitBreaker`.** No exceptions.
- **Don't commit anything under `secrets/`.**
- **Don't skip hooks** (`--no-verify`). Fix the underlying lint / test
  issue instead.

## Docs must follow

Any change that touches user-visible behavior — features, config
fields, CLI flags, tool surfaces, retry policies — must update the
mdBook under `docs/` in the **same commit**. Docs phase plan:
[`docs/PHASES.md`](https://github.com/lordmacu/nexo-rs/blob/main/docs/PHASES.md).

Pure-internal changes (private renames, refactors, test-only) are
exempt — mention that explicitly in the commit body.

## Local checks

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
mdbook build docs
```

CI runs all of the above on every push and every PR.

## Git pre-commit hook

The repo ships a pre-commit hook at `.githooks/pre-commit` that:

1. **Docs-sync gate** — rejects the commit if production files under
   `crates/`, `src/`, `config/`, `extensions/`, `scripts/`, `.github/`,
   or `Cargo.{toml,lock}` are staged without anything under `docs/`.
2. `cargo fmt --all -- --check`
3. `cargo clippy --workspace -- -D warnings`
4. `cargo test --workspace --quiet`

Enable it once per clone:

```bash
git config core.hooksPath .githooks
```

(`./scripts/bootstrap.sh` does this for you.)

### Bypass tags

The docs-sync gate honors a single opt-out tag. Include it in the
commit message when the change is genuinely internal and doesn't
need docs:

```
refactor: rename private fn [no-docs]
```

Acceptable reasons:

- Private refactor, no change to any public API
- Test-only changes
- Dependency bumps with no behavior change
- CI-config fiddling that doesn't alter ops

**Do not** use `[no-docs]` for anything a user would notice. If in
doubt, update the docs — it's the lower-regret path.

### Full escape hatch

`git commit --no-verify` disables all hooks (fmt, clippy, tests,
docs-sync). Last resort, not a habit.

## Reporting issues

Open a GitHub issue with:

- nexo-rs version / commit hash
- Rust version (`rustc -V`)
- OS / arch
- Relevant log lines (redact secrets)
- Minimal reproduction

## License of contributions

Contributions are dual-licensed MIT OR Apache-2.0 as described in
[License](./license.md).
