# 외부 영상 및 프레임 메타데이터 접근

이 문서의 기본 배포는 Receiver와 client가 같은 신뢰 사내망에 있고, TCP 8083과 8091에 직접 접근하는 구성이다. 두 port 모두 기본값은 TLS와 사용자 인증이 없는 평문이다. 보호 연결은 선택 사항으로 마지막에 설명한다.

## 1. 결론과 endpoint

외부 gRPC 접근과 URL 영상 접근은 동시에 가능하다. Web Relay는 Receiver 뒤의 선택적 consumer이므로 Relay를 켜도 기존 gRPC API, SRT ingest, raw recording, Dataset Builder는 그대로 유지된다.

```text
Edge --SRT/H.264--> Receiver :8083 --한 개의 gRPC 구독--> Web Relay :8091
                              |                         |-- HLS URL
                              |                         `-- metadata SSE
                              `-- 기존 외부 gRPC client
```

| 목적 | endpoint | 내용 |
|---|---|---|
| session 발견 | `ReceiverMetadata.ListSessions` on TCP 8083 | authoritative/connected session 목록 |
| 원본 application API | `ReceiverMetadata` on TCP 8083 | 동기화 step, H.264 AU, manifest, quality |
| stream catalog | `GET http://HOST:8091/api/v1/streams` | camera ID와 실제 HLS/SSE URL |
| URL 영상 | `GET /live/{session_id}/{camera_key}/index.m3u8` | H.264를 재인코딩하지 않은 MPEG-TS HLS |
| 웹 메타데이터 | `GET /metadata/{session_id}/{camera_key}` | frame event SSE, bounded history 뒤 live 전환 |
| Relay 상태 | `/healthz`, `/readyz`, `/metrics` | process, active session/HLS, counter |

`camera_key`는 임의 camera ID를 안전한 URL path로 만들기 위한 UTF-8 bytes의 hex 값이다. 직접 계산할 필요 없이 catalog가 반환하는 URL을 사용한다.

## 2. Relay 시작

최초 한 번 local env와 개발 secret을 만든다. production에서는 개발 인증서를 교체한다.

```bash
./scripts/bootstrap-example-config.sh
./scripts/generate-dev-secrets.sh
```

`.env.web-relay`의 기본값은 Receiver Compose network의 `http://receiver:8083`, host TCP 8091, 1초 HLS target, 6개 playlist segment, 8개 최대 파일, camera별 최근 metadata 512개다. `RELAY_SESSION_ID`가 비어 있으면 Relay가 authoritative이며 연결된 최신 session을 자동 선택한다.

Receiver와 선택적 Relay를 함께 시작한다.

```bash
docker compose --profile web -f compose.receiver.yaml up -d --build --wait
docker compose --profile web -f compose.receiver.yaml ps
curl -fsS http://127.0.0.1:8091/healthz
```

`healthz`는 Relay process/GStreamer가 살아 있으면 200이다. Edge session과 첫 HLS pipeline까지 준비되어야 `readyz`가 200이다.

```bash
curl -fsS http://127.0.0.1:8091/readyz
curl -fsS http://127.0.0.1:8091/metrics | grep '^robot_\|^relay_'
```

Relay는 secret을 mount하지 않고 non-root/read-only/cap-drop/no-new-privileges로 실행한다. HLS 파일은 512 MiB 제한 tmpfs에만 둔다. Relay 장애는 Receiver health나 ingest를 실패시키지 않는다.

## 3. session과 URL 찾기

외부 gRPC에서 현재 session을 먼저 볼 수 있다.

```bash
RECEIVER_HOST=10.30.0.20
python scripts/receiver-metadata-client.py \
  --endpoint "${RECEIVER_HOST}:8083" sessions
```

URL consumer는 Relay catalog를 사용한다.

```bash
RELAY_ORIGIN=http://10.30.0.20:8091
curl -fsS "${RELAY_ORIGIN}/api/v1/streams" | jq .
```

예시 응답은 다음 형태다.

