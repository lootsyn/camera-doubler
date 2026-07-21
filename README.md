# Generic Robot Multi-Camera Backend Specification 2.1

특정 로봇 벤더에 종속되지 않는 멀티카메라 가상카메라·외부 송출·시간 동기화·LeRobot 데이터셋 백엔드의 구현 설계서와 Docker 스캐폴딩이다.

## 핵심 문서

- `ROBOT_MULTICAMERA_BACKEND_DESIGN.md`: 전체 구현 명세
- `docs/TRANSPORT_BOOTSTRAP.md`: 카메라별 SRT 수신, anchor 판정, SEI 추출 규칙
- `docs/PROTOCOL_CONSTANTS.md`: 고정 SEI UUID, CRC, canonical stream ID
- `docs/OPERATIONS.md`: 배포와 운영
- `REVIEW_REPORT.md`: 네 차례 독립 검토 결과와 남은 runtime gate
- `validation/four_pass_results.json`: machine-readable PASS evidence
- `validation/STATIC_CHECKS.txt`: 간단한 검증 요약

## 주요 프로토콜

- `proto/adapter_api.proto`: 로봇/그리퍼/부품 Adapter 계약
- `proto/backend_api.proto`: Generic Edge Control Gateway
- `proto/frame_metadata.proto`: timestamp, anchor context packet, manifest chunk
- `proto/receiver_api.proto`: camera/anchor/manifest/quality/synchronized-step 조회

## 설정 준비

```bash
./scripts/bootstrap-example-config.sh  # .env.edge/.env.receiver/.env.dataset-builder 생성
./scripts/generate-dev-secrets.sh
sudo ./scripts/prepare-host.sh
./scripts/validate-package.py
```

`ANCHOR_CAMERA_SELECTOR`는 정확히 한 카메라와 일치해야 한다. 송출만 제외하려면 `CAMERA_STREAM_EXCLUDE`, 가상카메라까지 비활성화하려면 `CAMERA_DISABLE`을 사용한다.

## Docker

Dockerfile과 Compose는 AI 구현 에이전트가 생성할 Rust/Python source tree를 기준으로 한 빌드 스캐폴딩이다. 현재 패키지는 설계 산출물이므로 실제 source가 생성되기 전에는 image build가 완료되지 않는다.

Receiver:

```bash
docker compose --env-file .env.receiver -f compose.receiver.yaml up -d --build
```

Edge + RB-Y1 reference Adapter:

```bash
docker compose --env-file .env.edge --profile rby1 -f compose.edge.yaml up -d --build
```

Receiver는 dataset-builder 장애와 독립적으로 ingest/preview를 계속할 수 있도록 Compose dependency를 분리했다. Dataset Builder는 `.env.dataset-builder`에서 exact LeRobot version, cadence, atomic export 정책을 검증한다.

## Transport 요약

1. 각 camera는 `base + stable slot`의 독립 SRT connection을 사용한다.
2. 수신 port와 HMAC-protected SRT stream ID로 camera/session/slot/epoch를 provisional 식별한다.
3. `SessionManifestV1.anchor_camera_id`를 authoritative anchor로 확정한다.
4. 모든 camera AU에는 timestamp SEI가 있고, anchor AU에만 state/action context와 manifest chunk가 있다.
5. Receiver는 decoder 전에 SEI를 추출하고 canonical decoded context와 exact CRC packet을 보존한 뒤 anchor timestamp로 synchronized step을 생성한다.
6. raw TS를 저장할 때 transport identity는 connection-level `stream-envelope.json`으로 보존한다.
7. 최종 LeRobot export는 pinned package version으로 temp/finalize/load-scan/atomic-commit을 수행하고 irregular cadence를 30 Hz로 조용히 표기하지 않는다.
