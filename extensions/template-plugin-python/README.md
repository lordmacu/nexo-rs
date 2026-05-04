# template-plugin-python

Skeleton out-of-tree **subprocess plugin** for nexo, written in
Python. Counterpart to [`template-plugin-rust`](../template-plugin-rust/)
and [`template-plugin-typescript`](../template-plugin-typescript/)
— same wire format
([`nexo-plugin-contract.md`](../../nexo-plugin-contract.md)),
different SDK + asset convention.

## What this template provides

- `src/main.py` — minimal `PluginAdapter` driver that:
  - Parses the bundled `nexo-plugin.toml` once at startup.
  - Replies to `initialize` requests with the manifest.
  - Echoes inbound `broker.event` notifications back as
    `plugin.inbound.template_echo_py` publishes.
  - Logs `shutdown` requests to stderr before exiting.
- `nexo-plugin.toml` declaring `[plugin.entrypoint]` so the
  daemon's auto-subprocess fallback can spawn this binary
  automatically when the manifest is dropped into a
  `plugins.discovery.search_paths` directory.
- `scripts/pack-tarball-python.sh` — vendors deps + SDK + plugin
  source into a single `noarch` tarball matching the convention
  `nexo plugin install` expects.
- `.github/workflows/release.yml` — tag-driven publish workflow
  that produces signed releases (cosign opt-in via
  `COSIGN_ENABLED` repo variable).

## Quick start

```bash
# 1. Copy this directory out of the workspace
cp -r extensions/template-plugin-python /tmp/my-plugin
cd /tmp/my-plugin

# 2. Rename the package + plugin id
sed -i 's/template_plugin_python/my_plugin/g' nexo-plugin.toml src/main.py
sed -i 's/template_echo_py/my_kind/g' nexo-plugin.toml src/main.py

# 3. Implement your handler in src/main.py — replace `on_event`.

# 4. Add any pure-Python deps to requirements.txt. Native
#    extensions (.so / .pyd / .dylib) invalidate the noarch
#    convention and the publish workflow's audit job will
#    reject them — see scripts/verify-pure-python.sh.

# 5. Smoke test locally
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
    | python3 src/main.py
# expect one JSON-RPC response with your manifest

# 6. Tag + push
git tag v0.1.0
git push --tags
# .github/workflows/release.yml runs and uploads
# my_plugin-0.1.0-noarch.tar.gz to the release.
```

## Asset convention (`noarch`)

For every release tag `v<semver>` the workflow uploads:

| Asset | Required | Contents |
|-------|----------|----------|
| `nexo-plugin.toml` | ✅ | Plugin manifest. |
| `<id>-<version>-noarch.tar.gz` | ✅ | Single tarball runnable on every operator host. Layout: `bin/<id>` (bash launcher, mode 0755) + `nexo-plugin.toml` at root + `lib/plugin/main.py` + vendored `lib/nexo_plugin_sdk/`. |
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

## Constraint: pure Python deps only

Vendoring at the operator host requires deps that are
architecture-independent. `pip install --target` may pull a
wheel containing native extensions (`*.so`, `*.pyd`, `*.dylib`)
which would not run on every operator's CPU. The workflow's
audit step (`scripts/verify-pure-python.sh`) rejects vendored
trees that contain any of these suffixes.

If you need a native dep, publish per-target tarballs (Phase
31.4.b — not yet shipped) instead of `noarch`.

## Layout reference

```
template-plugin-python/
├── nexo-plugin.toml
├── requirements.txt
├── src/
│   └── main.py
├── scripts/
│   ├── extract-plugin-meta.sh        # shared with rust template
│   ├── pack-tarball-python.sh
│   └── verify-pure-python.sh
├── tests/
│   └── test_pack_tarball.py          # end-to-end pack assertions
├── .github/workflows/release.yml
└── README.md
```

## Testing

```bash
# Pack assertion (proves the bash pipeline produces the asset
# convention 31.1 consumes):
python3 -m unittest tests/test_pack_tarball.py -v
```

The SDK itself ships separate tests under
[`extensions/sdk-python/tests/`](../sdk-python/tests/).

## Phase tracking

- 31.4 (shipped) — Python SDK + this template + `noarch`
  resolver fallback in `crates/ext-installer/`.
- 31.4.b (deferred) — per-target Python tarballs for plugins
  that need native extensions.
