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

## 2.0

- Generic Edge Core와 hardware Adapter 계층 분리
- 환경변수 기반 anchor camera
- 모든 camera timestamp, anchor-only state/action metadata 정책
