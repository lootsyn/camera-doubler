from __future__ import annotations

import json
from pathlib import Path

import pytest

from app.transaction import AtomicExport, ExportError, fixed_grid_nearest, validate_cadence


def test_irregular_cadence_is_rejected() -> None:
    with pytest.raises(ExportError):
        validate_cadence([1, 33_333_334, 100_000_000], 30, 15, 100)


def test_fixed_grid_forbids_reuse() -> None:
    assert fixed_grid_nearest([1, 33_333_334, 66_666_667], 30, 2) == [0, 1, 2]
    with pytest.raises(ExportError):
        fixed_grid_nearest([1, 60_000_000], 30, 5_000_000)


def test_atomic_export_preserves_old_data_and_quarantines_failure(tmp_path: Path) -> None:
    temporary = tmp_path / "temporary"
    final = tmp_path / "dataset"

    def build(root: Path) -> None:
        (root / "data.json").write_text("{}", encoding="utf-8")

    AtomicExport(temporary, final).run(build, lambda root: None, {"test": True})
    assert json.loads((final / "export-provenance.json").read_text())["test"] is True
    with pytest.raises(ExportError):
        AtomicExport(temporary, final).run(build, lambda root: None, {})

    another = tmp_path / "broken"
    with pytest.raises(RuntimeError):
        AtomicExport(temporary, another).run(build, lambda root: (_ for _ in ()).throw(RuntimeError("scan")), {})
    assert list((temporary / "failed").iterdir())
