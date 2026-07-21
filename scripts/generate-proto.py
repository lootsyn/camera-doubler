#!/usr/bin/env python3
"""Generate the checked-at-build-time Python protobuf/gRPC SDK."""

from __future__ import annotations

import subprocess
import sys
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "python" / "generated"


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "__init__.py").touch()
    protos = sorted((ROOT / "proto").glob("*.proto"))
    command = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"-I{ROOT / 'proto'}",
        f"--python_out={OUT}",
        f"--grpc_python_out={OUT}",
        "--pyi_out=" + str(OUT),
        *map(str, protos),
    ]
    subprocess.run(command, check=True)
    # grpcio-tools emits top-level sibling imports. Make the generated SDK a
    # normal package so callers need only `python` on PYTHONPATH.
    for generated in OUT.glob("*_pb2*.py"):
        source = generated.read_text(encoding="utf-8")
        source = re.sub(
            r"^import (\w+_pb2) as ",
            r"from . import \1 as ",
            source,
            flags=re.MULTILINE,
        )
        generated.write_text(source, encoding="utf-8")


if __name__ == "__main__":
    main()
