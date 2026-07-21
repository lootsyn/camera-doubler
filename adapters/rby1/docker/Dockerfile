FROM ubuntu:24.04@sha256:4fbb8e6a8395de5a7550b33509421a2bafbc0aab6c06ba2cef9ebffbc7092d90

ARG RBY1_SDK_VERSION=0.10.0
ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONPATH=/app:/app/python \
    PATH=/opt/venv/bin:$PATH

RUN apt-get update && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends \
      ca-certificates python3 python3-venv \
    && python3 -m venv /opt/venv \
    && /opt/venv/bin/pip install --no-cache-dir \
      "rby1-sdk==${RBY1_SDK_VERSION}" \
      grpcio==1.71.0 grpcio-tools==1.71.0 protobuf==5.29.6 \
    && groupadd --gid 10001 adapter \
    && useradd --uid 10001 --gid adapter --no-create-home --shell /usr/sbin/nologin adapter \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY proto/ ./proto/
COPY scripts/generate-proto.py ./scripts/generate-proto.py
COPY python/__init__.py ./python/__init__.py
COPY python/adapter_runtime/ ./python/adapter_runtime/
COPY adapters/__init__.py ./adapters/__init__.py
COPY adapters/rby1/ ./adapters/rby1/
RUN python scripts/generate-proto.py \
    && RBY1_USE_MOCK=1 python -m adapters.rby1.app self-test \
    && find /app -type d -name __pycache__ -prune -exec rm -rf '{}' +

USER 10001:10001
ENTRYPOINT ["python", "-m", "adapters.rby1.app"]
CMD ["serve"]
