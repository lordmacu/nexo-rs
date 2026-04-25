# Rotating config without downtime

Three practical hot-reload scenarios. Each shows the YAML edit, how to
trigger the swap, and what the operator should see in the logs and on
the metrics endpoint. Reference: [Config hot-reload](../ops/hot-reload.md).

## Prerequisites

- A running daemon (`agent` in another terminal or under systemd).
- Broker reachable from the same host (`broker.yaml`).
- Phase 16 + Phase 18 features enabled (default since `0.x` of nexo-rs).

A quick sanity check:

```bash
$ agent reload
reload v1: applied=1 rejected=0 elapsed=14ms
  ✓ ana
```

If you get exit 1 with "no `control.reload.ack` received within 5s",
the daemon isn't running or `runtime.reload.enabled` is `false` —
fix that first.

---

## 1. Rotate an LLM API key

The Anthropic key on production rotates every 90 days. Old key still
valid for an hour after the rotation.

### Edit

`config/llm.yaml`:

```diff
 providers:
   anthropic:
-    api_key: ${file:./secrets/anthropic_old.txt}
+    api_key: ${file:./secrets/anthropic_new.txt}
     base_url: https://api.anthropic.com
```

### Apply

```bash
# Drop the new key first, THEN trigger the reload — the file watcher
# would also do it 500 ms after the save, the CLI is just explicit.
$ printf '%s' "sk-ant-..." > secrets/anthropic_new.txt
$ chmod 600 secrets/anthropic_new.txt
$ agent reload
reload v2: applied=2 rejected=0 elapsed=22ms
  ✓ ana
  ✓ bob
```

### Verify

```bash
# The aggregate counter bumped:
$ curl -s localhost:9090/metrics | grep config_reload_applied_total
config_reload_applied_total 2

# Per-agent versions advanced:
$ curl -s localhost:9090/metrics | grep runtime_config_version
runtime_config_version{agent_id="ana"} 2
runtime_config_version{agent_id="bob"} 2

# Watch one agent's next turn — the new key is used by the LlmClient
# rebuilt inside RuntimeSnapshot::build:
$ tail -f agent.log | grep "llm request"
```

In-flight LLM calls keep using the old client (the in-flight `Arc<dyn
LlmClient>` is captured per-turn). They land in <30 s; the old key is
still valid for the hour the auth team gave you.

---

## 2. A/B test a system prompt

You want to roll out a friendlier sales pitch on Ana's WhatsApp
binding without touching the Telegram one (which has a longer
support persona).

### Edit

`config/agents.d/ana.yaml`:

```diff
 inbound_bindings:
   - plugin: whatsapp
     allowed_tools: [whatsapp_send_message]
     outbound_allowlist:
       whatsapp: ["573115728852"]
-    system_prompt_extra: |
-      Channel: WhatsApp sales. Follow the ETB/Claro lead-capture flow.
+    system_prompt_extra: |
+      Channel: WhatsApp sales (variant B — warmer tone).
+      Follow the ETB/Claro lead-capture flow but lead with a personal
+      greeting and use first names.
   - plugin: telegram
     instance: ana_tg
     allowed_tools: ["*"]
     ...
```

### Apply

The file watcher picks the save up automatically:

```bash
$ tail -f agent.log
INFO config reload applied version=3 applied=["ana"] rejected_count=0 elapsed_ms=18
```

Or trigger manually:

```bash
$ agent reload
reload v3: applied=1 rejected=0 elapsed=18ms
  ✓ ana
```

### Verify

Send one message on each channel and tail the LLM request log to see
which prompt block went to the model.

```bash
$ grep "snapshot_version=3" agent.log
INFO inbound matched binding agent_id=ana plugin=whatsapp \
  binding_index=0 snapshot_version=3
```

Telegram binding's system_prompt_extra is unchanged; only the WA
binding picks up variant B.

### Roll back

If variant B underperforms, `git revert` the YAML and `agent reload`.
Sessions in flight finish their turn on B; the next inbound is back
on A.

---

## 3. Tighten an outbound allowlist after an incident

A jailbroken prompt almost made Ana send WhatsApp messages to
arbitrary numbers (Phase 16's defense-in-depth caught it). Until you
investigate, narrow the allowlist to the on-call advisor only.

### Edit

`config/agents.d/ana.yaml`:

```diff
 inbound_bindings:
   - plugin: whatsapp
     allowed_tools: [whatsapp_send_message]
     outbound_allowlist:
       whatsapp:
-        - "573115728852"
-        - "573215555555"
-        - "573009999999"
+        - "573115728852"   # incident-only: on-call advisor
```

### Apply

```bash
$ agent reload
reload v4: applied=1 rejected=0 elapsed=15ms
  ✓ ana
```

### Verify

Try the previously-allowed-but-now-blocked number from a test message.
The LLM will try; the tool will reject:

```
ERROR tool_call rejected reason="recipient 573215555555 is not in \
  this agent's whatsapp outbound allowlist"
```

The session's `Arc<RuntimeSnapshot>` is captured at the start of each
turn, so even mid-conversation the next user reply re-loads from the
new snapshot and the allowlist update takes effect immediately.

---

## What you cannot reload (yet)

- **Adding or removing agents** — restart the daemon. Phase 19.
- **Plugin instances** (`whatsapp.yaml`, `telegram.yaml` instance
  blocks) — restart the daemon. Plugin sessions own QR pairing /
  long-polling state that needs lifecycle plumbing. Phase 19.
- **`broker.yaml`, `memory.yaml`** — restart the daemon. Long-lived
  connections + storage handles aren't safe to swap mid-flight.
- **`workspace`, `skills_dir`, `transcripts_dir`** on an agent —
  restart that agent.

The daemon logs every restart-required field that changed during a
reload as `warn` so you don't have to remember which knob lives where.

## See also

- [Config hot-reload](../ops/hot-reload.md) — full behaviour reference
- [agents.yaml](../config/agents.md) — per-binding override surface
- [Per-agent credentials](../config/credentials.md) — credential
  rotation has its own `POST /admin/credentials/reload` endpoint
- [Metrics](../ops/metrics.md) — `config_reload_*` series