```json
[
  {
    "session_id": "07070707-0707-0707-0707-070707070707",
    "camera_id": "front-left",
    "camera_key": "66726f6e742d6c656674",
    "stream_epoch": 1,
    "playlist_url": "/live/07070707-0707-0707-0707-070707070707/66726f6e742d6c656674/index.m3u8",
    "metadata_url": "/metadata/07070707-0707-0707-0707-070707070707/66726f6e742d6c656674",
    "playlist_ready": true,
    "last_capture_time_edge_ns": 123456789,
    "last_media_pts_seconds": 12.3,
    "last_access_unit_ordinal": 369
  }
]
```

catalog URL은 relative path다. client는 자신이 사용한 `RELAY_ORIGIN`을 앞에 붙인다. session/epoch가 바뀌면 기존 URL을 정상 stream으로 재사용하지 말고 catalog를 다시 읽는다.

## 4. VLC와 URL player

VLC의 `미디어 → 네트워크 스트림 열기`에 catalog의 완전한 playlist URL을 넣는다.

```bash
vlc "${RELAY_ORIGIN}/live/${SESSION_ID}/${CAMERA_KEY}/index.m3u8"
```

다른 player도 표준 HLS URL을 사용한다.

```bash
ffplay -fflags nobuffer \
  "${RELAY_ORIGIN}/live/${SESSION_ID}/${CAMERA_KEY}/index.m3u8"
```

첫 playlist는 Relay가 구독한 뒤 IDR 경계에서 segment를 닫아야 나타난다. catalog가 있지만 `playlist_ready=false`라면 다음 keyframe까지 기다린다. HLS는 파일 단위 buffering이 있으므로 gRPC AU/`ffplay` 직접 연결보다 지연이 크다.

Relay를 사용하지 않을 때는 URL endpoint가 없다. TCP 8083은 protobuf gRPC이고, UDP 10000~10015는 인증된 Edge 입력 listener이며, TCP 8080은 reserved port다. 이 주소들을 VLC URL로 사용하지 않는다.

## 5. 별도 웹 프로젝트에서 영상과 metadata 함께 표시

가능하다. 영상은 HLS URL, metadata는 같은 catalog 항목의 SSE URL을 사용한다. Relay는 SSE 접속 시 camera별 최근 bounded history를 먼저 보내고 이어서 live event를 보내므로, HLS playlist에 이미 들어간 과거 frame의 metadata도 web client가 받을 수 있다.

Safari 계열의 native HLS 또는 hls.js를 사용한다. 아래 예시는 npm의 `hls.js`를 사용한다고 가정한다.

```html
<video id="camera" autoplay muted controls playsinline></video>
<pre id="metadata"></pre>
```

```javascript
import Hls from "hls.js";

const relayOrigin = "http://10.30.0.20:8091";
const catalog = await fetch(`${relayOrigin}/api/v1/streams`).then(r => {
  if (!r.ok) throw new Error(`catalog HTTP ${r.status}`);
  return r.json();
});
const stream = catalog.find(item => item.camera_id === "front-left");
if (!stream || !stream.playlist_ready) throw new Error("camera HLS is not ready");

const video = document.querySelector("#camera");
const playlist = relayOrigin + stream.playlist_url;
if (video.canPlayType("application/vnd.apple.mpegurl")) {
  video.src = playlist;
} else if (Hls.isSupported()) {
  const hls = new Hls({liveSyncDurationCount: 2});
  hls.loadSource(playlist);
  hls.attachMedia(video);
} else {
  throw new Error("this browser has no HLS playback path");
}

// Relay의 default history 512개를 수용하고 오래된 값은 제거한다.
const metadataByPts = [];
const events = new EventSource(relayOrigin + stream.metadata_url);
events.addEventListener("frame", message => {
  const frame = JSON.parse(message.data);
  metadataByPts.push(frame);
  if (metadataByPts.length > 1024) metadataByPts.splice(0, 512);
});

function renderFrame(_now, rendered) {
  // GStreamer가 normalized PTS를 MPEG-TS PTS와 SSE의 seconds에 같이 사용한다.
  const mediaPts = rendered.mediaTime;
  let nearest = null;
  let distance = Number.POSITIVE_INFINITY;
  for (const item of metadataByPts) {
    const delta = Math.abs(item.media_pts_seconds - mediaPts);
    if (delta < distance) {
      nearest = item;
      distance = delta;
    }
  }
  // 30 fps 기준 0.05초 이내만 같은 displayed frame으로 취급한다.
  document.querySelector("#metadata").textContent =
    nearest && distance <= 0.05
      ? JSON.stringify(nearest, null, 2)
      : "matching frame metadata is buffering";
  video.requestVideoFrameCallback(renderFrame);
}
video.requestVideoFrameCallback(renderFrame);
```

