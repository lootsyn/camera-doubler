# 외부 영상 및 메타데이터 접근

## 1. 제공되는 두 영상 경로

### Edge local UI 경로

각 카메라는 `/dev/video<VIRTUAL_CAMERA_START + stable slot>` v4l2loopback으로 출력된다. 로봇 host의 LeRobot UI나 `ffplay`가 사용하며 network/Receiver 장애와 분리된다.

### Receiver synchronized gRPC 경로

Receiver `:8083`의 `SubscribeSynchronizedSteps`는 anchor 시각에 맞춘 camera별 H.264 Annex-B access unit, timestamp/skew/epoch/PTS/AU ordinal, observation/action/context를 한 메시지로 제공한다. 외부 진단 화면, recorder, metadata consumer는 이 경로를 사용한다.

이 경로는 동기화된 dataset step feed다. 일반 CCTV처럼 모든 camera의 독립 full-rate browser preview가 아니며 missing required camera나 invalid context로 step이 drop되면 화면 frame도 전달되지 않는다.

현재 TCP 8080에는 HTTP/HLS/RTSP/browser player가 구현되어 있지 않고 `FrameReference.preview_uri`도 비어 있다. 공개 웹 시청이 필요하면 Receiver gRPC 뒤에 인증된 별도 relay를 두며 Edge SRT key나 Receiver gRPC를 인터넷에 직접 노출하지 않는다.

## 2. 안전한 외부 연결

Receiver metadata gRPC는 현재 application-level TLS가 없는 trusted-network endpoint다. 다음 중 하나를 사용한다.

- WireGuard/Tailscale 같은 운영 VPN.
- 사설 VLAN과 source-IP firewall.
- SSH local tunnel.

SSH tunnel 예:

```bash
ssh -N -L 18083:127.0.0.1:8083 operator@receiver-host
```

이후 client endpoint는 `127.0.0.1:18083`이다. TCP 9090 metrics도 필요한 경우 별도 tunnel을 열되 공개 인터넷에는 publish하지 않는다.

Edge→Receiver SRT UDP range는 viewer port가 아니다. camera별 한 Edge caller를 인증하는 ingest port이므로 VLC/ffplay를 같은 port에 추가 연결하지 않는다.

