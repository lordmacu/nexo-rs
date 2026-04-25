#!/usr/bin/env bash
# Print the current crates.io max-version for every nexo-* crate in this
# workspace (and the workspace bin). Empty column = not yet published.
#
# Uses the sparse index — anonymous, no auth, no rate-limited API.
set -euo pipefail
cd "$(dirname "$0")/.."

UA="${CARGO_HTTP_USER_AGENT:-nexo-publish-check (informacion@cristiangarcia.co)}"

mapfile -t CRATES < <(python3 - <<'PY'
import re
from pathlib import Path
out=[]
for c in list(Path("crates").glob("*/Cargo.toml"))+list(Path("crates/plugins").glob("*/Cargo.toml")):
    m=re.search(r'^name\s*=\s*"([^"]+)"', c.read_text(), re.M)
    if m: out.append(m.group(1))
m=re.search(r'^name\s*=\s*"([^"]+)"', Path("Cargo.toml").read_text(), re.M)
if m: out.append(m.group(1))
print("\n".join(sorted(set(out))))
PY
)

local_ver=$(grep -E '^version' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')

printf "%-22s %-12s %s\n" "crate" "registry" "local=$local_ver"
printf "%-22s %-12s %s\n" "---------------------" "------------" "----------"
for c in "${CRATES[@]}"; do
  l=${#c}
  if   [ "$l" -le 2 ]; then p="$l/$c"
  elif [ "$l" -eq 3 ]; then p="3/${c:0:1}/$c"
  else                      p="${c:0:2}/${c:2:2}/$c"
  fi
  status=$(curl -sS -o /tmp/.idx.$$ -w "%{http_code}" -A "$UA" "https://index.crates.io/$p" || echo "000")
  if [ "$status" = "200" ]; then
    reg=$(awk 'END{print}' /tmp/.idx.$$ | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('vers','?'))")
  else
    reg="—"
  fi
  if [ "$reg" = "$local_ver" ]; then
    flag="up-to-date"
  elif [ "$reg" = "—" ]; then
    flag="NEW"
  else
    flag="NEEDS PUBLISH"
  fi
  printf "%-22s %-12s %s\n" "$c" "$reg" "$flag"
done
rm -f /tmp/.idx.$$
