from __future__ import annotations

import hashlib
import json
import math
import os
import shutil
import statistics
import uuid
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable, Iterable, Sequence


class ExportError(RuntimeError):
    pass


@dataclass(frozen=True)
class CadenceReport:
    fps: float
    expected_interval_ns: int
    median_interval_ns: int
    maximum_interval_ns: int
    maximum_jitter_ns: int
    sample_count: int


def validate_cadence(
    timestamps_ns: Sequence[int],
    fps: float,
    tolerance_percent: float,
    maximum_interval_ms: float,
) -> CadenceReport:
    if len(timestamps_ns) < 2 or fps <= 0 or tolerance_percent < 0 or maximum_interval_ms <= 0:
        raise ExportError("invalid cadence contract")
    if any(value <= 0 for value in timestamps_ns) or any(
        right <= left for left, right in zip(timestamps_ns, timestamps_ns[1:])
    ):
        raise ExportError("timestamps must be nonzero and strictly monotonic")
    expected = round(1_000_000_000 / fps)
    intervals = [right - left for left, right in zip(timestamps_ns, timestamps_ns[1:])]
    tolerance = expected * tolerance_percent / 100
    maximum = round(maximum_interval_ms * 1_000_000)
    if any(abs(interval - expected) > tolerance or interval > maximum for interval in intervals):
        raise ExportError("irregular anchor cadence cannot be labelled with nominal FPS")
    return CadenceReport(
        fps=fps,
        expected_interval_ns=expected,
        median_interval_ns=round(statistics.median(intervals)),
        maximum_interval_ns=max(intervals),
        maximum_jitter_ns=max(abs(interval - expected) for interval in intervals),
        sample_count=len(timestamps_ns),
    )


def fixed_grid_nearest(
    timestamps_ns: Sequence[int], fps: float, tolerance_ns: int
) -> list[int]:
    if not timestamps_ns or fps <= 0 or tolerance_ns < 0:
        raise ExportError("invalid fixed grid contract")
    period = round(1_000_000_000 / fps)
    selected: list[int] = []
    used: set[int] = set()
    grid = timestamps_ns[0]
    while grid <= timestamps_ns[-1]:
        index = min(range(len(timestamps_ns)), key=lambda item: abs(timestamps_ns[item] - grid))
        if index in used or abs(timestamps_ns[index] - grid) > tolerance_ns:
            raise ExportError("fixed grid would reuse or synthesize a frame")
        used.add(index)
        selected.append(index)
        grid += period
    return selected


def checksum_tree(root: Path) -> dict[str, str]:
    checksums: dict[str, str] = {}
    for path in sorted(item for item in root.rglob("*") if item.is_file()):
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        checksums[path.relative_to(root).as_posix()] = digest.hexdigest()
    return checksums


class AtomicExport:
    def __init__(self, temporary_root: Path, final_path: Path) -> None:
        self.temporary_root = temporary_root
        self.final_path = final_path

    def run(
        self,
        build: Callable[[Path], None],
        validate: Callable[[Path], None],
        provenance: dict,
    ) -> Path:
        self.temporary_root.mkdir(parents=True, exist_ok=True)
        self.final_path.parent.mkdir(parents=True, exist_ok=True)
        if self.final_path.exists():
            raise ExportError("committed dataset already exists")
        if os.stat(self.temporary_root).st_dev != os.stat(self.final_path.parent).st_dev:
            raise ExportError("atomic export requires temporary and final paths on one filesystem")
        temporary = self.temporary_root / f"export-{uuid.uuid4()}"
        failed = self.temporary_root / "failed"
        temporary.mkdir(mode=0o750)
        try:
            build(temporary)
            validate(temporary)
            payload = dict(provenance)
            payload["checksums_sha256"] = checksum_tree(temporary)
            payload_path = temporary / "export-provenance.json"
            payload_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            _fsync_tree(temporary)
            os.replace(temporary, self.final_path)
            _fsync_directory(self.final_path.parent)
            return self.final_path
        except Exception:
            failed.mkdir(exist_ok=True)
            quarantine = failed / temporary.name
            if temporary.exists():
                os.replace(temporary, quarantine)
            raise


def _fsync_tree(root: Path) -> None:
    for path in (item for item in root.rglob("*") if item.is_file()):
        with path.open("rb") as handle:
            os.fsync(handle.fileno())
    for path in sorted((item for item in root.rglob("*") if item.is_dir()), reverse=True):
        _fsync_directory(path)
    _fsync_directory(root)


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
