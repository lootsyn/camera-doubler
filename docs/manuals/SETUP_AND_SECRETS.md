# 환경·Secret·Toolchain 준비

## 1. Git에서 제외되는 항목

다음 항목은 host 종속, 민감정보 또는 재생성 가능한 대용량 산출물이므로 push하지 않는다.

| 경로 | 성격 | 재생성 방법 |
|---|---|---|
| `.env.edge`, `.env.receiver`, `.env.dataset-builder`, `.env.web-relay`, `.env.adapter-*` | 배포별 주소·selector·용량 값 | `scripts/bootstrap-example-config.sh` 실행 후 수정 |
| `config/camera-policy.yaml`, `config/embodiment.yaml` | 실제 장치 mapping | 같은 bootstrap script 실행 후 장치에 맞게 수정 |
| `secrets/*` | SRT/HMAC/mTLS key와 certificate | 개발은 `scripts/generate-dev-secrets.sh`, production은 외부 PKI/secret manager |
| `python/generated/` | protobuf generated Python SDK | `python scripts/generate-proto.py` |
| `validation/runtime/` | synthetic TS, replay index, fixture HMAC key, 임시 binary | synthetic/replay 명령으로 재생성 |
| `target/`, `.cargo-home/`, `.rustup-home/` | Rust build/toolchain cache | rustup/cargo로 재설치·재빌드 |
| `.venv-rby1/`, `.venv-dataset/`, `.venv-tools/` | Python executable/dependency 환경 | 아래 venv 절차로 재생성 |

`*.example`, lockfile, Dockerfile, source, SBOM과 감사 JSON은 Git에 포함한다. 실제 secret 값을 example이나 문서에 복사하지 않는다.

## 2. 지원 환경

- Physical Edge: v4l2loopback module을 적재할 수 있는 Linux host가 필요하다. WSL은 Receiver와 synthetic 검증에는 사용할 수 있지만 physical virtual-camera production host로 승인하지 않는다.
- Receiver: Linux/WSL2, Docker Engine과 Compose plugin, UDP listener port range를 사용할 수 있는 host.
- 개발 도구: Rust 1.85.1, Python 3.12, protoc/grpcio-tools, GStreamer 1.22 이상. 여러 SEI 경로는 1.24 이상 권장.

설치 후 repository root에서 확인한다.

```bash
docker version
docker compose version
rustc --version
python3 --version
gst-launch-1.0 --version
./scripts/verify-environment.sh
```

## 3. Local env와 config 생성

```bash
./scripts/bootstrap-example-config.sh
```

이미 파일이 존재하면 덮어쓰지 않는다. 최소한 다음 값을 실제 배포에 맞춘다.

### Edge

- `EMBODIMENT_ID`: Receiver 및 embodiment YAML과 동일한 robot-cell ID.
- `EDGE_INSTANCE_ID`: 해당 Edge host의 안정적인 non-secret ID.
- `ANCHOR_CAMERA_SELECTOR`: 정확히 한 logical camera와 일치하는 `serial:` 또는 `id:` selector.
- `SRT_TARGET_HOST`: Receiver가 수신할 수 있는 사설 IP/DNS.
- `SRT_BASE_PORT`, `MAX_CAMERAS`: Receiver와 같아야 한다.
- `VIRTUAL_CAMERA_START`, `VIRTUAL_CAMERA_POOL_SIZE`: `prepare-host.sh`로 만든 pool과 같아야 한다.

### Receiver

- `EMBODIMENT_ID`: Edge와 동일.
- `EXPECTED_EDGE_INSTANCE_ID`: production에서는 Edge ID로 고정하는 것이 권장된다.
- `SRT_LISTEN_BASE_PORT`, `MAX_CAMERAS`, `SRT_PBKEYLEN`: Edge와 동일.
- `MIN_FREE_DISK_GB`: Docker volume 또는 bind mount의 실제 용량보다 작고 운영 여유보다 크게 설정한다.
- `RECEIVER_GRPC_BIND`: metadata client가 접근할 bind. 공개 인터넷에 직접 노출하지 않는다.

### Web Relay

- `RECEIVER_GRPC_ENDPOINT`: Compose 내부에서는 `http://receiver:8083`; plaintext scheme을 명시한다.
- `RELAY_HTTP_BIND`: HLS/SSE/catalog bind. 기본 `0.0.0.0:8091`은 사내망 직접 접근용이다.
- `RELAY_HLS_*`: target/playlist/max-file 상한. tmpfs 용량과 GOP 간격에 맞춘다.
- `RELAY_METADATA_HISTORY_PER_STREAM`: HLS wall-clock 지연보다 긴 frame 수를 두되 feature 크기×camera 수의 heap을 측정한다.
- `RELAY_GRPC_MAX_MESSAGE_MIB`: 모든 camera AU가 한 step에 모인 최대 크기보다 크게, 4–256 MiB 범위에서 설정한다.
- `RELAY_CORS_ALLOW_ORIGIN`: 기본 `*`; 제한된 web origin만 허용하려면 정확한 origin으로 바꾼다.

Relay에는 SRT/HMAC/mTLS secret을 전달하지 않는다.

### Adapter

