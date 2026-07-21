FROM python:3.12.10-slim-bookworm@sha256:fd95fa221297a88e1cf49c55ec1828edd7c5a428187e67b5d1805692d11588db

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PYTHONPATH=/app:/app/python

RUN apt-get update && apt-get upgrade -y \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --no-cache-dir grpcio==1.71.0 grpcio-tools==1.71.0 protobuf==5.29.6 \
    && groupadd --gid 10001 adapter \
    && useradd --uid 10001 --gid adapter --no-create-home --shell /usr/sbin/nologin adapter
WORKDIR /app
COPY proto/ ./proto/
COPY scripts/generate-proto.py ./scripts/generate-proto.py
COPY python/__init__.py ./python/__init__.py
COPY python/adapter_runtime/ ./python/adapter_runtime/
COPY adapters/__init__.py ./adapters/__init__.py
COPY adapters/template/ ./adapters/template/
COPY adapters/gripper_fixture/ ./adapters/gripper_fixture/
RUN python scripts/generate-proto.py \
    && python -m adapters.template.app self-test \
    && python -m adapters.gripper_fixture.app self-test \
    && find /app -type d -name __pycache__ -prune -exec rm -rf '{}' +
USER 10001:10001
ENTRYPOINT ["python", "-m", "adapters.template.app"]
CMD ["serve"]