정확한 correlation key는 다음과 같다.

- player overlay: `media_pts_seconds`와 `requestVideoFrameCallback().mediaTime`의 가장 가까운 값
- audit/application identity: `(session_id, camera_id, stream_epoch, access_unit_ordinal)`
- multi-camera/robot 결합: `anchor_frame_seq`, `step_capture_time_edge_ns`, `skew_from_anchor_ns`

JavaScript number에서 큰 nanosecond integer의 정밀도가 줄 수 있으므로 UI 시간 정렬에는 `media_pts_seconds`를 사용한다. exact integer 보존이 필요한 client는 SSE JSON의 ordinal/epoch 또는 gRPC의 fixed64를 사용한다. browser/player가 MSE timestamp offset을 별도로 바꾸는 구성이라면 같은 constant offset을 보정하고 실제 브라우저에서 tolerance를 측정한다.

SSE의 첫 이벤트 ID는 `stream_epoch:access_unit_ordinal` 형식이다. slow client가 bounded broadcast를 따라가지 못하면 중간 preview event가 drop되고 `relay_sse_lag_total`이 증가한다. 정확한 모든 frame이 필요한 수집기는 SSE가 아니라 gRPC를 사용한다.

## 6. SSE frame metadata 상세

한 `frame` event는 한 camera AU와 그 AU가 선택된 synchronized step의 canonical anchor context를 함께 가진다.

| field | 의미 |
|---|---|
| `session_id` | Edge process가 만든 session UUID |
| `camera_id`, `camera_key` | manifest stable ID와 URL-safe key |
| `stream_epoch` | reconnect/discontinuity 세대 |
| `access_unit_ordinal` | 해당 camera/epoch의 단조 증가 AU 번호 |
| `capture_time_edge_ns` | camera AU의 공통 Edge monotonic capture time; Unix time이 아님 |
| `step_capture_time_edge_ns` | 선택 기준 anchor AU capture time |
| `skew_from_anchor_ns` | camera time - anchor time; anchor는 0 |
| `normalized_pts_ns`, `media_pts_seconds` | HLS MPEG-TS와 동일하게 설정한 media timeline |
| `anchor_frame_seq` | canonical robot context가 대응하는 anchor frame 번호 |
| `valid`, `invalid_reason`, `validity_flags` | step/context 사용 가능 여부 |
| `context_crc32c` | exact serialized anchor context의 Castagnoli CRC32C |
| `observation_schema_id`, `action_schema_id` | vector layout version |
| `named_features[]` | manifest slice를 적용한 이름/semantic/unit/shape/value/validity |
| `device_quality[]` | source clock, interpolation gap, residual, action source quality |

`named_features[]`는 `SessionManifestV1.feature_slices` 순서이며 각 항목은 `qualified_name`, `semantic`, `source_device_id`, `vector_kind`, `unit`, `shape`, `values`, `valid`, `required`를 제공한다. 따라서 web project가 RB-Y1 joint 순서를 하드코딩할 필요가 없다.

간단한 CLI 확인은 다음과 같다.

```bash
curl -Ns "${RELAY_ORIGIN}${METADATA_URL}" \
  | sed -n 's/^data: //p' \
  | jq '{camera_id, stream_epoch, access_unit_ordinal, media_pts_seconds,
         anchor_frame_seq, valid, named_features, device_quality}'
```

## 7. canonical gRPC 접근

웹 projection이 아니라 exact application feed가 필요하면 TCP 8083을 직접 사용한다. `ListSessions`로 session을 찾은 뒤 `GetSessionManifest`, `SubscribeSynchronizedSteps`를 사용한다.

