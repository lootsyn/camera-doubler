from __future__ import annotations

import asyncio
import json
import math
import os
import sys
import time

from generated import adapter_api_pb2 as pb
from python.adapter_runtime.service import Sample, descriptor_revision, serve, stable_feature_id


class TemplateBackend:
    def __init__(self, device_id: str = "component", vector_length: int = 1) -> None:
        if not device_id or vector_length <= 0 or vector_length > 256:
            raise ValueError("invalid component contract")
        self.device_id = device_id
        self.vector_length = vector_length
        self.target = [0.0] * vector_length
        self.position = [0.0] * vector_length
        prefix = f"{device_id}."
        self.descriptor = pb.AdapterDescriptor(
            api_version=1,
            adapter_instance_id=os.getenv("ADAPTER_INSTANCE_ID", f"{device_id}-adapter"),
            adapter_name="Generic Adapter template",
            adapter_version="0.1.0",
            vendor_sdk_version="none",
            source_clock=pb.SourceClockDescriptor(
                source_clock_id=f"{device_id}-monotonic",
                kind=pb.SOURCE_CLOCK_KIND_EDGE_MONOTONIC,
                supports_probe=True,
            ),
            devices=[pb.DeviceDescriptor(
                device_id=device_id,
                kind=pb.DEVICE_KIND_CUSTOM,
                role=device_id,
                vendor="template",
                model="synthetic-v1",
                required=True,
                command_modes=["position"],
                features=[
                    pb.FeatureDescriptor(
                        feature_id=stable_feature_id(prefix + "position"),
                        qualified_name=prefix + "position",
                        semantic="actual_position",
                        role=pb.FEATURE_ROLE_STATE,
                        unit="normalized",
                        shape=[vector_length],
                        interpolation=pb.INTERPOLATION_METHOD_LINEAR,
                        required=True,
                    ),
                    pb.FeatureDescriptor(
                        feature_id=stable_feature_id(prefix + "effective_target_position"),
                        qualified_name=prefix + "effective_target_position",
                        semantic="controller_target_position",
                        role=pb.FEATURE_ROLE_EFFECTIVE_ACTION,
                        unit="normalized",
                        shape=[vector_length],
                        interpolation=pb.INTERPOLATION_METHOD_ZERO_ORDER_HOLD,
                        required=True,
                    ),
                ],
            )],
        )
        self.descriptor.descriptor_revision = descriptor_revision(self.descriptor)

    def sample(self) -> Sample:
        self.position = [current + (target - current) * 0.25 for current, target in zip(self.position, self.target)]
        return Sample(time.monotonic_ns(), {
            f"{self.device_id}.position": self.position.copy(),
            f"{self.device_id}.effective_target_position": self.target.copy(),
        })

    def command(self, mode: str, values: list[float]) -> list[float]:
        if mode != "position" or len(values) != self.vector_length:
            raise ValueError("unsupported component command")
        if any(not math.isfinite(value) or not 0.0 <= value <= 1.0 for value in values):
            raise ValueError("component command outside [0, 1]")
        self.target = values.copy()
        return values

    def health(self) -> tuple[bool, bool, str, list[str]]:
        return True, True, "template synthetic backend ready", []


def main() -> None:
    command = sys.argv[1] if len(sys.argv) > 1 else "serve"
    backend = TemplateBackend(
        os.getenv("COMPONENT_DEVICE_ID", "component"),
        int(os.getenv("COMPONENT_VECTOR_LENGTH", "1")),
    )
    if command in {"healthcheck", "self-test"}:
        sample = backend.sample()
        assert sample.source_time_ns > 0 and len(sample.values) == 2
        print(json.dumps({"ready": True, "descriptor_revision": backend.descriptor.descriptor_revision}))
        return
    if command != "serve":
        raise SystemExit(f"unknown command: {command}")
    asyncio.run(serve(backend, os.getenv("ADAPTER_SOCKET", "/run/robot-adapters/component.sock")))


if __name__ == "__main__":
    main()