SRT listener의 stream ID callback과 reconnect property 의미는 [GStreamer `srtsrc` 공식 문서](https://gstreamer.freedesktop.org/documentation/srt/srtsrc.html)를 기준으로 한다.

개발 fixture를 수동 주입할 때는 Receiver가 실행 중인 상태에서 `scripts/send-synthetic-srt-fixture.sh`를 사용한다. 이 script는 secret 값을 출력하지 않는다.

문서의 전체 client 경로를 자동 검증하려면 gRPC dependency가 설치된 Python을 지정한다.

```bash
METADATA_CLIENT_PYTHON="$PWD/.venv-tools/bin/python" ./scripts/run-metadata-client-test.sh
```

## 3. Metadata client 준비

`SETUP_AND_SECRETS.md`의 `.venv-tools` 절차를 한 번 실행한다.

```bash
python3 -m venv .venv-tools
. .venv-tools/bin/activate
python -m pip install -r python/metadata_client/requirements.txt
python scripts/generate-proto.py
```

session UUID는 다음 로그 중 하나에서 얻는다. UUID와 camera ID는 non-secret이지만 raw signed stream ID 전체를 공유하지 않는다.

```bash
docker compose -f compose.edge.yaml logs edge-core | grep 'Edge session created'
docker compose -f compose.receiver.yaml logs receiver | grep 'authenticated SRT stream accepted'
```

아래 예에서는 값을 shell 변수에 넣는다.

```bash
SESSION_ID=00000000-0000-0000-0000-000000000000
ENDPOINT=127.0.0.1:18083
```

## 4. Catalog, anchor, manifest, quality 조회

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "$ENDPOINT" \
  snapshot --session "$SESSION_ID"
```

출력 JSON에는 다음이 포함된다.

- camera ID, stable slot, stream epoch, listen port, provisional role, connected/bootstrap 상태.
- authoritative `anchor_camera_id`와 manifest revision.
- camera catalog, feature slice/vector layout, clock/schema revision을 포함한 decoded manifest.
- camera별 received/dropped frame과 accepted/dropped synchronized step.

`manifest_validated=false`이거나 authoritative anchor가 없으면 metadata를 dataset 의미로 사용하지 않는다.

## 5. 동기화 메타데이터 실시간 구독

벡터 값을 제외한 요약:

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "$ENDPOINT" \
  watch --session "$SESSION_ID"
```

observation/action/auxiliary와 전체 decoded anchor context까지 출력:

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "$ENDPOINT" \
  watch --session "$SESSION_ID" --vectors
```

특정 camera만 선택하고 300 step 후 종료:

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "$ENDPOINT" \
  watch --session "$SESSION_ID" \
  --camera stable-camera-id --max-steps 300 --vectors
```

주요 field:

| field | 의미 |
|---|---|
| `capture_time_edge_ns` | 모든 camera/robot state가 정렬되는 Edge monotonic anchor 시각 |
| `frames[].skew_from_anchor_ns` | 선택된 camera frame과 anchor의 signed 차이 |
| `stream_epoch`, `access_unit_ordinal` | reconnect/discontinuity와 AU correlation 식별 |
| `observation_state`, `action`, `auxiliary` | manifest feature layout 순서의 flattened vector |
| `anchor_context` | schema/validity/device quality를 포함한 canonical decoded context |
| `anchor_context_packet_sha256` | exact CRC packet의 client-side audit fingerprint |
| `valid`, `invalid_reason` | 해당 step을 dataset/제어 분석에 사용해도 되는지 여부 |

## 6. 외부에서 화면 재생

`ffplay`가 설치된 client에서 snapshot 결과의 정확한 camera ID를 사용한다.

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "$ENDPOINT" \
  watch --session "$SESSION_ID" \
  --view-camera stable-camera-id
```

여러 camera는 `--view-camera`를 반복할 수 있다. subscriber가 IDR 중간에 시작하면 decoder warning이 잠시 보일 수 있으며 다음 keyframe/SPS/PPS에서 복구한다.

AU를 camera별 Annex-B 파일에 동시에 저장:

```bash
python scripts/receiver-metadata-client.py \
  --endpoint "$ENDPOINT" \
  watch --session "$SESSION_ID" \
  --dump-dir ./received-h264 --max-steps 900

ffplay -f h264 ./received-h264/stable-camera-id.h264
ffmpeg -f h264 -i ./received-h264/stable-camera-id.h264 -c copy received.mp4
```

`received-h264/`에는 영상이 포함되므로 운영 데이터 보존·접근 정책을 적용한다.

## 7. In-band metadata 원본 규칙

Receiver API 이전의 H.264 SEI 규칙은 다음과 같다.

| UUID | message | 위치 |
|---|---|---|
| `4a1191e6-9578-53b3-92a7-04c049fe0d5b` | `SyncTimestampV1` | 모든 streamed AU에 정확히 하나 |
| `62ef08bb-2eb4-59fb-b83f-f8f874a80043` | `AnchorFrameContextPacketV1` | anchor AU에만 존재, exact inner bytes CRC32C |
| `791a8fc5-d0c3-5abf-81da-abf7f0373194` | `SessionManifestChunkV1` | anchor에 반복되는 bounded manifest chunk |

일반 consumer는 직접 NAL을 파싱하지 말고 Receiver API를 사용한다. 직접 파싱해야 한다면 decoder 이전 byte-stream AU에서 UUID를 찾고, CRC/size/chunk count/compression ratio/timeout을 protocol constants대로 검증하며 non-anchor semantic metadata는 거부해야 한다.

## 8. Raw archive metadata 접근

Archive가 있을 때 envelope/index/hash와 SEI→step 복원을 함께 검증한다.

```bash
docker run --rm \
  -v "$ARCHIVE_ROOT:/archive:ro" \
  --entrypoint /usr/local/bin/robot-replay-verify \
  robot-multicam-receiver:local \
  /archive /archive/stream-envelope.json /archive/segments/index.jsonl /run/secrets/replay_hmac_key
```

실제 HMAC key는 secret mount로 전달한다. 성공 출력의 `metadata_sha256`, `step_sha256`, `deterministic=bit-for-bit`을 episode audit record에 보관한다.
