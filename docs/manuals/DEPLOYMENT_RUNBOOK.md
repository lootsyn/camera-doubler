# 배포 및 실행 Runbook

## 1. 구성과 네트워크

```text
Physical cameras + robot SDK
          │
          ▼
Edge host: Hardware Adapters ─UDS─ Edge Core ─SRT/UDP─► Receiver
          │                              │                │
          └─ /dev/video40+slot           │                ├─ gRPC metadata/H.264 AU :8083
             local LeRobot UI            └─ mTLS control ◄┤
                                                          └─ Dataset Builder :8090 (internal)
```

| 방향 | protocol/port | 목적 | 공개 정책 |
|---|---|---|---|
| Edge → Receiver | UDP `SRT_BASE_PORT..+MAX_CAMERAS-1` | 카메라별 encrypted MPEG-TS/SRT | Edge IP에서만 허용 |
| Metadata client → Receiver | TCP 8083 | ReceiverMetadata gRPC와 선택적 H.264 AU | VPN/SSH tunnel/사설망만 |
| Monitoring → Receiver | TCP 9090 | `/healthz`, `/readyz`, `/metrics` | monitoring subnet만 |
| Receiver → Edge | TCP 8082 | mTLS control gateway | Receiver IP에서만 허용 |
| Monitoring → Edge | TCP 9091 | Edge health/readiness/metrics | monitoring subnet만 |
| Receiver → Dataset Builder | TCP 8090, Compose network | export service | 외부 publish 금지 |

TCP 8080은 현재 호환성용 reserved port이며 browser video/REST endpoint가 아니다. 외부 영상은 `VIDEO_AND_METADATA_ACCESS.md`의 gRPC H.264 경로를 사용한다.

## 2. 최초 host 준비

Receiver와 Edge 양쪽에서 repository를 checkout하고 `SETUP_AND_SECRETS.md`에 따라 host별 env/config/secret을 materialize한다. Physical Edge에서만 실행한다.

```bash
sudo VIRTUAL_CAMERA_START=40 VIRTUAL_CAMERA_POOL_SIZE=16 ./scripts/prepare-host.sh
./scripts/verify-environment.sh
v4l2-ctl --list-devices
```

실제 selector를 정할 때 `/dev/videoN` 번호를 ID로 사용하지 않는다.

```bash
udevadm info --query=property --name=/dev/video0
v4l2-ctl --device=/dev/video0 --all
```

serial 또는 discovery가 만든 stable ID를 `ANCHOR_CAMERA_SELECTOR`와 camera policy에 넣는다.

## 3. 시작 순서

### 3.1 Receiver 먼저 시작

```bash
docker compose -f compose.receiver.yaml config -q
docker compose -f compose.receiver.yaml up -d --build --wait --wait-timeout 180
docker compose -f compose.receiver.yaml ps
curl -fsS http://127.0.0.1:9090/healthz
curl -fsS http://127.0.0.1:9090/readyz
```

카메라가 아직 연결되지 않아도 service process와 listener가 healthy여야 한다. `readyz`는 disk 또는 bootstrap 상태에 따라 fail할 수 있으므로 response body의 개별 check를 확인한다.

### 3.2 Adapter 시작

RB-Y1 없이 contract를 확인할 때 `.env.adapter-rby1`에 `RBY1_USE_MOCK=1`을 둔다. 실제 로봇은 안전 승인 후 `0`으로 바꾼다.

```bash
docker compose --profile rby1 -f compose.edge.yaml up -d --build state-init adapter-rby1
docker compose --profile rby1 -f compose.edge.yaml ps
docker compose --profile rby1 -f compose.edge.yaml logs --tail=100 adapter-rby1
```

별도 gripper fixture는 `--profile fixture`를 추가한다. 실제 component Adapter도 같은 UDS volume을 사용하되 socket 이름이 겹치면 안 된다.

### 3.3 Edge Core 시작

```bash
docker compose --profile rby1 -f compose.edge.yaml up -d --build --wait --wait-timeout 180
docker compose --profile rby1 -f compose.edge.yaml ps
curl -fsS http://127.0.0.1:9091/healthz
curl -fsS http://127.0.0.1:9091/readyz
```

Edge healthcheck는 실제 배포 이미지의 GStreamer plugin과 24-AU H.264/SEI/MPEG-TS/decode round-trip을 실행한다. 이 검사가 실패하면 카메라 송출을 시작하지 않는다.

## 4. Session과 카메라 확인

새 Edge process는 session UUID를 한 번 생성한다. raw stream ID나 secret을 출력하지 않고 UUID만 로그에서 찾는다.

```bash
docker compose -f compose.edge.yaml logs edge-core | grep 'Edge session created'
docker compose -f compose.receiver.yaml logs receiver | grep 'authenticated SRT stream accepted'
```

Receiver 로그에서 같은 session에 camera ID, epoch, listen port가 각각 연결되는지 확인한다. manifest가 검증된 뒤 metadata client로 authoritative anchor와 catalog를 조회한다.

## 5. Local virtual camera 확인

stable slot 0이 `/dev/video40`부터 할당된 구성이라면:

```bash
v4l2-ctl --device=/dev/video40 --all
ffplay -fflags nobuffer -f v4l2 -i /dev/video40
```

실제 번호는 `/var/lib/robot-edge/camera-map.json`과 Edge log/metrics를 기준으로 한다. virtual device 번호를 수동으로 다른 camera에 재사용하지 않는다.

## 6. 정상 운영 확인

```bash
docker compose -f compose.receiver.yaml logs --tail=200 receiver dataset-builder
docker compose --profile rby1 -f compose.edge.yaml logs --tail=200 edge-core adapter-rby1
curl -fsS http://127.0.0.1:9090/metrics | grep '^robot_'
curl -fsS http://127.0.0.1:9091/metrics | grep '^robot_'
```

확인해야 할 invariant:

- 모든 streamed camera는 `base + stable slot` port에 하나씩 연결된다.
- exactly one anchor만 manifest의 `anchor_camera_id`와 일치한다.
- secondary에는 timestamp 이외 semantic per-frame metadata가 없다.
- camera/Adapter/disk fault가 readiness 또는 drop counter에 드러나며 silent loss가 없다.
- Dataset Builder 장애가 Receiver listener와 raw ingest를 중단하지 않는다.

## 7. 종료와 재시작

진행 episode/export를 먼저 종료한 후 Edge, Adapter, Receiver 순서로 내린다.

```bash
docker compose --profile rby1 -f compose.edge.yaml stop edge-core adapter-rby1
docker compose -f compose.receiver.yaml stop dataset-builder receiver
```

`down --volumes`는 receiver dataset/raw volume과 Edge mapping을 삭제할 수 있으므로 일반 운영 종료에 사용하지 않는다. 볼륨 삭제는 정확한 backup과 명시적 데이터 폐기 승인 후에만 수행한다.

## 8. 장애 진단 순서

1. `/healthz`로 process/plugin 상태 확인.
2. `/readyz`로 anchor, manifest, disk, Adapter check 확인.
3. Compose log에서 session/camera/epoch/port와 rejection reason 확인.
4. UDP firewall/NAT와 양쪽 base port/key 일치 확인.
5. `scripts/run-synthetic-roundtrip.sh`로 physical camera와 독립된 codec gate 확인.
6. `scripts/run-srt-reconnect-test.sh`로 authenticated reconnect 확인.
7. Adapter `healthcheck`와 UDS socket permission 확인.

상세 failure semantics, retention, key rotation은 `docs/OPERATIONS.md`를 따른다.
