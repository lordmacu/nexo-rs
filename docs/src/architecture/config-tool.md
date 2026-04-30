# ConfigTool — gated self-config (Phase 79.10)

The `Config` tool lets an agent read and propose changes to its
own YAML configuration from inside a chat-driven turn. The flow
is intentionally two-step (`propose` then `apply`) so a remote
operator can approve or reject the change with a regular message
on the same channel that originated the proposal — there is no
host `'ask'` permission prompt the way Claude Code's upstream shows
(`upstream agent CLI`).

## Cargo feature gate

ConfigTool ships behind the `config-self-edit` Cargo feature
(off by default). Build with:

```bash
cargo build --workspace --features config-self-edit
```

A binary distributed without the feature cannot expose the tool
even when an operator sets `config_tool.self_edit: true` in YAML
— the gate is a hard ship-control until security review.

## Per-agent YAML

```yaml
agents:
  - id: cody
    config_tool:
      self_edit: true            # default false; opt-in per agent
      allowed_paths:              # empty = every SUPPORTED_SETTINGS key
        - "model.model"
        - "language"
      approval_timeout_secs: 86400  # 24 h
```

`allowed_paths` is intersected with `SUPPORTED_SETTINGS` (12
keys at MVP: `model.{provider,model}`, `language`,
`system_prompt`, `heartbeat.{enabled,interval_secs}`,
`link_understanding.enabled`, `web_search.enabled`,
`lsp.{enabled,languages,idle_teardown_secs,prewarm}`) and then
filtered by the hard-coded denylist.

## Three operations

```json
{ "op": "read",    "key": "model.model" }
{ "op": "propose", "key": "model.model", "value": "claude-opus-4-7", "justification": "operator asked" }
{ "op": "apply",   "patch_id": "01J7HVK..." }
```

`op: read` is read-only and passes plan-mode gating. `propose`
and `apply` are mutating — plan-mode refuses them.

## Hard-coded denylist

`crates/setup/src/capabilities.rs::CONFIG_SELF_EDIT_DENYLIST`
holds 13 globs that the tool MUST NEVER touch:

| Glob | Intent |
|------|--------|
| `*_token`, `*_secret`, `*_password`, `*_key` | credential-shaped suffixes |
| `pairing.*` | pairing internals — touching these revokes the operator's grip |
| `capabilities.*` | cannot widen own capabilities |
| `mcp.servers.*.auth.*`, `mcp.servers.*.command` | MCP auth + spawn args (running arbitrary binaries via config self-edit is game-over) |
| `binding.*.role` | cannot self-promote to coordinator |
| `binding.*.plan_mode.*` | cannot drop plan-mode guardrails |
| `remote_triggers[*].url`, `remote_triggers[*].secret_env` | outbound webhook URLs + signing keys |
| `cron.user_max_entries` | operator-only |
| `agent_registry.store.*` | changing the store under a running goal is unsafe |

Source-of-truth lives in code, not YAML — a model that proposes
a patch widening the denylist cannot succeed because that's a
code change requiring review. Validation runs at BOTH `propose`
(early reject) and `apply` (defense-in-depth: the staging file
may have been edited externally between propose and apply).

## Approval flow

1. Model emits `Config { op: "propose", key, value, justification }`.
2. Handler validates the triple gate (capability on, key in
   `SUPPORTED_SETTINGS ∩ allowed_paths`, key NOT in denylist),
   runs the per-key validator, generates a `patch_id`, persists
   the proposal under `.nexo/config-proposals/<patch_id>.yaml`,
   parks an approval `oneshot` with the `ApprovalCorrelator`,
   writes a `proposed` row to the audit store, fires
   `notify_origin` on the binding's channel with:
   ```
   Operator: reply [config-approve patch_id=<id>]
   or [config-reject patch_id=<id> reason=...] within 24 h.
   ```
3. Operator replies with the bracketed command on the SAME
   `(channel, account_id)` that originated the proposal.
   Cross-binding messages are rejected (the entry stays parked).
4. Model emits `Config { op: "apply", patch_id }`. Handler
   awaits the correlator decision (Approved / Rejected /
   Expired):
   - **Approved**: snapshot `agents.yaml`, apply the patch,
     trigger Phase 18 hot-reload. On reload-Err: restore the
     snapshot + record `rolled_back`. On Ok: cleanup staging
     + record `applied`.
   - **Rejected**: record `rejected` with the reason.
   - **Expired** (24 h elapsed): record `expired`.

## Audit log

`<state_dir>/config_changes.db` stores every state transition
(idempotent on `(patch_id, status)`). Read-only LLM tool
`config_changes_tail { n: 20 }` (always available, regardless
of the Cargo feature) returns a markdown table suitable for the
agent's post-mortem.

Schema:
```
patch_id    TEXT
status      TEXT  -- proposed | applied | rolled_back | rejected | expired
binding_id  TEXT
agent_id    TEXT
op          TEXT  -- propose | apply | reject | expire
key         TEXT
value       TEXT  -- pre-redacted: secret-suffix paths render as "<REDACTED>"
error       TEXT
created_at  INTEGER
applied_at  INTEGER
PRIMARY KEY (patch_id, status)
```

## Plan-mode behaviour

`Config` is in `MUTATING_TOOLS` but the dispatcher inspects
`args.op` at call time. `op: "read"` short-circuits the gate;
`op: "propose"` and `op: "apply"` refuse with a
`PlanModeRefusal { tool_kind: Config }`.

`config_changes_tail` is always in `READ_ONLY_TOOLS` — never
refuses.

## Error kinds

The tool returns `{ "ok": false, "error", "kind", ... }` on
failure. Stable `kind` discriminators:

- `UnknownKey` — key not in `SUPPORTED_SETTINGS`.
- `PathNotAllowed` — key not in this agent's `allowed_paths`.
- `ForbiddenKey` — denylist hit (returns the matched glob).
- `ValidationFailed` — per-key validator rejected the value.
- `NoPending` — apply called for a `patch_id` that was never
  proposed or already consumed.
- `Rejected` — operator replied `[config-reject ...]`.
- `Expired` — 24 h elapsed without approval.
- `RolledBack` — reload rejected the post-apply config; the
  snapshot was restored.
- `Yaml` / `Io` / `InternalError` — fall-through.

## References

- **PRIMARY**: `upstream agent CLI`
  (`ConfigTool.ts`, `supportedSettings.ts`, `prompt.ts`,
  `constants.ts`).
- **Spec**: `proyecto/PHASES.md::79.10`.
