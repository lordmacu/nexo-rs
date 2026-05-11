# Installing personas — `nexo persona install`

A persona pack bundles an out-of-tree agent definition (system prompt
+ plugin bindings + workspace seed + secrets templates) that
operators install into their `nexo` daemon. Distinct from a plugin
(plugins register CODE; personas register CONFIG that consumes that
code). Authored as a v2 manifest pack and published as a GitHub
Release; the daemon resolves + downloads + verifies + extracts under
the operator's configured search path.

> **v1 vs v2.** The legacy `install.sh`-driven flow (v1 manifest)
> stays supported for airgapped hosts + CI. `nexo persona install`
> only consumes v2 manifests (`manifest_version = 2`); a v1 pack
> errors with a clear migration hint pointing at `install.sh`.

## Quickstart

```bash
# Install the latest release of a persona from GitHub:
nexo persona install lordmacu/nexo-persona-cody

# Pin to a specific release tag:
nexo persona install lordmacu/nexo-persona-cody@v0.2.0

# JSON output for CI:
nexo persona install lordmacu/nexo-persona-cody --json

# List every installed persona:
nexo persona list

# Remove (with confirmation gate):
nexo persona remove cody             # prints what WOULD be removed
nexo persona remove cody --yes       # actually removes
```

## Subcommands

| Command | Purpose |
|---------|---------|
| `nexo persona install <owner>/<repo>[@<tag>]` | Resolve + verify + extract a v2 persona pack. |
| `nexo persona list` | Walk every configured search path, render every installed persona. |
| `nexo persona remove <id> [--yes]` | Atomic removal of the install dir for `<id>`. |
| `nexo persona get <id>` | Print the full manifest + computed contributes paths for `<id>`. |
| `nexo persona upgrade <id>` | Re-resolve the installed persona's source repo at `latest` + install if newer. Refuses to downgrade. |
| `nexo persona run <path>` | Inner-loop dev: validate a local persona pack + boot the daemon with its parent dir prepended to `personas.discovery.search_paths`. Mirror of `nexo plugin run`. |
| `nexo persona help` | Print the help text inline. |

## Flags

### `install`

| Flag | Default | Effect |
|------|---------|--------|
| `--dest <dir>` | `cfg.personas.discovery.search_paths[0]` (or `<state_dir>/personas/`) | Override the install root. Must be absolute. |
| `--target <triple>` | Daemon's host triple (`NEXO_INSTALL_TARGET` env wins) | Asset-matching target. Persona packs typically publish `noarch` only; the resolver falls back automatically. |
| `--json` | off | Emit a JSON envelope instead of human-readable lines. CI-friendly. |

### `list`

| Flag | Effect |
|------|--------|
| `--json` | JSON array under `{ "personas": [...] }`. |

### `remove`

| Flag | Effect |
|------|--------|
| `--yes` | Required — without it the command prints what it WOULD remove and exits 0. |
| `--json` | Same JSON envelope as `install`. |

### `get`

Prints id / version / description / homepage / install_root + every
`contributes.agent_configs` and `contributes.plugin_configs_partial`
path resolved to absolute. JSON variant returns the full manifest
sections (`requires`, `meta`) too — CI can grep specific fields
without re-parsing the on-disk TOML.

| Flag | Effect |
|------|--------|
| `--json` | Emit the typed manifest payload as JSON instead of human lines. |

### `upgrade`

Inspects `cfg.personas.discovery.search_paths`, finds the installed
persona by id, extracts its source GitHub repo from
`manifest.persona.homepage`, hits the GitHub Releases API at
`/releases/latest`, and re-runs the install pipeline if the resolved
version is **strictly newer** than the on-disk one. Refuses to
downgrade (use `nexo persona install <coords>@<tag>` to pin if
intentional).

| Flag | Effect |
|------|--------|
| `--json` | Same JSON envelope as `install`. |

### `run`

Inner-loop dev — point the daemon at a local persona pack without
going through the install + verify pipeline. Validates the path's
`persona.toml`, prepends the pack's **parent dir** to
`cfg.personas.discovery.search_paths` (so the boot-time F5
discovery picks it up as `<parent>/<id>-<version>/`), then falls
through to the daemon boot path.

```bash
# Develop a persona locally:
mkdir -p /tmp/dev/cody-0.99.0
$EDITOR /tmp/dev/cody-0.99.0/persona.toml
nexo persona run /tmp/dev/cody-0.99.0
```

| Flag | Effect |
|------|--------|
| `--json` | Emit the override payload as JSON before daemon boot starts streaming logs. |

