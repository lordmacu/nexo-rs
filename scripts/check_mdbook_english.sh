#!/usr/bin/env bash
set -euo pipefail

# Guardrail for mdBook content: keep docs in English.
# The matcher is intentionally conservative (high-signal Spanish tokens)
# to avoid false positives on names and technical terms.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PATTERN='(^|[^[:alpha:]])(eres|ayudas?|clientes?|elegir|primer|mensaje|contiene|en[[:space:]]+otro[[:space:]]+caso|pregunta|cu[aá]l|operador|captura|direcci[oó]n|estrato|preferencia|cuando|listo|invoca|contenga|intentes?|nadie|herramienta|abr[ií]|c[oó]digo|v[aá]lido|esperando|aprobaci[oó]n|siguiente|paso|habilitar|deshabilitar|configurar|pendiente|resuelto|deuda[[:space:]]+t[ée]cnica|telefon[íi]a)([^[:alpha:]]|$)|[¿¡]'

if rg -n --ignore-case -g '*.md' -e "$PATTERN" docs >/tmp/mdbook_lang_hits.txt; then
  echo "mdBook language check failed: possible Spanish text found in docs/. Please rewrite in English."
  echo
  cat /tmp/mdbook_lang_hits.txt
  exit 1
fi

echo "mdBook language check passed (no Spanish markers found)."
