from __future__ import annotations

import asyncio
import os
from pathlib import Path

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

from .builder import assert_lerobot_version, export_episode
from .transaction import ExportError


class ExportRequest(BaseModel):
    session_root: str = Field(min_length=1, max_length=1024)
    episode_file: str = Field(min_length=1, max_length=1024)
    final_path: str = Field(min_length=1, max_length=1024)
    repository_id: str = Field(min_length=3, max_length=256, pattern=r"^[^/]+/[^/]+$")
    fps: float = Field(gt=0, le=240)


app = FastAPI(title="Robot Multicam Dataset Builder", version="1")
_maximum_exports = int(os.getenv("MAX_CONCURRENT_EXPORTS", "1"))
if not 1 <= _maximum_exports <= 16:
    raise RuntimeError("MAX_CONCURRENT_EXPORTS must be within 1..16")
_export_limit = asyncio.Semaphore(_maximum_exports)


@app.get("/healthz")
@app.get("/readyz")
async def health() -> dict[str, object]:
    return {"ready": True, "lerobot_version": assert_lerobot_version()}


@app.post("/v1/exports")
async def create_export(request: ExportRequest) -> dict[str, str]:
    data_root = Path(os.getenv("DATA_ROOT", "/data")).resolve(strict=True)
    try:
        session_root = _confined(data_root, request.session_root, must_exist=True)
        episode_file = _confined(data_root, request.episode_file, must_exist=True)
        final_path = _confined(data_root, request.final_path, must_exist=False)
        if not session_root.is_dir() or not episode_file.is_file():
            raise ExportError("session_root/episode_file type mismatch")
        async with _export_limit:
            result = await asyncio.to_thread(
                export_episode,
                session_root,
                episode_file,
                final_path,
                request.repository_id,
                request.fps,
            )
        return {"status": "committed", "path": str(result.relative_to(data_root))}
    except (ExportError, OSError, ValueError) as error:
        raise HTTPException(status_code=422, detail=str(error)[:512]) from error


def _confined(root: Path, raw: str, must_exist: bool) -> Path:
    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = root / candidate
    resolved = candidate.resolve(strict=must_exist)
    if resolved == root or root not in resolved.parents:
        raise ExportError("dataset path escapes DATA_ROOT")
    return resolved


def main() -> None:
    assert_lerobot_version()
    host, raw_port = os.getenv("API_BIND", "0.0.0.0:8090").rsplit(":", 1)
    uvicorn.run(
        app,
        host=host,
        port=int(raw_port),
        workers=1,
        limit_concurrency=int(os.getenv("API_MAX_CONCURRENCY", "32")),
        access_log=False,
    )


if __name__ == "__main__":
    main()
