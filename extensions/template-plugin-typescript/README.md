# template-plugin-typescript

Skeleton out-of-tree **subprocess plugin** for nexo, written in
TypeScript. Counterpart to
[`template-plugin-rust`](../template-plugin-rust/),
[`template-plugin-python`](../template-plugin-python/), and
[`template-plugin-php`](../template-plugin-php/) — same wire
format ([`nexo-plugin-contract.md`](../../nexo-plugin-contract.md)),
different SDK + asset convention.

## What this template provides

- `src/main.ts` — minimal `PluginAdapter` driver that:
  - Parses the bundled `nexo-plugin.toml` once at startup.
  - Replies to `initialize` requests with the manifest.
  - Echoes inbound `broker.event` notifications back as
    `plugin.inbound.template_echo_ts` publishes.
  - Logs `shutdown` requests to stderr before exiting.
- `nexo-plugin.toml` declaring `[plugin.entrypoint]` so the
  daemon's auto-subprocess fallback can spawn this binary
  automatically.
- `scripts/pack-tarball-typescript.sh` — vendors deps + SDK +
  compiled JS into a single `noarch` tarball.
- `.github/workflows/release.yml` — tag-driven publish workflow
  that produces signed releases (cosign opt-in via
  `COSIGN_ENABLED` repo variable).

## Quick start

```bash
# 1. Copy this directory out of the workspace.
cp -r extensions/template-plugin-typescript /tmp/my-plugin
cd /tmp/my-plugin

# 2. Rename the package + plugin id.
sed -i 's/template_plugin_typescript/my_plugin/g' nexo-plugin.toml src/main.ts
sed -i 's/template_echo_ts/my_kind/g' nexo-plugin.toml src/main.ts

# 3. Implement your handler in src/main.ts — replace `onEvent`.

# 4. Add deps via npm; pure-JS only (no native addons or the
#    publish workflow's audit job will reject the noarch tarball).

# 5. Smoke test locally
npm install
npm run build
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
    | node dist/main.js
# expect one JSON-RPC response with your manifest

# 6. Tag + push.
git tag v0.1.0
git push --tags
# .github/workflows/release.yml runs and uploads
# my_plugin-0.1.0-noarch.tar.gz to the release.
```

## Why TypeScript with `--strict`

The shipped SDK ships with strict types throughout. Plugin
authors who write `.ts` get compile-time guarantees that their
handler signatures match the SDK contract. JavaScript-only
authors can drop `tsconfig.json` and write `src/main.js`
directly; the publish workflow detects the absence and skips the
`tsc` step.

## Robustness defaults the SDK ships with

The `PluginAdapter` constructor defaults to:

- **`enableStdoutGuard: true`** — patches `process.stdout.write`
  so any stray `console.log` from your handler (or a chatty
  transitive dep) is diverted to stderr tagged with
  `[stdout-guard] ...` instead of corrupting the JSON-RPC
  stream the host parses. The line stays visible to operator
  logs; the host stays connected.
- **`maxFrameBytes: 1 MiB`** — rejects oversized inbound frames
  with a `WireError` log; dispatch loop continues.
- **`handleProcessSignals: true`** — Ctrl-C / SIGTERM trigger
  a graceful shutdown (drain in-flight handler tasks, exit 0).
- **In-flight task drain on shutdown** — handlers spawned via
  `broker.event` are awaited before the SDK replies `{ok: true}`
  to a shutdown request, so `broker.publish` calls never get
  cancelled mid-flight.

Opt out individually via the constructor options if you have a
specific reason.

## Asset convention (`noarch`)

For every release tag `v<semver>` the workflow uploads:

| Asset | Required | Contents |
|-------|----------|----------|
| `nexo-plugin.toml` | ✅ | Plugin manifest. |
| `<id>-<version>-noarch.tar.gz` | ✅ | Single tarball runnable on every operator host. Layout: `bin/<id>` (bash launcher, mode 0755) + `nexo-plugin.toml` at root + `lib/plugin/main.js` + vendored `lib/node_modules/`. |
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

## Constraint: pure-JS deps only

Vendoring at the operator host requires deps that are
architecture-independent. Native node addons (`*.node`,
`*.so`, `*.dylib`, `*.dll`) invalidate the `noarch` claim. The
workflow's audit step (`scripts/verify-pure-js.sh`) rejects
vendored trees that contain any of these suffixes.

If your plugin needs a native dep, per-target tarballs are
tracked as Phase 31.5.b and not yet shipped.

## Layout reference

```
template-plugin-typescript/
├── nexo-plugin.toml
├── package.json
├── tsconfig.json
├── src/
│   └── main.ts
├── scripts/
│   ├── extract-plugin-meta.sh        # shared with rust template
│   ├── pack-tarball-typescript.sh
│   └── verify-pure-js.sh
├── tests/
│   └── pack-tarball.test.mjs         # end-to-end pack assertions
├── .github/workflows/release.yml
└── README.md
```

## Testing

```bash
# Pack assertion (proves the bash pipeline produces the asset
# convention 31.1 consumes):
node --test tests/pack-tarball.test.mjs
```

The SDK itself ships separate tests under
[`extensions/sdk-typescript/tests/`](../sdk-typescript/tests/).

## Phase tracking

- 31.5 (shipped) — TypeScript SDK + this template.
- 31.5.b (deferred) — per-target TypeScript tarballs for plugins
  that need native node addons.
- 31.5.c (deferred) — PHP SDK + template.