```bash
ENDPOINT=10.30.0.20:8083
python scripts/receiver-metadata-client.py --endpoint "${ENDPOINT}" sessions
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" snapshot --session "${SESSION_ID}" > snapshot.json
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" watch --session "${SESSION_ID}" \
  --vectors --dump-dir ./received-h264 --max-steps 30 > steps.jsonl
```

`--dump-dir` 또는 `--view-camera`가 있어야 CLI가 `include_encoded_images=true`를 요청한다. 저장한 Annex-B 파일은 VLC/ffplay에서 열 수 있다.

```bash
vlc ./received-h264/front-left.h264
ffplay -f h264 ./received-h264/front-left.h264
```

Python client의 핵심은 다음과 같다.

```python
import sys
from pathlib import Path

import grpc

sys.path.insert(0, str(Path.cwd() / "python"))
from generated import receiver_api_pb2 as receiver_pb
from generated import receiver_api_pb2_grpc as receiver_grpc

channel = grpc.insecure_channel("10.30.0.20:8083")
stub = receiver_grpc.ReceiverMetadataStub(channel)
sessions = stub.ListSessions(receiver_pb.ListSessionsRequest(), timeout=10).sessions
session = next(s.session_id for s in sessions if s.authoritative and s.connected_cameras)

request = receiver_pb.SubscribeSynchronizedStepsRequest(
    session_id=session,
    include_encoded_images=True,
    camera_ids=[],
)
for step in stub.SubscribeSynchronizedSteps(request):
    assert step.valid, step.invalid_reason
    for frame in step.frames:
        key = (bytes(step.session_id), frame.camera_id,
               frame.stream_epoch, frame.access_unit_ordinal)
        print(key, frame.capture_time_edge_ns, frame.skew_from_anchor_ns,
              len(frame.encoded_image))
    print(step.anchor_context, step.anchor_context_packet.payload_crc32c)
```

`anchor_context_packet.serialized_context`를 다시 protobuf parse한 값은 `step.anchor_context`와 같아야 한다. CRC32C는 일반 `zlib.crc32`가 아닌 Castagnoli CRC32C다.

## 8. flattened vector를 이름으로 복원

gRPC consumer는 manifest slice를 직접 적용한다. SSE consumer는 같은 계산이 끝난 `named_features`를 받는다.

```python
vectors = {
    metadata_pb.FEATURE_VECTOR_KIND_OBSERVATION: step.observation_state,
    metadata_pb.FEATURE_VECTOR_KIND_ACTION: step.action,
    metadata_pb.FEATURE_VECTOR_KIND_AUXILIARY: step.auxiliary,
}
bitmap = step.anchor_context.feature_validity_bitmap

for index, feature in enumerate(manifest.feature_slices):
    vector = vectors[feature.vector_kind]
    begin, end = feature.offset, feature.offset + feature.length
    assert end <= len(vector)
    print({
        "name": feature.qualified_name,
        "values": list(vector[begin:end]),
        "valid": bool(bitmap[index // 8] & (1 << (index % 8))),
        "unit": feature.unit,
        "shape": list(feature.shape),
    })
```

required feature/device가 invalid이거나 schema/manifest revision이 다르면 정상 dataset/control metadata로 저장하지 않는다.

## 9. 왜 Receiver가 원래 gRPC로 영상을 제공했는가

gRPC는 단순 player protocol이 아니라 시스템의 canonical synchronized dataset contract다.

1. anchor context CRC, manifest/schema, camera identity와 timestamp를 Receiver가 검증한 뒤 publish한다.
2. 한 step에서 여러 camera AU와 observation/action/quality를 loss 없이 묶는다.
3. camera filter, exact fixed64 ordinal/time, optional encoded bytes와 backpressure 경계를 명시한다.
4. Dataset/audit consumer가 browser/HLS segment 정책에 종속되지 않는다.

HLS는 browser/VLC 호환성을 위한 편의 projection이다. segment/keyframe 지연, playlist window, slow-client drop을 허용하며 exact 모든 frame delivery를 보장하지 않는다. 그래서 Relay도 raw SRT를 다시 해석하지 않고 검증된 gRPC stream을 한 번 구독한다. 이 방식이 기존 synchronization/validation을 중복 구현하지 않으면서 URL 영상을 추가한다.

## 10. 성능과 용량

