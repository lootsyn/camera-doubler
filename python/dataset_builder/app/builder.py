from __future__ import annotations

import importlib.metadata
import json
import os
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image

from .transaction import AtomicExport, ExportError, validate_cadence


def assert_lerobot_version() -> str:
    expected = os.environ.get("LEROBOT_VERSION", "0.6.0")
    actual = importlib.metadata.version("lerobot")
    if actual != expected:
        raise ExportError(f"LeRobot version mismatch: expected {expected}, got {actual}")
    if os.environ.get("LEROBOT_DATASET_FORMAT", "v3") != "v3":
        raise ExportError("only LeRobot v3 is supported")
    return actual


def export_episode(
    session_root: Path,
    episode_file: Path,
    final_path: Path,
    repository_id: str,
    fps: float,
) -> Path:
    lerobot_version = assert_lerobot_version()
    if not repository_id or "/" not in repository_id:
        raise ExportError("LeRobot repository ID must be namespace/name")
    episode = json.loads(episode_file.read_text(encoding="utf-8"))
    steps = episode.get("steps") or []
    timestamps = [int(step["capture_time_edge_ns"]) for step in steps]
    cadence = validate_cadence(
        timestamps,
        fps,
        float(os.getenv("ANCHOR_RATE_TOLERANCE_PCT", "15")),
        float(os.getenv("ANCHOR_MAX_FRAME_INTERVAL_MS", "100")),
    )
    manifest = json.loads((session_root / "manifest.json").read_text(encoding="utf-8"))
    temporary_root = Path(os.getenv("EXPORT_TEMP_ROOT", str(session_root / ".exports")))

    def build(root: Path) -> None:
        from lerobot.datasets.lerobot_dataset import LeRobotDataset

        features = _features(manifest)
        # AtomicExport reserves an empty, same-filesystem path. LeRobot 0.6.0
        # intentionally creates its root with exist_ok=False.
        root.rmdir()
        dataset = LeRobotDataset.create(
            repo_id=repository_id,
            fps=round(fps),
            features=features,
            root=root,
            use_videos=True,
        )
        for step in steps:
            frame: dict[str, Any] = {
                "task": episode.get("task", "robot episode"),
                "observation.state": np.asarray(step["observation_state"], dtype=np.float32),
                "action": np.asarray(step["action"], dtype=np.float32),
            }
            if "auxiliary" in step and step["auxiliary"]:
                frame["auxiliary"] = np.asarray(step["auxiliary"], dtype=np.float32)
            for camera_id, image_path in step.get("images", {}).items():
                with Image.open(session_root / image_path) as image:
                    frame[f"observation.images.{camera_id}"] = image.convert("RGB").copy()
            dataset.add_frame(frame)
        dataset.save_episode()
        dataset.finalize()

    def validate(root: Path) -> None:
        from lerobot.datasets.lerobot_dataset import LeRobotDataset

        loaded = LeRobotDataset(repository_id, root=root)
        if len(loaded) != len(steps):
            raise ExportError("pinned loader full scan length mismatch")
        prior = -1.0
        for index in range(len(loaded)):
            frame = loaded[index]
            timestamp = float(frame["timestamp"])
            if timestamp <= prior:
                raise ExportError("loader scan found non-monotonic timestamp")
            prior = timestamp
            if len(frame["observation.state"]) != int(manifest["observation_vector_length"]):
                raise ExportError("loader scan observation shape mismatch")
            if len(frame["action"]) != int(manifest["action_vector_length"]):
                raise ExportError("loader scan action shape mismatch")

    provenance = {
        "session_id": manifest["session_id"],
        "episode_id": episode["episode_id"],
        "manifest_revision": manifest["manifest_revision"],
        "observation_schema_id": manifest["observation_schema_id"],
        "action_schema_id": manifest["action_schema_id"],
        "camera_catalog_revision": manifest["camera_catalog_revision"],
        "lerobot_version": lerobot_version,
        "cadence_policy": os.getenv("LEROBOT_CADENCE_POLICY", "reject_irregular"),
        "cadence": cadence.__dict__,
        "source_segment_hashes": episode.get("source_segment_hashes", {}),
    }
    return AtomicExport(temporary_root, final_path).run(build, validate, provenance)


def _features(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    features: dict[str, dict[str, Any]] = {
        "observation.state": {
            "dtype": "float32",
            "shape": (int(manifest["observation_vector_length"]),),
            "names": None,
        },
        "action": {
            "dtype": "float32",
            "shape": (int(manifest["action_vector_length"]),),
            "names": None,
        },
    }
    auxiliary = int(manifest.get("auxiliary_vector_length", 0))
    if auxiliary:
        features["auxiliary"] = {"dtype": "float32", "shape": (auxiliary,), "names": None}
    for camera in manifest["cameras"]:
        if camera.get("required_for_dataset", False):
            features[f"observation.images.{camera['stable_camera_id']}"] = {
                "dtype": "video",
                "shape": (int(camera["height"]), int(camera["width"]), 3),
                "names": ["height", "width", "channels"],
            }
    return features
