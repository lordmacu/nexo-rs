# template-plugin-rust

Skeleton out-of-tree **subprocess plugin** for Nexo. Copy + rename
to start a new channel plugin that speaks the wire format defined
in [`nexo-plugin-contract.md`](../../nexo-plugin-contract.md) at
the workspace root.

This template is the Rust counterpart to the Phase 31.4 Python
SDK ([`extensions/template-plugin-python/`](../template-plugin-python/))
and the Phase 31.5 TypeScript SDK
([`extensions/template-plugin-typescript/`](../template-plugin-typescript/)).
The wire format is identical across languages — only the SDK differs.

## What this template provides

- A minimal `PluginAdapter` driver in `src/main.rs` that:
  - Parses the bundled `nexo-plugin.toml` at boot
  - Responds to `initialize` requests with the full manifest
  - Echoes inbound `broker.event` notifications back as
    `broker.publish` notifications
  - Cleanly responds to `shutdown` requests
- A `nexo-plugin.toml` declaring `[plugin.entrypoint]` so the
  daemon's auto-subprocess fallback (Phase 81.17 + 81.17.b) can
  spawn this binary automatically when the manifest is dropped
  into a `plugins.discovery.search_paths` directory.
- A `Cargo.toml` with the SDK dep gated behind the `plugin`
  feature so out-of-tree builds only pull what they need.

## Quick start

```bash
# 1. Copy this directory out of the workspace
cp -r extensions/template-plugin-rust /tmp/my-plugin
cd /tmp/my-plugin

# 2. Rename the package + binary
sed -i 's/template-plugin-rust/my-plugin/g' Cargo.toml
sed -i 's/template_plugin_rust/my_plugin/g' src/main.rs nexo-plugin.toml
mv src/main.rs src/main.rs.bak # then edit your handler

# 3. Edit nexo-plugin.toml: pick a unique plugin.id + your
#    own [[plugin.channels.register]].kind

# 4. Once the SDK ships to crates.io, swap the path deps in
#    Cargo.toml for the published versions:
#       nexo-microapp-sdk = { version = "0.1", features = ["plugin"] }
#       nexo-broker = "0.1"

# 5. Build
cargo build --release

# 6. Drop into operator's discovery search_paths
cp nexo-plugin.toml ~/.local/share/nexo/plugins/my-plugin/
cp target/release/my-plugin ~/.local/share/nexo/plugins/my-plugin/

# 7. Edit ~/.local/share/nexo/plugins/my-plugin/nexo-plugin.toml
#    [plugin.entrypoint] command to the absolute path:
#       command = "/home/<user>/.local/share/nexo/plugins/my-plugin/my-plugin"

# 8. Restart the nexo daemon (or trigger Phase 18 hot-reload).
#    Plugin loads automatically — see daemon logs for
#    "plugin registry wire complete" with your plugin id.
```

## Where to add your channel logic

Replace the `handle_event` function in `src/main.rs`:

```rust
async fn handle_event(topic: String, event: Event, broker: BrokerSender) {
    // 1. Decode event.payload into your channel's message shape:
    //    let msg: MyChannelMessage = serde_json::from_value(event.payload)?;

    // 2. Forward to the external service:
    //    let reply = http_client.post("...").json(&msg).send().await?;

    // 3. Optionally publish the service's reply back through the
    //    broker so agents can observe it:
    //    let inbound = Event::new(
    //        "plugin.inbound.<your_kind>",
    //        "<your_plugin_id>",
    //        serde_json::to_value(reply)?,
    //    );
    //    broker.publish("plugin.inbound.<your_kind>", inbound).await?;
}
```

## Topic conventions

The daemon derives the broker bridge subscribe / publish allowlist
from your manifest's `[[plugin.channels.register]]` entries.
For each declared `kind = K`:

| Direction | Topics the daemon allows |
|-----------|--------------------------|
| Outbound (daemon → your plugin) | `plugin.outbound.K`, `plugin.outbound.K.<instance>` |
| Inbound (your plugin → daemon) | `plugin.inbound.K`, `plugin.inbound.K.<instance>` |

A child publish to anything outside this allowlist is dropped
with a `tracing::warn!` log. This is the daemon's primary defense
against a buggy / malicious plugin attempting to hijack core nexo
topics.

## What the daemon expects from your plugin

| Method | Source | What you reply with |
|--------|--------|---------------------|
| `initialize` | host → child | `{ manifest, server_version }`. The `manifest.plugin.id` MUST match the id under which the plugin was registered (the SDK handles this for you when you pass `MANIFEST` to `PluginAdapter::new`). |
| `broker.event` (notification) | host → child | No reply required. Process the event; optionally publish a `broker.publish` back. |
| `shutdown` | host → child | `{ ok: true }` after flushing state. Daemon waits 1s for clean exit before SIGKILL. |
| `broker.publish` (notification) | child → host | The daemon validates the topic against your allowlist before forwarding to the broker. No reply expected. |

Full spec: [`nexo-plugin-contract.md`](../../nexo-plugin-contract.md).

## Testing

