#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-${ROOT_DIR}.zip}"
cd "$ROOT_DIR"
./scripts/validate-package.py
find . -type f \
  ! -path './.git/*' \
  ! -path './scripts/__pycache__/*' \
  ! -name '*.pyc' \
  ! -name 'SHA256SUMS' \
  ! -name '*.zip' \
  -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
python - "$ROOT_DIR" "$OUT" <<'PY'
from pathlib import Path
import sys, zipfile
root=Path(sys.argv[1]).resolve(); out=Path(sys.argv[2]).resolve()
exclude={'.env.edge','.env.receiver','.env.dataset-builder','.env.adapter-rby1'}
with zipfile.ZipFile(out,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=9) as z:
    for p in sorted(root.rglob('*')):
        if not p.is_file(): continue
        rel=p.relative_to(root)
        if rel.name in exclude or '__pycache__' in rel.parts or rel.suffix=='.pyc': continue
        if rel.parts and rel.parts[0]=='secrets' and rel.name not in {'.gitignore','README.md'}: continue
        z.write(p, Path(root.name)/rel)
print(out)
PY
sha256sum "$OUT"
