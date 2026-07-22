# 다른 로봇용 Adapter 작성 가이드

이 문서는 AI coding agent가 Generic Edge Core/Receiver를 수정하지 않고 새 robot, arm, mobile base, gripper, tool 또는 sensor Adapter를 구현하기 위한 완결된 계약이다.

## 1. 시작 전에 반드시 확보할 입력

AI agent는 다음 사실이 없으면 추측하지 않고 해당 항목을 hardware/vendor gate로 기록한다.

1. vendor SDK package/repository와 정확한 지원 version, license/redistribution 조건.
2. 연결 방식, authentication, reconnect API, thread-safety와 blocking 여부.
3. robot model별 DOF, canonical joint/axis 순서, unit, frame convention.
4. state timestamp의 clock domain과 의미. 장치 측정 시각인지 SDK 수신 시각인지 구분.
5. 실제 state, effective controller target, requested command의 SDK field 차이.
6. 지원 command mode, shape, hard/soft range, velocity/acceleration/force limit.
7. enable/servo/mode 전환, E-stop, protective stop, fault clear의 안전 절차.
8. 실제 hardware에서 수행 가능한 read-only test와 motion test 승인 범위.

실제 command 의미가 불명확하면 `effective action`을 requested command로 위조하지 않는다.

## 2. 변경 경계

새 vendor slug를 `<vendor>`라고 할 때 기본 변경 범위:

```text
adapters/<vendor>/__init__.py
adapters/<vendor>/mapping.py       # vendor SDK import를 허용하는 유일한 module
adapters/<vendor>/app.py
adapters/<vendor>/docker/Dockerfile
.env.adapter-<vendor>.example
compose.edge.yaml                  # 독립 profile/service 추가
config/embodiment.example.yaml     # 예시 또는 별도 production config
adapters/<vendor>/tests/           # semantic/shape/safety tests
```

`crates/edge-core`, `crates/receiver`, Generic Cargo dependency, common protobuf field number를 vendor 편의를 위해 수정하지 않는다. 새 범용 기능이 진짜 필요하면 별도 architecture change로 분리한다.

참조 구현:

- 최소 backend: `adapters/template/app.py`
- 실제 SDK isolation: `adapters/rby1/mapping.py`
- UDS service runtime: `python/adapter_runtime/service.py`
- wire contract: `proto/adapter_api.proto`
- 다중 component 예: `adapters/gripper_fixture/app.py`

## 3. HardwareAdapter RPC 계약

| RPC | Adapter 책임 | fail-closed 규칙 |
|---|---|---|
| `GetDescriptor` | 고정 API/version/clock/device/feature/command schema 반환 | runtime shape와 descriptor가 다르면 시작 실패 |
| `StreamSamples` | 1..1000 Hz 요청 범위에서 timestamped feature blocks stream | missing/non-finite는 valid=false; 임의 0으로 위조 금지 |
| `CommandStream` | mode/shape/finite/range/machine state를 검증하고 feedback 반환 | unsafe command는 REJECTED; exception text는 512 bytes 이하 |
| `ProbeClock` | nonce와 동일 clock ID의 sample time 반환 | clock을 지원하지 않으면 descriptor에 정확히 선언 |
| `GetHealth` | liveness/readiness/status/warnings 반환 | SDK disconnected 또는 unsafe state는 ready=false |

Core의 lease/mTLS/schema 검증과 별개로 Adapter도 vendor 단위 안전 검증을 반복한다. Adapter는 직접 외부 TCP service를 열지 않고 shared `adapter-sockets` volume의 Unix Domain Socket만 사용한다.

## 4. Descriptor 설계

### Adapter와 clock

- `api_version=1`.
- `adapter_instance_id`는 env/config와 동일하고 session 안에서 안정적이다.
- `vendor_sdk_version`은 실제 설치 package에서 읽고 지원 exact version과 다르면 시작을 거부한다.
- `source_clock_id`는 모든 sample과 feedback에서 동일하게 사용한다.
- `SOURCE_MONOTONIC`: 장치/SDK monotonic clock이며 Edge probe mapping이 필요.
- `TAI_UTC`: SDK가 명확히 절대 시각을 보장할 때만 사용.
- `EDGE_MONOTONIC`: sample timestamp를 Adapter host의 monotonic clock에서 직접 찍을 때만 사용.
- `UNSTAMPED`: 원본 timestamp가 없음을 숨기지 않는 최후 수단.

