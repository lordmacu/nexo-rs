# Agent drop-in directory

This directory holds **per-agent YAML drop-ins** that the daemon
loads at boot and merges with the base `config/agents.yaml`. Useful
for keeping business-sensitive agent definitions (custom system
prompts, pricing scripts, internal contacts) out of the main config
file — the parent `.gitignore` excludes `*.yaml` here by default,
preserving privacy.

`*.example.yaml` files are exceptions to the ignore (they document
the shape without leaking real values) — see `ana.example.yaml`
and `marketing.multiclient.example.yaml` for templates.

## Out-of-tree agent personas

Some agents live in their own sibling repos as **persona packs** so
multiple operators can share them without forking the framework:

### Cody — programmer pair

The `cody` agent (programmer pair driving Claude Code goals from
chat — Telegram + WhatsApp) lives at:

  https://github.com/lordmacu/nexo-persona-cody

Install:

```bash
git clone https://github.com/lordmacu/nexo-persona-cody ~/chat/nexo-persona-cody
cd ~/chat/nexo-persona-cody
./install.sh
```

The installer drops `cody.yaml` into this directory and merges the
`cody_nexo_bot` Telegram block into `config/plugins/telegram.yaml`.
See the persona pack's README for full setup (bot token, Anthropic
credentials, pairing flow).

## Why is the persona out-of-tree?

Per the framework's "framework agnostic, microapps drive config"
rule, agent definitions are operator-owned config — not framework
artefacts. Sibling-repo distribution lets:

- Operators install Cody in any nexo-rs deployment without forking
  the framework.
- Future personas (e.g. `claudia`, `frida`, ...) ship via the same
  `persona.toml` + `install.sh` pattern.
- The framework crates stay generic (no `agent_id == "cody"`
  branches anywhere — verified by the 2026-05-11 mapping audit).
