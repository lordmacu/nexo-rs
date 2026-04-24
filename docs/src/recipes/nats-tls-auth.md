# NATS with TLS + auth

Harden the broker for a multi-node deployment: mTLS on the client
connection, NKey-based authentication, and a separate NATS server
process (not the throwaway Docker-compose one).

## Prerequisites

- A NATS server ≥ 2.10
- `nsc` CLI for generating NKeys
- The agent binary deployed where it will run

## 1. Generate NKeys

```bash
nsc add operator --generate-signing-key nexo-ops
nsc add account --name nexo-prod
nsc add user --name agent-kate --account nexo-prod
nsc generate creds --account nexo-prod --name agent-kate > secrets/agent-kate.nkey
```

`secrets/agent-kate.nkey` is a single-file credential that contains
both the NKey seed and the signed JWT. Treat it like any other
secret — gitignored, Docker-secret, k8s-secret.

## 2. Configure the NATS server

`nats-server.conf`:

```conf
listen: 0.0.0.0:4222
http: 0.0.0.0:8222

tls {
  cert_file: "/etc/nats/tls/server.crt"
  key_file:  "/etc/nats/tls/server.key"
  ca_file:   "/etc/nats/tls/ca.crt"
  verify:    true       # require client certs too (mTLS)
}

authorization {
  operator = "/etc/nats/nsc/operator.jwt"
  resolver = MEMORY
  accounts = [
    { name: nexo-prod, jwt: "/etc/nats/nsc/nexo-prod.jwt" }
  ]
}
```

Start the server:

```bash
nats-server -c nats-server.conf
```

## 3. Configure the agent

`config/broker.yaml`:

```yaml
broker:
  type: nats
  url: tls://nats.example.com:4222
  auth:
    enabled: true
    nkey_file: ./secrets/agent-kate.nkey
  persistence:
    enabled: true
    path: ./data/queue
  fallback:
    mode: local_queue
    drain_on_reconnect: true
```

The agent reads `nkey_file` at startup and presents it on every
connection.

## 4. Verify the client

Before starting the full agent, smoke-test the credentials with the
`nats` CLI:

```bash
nats --creds ./secrets/agent-kate.nkey \
     --tlsca /etc/nats/tls/ca.crt \
     -s tls://nats.example.com:4222 \
     pub test.topic "hello"
```

If this works, the agent will too.

## 5. Deploy

Start the agent as usual:

```bash
agent --config ./config
```

On boot the agent:

1. Opens a TLS connection to the broker
2. Presents its NKey + JWT
3. Server validates against the operator/account JWT
4. Subscribes only to subjects its account is allowed to access

## 6. Multi-agent isolation

Give each agent its own NKey and an **export/import** declaration in
the NSC account so agents can talk to each other on specific
subjects only. Example policy:

```
# allow kate to publish agent.route.ops
# deny kate from publishing plugin.outbound.* (only the WA plugin should)
```

The agent does not enforce NATS auth itself — it just presents
credentials. The broker enforces. That's the point: you can revoke a
compromised agent without touching the agent's code or config.

## Observability

- `circuit_breaker_state{breaker="nats"}` flips to `1` if the
  broker rejects the credentials on startup or after a refresh
- `disk queue` buffers every publish while the circuit is open — see
  [Event bus — disk queue](../architecture/event-bus.md#disk-queue)
- `nats --trace` on the server side logs every auth failure with
  the rejected subject

## Gotchas

- **`verify: true` (mTLS)** requires client certs **and** NKey auth.
  Picking one or the other is a policy choice — don't half-configure.
- **JWT expiry.** Account JWTs expire; NSC's `push` command renews
  them against the resolver.
- **Disk queue on client side.** Even with auth misconfigured, the
  agent keeps running on the local fallback; operators may miss the
  outage without alerting on `circuit_breaker_state`.

## Cross-links

- [Config — broker.yaml](../config/broker.md)
- [Event bus](../architecture/event-bus.md)
- [Fault tolerance — CircuitBreaker](../architecture/fault-tolerance.md#circuitbreaker)
