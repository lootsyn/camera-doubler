from __future__ import annotations

from pathlib import Path

import pytest

from app.__main__ import _confined
from app.transaction import ExportError


def test_export_paths_are_confined_to_data_root(tmp_path: Path) -> None:
    root = tmp_path / "data"
    root.mkdir()
    episode = root / "session" / "episode.json"
    episode.parent.mkdir()
    episode.write_text("{}", encoding="utf-8")
    assert _confined(root, "session/episode.json", must_exist=True) == episode
    with pytest.raises(ExportError):
        _confined(root, "../escape", must_exist=False)
