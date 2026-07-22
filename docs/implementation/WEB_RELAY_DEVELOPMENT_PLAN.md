# Web Relay 개발계획

## 목표

기존 Receiver의 외부 평문 gRPC `:8083` 접근을 그대로 유지하면서, 별도 non-root Docker container가 Receiver를 구독해 browser용 HLS URL과 frame-correlated metadata SSE URL을 제공한다. Relay 장애나 느린 browser가 Edge ingest, Receiver synchronization, raw recording, Dataset Builder를 중단시키지 않아야 한다.

## 확정 아키텍처

```text
Edge --SRT/H.264--> Receiver --native gRPC, one subscription--> Web Relay
                           \                               |-- HTTP HLS URL
                            \-- external gRPC :8083       `-- HTTP metadata SSE
```

- 외부 gRPC: `ReceiverMetadata` wire contract와 TCP 8083을 유지한다.
- 영상 URL: `/live/{session_id}/{camera_key}/index.m3u8`와 `.ts` segment.
- metadata URL: `/metadata/{session_id}/{camera_key}` SSE.
- catalog URL: `/api/v1/streams`에서 실제 camera ID, session, epoch와 두 URL을 찾는다.
- media 처리: H.264 decode/encode 없이 `appsrc -> h264parse -> mpegtsmux -> hlssink` pass-through/remux.
- correlation: SSE에 `stream_epoch`, `access_unit_ordinal`, capture time, normalized PTS, skew, vectors, validity와 named feature를 보낸다.

## 구현 범위

1. Receiver API에 active session discovery를 추가한다.
2. H.264 protobuf bytes의 불필요한 deep-copy를 줄인다.
3. Rust `web-relay` workspace crate를 추가한다.
4. Relay는 Receiver를 한 번만 구독하고 camera별 GStreamer HLS pipeline으로 fan-out한다.
5. HTTP catalog, HLS static files, metadata SSE, health/readiness/metrics를 제공한다.
6. Relay 전용 multi-stage Docker image와 Receiver Compose service를 추가한다.
7. synthetic SRT fixture로 gRPC와 HLS/SSE가 동시에 동작함을 검증한다.
8. 운영·접근·성능 매뉴얼과 release evidence를 갱신한다.

## 비범위

- H.264 transcoding, 해상도/bitrate 변경.
- 공개 인터넷용 인증 서비스나 TLS termination.
- Relay를 Receiver readiness의 필수 dependency로 만드는 변경.
- 일반 `<video>`의 native HLS 지원을 가정하는 것. Web UI는 native HLS 또는 hls.js 계열 player를 선택한다.
- VLC에서 protobuf metadata를 직접 노출하는 기능.

## 구현 단계

### A. Receiver contract

- `ListSessions` unary RPC와 `SessionStatus`를 추가한다.
- authoritative manifest, anchor camera, connected camera 수, last capture time, accepted/dropped step을 반환한다.
- 기존 RPC field number와 의미는 변경하지 않는다.
- Relay는 `RELAY_SESSION_ID` override가 없으면 authoritative이며 connected camera가 있는 session 중 last capture time이 가장 최근인 항목을 선택한다.
- discovery poll 중 더 최신 session이 나타나면 기존 subscription을 취소하고 전환한다. 이전 session URL은 더 이상 active catalog에 노출하지 않는다.
- 구버전 Receiver가 `ListSessions`를 지원하지 않으면 Relay는 ready가 되지 않고 bounded retry하며 기존 Receiver RPC에는 영향을 주지 않는다.

### B. Relay ingest

- `ListSessions -> GetSessionManifest -> SubscribeSynchronizedSteps` 순서로 연결한다.
- `include_encoded_images=true`, 빈 camera filter로 단일 구독한다.
- session 변경, gRPC disconnect, Receiver restart에 bounded exponential reconnect를 적용한다.
- 새 session/epoch에서는 기존 HLS pipeline을 종료하고 새 pipeline으로 전환한다.
- protobuf generation에서 `FrameReference.encoded_image`를 `bytes::Bytes`로 생성해 broadcast/subscriber clone이 AU payload를 deep-copy하지 않게 한다. Python/wire representation은 변경하지 않는다.
- GStreamer `appsrc`는 non-blocking, bounded, downstream-leaky로 설정해 HLS writer가 gRPC receive loop를 멈추지 않게 한다.

### C. HLS output

- camera ID를 path에 직접 넣지 않고 reversible safe key로 변환한다.
- GStreamer buffer PTS/DTS는 `normalized_pts_ns`를 사용한다.
- segment는 IDR 경계에서 생성하며 current Edge의 SPS/PPS와 30-frame keyint를 유지한다.
- HLS window와 file 수를 bounded config로 제한한다.
- Relay가 생성한 고정 `live` 하위 directory만 정리하고, empty/root output path를 거부하며 임의 host path는 삭제하지 않는다.
- epoch 변경 또는 GStreamer flow error는 해당 camera pipeline만 재생성하고 다른 camera와 Receiver subscription은 유지한다.

### D. Web metadata

- 한 SSE event는 한 synchronized step에서 선택된 camera frame 하나와 canonical anchor context를 연결한다.
- manifest feature slice를 적용해 이름, semantic, unit, shape, values, validity를 제공한다.
- slow SSE consumer는 bounded broadcast에서 오래된 preview event를 건너뛴다.
- metadata event는 `Arc`로 fan-out하고 camera별로 전체 protobuf step을 다시 복제하지 않는다.
- 원본 gRPC와 replay contract가 canonical source이며 SSE는 web projection임을 명시한다.

### E. Container/Compose

- non-root UID/GID, read-only rootfs, `cap_drop: ALL`, `no-new-privileges`.
- HLS output은 size-limited tmpfs에 둔다.
- Receiver Compose network에서 `receiver:8083`만 사용한다.
- host에는 Relay HTTP port 하나만 publish한다.
- Relay failure가 Receiver/Dataset Builder를 restart시키지 않는다.
- 기본 HTTP/SSE는 사내망용 평문이며 CORS origin 기본값은 명시적으로 설정한다. 인터넷 노출용 TLS/auth reverse proxy는 optional deployment로 분리한다.
- Relay에는 SRT passphrase, HMAC key, Edge control certificate를 mount하지 않는다.

## 검증

- Proto/API backward compatibility와 Receiver unit test.
- Relay config, session selection, camera key, feature slicing unit test.
- GStreamer plugin self-test와 H.264 pass-through HLS segment 생성.
- synthetic SRT ingest 중 기존 external gRPC snapshot/watch가 계속 PASS.
- 같은 실행에서 HLS playlist/segment와 metadata SSE가 PASS.
- slow/no Relay 상태에서 Receiver accepted/dropped step과 readiness가 변하지 않음.
- Docker non-root/read-only/cap-drop/no-new-privileges healthcheck.
- format, Clippy `-D warnings`, workspace tests, Compose config, ShellCheck, package/vendor scan.
- 최종 이미지 SBOM과 fixable HIGH/CRITICAL vulnerability scan.

## 성능 인수 기준

- Relay는 Receiver gRPC subscription을 session당 정확히 하나만 유지한다.
- transcode element를 pipeline에 포함하지 않는다.
- Relay output queue와 HLS file 수는 모두 bounded다.
- Relay disconnect/slow consumer가 Receiver ingest thread를 block하지 않는다.
- 4 Mbps × camera 수만큼 Receiver→Relay traffic이 추가되는 것을 metrics/문서에 표시한다.
- baseline 대비 Receiver/Relay CPU, RSS, network, step loss와 HLS startup latency를 같은 fixture에서 기록한다.

## 계획 검토 기록

구현 전에 아래 네 번의 review를 순서대로 수행하고 발견사항과 계획 변경을 이 문서에 기록한다.

1. API·호환성·세션 전환 review
2. 성능·복사·backpressure review
3. 장애 격리·보안·bounded storage review
4. 테스트·관측성·운영 인수 review

### Review 1 — API·호환성·세션 전환

발견사항:

- 기존 API는 모든 query에 session UUID가 필요하지만 active session을 열거하는 RPC가 없어 container가 Edge log에 의존하게 된다.
- Receiver의 session map에는 과거 session도 남으므로 단순 UUID 정렬이나 첫 항목 선택은 잘못된 session을 고를 수 있다.
- Edge 재시작 후 Relay가 이전 server stream에 계속 대기하면 새 session을 자동 감지하지 못한다.
- 새 RPC를 추가하더라도 기존 RPC field number를 보존하면 기존 external gRPC client는 계속 동작할 수 있다.

보완 결과:

- `ListSessions`와 명시적인 `SessionStatus`를 append-only wire 변경으로 추가한다.
- 선택 조건을 authoritative + connected + 최신 capture time으로 고정한다.
- `RELAY_SESSION_ID` override를 제공하고 자동 모드에서는 subscription 중에도 discovery polling을 계속한다.
- 전환 시 active catalog와 pipeline을 원자적으로 새 session으로 교체한다.
- 통합 테스트에서 같은 Receiver에 기존 snapshot/watch client와 Relay가 동시에 연결되는 것을 필수 acceptance로 추가한다.

### Review 2 — 성능·복사·backpressure

발견사항:

- 현재 `FrameReference.encoded_image`가 `Vec<u8>`라 synchronized step clone마다 H.264 AU가 deep-copy된다.
- camera filter는 broadcast step을 받은 뒤 적용되므로 camera별 gRPC subscription 여러 개는 전체 step 복사를 반복한다.
- HLS file writer나 slow web client가 gRPC receive task를 block하면 32-step outbound queue와 256-step broadcast buffer가 차고 preview gap이 생긴다.
- 4 Mbps × 16 camera이면 Receiver→Relay가 최대 64 Mbps이고, 30초 HLS window는 영상 payload만 약 240 MB다.

보완 결과:

- Relay는 빈 camera filter로 정확히 하나의 subscription만 만들고 process 내부에서 camera fan-out한다.
- Rust protobuf의 encoded image field만 `bytes::Bytes`로 생성해 wire/Python 호환성을 유지하면서 clone을 reference-counted로 바꾼다.
- camera별 `appsrc`와 SSE broadcast를 bounded/leaky로 만들고 preview drop을 ingest block보다 우선한다.
- HLS file 수, playlist 길이, target duration, tmpfs 크기를 필수 bounded config로 둔다.
- 1/4/16 camera의 추가 bandwidth와 HLS cache 공식을 운영 문서에 넣고 실제 synthetic benchmark 결과를 별도 evidence로 기록한다.

### Review 3 — 장애 격리·보안·bounded storage

발견사항:

- HLS pass-through는 H.264 SEI도 보존할 수 있으므로 URL 접근자는 화면뿐 아니라 in-band protobuf metadata를 추출할 가능성이 있다.
- 임의 camera ID를 filesystem path로 쓰면 traversal/충돌 위험이 있고, 잘못된 output root 정리는 host data를 삭제할 수 있다.
- session/epoch 전환 때 old playlist가 남으면 client가 stale stream을 정상 stream으로 오인할 수 있다.
- Relay에 Receiver용 SRT/control secret은 필요하지 않으며 mount하면 권한 범위만 불필요하게 넓어진다.

보완 결과:

- camera ID는 hex safe key로 변환하고 catalog에서 원본 ID와 mapping한다. session path는 canonical UUID만 허용한다.
- Relay 전용 tmpfs의 고정 `live` 하위만 관리하고 root/empty path config를 거부한다.
- session 전환은 old pipeline을 Null로 내리고 active catalog를 교체한 뒤 managed live tree를 갱신한다.
- HLS/SSE는 기본 사내 평문 endpoint로 제공하되 SEI까지 접근 가능한 데이터 경계임을 매뉴얼에 경고한다. 외부 인터넷은 reverse proxy TLS/auth를 선택 적용한다.
- container에는 secret mount 없이 non-root/read-only/cap-drop/no-new-privileges와 size-limited tmpfs만 부여한다.
- Relay health/readiness는 Relay만 표현하며 Compose dependency를 Receiver 반대 방향으로 만들지 않는다.

### Review 4 — 테스트·관측성·운영 인수

발견사항:

- playlist 존재만 검사하면 segment가 깨졌거나 metadata correlation이 틀려도 PASS가 될 수 있다.
- Relay를 켠 상태의 HLS만 검사하면 기존 외부 gRPC가 계속 접근 가능한지 증명하지 못한다.
- CPU 비율은 host 성능에 종속되므로 synthetic 한 번의 숫자를 일반화하면 안 되지만, bandwidth/cache/queue 증가는 결정적으로 계산할 수 있다.
- subscriber lag를 server stream이 건너뛰므로 Relay가 AU ordinal/epoch gap을 자체 관측하지 않으면 화면 누락이 조용히 발생한다.

보완 결과:

- 통합 test는 같은 ingest에서 `ListSessions`, 기존 snapshot/watch, HLS playlist, TS segment demux/H.264 parse, SSE frame metadata와 ordinal correlation을 모두 검사한다.
- Relay metrics에 received step/AU/byte, session switch, gRPC reconnect, ordinal gap, pipeline restart, SSE lag를 추가한다.
- Docker test는 non-root/read-only/cap-drop/no-new-privileges와 size-limited tmpfs에서 실행한다.
- 성능 evidence는 relay-off 구조적 baseline과 relay-on CPU/RSS snapshot을 구분하고 1/4/16 camera bandwidth, bounded queue/history, HLS window 공식을 기록한다.
- CI/package/SBOM/Trivy 대상에 Relay image와 통합 script를 추가한다.
- 매뉴얼에는 HLS URL 발견, hls.js/native HLS 선택, SSE overlay, gRPC 병행 접근, session 전환, 용량 산정과 장애 대응을 포함한다.
- HLS가 과거 segment를 재생하는 동안 SSE가 live event만 주는 불일치를 막기 위해 camera별 bounded metadata history를 먼저 replay한 뒤 live event로 전환하고, SSE ID를 `epoch:ordinal`로 둔다.

## 최종 검토 후 구현 결정

네 차례 검토 결과 외부 native gRPC와 URL 영상/SSE metadata는 동시에 제공 가능하며 wire/port 충돌이 없다. 구현은 기존 Receiver를 canonical source로 유지하고 별도 Relay가 optional consumer가 되는 방식으로 진행한다. 필수 보완사항은 append-only session discovery, reference-counted encoded bytes, 단일 subscription, bounded HLS/SSE, safe path, session/epoch recovery 및 동시-access 통합 검증이다.

## 구현 및 인수 결과

- `ListSessions`를 append-only로 추가했고 기존 gRPC snapshot/watch와 새 Relay가 한 Receiver에 동시에 연결됐다.
- H.264 `Bytes` payload를 한 subscription으로 받아 `h264parse → mpegtsmux → hlssink`만 거치며 transcode element는 없다.
- HLS file/appsrc/SSE/history/tmpfs/config은 모두 상한이 있고 slow preview client는 ingest를 block하지 않는다.
- multi-camera keyframe step도 명시적 한도 안에서 받도록 gRPC decode message를 기본 64 MiB, 허용 4–256 MiB로 bounded 설정했다.
- synthetic 48 AU에서 native gRPC encoded frame, HLS playlist/segment TS demux, SSE history ID와 frame CRC/feature/ordinal correlation이 함께 PASS했다.
- 작은 4 AU/s fixture의 인접 최종 build 세 실행에서 Relay 0.51–0.60% CPU, 11.98–19.86 MiB RSS가 관측됐으며 production 일반화 대신 bitrate/cache/history 공식을 별도 성능 문서에 기록했다.
