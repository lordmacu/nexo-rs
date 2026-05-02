# Plan — 83.8.4.b production `ChannelOutboundDispatcher` adapter

**Status:** plan (post `/forge spec 83.8.4.b`).
Spec: `proyecto/spec-83.8.4.b-channel-outbound-adapter.md`.

## Mining (regla irrompible)

- `research/extensions/whatsapp/src/connection-controller.ts:20` —
  shape de send TS, confirma per-channel translator approach.
- `research/src/channels/conversation-binding-context.ts:1` —
  binding-id render shape; equivalente Rust ya en Phase 82.1.
- `claude-code-leak/` — **ausente** (`ls` retorna "No such file").
  Cumplido el reporte por la regla.

In-repo source-of-truth (5 cites — la lib más útil esta vez no es
research, es el propio repo):

- `crates/plugins/whatsapp/src/dispatch.rs:31,40-60` (topic +
  payload shape).
- `crates/plugins/whatsapp/src/pairing_adapter.rs:25` (account
  suffix).
- `crates/broker/src/handle.rs:25` (publish signature).
- `crates/setup/src/admin_bootstrap.rs:135-164` (Inputs shape
  to extend).
- `crates/core/src/agent/admin_rpc/channel_outbound.rs:1-100`
  (trait shape from 83.8.4.a).

## Methodology — stress-test recap

Esta sub-fase cierra el gap framework anotado en 83.8.4.a
(`channel_outbound dispatcher not configured`). No expansión de
scope: solo la implementación que las plumbing-trait esperan.
Cualquier fricción nueva durante ejecución → fix framework
agnostic + log follow-up. NUNCA hackear el microapp para
trabajar sin el adapter.

## Numbering

3 atomic sub-steps + close-out:

```
83.8.4.b.1  Adapter — ChannelPayloadTranslator + WhatsAppTranslator + BrokerOutboundDispatcher + unit tests
83.8.4.b.2  Boot wire — AdminBootstrapInputs.broker + dispatcher.with_channel_outbound + main.rs
83.8.4.b.3  Integration test + docs + close-out (FOLLOWUPS resolved + CHANGELOG)
```

3 commits. Bounded.

---

## 83.8.4.b.1 — Adapter shipped

**Files NEW:**

- *(none — extends `admin_adapters.rs`)*

**Files MODIFIED:**

- `crates/setup/src/admin_adapters.rs` — append:
  - `pub use` re-export of the WhatsApp topic constant from
    `crates/plugins/whatsapp/src/dispatch.rs::TOPIC_OUTBOUND`
    so the translator and the plugin agree on the literal.
  - `pub trait ChannelPayloadTranslator` (signature in spec §4.1).
  - `pub enum TranslationError` (3 variants per spec §4.1).
  - `pub struct WhatsAppTranslator` impl (spec §4.3 — handles
    `text` / `reply` / `media`, rejects others).
  - `pub struct BrokerOutboundDispatcher` impl
    (`ChannelOutboundDispatcher`).
