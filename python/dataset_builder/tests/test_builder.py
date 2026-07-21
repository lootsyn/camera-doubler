from __future__ import annotations

import json
from pathlib import Path

from app.builder import export_episode


def test_exact_lerobot_export_finalize_reload_and_full_scan(tmp_path: Path, monkeypatch) -> None:
    session = tmp_path / "session"
    session.mkdir()
    manifest = {
        "session_id": "01010101-0101-0101-0101-010101010101",
        "manifest_revision": 1,
        "observation_schema_id": 11,
        "action_schema_id": 12,
        "camera_catalog_revision": 1,
        "observation_vector_length": 2,
        "action_vector_length": 2,
        "auxiliary_vector_length": 0,
        "cameras": [],
    }
    (session / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    episode = {
        "episode_id": "episode-1",
        "task": "contract test",
        "source_segment_hashes": {"anchor.ts": "ab" * 32},
        "steps": [
            {
                "capture_time_edge_ns": 1_000_000_000 + index * 33_333_333,
                "observation_state": [float(index), float(index + 1)],
                "action": [0.1, -0.1],
            }
            for index in range(4)
        ],
    }
    episode_path = session / "episode.json"
    episode_path.write_text(json.dumps(episode), encoding="utf-8")
    monkeypatch.setenv("EXPORT_TEMP_ROOT", str(tmp_path / "temporary"))
    monkeypatch.setenv("LEROBOT_VERSION", "0.6.0")
    monkeypatch.setenv("LEROBOT_DATASET_FORMAT", "v3")
    final = tmp_path / "committed"
    result = export_episode(
        session,
        episode_path,
        final,
        "local/robot-multicam-contract",
        30.0,
    )
    assert result == final
    provenance = json.loads((final / "export-provenance.json").read_text(encoding="utf-8"))
    assert provenance["lerobot_version"] == "0.6.0"
    assert provenance["checksums_sha256"]