## Configuration — `personas/discovery.yaml`

Lives at `<config_dir>/personas/discovery.yaml`. Optional —
absent file means no scan happens (the daemon boots with an
empty persona catalog).

```yaml
discovery:
  search_paths:
    - /var/lib/nexo/personas        # default for system installs
    - /home/operator/.nexo/personas # default for user installs
  disabled: []                      # ids skipped even when found
  allowlist: []                     # empty = accept any; non-empty = whitelist
```

The CLI consumes the same config: `nexo persona list` walks
`search_paths` and applies the `disabled` / `allowlist` filters.

## Layout on disk

After a successful install, the pack lives under:

```
<install_root>/
  <id>-<version>/
    persona.toml
    agents.d/
      <agent>.yaml
    plugins/
      <plugin>.partial.yaml
    secrets/
      <secret>.txt.template
    data/
      workspace/...
```

The `<id>-<version>` shape mirrors the plugin install layout (Phase
31.1.b) so operators familiar with one immediately read the other.
Re-installing the same `id+version` short-circuits via the
idempotency check (no re-download, returns `was_already_present:
true`).

## Boot-time discovery

When the daemon starts, after `plugins.start_all` it walks
`cfg.personas.discovery.search_paths`, parses + validates every
`<id>-<version>/persona.toml`, applies the `disabled` / `allowlist`
filters, and registers each survivor in an in-memory persona
catalog. Discovery is best-effort: malformed / unparseable packs are
logged at WARN and skipped rather than aborting boot.

## Kill switch — `NEXO_DISABLE_BUNDLED_PERSONAS`

Set to `1` / `true` / `on` to skip discovery entirely, regardless of
`cfg.personas.discovery.search_paths`. The daemon's in-memory
catalog stays empty; the CLI still works against the on-disk dirs
(it re-runs discovery itself).

```bash
export NEXO_DISABLE_BUNDLED_PERSONAS=1
nexo daemon
```

Useful for hardened deployments that want to refuse all out-of-tree
persona packs at the daemon level even when the search paths config
still references dirs. Surfaces in `nexo doctor capabilities` as a
**Medium** risk toggle (Phase F7 of `cody-cli-install`).

## Wire shape — release JSON conventions

A v2 persona release on GitHub must publish these assets at the
release tag:

| Asset | Required | Purpose |
|-------|----------|---------|
| `persona.toml` | yes | The v2 manifest. |
| `<id>-<version>-<target>.tar.gz` OR `<id>-<version>-noarch.tar.gz` | yes (one of) | The pack tarball. `noarch` is the fallback when no per-target asset exists. |
| `<tarball>.sha256` | yes | Single line of lowercase hex (64 chars). |
| `<tarball>.sig` + `<tarball>.cert` | optional | Cosign material — when both present, the resolver records them in the resolved entry (verification gates land in a follow-up wave). |

The naming convention mirrors `nexo plugin install` (Phase 31.1.c)
so a single CI workflow can publish both flavors with the same
tooling (`cargo dist`, `gh release upload`).

## Errors

| Symptom | Cause | Fix |
|---------|-------|-----|
| `release tag does not parse as semver` | Tag uses `release-1.2.3` or another non-semver shape. | Re-tag as `vX.Y.Z`. |
| `release is missing required asset persona.toml` | The release JSON has no `persona.toml` asset. | Upload the manifest as a release asset matching the convention. |
| `persona id violates id regex` | The manifest's `[persona] id` has uppercase / spaces / etc. | Rename to `^[a-z0-9][a-z0-9-]{2,63}$`. |
| `v1 packs install via the persona's install.sh` | Manifest declares `manifest_version = 1`. | Bump to `2` (no field-shape changes); same TOML re-parses. |
| `tar entry path contains ..; rejected for safety` | Malicious / malformed tarball. | Re-pack ensuring every entry path is relative + traversal-free. |
| `persona install root must be an absolute path` | `--dest <relative>`. | Pass an absolute path. |

## Related

- Persona pack manifest schema (`persona.toml`) — see the
  [Cody pack README](https://github.com/lordmacu/nexo-persona-cody)
  for the v2 manifest shape (a dedicated docs page is a TBD
  follow-up).
- [Plugin install (`nexo plugin install`)](../plugins/publishing.md)
  — sister CLI surface; the persona installer reuses ~60 % of the
  resolve + download + sha256-verify plumbing.
- [Broker shapes](../architecture/broker-shapes.md) — local vs.
  NATS vs. embedded (orthogonal, but referenced by personas
  declaring `[persona.requires] features`).
