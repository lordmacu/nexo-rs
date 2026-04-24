---
name: Endpoint Check
description: HTTP probe (status + latency) and TLS certificate inspection (expiry, issuer, SANs).
requires:
  bins: []
  env: []
---

# Endpoint Check

Use when the user wants to verify that an HTTP endpoint is alive, measure
its latency, or inspect a TLS certificate's expiry and issuer. Combines
naturally with Phase 7 heartbeat for periodic monitoring.

## Use when

- "¿Está arriba mi API?"
- "¿Cuánto falta para que venza el cert de X?"
- "Revisa mis endpoints y avísame si algo falla" (wrap in heartbeat)
- Verificar que un deploy publicó el código nuevo (compare response body)

## Do not use when

- Necesitas auth compleja (OAuth, mTLS) — usa fetch-url o una extension específica
- Querés descargar el body — usa fetch-url
- Es un health check interno a un container — usa el propio orquestador

## Tools

### `status`
No args. Info + limits.

### `http_probe { url, method?, timeout_secs?, follow_redirects?, expected_status? }`
- `url` obligatorio (http/https)
- `method` GET (default) o HEAD
- `timeout_secs` 1..60 (default 10)
- `follow_redirects` default true
- `expected_status` opcional: devuelve `matches_expected: bool`

Returns `{status, latency_ms, final_url, content_type, body_preview (≤500 chars), [matches_expected]}`.

### `ssl_cert { host, port?, timeout_secs?, warn_days? }`
- `host` obligatorio
- `port` default 443
- `timeout_secs` 1..60 (default 10)
- `warn_days` default 30 — flag `expiring_soon: true` si quedan menos

Returns `{subject, issuer, sans, serial_hex, signature_algorithm, chain_length, not_before_unix, not_after_unix, seconds_until_expiry, days_until_expiry, expiring_soon, expired}`.

Aviso: **no valida la cadena de confianza** — expired/self-signed certs
devuelven datos normalmente. Usa `expired`/`expiring_soon` para decidir.

## Execution guidance

- Para monitoreo periódico, combina con heartbeat: probe cada N minutos,
  alerta si `status` cambia o `expiring_soon` se prende.
- Para comparar estados entre deploys, guarda el primer probe en
  `state_json` del TaskFlow y compara.
- `ssl_cert` es informativo; para alertas accionables usa `days_until_expiry`
  con un umbral agresivo (14 días) en producción.
- Error `-32005` timeout → el servidor no respondió en `timeout_secs`.
- Error `-32060/-32061` en ssl_cert → DNS o TCP connect fallaron (probablemente
  el host no existe o no escucha en `port`).