- `ADAPTER_INSTANCE_ID`와 `ADAPTER_SOCKET`은 `config/embodiment.yaml`의 endpoint와 정확히 일치해야 한다.
- physical RB-Y1은 `RBY1_USE_MOCK=0`, `RB_Y1_ADDRESS`, model/command 제한을 실제 값으로 지정한다.
- robot 없이 SDK/contract만 검증할 때만 `RBY1_USE_MOCK=1`을 사용한다.

두 YAML의 핵심 계약은 다음과 같다.

- `camera-policy.yaml`: anchor, stream-only exclusion, full disable, dataset required/optional camera.
- `embodiment.yaml`: Adapter UDS endpoint, device ownership, observation/action/auxiliary vector 순서.

## 4. Secret 생성과 배치

개발 전용 secret은 다음 명령으로 idempotent하게 생성한다.

```bash
./scripts/generate-dev-secrets.sh
```

생성 파일과 사용 주체:

| 파일 | Edge | Receiver | 설명 |
|---|:---:|:---:|---|
| `srt_passphrase.txt` | O | O | SRT payload encryption. 양쪽 값이 같아야 함 |
| `srt_streamid_hmac_key.bin` | O | O | canonical `rmc1` stream ID 서명/검증 |
| `edge_control_ca.pem` | O | O | control mTLS trust anchor |
| `edge_control_server.crt/key` | O | - | Edge control server identity |
| `edge_control_client.crt/key` | - | O | Receiver control client identity |
| `edge_control_ca.key` | 배포 금지 | 배포 금지 | CA signing 전용. production에서는 offline 보관 |

Production에서는 같은 파일 이름으로 host secret store가 materialize하도록 구성하되 개발 CA를 재사용하지 않는다. 권장 권한은 directory `0700`, private key/HMAC/passphrase `0600`, public certificate `0644`다. Compose는 key를 image나 env에 넣지 않고 `/run/secrets/*`로 read-only mount한다.

값을 확인하기 위해 `cat`하거나 shell history에 넣지 말고 존재·권한·길이만 확인한다.

```bash
find secrets -maxdepth 1 -type f -printf '%M %f %s bytes\n'
git check-ignore secrets/srt_passphrase.txt secrets/srt_streamid_hmac_key.bin
```

회전은 session boundary에서 Receiver 검증 key를 먼저 staging하고 Edge 발급 key를 교체한다. 자세한 순서는 `docs/OPERATIONS.md`의 key rotation 절을 따른다.

## 5. Rust/Python generated toolchain 재생성

Rust는 `rust-toolchain.toml`과 `Cargo.lock`을 기준으로 한다.

```bash
rustup toolchain install 1.85.1
cargo fetch --locked
cargo build --locked --workspace
```

Python metadata/Adapter 도구:

```bash
python3 -m venv .venv-tools
. .venv-tools/bin/activate
python -m pip install --upgrade pip
python -m pip install -r python/metadata_client/requirements.txt
python scripts/generate-proto.py
python scripts/receiver-metadata-client.py --help
```

PowerShell에서는 activation 대신 venv Python을 직접 호출할 수 있다.

```powershell
py -3.12 -m venv .venv-tools
.\.venv-tools\Scripts\python.exe -m pip install -r python\metadata_client\requirements.txt
.\.venv-tools\Scripts\python.exe scripts\generate-proto.py
.\.venv-tools\Scripts\python.exe scripts\receiver-metadata-client.py --help
```

RB-Y1 local SDK venv가 필요한 경우:

```bash
python3 -m venv .venv-rby1
. .venv-rby1/bin/activate
python -m pip install 'rby1-sdk==0.10.0' grpcio==1.71.0 grpcio-tools==1.71.0 protobuf==5.29.6
python scripts/generate-proto.py
PYTHONPATH=.:./python python scripts/adapter-uds-smoke.py
```

일반 실행은 host venv 대신 pinned Docker image를 사용한다.

## 6. Runtime fixture 재생성

기본 codec gate:

```bash
./scripts/run-synthetic-roundtrip.sh
```

Archive/replay fixture를 명시적으로 보존하려면:

```bash
mkdir -p validation/runtime/archive-conformance
docker run --rm \
  -e HOME=/tmp \
  -e SYNTHETIC_ARCHIVE_ROOT=/archive \
  -v "$PWD/validation/runtime/archive-conformance:/archive" \
  --entrypoint /usr/local/bin/robot-synthetic-roundtrip \
  robot-multicam-edge-core:local

docker run --rm \
  -e HOME=/tmp \
  -v "$PWD/validation/runtime/archive-conformance:/archive:ro" \
  --entrypoint /usr/local/bin/robot-replay-verify \
  robot-multicam-receiver:local \
  /archive /archive/stream-envelope.json /archive/segments/index.jsonl /archive/hmac-key.bin
```

이 fixture의 `hmac-key.bin`도 secret 취급하며 Git에 추가하지 않는다. `git status --ignored`로 제외 여부를 확인한다.

## 7. Docker image 재생성

```bash
docker compose -f compose.receiver.yaml build --pull
docker compose --profile web -f compose.receiver.yaml build --pull
docker compose --profile rby1 --profile fixture -f compose.edge.yaml build --pull
```

base image digest와 Python lock을 바꿨다면 Rust/Python tests, synthetic round-trip, SBOM/Trivy 및 최종 감사 evidence를 다시 생성해야 한다.
