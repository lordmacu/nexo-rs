# PHP plugin SDK

Phase 31.5.c. Author plugins in PHP 8.1+ that the daemon spawns
as subprocesses, talking the same JSON-RPC 2.0 wire format used
by the Rust + Python + TypeScript SDKs.

Reference template:
[`extensions/template-plugin-php/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-php).
The SDK package itself lives at
[`extensions/sdk-php/`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/sdk-php).

## Why PHP 8.1+

The SDK uses **Fibers** (introduced in PHP 8.1) to run each
`broker.event` handler as a cooperative coroutine. Without
Fibers the dispatch loop would block on slow handlers,
breaking the contract invariant proven necessary by the TS +
Python SDKs.

## Architecture summary

```
Operator host                          Plugin process
┌──────────────────┐    stdin    ┌────────────────────────────┐
│ daemon (Rust)    │──JSON-RPC──▶│ bin/<id> (bash launcher)   │
│ subprocess host  │             │   exec php main.php        │
│                  │◀──JSON-RPC──│   PluginAdapter::run()     │
└──────────────────┘    stdout   │   Fiber scheduler ticks    │
                                 │   between stdin polls      │
                                 └────────────────────────────┘
```

The bash launcher in `bin/<id>` runs:

```bash
exec env php -d display_errors=stderr -d log_errors=0 \
    "$DIR/lib/plugin/main.php" "$@"
```

`-d display_errors=stderr` is critical — without it, PHP's
default behavior writes errors to stdout, which would corrupt
the JSON-RPC frame stream.

Daemon-side spawn code in
[`crates/core/src/agent/nexo_plugin_registry/subprocess.rs`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core/src/agent/nexo_plugin_registry)
treats the plugin as an opaque executable; PHP plugins re-use
it without modification.

## Public API

```php
use Nexo\Plugin\Sdk\PluginAdapter;     // async dispatch loop
use Nexo\Plugin\Sdk\BrokerSender;      // write-only broker handle
use Nexo\Plugin\Sdk\Event;             // value object
use Nexo\Plugin\Sdk\Manifest;          // standalone TOML parser
use Nexo\Plugin\Sdk\StdoutGuard;       // defensive guard
use Nexo\Plugin\Sdk\Wire;              // JSON-RPC frame helpers + MAX_FRAME_BYTES
use Nexo\Plugin\Sdk\PluginError;       // base exception
use Nexo\Plugin\Sdk\ManifestError;     // raised when manifest malformed
use Nexo\Plugin\Sdk\WireError;         // raised on malformed/oversized frames
```

`PluginAdapter` constructor options:

| Option | Required | Description |
|--------|----------|-------------|
| `manifestToml: string` | ✅ | Body of `nexo-plugin.toml`. Read once at startup; the SDK validates `plugin.id` (regex `/^[a-z][a-z0-9_]{0,31}$/`), `plugin.version`, `plugin.name`, `plugin.description`. |
| `serverVersion?: string` | ⬜ | Returned in the `initialize` reply. Default `"0.1.0"`. |
| `onEvent?: callable(string, Event, BrokerSender): void` | ⬜ | Invoked for every `broker.event` notification. Runs in a Fiber so the dispatch loop continues. |
| `onShutdown?: callable(): void` | ⬜ | Awaited before `{ok: true}` reply to the host's `shutdown` request. In-flight Fibers also drained first. |
| `enableStdoutGuard?: bool` | ⬜ default `true` | Installs an `ob_start` callback that diverts non-JSON `echo`/`print`/`printf`/`var_dump` output to stderr tagged with `[stdout-guard]`. |
| `maxFrameBytes?: int` | ⬜ default `1048576` | Reject inbound frames larger than this with `WireError`; dispatch continues. |
| `handleProcessSignals?: bool` | ⬜ default `true` | Listen for SIGTERM + SIGINT via `pcntl_async_signals` and trigger graceful shutdown (drain in-flight, exit 0). |

## Tarball convention (`noarch`)

Operators install PHP plugins via the same
`nexo plugin install <owner>/<repo>[@<tag>]` CLI. The resolver
in `nexo-ext-installer` falls back to `noarch` when no
per-target tarball matches the daemon's host triple (Phase
31.4):

```
<id>-<version>-noarch.tar.gz
├── nexo-plugin.toml
├── bin/<id>           # bash launcher mode 0755
└── lib/
    ├── plugin/main.php
    └── vendor/        # composer install --no-dev output
        ├── autoload.php
        ├── nexo/plugin-sdk/...
        ├── yosymfony/toml/...
        └── composer/...
```

## Composer integration

Templates consume the in-tree SDK via a **path repository**:

```json
"repositories": [
  {
    "type": "path",
    "url": "../sdk-php",
    "options": { "symlink": false }
  }
]
```

`symlink: false` is critical — without it Composer creates a
symlink in `vendor/nexo/plugin-sdk/` pointing at the path repo.
When the tarball is packed, that symlink would break on the
operator host. With `symlink: false` Composer copies the SDK
files physically — the tarball stays self-contained.

The publish workflow runs:

```bash
composer install --no-dev --optimize-autoloader --classmap-authoritative
```

This produces a deterministic + smallest vendor tree. The
operator host does NOT need Composer installed — the
`vendor/autoload.php` shipped in the tarball is plain PHP and
works with just `php-cli`.

`composer.lock` is checked in for the template (reproducibility
analogous to `Cargo.lock` for binary projects). The SDK itself
omits the lockfile so consumers resolve fresh against their own
constraints.

## Pure-PHP deps constraint

`noarch` requires that vendored deps work on every operator's
CPU. Native PHP extensions (`*.so`, `*.dylib`, `*.dll`) are
normally loaded via `php.ini` from `/usr/lib/php/<version>/`,
NOT vendored. If a Composer dep smuggles in a native build
artifact under `vendor/`, the publish workflow's
`scripts/verify-pure-php.sh` audit step rejects the tarball.

If your plugin needs a native dep, per-target tarballs are
tracked as Phase 31.5.c.b and not yet shipped.

## Stdout guard — what's guarded vs not

| API | Behavior |
|-----|----------|
| `echo $x;` | ✅ Guarded — non-JSON lines diverted to stderr. |
| `print $x;` | ✅ Guarded. |
| `printf("%s", $x);` | ✅ Guarded. |
| `var_dump($x);` | ✅ Guarded. |
| `fwrite(STDOUT, $x);` | ❌ **NOT guarded** — bypasses `ob_start`. The SDK's own `BrokerSender::publish()` uses this deliberately so blessed JSON frames always reach the host. |

**Plugin authors who need stdout output should use `echo` /
`print` / `printf`** — those are guarded. Calling
`fwrite(STDOUT, ...)` directly from author code is undefined
behavior; the operator's daemon will see the raw bytes and
disconnect on parser failure.

## CI publish workflow

The shipped workflow in
[`extensions/template-plugin-php/.github/workflows/release.yml`](https://github.com/lordmacu/nexo-rs/tree/main/extensions/template-plugin-php/.github/workflows/release.yml)
has the same 4-job shape as the Rust + Python + TS templates
but:

- Build matrix has a single `noarch` entry.
- Build step uses `shivammathur/setup-php@v2` with
  `php-version: "8.3"` + `tools: composer:v2`.
- `composer validate --strict` gates the build.
- `composer install --no-dev --optimize-autoloader
  --classmap-authoritative` produces the vendor tree.
- Pack step calls `scripts/pack-tarball-php.sh` with
  `SKIP_COMPOSER=1` (composer ran already).
- Vendor audit step calls `scripts/verify-pure-php.sh
  .audit/lib/vendor` to enforce pure-PHP.

Sign + release jobs are identical to the other templates;
cosign keyless OIDC ships `.sig` + `.pem` + `.bundle` per asset
when the `COSIGN_ENABLED` repo variable is `"true"`.

## Operator install flow (no changes for PHP)

```bash
nexo plugin install your-handle/your-plugin@v0.2.0
```

Identical pipeline to the Rust + Python + TS install paths:

1. Resolve release JSON.
2. Try `<id>-0.2.0-<host-triple>.tar.gz` (miss for noarch
   plugins).
3. **Fall back to `<id>-0.2.0-noarch.tar.gz`** (Phase 31.4
   addition).
4. Verify sha256.
5. Cosign verify per `trusted_keys.toml` (Phase 31.3).
6. Extract under `<dest_root>/<id>-0.2.0/`.
7. Daemon picks it up at next boot or hot-reload; spawns
   `bin/<id>` which exec's `php lib/plugin/main.php`.

## Local smoke test

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
    | php src/main.php
```

Should print one JSON-RPC response with your manifest +
`server_version`.

End-to-end test for the pack pipeline:

```bash
php tests/test_pack_tarball.php
```

## SDK tests

```bash
cd extensions/sdk-php
composer install
php tests/run-all.php
```

14 test cases across handshake, manifest validation, dispatch
(incl. Fiber-based slow-handler proof + drain), stdout-guard,
wire-format hardening, lifecycle, event round-trip. All run via
plain PHP scripts using `proc_open` — zero PHPUnit / Pest dep,
mirroring the TS SDK's `node:test` choice and the Python SDK's
`unittest` choice.

## Plugin author constraint: cooperative scheduling

The Fiber scheduler preserves the "reader does not block on
handler" invariant **only at SDK boundaries**. If your handler
calls a synchronous blocking I/O function:

```php
$result = file_get_contents("https://example.com/slow");  // blocks
```

…the dispatch loop blocks for the duration of the call.
Cooperative scheduling cannot interrupt blocking I/O. Two
mitigations:

1. Keep handlers fast — typical channel plugins do work in
   <10ms.
2. For long external calls, periodically `Fiber::suspend()` to
   yield. The SDK doesn't auto-suspend; that's an explicit
   author decision.

This matches the Python and TypeScript SDKs' contract — long
blocking work is the author's responsibility to break up.

## See also

- [Publishing a plugin (CI workflow)](./publishing.md) — Rust
  counterpart of the publish workflow this template is modeled
  after.
- [TypeScript plugin SDK](./typescript-sdk.md) — sibling SDK
  with similar robustness defaults.
- [Python plugin SDK](./python-sdk.md) — sibling SDK with the
  closest async model match.
- [Plugin trust (`trusted_keys.toml`)](../ops/plugin-trust.md)
  — operator-side cosign verification policy that applies to
  PHP plugins too.
- [Plugin contract](./contract.md) — wire format all SDKs
  implement.
