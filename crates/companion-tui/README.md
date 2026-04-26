# nexo-companion-tui

Reference companion CLI for the [Nexo](https://github.com/lordmacu/nexo-rs)
pairing protocol (Phase 26).

This crate is intentionally tiny: it decodes the setup-code
payload that `nexo pair start` emits, validates the embedded
token expiry, and prints what a full companion would do next.
The actual WebSocket handshake is deferred to a follow-up;
this scaffold proves the wire format works end-to-end across a
non-`nexo-pairing` consumer.

- **Main repo:** <https://github.com/lordmacu/nexo-rs>
- **Runtime engine:** [`nexo-core`](https://github.com/lordmacu/nexo-rs/tree/main/crates/core)
- **Public docs:** <https://lordmacu.github.io/nexo-rs/>
- **Pairing docs:** <https://lordmacu.github.io/nexo-rs/ops/pairing.html>

## What this crate does

- Single binary `nexo-companion` that consumes the base64url
  setup-code payload from `nexo pair start`.
- Decodes via `nexo_pairing::setup_code::decode_setup_code`
  (same library the daemon uses to emit it — the contract is
  enforced by both sides).
- Validates the embedded bootstrap-token expiry.
- Prints the URL + redacted token + next-step instructions in
  pretty or `--json` form.

## What it does NOT do (yet)

Tracked under FOLLOWUPS PR-4 in the main repo:

- Open the WebSocket against `payload.url`.
- Present the bootstrap token in the first frame.
- Receive + persist a session token.
- Re-render the QR for hands-off scanning workflows.

## Install

```toml
[dependencies]
nexo-companion-tui = "0.1"
```

Or build the binary from a checkout:

```bash
cargo install --path crates/companion-tui
```

## Usage

```bash
# Explicit
nexo-companion --code <BASE64URL>

# Stdin (e.g. piped from a QR scanner)
echo <BASE64URL> | nexo-companion

# Machine-readable
nexo-companion --code <BASE64URL> --json
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/lordmacu/nexo-rs/blob/main/LICENSE-MIT))

at your option.
