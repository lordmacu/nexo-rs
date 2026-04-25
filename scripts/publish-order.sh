#!/usr/bin/env bash
# Print crate publish order (topological by runtime deps; dev-deps ignored).
# Usage: scripts/publish-order.sh [--dry-run | --publish]
set -euo pipefail
cd "$(dirname "$0")/.."

mode="${1:-list}"

mapfile -t LAYERS < <(python3 - <<'PY'
import re, sys
from pathlib import Path
def runtime_deps(p):
    txt=p.read_text()
    parts=re.split(r'^\[([^\]]+)\]\s*$', txt, flags=re.M)
    deps=set()
    for i in range(1,len(parts),2):
        sec=parts[i].strip(); body=parts[i+1]
        if sec=="dependencies" or (sec.startswith("target.") and sec.endswith(".dependencies")):
            deps.update(re.findall(r'^(nexo-[a-z\-]+)\s*=', body, re.M))
    return deps
crates={}
for c in list(Path("crates").glob("*/Cargo.toml"))+list(Path("crates/plugins").glob("*/Cargo.toml")):
    txt=c.read_text()
    m=re.search(r'^name\s*=\s*"([^"]+)"', txt, re.M)
    if not m: continue
    crates[m.group(1)]=runtime_deps(c)-{m.group(1)}
rem=dict(crates)
while rem:
    layer=sorted([n for n,d in rem.items() if not (d & set(rem))])
    if not layer: print("CYCLE:",rem,file=sys.stderr); sys.exit(1)
    print(" ".join(layer))
    for n in layer: rem.pop(n)
PY
)

case "$mode" in
  list)
    for i in "${!LAYERS[@]}"; do
      echo "L$((i+1)): ${LAYERS[$i]}"
    done
    ;;
  --dry-run)
    for layer in "${LAYERS[@]}"; do
      for crate in $layer; do
        echo "==> dry-run $crate"
        cargo publish --dry-run -p "$crate"
      done
    done
    ;;
  --publish)
    for layer in "${LAYERS[@]}"; do
      for crate in $layer; do
        echo "==> publish $crate"
        cargo publish -p "$crate"
        # crates.io index propagation delay
        sleep 30
      done
    done
    ;;
  *)
    echo "usage: $0 [list|--dry-run|--publish]" >&2
    exit 2
    ;;
esac
