# Worker mode (Phase 84.4)

Complement to [coordinator mode](./coordinator-mode.md). Bindings
with `role: worker` get a worker-specific system prompt block
prepended to the agent's existing `system_prompt`. Workers run
self-contained tasks dispatched by a coordinator; the persona
steers them away from user-facing dialogue and toward terse,
verified, on-spec output.

## What 84.4 ships

When a binding's resolved `BindingRole` is `Worker`, the runtime
prepends the worker persona block. `Coordinator` bindings get the
coordinator block instead; `Proactive` and absent `role` are
byte-identical to today.

```
# WORKER ROLE
{persona block — sections below}

{agent.system_prompt}

# CHANNEL ADDENDUM
{binding.system_prompt_extra — when set}
```

## Persona block sections

1. **Role declaration** — frames the agent as an executor: do the
   work, report results, do **not** initiate user-facing dialogue.
   Scope questions go back to the coordinator via the final answer
   (`"blocked: need X"`).
2. **Output discipline** — the final answer is read by another
   agent, not a human. Optimize for parseability:
   - **Code work**: file path + line range + actual diff (or
     commit hash).
   - **Research**: bullet list with `file_path:line` references.
   - **Failures**: actual error output verbatim, not paraphrased.
3. **Self-verification** — typecheck + test + read the diff
   before reporting done. False "done" reports poison the
   synthesis above.
4. **Tools available to you** — the binding's `allowed_tools` list
   verbatim (workers see exactly the surface the operator
   granted, no curated subset). Followed by an explicit reminder
   that `TeamCreate`, `SendToPeer`, `SendMessageToWorker`, and
   `TaskStop` are **not** worker tools — wanting one means the
   task scope is too large.
5. **Scratchpad** *(optional)* — appears when `TodoWrite` is in
   `allowed_tools`. Worker scratchpad is for the worker's own
   multi-step state, not for cross-worker coordination.

## Configuring a worker binding

```yaml
agents:
  ana-worker:
    inbound_bindings:
      - plugin: whatsapp
        instance: ana_worker
        role: worker
        allowed_tools:
          - BashTool
          - FileEdit
          - WebFetch
          - TodoWrite
```

## Composition with other phases

| Phase | Composition |
|---|---|
| 16 — `EffectiveBindingPolicy` | Worker prepend runs inside `resolve()` after `allowed_tools` is computed |
| 77.18 — `BindingRole::Worker` | The role string parses to `BindingRole::Worker`; only that triggers the prepend |
| 79.4 — `TodoWrite` | Scratchpad section appears when `TodoWrite` is on the binding's surface |
| 84.1 — coordinator persona | The two persona builders are sister modules; the boot-path matcher in `apply_persona_prefix` dispatches on role |
| 84.2 — `<task-notification>` envelope | A worker's final assistant text becomes the `result` field of the envelope when the spawn pipeline ships |
| 84.3 — `SendMessageToWorker` | Workers don't *call* SendMessageToWorker (it's coordinator-only), but their session is what the tool resumes |

## Inspecting the rendered prompt

The full path is exercised in
`cargo test -p nexo-core agent::effective::tests::worker_role_loaded_from_yaml_renders_persona_block`,
which deserializes a YAML binding fixture and asserts the
`# WORKER ROLE` prefix lands ahead of the agent's own prompt with
the expected sections.
