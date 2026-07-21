#!/usr/bin/env python3
from __future__ import annotations

import asyncio
import tempfile
import uuid
from pathlib import Path

import grpc
from google.protobuf.empty_pb2 import Empty

from adapters.rby1.mapping import SyntheticRby1Backend
from generated import adapter_api_pb2 as pb
from generated import adapter_api_pb2_grpc as pb_grpc
from python.adapter_runtime.service import HardwareAdapterService


async def main() -> None:
    with tempfile.TemporaryDirectory(prefix="robot-adapter-") as directory:
        socket = Path(directory) / "rby1.sock"
        server = grpc.aio.server(
            options=[
                ("grpc.max_receive_message_length", 1_048_576),
                ("grpc.max_send_message_length", 1_048_576),
            ],
            maximum_concurrent_rpcs=16,
        )
        backend = SyntheticRby1Backend()
        pb_grpc.add_HardwareAdapterServicer_to_server(HardwareAdapterService(backend), server)
        if server.add_insecure_port(f"unix:{socket}") != 1:
            raise RuntimeError("UDS bind failed")
        await server.start()
        channel = grpc.aio.insecure_channel(f"unix:{socket}")
        stub = pb_grpc.HardwareAdapterStub(channel)
        descriptor = await stub.GetDescriptor(Empty())
        samples = stub.StreamSamples(pb.StreamSamplesRequest(device_ids=["body"], requested_rate_hz=100))
        sample = await samples.read()

        async def commands():
            yield pb.CommandEnvelope(
                command_id=uuid.uuid4().bytes,
                lease_id=uuid.uuid4().bytes,
                device_id="body",
                command_mode="joint_position",
                action_schema_id=1,
                values=[0.01] * 20,
            )

        feedback = await stub.CommandStream(commands()).read()
        health = await stub.GetHealth(Empty())
        samples.cancel()
        await channel.close()
        await server.stop(grace=0)
        assert descriptor.vendor_sdk_version == "0.10.0"
        assert sample.descriptor_revision == descriptor.descriptor_revision
        assert sample.source_time_ns > 0 and len(sample.feature_blocks) == 5
        assert feedback.status == pb.COMMAND_STATUS_ACCEPTED
        assert health.live and health.ready
        print("Adapter UDS gRPC smoke PASS: descriptor/sample/command/health")


if __name__ == "__main__":
    asyncio.run(main())