Relay pipeline은 `appsrc → h264parse → mpegtsmux → hlssink`이며 decoder와 encoder가 없다. 따라서 GPU encoder session을 추가로 쓰지 않고 화질/bitrate도 바꾸지 않는다.

- Receiver→Relay traffic: 모든 camera encoded bitrate 합 `ΣB`가 한 번 추가된다.
- HLS client traffic: 동시 viewer마다 선택한 camera bitrate가 Relay egress에 추가된다.
- HLS cache 근사: camera별 `bitrate × target duration × max files / 8` bytes.
- appsrc: camera별 최대 8 AU, downstream-leaky.
- metadata history: camera별 최대 `RELAY_METADATA_HISTORY_PER_STREAM`; default 512 event.
- SSE live queue: 전역 `RELAY_EVENT_BUFFER`; default 256 event.
- gRPC step decode: `RELAY_GRPC_MAX_MESSAGE_MIB`; default 64 MiB, 허용 4–256 MiB.

4 Mbps, 1초 segment, 최대 8 file이면 HLS payload cache는 camera당 약 4 MB, 16 camera면 약 64 MB다. metadata 크기는 feature 수에 비례하므로 production camera/robot schema로 RSS를 측정한다. 별도 host Relay면 `ΣB`가 실제 NIC traffic이고, 같은 Compose host면 대부분 bridge/memory copy 비용이다.

실제 synthetic 통합검증의 관측값과 1/4/16 camera 산정표는 `docs/implementation/WEB_RELAY_PERFORMANCE_ANALYSIS.md`에 있다. 작은 synthetic 수치를 production capacity로 그대로 사용하지 않는다.

## 11. 기본 평문 외부 연결과 선택적 보호

사내망 기본은 다음처럼 직접 publish된 port를 사용한다.

```bash
nc -vz 10.30.0.20 8083
nc -vz 10.30.0.20 8091
```

`RELAY_CORS_ALLOW_ORIGIN=*`도 기본값이므로 다른 사내 web origin에서 HLS/SSE를 읽을 수 있다. 이 구성은 암호화·사용자 인증이 없다. 인터넷에 그대로 port-forward하지 않는다.

보호가 필요하면 application을 바꾸지 않고 선택적으로 VPN/source-IP firewall 또는 TLS/auth reverse proxy를 8091 앞에 둔다. gRPC 8083은 gRPC를 지원하는 L4/L7 proxy 또는 SSH tunnel을 사용한다.

```bash
ssh -N \
  -L 18083:127.0.0.1:8083 \
  -L 18091:127.0.0.1:8091 \
  operator@receiver-host
```

H.264 pass-through HLS segment에는 원본 user-data-unregistered SEI가 남을 수 있다. 따라서 HLS URL 권한은 화면만이 아니라 in-band timestamp/anchor context 접근 권한으로 취급한다. SSE만 보호하고 HLS를 공개하는 것은 metadata를 완전히 보호하는 구성이 아니다.

## 12. 문제 확인 순서

1. `healthz` 200과 `readyz` body의 `receiver_grpc`, `active_session`, `hls_output`을 확인한다.
2. `receiver-metadata-client.py sessions`에서 authoritative/connected session을 확인한다.
3. catalog에 camera가 있고 `playlist_ready=true`인지 확인한다.
4. IDR 간격과 HLS target 때문에 첫 segment가 늦는지 확인한다.
5. `docker compose --profile web -f compose.receiver.yaml logs --tail=200 web-relay receiver`를 본다.
6. `/metrics`의 `relay_grpc_reconnect_total`, `relay_access_unit_gap_total`, `relay_pipeline_restart_total`, `relay_sse_lag_total`을 확인한다.
7. 웹 overlay가 늦으면 HLS playback delay보다 metadata history가 긴지, `media_pts_seconds` tolerance가 실제 FPS에 맞는지 확인한다.
8. exact frame 누락을 허용할 수 없으면 HLS/SSE가 아니라 gRPC를 사용한다.

전체 자동 검증은 실제 GStreamer remux/demux까지 실행한다.

```bash
METADATA_CLIENT_PYTHON="$PWD/.venv-tools/bin/python" \
  ./scripts/run-web-relay-test.sh
```
