# 운영 및 troubleshooting

설치/env/secret은 `docs/manuals/SETUP_AND_SECRETS.md`, 단계별 배포는 `docs/manuals/DEPLOYMENT_RUNBOOK.md`, 외부 화면과 metadata 조회는 `docs/manuals/VIDEO_AND_METADATA_ACCESS.md`, 새 로봇 Adapter는 `docs/manuals/ADAPTER_AUTHORING_FOR_AI_AGENTS.md`를 먼저 따른다.

## 시작 순서

1. `bootstrap-example-config.sh`로 local env를 만들고 production 값으로 수정한다.
2. repository 밖의 secret manager 또는 `generate-dev-secrets.sh`로 SRT/HMAC/mTLS secret을 준비한다.
3. Linux host에서 `prepare-host.sh`로 정확한 v4l2loopback pool을 확인한다.
4. Receiver, Hardware Adapter, Edge 순서로 시작한다.
5. `/healthz`, `/readyz`, `/metrics`, Receiver metadata gRPC와 Edge mTLS Control gRPC를 확인한다.

```bash
docker compose --env-file .env.receiver -f compose.receiver.yaml up -d --build
docker compose --env-file .env.edge --profile rby1 -f compose.edge.yaml up -d --build
docker compose -f compose.receiver.yaml ps
docker compose -f compose.edge.yaml ps
```

`ANCHOR_CAMERA_SELECTOR`는 정확히 한 logical camera와 일치해야 한다. `CAMERA_STREAM_EXCLUDE`는 UI만 유지하고 송출을 제외하며 `CAMERA_DISABLE`은 virtual output까지 제거한다.

## 정상 readiness

- protocol constants가 compiled 값과 일치
- required Adapter descriptor와 source clock mapper 준비
- 정확히 하나의 anchor camera와 virtual output 준비
- 모든 required SRT transport identity/port/epoch 검증
- bounded manifest reassembly와 authoritative camera catalog 검증
- valid anchor context와 required secondary frame skew 충족
- data root free-space/spool cap 충족
- pinned LeRobot version과 export filesystem 조건 충족

## 장애 동작

- Receiver/SRT 손실: UI virtual camera branch 유지, bounded queue에서 오래된 network AU drop
- secondary 손실: 해당 step을 drop하고 frame을 재사용하지 않음
- anchor 손실: readiness false, 진행 episode invalid/finalized 처리
- Adapter 손실/clock jump: context invalid 또는 step drop; 카메라 UI는 유지
- manifest timeout/CRC/ratio failure: preview-only 또는 quarantine, dataset 미생성
- Dataset Builder 손실: Receiver ingest/preview/raw integrity 기능 유지
- disk low/full: readiness에 `low/full` 반영, 새 episode/export 중지, silent loss 금지
- hotplug/collision: stable mapping 유지; canonical identity collision은 fail closed

## key rotation

HMAC/SRT/mTLS key는 session boundary에서만 교체한다.

1. 새 secret을 Receiver와 Edge host에 staging한다.
2. 진행 episode를 종료하고 raw/index fsync를 확인한다.
3. Receiver를 새 verification secret으로 재시작한다.
4. Edge를 새 issuance secret으로 재시작해 새 session ID, manifest revision, stream epoch를 발급한다.
5. 이전 session replay hash와 새 connection HMAC/mTLS를 확인한 뒤 이전 key를 폐기한다.

key를 image, env example, log 또는 stream ID에 넣지 않는다. stream ID에는 non-secret identifiers와 signature만 포함한다.

## retention과 복구

- `MIN_FREE_DISK_GB`, spool cap, segment hash algorithm을 production volume 용량에 맞춘다.
- retention은 가장 오래된 unprotected finalized segment부터 exact session root 안에서만 제거한다.
- active segment, 진행 episode, commit 전 export는 protected set으로 보존한다.
- `segments/index.jsonl` byte count/SHA-256 mismatch가 있으면 replay/export를 중단하고 quarantine한다.
- failed Dataset export는 `.exports/failed`에 보존하고 기존 committed dataset을 덮어쓰지 않는다.

## 진단 명령

```bash
docker compose -f compose.edge.yaml logs --tail=200 edge-core adapter-rby1
docker compose -f compose.receiver.yaml logs --tail=200 receiver dataset-builder
gst-inspect-1.0 v4l2src v4l2sink x264enc h264parse mpegtsmux srtsink srtsrc
scripts/run-synthetic-roundtrip.sh
scripts/run-fault-tests.sh
python3 scripts/verify-vendor-boundary.py
python3 scripts/receiver-metadata-client.py --help
```

SEI round-trip가 실패하면 GStreamer warning의 NAL ordering, H.264 `stream-format=byte-stream,alignment=au`, timestamp exactly-one, mux/demux dynamic pad를 순서대로 확인한다. `v4l2loopback`이 없는 WSL에서는 synthetic round-trip을 codec gate로 사용하되 USB identity/hotplug를 PASS로 가장하지 않는다.

## capacity와 release

USB controller bandwidth, encoder session, CPU/memory, virtual-output latency, encrypted SRT reconnect, metadata kbps, camera skew p95/max, disk throughput을 측정해 `MAX_CAMERAS`와 bitrate를 정한다. release 전 format/clippy/test, Python export loader scan, Docker builds/self-tests, Compose config, synthetic codec, package/vendor scan과 `docs/audit/FINAL_RELEASE_AUDIT.md`를 모두 갱신한다.
