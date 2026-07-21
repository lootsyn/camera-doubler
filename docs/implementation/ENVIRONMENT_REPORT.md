# 검증 환경 보고서

검증일: 2026-07-22, 시간대: Asia/Seoul.

| 항목 | 설치/확인 값 | 판정 |
|---|---|---|
| Host | Windows + WSL2 Ubuntu 24.04 | PASS |
| Rust | 1.85.1 workspace-local toolchain | PASS |
| GStreamer | 1.24.2 in WSL, full H.264/MPEG-TS/SRT plugins | PASS |
| Docker Engine | 29.1.3 | PASS |
| Docker Compose | 2.40.3 | PASS |
| Protobuf compiler | Ubuntu package + vendored Rust protoc | PASS |
| RB-Y1 SDK | official Python `rby1-sdk==0.10.0` | PASS |
| LeRobot | exact `lerobot==0.6.0` dataset extra | PASS |
| Camera input | `videotestsrc` encoded H.264 synthetic camera | PASS |
| Physical USB camera | WSL에 전달되지 않음 | BLOCKED_HARDWARE |
| Physical RB-Y1 | 장치 주소/로봇 없음 | BLOCKED_HARDWARE |

WSL Microsoft kernel에는 host용 v4l2loopback module을 안전하게 적재할 수 없으므로, codec·SEI·mux/demux·predecode extraction·decoder 검증은 동일 GStreamer 플러그인을 사용하는 synthetic camera round-trip으로 대체한다. 이는 카메라 영상 경로의 환경 차단이 아니며, 실제 USB identity/hotplug와 physical v4l2 sink만 hardware gate로 남긴다.

Docker daemon은 WSL 안에서 직접 실행하며 최종 이미지 5개를 빌드했다. Edge/Receiver는 Debian 12.15 runtime, RB-Y1은 Ubuntu 24.04 runtime이며 모든 배포 컨테이너 검증은 non-root, read-only root filesystem, `cap_drop: ALL`, `no-new-privileges` 조건에서 수행했다. Receiver의 강제 저용량 임계값은 `/readyz` HTTP 503과 `disk_pressure=low`를 반환했다.

작업 중 D: 용량 고갈을 방지하기 위해 이 작업 전용 Rust toolchain, Cargo target/registry, Python venv, Trivy cache와 WSL VHDX를 `E:\codex-task-cache-camera-doubler-20260722`로 보존했다. 기존 workspace 경로는 junction으로 유지했으며 WSL/Docker/Rust/Python 재실행으로 정상 동작을 확인했다. 환경 설치 실패나 누락으로 남은 `BLOCKED_ENVIRONMENT`는 0건이다.
