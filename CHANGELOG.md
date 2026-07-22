# Changelog

## 2.1

- Receiver의 camera별 SRT port/slot 규칙과 canonical stream ID/HMAC 계약 명문화
- transport hint와 `SessionManifestV1.anchor_camera_id` authoritative 판정 추가
- Receiver bootstrap state machine, preview-only pre-manifest 동작, late join 추가
- decoder 전 H.264/H.265 SEI 추출과 Receiver metadata API 추가
- exact-byte context CRC packet, manifest chunking/compression/CRC 추가
- session 변경 시 SRT reconnect 및 raw TS replay용 connection envelope 추가
- physical camera의 multi-node logical grouping과 timestamp calibration 정책 추가
- metadata bitrate budget, USB/encoder admission control, disk/retention/spool 정책 추가
- Receiver ingest와 dataset-builder 장애 격리
- Dockerfile, env, camera policy, protocol constants, 운영/검증 스크립트 보강
- 네 차례 독립 검토 보고서 및 package checksum 추가
- stable camera identity collision, persistent slot tombstone 및 manual reclaim 정책 추가
- Dataset Builder exact LeRobot pin, cadence gate, finalize/load-scan/atomic export transaction 추가
- Receiver API에 canonical full anchor context와 exact CRC packet/PTS/AU ordinal 노출 추가
- four-pass machine-readable validation evidence 추가
- 전체 Phase 0–7 구현, 공식 RB-Y1 SDK 0.10.0 및 exact LeRobot 0.6.0 런타임 검증
- Edge production healthcheck에 GStreamer plugin 및 24-AU SEI/MPEG-TS/decode conformance 통합
- raw TS에서 synchronized step 24개를 두 번 재생성하는 bit-for-bit deterministic replay 추가
- hardened Docker runtime, disk-pressure readiness fault, CycloneDX SBOM 및 fixable HIGH/CRITICAL 0건 검증
- Chapter 29 전체 release audit와 machine-readable acceptance evidence 추가
- env/secret/toolchain/runtime fixture 재생성, production 배포, 외부 영상·metadata 접근 매뉴얼 추가
- Receiver gRPC synchronized H.264/metadata client와 session/camera acceptance 운영 로그 추가
- 다른 로봇용 Adapter를 AI agent가 구현할 수 있는 wire/clock/feature/command/Docker/test 계약 가이드 추가
- VLC URL endpoint 지원 여부와 ingest/output 구분, 사내망 평문 접속, frame/AU별 metadata 및 manifest feature slice 추출 절차 명확화
- Receiver native gRPC와 동시에 사용할 수 있는 별도 non-root Web Relay container 및 `ListSessions` discovery 추가
- H.264 decode/encode 없는 HLS pass-through/remux, stream catalog, bounded-history frame metadata SSE 추가
- VLC/hls.js URL 재생, PTS/epoch/ordinal overlay correlation, 1/4/16 camera 성능·용량 분석과 통합검증 추가

## 2.0

- Generic Edge Core와 hardware Adapter 계층 분리
- 환경변수 기반 anchor camera
- 모든 camera timestamp, anchor-only state/action metadata 정책
