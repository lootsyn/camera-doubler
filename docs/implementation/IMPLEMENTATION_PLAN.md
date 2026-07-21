# 전체 구현 계획

기준일은 2026-07-22이며 구현 범위는 `AI_AGENT_CODEGEN_PROMPTS.md`의 Phase 0–7 전체다.

## 고정 기준 inventory

- `ROBOT_MULTICAMERA_BACKEND_DESIGN.md`: 아키텍처, wire contract, 수용 기준의 최상위 기준
- `REVIEW_REPORT.md`: 남은 runtime gate와 보안 검토 기준
- `docs/TRANSPORT_BOOTSTRAP.md`: SRT port/slot, 인증 stream ID, authoritative manifest 절차
- `docs/PROTOCOL_CONSTANTS.md`와 `config/protocol_constants.toml`: SEI UUID, CRC32C, HMAC, 크기 제한
- `proto/*.proto`: Adapter, Control, frame metadata, Receiver API wire schema
- `compose.*.yaml`, `.env.*.example`: 배포 및 bounded-resource 계약

고정 UUID는 timestamp `4a1191e6-9578-53b3-92a7-04c049fe0d5b`, anchor context `62ef08bb-2eb4-59fb-b83f-f8f874a80043`, manifest `791a8fc5-d0c3-5abf-81da-abf7f0373194`다. CRC는 CRC32C이며 SRT identity는 canonical `rmc1` 문자열의 HMAC-SHA256 앞 128 bit를 base64url(no padding)로 표현한다.

## 단계와 종료 조건

| Phase | 구현 범위 | 종료 증거 |
|---|---|---|
| 0 | workspace, protocol generation, config/observability 기초 | 전체 crate compile, constants drift test |
| 1 | discovery, stable slot, v4l2loopback, capture/UI/encode/SRT, timestamp SEI, predecode ingest | unit test와 실제 synthetic GStreamer MPEG-TS round-trip |
| 2 | Adapter generated SDK/client/registry, Clock Mapper, embodiment compiler | descriptor/clock/schema 및 vendor-boundary test |
| 3 | anchor policy, feature ring/resampling, context packet, AU hold, manifest 반복/재조립 | anchor-only/CRC/timeout/late-join test |
| 4 | 공식 RB-Y1 SDK 0.10.0 reference Adapter | SDK import/version 및 synthetic semantic self-test; 실제 로봇은 별도 hardware gate |
| 5 | template와 gripper fixture, composite vectors | deterministic composite layout 및 partial-failure test |
| 6 | mTLS Control Gateway, lease/safety, Receiver metadata API, synchronized steps, session/replay, LeRobot export | gRPC glue test, replay hash test, exact LeRobot loader full scan |
| 7 | least privilege Compose, metrics/readiness, retention, fault scripts, CI, 운영/감사 문서 | Docker builds/self-tests, compose config, lint/test, audit artifacts |

각 단계는 production callback에서 blocking I/O를 하지 않고 queue/ring/reassembly/stream/history를 명시적으로 제한한다. 실제 물리 RB-Y1 동작과 실제 USB 카메라 hotplug만 `BLOCKED_HARDWARE`로 분류하며, SDK·Docker·codec·가상 입력은 로컬에서 실행해 검증한다.
