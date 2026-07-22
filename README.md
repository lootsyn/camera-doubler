# Generic Robot Multi-Camera Backend 2.1

벤더 독립 Edge/Receiver와 독립 Hardware Adapter로 구성된 멀티카메라 동기화·제어·LeRobot Dataset backend 구현이다. 모든 카메라 AU에 Edge timestamp를 넣고, 환경변수로 지정한 anchor AU에만 CRC-protected state/action context와 반복 manifest를 넣는다.

## 구현 구성

- Rust workspace: camera discovery/stable slots, v4l2loopback manager, GStreamer capture/UI/H.264/SRT, SEI codec, Adapter client, clock mapping, anchor resampling, mTLS control gateway, Receiver bootstrap/API/synchronization/replay
- Python: 공식 `rby1-sdk==0.10.0` Adapter, generic Adapter template와 gripper fixture, exact `lerobot[dataset]==0.6.0` transactional builder
- 배포: non-root/read-only/least-capability Docker images, 독립 Edge/Receiver Compose, 선택적 H.264 pass-through Web Relay
- 검증: Rust unit/integration tests, Python contract/export tests, healthcheck-integrated synthetic camera codec round-trip, deterministic raw replay, fault tests, SBOM/Trivy, package/vendor-boundary checks

기준 문서는 `ROBOT_MULTICAMERA_BACKEND_DESIGN.md`이며 구현 추적은 `docs/implementation/REQUIREMENTS_TRACEABILITY.md`, 현재 상태는 `docs/implementation/IMPLEMENTATION_STATUS.md`에 있다.

## 매뉴얼

- `docs/manuals/SETUP_AND_SECRETS.md`: Git에서 제외되는 env/secret/toolchain/runtime fixture의 생성·배치·재생성
- `docs/manuals/DEPLOYMENT_RUNBOOK.md`: Receiver → Adapter → Edge 시작 순서, port/firewall, health/readiness, 종료
- `docs/manuals/VIDEO_AND_METADATA_ACCESS.md`: 외부 gRPC와 HLS URL 동시 접근, VLC/hls.js, frame별 SSE/gRPC metadata correlation
- `docs/manuals/ADAPTER_AUTHORING_FOR_AI_AGENTS.md`: 다른 로봇 Adapter를 AI agent가 구현하기 위한 완전한 계약과 prompt template
- `docs/manuals/PRODUCTION_CHECKLIST.md`: 실제 camera/robot/capacity/security production 승인 항목

전체 문서 탐색은 `docs/manuals/README.md`에서 시작한다.

## 빠른 검증

Linux/WSL에서:

```bash
python3 scripts/validate-package.py
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
scripts/run-synthetic-roundtrip.sh
scripts/run-fault-tests.sh
docker compose -f compose.edge.yaml config -q
docker compose -f compose.receiver.yaml config -q
docker run --rm --entrypoint /usr/local/bin/robot-replay-verify robot-multicam-receiver:local --help
python scripts/receiver-metadata-client.py --help
METADATA_CLIENT_PYTHON="$PWD/.venv-tools/bin/python" scripts/run-web-relay-test.sh
```

개발 설정과 secret을 준비하려면:

```bash
./scripts/bootstrap-example-config.sh
./scripts/generate-dev-secrets.sh
sudo ./scripts/prepare-host.sh
./scripts/verify-environment.sh
```

## 실행

Receiver:

```bash
docker compose --env-file .env.receiver -f compose.receiver.yaml up -d --build
```

Receiver와 URL 영상 Web Relay:

```bash
docker compose --profile web -f compose.receiver.yaml up -d --build
curl http://127.0.0.1:8091/api/v1/streams
```

Edge와 RB-Y1 Adapter:

```bash
docker compose --env-file .env.edge --profile rby1 -f compose.edge.yaml up -d --build
```

물리 RB-Y1이 없을 때 `.env.adapter-rby1`의 `RBY1_USE_MOCK=1`로 공식 SDK contract를 사용하는 synthetic backend를 실행한다. 물리 카메라가 없는 검증 환경에서는 `robot-synthetic-roundtrip`이 `videotestsrc → H.264 → SEI → MPEG-TS → predecode extraction → decode`를 실제 플러그인으로 수행한다.

최종 감사 결과와 항목별 증거는 `docs/audit/FINAL_RELEASE_AUDIT.md` 및 `docs/audit/ACCEPTANCE_EVIDENCE.csv`에 있다. CycloneDX SBOM과 Trivy JSON은 `validation/security/`에 저장한다.

## 핵심 안전 규칙

1. Edge만 physical camera를 열며 UI와 network branch는 독립 leaky queue를 사용한다.
2. `CAMERA_STREAM_EXCLUDE`는 UI를 유지하고 외부 송출만 끈다. anchor와 exclude/disable 충돌은 시작 오류다.
3. SRT connection은 `base + stable slot`, authenticated canonical `rmc1` identity와 epoch를 사용한다.
4. Receiver는 decoder 전에 SEI를 추출하고 manifest의 anchor/camera catalog를 transport identity와 교차 검증한다.
5. command는 mTLS, exclusive TTL lease, UUID 중복 방지, schema/mode/shape/finite/range 검증을 모두 통과해야 Adapter로 전달된다.
6. Dataset export는 cadence 검사 후 temp/finalize/full loader scan/checksum/atomic rename 순서로 commit한다. frame reuse나 synthetic image 생성은 금지한다.
7. Web Relay는 gRPC를 한 번 구독하고 H.264를 재인코딩하지 않으며, HLS/SSE의 bounded preview drop이 Receiver ingest를 막지 않게 한다.

실제 로봇 motion과 USB camera hotplug만 hardware gate이며 SDK, Docker, GStreamer, synthetic camera와 LeRobot loader는 release 검증 대상이다.
