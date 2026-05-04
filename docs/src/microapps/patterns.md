# Microapp patterns

Common shapes for nexo microapps. A microapp is a complete product
that consumes nexo-rs as its agent runtime — your microapp owns
the UI, the multi-tenant story, the billing; the framework runs
out of view.

Microapps talk to the framework over **admin RPC over NATS** —
provision tenants, configure agents, manage knowledge bases,
rotate API keys.

---

## Pattern 1 · Single-tenant deploy

> When to use · You're building an internal tool for one team or
> one company. Multi-tenancy is overkill.

Microapp configures one tenant at boot, never creates more. Used
for: an internal sales bot, a personal AI assistant, a
single-org customer-support system.

```rust
use nexo_microapp_sdk::admin::{AdminClient, TenantSpec, AgentSpec};

let admin = AdminClient::connect("nats://localhost:4222").await?;

// Bootstrap on first run; idempotent on subsequent boots.
admin.ensure_tenant(TenantSpec {
    id: "default".into(),
    plan: "internal".into(),
    quotas: Quotas::unlimited(),
}).await?;

admin.ensure_agent("default", AgentSpec {
    id: "ana".into(),
    persona_path: "./personas/ana.md".into(),
    channels: vec!["whatsapp:internal".into()],
    llm: "minimax-m2.5".into(),
}).await?;
```

Microapp's UI is a thin admin panel. Most config lives in YAML;
microapp tweaks runtime knobs.

---

## Pattern 2 · Multi-tenant SaaS

> When to use · You're selling to multiple customers. Each gets
> isolated state, their own agents, their own KB.

Microapp creates a tenant per signup. The framework partitions
state per `tenant_id`. Microapp owns the auth / billing / UI;
framework runs the agent loop.

```rust
async fn handle_signup(req: SignupRequest, admin: &AdminClient) -> Result<TenantId> {
    let tenant_id = format!("client-{}", uuid::Uuid::new_v4());

    admin.create_tenant(TenantSpec {
        id: tenant_id.clone(),
        plan: req.plan,
        quotas: quotas_for_plan(&req.plan),
    }).await?;

    // Provision the customer's first agent.
    admin.create_agent(&tenant_id, AgentSpec {
        id: "default-agent".into(),
        persona_path: req.persona.unwrap_or_else(default_persona),
        channels: vec![],   // customer pairs channels via UI later
        llm: "minimax-m2.5".into(),
    }).await?;

    Ok(tenant_id)
}
```

`agent-creator-microapp` (the reference implementation) is built
exactly this way — every signup gets a tenant, end-users build
their own WhatsApp agents through a WhatsApp-Web-style UI.

→ [`agent-creator` reference](./agent-creator.md)
→ [Multi-tenant SaaS guide](../extensions/multi-tenant-saas.md)

---

## Pattern 3 · BYO-UI

> When to use · You're building a SaaS but want full control over
> the user-facing interface (custom React app, mobile app,
> Tauri desktop).

Microapp exposes its own HTTP / GraphQL / gRPC API. The frontend
calls the microapp; the microapp calls the framework via admin
RPC. The framework never serves UI directly.

```typescript
// React frontend
async function pairWhatsApp(agentId: string): Promise<{ qr: string }> {
  return fetch(`/api/agents/${agentId}/whatsapp/pair`, { method: "POST" })
    .then(r => r.json());
}
```

```rust
// Microapp backend (Rust + Axum)
async fn pair_whatsapp(
    State(admin): State<AdminClient>,
    Path(agent_id): Path<String>,
    auth: AuthSession,  // resolves tenant_id
) -> Json<PairQrResponse> {
    let qr = admin.pair_channel(
        &auth.tenant_id,
        &agent_id,
        ChannelKind::Whatsapp,
    ).await.unwrap();
    Json(PairQrResponse { qr })
}
```

The microapp can be in any language — Rust, Python, TypeScript,
PHP, Go — as long as it speaks NATS to the framework.

---

## Pattern 4 · Knowledge-as-a-Service

> When to use · Customers upload documents (PDFs, MD, URLs); your
> microapp ingests them into a per-tenant vector store; agents
> answer from the KB.

Microapp owns the upload UI + ingestion pipeline. Framework
exposes vector-store admin RPC; microapp uses it to populate
each tenant's KB.

```rust
async fn ingest_document(
    tenant_id: &str,
    doc: UploadedDoc,
    admin: &AdminClient,
) -> Result<()> {
    let chunks = chunk_document(&doc.content);
    for chunk in chunks {
        let embedding = embed(&chunk).await?;
        admin.vector_upsert(tenant_id, VectorRecord {
            id: uuid::Uuid::new_v4().to_string(),
            collection: "kb".into(),
            content: chunk.text,
            embedding,
            metadata: doc.metadata.clone(),
        }).await?;
    }
    Ok(())
}
```

Agents in that tenant query via a `search_kb` tool that the
framework wires automatically when `vector_collections: [kb]` is
declared in their `agents.yaml`.

