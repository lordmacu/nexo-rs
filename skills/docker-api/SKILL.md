---
name: Docker API
description: Inspeccionar y controlar containers Docker locales.
requires:
  bins: [docker]
  env: []
---

# Docker API

Wraps el CLI `docker` para que kate pueda reportar/controlar containers.
Reads son libres; lifecycle (start/stop/restart) gated por `DOCKER_API_ALLOW_WRITE=true`.

## Tools
- `status`
- `ps(all?, filter?)` — list containers
- `inspect(target)` — full metadata
- `logs(target, lines?, since?)` — last N lines
- `stats(target)` — one-shot CPU/mem/net
- `start†(target)`, `stop†(target, timeout_secs?)`, `restart†(target, timeout_secs?)`

Container names regex-validated `[A-Za-z0-9][A-Za-z0-9_.-]*` para blocking shell injection.
