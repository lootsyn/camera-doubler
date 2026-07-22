# 운영 매뉴얼 인덱스

이 디렉터리는 repository에 포함되지 않는 local 설정·secret·runtime 산출물을 다시 만드는 방법부터 실제 송출 조회와 새 로봇 Adapter 작성까지의 운영 절차를 모은다.

| 문서 | 대상 | 목적 |
|---|---|---|
| `SETUP_AND_SECRETS.md` | 설치/보안 담당자 | env, config, secret, toolchain, generated code, runtime fixture 생성과 보관 |
| `DEPLOYMENT_RUNBOOK.md` | Edge/Receiver 운영자 | 실제 카메라·Adapter·Receiver의 시작, 확인, 종료, 네트워크 포트 |
| `VIDEO_AND_METADATA_ACCESS.md` | UI/데이터 소비자 | 외부에서 동기화 H.264 화면을 보고 manifest/context/quality를 조회 |
| `ADAPTER_AUTHORING_FOR_AI_AGENTS.md` | AI/개발 에이전트 | 다른 로봇·그리퍼·base·tool용 Adapter를 Core 변경 없이 구현 |
| `PRODUCTION_CHECKLIST.md` | 배포 승인자 | 보안, capacity, hardware acceptance, 백업/복구 체크리스트 |

아키텍처 원문은 `ROBOT_MULTICAMERA_BACKEND_DESIGN.md`, 일상 장애 대응은 `docs/OPERATIONS.md`, 최종 판정은 `docs/audit/FINAL_RELEASE_AUDIT.md`를 함께 사용한다.

## 가장 짧은 시작 경로

```bash
./scripts/bootstrap-example-config.sh
./scripts/generate-dev-secrets.sh
sudo ./scripts/prepare-host.sh
./scripts/verify-environment.sh
docker compose -f compose.receiver.yaml up -d --build --wait
docker compose --profile rby1 -f compose.edge.yaml up -d --build --wait
```

개발 인증서는 production에 사용하지 않는다. 실제 로봇 명령을 허용하기 전에 `PRODUCTION_CHECKLIST.md`의 hardware gate를 닫아야 한다.
