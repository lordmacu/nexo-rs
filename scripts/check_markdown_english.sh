#!/usr/bin/env bash
set -euo pipefail

# Repository-wide guardrail: keep Markdown in English.
# The pattern is intentionally conservative to avoid false positives on
# proper names or technical tokens.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PATTERN='(^|[^[:alpha:]])(deuda[[:space:]]+t[ée]cnica|regla|[uú]ltima|s[ií]ntoma|causas|fallaba|fallado|abierto|resuelto|pendiente|migraci[oó]n|acci[oó]n|sesi[oó]n|archivo|herramienta|leer|crear|nodos|fases|bloquean|reconexi[oó]n|validaci[oó]n|integraci[oó]n|m[ée]tricas|telemetr[ií]a|paginaci[oó]n|auditor[ií]a|autenticaci[oó]n|corrupci[oó]n|configuraci[oó]n|duraci[oó]n|pr[oó]ximo|a[nñ]adir|diferid[oa]s?|vac[ií]o|inv[aá]lido|v[aá]lido|construcci[oó]n|operador|pregunta|siguiente[[:space:]]+paso|qu[eé]|cu[aá]ndo|d[oó]nde|qui[eé]n|hola|gracias)([^[:alpha:]]|$)|[¿¡]'

# Keep legal docs exempt from linguistic checks due proper names.
if rg -n --ignore-case -g '*.md' \
  -g '!docs/src/license.md' \
  -g '!docs/src/adr/0009-dual-license.md' \
  -e "$PATTERN" . >/tmp/markdown_lang_hits.txt; then
  echo "Markdown language check failed: possible Spanish text found. Please rewrite in English."
  echo
  cat /tmp/markdown_lang_hits.txt
  exit 1
fi

echo "Markdown language check passed (no Spanish markers found)."
