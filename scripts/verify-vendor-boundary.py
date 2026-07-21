#!/usr/bin/env python3
from __future__ import annotations

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
GENERIC = [ROOT / "crates" / "edge-core", ROOT / "crates" / "receiver"]
forbidden = re.compile(r"rby1[_-]sdk|import\s+rby1|use\s+rby1", re.IGNORECASE)
violations: list[str] = []
for base in GENERIC:
    for path in base.rglob("*"):
        if path.is_file() and path.suffix in {".rs", ".toml", ".py"}:
            if forbidden.search(path.read_text(encoding="utf-8")):
                violations.append(str(path.relative_to(ROOT)))
if violations:
    raise SystemExit("vendor dependency leaked into Generic Core/Receiver: " + ", ".join(violations))
print("vendor boundary PASS")
