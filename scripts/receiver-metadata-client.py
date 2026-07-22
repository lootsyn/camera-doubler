#!/usr/bin/env python3
"""Inspect Receiver metadata and optionally view synchronized H.264 access units."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import uuid
from pathlib import Path
from typing import BinaryIO

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

try:
    import grpc
    from google.protobuf.json_format import MessageToDict
    from generated import frame_metadata_pb2 as metadata_pb
    from generated import receiver_api_pb2 as receiver_pb
    from generated import receiver_api_pb2_grpc as receiver_grpc
except ImportError as error:  # pragma: no cover - operator guidance
    raise SystemExit(
        "metadata client dependencies/generated protobuf are missing; run "
        "`python scripts/generate-proto.py` and install "
        "`python/metadata_client/requirements.txt`"
    ) from error


def session_bytes(value: str) -> bytes:
    try:
        return uuid.UUID(value).bytes
    except ValueError as error:
        raise argparse.ArgumentTypeError("session must be a canonical UUID") from error


def message_dict(message) -> dict:
    return MessageToDict(
        message,
        preserving_proto_field_name=True,
        always_print_fields_with_no_presence=True,
    )


def connect(endpoint: str, timeout: float):
    channel = grpc.insecure_channel(
        endpoint,
        options=[
            ("grpc.max_receive_message_length", 16 * 1024 * 1024),
            ("grpc.max_send_message_length", 1024 * 1024),
        ],
    )
    try:
        grpc.channel_ready_future(channel).result(timeout=timeout)
    except grpc.FutureTimeoutError as error:
        channel.close()
        raise SystemExit(f"Receiver gRPC endpoint is unavailable: {endpoint}") from error
    return channel, receiver_grpc.ReceiverMetadataStub(channel)


def snapshot(args: argparse.Namespace) -> int:
    channel, stub = connect(args.endpoint, args.connect_timeout)
    try:
        session = args.session
        cameras = stub.ListCameras(
            receiver_pb.ListCamerasRequest(session_id=session), timeout=args.timeout
        )
        anchor = stub.GetAnchor(
            receiver_pb.GetAnchorRequest(session_id=session), timeout=args.timeout
        )
        quality = stub.GetSessionQuality(
            receiver_pb.GetSessionQualityRequest(session_id=session), timeout=args.timeout
        )
        manifest_response = stub.GetSessionManifest(
            receiver_pb.GetSessionManifestRequest(session_id=session), timeout=args.timeout
        )
        manifest = metadata_pb.SessionManifestV1()
        manifest.ParseFromString(manifest_response.serialized_session_manifest)
        print(
            json.dumps(
                {
                    "session_id": str(uuid.UUID(bytes=session)),
                    "cameras": message_dict(cameras).get("cameras", []),
                    "anchor": message_dict(anchor),
                    "quality": message_dict(quality),
                    "manifest": message_dict(manifest),
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        return 0
    except grpc.RpcError as error:
        raise SystemExit(
            f"Receiver snapshot failed: {error.code().name}: {error.details()}"
        ) from error
    finally:
        channel.close()


def safe_camera_name(camera_id: str) -> str:
    value = re.sub(r"[^A-Za-z0-9._-]+", "_", camera_id).strip("._")
    return value or "camera"


def open_player(camera_id: str) -> subprocess.Popen[bytes]:
    try:
        return subprocess.Popen(
            [
                "ffplay",
                "-loglevel",
                "warning",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-f",
                "h264",
                "-window_title",
                f"Receiver {camera_id}",
                "-i",
                "-",
            ],
            stdin=subprocess.PIPE,
        )
    except FileNotFoundError as error:
        raise SystemExit("ffplay is required when --view-camera is used") from error


def watch(args: argparse.Namespace) -> int:
    channel, stub = connect(args.endpoint, args.connect_timeout)
    include_images = bool(args.dump_dir or args.view_camera)
    output_root = Path(args.dump_dir).resolve() if args.dump_dir else None
    if output_root:
        output_root.mkdir(parents=True, exist_ok=True)
    files: dict[str, BinaryIO] = {}
    players: dict[str, subprocess.Popen[bytes]] = {}
    requested_players = set(args.view_camera)
    steps = 0
    request = receiver_pb.SubscribeSynchronizedStepsRequest(
        session_id=args.session,
        include_encoded_images=include_images,
        camera_ids=args.camera,
    )
    try:
        stream = stub.SubscribeSynchronizedSteps(request, timeout=args.stream_timeout)
        for step in stream:
            record = {
                "session_id": str(uuid.UUID(bytes=step.session_id)),
                "manifest_revision": step.manifest_revision,
                "capture_time_edge_ns": step.capture_time_edge_ns,
                "valid": step.valid,
                "invalid_reason": step.invalid_reason,
                "observation_length": len(step.observation_state),
                "action_length": len(step.action),
                "auxiliary_length": len(step.auxiliary),
                "anchor_context_packet_sha256": hashlib.sha256(
                    step.anchor_context_packet.SerializeToString(deterministic=True)
                ).hexdigest(),
                "frames": [
                    {
                        "camera_id": frame.camera_id,
                        "capture_time_edge_ns": frame.capture_time_edge_ns,
                        "skew_from_anchor_ns": frame.skew_from_anchor_ns,
                        "stream_epoch": frame.stream_epoch,
                        "normalized_pts_ns": frame.normalized_pts_ns,
                        "access_unit_ordinal": frame.access_unit_ordinal,
                        "encoded_bytes": len(frame.encoded_image),
                    }
                    for frame in step.frames
                ],
                "device_quality": [message_dict(item) for item in step.device_quality],
            }
            if args.vectors:
                record.update(
                    observation_state=list(step.observation_state),
                    action=list(step.action),
                    auxiliary=list(step.auxiliary),
                    anchor_context=message_dict(step.anchor_context),
                )
            print(json.dumps(record, ensure_ascii=False), flush=True)
            for frame in step.frames:
                if not frame.encoded_image:
                    continue
                if output_root:
                    writer = files.get(frame.camera_id)
                    if writer is None:
                        writer = (output_root / f"{safe_camera_name(frame.camera_id)}.h264").open("ab")
                        files[frame.camera_id] = writer
                    writer.write(frame.encoded_image)
                    writer.flush()
                if frame.camera_id in requested_players:
                    player = players.get(frame.camera_id)
                    if player is None:
                        player = open_player(frame.camera_id)
                        players[frame.camera_id] = player
                    if player.stdin and player.poll() is None:
                        try:
                            player.stdin.write(frame.encoded_image)
                            player.stdin.flush()
                        except BrokenPipeError:
                            player.stdin.close()
            steps += 1
            if args.max_steps and steps >= args.max_steps:
                stream.cancel()
                break
        return 0
    except grpc.RpcError as error:
        if error.code() == grpc.StatusCode.CANCELLED and args.max_steps:
            return 0
        raise SystemExit(f"Receiver stream failed: {error.code().name}: {error.details()}") from error
    except KeyboardInterrupt:
        return 130
    finally:
        channel.close()
        for writer in files.values():
            writer.close()
        for player in players.values():
            if player.stdin and not player.stdin.closed:
                player.stdin.close()
            try:
                player.wait(timeout=3)
            except subprocess.TimeoutExpired:
                player.terminate()


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("--endpoint", default="127.0.0.1:8083")
    value.add_argument("--connect-timeout", type=float, default=5.0)
    subparsers = value.add_subparsers(dest="command", required=True)

    inspect_parser = subparsers.add_parser("snapshot", help="print cameras, anchor, quality, and manifest")
    inspect_parser.add_argument("--session", required=True, type=session_bytes)
    inspect_parser.add_argument("--timeout", type=float, default=10.0)
    inspect_parser.set_defaults(function=snapshot)

    watch_parser = subparsers.add_parser("watch", help="stream synchronized metadata and optional H.264 AUs")
    watch_parser.add_argument("--session", required=True, type=session_bytes)
    watch_parser.add_argument("--camera", action="append", default=[], help="filter camera ID; repeatable")
    watch_parser.add_argument("--view-camera", action="append", default=[], help="camera ID to pipe to ffplay")
    watch_parser.add_argument("--dump-dir", help="append each camera's Annex-B AUs to CAMERA.h264")
    watch_parser.add_argument("--vectors", action="store_true", help="include complete observation/action/context vectors")
    watch_parser.add_argument("--max-steps", type=int, default=0, help="zero streams until interrupted")
    watch_parser.add_argument("--stream-timeout", type=float, default=None)
    watch_parser.set_defaults(function=watch)
    return value


def main() -> int:
    args = parser().parse_args()
    if getattr(args, "max_steps", 0) < 0:
        raise SystemExit("--max-steps must be zero or positive")
    return args.function(args)


if __name__ == "__main__":
    raise SystemExit(main())