---

## Pattern 5 · Webhook-driven SaaS

> When to use · External services (Stripe, GitHub, Shopify) push
> events to your SaaS; you trigger agent workflows from those
> events.

Microapp accepts webhooks at `POST /webhook/<provider>`. Each
webhook becomes a `RemoteTrigger` published to the framework,
which routes to the right agent based on tenant + provider.

```rust
async fn stripe_webhook(
    State((admin, secret)): State<(AdminClient, String)>,
    body: Bytes,
    headers: HeaderMap,
) -> StatusCode {
    let event = stripe::verify_webhook(&body, &headers, &secret)?;
    let tenant_id = lookup_tenant_by_stripe_customer(&event.customer).await?;

    admin.publish_remote_trigger(&tenant_id, RemoteTrigger {
        kind: "stripe.charge.failed".into(),
        target_agent: "billing-bot".into(),
        payload: serde_json::to_value(&event)?,
    }).await?;

    StatusCode::OK
}
```

→ [RemoteTrigger outbound publisher](../architecture/remote-trigger.md)

---

## Pattern 6 · Background workers + scheduled jobs

> When to use · Microapp needs to run periodic tasks (digest
> emails, lead nurturing campaigns, billing reconciliation) that
> don't fit naturally into the agent loop.

Microapp uses its own job runner (Sidekiq / Celery / cron). When
a job fires, it talks to the framework via admin RPC to dispatch
the agent task.

```python
# Microapp's celery worker
@celery.task
def daily_digest(tenant_id: str):
    admin = AdminClient.connect("nats://...")
    leads = fetch_new_leads(tenant_id)
    if not leads:
        return
    admin.dispatch_agent_task(
        tenant_id=tenant_id,
        agent_id="digest-bot",
        prompt=f"Build a 3-line summary of {len(leads)} new leads",
        context={"leads": leads},
    )
```

The framework ships `cron_schedule` tools too — but microapp-side
jobs can do anything the framework can't (DB queries, third-party
API calls, multi-step orchestration).

---

## Pattern 7 · White-label deploy

> When to use · You're selling the same microapp to multiple
> customers, each with their own branding / domain.

Microapp reads its branding (logo, name, primary color) from the
tenant's config. Each tenant's domain points to the same
microapp deploy with a header (`X-Tenant-Slug: acme`) that
resolves to the right tenant.

```rust
async fn extract_tenant(headers: &HeaderMap) -> Result<TenantId> {
    let slug = headers.get("X-Tenant-Slug")
        .and_then(|v| v.to_str().ok())
        .ok_or(BadRequest)?;
    Ok(tenant_id_for_slug(slug).await?)
}
```

The framework's per-tenant secrets + audit logs handle the
isolation; microapp handles the branding.

---

## Pattern 8 · Hybrid (your stack + framework)

> When to use · You have an existing product (Rails / Django /
> Laravel SaaS) and want to add agent capability without rebuilding.

Microapp keeps its existing UI / DB / auth. It only delegates the
agent loop to nexo-rs. The integration is one admin RPC client
in your existing backend.

```php
// Existing Laravel SaaS adds an agent endpoint
class AgentController extends Controller
{
    public function ask(Request $req): JsonResponse
    {
        $admin = app(AdminClient::class);
        $reply = $admin->dispatchAgentTask(
            tenantId: auth()->user()->tenant_id,
            agentId: 'support-copilot',
            prompt: $req->input('message'),
        );
        return response()->json(['reply' => $reply]);
    }
}
```

Your existing app stays as-is; nexo-rs becomes a backend service
your code calls when it needs an agent.

---

## Choosing between patterns

| If you... | Use |
|-----------|-----|
| Build for one team / one company | Single-tenant deploy (1) |
| Sell to multiple customers | Multi-tenant SaaS (2) |
| Want a custom UI (React / mobile / Tauri) | BYO-UI (3) |
| Customers upload docs to query | Knowledge-as-a-Service (4) |
| External services push events to you | Webhook-driven (5) |
| Need scheduled tasks beyond cron tools | Background workers (6) |
| Sell to multiple resellers | White-label (7) |
| Have an existing SaaS to augment | Hybrid (8) |

---

## Microapp vs Extension — quick decision

If you're between Microapp and Extension:

- Choose **Microapp** when: you own the UI, the auth, the billing,
  and the framework runs out of view. End-users never see nexo.
- Choose **Extension** when: you're contributing functionality
  *into* the framework that operators install with `nexo ext
  install`. End-users may see your tool / advisor / skill output
  but not your code's UI.

A SaaS often combines both: a multi-tenant microapp + one or
two custom extensions for the vertical.

---

## See also

- [Microapps · getting started](./getting-started.md) — 1-hour
  walkthrough of pattern 1.
- [`agent-creator` reference](./agent-creator.md) — full
  pattern 2 implementation.
- [Admin RPC](./admin-rpc.md) — every endpoint you'll call.
- [Building microapps in Rust](./rust.md) — language-specific
  guide.
