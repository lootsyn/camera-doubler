# Web Relay 성능 영향 분석

## 결론

H.264 pass-through/remux Relay는 decode/encode를 하지 않으므로 encoder session과 화질에는 영향이 없다. 비용은 Receiver의 gRPC 직렬화/전송 한 번, Relay의 H.264 parse와 MPEG-TS mux, bounded HLS/metadata memory, HTTP client별 전송이다. Edge capture와 Receiver ingest/recording은 별도 process/queue 경계라 Relay가 느려져도 직접 block하지 않는다.

Relay가 없는 상태와 비교하면 가장 확실한 증가는 encoded bitrate 한 벌이다. 카메라 encoded bitrate 합을 `ΣB`, step metadata bitrate를 `M`, MPEG-TS overhead 비율을 `T`, camera별 HLS viewer 수를 `Vᵢ`라 하면 다음과 같다.

```text
Receiver -> Relay       ≈ ΣB + M
Relay -> HLS clients    ≈ Σ(Bᵢ × Vᵢ × (1 + T))
Relay -> SSE clients    ≈ metadata JSON size × event rate × clients
```

`T`와 JSON 크기는 실제 H.264 NAL 크기, FPS, feature 수, client 수로 측정한다. gRPC protobuf framing과 MPEG-TS packetization 때문에 wire bytes는 원본 H.264 bytes보다 크다.

## 1/4/16 camera 용량 예시

camera당 4 Mbps, 1초 target, 최대 8 segment, camera당 HLS viewer 한 명을 가정한 payload 근사다. metadata와 protocol overhead는 제외했다.

| camera 수 | Receiver→Relay 추가 | HLS client egress 추가 | HLS payload cache | 두 경로 합계 추가 |
|---:|---:|---:|---:|---:|
| 1 | 4 Mbps | 4 Mbps | 4 MB | 8 Mbps |
| 4 | 16 Mbps | 16 Mbps | 16 MB | 32 Mbps |
| 16 | 64 Mbps | 64 Mbps | 64 MB | 128 Mbps |

Relay가 Receiver와 같은 host의 Compose bridge에 있으면 첫 열은 외부 NIC보다 kernel/container memory traffic의 영향이 크다. 별도 host라면 첫 열과 client egress가 모두 실제 NIC를 사용한다. viewer가 늘면 HLS egress만 선형 증가한다.

## CPU 영향

Relay-off에서는 이 경로의 CPU가 0이고 Receiver는 encoded payload를 Relay용으로 직렬화하지 않는다. Relay-on에서는 다음이 추가된다.

- Receiver: 한 gRPC subscriber에 대한 protobuf/HTTP2 serialization과 socket copy
- Relay: protobuf decode, `h264parse`, `mpegtsmux`, playlist/file 관리
- HTTP: segment 요청마다 file read와 response buffer, SSE JSON serialization/전송

decoder/encoder가 없으므로 transcoding Relay보다 훨씬 작지만, CPU 증가율을 camera 수만으로 고정할 수는 없다. AU 수, bitrate, feature 수, viewer 수, TLS reverse proxy 사용 여부에 따라 달라진다. 일반식은 camera/AU 처리에 대해 대체로 선형이고, HLS 전송은 viewer 수에 대해 선형이다.

## Memory와 storage 상한

기본 상한은 다음과 같다.

| 항목 | 기본값 | 상한 방식 |
|---|---:|---|
| GStreamer appsrc | camera당 8 AU | non-blocking downstream-leaky |
| SSE broadcast | 전체 256 event | lagging client event drop |
| metadata history | camera당 512 event | 오래된 event부터 제거 |
| gRPC decoded step | 64 MiB | config 허용 4–256 MiB |
| HLS playlist | 6 segment | bounded window |
| HLS files | camera당 최대 8 | `hlssink max-files` |
| HLS tmpfs | container 전체 512 MiB | Compose tmpfs size |

metadata history heap 근사는 `camera 수 × history 수 × 평균 JSON 전 event object 크기`다. `named_features`가 전체 vector 값을 포함하므로 큰 robot schema에서는 이 항목이 HLS cache보다 커질 수 있다. `RELAY_METADATA_HISTORY_PER_STREAM`은 HLS의 실제 wall-clock 지연을 덮는 최소값으로 조정한다.

Compose Relay에는 1 GiB memory, 2 CPU, 256 PID 상한을 둔다. HLS tmpfs 512 MiB와 process heap이 같은 memory budget을 소비하므로 production은 OOM 전 여유를 관측해야 한다.

## 지연

HLS 첫 화면 지연은 대략 다음 합이다.

```text
session discovery + 다음 IDR 대기 + segment target/마감 + playlist polling + player buffer
```

기본 target은 1초다. Edge keyframe 간격이 target보다 길면 segment는 IDR까지 늘어난다. 짧은 지연을 위해 무작정 target을 줄이면 file/request churn이 증가하고 GOP를 재인코딩 없이 바꿀 수는 없다.

## 2026-07-23 synthetic 관측

`scripts/run-web-relay-test.sh`가 320×240 H.264 48 AU를 4 AU/s wall-clock으로 보내고, 동시에 native gRPC, HLS demux, SSE history/live correlation을 검사한 인접 최종 build의 세 회 샘플 범위다. 마지막 build 차이는 bounded gRPC decode 한도 명시뿐이다.

| 항목 | 관측값 |
|---|---:|
| Receiver CPU | 0.79–0.85% |
| Receiver RSS | 34.99–37.52 MiB |
| Relay CPU | 0.51–0.60% |
| Relay RSS | 11.98–19.86 MiB |
| 첫 HLS playlist wall-clock | 8.029–10.270 s |

이 값은 기능 검증용 snapshot이며 production benchmark가 아니다. fixture PTS는 30 fps지만 network pacing은 4 fps이므로 약 8초를 정상 30 fps HLS 지연으로 해석하면 안 된다. 유효한 결론은 pass-through container가 작은 fixture에서 low-single-digit CPU와 작은 RSS로 실제 remux/demux를 수행했다는 것뿐이다.

## production 비교 측정 절차

동일 host, 동일 camera bitrate/GOP, 동일 10분 구간을 Relay-off와 Relay-on으로 각각 측정한다.

1. Receiver의 accepted/dropped step, broadcast lag, CPU/RSS, network bytes를 baseline으로 기록한다.
2. Relay를 켜되 viewer 없이 Receiver와 Relay의 CPU/RSS 및 bridge/NIC bytes를 기록한다.
3. camera당 viewer 1명, 예상 최대 동시 viewer에서 HLS egress와 HTTP memory를 기록한다.
4. `relay_access_unit_gap_total`, `relay_pipeline_restart_total`, `relay_sse_lag_total`이 증가하지 않는지 확인한다.
5. first-frame/p95 playback latency와 SSE-to-render PTS 오차를 실제 browser에서 측정한다.
6. Receiver accepted/dropped step과 raw archive hash가 baseline과 같아야 한다.

승인 기준은 host별로 정하되 CPU/NIC/disk가 정상 peak에서도 30% 이상 여유를 남기고, Relay-on이 Receiver ingest/dataset counter를 악화시키지 않게 한다. 넘으면 Relay를 별도 host로 이동하거나 camera/viewer 수, HLS window/history를 줄인다.
