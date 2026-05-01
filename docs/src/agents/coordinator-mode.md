# Coordinator mode (Phase 84)

Phase 77.18 introduced `role: coordinator | worker` as a binding flag
and gated the team-coordination tool surface behind it. Phase 84
closes the gap that remained: until 84.1 shipped, a coordinator
binding only saw the *tools* — it ran the same system prompt as any
other binding and treated worker results as opaque chat fragments.

## What 84.1 ships

When a binding's resolved `BindingRole` is `Coordinator`, the runtime
prepends a purpose-built **coordinator persona** block ahead of the
agent's existing system prompt. The block is deterministic (same
inputs → same bytes) so prompt-cache prefix matching stays warm
across turns.

Order of the rendered system prompt:

```
# COORDINATOR ROLE
{persona block — sections below}

{agent.system_prompt}

# CHANNEL ADDENDUM
{binding.system_prompt_extra — when set}
```

`Worker`, `Proactive`, and absent `role` bindings are byte-identical
to today; the persona prefix only kicks in for `Coordinator`.

## Persona block sections

1. **Role declaration** — frames the agent as a coordinator: directs
   workers, synthesizes results, communicates with the user.
2. **Tools available to you** — the binding's `allowed_tools`
   filtered to the curated coordinator surface (`TeamCreate`,
   `TeamDelete`, `SendToPeer`, `ListPeers`, `SendMessageToWorker`,
   `TaskStop`, `TaskList`, `TaskGet`, `TodoWrite`). Tools the
   binding doesn't surface drop out.
3. **Worker result envelope** — instruction to treat the
   `<task-notification>` XML envelope (Phase 84.2) as a system
   event, never as a user message.
4. **Continue-vs-spawn matrix** — decision table guiding when to
   reuse a finished worker (`SendMessageToWorker`, Phase 84.3) vs.
   spawn fresh (`TeamCreate`, Phase 79.6) vs. message a live peer
   (`SendToPeer`, Phase 80.11).
5. **Synthesis discipline** — coordinator must produce
   implementation specs with file paths and line numbers.
   Anti-pattern: "based on your findings, fix the bug" (delegates
   understanding back to the worker).
6. **Verification rigor** — real verification (run failing case
   first, apply fix, run again, confirm broader suite, read the
   diff). "The build passed" is not verification.
7. **Parallelism** — independent work fans out via concurrent tool
   calls in a single assistant message.
8. **Scratchpad** *(optional)* — appears when the binding has
   `TodoWrite` (Phase 79.4) in `allowed_tools`. Mandatory for 3+
   workers.
9. **Known workers** *(optional)* — only rendered when the
   `CoordinatorPromptCtx.workers` slice is populated; the boot
   path passes empty by default since peer discovery is dynamic.

## Configuring a coordinator binding

```yaml
agents:
  ana:
    inbound_bindings:
      - plugin: whatsapp
        instance: ana_main
        role: coordinator
        allowed_tools:
          - TeamCreate
          - TeamDelete
          - SendToPeer
          - ListPeers
          - SendMessageToWorker     # Phase 84.3
          - TaskStop
          - TodoWrite               # enables Scratchpad section
```

The `allowed_tools` list shapes both the runtime tool surface
(Phase 16) and the persona block's tool list — keeping the two in
sync without a parallel config.

## Continue-vs-spawn matrix

| Situation | Action |
|---|---|
| Worker finished; new ask builds on its loaded context | **Continue** (`SendMessageToWorker`, Phase 84.3) |
| New work has no overlap with any finished worker | **Spawn fresh** (`TeamCreate`) |
| Two unrelated streams of work | **Spawn in parallel** — one assistant message, both calls |
| Worker still in_progress; want to nudge | **Send to peer** (`SendToPeer`) |
| Worker silent past budget | `TaskStop`, then decide spawn-vs-continue from partial |

Default to **continue** when the new ask shares >50% of the prior
worker's read files / search terms. Default to **spawn** when in a
different subsystem.

## `<task-notification>` envelope (Phase 84.2)

Worker results arrive in the coordinator's session wrapped in a
`<task-notification>` XML block. The block carries `task-id`,
`status`, `summary`, optional `result`, and optional `usage`:

```xml
<task-notification>
<task-id>goal-9f3a</task-id>
<status>completed</status>
<summary>Found 3 candidate fixes</summary>
<result>See `crates/auth.rs:142`.</result>
<usage>
<total_tokens>1280</total_tokens>
<tool_uses>4</tool_uses>
<duration_ms>12400</duration_ms>
</usage>
</task-notification>
```

`status` is one of `completed | failed | killed | timeout`.
Optional elements (`<result>`, `<usage>`) collapse out when the
producer has no value.

**Treat these blocks as system events, not user messages.** The
persona prompt explicitly forbids `<thank>` / `<acknowledge>`
responses to a notification — read it, factor into synthesis, and
either continue the worker (84.3), spawn the next one, or report
to the user.

**Producer surface** lives in `nexo-fork::fork_handle`:

```rust
let n = fork_result.to_task_notification(task_id, summary, duration_ms);
let xml = n.to_xml(); // injected into the coordinator's next user turn
```

`fork_error_to_task_notification(err, task_id, duration_ms)` covers
the failure paths (`ForkError::Aborted` → `killed`,
`ForkError::Timeout` → `timeout`, others → `failed`).

**Consumer wiring** (the producer-to-LLM-context bridge) lands with
84.3 — that's where the fork-pass + TeamCreate completion paths
actually exist. The 84.2 work pre-builds the type + producer helpers
so 84.3 has one canonical path.

**Backwards compatibility**: `TaskNotification::parse_block(text)`
returns `None` when the input lacks the envelope, so legacy
consumers that read raw final text keep working during the rollout.

## Composition with other phases

| Phase | Composition |
|---|---|
| 16 — `EffectiveBindingPolicy` | Persona prepend runs inside `resolve()` after `allowed_tools` is computed so the tool list reflects the effective binding surface |
| 77.18 — `BindingRole` | The role string is parsed via `BindingRole::from_role_str`; only `Coordinator` triggers the prepend |
| 79.4 — `TodoWrite` | When present in `allowed_tools`, the Scratchpad section is rendered |
| 79.6 — `TeamCreate` / `TeamDelete` | Listed in the persona's tool section when on the binding's surface |
| 80.11 — `SendToPeer` / `ListPeers` | Same |
| 84.2 — `<task-notification>` envelope | The persona instructs the agent to treat these blocks as system events |
| 84.3 — `SendMessageToWorker` | Listed in the persona's tool section; the continue-vs-spawn matrix references it |

## Inspecting the rendered prompt

For verification on a configured agent, the prompt is visible in
the runtime `EffectiveBindingPolicy::resolve(...).system_prompt`
field. A `cargo test -p nexo-core agent::effective::tests::coordinator`
run exercises the full path including a YAML-fixture smoke test.
