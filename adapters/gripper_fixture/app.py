from __future__ import annotations

import asyncio
import os
import sys

from adapters.template.app import TemplateBackend
from python.adapter_runtime.service import serve


def main() -> None:
    backend = TemplateBackend("right_gripper", 1)
    command = sys.argv[1] if len(sys.argv) > 1 else "serve"
    if command in {"healthcheck", "self-test"}:
        backend.command("position", [0.75])
        for _ in range(8):
            sample = backend.sample()
        actual = sample.values["right_gripper.position"][0]
        if abs(actual - 0.75) > 0.1:
            raise SystemExit("gripper fixture convergence failed")
        print("gripper fixture PASS")
        return
    asyncio.run(serve(backend, os.getenv("ADAPTER_SOCKET", "/run/robot-adapters/right-gripper.sock")))


if __name__ == "__main__":
    main()
