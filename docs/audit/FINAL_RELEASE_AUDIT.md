# 최종 릴리스 감사

- 감사일: 2026-07-22 (Asia/Seoul)
- 기준: `ROBOT_MULTICAMERA_BACKEND_DESIGN.md` Chapter 29, `REVIEW_REPORT.md` mandatory gates, 전체 source/test/Docker/Compose/package
- 결론: **소프트웨어·합성 입력 release candidate 통과, 물리 생산 인수는 하드웨어 gate 해소 전 보류**

## 판정 요약

| 판정 | 개수 |
|---|---:|
| PASS | 33 |
| BLOCKED_HARDWARE | 4 |
| FAIL | 0 |
| BLOCKED_ENVIRONMENT | 0 |
| NOT_IMPLEMENTED | 0 |

Chapter 29의 37개 항목을 모두 분류했다. 물리 USB 카메라 자동 등록/hotplug와 실제 kernel v4l2loopback 출력에만 하드웨어 차단을 적용했다. Docker, Compose, GStreamer, protoc, 공식 RB-Y1 SDK와 LeRobot은 설치해 실제 실행했으므로 환경 차단으로 남기지 않았다. 항목별 판정은 `ACCEPTANCE_EVIDENCE.csv`, machine-readable 결과는 `validation/final_release_audit.json`에 있다.

## 핵심 실행 증거

| 영역 | 실제 결과 | 주요 증거 |
|---|---|---|
| Rust 전체 | format PASS, Clippy `-D warnings` PASS, 46 tests PASS | workspace source/tests, `validation/RELEASE_CHECKS.txt` |
| Codec/SEI | 배포 Edge healthcheck에서 24 AU H.264, secondary timestamp-only, anchor CRC context/manifest, MPEG-TS mux/demux, predecode extraction, decoder PASS | `crates/edge-core/src/main.rs`, `crates/edge-core/src/bin/synthetic_roundtrip.rs` |
| Replay | raw TS 24 AU에서 synchronized step 24개를 두 번 재구성, protobuf bit-for-bit 동일 | `crates/receiver/src/bin/replay_verify.rs`, `validation/runtime/archive-conformance/` |
| SRT | 서명된 canonical stream ID와 암호화된 SRT로 동일 raw TS를 두 번 연결, Receiver readiness 유지 | `scripts/run-srt-reconnect-test.sh` |
| Receiver fault | normal readiness PASS; 강제 disk pressure는 HTTP 503과 low 상태 | `crates/receiver/src/main.rs`, `validation/RELEASE_CHECKS.txt` |
| RB-Y1 | official `rby1-sdk==0.10.0`, semantic mapping 및 UDS descriptor/state/command/health PASS | `adapters/rby1/`, `scripts/adapter-uds-smoke.py` |
| LeRobot | exact `lerobot==0.6.0`, transactional export/cadence/loader scan pytest 5 PASS | `python/dataset_builder/` |
| Deployment | 5개 이미지 build, non-root/read-only/cap-drop/no-new-privileges runtime PASS; Compose config PASS | `docker/`, `compose.edge.yaml`, `compose.receiver.yaml` |
| Supply chain | 5개 CycloneDX SBOM 생성; Trivy fixable HIGH/CRITICAL 0 | `validation/security/` |
| Package | 실제 ZIP 내부를 열어 toolchain/target/venv/runtime secret/local env 제외를 검증 | `scripts/package.sh`, `scripts/validate-package.py`, `SHA256SUMS` |

결정론적 replay의 최종 fingerprint는 metadata SHA-256 `901bd0be6db8273e7cb79beb571899f093ea3d58e408a7e041b04629ccc48268`, synchronized-step SHA-256 `8c8b9d9cca045f7ba9a0570d4685aad9ddc75d1b63a5474241f6537b9405d46b`다.

## 감사 범위별 결론

