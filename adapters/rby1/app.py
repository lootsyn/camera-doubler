from __future__ import annotations

import asyncio
import json
import os
import sys

from python.adapter_runtime.service import serve

from .mapping import RealRby1Backend, SyntheticRby1Backend, semantic_self_test


def backend():
    mock = os.getenv("RBY1_USE_MOCK", os.getenv("RB_Y1_USE_MOCK", "0")) == "1"
    return SyntheticRby1Backend() if mock else RealRby1Backend()


def main() -> None:
    command = sys.argv[1] if len(sys.argv) > 1 else "serve"
    instance = backend()
    if command in {"healthcheck", "self-test"}:
        print(json.dumps(semantic_self_test(instance), sort_keys=True))
        return
    if command != "serve":
        raise SystemExit(f"unknown command: {command}")
    asyncio.run(serve(instance, os.getenv("ADAPTER_SOCKET", "/run/robot-adapters/rby1.sock")))


if __name__ == "__main__":
    main()
