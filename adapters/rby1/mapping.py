"""The only module allowed to import and translate the official RB-Y1 SDK."""

from __future__ import annotations

import importlib.metadata
import math
import os
import time
from dataclasses import dataclass
from typing import Any

import numpy as np
import rby1_sdk as rby

from generated import adapter_api_pb2 as pb
from python.adapter_runtime.service import Sample, descriptor_revision, stable_feature_id

SUPPORTED_SDK_VERSION = "0.10.0"


def _feature(name: str, semantic: str, role: int, unit: str, length: int, interpolation: int, required: bool) -> pb.FeatureDescriptor:
    return pb.FeatureDescriptor(
        feature_id=stable_feature_id(name),
        qualified_name=name,
        semantic=semantic,
        role=role,
        unit=unit,
        shape=[length],
        interpolation=interpolation,
        required=required,
    )


def make_descriptor(dof: int, mock: bool = False) -> pb.AdapterDescriptor:
    sdk_version = importlib.metadata.version("rby1-sdk")
    if sdk_version != SUPPORTED_SDK_VERSION:
        raise RuntimeError(f"rby1-sdk mismatch: expected {SUPPORTED_SDK_VERSION}, got {sdk_version}")
    descriptor = pb.AdapterDescriptor(
        api_version=1,
        adapter_instance_id=os.getenv("ADAPTER_INSTANCE_ID", "rby1-main"),
        adapter_name="RB-Y1 reference adapter" + (" (synthetic)" if mock else ""),
        adapter_version="0.1.0",
        vendor_sdk_version=sdk_version,
        source_clock=pb.SourceClockDescriptor(
            source_clock_id="rby1-state-clock",
            kind=(pb.SOURCE_CLOCK_KIND_SOURCE_MONOTONIC if mock else pb.SOURCE_CLOCK_KIND_TAI_UTC),
            description="RB-Y1 RobotState timestamp; probed against Edge monotonic",
            supports_probe=True,
        ),
        devices=[pb.DeviceDescriptor(
            device_id="body",
            kind=pb.DEVICE_KIND_ROBOT,
            role="main_robot",
            vendor="Rainbow Robotics",
            model=f"RB-Y1-{os.getenv('RBY1_MODEL', 'a').upper()}",
            required=True,
            command_modes=["joint_position"],
            features=[
                _feature("body.joint.position", "actual_joint_position", pb.FEATURE_ROLE_STATE, "rad", dof, pb.INTERPOLATION_METHOD_LINEAR, True),
                _feature("body.joint.velocity", "actual_joint_velocity", pb.FEATURE_ROLE_STATE, "rad/s", dof, pb.INTERPOLATION_METHOD_LINEAR, False),
                _feature("body.joint.effective_target_position", "controller_target_position", pb.FEATURE_ROLE_EFFECTIVE_ACTION, "rad", dof, pb.INTERPOLATION_METHOD_ZERO_ORDER_HOLD, True),
                _feature("body.joint.effective_target_velocity", "controller_target_velocity", pb.FEATURE_ROLE_EFFECTIVE_ACTION, "rad/s", dof, pb.INTERPOLATION_METHOD_ZERO_ORDER_HOLD, False),
                _feature("body.control_status", "ready_joint_fraction", pb.FEATURE_ROLE_AUXILIARY, "ratio", 1, pb.INTERPOLATION_METHOD_NEAREST, False),
            ],
        )],
    )
    descriptor.descriptor_revision = descriptor_revision(descriptor)
    return descriptor


@dataclass
class SyntheticRby1Backend:
    dof: int = 20

    def __post_init__(self) -> None:
        self.descriptor = make_descriptor(self.dof, mock=True)
        self._start = time.monotonic_ns()
        self._target = np.zeros(self.dof, dtype=np.float64)

    def sample(self) -> Sample:
        source_time = time.monotonic_ns()
        phase = (source_time - self._start) / 1_000_000_000
        position = np.sin(np.arange(self.dof) * 0.1 + phase) * 0.05
        velocity = np.cos(np.arange(self.dof) * 0.1 + phase) * 0.05
        return Sample(source_time, {
            "body.joint.position": position.tolist(),
            "body.joint.velocity": velocity.tolist(),
            "body.joint.effective_target_position": self._target.tolist(),
            "body.joint.effective_target_velocity": [0.0] * self.dof,
            "body.control_status": [1.0],
        })

    def command(self, mode: str, values: list[float]) -> list[float]:
        if mode != "joint_position" or len(values) != self.dof or any(not math.isfinite(v) or abs(v) > 3.2 for v in values):
            raise ValueError("unsafe RB-Y1 joint_position command")
        self._target = np.asarray(values, dtype=np.float64)
        return values

    def health(self) -> tuple[bool, bool, str, list[str]]:
        return True, True, "synthetic RB-Y1 SDK contract active", ["not connected to physical robot"]


