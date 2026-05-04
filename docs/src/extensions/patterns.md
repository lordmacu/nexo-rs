# Extension patterns

Common shapes for nexo extensions. An extension is a self-contained
directory with a `manifest.toml` that declares contributed tools,
advisors, skills, MCP servers, channel adapters, and config schemas.
Operators install with `nexo ext install ./your-extension`.

Pick the closest match; copy the skeleton; modify.

---

## Pattern 1 · Tool bundle

> When to use · You have 3-10 related tools (e.g. CRM ops:
> `crm_lookup`, `crm_create_contact`, `crm_update_deal`,
> `crm_close_deal`) and you want to ship them as a unit.

A tool bundle is the simplest extension. Each tool gets its own
JSON schema + handler binary (or in-process Rust function). The
manifest enumerates them; the daemon registers all on
`nexo ext install`.

```toml
[extension]
id = "crm-tools"
version = "0.2.0"
description = "Salesforce-style CRM operations"

[[tools]]
name = "crm_lookup"
schema_path = "tools/crm_lookup.json"
binary = "./bin/crm-tools"

[[tools]]
name = "crm_create_contact"
schema_path = "tools/crm_create_contact.json"
binary = "./bin/crm-tools"

[[tools]]
name = "crm_close_deal"
schema_path = "tools/crm_close_deal.json"
binary = "./bin/crm-tools"
```

The binary is a single executable that dispatches by tool name.
Operators add the tool names to `agents.yaml` once installed.

---

## Pattern 2 · Advisor pack

> When to use · You're shipping domain-specific personas (sales,
> legal-review, customer-support escalation) that other operators
> can drop into their agents.

Each advisor is a markdown system-prompt file the agent prepends
to its base persona when handling specific topics. Bundle 3-8
together for a vertical.

```toml
[extension]
id = "sales-advisor-pack"
version = "0.1.0"
description = "BANT-style qualification + handoff prompts"

[[advisors]]
id = "bant-qualifier"
prompt_path = "advisors/bant_qualifier.md"

[[advisors]]
id = "objection-handler"
prompt_path = "advisors/objection_handler.md"

[[advisors]]
id = "demo-booker"
prompt_path = "advisors/demo_booker.md"
```

`advisors/bant_qualifier.md`:

```markdown
You are a BANT-trained sales qualifier. For every inbound message,
internally score:
- Budget: ...
- Authority: ...
- Need: ...
- Timeline: ...
Only progress to demo-booker advisor when score >= 70.
```

---

## Pattern 3 · Skill bundle

> When to use · You have multi-step workflows (`send-quote`,
> `escalate-to-human`, `handoff-to-team`) that aren't single LLM
> turns — they need scripted sequences with branching.

Skills are YAML-defined workflows the agent can invoke. Multi-step
with conditionals and tool calls. The extension ships YAML +
referenced templates.

```toml
[extension]
id = "support-skills"
version = "0.3.1"

[[skills]]
id = "escalate-to-human"
yaml_path = "skills/escalate.yaml"

[[skills]]
id = "schedule-followup"
yaml_path = "skills/followup.yaml"
```

`skills/escalate.yaml`:

```yaml
id: escalate-to-human
description: "Hand off to a human on a Telegram channel"
steps:
  - tool: format_transcript
    args: { last_n: 10 }
  - tool: telegram_post
    args:
      channel: ${ESCALATION_CHANNEL}
      message: |
        ⚠ Escalation request from ${user_id}
        Summary: ${summary}
        Transcript: ${transcript_url}
  - reply: "Te conecto con un agente humano. Te responderá pronto."
```

---

## Pattern 4 · MCP server bundle

> When to use · You're wrapping an external service as an MCP
> server so multiple agents can use it.

The extension ships a binary that speaks MCP (stdio or HTTP+SSE).
Operators register the MCP server via the manifest; agents see its
tools as native ones.

```toml
[extension]
id = "github-mcp"
version = "1.0.0"

[[mcp_servers]]
id = "github"
command = "./bin/github-mcp"
transport = "stdio"
env_passthrough = ["GITHUB_TOKEN"]
```

→ See [Building an MCP server extension](./mcp-server-extension.md) for the full walkthrough.

---

## Pattern 5 · Multi-tenant SaaS extension

> When to use · You're building a vertical SaaS (sales / support /
> marketing) where each tenant gets the same toolkit but isolated
> state, scoped credentials, per-tenant audit logs.

