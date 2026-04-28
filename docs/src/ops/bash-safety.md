# Bash safety knobs

Nexo's Bash safety model is layered. Even when the `Bash` tool is
available, execution is constrained by policy and runtime gates.

## Main safety layers

1. Per-binding tool allowlist
   - `allowed_tools` can remove `Bash` entirely for selected channels.
2. Plan mode gating
   - mutating paths are blocked until explicit exit/approval workflow.
3. Destructive-intent integration
   - plan-mode policy can auto-enter on destructive command detection.
4. Worker-role curation
   - worker bindings run a constrained tool surface by default.

## Relevant config knobs

- `agents[].allowed_tools`
- `agents[].inbound_bindings[].allowed_tools`
- `agents[].plan_mode.*`
- `agents[].inbound_bindings[].plan_mode.*`
- `agents[].inbound_bindings[].role`

## Operational guidance

- For user-facing channels, prefer narrowing `allowed_tools` rather
  than trusting prompt-only behavior.
- Keep plan mode enabled for coordinator bindings.
- Use worker role for delegated execution to reduce blast radius.

## Related docs

- [Plan mode](../architecture/plan-mode.md)
- [Context optimization](./context-optimization.md)
- [Capability toggles](./capabilities.md)
