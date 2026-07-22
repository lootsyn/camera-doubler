# 요구사항 추적표

`ROBOT_MULTICAMERA_BACKEND_DESIGN.md` 29장의 수용 기준을 구현과 자동 증거에 연결한다.

| ID | 요구사항 | 구현 | 자동 증거 |
|---|---|---|---|
| F01 | 지원 logical camera 자동 등록 | `camera-discovery` Linux discovery/grouping | camera grouping tests |
| F02 | 활성 카메라별 virtual camera | `virtual-camera`, Edge reconciliation | manager mock tests, Compose device policy |
| F03 | stream exclude는 UI 유지 | `CameraPolicy`, `UiOnlyHandle` | exclusion/anchor tests |
| F04 | anchor 하나로 해석 | selectors와 policy validation | selector tests |
| F05 | anchor와 exclude/disable 충돌 오류 | `CameraPolicy::evaluate` | conflict test |
| F06 | 모든 송출 AU exactly-one timestamp | `metadata-codec`, Edge worker | codec tests, synthetic round-trip |
| F07 | anchor-only CRC context | `ContextAssembler`, `AnchorMetadataProvider` | CRC/context tests |
| F08 | secondary는 timestamp-only | strict secondary inspector | semantic rejection tests |
| F09 | Core 수정 없이 Adapter 추가 | generated Adapter API와 UDS client | template/gripper fixture, vendor-boundary script |
| F10 | anchor 시각 state/action resampling | ClockMapper와 feature rings | interpolation/context tests |
| F11 | 카메라별 SRT base+slot | stream identity와 Edge output | port-slot tests |
| F12 | Receiver provisional identity | `ReceiverRegistry::accept` | lifecycle tests |
| F13 | manifest anchor authoritative | `validate_manifest` | conflict/bootstrap tests |
| F14 | role/manifest/slot/epoch/port 교차 검증 | Receiver synchronizer | manifest conflict test |
| F15 | decoder 전 SEI 추출 | Receiver `appsink` before decoder tee | Linux image compile, synthetic pipeline |
| F16 | late join/bounded pending | manifest reassembler와 bounded ingest | timeout/capacity tests |
| F17 | anchor 기준 synchronized step | `ReceiverRuntime`, `StepSynchronizer` | runtime step-channel test |
| F18 | Receiver metadata API | tonic `ReceiverMetadataService` | compile and runtime state tests |
| F19 | raw replay identity/index/hash | `receiver::replay` | exact segment hash test, 24-AU deterministic synchronized-step replay |
| F20 | AU correlation failure invalid/drop | context/step synchronizers | missing-camera/context tests |
| F21 | 실제 anchor cadence, 무합성 | dataset transaction policy | cadence/fixed-grid tests |
| F22 | stream 장애와 UI branch 격리 | leaky tee queues/UI-only handle | pipeline plan and fault tests |
| F23 | Compose 실행 | hardened Edge/Receiver/Adapter Compose | `docker compose config`, image builds |
| F24 | native gRPC와 URL 영상 동시 접근 | `ListSessions`, `web-relay` HLS/SSE | synthetic gRPC/HLS/TS/SSE integration |
| N01 | Generic crate에 vendor SDK 없음 | RB-Y1 Python mapping 단일 경계 | `verify-vendor-boundary.py` |
| N02 | Adapter/stream failure 격리 | 독립 services, per-camera pipelines | queue/fault tests |
| N03 | 모든 resource bounded | queue/ring/reassembly/stream/history/spool caps | unit tests and env validation |
| N04 | restart/hotplug stable mapping/collision fail-closed | atomic `CameraMap` | persistence/collision tests |
| N05 | tombstone 자동 재사용 금지 | manual mapping lifecycle | slot test |
| N06 | secrets 비포함 | Compose secrets와 package validator | repository scan/validator |
| N07 | HMAC와 port-slot 강제 | `StreamIdentity`, Receiver registry | canonical vectors/tamper tests |
| N08 | metadata DoS 제한 | codec constants/reassembler | size/CRC/ratio/timeout tests |
| N09 | control lease/safety | mTLS gateway와 `ControlGateway` | exclusive lease/unsafe command tests |
| N10 | disk pressure 명시화 | readiness와 retention report | disk/retention tests, hardened container `/readyz` 503 fault injection |
| N11 | GStreamer/SEI self-test | production healthcheck의 plugin 검사와 synthetic round-trip | hardened Edge image healthcheck 24-AU runtime evidence |
| N12 | revision 재현성 | compiled constants, JCS schema hash | constants/schema tests |
| N13 | exact LeRobot transaction | Python Dataset Builder | version/transaction/export loader tests |
| N14 | irregular cadence fail-closed | cadence validator | pytest cadence tests |
| N15 | Relay 장애와 slow viewer 격리 | optional service, bounded/leaky HLS/SSE/history | hardened container integration, gap/lag metrics |

각 항목의 최종 PASS/BLOCKED 판정은 `docs/audit/ACCEPTANCE_EVIDENCE.csv`에 있다. 물리 장치에서만 확인 가능한 RB-Y1 실제 명령, USB camera unplug/replug, kernel v4l2loopback 장기 안정성은 `docs/audit/OPEN_GATES.md`에서 별도의 `BLOCKED_HARDWARE` gate로 관리한다.
