# RB-Y1 reference Adapter. Vendor SDK installation remains isolated here.
ARG RUST_IMAGE=rust:1.85-bookworm
FROM ${RUST_IMAGE} AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config clang cmake protobuf-compiler libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
# The implementation agent MUST install or vendor the supported RB-Y1 SDK in
# this stage only. Edge Core and Receiver images MUST NOT depend on it.
RUN cargo build --locked --release -p adapter-rby1

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/adapter-rby1 /usr/local/bin/adapter-rby1
USER root
ENTRYPOINT ["/usr/local/bin/adapter-rby1"]
CMD ["serve"]
