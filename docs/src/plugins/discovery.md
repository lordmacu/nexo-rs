# Plugin discovery

Phase 98 ships a public catalogue layer on top of the Phase 97.1
install pipeline. Operators can browse plugins by name / tag /
category / source before deciding what to install — no more
guessing the crate name from a chat log.

## How it works

The daemon fetches plugin metadata from three independent sources
on a 24-hour cache cadence, merges by canonical crate name, and
surfaces the result via:

- **CLI** — `agent plugin search [QUERY] [--compat-only] [--category=…] [--source=…] [--json]`
- **Admin RPC** — `nexo/admin/plugins/{search,compat_check,refresh_index}`
- **Admin UI** — `/m/plugins` "Available" tab in `nexo-rs-plugin-admin`

### Sources

| Source | What it pulls | Auth | Rate limit |
|---|---|---|---|
| **crates.io** | `q=nexo-plugin` + `q=nexo-poller` REST | none | 100 req/min |
| **GitHub topic** | repos tagged `topic:nexo-plugin` | optional `GITHUB_TOKEN` env | 10/min unauth · 5000/hr auth |
| **Curated index** | `lordmacu/nexo-plugin-index/index.json` | none | 60/hr per IP (raw.githubusercontent.com) |

Per-source failures (rate limit / 5xx / network) surface as a
`partial_failures: Vec<SourceError>` in the response; healthy
sources still contribute to the catalogue.

### Compat gate

Each entry's `compat` field compares the plugin's manifest
`[plugin] min_nexo_version` semver range against the running
daemon's `CARGO_PKG_VERSION`:

- `compatible` — install button enabled.
- `needs_upgrade { required, current }` — daemon too old; UI shows
  "upgrade daemon to ≥X.Y.Z".
- `incompatible { reason }` — daemon too new for the plugin's
  pinned upper bound; install button disabled.
- `unknown` — manifest not fetched (404 / parse error / source
  didn't expose a URL). Install allowed but the badge warns.

### Trust tiers

Three operator-facing signals; NOT cryptographically enforced
(Phase 97.1's cosign pipeline handles the signature side):

- **`official`** — owner appears in the daemon's
  `trusted_keys.toml` allowlist.
- **`community_indexed`** — plugin listed in
  `lordmacu/nexo-plugin-index`.
- **`unverified`** — neither. Use after manual review.

## CLI usage

```sh
# Browse the full catalogue.
$ agent plugin search

# Filter by substring (matches name + description + tags).
$ agent plugin search telegram

# Compat-only + structured output.
$ agent plugin search --compat-only --json

# Show only channels from the curated index.
$ agent plugin search --category=channel --source=curated_index

# Invalidate the 24h cache so next search re-fetches.
$ agent plugin refresh
```

Output is a table by default (`NAME | VERSION | OWNER | TRUST |
COMPAT | INSTALL`) or raw JSON with `--json`. Partial source
failures append a `! Partial failures:` footer.

## Appearing in the catalogue

You have three paths; each is independent.

1. **Publish to crates.io** with a name starting `nexo-plugin-` or
   `nexo-poller-`. The daemon's `CratesIoSource` picks you up
   automatically on the next refresh.
2. **Add the `nexo-plugin` GitHub topic** to your repo (Settings
   → About → Topics). `GithubTopicSource` discovers you within the
   next refresh window.
3. **Open a PR on `lordmacu/nexo-plugin-index`** with a new entry
   in `index.json`. The curated source promotes your plugin to
   `community_indexed` trust + lets you supply explicit category +
   tags + description for the UI.

For trust-tier `official` the daemon operator adds your `owner` to
their local `trusted_keys.toml`. That's a per-operator decision —
not something a plugin author can request.

## Configuration knobs

`DiscoveryConfig` lives in `nexo-plugin-discovery::config`. The
daemon builds it with `DiscoveryConfig::with_defaults(state_dir,
daemon_version)` at boot; each field overridable via main.rs
manually if a deployment needs to point at private mirrors:

| Field | Default | Why override |
|---|---|---|
| `cache_ttl` | 24 hours | Stricter freshness for staging environments |
| `crates_io_endpoint` | `https://crates.io` | Air-gapped mirror |
| `github_endpoint` | `https://api.github.com` | GitHub Enterprise |
| `index_url` | `…/nexo-plugin-index/main/index.json` | Operator's own curated list |
| `http_timeout` | 10 seconds | Slow link tolerance |
| `official_owners` | `["lordmacu", "nexo-rs"]` | Per-tenant allowlist |
| `daemon_version` | `CARGO_PKG_VERSION` | Test compat against an older or newer host |
| `github_token` | `None` | Lift unauth GitHub rate limit |

## What goes live on install (no restart)

*Phase 97.1.γ.* Installing a plugin (`nexo/admin/plugins/install` →
auto-`scan`, or `nexo/admin/plugins/scan`) registers its contributions
into the running daemon **without a restart**:

| Contribution | Live on install? |
|---|---|
| Tools (`extends.tools`) | ✅ into the shared `ToolRegistry` |
| Channels, hooks, LLM providers, vector backends | ✅ |
| Admin / poller routes, pairing triggers | ✅ |
| **HTTP routes** (`[plugin.http]`) | ✅ the route table is interior-mutable; install adds + uninstall drops |
| **Skills** (`[plugin.skills] contributes_dir`) | ✅ re-walked + merged into the shared live handle every agent reads next turn |
| **Metrics** (`[plugin.metrics]`) | ✅ appended to the live descriptor list the `/metrics` scrape reads |
| **Registry snapshot** (admin-UI / discovery / capabilities) | ✅ swapped so a hot-installed plugin's `[plugin.admin_ui]` screen + catalogue entry appear live |

Enabling/disabling (`set_enabled`) and uninstalling apply the inverse
live — every contribution above is added on enable/install and dropped
on disable/uninstall.

**Plugin skills in the admin UI.** Plugin-contributed skills appear in
the **Skills** screen alongside operator skills, tagged with a
`from plugin <id>` badge (the `skills/list` RPC carries a
`source_plugin` field). They are read-only there — remove them by
uninstalling the plugin. Operator skills win on a name collision.

Still restart-bound: plugin-contributed **agents** (`[plugin.agents]` —
no plugin ships these yet, so the hot-spawn path is unbuilt). Setup-
wizard channel **dashboards** are not affected — they re-read from disk
on every `nexo setup` run, so a hot-installed plugin shows up on the
next invocation without any restart.

## Architecture pointers

- `crates/plugin-discovery/` — standalone publishable crate.
  Sources + cache + manifest fetcher + compat + merge + client.
- `crates/tool-meta/src/admin/plugin_discovery.rs` — wire shapes
  shared with the admin frontend.
- `crates/core/src/agent/admin_rpc/domains/plugin_discovery.rs` —
  3 admin RPC handlers + `DiscoveryReader` trait.
- `crates/setup/src/discovery_adapter.rs` — production adapter
  bridging the daemon's RPC layer to the standalone crate.
- `src/plugin_install_adapter.rs` — Phase 97.1 install pipeline
  that consumes a `DiscoveredPlugin.install_params` pre-fill from
  the catalogue click in the admin UI.

For the design rationale + race-condition fixes surfaced in the
audit phase see
[`Plugin install pipeline audit`](../architecture/plugin-install-audit-2026-05-19.md)
and
[`Plugin discovery architecture`](../architecture/plugin-discovery.md).
