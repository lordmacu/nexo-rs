# Pairing protocol

Two coexisting protocols ship in `nexo-pairing`:

- **DM-challenge inbound gate** — opt-in per binding. Unknown senders
  on WhatsApp / Telegram receive a one-time human-friendly code; the
  operator approves them via CLI. Existing senders pass through
  unchanged.
- **Setup-code QR** — operator-initiated. `nexo pair start` issues a
  short-lived HMAC-signed bearer token + a gateway URL, packs them
  into a base64url payload, and renders a QR. A companion app scans,
  presents the token to the daemon, and gets a session token in
  return.

The feature is **off by default**. Existing setups see no behaviour
change until the operator flips `pairing_policy.auto_challenge` on a
binding.

## DM-challenge gate

### Per-binding config

```yaml
# config/agents.yaml
agents:
  - id: ana
    inbound_bindings:
      - plugin: whatsapp
        instance: personal
        pairing_policy:
          auto_challenge: true   # default false
```

The gate runs *before* the plugin publishes to the broker. Three
outcomes per inbound message:

| Outcome | When | Plugin action |
|---|---|---|
| `Admit` | sender in `pairing_allow_from` (or policy off) | publish as normal |
| `Challenge { code }` | unknown sender, `auto_challenge: true`, slot free | reply with code, drop message |
| `Drop` | max-pending exhausted (3 per channel/account) | silent drop |

### Operator workflow

```
$ nexo pair list
CODE       CHANNEL         ACCOUNT          CREATED                     SENDER
K7M9PQ2X   whatsapp        personal         2026-04-25T13:21:00Z        +57311...

$ nexo pair approve K7M9PQ2X
Approved whatsapp:personal:+57311... (added to allow_from)
```

The next message from `+57311...` admits through the gate.

### Cache + revoke

The gate caches decisions for 30 s to keep SQLite off the hot path.
Revokes are eventually consistent within that window:

```
$ nexo pair revoke whatsapp:+57311...
Revoked whatsapp:+57311...
```

For an immediate effect, restart the daemon (or call the gate's
`flush_cache()` from a future admin endpoint).

### Migrating an existing bot

If you already have known senders, seed them so the gate doesn't
challenge mid-conversation when you flip `auto_challenge: true`:

```
$ nexo pair seed whatsapp personal +57311... +57222... +57333...
Seeded 3 sender(s) into whatsapp:personal allow_from
```

`seed` is idempotent; running it twice is safe and re-activates any
sender that was previously revoked.

## Setup-code QR

### Issuing

```
$ nexo pair start --public-url wss://nexo.example.com --qr-png /tmp/p.png --json
{
  "url": "wss://nexo.example.com",
  "url_source": "pairing.public_url",
  "bootstrap_token": "eyJwcm9maWxlIjoi...",
  "expires_at": "2026-04-25T13:32:00Z",
  "payload": "eyJ1cmwi..."
}
```

`payload` is what goes in the QR. The companion decodes it to recover
`{url, bootstrap_token, expires_at}`, opens the WebSocket, and
presents the token as `Authorization: Bearer <bootstrap_token>`.

### URL resolution

Priority chain (first non-empty wins):

1. `--public-url` (CLI flag)
2. `tunnel.url` (Phase tunnel — TODO: wire when accessor lands)
3. `gateway.remote.url`
4. LAN bind address (when `gateway.bind=lan`)
5. **fail-closed**: the daemon refuses to issue a code on a
   loopback-only gateway

### ws/wss security policy

Cleartext `ws://` is allowed only on hosts the operator can
reasonably trust to be private:

- `127.0.0.1` / `::1` (loopback)
- RFC1918 (10/8, 172.16/12, 192.168/16)
- link-local (169.254/16)
- `*.local` mDNS hostnames
- `10.0.2.2` (Android emulator)
- Any host listed in `pairing.ws_cleartext_allow_extra`

Everything else exigirá `wss://`. This matches OpenClaw's posture in
`research/src/pairing/setup-code.ts`.

### Token format

