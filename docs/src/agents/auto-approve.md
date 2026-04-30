# Auto-approve dial

The `auto_approve` per-binding flag flips the approval gate from
"always ask the operator" to "auto-allow a curated subset of safe
tools". It is the missing piece that makes [assistant mode](./assistant-mode.md)
practical: a proactive agent running cron-driven goals can't block
on interactive approvals at every tool call.

Default is **disabled** — current interactive-approval behaviour
preserved unchanged for every existing binding.

## What auto-approves

When `auto_approve: true` AND the tool is in the curated subset
AND its call passes the per-tool conditional checks, the approval
prompt is skipped and the tool runs as `AllowOnce`. Otherwise the
existing approval pipeline takes over.

| Bucket | Tools | Notes |
|--------|-------|-------|
| Read-only / info | `FileRead`, `Glob`, `Grep`, `LSP`, `WebFetch`, `WebSearch`, `list_agents`, `agent_status`, `agent_turns_tail`, `memory_history`, `dream_runs_tail`, `list_mcp_resources`, `read_mcp_resource`, `list_followups`, `list_peers`, `task_get` | Always auto when dial on |
| Bash conditional | `Bash` | Only when `is_read_only` AND not `destructive_command` AND not `sed_in_place` |
| Scoped writes | `FileEdit`, `FileWrite` | Only when path canonicalises under `workspace_path`; new-file case canonicalises parent then re-attaches filename; symlink-escape resistant |
| Notify + memory | `notify_origin`, `notify_channel`, `notify_push`, `forge_memory_checkpoint`, `dream_now`, `ask_user_question` | Always auto |
| Coordination | `delegate`, `team_create`, `team_delete`, `send_to_peer`, `task_create`, `task_update`, `task_stop` | Always auto |

## What ALWAYS asks (regardless of dial)

| Tool | Why |
|------|-----|
| `ConfigTool` / `config_self_edit` | Self-editing YAML is too dangerous |
| `REPL` | Stateful subprocess side-effects |
| `remote_trigger` | Outbound webhook to arbitrary URL |
| `schedule_cron` | Persistent state mutation |
| Bash with destructive / sed-in-place | Phase 77.8/77.9 vetoes ALWAYS apply |
| `mcp_*` / `ext_*` prefix | Heterogeneous per-server semantics |
| Unknown tool name | Default-deny — new tools must be explicitly added |

## Layering

The dial composes with the existing gates rather than replacing
them:

```
┌─────────────────────────────────────────────────────────────┐
│ 1. Phase 16 binding `allowed_tools` (tool name filter)      │
│    └── Tool not in list → never even reaches the registry   │
│ 2. Capability gate (env vars + cargo features)              │
│    └── Host-level dangerous toggle off → tool stripped      │
│ 3. Auto-approve dial (THIS layer)                           │
│    └── Curated subset → AllowOnce                           │
│    └── Otherwise → fall through to operator prompt          │
│ 4. Bash destructive heuristic (Phase 77.8/77.9)             │
│    └── ALWAYS vetoes regardless of dial                     │
│ 5. Operator interactive approval                            │
│    └── Companion-tui / pairing / chat reply                 │
└─────────────────────────────────────────────────────────────┘
```

The dial NEVER widens the tool surface. A tool absent from the
binding's `allowed_tools` is still absent. The dial only skips
the prompt for tools that are already on the surface AND fall in
the curated subset.

## Configuration

```yaml
agents:
  - id: kate
    workspace: /home/kate/projects   # used to scope FileEdit/Write
    allowed_tools: ["FileRead", "Bash", "FileEdit", "delegate"]
    auto_approve: true               # agent-level default
    inbound_bindings:
      - plugin: whatsapp
        # Per-binding override (optional). None inherits agent default.
        auto_approve: false           # this binding stays interactive
```

`agent.auto_approve` is the agent-level default; per-binding
override at `inbound_bindings[].auto_approve` is `Option<bool>`
where `None` inherits.

## Wiring on the operator side (deferred 80.17.b.b/c)

Today's slim MVP ships:

- `is_curated_auto_approve(tool_name, args, on, workspace_path) -> bool`
  decision table (`crates/driver-permission/src/auto_approve.rs`)
- `AutoApproveDecider<D>` decorator (same module) wrapping any
  `PermissionDecider` chain
- `AgentConfig.auto_approve: bool` + `InboundBinding.auto_approve:
  Option<bool>` YAML schema
- `EffectiveBindingPolicy.auto_approve: bool` + `workspace_path:
  Option<PathBuf>` resolved per binding

Pending:

- **80.17.b.b** — boot-time wrap of the active decider with
  `AutoApproveDecider::new(...)` (1-line snippet)
- **80.17.b.c** — caller-side `metadata` population: the wire
  that constructs `PermissionRequest` must insert
  `metadata.auto_approve` and `metadata.workspace_path` from the
  resolved policy before invoking the decider
- **80.17.c** — `nexo setup doctor` warn for `assistant_mode +
  !auto_approve` misconfiguration

Until those ship, the decorator is a transparent pass-through —
the helper is called but the metadata never reads `true`. Test it
locally by hand-populating metadata in your decider wrapper.

## Defense-in-depth

Five layers protect against agent misbehaviour even with the dial
on:

1. **Phase 16 binding policy** — tool not on the surface = never
   reachable.
2. **Default-deny match arm** — newly introduced tools never
   auto-approve until explicitly added to the decision table.
3. **Phase 77.8/77.9 destructive heuristic** — `rm -rf`, `dd`,
   `mkfs`, `sed -i`, fork-bomb shapes always veto.
4. **Workspace-scoped writes** — symlink-escape resistant via
   `Path::canonicalize` + `starts_with`.
5. **Operator restart kill-switch** — flipping `auto_approve:
   false` and restarting takes < 5 seconds.

## See also

- [Assistant mode](./assistant-mode.md)
- [AWAY_SUMMARY digest](./away-summary.md) — what the agent
  reports when you re-connect after running auto-approved
- [Multi-agent coordination](./multi-agent-coordination.md)