1. Generic Core/Receiver에는 vendor SDK import/dependency가 없고 RB-Y1 import는 Adapter 경계에만 있다.
2. stable ID, selector precedence, collision fail-closed, tombstone 비재사용은 assertion이 있는 테스트를 통과했다. 실제 USB hotplug만 hardware gate다.
3. capture/UI/network는 독립 bounded queue를 사용한다. 실제 encrypted SRT reconnect 중 Receiver가 유지됐다.
4. process monotonic timebase, offset/drift 추정, outlier와 clock jump reset이 테스트됐다.
5. secondary timestamp-only, anchor-only context, exactly-one timestamp와 CRC는 실제 encoded AU round-trip으로 검증했다.
6. SRT identity는 canonical HMAC/port-slot/epoch를 먼저 검증하고 manifest anchor/camera catalog를 최종 기준으로 사용한다.
7. Receiver는 decoder tee 이전 appsink에서 SEI를 추출한다. raw TS replay는 hash/envelope/index 검증 후 synchronized step을 결정론적으로 복원했다.
8. queue/ring/pending/reassembly/history/spool에 명시적 상한이 있고 크기/CRC/ratio/timeout 검사가 fail closed한다.
9. control은 mTLS 경계, exclusive TTL lease, UUID replay 방지, shape/type/finite/range 검사를 거친다.
10. Dataset Builder는 exact version 확인 후 temp/finalize/full load scan/checksum/provenance/atomic commit 순서로 동작한다.
11. 이미지 base는 digest pin이며 runtime은 least privilege다. secret은 mount로만 전달되고 package에서 제외된다.
12. mock-only 결과를 물리 PASS로 올리지 않았다. 실제 USB/v4l2loopback/RB-Y1 motion은 `OPEN_GATES.md`에 남겼다.

## 소스 감사

- Rust production source에서 `unsafe` block과 unbounded channel을 찾지 못했다.
- blocking device/network I/O는 GStreamer/worker/service 경계에 있고 appsink callback은 bounded latest queue로 복사한 뒤 반환한다.
- package validator가 Generic crate의 `rby1`/vendor leakage, local env, generated secrets, toolchain/venv/target 포함을 거부한다.
- Compose의 배포 서비스는 privileged mode를 사용하지 않는다. 초기화 서비스만 명시적 CHOWN/FOWNER capability를 가진 뒤 종료한다.
- panic/`expect` 검색 결과 production data path의 외부 입력을 panic으로 처리하는 경로는 없고, 발견된 항목은 테스트 또는 build-time validated invariant다.

## 감사 중 발견·수정한 항목

| 심각도 | 문제 | 재현/영향 | 수정 |
|---|---|---|---|
| HIGH | Edge readiness가 실제 GStreamer/SEI round-trip을 실행하지 않음 | plugin/codec drift가 readiness PASS로 숨을 수 있음 | production healthcheck/serve에 plugin 검사와 companion 24-AU conformance 실행 추가 |
| HIGH | replay가 segment/hash 검증만 하고 synchronized step 결정론성을 증명하지 않음 | replay 결과가 원본과 달라도 탐지 범위가 부족함 | manifest/context 재조립, step 동기화 2회, protobuf byte equality와 SHA-256 추가 |
| MEDIUM | synthetic manifest의 camera/feature catalog가 비어 있었음 | 강화된 replay가 manifest validation에서 정확히 실패 | 실제 descriptor/schema/feature slice를 넣은 유효 manifest로 교정 |
| MEDIUM | RB-Y1 Compose healthcheck가 이미지에 없는 executable을 참조 | Adapter는 실행돼도 Compose health가 영구 실패 | 실제 Python module entrypoint healthcheck로 교정하고 RB-Y1/gripper Compose가 모두 healthy임을 확인 |
| MEDIUM | 보안 스캔 시 동시 DB 갱신이 task cache를 손상 | scan 재현성 저하, source/image 영향 없음 | 독립 final cache에서 DB를 한 번 갱신하고 Edge/Receiver를 순차 재검사 |

최종 상태에는 미해결 FAIL 또는 NOT_IMPLEMENTED가 없다. 다만 실제 물리 생산 배포는 `OPEN_GATES.md`의 RB-Y1 motion, USB/hotplug/v4l2loopback, 최대 카메라 수용량과 시각 교정 완료 전까지 승인하지 않는다.
