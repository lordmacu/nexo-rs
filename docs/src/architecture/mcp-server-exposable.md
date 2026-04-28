# MCP server exposable catalog (Phase 79.M)

`nexo mcp-server` advertises a *curated subset* of the runtime
tool registry to external MCP clients (Claude Desktop, Cursor,
Zed, etc.). The subset is defined in code by a static slice;
operators pick which entries to enable via
`mcp_server.expose_tools`.

## Source-of-truth: `EXPOSABLE_TOOLS`

```rust
// crates/config/src/types/mcp_exposable.rs

pub static EXPOSABLE_TOOLS: &[ExposableToolEntry] = &[
    // ...
    ExposableToolEntry {
        name: "cron_list",
        tier: SecurityTier::ReadOnly,
        boot_kind: BootKind::Always,
        feature_gate: None,
    },
    // ...
];
```

Adding a tool to this slice does **not** expose it — the operator
must still list the name in `mcp_server.expose_tools`. The slice
controls what is *legal* to expose; YAML controls what is
actually exposed.

## YAML

```yaml
# config/mcp_server.yaml
mcp_server:
  enabled: true
  name: "kate"
  expose_tools:
    - cron_list
    - cron_create
    - ListMcpResources
    - ReadMcpResource
    - config_changes_tail
    - web_search
    - web_fetch
    - EnterPlanMode
    - ExitPlanMode
    - ToolSearch
    - TodoWrite
    - NotebookEdit
  expose_denied_tools:
    - Heartbeat
  denied_tools_profile:
    enabled: true
    require_auth: true
    require_delegate_allowlist: true
    require_remote_trigger_targets: true
    allow:
      heartbeat: true
      delegate: false
      remote_trigger: false
```

## Three-bucket policy

| Bucket | `BootKind` | Behaviour |
|--------|-----------|-----------|
| **Expose** | `Always` | Boot helper constructs the tool from `McpServerBootContext`; missing handle → labelled skip. |
| **Expose (gated)** | `FeatureGated` | Skipped unless the named Cargo feature is enabled. `Config` is the only entry today. |
| **Deny by default** | `DeniedByPolicy { reason }` | Dispatcher denies by default (`Heartbeat`, `delegate`, `RemoteTrigger`). `run_mcp_server` can optionally override selected entries via `mcp_server.expose_denied_tools` plus extra safety checks. |
| **Defer** | `Deferred { phase, reason }` | Wiring postponed to a follow-up sub-phase. `Lsp`, `Team*`. |

## Boot dispatch flow

```
expose_tools (YAML) ┐
                    ├──► EXPOSABLE_TOOLS lookup
                    │     │
                    │     ├──► Always       → boot helper → Registered | SkippedInfraMissing
                    │     ├──► FeatureGated → cfg!(feature) check → Registered | SkippedFeatureGated
                    │     ├──► DeniedByPolicy → SkippedDenied (or override path in run_mcp_server)
                    │     └──► Deferred    → SkippedDeferred
                    └──► (typo / removed)  → UnknownName
```

Every outcome lands in two telemetry counters:

- `mcp_server_tool_registered_total{name, tier}`
- `mcp_server_tool_skipped_total{name, reason}`

`reason` ∈ `{denied_by_policy, deferred, feature_gate_off,
infra_missing, unknown_name}`.

## Boot context

```rust
// crates/core/src/agent/mcp_server_bridge/context.rs

pub struct McpServerBootContext {
    pub agent_id: String,
    pub broker: AnyBroker,
    pub cron_store: Option<Arc<dyn CronStore>>,
    pub mcp_runtime: Option<Arc<SessionMcpRuntime>>,
    pub config_changes_store: Option<Arc<dyn ConfigChangesStore>>,
    pub web_search_router: Option<Arc<WebSearchRouter>>,
    pub link_extractor: Option<Arc<LinkExtractor>>,
    pub agent_context: Arc<AgentContext>,
}
```

`run_mcp_server` builds the context best-effort: it tries to
open `./data/cron.db`, `./data/config_changes.db`, and the
env-driven web-search providers when the corresponding entry
is in `expose_tools`. If a handle cannot be constructed the
relevant tool is skipped with a labelled warn line; the
server still boots.

## Safe profile for denied overrides

Denied-by-default tools now require two explicit opt-ins:

1. Tool name in `mcp_server.expose_denied_tools`.
2. Matching allow-bit in `mcp_server.denied_tools_profile.allow.*` with
   `denied_tools_profile.enabled: true`.

Default profile is fail-closed (`enabled: false`, all allow bits false).

Additional hardening gates in the profile:

- `require_auth` (default `true`): requires `mcp_server.auth_token_env`
  or `mcp_server.http.auth`.
- `require_delegate_allowlist` (default `true`): `delegate` only boots
  when `agents.<id>.allowed_delegates` is non-empty and not `["*"]`.
- `require_remote_trigger_targets` (default `true`): `RemoteTrigger`
  only boots when `agents.<id>.remote_triggers` has at least one entry.