class RealRby1Backend:
    def __init__(self) -> None:
        address = os.getenv("RBY1_ADDRESS") or os.environ["RB_Y1_ADDRESS"]
        model_name = os.getenv("RBY1_MODEL", os.getenv("RB_Y1_MODEL", "a"))
        self.robot = rby.create_robot(address, model_name)
        if not self.robot.connect(max_retries=5, timeout_ms=1000):
            raise RuntimeError("RB-Y1 connection failed")
        self.indices = list(self.robot.model().body_idx)
        self.descriptor = make_descriptor(len(self.indices))

    def sample(self) -> Sample:
        state = self.robot.get_state()
        source_time = int(state.timestamp.timestamp() * 1_000_000_000)
        ready = np.asarray(state.is_ready)[self.indices]
        return Sample(source_time, {
            "body.joint.position": np.asarray(state.position)[self.indices].tolist(),
            "body.joint.velocity": np.asarray(state.velocity)[self.indices].tolist(),
            "body.joint.effective_target_position": np.asarray(state.target_position)[self.indices].tolist(),
            "body.joint.effective_target_velocity": np.asarray(state.target_velocity)[self.indices].tolist(),
            "body.control_status": [float(np.count_nonzero(ready)) / len(self.indices)],
        })

    def command(self, mode: str, values: list[float]) -> list[float]:
        if mode != "joint_position" or len(values) != len(self.indices):
            raise ValueError("unsupported command mode or shape")
        vector = np.asarray(values, dtype=np.float64)
        if not np.all(np.isfinite(vector)):
            raise ValueError("non-finite command")
        joint = (rby.JointPositionCommandBuilder()
                 .set_position(vector)
                 .set_minimum_time(float(os.getenv("RBY1_COMMAND_MINIMUM_TIME_SEC", "0.1"))))
        command = rby.RobotCommandBuilder().set_command(
            rby.ComponentBasedCommandBuilder().set_body_command(
                rby.BodyCommandBuilder().set_command(joint)))
        self.robot.send_command(command, int(os.getenv("RBY1_COMMAND_PRIORITY", "10"))).get()
        return values

    def health(self) -> tuple[bool, bool, str, list[str]]:
        connected = bool(self.robot.is_connected())
        return True, connected, "connected" if connected else "disconnected", []


def semantic_self_test(backend: Any, samples: int = 6) -> dict[str, Any]:
    if samples < 3:
        raise ValueError("at least three samples required")
    captured = [backend.sample() for _ in range(samples)]
    times = [sample.source_time_ns for sample in captured]
    names = {feature.qualified_name for feature in backend.descriptor.devices[0].features}
    expected = {
        "body.joint.position", "body.joint.velocity",
        "body.joint.effective_target_position", "body.joint.effective_target_velocity",
    }
    positions = [sample.values["body.joint.position"] for sample in captured]
    dof = backend.descriptor.devices[0].features[0].shape[0]
    result = {
        "sdk_version": backend.descriptor.vendor_sdk_version,
        "timestamp_monotonic": all(b > a for a, b in zip(times, times[1:])),
        "joint_order_and_shape": all(len(value) == dof for value in positions),
        "effective_target_present": expected.issubset(names),
        "sample_count": len(captured),
        "command_feedback_capable": "joint_position" in backend.descriptor.devices[0].command_modes,
    }
    if not all(value for key, value in result.items() if key not in {"sdk_version", "sample_count"}):
        raise RuntimeError(f"RB-Y1 semantic self-test failed: {result}")
    return result