```
b64u(claims_json) + "." + b64u(hmac_sha256(secret, claims_json))
```

- `claims_json` = `{"profile":"companion-v1","expires_at":"...","nonce":"<32 hex>","device_label":"..."}`
- `secret` = 32 bytes in `~/.nexo/secret/pairing.key` (auto-generated
  on first boot with 0600 perms; rotate by deleting + restarting).

Verification is constant-time (`subtle` crate) so timing leaks don't
discriminate between "wrong sig" and "wrong claims".

## Threat model

| Concern | Mitigation |
|---|---|
| Brute-force pairing code | 32^8 ≈ 10^12 keyspace; 60 min TTL; max 3 pending per (channel, account) |
| Token replay after expiry | TTL on `expires_at` (default 10 min); HMAC verify fails closed |
| Token forgery | HMAC-SHA256 with 32-byte secret; constant-time compare |
| Secret leak | Rotate via `rm ~/.nexo/secret/pairing.key && restart`; all in-flight tokens invalidate |
| TOCTOU on approve | Single SQL transaction (`approve` reads + insert + delete in one tx) |
| ws cleartext on hostile network | Refuse to issue cleartext URL outside private-host allowlist |
| DoS via flood of pending requests | Max 3 per (channel, account); TTL 60 min auto-prunes |

## Storage layout

Two SQLite tables in `<memory_dir>/pairing.db`:

```sql
pairing_pending (channel, account_id, sender_id PRIMARY KEY,
                 code, created_at, meta_json)

pairing_allow_from (channel, account_id, sender_id PRIMARY KEY,
                    approved_at, approved_via, revoked_at)
```

Soft-delete (`revoked_at`) keeps historical context: an operator can
later see "+57311 was approved on X, revoked on Y" for audit.

## When to leave it off

- Single-user setups where the operator is the only sender — the
  gate adds a SQL hit per message for no security gain.
- Bots that take public input by design (e.g. a self-service support
  bot) — the gate would block every customer.
- Until you have an `agent setup web-search`-style wizard, manual
  `pair seed` is the only friendly migration path.

## Adapter registry

Each channel that participates in pairing implements
`PairingChannelAdapter` in its plugin crate. The adapter owns three
channel-specific decisions the runtime cannot make on its own:

- **`normalize_sender(raw)`** — canonicalise inbound sender ids before
  the gate hits the store. WhatsApp strips `@c.us` /
  `@s.whatsapp.net` and prepends `+`; Telegram lower-cases `@username`
  and passes numeric chat ids through.
- **`format_challenge_text(code)`** — render the operator-facing
  pairing message. The default is plain UTF-8; the Telegram adapter
  overrides it to escape MarkdownV2 reserved characters and wrap the
  code in backticks so the user can long-press to copy.
- **`send_reply(account, to, text)`** — publish the challenge through
  the channel's outbound topic
  (`plugin.outbound.{whatsapp,telegram}[.<account>]`) using the
  payload shape that channel's dispatcher expects.

The bin (`src/main.rs`) constructs a `PairingAdapterRegistry` at boot
and registers the WhatsApp + Telegram adapters. The runtime consults
the registry on every inbound event whose binding has
`pairing.auto_challenge: true`. Channels with **no** registered
adapter fall back to a hardcoded broker publish that mirrors the
legacy text on `plugin.outbound.{channel}` — operators still see the
challenge in their channel, but without per-channel formatting.

Telemetry lives under
`pairing_inbound_challenged_total{channel,result}` with `result` one of
`delivered_via_adapter`, `delivered_via_broker`, `publish_failed`,
`no_adapter_no_broker_topic`, so dashboards can split adapter vs.
fallback delivery rates per channel.

## CLI reference

```
nexo pair start [--for-device <name>] [--public-url <url>]
                 [--qr-png <path>] [--ttl-secs <n>] [--json]
nexo pair list  [--channel <id>] [--json]
nexo pair approve <CODE> [--json]
nexo pair revoke <channel>:<sender_id>
nexo pair seed <channel> <account_id> <sender_id> [<sender_id>...]
nexo pair help
```