- `crates/plugins/whatsapp/src/dispatch.rs` — promote
  `TOPIC_OUTBOUND` from `pub const` to `pub const` re-exported
  via `crates/plugins/whatsapp/src/lib.rs` so `nexo-setup` can
  import it (today it's `pub` but lib.rs may not re-export).
- `crates/setup/Cargo.toml` — already has
  `nexo-plugin-whatsapp` path-dep; nothing to add. Verify.

**Wire-up topic constants:**

```rust
// In admin_adapters.rs:
use nexo_plugin_whatsapp::dispatch::TOPIC_OUTBOUND as WHATSAPP_TOPIC;
```

This is the R5 mitigation from the spec — single source of truth
for the topic literal.

**Unit tests (in admin_adapters.rs `#[cfg(test)]` module):**

- `whatsapp_translator_text_round_trips`: `OutboundMessage{
  channel:"whatsapp", account_id:"wa.0", to:"wa.55", body:"hi",
  msg_kind:"text" }` → `("plugin.outbound.whatsapp.wa.0",
  json!({"to":"wa.55","text":"hi","kind":"text"}))`.
- `whatsapp_translator_reply_includes_msg_id`.
- `whatsapp_translator_reply_missing_msg_id_errors_with_missing_field`.
- `whatsapp_translator_media_reads_first_attachment`.
- `whatsapp_translator_media_missing_url_errors`.
- `whatsapp_translator_unknown_kind_errors_with_unsupported`.
- `whatsapp_translator_default_account_uses_base_topic`
  (`account_id == "default"` AND `account_id == ""` both produce
  `plugin.outbound.whatsapp` no suffix).
- `broker_dispatcher_routes_known_channel_to_translator`
  (in-process `LocalBroker`, capturing subscriber asserts the
  payload landed).
- `broker_dispatcher_unknown_channel_returns_channel_unavailable`
  (translates(...) for `channel:"telegram"` returns the typed
  error from `ChannelOutboundDispatcher::send`).
- `broker_dispatcher_publish_failure_returns_transport_error`
  (mock broker that errors on publish; assert the
  `Transport(_)` variant).

**Done:**

- `cargo build -p nexo-setup` clean.
- `cargo test -p nexo-setup admin_adapters::*outbound*` 9
  passing.
- `cargo build --workspace` clean (no other crate breaks from
  the topic re-export tweak).

---

## 83.8.4.b.2 — Boot wire

**Files MODIFIED:**

- `crates/setup/src/admin_bootstrap.rs`:
  - Extend `AdminBootstrapInputs<'a>` with
    `pub broker: Option<AnyBroker>`.
  - In `build_inner`, after the `with_channels_domain()` call:

    ```rust
    if let Some(broker) = inputs.broker.clone() {
        let outbound = Arc::new(
            BrokerOutboundDispatcher::new(broker)
                .with_translator(Box::new(WhatsAppTranslator)),
        );
        dispatcher = dispatcher.with_channel_outbound(outbound);
    }
    ```

  - Update existing `AdminBootstrapInputs` struct literals in the
    same file (test setups around line 542 / 566 / 591 / 630 /
    662 / 693). Each gets `broker: None`.
- `src/main.rs` (workspace root binary) — locate the
  `AdminBootstrapInputs { ... }` site and pass
  `broker: Some(broker.clone())`.

**Tests:**

- Existing admin_bootstrap tests still pass with `broker: None`
  default.
- Add one test: `bootstrap_with_broker_attaches_channel_outbound`
  — stub broker, confirm the resulting `AdminRpcDispatcher` has
  `channel_outbound` set (via a public-test accessor or
  by issuing a mocked `processing.intervention` call that
  reaches the translator).

**Done:**

- `cargo build --workspace` clean.
- `cargo test -p nexo-setup admin_bootstrap` passing (existing +
  new).
- `cargo run --bin agent --help` smoke (boot wires correctly
  without crashing).

---

## 83.8.4.b.3 — Integration test + docs + close-out

**Files MODIFIED:**

- `crates/setup/tests/` — new integration test
  `channel_outbound_end_to_end.rs`:
  - Spin up `LocalBroker`.
  - Build `BrokerOutboundDispatcher` + `WhatsAppTranslator`.
  - Subscribe to `plugin.outbound.whatsapp.test-acct` with a
    `tokio::sync::mpsc::Sender` capturing payload.
  - Call dispatcher.send(`OutboundMessage{ channel:"whatsapp",
    account_id:"test-acct", to:"wa.42", body:"end-to-end",
    msg_kind:"text" }`).
  - Assert the subscriber received the matching JSON within
    100 ms.
- `proyecto/FOLLOWUPS.md`:
  - Move `Phase 83.8.4.b — production ChannelOutboundDispatcher
    adapter` block from "Open items" to "Resolved" with date.
  - Add a fresh follow-up entry: "83.8.4.c — outbound_message_id
    correlation ack flow" + "83.8.4.b.tg — Telegram translator"
    + "83.8.4.b.em — Email translator".
- `proyecto/CHANGELOG.md` — entry under `[Unreleased]`:
  `feat(83.8.4.b): production ChannelOutboundDispatcher adapter
  (broker-publish + WhatsApp translator)`.
- `docs/src/microapps/agent-creator.md` — replace the line that
  says "Production adapter that bridges to plugin send paths is
  deferred-83.8.4.b" with the shipped reference + link to the
  spec. The takeover_send paragraph now reads "flows through the
  `BrokerOutboundDispatcher` published in Phase 83.8.4.b".
- `proyecto/PHASES-microapps.md` — under Phase 83.8 row, expand
  the `83.8.4.b` line to ✅ with the same delta the other steps
  have.

**Tests:**

- The integration test above runs in `cargo test -p nexo-setup
  --test channel_outbound_end_to_end`.

**Done:**

- `cargo build --workspace` clean.
- `cargo test --workspace` passing.
- `mdbook build docs` clean.
- `git log --oneline | head -5` shows the 3 commits with
  conventional-commit prefixes.

---

## Risks

- **R1 — `nexo-plugin-whatsapp` cycle**: `nexo-setup` already
  depends on `nexo-plugin-whatsapp` (path-dep visible in
  Cargo.toml) so importing `TOPIC_OUTBOUND` introduces no new
  cycle. Verified before commit 1.
- **R2 — main.rs broker availability ordering**: the broker is
  constructed early in `main.rs` (Phase 80.x boot order). Admin
  bootstrap runs after broker init. No reorder needed.
  Verify by reading `main.rs` before commit 2.
- **R3 — Existing `AdminBootstrapInputs` literals breaking**:
  workspace-wide grep returns 6 sites in admin_bootstrap.rs
  tests + 1 in main.rs. Each gets the `broker: None` (or
  `broker: Some(broker.clone())` for main). No-op for tests.
- **R4 — Integration test flakiness**: in-process broker is
  deterministic; subscriber loop uses `mpsc` with explicit
  `recv().await`. 100 ms timeout is generous. Mitigation:
  if flaky, switch to `tokio::sync::Notify` instead of mpsc.
- **R5 — Concurrent translator construction**: `BrokerOutbound
  Dispatcher` construction is sync; the `HashMap<String, Box<dyn
  ...>>` build is single-threaded at boot. No `Arc<RwLock>`
  needed.

## Order of execution

```
83.8.4.b.1  → 83.8.4.b.2  → 83.8.4.b.3
```

No interleaving. Each commit must `cargo build --workspace` clean
before the next.

---

**Listo para `/forge ejecutar 83.8.4.b-channel-outbound-adapter`.**
