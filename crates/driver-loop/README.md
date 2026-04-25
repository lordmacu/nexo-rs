# nexo-driver-loop

Goal orchestrator for Phase 67. This is where 67.0–67.3 get wired
together:

- `DriverOrchestrator::run_goal` runs one goal end-to-end.
- `ClaudeHarness` impl `AgentHarness` — closes the contract from 67.0.
- `LlmDecider` is the production `PermissionDecider` (consults
  MiniMax via `nexo-llm`).
- `DriverSocketServer` (daemon side) + `SocketDecider` (bin side, in
  `nexo-driver-permission`) form the IPC bridge between the
  daemon-resident decider and the MCP child Claude spawns per turn.
- `nexo-driver` CLI runs goals from YAML.

See `docs/src/architecture/driver-subsystem.md` for the architecture
diagram and the goal lifecycle walkthrough.
