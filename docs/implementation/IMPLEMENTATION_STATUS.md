# 구현 상태

## Phase 상태

| Phase | 상태 | 핵심 결과 |
|---|---|---|
| 0 | COMPLETE | Rust workspace, exact dependencies, generated protobuf, constants drift rejection |
| 1 | COMPLETE | discovery/stable slots, virtual output manager, independent GStreamer branches, SEI/SRT Receiver |
| 2 | COMPLETE | UDS Adapter client, descriptor registry, robust clock mapper, deterministic embodiment schema |
| 3 | COMPLETE | anchor policy, resampling, context CRC, AU correlation, manifest schedule/reassembly |
| 4 | COMPLETE_SDK | official `rby1-sdk==0.10.0` mapping and synthetic semantic test; physical motion remains hardware gate |
| 5 | COMPLETE | generic template, gripper fixture, composite vector compiler |
| 6 | COMPLETE | mTLS control service, leases, Adapter routing, Receiver API/runtime, session/replay, LeRobot builder |
| 7 | COMPLETE | Docker/Compose, least privilege, optional H.264 Web Relay, metrics, retention, fault/CI, SBOM/vulnerability scan, release audit |

## Contract 변경

기존 protobuf field number와 protocol constants는 변경하지 않았다. Receiver API에는 append-only `ListSessions` RPC를 추가했고 기존 RPC와 field number는 보존했다. Dataset dependency는 `requirements.lock`으로 완전히 고정하고 RB-Y1 SDK는 해당 Adapter image와 `adapters/rby1/mapping.py`에만 존재한다.

## 알려진 gate

- 실제 RB-Y1 주소와 물리 로봇이 없어 실제 actuator command/motion은 `BLOCKED_HARDWARE`다.
- WSL에 USB 카메라와 loadable `v4l2loopback` kernel module이 없어 물리 hotplug 및 `/dev/video*` 출력은 `BLOCKED_HARDWARE`다. 동일 codec/metadata 경로는 `videotestsrc` synthetic camera로 실행한다.
- 환경 도구 부재를 이유로 남겨 둔 `BLOCKED_ENVIRONMENT` 항목은 없다. Docker, Compose, GStreamer, protoc, SDK와 LeRobot은 설치해 검증한다.

## 최종 검증 요약

- `cargo fmt --all --check`, workspace Clippy `-D warnings`, Rust tests 48개를 통과했다.
- 배포 Edge 이미지의 healthcheck가 실제 GStreamer plugin 검사와 24 AU H.264/SEI/MPEG-TS round-trip을 실행한다.
- raw TS archive에서 24개 synchronized step을 두 번 재구성했으며 protobuf bytes가 bit-for-bit 동일했다.
- 공식 `rby1-sdk==0.10.0`을 설치한 이미지와 로컬 venv에서 semantic/UDS contract를 실행했다.
- exact `lerobot==0.6.0` 이미지에서 transactional export 및 loader scan pytest 5개를 통과했다.
- Web Relay를 포함한 최종 이미지 6개 모두 CycloneDX SBOM을 생성했고 Trivy 기준 수정 가능한 HIGH/CRITICAL은 0건이다.
- 외부 gRPC와 HLS URL, TS demux, bounded-history SSE frame metadata를 같은 synthetic session에서 동시에 검증한다.
- Chapter 29 판정은 PASS 33, BLOCKED_HARDWARE 4, FAIL/BLOCKED_ENVIRONMENT/NOT_IMPLEMENTED 0이다. 상세 증거는 `docs/audit/FINAL_RELEASE_AUDIT.md`에 있다.
