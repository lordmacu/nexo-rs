# ADR 0007 — WhatsApp via whatsapp-rs (Signal Protocol)

**Status:** Accepted
**Date:** 2026-02

## Context

"Add WhatsApp support" has three common paths:

1. **Official WhatsApp Business API** — rate-limited, costs per
   message, requires business verification, limits proactive outreach
   to approved templates. Fine for some deployments, a bad fit for
   "run an agent on your personal number for a small business."
2. **Unofficial web-scraping libraries** (e.g. `whatsapp-web.js`) —
   pretend to be a browser, fragile against UI changes, frequently
   banned
3. **Signal Protocol reimplementation** — speak the native protocol
   that the WhatsApp mobile app speaks. Stable, fast, no
   scraping, permits all message types (voice, media, reactions,
   edits, etc.)

## Decision

Use **`whatsapp-rs`** (Cristian's crate) which implements the Signal
Protocol handshake + pairing + message layer in Rust. nexo-rs wraps
it in `crates/plugins/whatsapp`:

- **Pairing:** setup-time QR scan via `Client::new_in_dir()` — the
  wizard creates a per-agent session dir and renders the QR as
  Unicode blocks
- **Runtime:** the plugin subscribes to inbound messages, forwards
  to `plugin.inbound.whatsapp[.<instance>]`, handles the outbound
  side via the tool family (`whatsapp_send_message`,
  `whatsapp_send_reply`, `whatsapp_send_reaction`, `whatsapp_send_media`)
- **Credentials expiry:** the plugin does **not** fall back to a
  runtime QR on 401 — the operator must re-pair via the wizard. The
  runtime refuses to boot without valid creds. This is a deliberate
  safety net against silent re-pair loops that would cross-deliver
  to the wrong account
- **Multi-account:** each agent points at its own session dir. No
  XDG_DATA_HOME mutation

## Consequences

**Positive**

- Full feature coverage (voice, media, reactions, edits, groups)
- No per-message cost beyond the bandwidth
- No business-verification paperwork
- Works on a personal number, a secondary SIM, anything you can pair
  to WhatsApp's Linked Devices

**Negative**

- Signal Protocol parity is non-trivial; keeping up with WhatsApp
  protocol evolution is an ongoing commitment of `whatsapp-rs`
- **Running an agent on a personal number is a policy choice.**
  WhatsApp's Terms of Service don't love automated accounts; use
  `whatsapp-rs` on numbers you own and are ready to re-pair if they
  get banned
- Multi-account needs careful session-dir management — see
  [Plugins — WhatsApp gotchas](../plugins/whatsapp.md#gotchas)

**Forbidden alternatives**

- **Puppeteer / whatsapp-web.js / selenium** — pulls the entire
  Chromium runtime into the process, breaks constantly, and is
  detected and banned faster than the Signal Protocol path
- **Business API** — only if the deployment pays for it and the
  agent flow survives template constraints; ship a separate plugin
  if this comes up

## Related

- `../whatsapp-rs/` sibling crate (Signal Protocol + pairing +
  Client)
- [Plugins — WhatsApp](../plugins/whatsapp.md)
- [Recipes — WhatsApp sales agent](../recipes/whatsapp-sales-agent.md)
