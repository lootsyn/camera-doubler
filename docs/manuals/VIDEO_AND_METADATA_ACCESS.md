# 외부 영상 및 프레임 메타데이터 접근

이 문서는 Receiver와 client가 같은 사내망에 있고 TCP 8083에 직접 접근할 수 있다고 가정한다. 현재 기본 경로는 TLS와 사용자 인증이 없는 평문 gRPC다. VPN, SSH tunnel 같은 보호 연결은 마지막 절의 선택 사항으로 설명한다.

## 1. 먼저 보는 결론

| 원하는 작업 | 현재 지원 방법 |
|---|---|
| 사내 PC에서 실시간 영상 보기 | 제공 client의 `watch --view-camera CAMERA_ID`로 H.264 AU를 `ffplay`에 전달 |
| 프레임별 영상과 메타데이터 받기 | `SubscribeSynchronizedSteps(include_encoded_images=true)` gRPC 구독 |
| 로봇 feature를 이름별로 해석 | manifest의 `feature_slices`로 `observation_state`, `action`, `auxiliary`를 slice |
| 파일로 저장한 뒤 VLC에서 보기 | `watch --dump-dir`로 `.h264` 저장 후 VLC에서 파일 열기 |
| VLC에서 `http://`, `rtsp://`, `srt://` URL 직접 열기 | **현재 미지원**. Receiver에 viewer용 media endpoint/relay가 없음 |
| 원본 in-band 증거 검사 | H.264 SEI 또는 raw archive를 replay verifier로 검증 |

가장 짧은 실시간 확인은 다음과 같다.

```bash
RECEIVER_HOST=10.30.0.20
SESSION_ID=00000000-0000-0000-0000-000000000000
CAMERA_ID=stable-camera-id

python scripts/receiver-metadata-client.py \
  --endpoint "${RECEIVER_HOST}:8083" \
  watch --session "${SESSION_ID}" \
  --camera "${CAMERA_ID}" --view-camera "${CAMERA_ID}"
```

이 명령은 VLC용 URL을 만드는 것이 아니라 gRPC에서 받은 H.264 Annex-B access unit을 local `ffplay` 표준입력으로 전달한다.

## 2. VLC용 URL이 없는 이유

현재 구현의 port와 역할은 다음과 같다.

| 시도할 수 있는 주소 | 실제 역할 | 재생되지 않는 이유 |
|---|---|---|
| `http://RECEIVER:8080/...` | Compose에 남아 있는 reserved TCP port | HTTP/HLS server나 route가 구현되어 있지 않음 |
| `http://RECEIVER:8083/...` | `ReceiverMetadata` 평문 gRPC/HTTP2 | MPEG-TS/HLS 같은 media resource가 아니라 protobuf-framed RPC임 |
| `rtsp://RECEIVER:PORT/...` | 없음 | Receiver에 RTSP server, mount point, SDP 생성기가 없음 |
| `srt://RECEIVER:10000` | camera slot 0의 **입력** listener | Edge sender용 signed stream ID/passphrase를 검증하는 ingest이며 viewer용 fan-out이 아님 |
| `FrameReference.preview_uri` | API field만 예약 | 현재 runtime이 빈 문자열을 반환함 |