## Adding a new tool

1. Implement the tool somewhere in `nexo-core::agent::*` so it
   has a `tool_def() -> ToolDef` and a `ToolHandler` impl.
2. Add an `ExposableToolEntry` to `EXPOSABLE_TOOLS` with the
   appropriate `tier` + `boot_kind`.
3. Add a match arm in `boot_always` (or per-bucket helper)
   that constructs the tool from the boot context and returns
   `BootResult::Registered`.
4. Add a unit test in
   `crates/core/src/agent/mcp_server_bridge/dispatch.rs::tests`
   covering the missing-handle and present-handle cases.
5. The conformance suite in
   `crates/core/tests/exposable_catalog_test.rs` will
   automatically pick it up via the `every_always_entry_boots_*`
   tests.

## Comparison vs `nexo run`

| | `nexo run` | `nexo mcp-server` |
|--|-----------|--------------------|
| Tool registry | full (~31 tools, per-binding) | curated subset of `EXPOSABLE_TOOLS` |
| Plan-mode gating | yes (`MUTATING_TOOLS` / `READ_ONLY_TOOLS`) | yes — same gates apply |
| Capability YAML | per-agent `team.enabled`, `lsp.enabled`, etc. | `mcp_server.expose_tools` allowlist |
| Auth | local trust + binding policy | optional `auth_token_env` / `http.auth.kind` |

## Threat model — Config self-edit via MCP

The `Config` tool is the only entry that lets an external MCP
client mutate the agent's YAML at runtime. It is gated by **four
locks** that all must be open before the boot dispatcher
registers it:

| Lock | Where | Failure → |
|------|-------|-----------|
| 1. Cargo feature `config-self-edit` | compile-time | `SkippedFeatureGated` |
| 2. `mcp_server.auth_token_env` or `http.auth` set | boot-time | `SkippedDenied { config-requires-auth-token }` |
| 3. `agents.<id>.config_tool.self_edit = true` | per-agent YAML | `SkippedDenied { config-self-edit-policy-disabled }` |
| 4. `agents.<id>.config_tool.allowed_paths` non-empty | per-agent YAML | `SkippedDenied { config-allowed-paths-must-be-explicit }` |

Plus the inherent denylist
(`crates/setup/src/capabilities.rs::CONFIG_SELF_EDIT_DENYLIST`)
which permanently blocks credentials, allowed_delegates,
outbound_allowlist, system_prompt, plugins, mcp_server.*, and
broker.*. The denylist is hard-coded in code, not operator-
editable from inside a Config call.

Approval flow:

1. Model calls `Config { op: "propose", key, value, justification }`.
2. ConfigTool stages the patch under `<state_dir>/config-proposals/<patch_id>.yaml`.
3. ApprovalCorrelator parks a `oneshot::Receiver` keyed by `patch_id`.
4. Operator sends `[config-approve patch_id=<id>]` on any plugin
   inbound topic the daemon subscribes to (works because mcp-
   server's correlator subscribes to `plugin.inbound.>` if NATS
   is shared with the operator's `nexo run` daemon).
5. Model calls `Config { op: "apply", patch_id }`. If approved,
   the YAML write happens; ConfigChangesStore records the row;
   ReloadTrigger fires.

In mcp-server mode the `McpServerReloadTrigger` is a stub that
returns `Ok` with a log line. The mutated YAML is durable on disk;
the operator's `nexo run` daemon picks it up via Phase 18 file
watcher. **The mcp-server process itself does not run a
ConfigReloadCoordinator** — same-process reload only happens in
`nexo run`.

Audit:

- Every read/propose/apply lands in `config_changes` SQLite
  (`<state_dir>/config_changes.db`) via `ConfigChangesStore`.
- Tail with `Config { op: ... }` events:
  `config_changes_tail` (read-only, exposable).
- Secret values redacted via `DefaultSecretRedactor` (matches
  `*_token`, `*_secret`, `*_password`, `*_key` suffixes).

What an MCP client **cannot** do, even with all locks open:

- Change credentials, API keys, OAuth tokens (denylist).
- Add/remove agent bindings (denylist on `inbound_bindings`).
- Modify `allowed_delegates`, `outbound_allowlist`, `system_prompt`
  (denylist).
- Toggle plugins (denylist on `plugins`).
- Self-elevate `mcp_server.expose_tools` (denylist on
  `mcp_server.*`).
- Bypass approval — `apply` always blocks until correlator gets
  a matching `[config-approve patch_id=<id>]` from inbound.
- Read secret values without redaction.

## References

- **PRIMARIO**: `claude-code-leak/src/Tool.ts:395-449`,
  `claude-code-leak/src/services/mcp/channelAllowlist.ts:1-80`.
- **SECUNDARIO**: `research/docs/cli/mcp.md:30-120`
  (`openclaw mcp serve` curated catalog).
- **Spec**: `proyecto/PHASES.md::79.M`.
