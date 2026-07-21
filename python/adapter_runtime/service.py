"""Bounded generated-API Adapter service shared by vendor containers."""

from __future__ import annotations

import asyncio
import hashlib
import math
import os
import time
from dataclasses import dataclass
from pathlib import Path
from typing import AsyncIterator, Protocol

import grpc

from generated import adapter_api_pb2 as pb
from generated import adapter_api_pb2_grpc as pb_grpc


def stable_feature_id(name: str) -> int:
    value = int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "big")
    if value == 0:
        raise ValueError("zero feature id is reserved")
    return value


def descriptor_revision(descriptor: pb.AdapterDescriptor) -> int:
    clone = pb.AdapterDescriptor()
    clone.CopyFrom(descriptor)
    clone.descriptor_revision = 0
    value = int.from_bytes(hashlib.sha256(clone.SerializeToString(deterministic=True)).digest()[:8], "big")
    return value or 1


@dataclass(frozen=True)
class Sample:
    source_time_ns: int
    values: dict[str, list[float]]


class Backend(Protocol):
    descriptor: pb.AdapterDescriptor

    def sample(self) -> Sample: ...

    def command(self, mode: str, values: list[float]) -> list[float]: ...

    def health(self) -> tuple[bool, bool, str, list[str]]: ...


class HardwareAdapterService(pb_grpc.HardwareAdapterServicer):
    def __init__(self, backend: Backend) -> None:
        self.backend = backend
        self._sample_seq = 0
        self._closed = False

    async def GetDescriptor(self, request, context):  # noqa: N802
        del request, context
        return self.backend.descriptor

    async def StreamSamples(self, request, context):  # noqa: N802
        rate = request.requested_rate_hz or 100
        if not 1 <= rate <= 1000:
            await context.abort(grpc.StatusCode.INVALID_ARGUMENT, "requested_rate_hz outside 1..1000")
        period = 1.0 / rate
        features = {
            feature.qualified_name: feature.feature_id
            for device in self.backend.descriptor.devices
            for feature in device.features
        }
        while not self._closed and not context.cancelled():
            started = time.monotonic()
            try:
                sample = await asyncio.to_thread(self.backend.sample)
                blocks = []
                for name, feature_id in features.items():
                    values = sample.values.get(name)
                    blocks.append(pb.FeatureBlock(
                        feature_id=feature_id,
                        values=values or [],
                        valid=values is not None and all(math.isfinite(value) for value in values),
                        invalid_reason="" if values is not None else "feature unavailable",
                        source_time_ns=sample.source_time_ns,
                    ))
                self._sample_seq += 1
                yield pb.DeviceSample(
                    adapter_instance_id=self.backend.descriptor.adapter_instance_id,
                    device_id=self.backend.descriptor.devices[0].device_id,
                    sample_seq=self._sample_seq,
                    source_clock_id=self.backend.descriptor.source_clock.source_clock_id,
                    source_time_ns=sample.source_time_ns,
                    feature_blocks=blocks,
                    descriptor_revision=self.backend.descriptor.descriptor_revision,
                )
            except Exception as exc:  # hardware boundary is reported, not fatal to server
                await context.abort(grpc.StatusCode.UNAVAILABLE, str(exc)[:512])
            await asyncio.sleep(max(0.0, period - (time.monotonic() - started)))

    async def CommandStream(self, request_iterator, context):  # noqa: N802
        seen: set[bytes] = set()
        async for command in request_iterator:
            if len(command.command_id) != 16 or len(command.lease_id) != 16:
                await context.abort(grpc.StatusCode.INVALID_ARGUMENT, "UUIDs must be 16 bytes")
            if command.command_id in seen:
                await context.abort(grpc.StatusCode.ALREADY_EXISTS, "duplicate command ID")
            if len(seen) >= 4096:
                seen.clear()
            seen.add(command.command_id)
            if not command.values or any(not math.isfinite(value) for value in command.values):
                await context.abort(grpc.StatusCode.INVALID_ARGUMENT, "invalid command values")
            try:
                effective = await asyncio.to_thread(
                    self.backend.command, command.command_mode, list(command.values)
                )
                yield pb.CommandFeedback(
                    command_id=command.command_id,
                    device_id=command.device_id,
                    status=pb.COMMAND_STATUS_ACCEPTED,
                    source_time_ns=time.monotonic_ns(),
                    source_clock_id=self.backend.descriptor.source_clock.source_clock_id,
                    effective_values=effective,
                )
            except Exception as exc:
                yield pb.CommandFeedback(
                    command_id=command.command_id,
                    device_id=command.device_id,
                    status=pb.COMMAND_STATUS_REJECTED,
                    source_time_ns=time.monotonic_ns(),
                    source_clock_id=self.backend.descriptor.source_clock.source_clock_id,
                    reason=str(exc)[:512],
                )

    async def ProbeClock(self, request, context):  # noqa: N802
        del context
        return pb.ClockProbeResponse(
            nonce=request.nonce,
            source_clock_id=self.backend.descriptor.source_clock.source_clock_id,
            source_time_ns=time.monotonic_ns(),
        )

    async def GetHealth(self, request, context):  # noqa: N802
        del request, context
        live, ready, status, warnings = self.backend.health()
        return pb.HealthResponse(live=live, ready=ready, status=status, warnings=warnings)


async def serve(backend: Backend, socket_path: str) -> None:
    if not socket_path.startswith("/") or ".." in Path(socket_path).parts:
        raise ValueError("adapter socket must be an absolute normalized path")
    path = Path(socket_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.unlink(missing_ok=True)
    server = grpc.aio.server(
        options=[
            ("grpc.max_receive_message_length", 1_048_576),
            ("grpc.max_send_message_length", 1_048_576),
        ],
        maximum_concurrent_rpcs=64,
    )
    pb_grpc.add_HardwareAdapterServicer_to_server(HardwareAdapterService(backend), server)
    if server.add_insecure_port(f"unix:{socket_path}") != 1:
        raise RuntimeError("failed to bind adapter unix socket")
    await server.start()
    os.chmod(path, 0o660)
    await server.wait_for_termination()
