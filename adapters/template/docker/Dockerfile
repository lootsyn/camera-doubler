# Copy this Adapter template for a robot, gripper, base, tool, or sensor.
ARG RUST_IMAGE=rust:1.85-bookworm
FROM ${RUST_IMAGE} AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config clang cmake protobuf-compiler libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
ARG ADAPTER_PACKAGE=adapter-template
RUN cargo build --locked --release -p "${ADAPTER_PACKAGE}"

FROM debian:bookworm-slim
ARG ADAPTER_BINARY=adapter-template
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/${ADAPTER_BINARY} /usr/local/bin/adapter
ENTRYPOINT ["/usr/local/bin/adapter"]
CMD ["serve"]