VLC가 HTTP stream을 재생하려면 누군가가 HTTP access output과 MPEG-TS 같은 mux를 실제로 publish해야 한다. 현재 Receiver는 이를 만들지 않는다. VLC의 공식 HTTP streaming 예제도 송출 측이 `access=http`, `mux=ts`, `dst`를 제공하는 구조다: [VLC Stream over HTTP](https://docs.videolan.me/vlc-user/desktop/3.0/en/advanced/streaming/stream_over_http.html).

SRT player 지원 여부와 무관하게 이 시스템의 UDP 10000~10015는 시청 endpoint가 아니다. 한 camera의 Edge caller를 인증해 Receiver로 수집하는 방향이며, 받은 stream을 다른 client에 재송출하지 않는다. 임의 VLC 연결은 signed `rmc1` stream identity 검증에서 거부되거나 정상 ingest와 충돌할 수 있다.

이 설계의 외부 feed는 CCTV preview가 아니라 **anchor frame 단위 dataset feed**다. Receiver는 manifest와 anchor context를 검증하고, secondary camera에서 허용 skew 이내의 가장 가까운 AU를 선택한 뒤에야 한 `SynchronizedDatasetStep`을 publish한다. required camera 누락, 잘못된 context 또는 subscriber lag가 있으면 독립적인 상시 preview처럼 동작하지 않는다.

### URL 재생 기능을 추가할 때 필요한 별도 구성

VLC URL이 필수라면 Receiver 뒤에 다음 계약을 가진 relay를 별도 구현해야 한다.

1. `SubscribeSynchronizedSteps(include_encoded_images=true)`를 session/camera별로 구독한다.
2. IDR/SPS/PPS 이전 client 접속을 처리할 keyframe cache를 둔다.
3. H.264 AU를 MPEG-TS/HTTP 또는 RTSP로 mux/fan-out한다.
4. 예: `http://relay:8091/sessions/{session_id}/cameras/{camera_id}.ts` 같은 안정적인 URL을 발급한다.
5. slow client가 Receiver gRPC subscriber나 ingest를 막지 않도록 bounded queue와 drop policy를 둔다.
6. 영상 URL과 별도로 아래 gRPC metadata를 유지한다. VLC 화면만으로는 protobuf robot context를 조회할 수 없다.

이 relay는 현재 repository에 구현되어 있지 않으므로 위 형태의 URL을 운영 endpoint로 가정하면 안 된다.

## 3. 사내망 평문 연결 준비

Receiver 기본 설정은 `RECEIVER_GRPC_BIND=0.0.0.0:8083`이며 Compose가 TCP 8083을 host에 publish한다. client PC에서 Receiver의 사내 IP로 직접 연결한다.

```bash
RECEIVER_HOST=10.30.0.20
ENDPOINT="${RECEIVER_HOST}:8083"
nc -vz "${RECEIVER_HOST}" 8083
```

PowerShell에서는 다음으로 확인한다.

```powershell
$ReceiverHost = "10.30.0.20"
$Endpoint = "${ReceiverHost}:8083"
Test-NetConnection $ReceiverHost -Port 8083
```

이 기본 연결은 `grpc.insecure_channel()`을 사용한다. 패킷 암호화, server identity 검증, 사용자 인증이 없으므로 신뢰할 수 있는 사내 VLAN과 방화벽 안에서만 사용한다. 외부 인터넷에 그대로 port-forward하지 않는다.

client dependency와 generated protobuf를 준비한다.

```bash
python3 -m venv .venv-tools
. .venv-tools/bin/activate
python -m pip install -r python/metadata_client/requirements.txt
python scripts/generate-proto.py
python scripts/receiver-metadata-client.py --help
```

Windows에서는 activation 없이 `.venv-tools\Scripts\python.exe`를 아래 `python` 대신 사용할 수 있다.

## 4. Session ID와 camera ID 찾기

Edge가 시작할 때 생성한 session UUID는 secret이 아니다. raw signed stream ID 전체나 secret은 출력하지 않는다.

```bash
docker compose -f compose.edge.yaml logs edge-core \
  | grep 'Edge session created'

docker compose -f compose.receiver.yaml logs receiver \
  | grep 'authenticated SRT stream accepted'
```

찾은 UUID로 catalog, authoritative anchor, manifest, quality를 한 번에 조회한다.

```bash
SESSION_ID=00000000-0000-0000-0000-000000000000

python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" \
  snapshot --session "${SESSION_ID}" \
  > session-snapshot.json
```

```bash
jq '.cameras[] | {camera_id, stream_slot, stream_epoch, connected, manifest_validated}' \
  session-snapshot.json
jq '.anchor, .quality' session-snapshot.json
jq '.manifest.feature_slices' session-snapshot.json
```

`manifest_validated=true`이고 anchor의 `authoritative=true`인 session만 canonical metadata source로 사용한다.

## 5. 현재 지원되는 영상 보기와 저장

### 5.1 실시간 `ffplay`

client host에 `ffplay`가 있어야 한다.

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" \
  watch --session "${SESSION_ID}" \
  --camera stable-camera-id \
  --view-camera stable-camera-id
```

여러 camera는 `--camera`와 `--view-camera`를 반복한다. 각 camera마다 별도 창이 열린다.

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" \
  watch --session "${SESSION_ID}" \
  --camera front-left --camera wrist \
  --view-camera front-left --view-camera wrist
```

subscriber가 IDR 중간에 시작하면 다음 keyframe/SPS/PPS까지 decoder warning이나 빈 화면이 보일 수 있다.

### 5.2 H.264 파일 저장 후 VLC 재생

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" \
  watch --session "${SESSION_ID}" \
  --camera stable-camera-id \
  --dump-dir ./received-h264 --max-steps 900
```

VLC에서 `received-h264/stable-camera-id.h264` 파일을 열거나 다음처럼 실행한다.

```bash
vlc ./received-h264/stable-camera-id.h264
ffplay -f h264 ./received-h264/stable-camera-id.h264
ffmpeg -f h264 -i ./received-h264/stable-camera-id.h264 -c copy received.mp4
```

`--dump-dir`은 같은 이름의 파일에 append하므로 새 capture를 시작할 때 별도 빈 directory를 사용한다.

## 6. 프레임 메타데이터가 만들어지는 과정

여기서 frame은 decoder 출력 bitmap이 아니라 하나의 coded-picture H.264 access unit(AU)을 뜻한다.

1. 모든 camera AU에는 `SyncTimestampV1.capture_time_edge_ns`가 exactly one SEI로 들어간다.
2. anchor camera AU에는 그 frame에 맞춘 `AnchorFrameContextPacketV1`이 추가된다.
3. packet의 `serialized_context`는 `AnchorFrameContextV1` protobuf이며 `payload_crc32c`로 exact bytes를 검증한다.
4. anchor에는 session 시작/변경 시 `SessionManifestChunkV1`도 실린다.
5. Receiver는 decode 전에 SEI를 꺼내 timestamp, CRC, schema/session/manifest revision을 검증한다.
6. accepted anchor AU마다 secondary camera에서 timestamp가 가장 가까운 AU를 선택한다.
7. 선택된 camera frame들과 anchor의 robot context를 하나의 `SynchronizedDatasetStep`으로 gRPC publish한다.

따라서 secondary AU 자체에는 로봇 observation/action이 반복 저장되지 않는다. secondary frame의 `capture_time_edge_ns`와 `skew_from_anchor_ns`로 같은 step의 anchor robot context에 결합한다.

## 7. CLI로 frame별 metadata 꺼내기

### 7.1 한 step 전체를 JSON으로 받기

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" \
  watch --session "${SESSION_ID}" \
  --vectors --max-steps 1 \
  > one-step.jsonl
```

`--vectors`가 없으면 vector 길이와 frame correlation 정보만 출력한다. `--vectors`를 주면 실제 `observation_state`, `action`, `auxiliary` 값과 decoded `anchor_context`까지 출력한다.

camera별 frame metadata만 확인한다.

```bash
jq -c '.frames[] | {
  camera_id,
  capture_time_edge_ns,
  skew_from_anchor_ns,
  stream_epoch,
  normalized_pts_ns,
  access_unit_ordinal,
  encoded_bytes
}' one-step.jsonl
```

robot/context validity를 확인한다.

```bash
jq '{
  step_time: .capture_time_edge_ns,
  valid,
  invalid_reason,
  manifest_revision,
  observation_state,
  action,
  auxiliary,
  context: .anchor_context,
  context_packet_sha256: .anchor_context_packet_sha256
}' one-step.jsonl
```

### 7.2 주요 field의 정확한 의미

| 위치 | 의미 |
|---|---|
| `SynchronizedDatasetStep.capture_time_edge_ns` | 기준 anchor AU의 Edge monotonic capture time. Unix epoch가 아님 |
| `frames[].camera_id` | manifest의 `stable_camera_id` |
| `frames[].capture_time_edge_ns` | 해당 camera AU의 공통 Edge timebase timestamp |
| `frames[].skew_from_anchor_ns` | `camera capture time - anchor capture time`; anchor는 0 |
| `frames[].stream_epoch` | reconnect/discontinuity 세대. epoch가 바뀌면 연속 stream으로 간주하지 않음 |
| `frames[].normalized_pts_ns` | Receiver media timeline에서 정규화된 AU PTS |
| `frames[].access_unit_ordinal` | 해당 stream epoch의 단조 증가 AU 번호 |
| `frames[].encoded_image` | request의 `include_encoded_images=true`일 때만 들어오는 H.264 Annex-B AU |
| `observation_state/action/auxiliary` | anchor context vector의 편의 mirror |
| `anchor_context` | schema ID, frame sequence, 전체 vector, feature bitmap, device clock/quality, validity flag를 가진 canonical context |
| `anchor_context_packet` | 전송된 exact serialized context와 CRC32C를 보존한 audit packet |
| `valid/invalid_reason` | 해당 step 전체를 dataset/control 분석에 사용할 수 있는지 표시 |

`frames` 배열은 camera ID로 정렬되므로 첫 원소가 anchor라고 가정하지 않는다. snapshot의 `manifest.anchor_camera_id`와 `frame.camera_id`를 비교한다.

## 8. Python gRPC에서 exact frame/AU/context 접근

아래 코드는 CLI 내부와 같은 평문 gRPC를 사용한다. `include_encoded_images=True`가 실제 H.264 bytes를 요청하는 핵심이다.

```python
import sys
import uuid
from pathlib import Path

import grpc

repo = Path("/path/to/camera-doubler")
sys.path.insert(0, str(repo / "python"))

from generated import frame_metadata_pb2 as metadata_pb
from generated import receiver_api_pb2 as receiver_pb
from generated import receiver_api_pb2_grpc as receiver_grpc

session = uuid.UUID("00000000-0000-0000-0000-000000000000").bytes
channel = grpc.insecure_channel("10.30.0.20:8083")
stub = receiver_grpc.ReceiverMetadataStub(channel)

# Vector layout과 anchor camera ID를 먼저 고정한다.
manifest_reply = stub.GetSessionManifest(
    receiver_pb.GetSessionManifestRequest(session_id=session), timeout=10
)
manifest = metadata_pb.SessionManifestV1()
manifest.ParseFromString(manifest_reply.serialized_session_manifest)

request = receiver_pb.SubscribeSynchronizedStepsRequest(
    session_id=session,
    include_encoded_images=True,
    camera_ids=[],  # empty: manifest dataset camera set
)

for step in stub.SubscribeSynchronizedSteps(request):
    context = step.anchor_context
    packet = step.anchor_context_packet

    print("anchor_seq", context.anchor_frame_seq)
    print("step_time", step.capture_time_edge_ns)
    print("valid", step.valid, step.invalid_reason)
    print("context_crc32c", packet.payload_crc32c)
    print("exact_context_bytes", len(packet.serialized_context))

    for frame in step.frames:
        print(
            frame.camera_id,
            frame.capture_time_edge_ns,
            frame.skew_from_anchor_ns,
            frame.stream_epoch,
            frame.access_unit_ordinal,
            frame.encoded_image_media_type,
            len(frame.encoded_image),
        )
        if frame.camera_id == manifest.anchor_camera_id:
            with Path("anchor.h264").open("ab") as output:
                output.write(frame.encoded_image)

    break

channel.close()
```

`packet.serialized_context`를 다시 parse하면 `step.anchor_context`와 동일해야 한다.

```python
decoded = metadata_pb.AnchorFrameContextV1()
decoded.ParseFromString(step.anchor_context_packet.serialized_context)
assert decoded == step.anchor_context
assert decoded.anchor_frame_seq == next(
    frame.access_unit_ordinal
    for frame in step.frames
    if frame.camera_id == manifest.anchor_camera_id
)
```

CRC32C는 일반 `zlib.crc32`가 아니다. 직접 검증하는 consumer는 Castagnoli CRC32C 구현을 사용해야 한다. Receiver가 성공적으로 publish한 step은 이미 packet CRC와 context invariant를 검증한 결과지만, archive/audit consumer는 exact packet bytes와 CRC를 다시 보존·검증한다.

## 9. Flattened vector를 feature 이름으로 복원

실제 robot metadata의 이름, offset, 길이, 단위, shape는 `SessionManifestV1.feature_slices`가 정의한다. 코드에 RB-Y1 joint 순서를 하드코딩하지 않는다.

```python
vectors = {
    metadata_pb.FEATURE_VECTOR_KIND_OBSERVATION: step.observation_state,
    metadata_pb.FEATURE_VECTOR_KIND_ACTION: step.action,
    metadata_pb.FEATURE_VECTOR_KIND_AUXILIARY: step.auxiliary,
}

bitmap = step.anchor_context.feature_validity_bitmap

for index, feature in enumerate(manifest.feature_slices):
    vector = vectors[feature.vector_kind]
    begin = feature.offset
    end = begin + feature.length
    values = list(vector[begin:end])
    feature_valid = bool(bitmap[index // 8] & (1 << (index % 8)))

    print({
        "name": feature.qualified_name,
        "semantic": feature.semantic,
        "source_device_id": feature.source_device_id,
        "unit": feature.unit,
        "shape": list(feature.shape),
        "values": values,
        "valid": feature_valid,
        "required": feature.required,
    })
```

다음 invariant를 consumer에서 확인한다.

- `offset + length`가 해당 vector 길이를 넘지 않는다.
- manifest의 observation/action/auxiliary vector length와 실제 step 길이가 같다.
- `feature_validity_bitmap`의 bit `i`는 `feature_slices[i]`에 대응한다.
- `anchor_context.manifest_revision`, observation/action schema ID가 manifest와 같다.
- `anchor_context.device_quality[]`에서 required device의 `valid`, clock residual, interpolation gap을 확인한다.
- `anchor_context.validity_flags`와 `step.valid`를 모두 확인하고 invalid step을 정상 데이터로 저장하지 않는다.

## 10. 원본 H.264 SEI에서 직접 꺼낼 때

일반 application은 검증과 동기화를 끝낸 gRPC step을 사용한다. decoder 이전 원본 증거를 다뤄야 하는 recorder/replay 도구만 SEI를 직접 읽는다.

| user-data-unregistered UUID | protobuf | 위치 |
|---|---|---|
| `4a1191e6-9578-53b3-92a7-04c049fe0d5b` | `SyncTimestampV1` | 모든 coded-picture AU에 정확히 하나 |
| `62ef08bb-2eb4-59fb-b83f-f8f874a80043` | `AnchorFrameContextPacketV1` | anchor AU에만 하나 |
| `791a8fc5-d0c3-5abf-81da-abf7f0373194` | `SessionManifestChunkV1` | anchor의 bounded manifest chunk |

직접 parser를 만들 때는 `crates/metadata-codec`과 `config/protocol_constants.toml`을 기준으로 다음을 모두 지켜야 한다.

1. Annex-B start code로 NAL을 분리하고 NAL type 6 SEI를 찾는다.
2. emulation-prevention byte를 제거한 RBSP에서 payload type 5를 찾는다.
3. 앞 16-byte UUID로 protobuf type을 선택한다.
4. timestamp exactly-one, anchor-only semantic metadata 규칙을 검사한다.
5. context의 Castagnoli CRC32C를 exact `serialized_context` bytes에 대해 검사한다.
6. manifest chunk count/index/size/CRC/compression limit와 timeout을 검사한다.
7. manifest/session/schema ID가 connection envelope 및 context와 같은지 검사한다.

단순히 VLC로 영상을 재생하는 것만으로는 이 protobuf SEI를 application metadata로 얻을 수 없다. 프레임 메타데이터 consumer는 gRPC 또는 repository의 replay/metadata codec을 사용한다.

## 11. Raw archive에서 복원·검증

```bash
docker run --rm \
  -v "${ARCHIVE_ROOT}:/archive:ro" \
  -v "${REPLAY_HMAC_KEY}:/run/secrets/replay_hmac_key:ro" \
  --entrypoint /usr/local/bin/robot-replay-verify \
  robot-multicam-receiver:local \
  /archive \
  /archive/stream-envelope.json \
  /archive/segments/index.jsonl \
  /run/secrets/replay_hmac_key
```

실제 HMAC key는 secret mount로 전달한다. `metadata_sha256`, `step_sha256`, `deterministic=bit-for-bit` 결과를 episode audit record와 함께 보관한다.

전체 client 경로는 synthetic SRT ingest까지 포함해 자동 검증할 수 있다.

```bash
METADATA_CLIENT_PYTHON="$PWD/.venv-tools/bin/python" \
  ./scripts/run-metadata-client-test.sh
```

## 12. 선택 사항: 보호된 외부 연결

사내망 밖이나 신뢰 경계가 다른 network에서는 평문 8083 직접 연결 대신 VPN, source-IP firewall 또는 SSH tunnel을 사용한다.

```bash
ssh -N -L 18083:127.0.0.1:8083 operator@receiver-host
```

이 경우 endpoint만 localhost tunnel로 바꾼다.

```bash
ENDPOINT=127.0.0.1:18083
python scripts/receiver-metadata-client.py \
  --endpoint "${ENDPOINT}" snapshot --session "${SESSION_ID}"
```

보호 연결을 사용해도 application protocol은 동일하며 영상/metadata field와 동기화 의미는 바뀌지 않는다.

## 13. 문제 확인 순서

1. `nc -vz RECEIVER 8083` 또는 `Test-NetConnection`으로 network path를 확인한다.
2. session UUID가 현재 Edge process의 값인지 확인한다.
3. `snapshot`에서 manifest와 authoritative anchor가 존재하는지 확인한다.
4. camera의 `connected`, `manifest_validated`, epoch와 quality counter를 확인한다.
5. `watch --max-steps 1 --vectors`로 metadata만 먼저 받는다.
6. 그다음 `--camera`와 `--view-camera` 또는 `--dump-dir`로 encoded AU를 요청한다.
7. 화면이 늦으면 IDR/SPS/PPS 대기 여부와 subscriber lag를 확인한다.
8. `http://RECEIVER:8080`, RTSP 또는 ingest SRT port를 viewer URL로 시도하지 않는다.