`ProbeClock`은 해당 source clock을 실제로 읽어야 한다. shared runtime 기본 구현은 host monotonic을 반환하므로 vendor clock이 별도라면 service 또는 backend contract를 범용적으로 확장하기 전에 설계 검토가 필요하다.

### Device와 feature

각 feature는 다음을 모두 정의한다.

- `qualified_name`: `<device_id>.<domain>.<field>` 형식의 영구 이름.
- `feature_id`: `stable_feature_id(qualified_name)` 사용. 수동 번호 부여 금지.
- `semantic`: `actual_*`, `controller_target_*`, sensor 의미를 명확히 표현.
- `role`: STATE, EFFECTIVE_ACTION, COMMAND, AUXILIARY 중 정확한 역할.
- `unit`: `rad`, `rad/s`, `m`, `N`, `ratio` 등 SI/명시 단위.
- `shape`: scalar도 `[1]`; 축/joint 순서를 문서화.
- `interpolation`: continuous state는 보통 LINEAR, target/mode는 ZERO_ORDER_HOLD, discrete quality는 NEAREST/NONE.
- `required`: 없으면 anchor context/step을 invalid 처리해야 하는지 여부.

`descriptor_revision()`으로 deterministic revision을 계산한다. feature order, shape, unit, required flag가 바뀌면 revision이 바뀌고 새 session/schema가 필요하다.

## 5. Sample mapping 규칙

backend의 `sample()`은 `Sample(source_time_ns, values)`를 반환한다.

1. vendor state snapshot을 가능하면 한 번만 읽고 모든 feature를 같은 snapshot에서 만든다.
2. descriptor의 joint/axis 순서를 고정하고 매 sample마다 shape를 검증한다.
3. SDK 배열 view를 그대로 넘기지 말고 immutable Python list로 복사한다.
4. NaN/Inf, stale state, missing field는 valid=false가 되도록 누락 또는 명시적 invalid reason으로 전달한다.
5. SDK call은 blocking일 수 있으므로 shared service가 `asyncio.to_thread`로 호출한다. backend 내부에서 무한 대기하지 말고 vendor timeout을 설정한다.
6. reconnect 중 이전 sample을 새 시각으로 재발행하지 않는다.
7. position/state와 effective target을 서로 다른 SDK field에서 읽는다.

다중 device Adapter를 구현할 때 현재 shared runtime이 sample message의 `device_id`를 첫 descriptor device로 설정하는 점을 고려한다. production에서는 component별 Adapter/UDS로 분리하는 것이 기본이며, 한 Adapter의 true multi-device stream이 필요하면 common runtime을 테스트와 함께 범용 확장한다.

## 6. Command 안전 규칙

backend `command(mode, values)`는 다음 순서로 처리한다.

1. mode allowlist.
2. exact vector shape/joint order.
3. 모든 값 finite.
4. hard position/velocity/force/torque/workspace limit.
5. robot connected/ready/servo/mode/protective-stop 상태.
6. vendor command builder에 명시적 duration/priority/rate limit 설정.
7. SDK acknowledgement 또는 effective accepted target 대기.
8. 실제 적용되거나 clamp된 `effective_values` 반환.

검증 실패는 motion 없이 `ValueError` 또는 명시적 domain error를 발생시켜 `COMMAND_STATUS_REJECTED`로 변환한다. timeout/transport failure는 FAILED/UNAVAILABLE 정책을 사용하며 성공으로 위조하지 않는다. command log에는 key, raw credential, 민감한 network address를 넣지 않는다.

실제 motion test는 E-stop, 안전 구역, 낮은 속도/범위, operator 입회 없이는 실행하지 않는다.

## 7. App와 Docker 구현

`app.py`는 `serve`, `healthcheck`, `self-test` 세 command를 제공한다.

- `serve`: backend 생성 후 `python.adapter_runtime.service.serve()`로 absolute UDS bind.
- `healthcheck`: SDK 설치/version과 read-only state/health를 확인. motion command 금지.
- `self-test`: mock 또는 vendor-supported simulator로 descriptor, timestamp monotonicity, shape, effective target semantics를 assertion.

Dockerfile 요구사항:

- base image digest pin.
- vendor SDK exact version pin과 `pip check`/동등 검사.
- non-root UID/GID 10001, read-only root compatible, `/tmp`만 writable.
- source에는 secret/robot address를 COPY하지 않음.
- build 중 mock/simulator semantic self-test.
- healthcheck가 실제 존재하는 module command를 사용.

