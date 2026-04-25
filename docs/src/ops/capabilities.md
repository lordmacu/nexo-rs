# Capability toggles

Several bundled extensions ship with **dangerous capabilities off
by default** — write paths, secret reveal, cache purges. Each
capability is gated by a single environment variable. The operator
flips it on by exporting the var in the agent process's environment.

`agent doctor capabilities` enumerates every known toggle, its
current state, and a hint for enabling it.

```sh
$ agent doctor capabilities
Capability toggles
──────────────────────────────────────────────────────────────────
EXT          ENV VAR                       STATE     RISK     EFFECT
onepassword  OP_ALLOW_REVEAL               disabled  HIGH     Reveal raw secret values…
onepassword  OP_INJECT_COMMAND_ALLOWLIST   disabled  HIGH     Allow `inject_template` to pipe…
cloudflare   CLOUDFLARE_ALLOW_WRITES       disabled  HIGH     Create / update / delete DNS…
cloudflare   CLOUDFLARE_ALLOW_PURGE        disabled  CRITICAL Purge zone cache…
docker-api   DOCKER_API_ALLOW_WRITE        disabled  HIGH     Start / stop / restart…
proxmox      PROXMOX_ALLOW_WRITE           disabled  CRITICAL VM / container lifecycle…
ssh-exec     SSH_EXEC_ALLOWED_HOSTS        disabled  HIGH     Allow `ssh_run` against…
ssh-exec     SSH_EXEC_ALLOW_WRITES         disabled  CRITICAL Allow `scp_upload`…
```

Pass `--json` for machine-readable output (admin UI, dashboards):

```sh
agent doctor capabilities --json
```

## Toggle reference

| Env var | Extension | Kind | Risk | Effect |
|---------|-----------|------|------|--------|
| `OP_ALLOW_REVEAL` | onepassword | bool | high | Returns secret values verbatim instead of fingerprints |
| `OP_INJECT_COMMAND_ALLOWLIST` | onepassword | allowlist | high | Enables `inject_template` exec mode for the listed commands |
| `CLOUDFLARE_ALLOW_WRITES` | cloudflare | bool | high | Authorizes `create_dns_record`, `update_dns_record`, `delete_dns_record` |
| `CLOUDFLARE_ALLOW_PURGE` | cloudflare | bool | critical | Authorizes `purge_cache` |
| `DOCKER_API_ALLOW_WRITE` | docker-api | bool | high | Authorizes `start_container`, `stop_container`, `restart_container` |
| `PROXMOX_ALLOW_WRITE` | proxmox | bool | critical | Authorizes VM/container lifecycle actions |
| `SSH_EXEC_ALLOWED_HOSTS` | ssh-exec | allowlist | high | Hosts the agent may target with `ssh_run` |
| `SSH_EXEC_ALLOW_WRITES` | ssh-exec | bool | critical | Authorizes `scp_upload` |

**Boolean kinds** accept `true`, `1`, or `yes` (case-insensitive).
Anything else — including unset — counts as disabled.

**Allowlist kinds** are comma-separated. Empty / whitespace-only
inputs count as disabled. The agent never falls back to "anything
goes" when the variable is unset.

## When to enable

The default is **off** because every toggle moves the agent from
"informational" to "consequential" — failures are no longer just a
bad reply, they can mutate real systems or leak secrets.

Enable a toggle only when:

1. The agent will provably need that capability for the next session.
2. The operator (you) is present and the session is observed.
3. There is a way to revert quickly — a wrapper script, a per-shell
   `.envrc`, or a systemd unit drop-in you can comment out.

Avoid enabling toggles globally in `~/.profile`. Scope them to the
specific shell or systemd unit that runs the agent.

## How to revoke

- Boolean: `unset CLOUDFLARE_ALLOW_WRITES` (or restart the shell /
  service).
- Allowlist: `unset OP_INJECT_COMMAND_ALLOWLIST` to disable, or
  `export OP_INJECT_COMMAND_ALLOWLIST=` (empty string) to keep the
  intent visible while still treating the feature as disabled.

The agent reads these on each call (no caching), so revocation is
immediate without a restart for most paths. The single exception is
`OP_INJECT_COMMAND_ALLOWLIST` reading happens at tool-call time, not
extension-spawn time, so it also picks up changes live.

## Adding a new toggle

When a future extension introduces a new write/reveal env var, add a
matching `CapabilityToggle` to
`crates/setup/src/capabilities.rs::INVENTORY`. Without that entry,
`agent doctor capabilities` is silently incomplete — the inventory
is the operator-facing source of truth.
