# template-plugin-php

Skeleton out-of-tree **subprocess plugin** for nexo, written in
PHP 8.1+. Counterpart to
[`template-plugin-rust`](../template-plugin-rust/),
[`template-plugin-python`](../template-plugin-python/), and
[`template-plugin-typescript`](../template-plugin-typescript/) —
same wire format
([`nexo-plugin-contract.md`](../../nexo-plugin-contract.md)),
different SDK + asset convention.

## What this template provides

- `src/main.php` — minimal `PluginAdapter` driver that:
  - Parses the bundled `nexo-plugin.toml` once at startup.
  - Replies to `initialize` requests with the manifest.
  - Echoes inbound `broker.event` notifications back as
    `plugin.inbound.template_echo_php` publishes.
  - Logs `shutdown` requests to stderr before exiting.
- `nexo-plugin.toml` declaring `[plugin.entrypoint]` so the
  daemon's auto-subprocess fallback can spawn this binary
  automatically.
- `composer.json` declaring the SDK as an in-tree path
  repository (`symlink: false`).
- `composer.lock` checked in for reproducibility.
- `scripts/pack-tarball-php.sh` — vendors deps via Composer +
  packs into a single `noarch` tarball.
- `.github/workflows/release.yml` — tag-driven publish workflow
  that produces signed releases (cosign opt-in via
  `COSIGN_ENABLED` repo variable).

## Quick start

```bash
# 1. Scaffold a fresh plugin (Phase 31.6 scaffolder)
nexo plugin new my_plugin --lang php --owner yourhandle --git
cd my_plugin

# 2. Update the `repositories.url` in composer.json to point at
#    your published SDK (or keep the path repo if you fork the
#    nexo-rs workspace).

# 3. Implement your handler in src/main.php — replace the
#    onEvent closure.

# 4. Install deps. PHP 8.1+ + Composer 2.x required.
composer install

# 5. Smoke test locally
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
    | php src/main.php
# expect one JSON-RPC response with your manifest

# 6. Tag + push
git remote add origin git@github.com:yourhandle/my_plugin.git
git push -u origin main
git tag v0.1.0 && git push --tags
# .github/workflows/release.yml runs and uploads
# my_plugin-0.1.0-noarch.tar.gz to the release.
```

## Why PHP 8.1+

The SDK uses **Fibers** (introduced in PHP 8.1) to run each
`broker.event` handler as a cooperative coroutine. Without
Fibers the dispatch loop would block on slow handlers,
breaking the contract invariant proven necessary by the TS +
Python SDKs.

## Robustness defaults the SDK ships with

The `PluginAdapter` constructor defaults to:

- **`enableStdoutGuard: true`** — `ob_start` callback intercepts
  every `echo` / `print` / `printf` / `var_dump` write,
  buffers + line-parses, and diverts non-JSON lines to stderr
  tagged with `[stdout-guard] ...`. Guards against the most
  common plugin-author mistake of accidental `echo "debug"`
  corrupting the JSON-RPC frame stream the host parses.
  
  **Limitation**: `fwrite(STDOUT, $x)` direct writes BYPASS
  this guard (PHP `ob_start` only intercepts buffered output).
  The SDK's own `BrokerSender::publish()` uses direct `fwrite`
  deliberately so blessed JSON frames always reach the host.
  Plugin authors who need stdout output should use `echo` /
  `print` / `printf` (guarded).
- **`maxFrameBytes: 1 MiB`** — rejects oversized inbound
  frames with a `WireError` log; dispatch continues.
- **`handleProcessSignals: true`** — Ctrl-C / SIGTERM trigger a
  graceful shutdown (drain in-flight Fibers, exit 0). Uses
  `pcntl_async_signals(true)`.
- **In-flight Fiber drain on shutdown** — handlers spawned via
  `broker.event` are awaited in the scheduler before the SDK
  replies `{ok: true}` to a shutdown request, so
  `BrokerSender::publish()` calls never get cancelled
  mid-flight.

Opt out individually via the constructor options if you have a
specific reason.

## Asset convention (`noarch`)

For every release tag `v<semver>` the workflow uploads:

| Asset | Required | Contents |
|-------|----------|----------|
| `nexo-plugin.toml` | ✅ | Plugin manifest. |
| `<id>-<version>-noarch.tar.gz` | ✅ | Single tarball runnable on every operator host. Layout: `bin/<id>` (bash launcher, mode 0755) + `nexo-plugin.toml` at root + `lib/plugin/main.php` + vendored `lib/vendor/`. |
| `<id>-<version>-noarch.tar.gz.sha256` | ✅ | One line of lowercase hex (64 chars). |
| `<id>-<version>-noarch.tar.gz.sig` / `.pem` / `.bundle` | ⬜ | Cosign keyless signing material when `COSIGN_ENABLED=true`. |

`nexo plugin install <owner>/<repo>@<tag>` on the operator side
falls back to `noarch` when no per-target tarball matches the
daemon's host triple — see Phase 31.4 release notes.

## What an operator's trust entry looks like for your plugin

Once `COSIGN_ENABLED=true`, an operator can allowlist your
identity in their `config/extensions/trusted_keys.toml` (see the
[plugin trust docs](../../docs/src/ops/plugin-trust.md)):

```toml
[[authors]]
owner = "your-github-username"
identity_regexp = "^https://github\\.com/your-github-username/[^/]+/\\.github/workflows/release\\.yml@.*$"
oidc_issuer = "https://token.actions.githubusercontent.com"
mode = "require"
```

## Constraint: pure-PHP deps only

Vendoring at the operator host requires deps that are
architecture-independent. Native PHP extensions (`*.so`,
`*.dylib`, `*.dll`) are normally loaded via `php.ini` from
`/usr/lib/php/<version>/`, NOT vendored. If a Composer dep
smuggles in a native build artifact under `vendor/`, the
publish workflow's audit step (`scripts/verify-pure-php.sh`)
rejects the tarball.

If your plugin needs a native dep, per-target tarballs are
tracked as Phase 31.5.c.b and not yet shipped.

## Composer integration

The template uses a **path repository** to consume the in-tree
SDK during dev. When you fork this template:

```json
"repositories": [
  {
    "type": "path",
    "url": "../sdk-php",
    "options": { "symlink": false }
  }
]
```

`symlink: false` is critical. Without it Composer creates a
symlink in `vendor/nexo/plugin-sdk/` pointing at the path repo;
when the tarball is packed, the symlink would break on the
operator host. With `symlink: false` Composer copies the SDK
files physically — the tarball stays self-contained.

`composer.lock` is checked in for reproducibility (analogous to
`Cargo.lock` for binary projects). Plugin authors regenerate it
with `composer update` when bumping deps.

## Layout reference

```
template-plugin-php/
├── nexo-plugin.toml
├── composer.json
├── composer.lock
├── src/
│   └── main.php
├── scripts/
│   ├── extract-plugin-meta.sh        # shared with rust template
│   ├── pack-tarball-php.sh
│   └── verify-pure-php.sh
├── tests/
│   └── test_pack_tarball.php         # end-to-end pack assertion
├── .github/workflows/release.yml
└── README.md
```

## Testing

```bash
# Pack assertion (proves the bash pipeline produces the asset
# convention 31.1 consumes):
php tests/test_pack_tarball.php
```

The SDK itself ships separate tests under
[`extensions/sdk-php/tests/`](../sdk-php/tests/).

## Phase tracking

- 31.5.c (shipped) — PHP SDK + this template.
- 31.5.c.b (deferred) — per-target PHP tarballs for plugins
  that need native PHP extensions.