The extension declares `multi_tenant.isolated_state = true`. The
framework partitions tool state, credentials, and skill output
per `tenant_id`. Agents bound to a tenant only see that tenant's
data.

```toml
[extension]
id = "sales-saas"
version = "1.2.0"

[[tools]]
name = "crm_lookup"
schema_path = "tools/crm_lookup.json"

[[advisors]]
id = "bant-qualifier"
prompt_path = "advisors/bant.md"

[multi_tenant]
isolated_state = true       # state stored under tenant scope
per_tenant_secrets = true   # secrets resolved per tenant
audit_per_tenant = true     # audit log scoped per tenant

[multi_tenant.quotas]
default = { llm_tokens_month = 1_000_000, agents = 3 }
```

The microapp layer (above) provisions tenants + assigns this
extension to them via admin RPC.

→ [Multi-tenant SaaS guide](./multi-tenant-saas.md)

---

## Pattern 6 · Channel adapter pack

> When to use · You're contributing a new channel kind that's not
> a subprocess plugin (e.g. a stdlib-friendly one that fits inline
> as a daemon module).

The extension declares a channel adapter implementation. The
framework registers it with the channel registry; agents reference
it via `channels: [<kind>:<instance>]` in `agents.yaml`.

```toml
[extension]
id = "discord-channel"
version = "0.1.0"

[[channel_adapters]]
kind = "discord"
adapter_module = "discord_adapter"   # rust crate path or shared lib
config_schema_path = "discord_config.json"
```

Most channels ship as **plugins** (subprocess), not extensions.
Use this pattern only when the adapter must run in-process for
performance or to share daemon state directly.

---

## Pattern 7 · Config schema extension

> When to use · You want to expose a new YAML config block that
> operators set in `agents.yaml` or a new file under `config/`.

The extension declares a JSON Schema for the new config; the
daemon merges it into `nexo doctor config` validation and
`nexo agent doctor` reports.

```toml
[extension]
id = "billing-config"
version = "0.1.0"

[[config_schemas]]
section = "billing"
schema_path = "billing.schema.json"
yaml_files = ["billing.yaml"]
```

Operator's `config/billing.yaml`:

```yaml
billing:
  provider: stripe
  webhook_secret: ${STRIPE_WEBHOOK_SECRET}
  default_plan: pro
```

`nexo doctor config` will validate the file against your schema.

---

## Pattern 8 · Knowledge-base loader

> When to use · You're shipping a curated KB (FAQs, runbooks,
> playbooks) that should land in the operator's vector store.

The extension ships markdown / JSON documents + a `kb_loader`
hook that imports them into the configured vector store on
install.

```toml
[extension]
id = "support-kb"
version = "1.0.0"

[[kb_collections]]
id = "support-faqs"
loader = "./bin/load-faqs"     # binary that reads docs/ and emits chunks
docs_dir = "docs/"
embedding_model = "minimax-embed"
```

The loader runs once at install time + re-runs whenever the
operator updates the extension version. Output lands in
`<state_dir>/<tenant_id>/vector/support-faqs/`.

---

## Choosing between patterns

| If you... | Use |
|-----------|-----|
| Have related tools to ship together | Tool bundle (1) |
| Have domain-specific persona prompts | Advisor pack (2) |
| Have multi-step scripted workflows | Skill bundle (3) |
| Wrap an external service as MCP | MCP server bundle (4) |
| Build a vertical SaaS | Multi-tenant SaaS (5) |
| Add a new in-process channel kind | Channel adapter (6) |
| Add a new config section | Config schema (7) |
| Ship a curated knowledge base | KB loader (8) |

---

## Plugin vs Extension — quick decision

If you find yourself between Plugin and Extension:

- Choose **Plugin** when: the work is a separate process, runs in
  a non-Rust language, or interacts with an external service that
  has its own connection lifecycle (WebSocket, gateway, push).
- Choose **Extension** when: the work is in-process Rust, ships
  with curated assets (advisors / skills / KBs), or needs tight
  multi-tenant state isolation.

---

## See also

- [Manifest reference](./manifest.md) — full TOML schema.
- [Templates](./templates.md) — copy-and-modify starters.
- [CLI](./cli.md) — `nexo ext install` / `list` / `doctor`.
- [Multi-tenant SaaS guide](./multi-tenant-saas.md) — full walkthrough of pattern 5.
