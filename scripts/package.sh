#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-${ROOT_DIR}.zip}"
cd "$ROOT_DIR"
./scripts/validate-package.py
python3 - "$ROOT_DIR" "$OUT" <<'PY'
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import sys
import zipfile

root = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
excluded_roots = {
    ".git",
    ".cargo-home",
    ".rustup-home",
    ".tools",
    ".venv-rby1",
    ".venv-dataset",
    ".venv-tools",
    "target",
}
excluded_env = {
    ".env.edge",
    ".env.receiver",
    ".env.dataset-builder",
    ".env.adapter-rby1",
}


def included(path: Path) -> bool:
    rel = path.relative_to(root)
    if not path.is_file() or not rel.parts:
        return False
    if rel.parts[0] in excluded_roots:
        return False
    if rel.parts[:2] == ("validation", "runtime"):
        return False
    if rel.parts[0] == "secrets" and rel.name not in {".gitignore", "README.md"}:
        return False
    if "__pycache__" in rel.parts or rel.suffix == ".pyc":
        return False
    if rel.name in excluded_env or rel.name == "SHA256SUMS" or rel.suffix == ".zip":
        return False
    return path.resolve() != out


files = []
for directory, directory_names, file_names in os.walk(root, topdown=True, followlinks=False):
    directory_path = Path(directory)
    relative_directory = directory_path.relative_to(root)
    if relative_directory == Path("."):
        directory_names[:] = [name for name in directory_names if name not in excluded_roots]
    elif relative_directory == Path("validation"):
        directory_names[:] = [name for name in directory_names if name != "runtime"]
    directory_names[:] = [name for name in directory_names if name != "__pycache__"]
    for name in file_names:
        path = directory_path / name
        if included(path):
            files.append(path)
files.sort(key=lambda path: path.relative_to(root).as_posix())
checksum_lines = []
for path in files:
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    checksum_lines.append(f"{digest}  {path.relative_to(root).as_posix()}\n")
(root / "SHA256SUMS").write_text("".join(checksum_lines), encoding="utf-8", newline="\n")
files.append(root / "SHA256SUMS")

with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for path in files:
        archive.write(path, Path(root.name) / path.relative_to(root))
print(out)
PY
sha256sum "$OUT"
./scripts/validate-package.py "$OUT"
