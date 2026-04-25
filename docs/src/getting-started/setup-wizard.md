# Setup wizard

The setup wizard is the recommended way to configure nexo-rs on a fresh
install. It pairs channels, writes secrets, and patches the YAML
config files so the runtime boots with everything it needs.

```bash
./target/release/agent setup
```

Run it from the repo root (or wherever your `config/` directory lives).

## What the wizard does

```mermaid
flowchart TD
    START([agent setup]) --> MENU{Menu}
    MENU --> LLM[LLM provider]
    MENU --> WA[WhatsApp pairing]
    MENU --> TG[Telegram bot]
    MENU --> GOOG[Google OAuth]
    MENU --> MEM[Memory DB location]
    MENU --> INFRA[NATS + runtime]
    MENU --> SKILLS[Enable / disable skills]

    LLM --> WRITE1[Write secrets/<br/>patch llm.yaml]
    WA --> QR[Scan QR<br/>write session dir]
    TG --> TOKEN[Ask bot token<br/>write secret]
    GOOG --> OAUTH[Open browser<br/>PKCE flow]
    MEM --> WRITE2[Patch memory.yaml]
    INFRA --> WRITE3[Patch broker.yaml]
    SKILLS --> WRITE4[Patch extensions.yaml]

    WRITE1 --> DONE([Done])
    QR --> DONE
    TOKEN --> DONE
    OAUTH --> DONE
    WRITE2 --> DONE
    WRITE3 --> DONE
    WRITE4 --> DONE
```

Every step is optional. You can run `setup` repeatedly — each section
is idempotent.

## Steps in detail

### LLM provider

Prompts for the default provider (MiniMax, Anthropic, OpenAI-compat,
Gemini). Writes the API key to `./secrets/<provider>_api_key.txt` and
ensures `config/llm.yaml` references it via `${file:...}` or the
corresponding env var.

### WhatsApp pairing (multi-instance)

Per-agent. Asks which agent you are pairing and which instance label
to use (`personal`, `work`, …). Each instance gets its own session
dir under `./data/workspace/<agent>/whatsapp/<instance>` and an
`allow_agents` list (defense-in-depth ACL). The wizard:

1. Normalises `config/plugins/whatsapp.yaml` to sequence form (legacy
   single-mapping entries are auto-converted on first edit).
2. Upserts the entry by instance label.
3. Writes `credentials.whatsapp: <instance>` on the chosen agent's
   YAML — `agents.yaml` if the agent lives there, otherwise the
   matching `agents.d/*.yaml`.
4. Launches the pairing loop and renders the QR as Unicode blocks.
   Scan with **WhatsApp → Settings → Linked Devices**.
5. Runs the credential gauntlet so any drift surfaces immediately.

Re-run the wizard once per number you want to pair; instance labels
are append-friendly.

### Telegram bot (multi-instance)

Same shape as WhatsApp. Asks for instance label (default
`<agent>_bot`) and bot token from @BotFather. Token lands at
`./secrets/<instance>_telegram_token.txt` with mode `0o600`; the
YAML references it via `${file:...}` so secrets never live in
`telegram.yaml` directly. Adds `credentials.telegram: <instance>`
on the agent.

### Google OAuth

The wizard writes one entry per agent in
`config/plugins/google-auth.yaml`:

```yaml
google_auth:
  accounts:
    - id: ana@google
      agent_id: ana
      client_id_path:     ./secrets/ana_google_client_id.txt
      client_secret_path: ./secrets/ana_google_client_secret.txt
      token_path:         ./secrets/ana_google_token.json
      scopes: [https://www.googleapis.com/auth/gmail.modify]
```

Two consent flows are offered after the YAML is written:

- **Device-code** (default — works headless / over SSH): the wizard
  prints `verification_url` + a 6-character `user_code`. Open the URL
  on **any** device, type the code, approve. The wizard polls
  `oauth2.googleapis.com/token` until approval and persists the
  refresh_token at `token_path` (mode `0o600`).
- **Skip and consent later** via the `google_auth_start` LLM tool —
  uses the loopback PKCE flow, requires a local browser.

Scopes are comma-separated at the prompt; defaults to
`gmail.modify`. Re-running with a different `id` adds a second
account; re-running with the same `id` overwrites in place.

### Memory DB location

Lets you pick where the SQLite long-term memory file lives. Default is
`./data/memory.db`. Per-agent isolation is on by default — each agent
gets its own DB file under its workspace.

### Infrastructure (NATS + runtime)

Asks for the NATS URL, optional user/password, and timeouts. Patches
`config/broker.yaml`.

### Skills on/off

Lets you selectively disable shipped extensions you don't plan to use
(reduces tool surface exposed to the LLM).

## Files the wizard touches

| Target | What it writes |
|--------|----------------|
| `config/llm.yaml` | Provider entries, base_url, auth mode |
| `config/plugins/whatsapp.yaml` | `session_dir`, `media_dir` |
| `config/plugins/telegram.yaml` | `token` (via `${file:...}`), allow-list |
| `config/plugins/google.yaml` | OAuth bundle path, scopes |
| `config/memory.yaml` | DB location |
| `config/broker.yaml` | NATS URL, creds |
| `config/extensions.yaml` | enabled/disabled list |
| `./secrets/*` | Plaintext secret files (gitignored) |

Every YAML patch preserves existing keys and comments via the
`yaml_patch` module — your hand edits survive.

## Re-running

Re-run `agent setup` as many times as you want. Paired channels are
detected and skipped unless you explicitly ask to re-pair. To wipe a
paired session:

```bash
./target/release/agent setup wipe whatsapp --agent ana
```

## Troubleshooting

- **WhatsApp QR expires too fast** → the QR refreshes every ~20s; the
  wizard re-renders. Scan from the phone with a stable network.
- **Google OAuth fails with `redirect_uri_mismatch`** → the wizard
  binds to `127.0.0.1:<port>`; make sure your OAuth client allows
  `http://127.0.0.1` as a redirect URI.
- **NATS unreachable** → the wizard will warn but still write config.
  The runtime's disk queue will drain once NATS comes back.