Compose service 예시:

```yaml
  adapter-<vendor>:
    profiles: ["<vendor>"]
    build:
      context: .
      dockerfile: adapters/<vendor>/docker/Dockerfile
    image: robot-adapter-<vendor>:local
    env_file:
      - path: .env.adapter-<vendor>
        required: false
    user: "10001:10001"
    read_only: true
    tmpfs: [/tmp:size=64m,mode=1777]
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    volumes:
      - adapter-sockets:/run/robot-adapters
    healthcheck:
      test: ["CMD", "python", "-m", "adapters.<vendor>.app", "healthcheck"]
```

## 8. Embodiment 연결

```yaml
adapters:
  - adapter_instance_id: <vendor>-main
    endpoint: unix:///run/robot-adapters/<vendor>.sock
    required: true

devices:
  - device_id: body
    adapter_instance_id: <vendor>-main
    role: main_robot
    required: true

vector_layout:
  observation:
    - feature: body.joint.position
      required: true
  action:
    - feature: body.joint.effective_target_position
      required: true
```

feature name은 Adapter descriptor와 byte-for-byte 일치해야 한다. action vector에는 requested command보다 실제 controller effective target을 우선한다.

## 9. 필수 테스트 Matrix

### SDK 없이 자동 실행

- protobuf generation/import.
- descriptor deterministic revision과 stable feature ID.
- 모든 feature shape/unit/order.
- monotonic sample sequence/timestamp.
- NaN/Inf/missing/stale sample invalid 처리.
- command mode/shape/range/duplicate UUID rejection.
- accepted command의 effective feedback.
- UDS descriptor/sample/command/health contract.
- disconnect/reconnect와 bounded timeout.
- `scripts/verify-vendor-boundary.py`.
- non-root/read-only/cap-drop Compose health.

### Simulator/official SDK 설치 환경

- exact SDK version fail-closed.
- 공식 simulator/mock state semantic mapping.
- clock probe offset/drift와 jump reset.
- SDK reconnect와 model variant별 DOF/joint order.

### 실제 hardware gate

- read-only state/effective target comparison.
- 낮은 범위 command와 actual motion/effective feedback.
- lease expiry/control loss/Adapter restart 시 stop behavior.
- E-stop/protective stop/fault 상태 command rejection.
- 장시간 sample cadence와 timestamp calibration.

Hardware 없이 수행한 test를 physical PASS로 기록하지 않는다.

## 10. AI coding agent용 작업 지시 Template

아래 block에 vendor 사실을 채워 agent에 전달한다.

```text
이 repository에 <VENDOR>/<MODEL> Hardware Adapter를 추가하라.

기준 문서:
- docs/manuals/ADAPTER_AUTHORING_FOR_AI_AGENTS.md 전체
- proto/adapter_api.proto
- adapters/template/app.py
- python/adapter_runtime/service.py
- config/embodiment.example.yaml

Vendor 입력:
- SDK package/version: <EXACT>
- 공식 문서/source: <PATH OR URL>
- model/DOF/joint order: <EXACT>
- state clock 의미: <EXACT>
- state/effective-target field: <EXACT>
- command modes/units/ranges: <EXACT>
- connection/reconnect/timeouts: <EXACT>
- simulator availability: <EXACT>
- 허용된 physical test 범위: <EXACT>

제약:
1. vendor SDK import는 adapters/<vendor>/mapping.py에만 둔다.
2. Generic Edge/Receiver와 기존 protobuf field number를 변경하지 않는다.
3. 모든 version/shape/clock/command safety를 fail closed한다.
4. UDS/non-root/read-only Compose service와 env example을 추가한다.
5. mock/simulator/contract tests를 실제 실행한다.
6. hardware가 필요한 항목만 BLOCKED_HARDWARE로 분류하고 재현 절차를 남긴다.
7. secret, 실제 robot credential/address를 commit하지 않는다.
8. format/lint/test/build/Compose/vendor-boundary/package validation을 통과한다.
```

## 11. 완료 기준

AI agent는 변경 파일, descriptor table, clock 근거, command safety 근거, 실행한 명령/결과, physical gate를 문서화한다. Core/Receiver vendor leakage 0, 자동 test 실패 0, package secret 0일 때만 software-complete로 결론 낸다.
