# Plugin catalog

Ten plugins maintained out-of-tree on their own GitHub repos. One
command — `nexo plugin install <owner>/<repo>` — grabs the release
tarball and drops it under `plugins.discovery.search_paths`. Or
build from source via `cargo install <crate>`. Both paths land the
same binary; the daemon discovers it via the
[`[plugin.*]` manifest sections](./manifest-unified.md).

## Channels (5)

| Plugin | Install | Provider docs |
|--------|---------|---------------|
| WhatsApp | `nexo plugin install lordmacu/nexo-plugin-whatsapp` | [→ WhatsApp](./whatsapp.md) |
| Telegram | `nexo plugin install lordmacu/nexo-plugin-telegram` | [→ Telegram](./telegram.md) |
| Email (IMAP/SMTP) | `nexo plugin install lordmacu/nexo-plugin-email` | [→ Email](./email.md) |
| Browser (CDP) | `nexo plugin install lordmacu/nexo-plugin-browser` | [→ Browser](./browser.md) |
| Google (OAuth + Gmail + Calendar + Drive) | `nexo plugin install lordmacu/nexo-rs-plugin-google` | [→ Google](./google.md) |

## Tools (1)

| Plugin | Install | Provider docs |
|--------|---------|---------------|
| Web Search (Brave · Tavily · DDG · Perplexity) | `nexo plugin install lordmacu/nexo-rs-plugin-web-search` | [→ Web Search](./web-search.md) |

## Pollers (3)

Cron-style scheduled tasks dispatched as agent prompts. Architecture:
[Poller v2](../architecture/poller-v2.md).

| Plugin | Install | Provider docs |
|--------|---------|---------------|
| RSS / Atom | `nexo plugin install lordmacu/nexo-rs-poller-rss` | [→ Poller · RSS](./poller-rss.md) |
| Gmail | `nexo plugin install lordmacu/nexo-rs-poller-gmail` | [→ Poller · Gmail](./poller-gmail.md) |
| Google Calendar | `nexo plugin install lordmacu/nexo-rs-poller-google-calendar` | [→ Poller · Google Calendar](./poller-google-calendar.md) |

## Ops (1)

| Plugin | Install | Provider docs |
|--------|---------|---------------|
| Admin (RPC + React UI) | `nexo plugin install lordmacu/nexo-rs-plugin-admin` | [GitHub repo →](https://github.com/lordmacu/nexo-rs-plugin-admin) |

## Build your own

The same JSON-RPC subprocess wire contract every official plugin
above speaks is documented in [Plugin contract](./contract.md).
Pick a language SDK ([Rust](./rust-sdk.md), [Python](./python-sdk.md),
[TypeScript](./typescript-sdk.md), [PHP](./php-sdk.md)) and ship a
single `noarch` tarball.

See [Quickstart (10 min)](./quickstart.md) to scaffold a plugin
from zero.

## Platform support

Every official plugin cross-compiles to 5 targets via
`cargo-zigbuild`:

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Linux musl tarballs are static (no glibc, runs on Alpine + scratch).
All plugins use `rustls` (no OpenSSL) so they cross-compile cleanly
to Android NDK / Termux when the daemon does.
