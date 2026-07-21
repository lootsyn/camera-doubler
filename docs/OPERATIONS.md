# 운영 가이드

## 시작 순서

1. 호스트에 v4l2loopback을 준비한다.
2. env/config와 secrets/certificates를 준비한다.
3. Receiver를 먼저 시작해 SRT listener를 연다.
4. Hardware Adapter를 시작한다.
5. Edge Core를 시작한다.
6. `/v1/cameras`, `/v1/anchor`, `/health/ready`를 확인한다.
7. LeRobot UI가 생성된 virtual camera를 열도록 설정한다.

## 명령

```bash
./scripts/bootstrap-example-config.sh
./scripts/generate-dev-secrets.sh
sudo ./scripts/prepare-host.sh
./scripts/verify-environment.sh
./scripts/validate-package.py

docker compose --env-file .env.receiver -f compose.receiver.yaml up -d --build
docker compose --env-file .env.edge --profile rby1 -f compose.edge.yaml up -d --build
```

## Dataset readiness

- anchor selector가 정확히 한 camera와 일치
- anchor stream transport identity 검증 완료
- SessionManifest reassembly/CRC 완료
- manifest anchor와 stream identity 일치
- required Adapter/camera 준비
- valid AnchorFrameContext 수신
- disk free-space threshold 충족

## 장애 시 기대 동작

- Receiver/SRT 장애: virtual camera 유지
- secondary camera 단절: required/optional policy 적용
- anchor 단절: 진행 episode 중단, readiness false
- Adapter 단절: 영상 유지, anchor context invalid
- manifest 미수신: preview-only
- dataset-builder 장애: ingest/preview/raw recording 유지, export만 degraded
- LeRobot version/cadence/export validation 실패: 기존 committed dataset 유지, 새 output quarantine
- disk low/full: preview 유지, recording/episode를 명시적으로 중단

## Capacity test

- USB controller별 bandwidth
- hardware encoder session 수
- CPU/GPU/memory 사용률
- virtual camera output latency
- SRT encryption(`pbkeylen`), queue/drop/reconnect
- context size/metadata kbps/serialization latency
- camera skew p95/max
- disk write throughput와 retention deletion

`MAX_ACTIVE_STREAMS`는 측정 결과보다 보수적으로 설정한다. Production 배포에서는 base image digest pin, SBOM, vulnerability scan을 release gate로 둔다.

## 추가 release gate

- anchor AU hold queue의 max time/entries/bytes와 orphan cleanup을 stress test한다.
- 실제 GStreamer image에서 같은 anchor AU에 timestamp, context, manifest의 복수 SEI를 삽입·추출하는 round trip을 실행한다. 1.22/1.23은 custom codec fallback이 필수다.
- dataset cadence report에서 anchor interval p50/p95/max, nominal FPS deviation, dropped/reused/synthetic frame 수를 확인한다. synthetic frame과 기본 frame reuse는 0이어야 한다.
- raw segment를 hash 검증한 뒤 `stream-envelope.json`과 `segments/index.jsonl`로 replay한다.
- stream ID에는 non-secret identifier만 넣고 key rotation은 session boundary에서 수행한다.
- stable camera identity collision과 slot tombstone/reclaim 동작을 unplug/replug 및 동일 모델 다중 연결로 검증한다.
- Dataset Builder가 exact LeRobot version을 확인하고 temp export, finalize, loader scan, checksum, atomic commit을 수행하는지 검증한다.
