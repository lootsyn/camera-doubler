# 열린 생산 하드웨어 Gate

환경 도구 누락으로 남은 gate는 없다. 아래 항목은 로컬 소프트웨어나 synthetic source로 물리 사실을 증명할 수 없어 `BLOCKED_HARDWARE`로 유지한다. 이 gate는 구현 미완료를 뜻하지 않지만 실제 생산 배포 승인을 막는다.

| Gate | 관련 수용 기준 | 필요한 장치/시험 | 종료 조건 |
|---|---|---|---|
| BH-01 USB discovery/hotplug | F01, N04 | 배포 대상 host의 지원 USB camera 여러 대 | logical multi-node grouping, 자동 등록, unplug/replug 후 동일 stable ID/slot, 동일 identity collision fail-closed 로그 |
| BH-02 v4l2loopback continuity | F02, F22 | 배포 kernel의 v4l2loopback과 실제 UI consumer | 카메라별 `/dev/video*` 생성, UI open/decode, Receiver/network 중단 중 output 지속, 재시작 후 slot 유지, 장시간 soak |
| BH-03 RB-Y1 physical motion | Chapter 29 외 mandatory vendor gate | 실제 RB-Y1, 안전 구역, E-stop, vendor network | SDK 0.10.0 실제 state clock mapping, command/effective target, lease expiry stop, mode/range 제한, 재시작 recovery를 안전 담당자 입회로 통과 |
| BH-04 capacity/admission | production sizing gate | 목표 최대 카메라 수, 실제 USB controller와 encoder | 목표 해상도/FPS/bitrate에서 USB·encoder·CPU·memory·virtual-output latency 예산과 admission rejection 기준 측정 |
| BH-05 timestamp calibration | production calibration gate | 모든 실제 카메라가 보는 공통 visual event | camera별 timestamp event 의미와 systematic offset/skew p95/max를 측정하고 calibration revision을 manifest에 기록 |

H.265는 현재 release profile에서 활성화하지 않았다. 활성화하려면 해당 parser/encoder/decoder 경로에 H.264와 동일한 startup conformance와 replay 증거를 먼저 추가해야 한다.

## 재현 명령 골격

```bash
sudo ./scripts/prepare-host.sh
./scripts/verify-environment.sh
docker compose --env-file .env.edge --profile rby1 -f compose.edge.yaml up -d --build
docker compose --env-file .env.receiver -f compose.receiver.yaml up -d --build
./scripts/run-fault-tests.sh
```

실제 RB-Y1 motion 시험에서는 `RBY1_USE_MOCK=0`을 사용하되, 주소/인증정보를 repository나 stream ID에 기록하지 않는다. USB/hotplug 및 motion 결과에는 장치 목록, manifest revision, 카메라 mapping snapshot, metrics, 시간 동기 상태와 운영자 승인 기록을 첨부한다.