```bash
# Quick handshake smoke test:
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"nexo_version":"0.1.5"}}' \
    | cargo run --release

# You should see one JSON-RPC response with your manifest +
# server_version.
```

End-to-end against the production daemon: drop the manifest into
`plugins.discovery.search_paths` and check `nexo agent doctor
plugins` (Phase 81.9.b) for the load + init outcome.

## Publishing your plugin (Phase 31.2)

Operators install plugins via `nexo plugin install <owner>/<repo>[@<tag>]`,
which fetches a GitHub Release matching a fixed asset naming
convention. This template ships a workflow + helper scripts that
produce that exact convention from a tag push.

### Asset convention

For every release tag `v<semver>` the workflow uploads:

| Asset | Required | Contents |
|-------|----------|----------|
| `nexo-plugin.toml` | ✅ | Manifest. Operator's CLI fetches first to learn `plugin.id`. |
| `<id>-<version>-<target>.tar.gz` | ✅ | One per supported target. Layout: `bin/<id>` + `nexo-plugin.toml` at root, no wrapping dir. |
| `<id>-<version>-<target>.tar.gz.sha256` | ✅ | Single line of lowercase hex. |
| `<id>-<version>-<target>.tar.gz.sig` / `.pem` / `.bundle` | ⬜ | Cosign keyless signing material. Phase 31.3 verifier consumes these when present. |

### What's in the box

```
.github/workflows/release.yml   # tag-driven publish workflow
scripts/extract-plugin-meta.sh  # exports PLUGIN_ID + PLUGIN_VERSION
scripts/pack-tarball.sh         # packs <id>-<version>-<target>.tar.gz + sha256
```

### Drop-in workflow

After copying this template to your own repo:

```bash
git tag v0.2.0
git push origin v0.2.0
```

The workflow:

1. Validates the tag format (`^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$`).
2. Asserts the tag matches `nexo-plugin.toml` `version`.
3. Builds release binaries per target (matrix). Default matrix:
   `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin`. Comment out
   matrix entries you do not want — macOS runners are billed at
   10× linux on GitHub-hosted runners.
4. Packs each target's binary via `scripts/pack-tarball.sh`,
   producing `dist/<id>-<version>-<target>.tar.gz` + sidecar.
5. (Optional) cosign signs each tarball when the repo variable
   `COSIGN_ENABLED` equals `"true"`. Set it once via:
   ```
   gh variable set COSIGN_ENABLED --body true
   ```
6. Creates the GitHub Release if missing, then uploads all
   artifacts (including `nexo-plugin.toml`).

### Required permissions

The workflow declares:

```yaml
permissions:
  contents: write   # gh release upload
  id-token: write   # cosign keyless OIDC
```

`GITHUB_TOKEN` is auto-provided. No additional secrets are
required for the unsigned path. Cosign keyless does not need any
secret either — it uses Sigstore/Fulcio with the workflow's OIDC
token.

### Validating the asset convention locally

Before pushing a tag, dry-run the pack step:

```bash
cargo build --release --target x86_64-unknown-linux-gnu
bash scripts/pack-tarball.sh x86_64-unknown-linux-gnu
ls dist/
# template_plugin_rust-0.1.0-x86_64-unknown-linux-gnu.tar.gz
# template_plugin_rust-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The integration test `tests/pack_tarball.rs` covers this end to
end against a synthetic binary.

### What an operator's trust entry looks like for your plugin

Once you tag a release with `COSIGN_ENABLED=true`, an operator
can allowlist your identity in their
`config/extensions/trusted_keys.toml` (Phase 31.3):

```toml
[[authors]]
owner = "your-github-username"
identity_regexp = "^https://github\\.com/your-github-username/[^/]+/\\.github/workflows/release\\.yml@.*$"
oidc_issuer = "https://token.actions.githubusercontent.com"
mode = "require"
```

The `identity_regexp` matches the cosign certificate Subject
Alternative Name, which in GitHub Actions keyless flow is the
workflow URL embedded in the cert. Tell your operators the
exact string (or regex) they should allowlist; it's stable as
long as you don't rename the workflow file.

### Constraint: bin name = plugin id

Cargo's `[[bin]] name` MUST equal `nexo-plugin.toml` `[plugin] id`
(the convention is `bin/<id>` inside the tarball, and
`pack-tarball.sh` looks for the binary at
`target/<target>/release/<id>`). This template ships them aligned
(`template_plugin_rust`); preserve the alignment when you rename
for your own plugin.

## Phase tracking

- 81.15.a (shipped) — `nexo-microapp-sdk` `plugin` feature +
  `PluginAdapter` child-side helper
- 81.16 (shipped) — `nexo-plugin-contract.md` v1.0.0
- 81.17 + 81.17.b (shipped) — daemon-side auto-subprocess
  pipeline activation
- 81.15.b (shipped) — clone-and-go starter drafted in-workspace.
- 31.2 (shipped) — release workflow + pack scripts (this section).
- 31.6 (deferred) — `nexo plugin new --lang rust` scaffolder will
  publish this template as `github.com/nexo-rs/plugin-template-rust`.
