# 범용 로봇 멀티카메라 동기화 송출 및 LeRobot 데이터셋 백엔드 설계서

- 문서 버전: 2.1
- 상태: 구현 기준안
- 구현 대상: AI 코딩 에이전트
- 주 언어: Rust
- 영상 프레임워크: GStreamer
- 로봇/부품 연동: 벤더 독립 Adapter API(gRPC over Unix domain socket 권장)
- 로컬 카메라 호환 계층: v4l2loopback
- 배포 방식: Docker / Docker Compose
- 기준 운영체제: Linux

## 문서 개정 요약

- Receiver의 카메라별 SRT 수신, provisional identity, authoritative anchor 판정 절차를 명문화했다.
- anchor/카메라 catalog는 `SessionManifestV1.anchor_camera_id`를 최종 기준으로 사용한다.
- SRT `streamid` canonical 계약, HMAC 검증, session 변경 시 reconnect 규칙을 추가했다.
- decoder 전 SEI 추출, manifest chunking/late join, raw TS replay identity 보존 규칙을 추가했다.
- 메타데이터 bitrate budget, 자원 admission, disk/spool, timestamp calibration 정책을 추가했다.
- Receiver API, Dockerfile, 운영/검증 스크립트 및 네 차례 검토 보고서를 패키지에 포함한다.

---

## 0. AI 구현 에이전트에 대한 지시

이 문서는 요구사항 명세, 아키텍처, 프로토콜, 배포 및 수용 기준을 동시에 제공한다. 구현 에이전트는 다음 규칙을 따른다.

1. `MUST`는 필수, `SHOULD`는 특별한 이유가 없으면 구현, `MAY`는 선택 사항이다.
2. LeRobot 또는 기존 LeRobot 카메라 코드를 수정하지 않는다.
3. OBS를 사용하지 않는다.
4. 물리 카메라는 범용 Edge Core만 열고, LeRobot은 생성된 가상카메라만 연다.
5. 외부 송출 실패·지연·재연결은 LeRobot UI용 가상카메라 분기를 블로킹해서는 안 된다.
6. Edge Core와 Receiver Core에는 특정 로봇 벤더 SDK, 특정 로봇 protobuf, 특정 그리퍼 SDK를 직접 링크하지 않는다.
7. 로봇·그리퍼·툴·모바일 베이스 등 하드웨어별 코드는 독립 Adapter 프로세스 또는 컨테이너로 분리한다.
8. 모든 카메라 프레임에는 공통 시간축의 동기화 timestamp를 영상 스트림 내부에 포함한다.
9. joint, gripper, state, action, 품질 정보 등 timestamp 이외의 프레임 컨텍스트는 환경설정으로 지정한 anchor 카메라에만 포함한다.
10. 비-anchor 카메라의 프레임별 SEI payload에는 동기화 timestamp 외의 의미 데이터가 들어가면 안 된다.
11. 최종 학습 샘플은 anchor 영상 프레임을 기준으로 생성하며, 다른 카메라는 timestamp nearest matching으로 결합한다.
12. RobotState 또는 부품 state는 원래 수신 주기로 버퍼링하되, 최종 데이터셋에는 anchor 프레임 시각으로 재샘플링된 한 세트만 저장한다.
13. 프레임 timestamp는 별도 네트워크 sidecar 없이 H.264/H.265 access unit 내부에서 복원 가능해야 한다.
14. 세션 manifest와 vector schema는 anchor 스트림에 주기적으로 포함하거나 관리 API로 제공한다. 프레임 정렬 자체는 관리 API에 의존하지 않는다.
15. 모든 테스트, Dockerfile, Compose 파일, 예제 환경변수, Adapter 템플릿, 운영 문서를 함께 제출한다.
16. 명세와 구현이 충돌하면 데이터 정합성, LeRobot UI 비간섭, 로봇 안전, 확장성 순으로 우선한다.
17. 각 송출 카메라는 독립 SRT 연결과 안정적인 `stream_slot`을 사용하며 `port = base + slot` 규칙을 따른다.
18. Receiver는 SRT `streamid`와 수신 포트로 연결을 임시 식별하되, 최종 anchor와 camera catalog는 `SessionManifestV1`을 authoritative source로 판정한다.
19. SRT `streamid`의 `role=anchor`는 부트스트랩 힌트일 뿐이며 manifest와 불일치하면 세션을 quarantine 또는 거부한다.
20. SEI는 MPEG-TS demux와 H.264/H.265 AU parser 뒤, decoder 전에 추출한다.
21. session 변경 시 SRT 연결을 재수립하여 transport `session_id`를 갱신한다. stale stream ID를 계속 사용해서는 안 된다.
22. SRT stream ID는 TS 파일 내부에 보존되지 않을 수 있으므로 Receiver는 연결 단위 `stream-envelope.json`을 저장한다. 이는 프레임 timestamp sidecar가 아니다.
23. manifest를 받기 전에는 preview와 bounded raw recording만 허용하고 dataset readiness는 false로 유지한다.
24. GStreamer runtime은 최소 1.22를 요구하며 SRT, MPEG-TS, AU parser, 복수 SEI round-trip capability를 startup self-test한다. 1.22 계열에서 parser element가 복수 SEI를 모두 노출하지 못하면 custom codec parser가 필수다.

---

## 1. 목표

로봇 컴퓨터에 연결된 모든 지원 카메라를 자동 조사하고, 비활성화되지 않은 각 카메라에 대해 다음을 동시에 수행하는 범용 백엔드를 구현한다.

```text
물리 카메라 1회 캡처
    ├─ 비압축/최소변환 영상 → 전용 가상카메라 → 기존 LeRobot UI
    └─ H.264/H.265 영상 + 프레임 내 공통 timestamp → 수신 서버

anchor 카메라만 추가로:
    └─ 같은 프레임 access unit에 observation/action 컨텍스트 포함
```

수신 서버는 다음 역할을 수행한다.

- 모든 카메라 스트림 수신 및 상태 관리
- anchor 프레임 기준 멀티카메라 시간 정렬
- 범용 타임라인 편집·재생 API
- 로봇과 부품 조합에 대한 명령 전달
- 실제 observation state와 실제 유효 action을 anchor 프레임에 결합
- episode/session 관리
- LeRobotDataset 생성
- 원본 스트림, 정합성 지표, manifest 보존

### 1.1 핵심 설계 결정

- 공통 시간축은 특정 로봇의 clock이 아니라 로봇 호스트의 **Edge Timebase**다.
- 각 로봇/부품 Adapter는 자신의 source clock을 Edge Timebase로 매핑하거나, 같은 호스트의 monotonic clock으로 직접 timestamp한다.
- 타임라인 명령 주기, 로봇 state 수신 주기, 카메라 FPS는 서로 독립적이다.
- 절대시각 예약 실행은 기본 요구사항이 아니다. 실제 실행 결과를 timestamp 기준으로 재구성한다.
- `observation.state`와 `action`의 구성은 Embodiment Manifest가 정의한다.
- action의 기본 권위 값은 Adapter가 보고한 `effective_action` 또는 현재 controller target이다.
- 이를 제공하지 못하는 Adapter는 마지막 accepted command를 fallback으로 사용할 수 있으나 품질 등급을 낮춘다.
- 멀티카메라의 동기화는 공통 timestamp와 허용 skew 검증을 의미하며, 임의의 USB 카메라에 하드웨어 동시 노출을 보장하지 않는다.

---

## 2. 메타데이터 성능 검토와 최종 정책

### 2.1 모든 카메라에 전체 로봇 메타데이터를 넣을 때의 비용

예를 들어 observation/action에 총 30~60개의 `float32` 값이 있고 actual position, target position, velocity 및 품질 필드를 포함하면 프레임당 대략 수백 바이트에서 1 KiB 안팎이 된다.

```text
대략적 예시
  50 state float + 50 action float = 400 bytes
  품질/헤더/프로토콜 오버헤드 포함 ≈ 500~900 bytes/frame
  30 fps × 8 cameras ≈ 120~216 KiB/s
```

영상 비트레이트와 비교하면 네트워크 대역폭 자체가 치명적인 수준은 아니다. 그러나 다음 문제가 더 중요하다.

- 카메라 수만큼 state 보간, serialization, SEI 삽입을 반복한다.
- 각 카메라의 capture 시각이 조금씩 다르므로 카메라마다 서로 다른 observation/action 사본이 생긴다.
- 한 dataset step의 authoritative state/action이 무엇인지 모호해진다.
- frame context map과 메모리 할당, lock contention, 인코더 후처리 비용이 카메라 수에 비례한다.
- 카메라 추가 시 메타데이터 처리 부하가 영상 인코더 부하와 함께 증가한다.

### 2.2 최종 정책

모든 카메라에는 프레임별 동기화 timestamp만 포함한다.

```text
모든 카메라의 access unit
└─ SyncTimestampV1
   └─ capture_time_edge_ns
```

anchor 카메라에는 같은 access unit에 전체 컨텍스트를 추가한다.

```text
anchor access unit
├─ SyncTimestampV1
│  └─ capture_time_edge_ns
└─ AnchorFrameContextPacketV1
   └─ CRC-verified AnchorFrameContextV1
      ├─ observation_state[]
      ├─ action[]
      ├─ auxiliary values
      ├─ interpolation/clock quality
      └─ validity
```

이 정책의 목적은 단순 대역폭 절약보다 **단일 authoritative sample**, 구현 단순화, 안정적 성능, 일관된 데이터셋 의미를 확보하는 데 있다.

### 2.3 비-anchor payload 제한

비-anchor 프레임의 per-frame SEI에는 다음만 허용한다.

```text
fixed64 capture_time_edge_ns
```

다음 정보는 비-anchor 프레임별 payload에 넣지 않는다.

- camera ID
- frame sequence
- robot/device state
- action
- joint 이름/스키마
- clock model residual
- session manifest
- 품질 플래그

camera identity, session, stream epoch는 SRT stream ID, 수신 포트 매핑 및 anchor의 SessionManifest로 식별한다. 프레임 순서는 PTS/AU 순서와 MPEG-TS continuity로 관리한다.

### 2.4 권장 payload 상한

- `SyncTimestampV1`: protobuf 기준 16 bytes 미만을 목표로 한다.
- `AnchorFrameContextPacketV1`: soft budget 2 KiB, hard cap 8 KiB.
- joint 이름, 단위, feature slice는 매 프레임 반복하지 않고 manifest에만 기록한다.
- 대용량 센서 데이터, point cloud, 원시 force trace, 로그 텍스트는 SEI에 넣지 않는다.

---

## 3. 범위와 비범위

### 3.1 범위

- Linux V4L2 카메라 자동 탐색 및 hotplug 감시
- 각 지원 카메라에 대한 안정적 ID 생성
- 물리 카메라별 가상 V4L2 카메라 생성/할당
- 카메라별 독립 GStreamer 파이프라인
- 환경변수 기반 송출 제외/완전 비활성화
- 환경변수 기반 anchor 카메라 지정
- 카메라별 SRT/MPEG-TS/H.264 송출
- 모든 송출 프레임의 timestamp SEI 삽입/추출
- anchor 프레임의 observation/action SEI 삽입/추출
- 벤더 독립 Adapter API
- 로봇, 그리퍼, 툴, 모바일 베이스 등 복수 장치 조합
- source clock과 Edge Timebase 매핑
- anchor 프레임 시각의 state/action 보간 및 재샘플링
- 멀티카메라 프레임 그룹 생성
- 타임라인 명령 실행 및 제어권 관리
- LeRobotDataset 변환
- Docker 이미지와 Compose 배포
- 로그, 메트릭, health check, 재연결, 장애 처리

### 3.2 비범위

- LeRobot UI 또는 LeRobot Python 패키지 수정
- 임의 USB 카메라의 하드웨어 동시 노출 보장
- 카메라 펌웨어 수정
- 정책 학습 자체
- 완전한 사용자용 프런트엔드
- WAN을 통한 하드 실시간 모터 제어 보장
- 특정 벤더 SDK의 내부 안정성 보장
- 대용량 센서 데이터를 영상 SEI에 전부 포함하는 기능

### 3.3 지원 카메라의 정의

MVP에서 “현재 연결된 모든 카메라”는 다음을 만족하는 모든 지원 V4L2 캡처 endpoint를 뜻한다.

- V4L2 `Video Capture` 또는 `Video Capture Multiplanar` capability
- metadata-only 또는 output-only 노드가 아님
- RGB/BGR/YUYV/UYVY/NV12/MJPEG 등 GStreamer로 8-bit 표시 영상으로 변환 가능
- 생성된 v4l2loopback 장치가 아님

깊이, IR, thermal, vendor 전용 포맷은 Camera Source Adapter 확장으로 추가한다. 탐색 결과에는 unsupported 사유를 노출한다.

---

## 4. 상위 아키텍처

```text
┌──────────────────────────── Robot Host ─────────────────────────────┐
│                                                                     │
│  Physical Cameras                                                   │
│   cam A ─┐                                                          │
│   cam B ─┼──► Generic Edge Core                                     │
│   cam N ─┘      ├─ Camera Discovery / Hotplug                       │
│                 ├─ Virtual Camera Manager                           │
│                 ├─ Shared Edge Timebase                             │
│                 ├─ Adapter Registry                                 │
│                 ├─ Embodiment State/Action Aggregator               │
│                 ├─ Anchor Frame Synchronizer                        │
│                 ├─ Per-Camera GStreamer Pipeline                    │
│                 └─ Generic Control Gateway                          │
│                                                                     │
│  Hardware Adapters (separate processes/containers)                  │
│   ├─ rby1-adapter ───────┐                                          │
│   ├─ other-robot-adapter ├─ gRPC/UDS Adapter API                    │
│   ├─ gripper-adapter ────┤                                          │
│   └─ tool-adapter ───────┘                                          │
│                                                                     │
│  Camera output                                                      │
│   ├─ /dev/video40.. → existing LeRobot UI                           │
│   └─ SRT streams → Receiver                                         │
└─────────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌────────────────────────── Receiver Host ────────────────────────────┐
│                                                                     │
│  Generic Receiver                                                   │
│   ├─ SRT ingest / reconnect                                         │
│   ├─ MPEG-TS demux                                                   │
│   ├─ timestamp/context SEI extraction                               │
│   ├─ raw stream segment storage                                     │
│   ├─ decoded preview                                                 │
│   ├─ anchor-based multicamera synchronizer                          │
│   ├─ timeline/session/episode API                                   │
│   ├─ Generic Control Client → Edge Control Gateway                  │
│   └─ normalized dataset staging                                     │
│                                                                     │
│  Python Dataset Builder                                             │
│   └─ official LeRobot library → LeRobotDataset                      │
└─────────────────────────────────────────────────────────────────────┘
```

### 4.1 제어 경로

```text
Timeline Server
  → Generic Control API
  → Edge Control Gateway
  → target device Adapter
  → vendor SDK/gRPC/CAN/serial
```

Receiver는 특정 로봇 벤더 API를 직접 호출하지 않는다. 새 로봇이나 그리퍼를 추가할 때 Receiver와 Edge Core를 수정하지 않고 Adapter를 추가하는 것이 목표다.

---

## 5. 계층 분리 원칙

### 5.1 Generic Edge Core

다음 기능만 포함한다.

- 카메라 탐색/캡처/가상카메라/송출
- Edge Timebase
- Adapter 연결과 capability 수집
- Adapter sample ring buffer
- feature별 보간
- anchor 컨텍스트 생성
- generic command routing
- 세션/manifest/메트릭

다음 코드는 포함하면 안 된다.

- `rby1_sdk` import 또는 link
- 특정 로봇의 joint order 하드코딩
- 특정 그리퍼 register map
- 벤더 protobuf 타입을 core data model에 노출
- `target_position` 같은 특정 필드가 항상 존재한다고 가정하는 코드

### 5.2 Hardware Adapter

Adapter는 벤더별 차이를 canonical model로 변환한다.

- 연결/인증/재연결
- 장치 capability 탐색
- state 수집
- effective action 또는 target 수집
- source timestamp 해석
- command 변환 및 전송
- 안전 제한과 벤더 오류 변환
- 벤더 SDK 버전 self-test

### 5.3 Adapter 배포 방식

기본 방식은 독립 프로세스/컨테이너와 gRPC over Unix domain socket이다.

장점:

- 벤더 SDK와 native dependency 격리
- SDK 충돌 및 Python/C++ runtime 충돌 방지
- Adapter만 별도 재시작 가능
- 새 로봇 추가 시 Core 이미지 재빌드 불필요
- Adapter별 권한 최소화

고성능이 필요한 특수 장치는 같은 프로세스의 Rust trait 구현을 MAY 지원하지만, 공개 확장 계약은 gRPC Adapter API로 유지한다.

---

## 6. 범용 Adapter API

### 6.1 Adapter가 노출하는 서비스

```protobuf
service HardwareAdapter {
  rpc GetDescriptor(GetDescriptorRequest) returns (AdapterDescriptor);
  rpc StreamSamples(StreamSamplesRequest) returns (stream DeviceSample);
  rpc CommandStream(stream CommandEnvelope) returns (stream CommandFeedback);
  rpc ProbeClock(ClockProbeRequest) returns (ClockProbeResponse);
  rpc GetHealth(HealthRequest) returns (HealthResponse);
}
```

### 6.2 Adapter Descriptor

Adapter는 한 개 이상의 논리 장치를 노출할 수 있다.

예:

```text
rby1-adapter
├─ body
├─ right_arm
├─ left_arm
├─ head
└─ integrated_gripper
```

또는:

```text
industrial-arm-adapter → arm
robotiq-adapter        → gripper
mobile-base-adapter    → base
```

각 `DeviceDescriptor`는 다음을 포함한다.

- `device_id`: session 내 고유 이름
- `device_kind`: robot, arm, base, gripper, tool, sensor, custom
- `role`: main_robot, left_gripper, right_gripper 등 사용자 의미
- `state_features`
- `effective_action_features`
- `command_features`
- feature 단위, shape, interpolation mode
- command capability: unary, stream, trajectory
- source clock descriptor
- adapter/vendor/sdk version
- safety limits와 optional capability flags

### 6.3 Sample 계약

각 sample은 다음을 제공한다.

```text
adapter_instance_id
device_id
sample_seq
source_time_ns (optional)
source_clock_id
state feature blocks
effective action feature blocks
sample validity
```

Core는 수신 시 Edge monotonic time을 추가하고 Clock Mapper를 통해 `sample_time_edge_ns`를 계산한다.

### 6.4 Command 계약

Command는 vendor field가 아니라 manifest에서 정의한 canonical action vector 또는 named feature block을 사용한다.

```text
command_id
device_id
command_mode
action_schema_id
values[]
client_time_ns (diagnostic)
lease_id
```

Adapter feedback:

```text
received
validated
accepted
applied/active, if observable
rejected
completed
vendor_error
```

### 6.5 action 권위 우선순위

최종 dataset action은 다음 우선순위를 사용한다.

1. Adapter가 controller에서 관찰한 effective target/action
2. Adapter가 받은 applied/active feedback
3. Adapter가 받은 accepted command
4. Edge가 마지막으로 전달한 command
5. unavailable

각 값은 `ActionSourceQuality`로 기록한다.

---

## 7. Embodiment와 부품 조합 모델

### 7.1 Embodiment

`Embodiment`는 한 학습 대상에 포함되는 장치들의 조합이다.

```text
Embodiment
├─ main robot
├─ left gripper
├─ right gripper
├─ mobile base
└─ optional tools
```

장치 조합은 `/etc/robot-edge/embodiment.yaml`에서 정의한다.

### 7.2 Canonical Feature Schema

매 프레임 joint 이름과 단위를 반복하지 않는다. SessionManifest에 deterministic vector layout을 정의한다.

예:

```text
observation_state vector
[0:7]   main_arm.joint.position
[7:14]  main_arm.joint.velocity
[14]    gripper.position
[15]    gripper.force

action vector
[0:7]   main_arm.joint.target_position
[7]     gripper.target_position
```

각 feature는 다음 속성을 갖는다.

- `feature_id`
- `qualified_name`
- `semantic`
- `dtype`
- `unit`
- `shape`
- `offset`와 `length`
- `interpolation`: linear, zoh, nearest, none
- `required`
- `source_device_id`

Manifest와 schema ID의 재현성을 위해 정렬과 해시 규칙을 고정한다.

- device descriptor는 `device_id`, camera descriptor는 `stable_camera_id` 오름차순으로 정렬한다.
- `feature_slices`는 vector kind, offset, qualified name 순서로 정렬하며 offset overlap/gap 정책을 검증한다.
- feature validity bitmap의 bit `i`는 `feature_slices[i]`에 대응하고 각 byte 안에서는 least-significant-bit first다.
- `observation_schema_id`와 `action_schema_id`는 해당 vector schema의 RFC 8785 JSON Canonicalization Scheme 표현에 SHA-256을 적용한 뒤 앞 8 bytes를 unsigned big-endian으로 해석한다. `0`은 invalid 예약값이다.
- manifest는 wire CRC를 위해 전송 exact bytes를 사용하므로 protobuf 재직렬화를 canonicalization으로 간주하지 않는다.
- observation/action/auxiliary의 숫자는 기본적으로 finite `float32`여야 한다. NaN/Inf는 명시적 feature policy가 없는 한 context invalid 사유다.

### 7.3 부품 추가

새 그리퍼를 추가할 때 필요한 작업:

1. Adapter API를 구현한다.
2. descriptor에서 gripper state/action feature를 선언한다.
3. Compose에 Adapter 서비스를 추가한다.
4. `embodiment.yaml`에 장치와 vector layout을 추가한다.
5. Core나 Receiver 소스는 수정하지 않는다.

### 7.4 Schema 고정

Episode 시작 시 다음을 고정한다.

- 활성 adapter 목록
- device descriptor revision
- observation schema ID
- action schema ID
- joint/component 순서
- 카메라 catalog와 anchor camera

Episode 도중 schema가 바뀌면 해당 episode를 종료 또는 실패 처리한다.

---

## 8. 카메라 탐색과 정책

### 8.1 탐색 소스

다음 두 경로를 결합한다.

1. `GstDeviceMonitor`
   - Video/Source 목록과 add/remove 이벤트
   - GStreamer caps
2. udev/sysfs
   - driver, bus, serial, product, interface, physical path
   - v4l2loopback 판별
   - stable identity 생성

### 8.2 물리 장치 필터와 logical camera grouping

다음 중 하나면 자동 제외한다.

- sysfs driver가 `v4l2loopback`
- `/sys/devices/virtual/video4linux` 아래 장치
- managed virtual camera label
- capture capability 없음
- metadata-only 또는 output-only node
- 지원 caps 없음

하나의 USB 카메라가 MJPEG/raw/metadata/depth 등 여러 `/dev/videoN` 노드를 노출할 수 있으므로 **video node 수를 물리 카메라 수로 간주해서는 안 된다.** udev parent, USB interface, serial, `bus_info`, media-controller entity를 이용해 logical camera group을 만들고, 정책에 맞는 하나의 RGB capture endpoint를 선택한다.

```text
Physical device / logical camera
├─ /dev/video0  RGB capture       ← 선택
├─ /dev/video1  metadata          ← 제외
└─ /dev/video2  alternate profile ← 정책에 따라 제외
```

같은 logical camera의 여러 독립 센서 스트림을 동시에 사용해야 하는 장치는 일반 V4L2 자동 탐색이 아니라 Camera Source Adapter로 명시적으로 모델링한다.

---

### 8.3 Stable Camera ID

우선순위:

```text
vendor/product/serial
USB interface
udev ID_PATH
V4L2 bus_info
endpoint role
```

권장 생성:

```text
stable_camera_id = "cam_" + base32(blake3(canonical_identity))[0:12]
```

serial이 없는 동일 모델은 physical path를 사용하며 포트 이동 시 ID 변경 경고를 표시한다.

Stable ID와 transport slot/virtual device mapping은 `${EDGE_STATE_DIR}/camera-map.json`에 원자적으로 저장한다. 운영 규칙은 다음과 같다.

- 동일 canonical identity가 서로 다른 active physical camera 두 대에 매핑되면 `STABLE_CAMERA_ID_COLLISION_POLICY=fail`로 readiness를 실패시키며 임의 suffix를 붙이지 않는다.
- serial이 없거나 firmware가 identity field를 바꾸는 장치는 operator override로 영구 alias를 부여할 수 있다. alias 변경은 새 camera identity revision으로 기록한다.
- 제거된 camera의 slot과 virtual device 번호는 tombstone으로 유지하며 session/episode 중에는 절대 재사용하지 않는다.
- 기본 `CAMERA_SLOT_RECLAIM_POLICY=manual`에서는 운영자가 명시적으로 reclaim하기 전까지 slot을 보존한다. 자동 reclaim을 구현할 경우 최소 tombstone 기간과 active archive 참조 검사를 모두 통과해야 한다.
- mapping 파일은 checksum과 generation을 가지며, crash 중 partial write를 막기 위해 temp-file + fsync + rename으로 commit한다.
- Receiver가 보는 `stream_slot`은 transport 주소일 뿐 camera identity가 아니다. authoritative identity는 signed stream ID와 manifest camera catalog의 교차 검증 결과다.

```dotenv
STABLE_CAMERA_ID_COLLISION_POLICY=fail
CAMERA_SLOT_RECLAIM_POLICY=manual
CAMERA_SLOT_TOMBSTONE_DAYS=30
```

### 8.4 송출 제외와 완전 비활성화

```dotenv
CAMERA_STREAM_EXCLUDE=serial:EXAMPLE_SKIP;name_regex:^Integrated Camera$
CAMERA_DISABLE=id:cam_deadbeef1234
```

평가 순서:

1. 생성된 virtual camera 자동 제외
2. `CAMERA_DISABLE`
3. virtual camera 생성
4. `CAMERA_STREAM_EXCLUDE`
5. stream branch 생성 여부 결정

`CAMERA_STREAM_EXCLUDE` 카메라는 LeRobot UI에서 계속 보이지만 외부 송출은 하지 않는다.

### 8.5 Anchor 카메라 지정

Anchor는 환경변수로 반드시 명시한다.

```dotenv
ANCHOR_CAMERA_SELECTOR=serial:FRONT_CAM_001
```

지원 selector:

- `id:<stable_camera_id>`
- `serial:<exact_serial>`
- `path:</dev/videoN>`
- `name:<exact_product_name>`
- `name_regex:<regex>`
- `usb_path:<udev_ID_PATH>`

규칙:

- selector는 정확히 한 개의 지원 카메라와 일치해야 한다.
- stable ID 또는 serial 사용을 권장한다.
- anchor가 `CAMERA_DISABLE` 또는 `CAMERA_STREAM_EXCLUDE`와 일치하면 configuration error다.
- `required_for_dataset` camera가 stream-excluded로 평가되면 configuration error다. virtual-only camera는 manifest에 포함할 수 있지만 `stream_excluded=true`, `required_for_dataset=false`, `transport_port=0`이어야 한다.
- anchor가 없으면 가상카메라와 비-anchor preview/stream은 계속할 수 있지만 dataset readiness는 false다.
- 기본 `ANCHOR_MISSING_POLICY=degraded`는 LeRobot UI 비간섭을 위해 전체 Edge Core를 종료하지 않는다.
- `ANCHOR_MISSING_POLICY=fail_start`를 운영 정책으로 선택할 수 있다.
- episode 도중 anchor가 사라지면 해당 episode는 즉시 invalid/abort 처리한다.
- episode 중 자동 anchor failover는 금지한다. 데이터 의미가 바뀌기 때문이다.

### 8.6 프로파일 선택

기본 목표:

```dotenv
CAMERA_WIDTH=1280
CAMERA_HEIGHT=720
CAMERA_FPS=30
CAMERA_FORMAT_PREFERENCE=MJPG,NV12,YUY2
CAMERA_STRICT_PROFILE=true
```

모든 카메라가 동일 FPS를 지원하지 않으면 원본 FPS를 유지하고 receiver에서 anchor 기준 nearest matching한다. 프레임 복제는 기본 비활성화한다.

---

## 9. 가상카메라 관리

### 9.1 Virtual Device Pool

```dotenv
VIRTUAL_CAMERA_START=40
VIRTUAL_CAMERA_POOL_SIZE=16
VIRTUAL_CAMERA_LABEL_PREFIX=LeRobot Virtual
```

stable mapping 저장:

```text
/var/lib/robot-edge/camera-map.json
```

### 9.2 v4l2loopback 정책

- `exclusive_caps=1`
- `keep_format=1`
- `sustain_framerate=0`
- timeout 기본 3000 ms
- 카메라 reconnect 시 기존 virtual slot 재사용

### 9.3 LeRobot 산출물

```text
/var/lib/robot-edge/cameras.json
/var/lib/robot-edge/lerobot-camera-snippet.yaml
```

LeRobot 자체는 수정하거나 자동 재시작하지 않는다.

---

## 10. 범용 공통 시간축

### 10.1 용어

`robot_time`이라는 이름을 사용하지 않는다. 시간 기준은 로봇 종류와 독립적인 `Edge Timebase`다.

```text
edge_time_ns
clock_domain_id
session_id
session_epoch_edge_ns
```

### 10.2 Edge Timebase

같은 Linux 호스트의 모든 프로세스는 `CLOCK_MONOTONIC`을 공통 기준으로 사용할 수 있다. GStreamer pipeline은 공유 `GstSystemClock`을 사용한다.

```text
edge_time_ns = CLOCK_MONOTONIC nanoseconds
```

`edge_time_ns`는 같은 Linux kernel boot 안에서만 직접 비교 가능하다. 모든 transport와 저장 키는 최소 다음 묶음을 사용한다.

```text
edge_boot_id + session_id + edge_time_ns
```

- `edge_boot_id`: `/proc/sys/kernel/random/boot_id` 또는 그 UUID bytes
- `session_id`: schema/anchor/timebase 연속성이 유지되는 수집 세션
- `edge_time_ns`: 해당 boot의 monotonic 시각

Receiver는 서로 다른 `edge_boot_id` 또는 `session_id`의 timestamp를 직접 비교하거나 하나의 episode로 합치지 않는다. 세션 외부 비교가 필요하면 optional `CLOCK_TAI`/PTP mapping을 추가하지만 단일 Edge 호스트의 학습 데이터 정렬에는 필수가 아니다.

---

### 10.3 카메라 timestamp

우선순위:

1. hardware exposure/capture timestamp와 검증된 mapping
2. V4L2 driver monotonic timestamp
3. GStreamer source running-time + pipeline base-time
4. source pad dequeue 시 Edge Timebase

Raw buffer에서 얻은 `capture_time_edge_ns`를 encoded AU PTS와 매핑하고 encoder 뒤에서 SEI로 삽입한다. PTS가 보존되지 않거나 중복/누락되는 encoder에서는 nearest timestamp로 추정하지 말고 해당 context를 invalid 처리하거나 검증된 input-output sequence mapper를 사용한다.

서로 다른 camera driver는 exposure start, exposure end, USB completion, dequeue 등 서로 다른 사건에 timestamp를 붙일 수 있다. 따라서 camera별 systematic offset과 품질을 관리한다.

```text
capture_time_edge_ns
    = mapped_source_timestamp_ns
    + calibrated_capture_offset_ns
```

- 기본 offset은 0이지만 timestamp source 의미를 manifest에 기록한다.
- LED flash, display flash 또는 빠른 공통 motion fixture로 camera 간 offset을 측정할 수 있다.
- `expected_timestamp_accuracy_ns`, calibration revision 및 offset을 camera descriptor에 기록한다.
- timestamp source/clock이 재설정되거나 calibration 의미가 바뀌면 `stream_epoch`와 manifest revision을 증가시킨다.
- timestamp mapping이 invalid면 해당 AU는 dataset sync에 사용하지 않는다. `capture_time_edge_ns=0`은 invalid 예약값이다.
- Receiver 도착 시각으로 capture offset을 임의 보정해서는 안 된다.

---

### 10.4 Adapter source clock

Adapter마다 다음 중 하나를 선언한다.

- `EDGE_MONOTONIC`: sample timestamp가 Edge Timebase와 동일
- `SOURCE_MONOTONIC`: 별도 controller/device clock
- `TAI_UTC`: 절대 시간 계열
- `UNSTAMPED`: source timestamp 없음

`SOURCE_MONOTONIC`인 경우 Core의 generic Clock Mapper가 다음 모델을 유지한다.

```text
edge_time_ns ≈ a × source_time_ns + b
```

Clock Mapper는 벤더 독립 모듈이다.

- streaming sample의 source timestamp와 Edge receive timestamp
- optional `ProbeClock` RTT midpoint
- sliding window robust regression
- offset, drift, residual
- clock jump/reboot 탐지

### 10.5 feature별 timestamp

특정 SDK가 joint별 age 또는 센서별 update age를 제공하면 해당 계산은 Adapter 내부에서 처리한다. Core는 이미 정규화된 feature sample time을 받는다.

예:

```text
vendor state timestamp - vendor joint age
    → adapter normalized source sample time
    → generic Clock Mapper
    → sample_time_edge_ns
```

이로써 RB-Y1에만 존재하는 필드가 Core에 누출되지 않는다.

### 10.6 공통 시간축의 한계

공통 timestamp는 사후 정렬을 가능하게 하지만 카메라 exposure를 물리적으로 동시에 시작시키지 않는다. hardware trigger/PTP 카메라가 필요한 경우 Camera Source Adapter capability로 추가한다.

---

## 11. State/Action 수집과 anchor 프레임 보간

### 11.1 Adapter sample rate

각 Adapter는 장치가 허용하는 충분한 주기로 state/effective action을 전송한다.

예:

```text
camera anchor: 30 Hz
robot state:   100 Hz
fast gripper:  200 Hz
mobile base:    50 Hz
```

최종 LeRobot dataset은 anchor 30 Hz로 저장한다.

### 11.2 Ring Buffer

Core는 device/feature별 bounded time ring을 유지한다.

```text
sample_time_edge_ns
state values
effective action values
quality
clock model revision
```

기본 보관 시간은 5초다.

### 11.3 anchor frame trigger

Anchor raw frame이 캡처되면 `t_anchor = capture_time_edge_ns`를 기준으로 모든 required feature를 재샘플링한다.

```text
observation_state[k] = state features at t_anchor
action[k]            = effective action features at t_anchor
```

비-anchor 프레임에서는 state/action 보간을 수행하지 않는다.

### 11.4 보간 규칙

feature descriptor가 보간 방식을 결정한다.

- 연속 joint position/velocity: linear
- controller target이 연속 trajectory면 linear
- 계단식 command target, gripper mode, safety state: zero-order hold
- bool/enum/contact: nearest 또는 ZOH
- timestamp가 불충분한 feature: invalid

### 11.5 대기 정책

양방향 linear interpolation을 위해 anchor stream branch만 다음 sample을 짧게 기다릴 수 있다.

```dotenv
STATE_INTERPOLATION_WAIT_MS=15
STATE_MAX_GAP_MS=30
STATE_MISSING_POLICY=mark_invalid
ANCHOR_AU_HOLD_MAX_MS=25
```

- virtual camera UI branch는 기다리지 않는다.
- 비-anchor 송출 branch도 기다리지 않는다.
- anchor 송출 branch만 bounded wait한다.
- anchor raw capture 시 `FrameContextRequest`를 PTS와 내부 capture ordinal로 등록한다.
- encoder가 해당 coded-picture AU를 먼저 출력하면 `AnchorAuHoldQueue`가 최대 `ANCHOR_AU_HOLD_MAX_MS` 동안 AU를 보류한다. state bracket이 준비되거나 deadline에 도달하면 valid 또는 invalid context를 삽입해 즉시 방출한다.
- hold queue는 frame 수/bytes/time 모두 bounded이며 overflow 시 오래된 AU를 context 없이 보내지 않고 invalid context를 넣어 방출하거나 정책에 따라 해당 stream frame을 drop한다. UI branch는 이 queue를 통과하지 않는다.
- bracket이 없으면 영상은 송출하되 `AnchorFrameContextV1`을 invalid로 표시한다.
- receiver는 invalid anchor step을 dataset에서 제외한다.

### 11.6 구성 장치의 부분 실패

- required device state 누락: anchor context invalid
- optional device state 누락: manifest 정책에 따라 NaN/기본값/step drop
- action unavailable: action quality를 unavailable로 표시하고 기본 dataset 생성은 drop
- Adapter schema 변경: episode abort

---

## 12. 카메라별 GStreamer 파이프라인

### 12.1 공통 구조

```text
v4l2src
  → capture timestamp probe
  → decode if needed
  → common raw format
  → tee
      ├─ queue(leaky) → minimal convert → v4l2sink virtual camera
      └─ queue(leaky) → low-latency encoder
                       → h264parse
                       → video/x-h264,stream-format=byte-stream,alignment=au
                       → SyncTimestamp injector
                       → [anchor only] AnchorContext injector
                       → mpegtsmux
                       → srtsink
```

### 12.2 UI 분기

- 별도 queue/thread
- `max-size-buffers=2`
- `leaky=downstream`
- `sync=false`
- 외부 송출과 Adapter 상태에 무관하게 최신 프레임 유지

### 12.3 비-anchor 송출 분기

- 인코딩 완료 AU에 `SyncTimestampV1`만 삽입
- state ring, Adapter, context serializer에 접근하지 않음
- anchor state 지연으로 인한 영향을 받지 않음

### 12.4 anchor 송출 분기

- 같은 `SyncTimestampV1` 삽입
- raw frame PTS와 내부 capture ordinal로 anchor frame context 조회
- bounded state interpolation과 `AnchorAuHoldQueue` 적용
- `AnchorFrameContextPacketV1` 삽입
- IDR에서 시작하는 bounded SessionManifest chunk sequence를 주기적으로 삽입
- context lookup 실패 시 nearest frame으로 추정하지 않고 invalid/drop 정책을 적용

### 12.5 Encoder 설정

- B-frame 비활성화
- GOP 기본 1초
- low-latency mode
- hardware encoder 자동 탐지, software fallback
- PTS 보존
- encoder restart 시 stream epoch 갱신

### 12.6 Backpressure

- UI queue full: 오래된 frame drop
- 비-anchor stream queue full: 오래된 frame drop
- anchor state wait timeout: context invalid, UI 영향 없음
- SRT disconnect: bounded queue 유지 후 reconnect
- 한 카메라 pipeline 장애가 다른 카메라를 중단하지 않음

### 12.7 자원 admission control

모든 연결 카메라를 자동 송출한다고 해서 호스트가 임의 개수의 720p30/1080p30 encoder를 감당할 수 있는 것은 아니다. Edge Core는 pipeline 시작 전에 다음 자원을 점검한다.

- USB controller별 예상 bandwidth와 camera profile
- hardware encoder session 수와 codec/profile 지원
- software encoder fallback 시 CPU budget
- `/dev/dri` 또는 vendor encoder device 가용성
- virtual camera pool과 SRT slot/port 범위
- bounded queue의 총 memory budget
- anchor context serialization/SEI bitrate budget

자원이 부족하면 조용히 software fallback하여 LeRobot UI를 악화시키지 않는다. 기본 정책은 다음과 같다.

```text
UI virtual camera branch: 계속 유지
외부 stream branch: 명시적 degraded/disabled
anchor stream 불가: dataset readiness=false
```

capacity test 결과를 `MAX_ACTIVE_STREAMS`, camera override 및 encoder policy에 반영한다.

---

## 13. 프레임 내 메타데이터 프로토콜

### 13.1 SEI 종류와 고정 UUID

H.264/H.265 `user_data_unregistered` SEI를 사용하며 UUID bytes는 RFC 4122/network order다.

| Payload | 상수 | UUID |
|---|---|---|
| Timestamp | `SYNC_TIMESTAMP_UUID_V1` | `4a1191e6-9578-53b3-92a7-04c049fe0d5b` |
| Anchor context packet | `ANCHOR_CONTEXT_UUID_V1` | `62ef08bb-2eb4-59fb-b83f-f8f874a80043` |
| Manifest chunk | `SESSION_MANIFEST_UUID_V1` | `791a8fc5-d0c3-5abf-81da-abf7f0373194` |

고정값의 machine-readable source는 `config/protocol_constants.toml`이다. UUID가 payload type과 schema major version을 식별한다.

### 13.2 SyncTimestampV1

모든 송출 카메라의 모든 coded-picture AU에 정확히 하나 포함한다.

```protobuf
message SyncTimestampV1 {
  fixed64 capture_time_edge_ns = 1;
}
```

`0`은 invalid 예약값이다. 비-anchor AU에는 이 timestamp 외의 semantic per-frame metadata를 넣지 않는다.

### 13.3 AnchorFrameContextV1과 packet envelope

anchor AU에만 `AnchorFrameContextPacketV1`을 포함한다. Packet의 `serialized_context`가 실제 `AnchorFrameContextV1` protobuf bytes이며 CRC32C(Castagnoli)는 이 exact byte sequence에 대해 계산한다.

Context 내용:

- session ID와 anchor frame sequence
- manifest revision, observation/action schema ID
- deterministic packed `float32` observation/action/auxiliary vector
- feature validity bitmap
- device별 interpolation bracket/gap와 clock residual
- device별 및 전체 action source quality
- invalid reason 및 품질 flag

Timestamp는 같은 AU의 `SyncTimestampV1`을 사용한다. Adapter의 bool/int/float64 source feature는 manifest의 canonical conversion에 따라 float32로 정규화한다. required feature가 invalid면 context 전체를 invalid 처리하며 의미 불명의 NaN을 조용히 삽입하지 않는다.

CRC는 decoded protobuf를 재직렬화한 결과가 아니라 전송된 exact bytes에 대해 검증한다.

### 13.4 Context bitrate budget

프레임 메타데이터도 bitrate와 CPU를 소비한다.

```text
metadata_kbps ≈ payload_bytes × fps × 8 / 1000
2 KiB × 30 fps ≈ 492 kbps
4 KiB × 30 fps ≈ 983 kbps
8 KiB × 30 fps ≈ 1966 kbps
```

기본 soft budget은 2 KiB/frame, hard cap은 8 KiB/frame다. soft budget을 넘으면 optional auxiliary feature를 manifest 정책 순서대로 제외하거나 context를 invalid 처리한다. hard cap을 초과한 payload를 잘라 보내서는 안 된다. context size와 metadata bitrate는 metric으로 노출한다.

### 13.5 SessionManifest와 chunk envelope

Manifest 전송은 anchor stream의 첫 IDR, 주기 IDR, revision 변경 후 첫 IDR, SRT reconnect 직후 강제 IDR에서 시작한다.

Manifest 본문:

- embodiment, edge instance/boot/session ID
- authoritative anchor camera ID
- 전체 camera descriptor와 slot/epoch/port/codec/role
- adapter/device descriptor
- observation/action vector layout와 단위/의미
- software/adapter versions
- Edge clock domain과 timestamp 품질/calibration
- camera catalog/manifest revision

Manifest가 단일 SEI에서 커지는 것을 방지하기 위해 `SessionManifestV1`을 optional zstd로 압축하고 `SessionManifestChunkV1`으로 분할한다.

```text
SessionManifestV1 exact protobuf bytes
  → optional zstd
  → bounded chunks
  → SessionManifestChunkV1 × N
```

기본 chunk cap은 8 KiB, 전체 uncompressed manifest hard cap은 256 KiB다. 한 coded-picture AU에는 기본 1개 chunk만 넣고 나머지는 후속 AU에 순서대로 분산하여 큰 IDR burst를 피한다. Receiver는 session/revision별 bounded reassembly, timeout, chunk 중복/누락 검증, uncompressed exact bytes CRC32C 검증을 완료한 뒤에만 manifest를 활성화한다. `chunk_count`는 `ceil(max_total/max_chunk)` 이하로 제한하고, zstd 사용 시 declared uncompressed size, 최대 압축비, decompression CPU/time budget을 먼저 검증하여 compression bomb를 차단한다.

### 13.6 삽입 위치

Raw `GstMeta`가 encoder를 자동 통과한다고 가정하지 않는다.

```text
raw frame PTS
  → bounded FrameContextMap
encoded AU PTS
  → timestamp/context lookup
  → codec parser로 SEI NAL 삽입
```

B-frame은 기본 비활성화한다. 구현은 AU 단위 normalized PTS를 1차 key로 사용하고 동일 PTS 충돌을 내부 capture ordinal로 검증한다. DTS/도착 순서 또는 nearest timestamp로 context를 결합하지 않는다. PTS가 `NONE`, 비단조, 중복이거나 encoder가 input/output 대응을 보장하지 못하면 해당 AU context를 invalid 처리하고 metric을 남긴다. `FrameContextMap`과 orphan entry는 time/entry/byte cap으로 정리한다.

### 13.7 수신 위치와 parser 계약

SEI는 decoder 전에 추출한다.

```text
SRT → MPEG-TS demux → h264parse/h265parse(alignment=au)
    → SEI extractor
    ├─ encoded AU recorder
    └─ decoder → preview/frame queue
```

GStreamer 1.22 이상에서는 H.264/H.265 User Data Unregistered SEI를 나타내는 codec parser API를 사용할 수 있다. 복수 unregistered SEI 처리의 parser element 지원이 명확해진 1.24 이상을 권장한다. 1.22/1.23을 사용하거나 startup round-trip에서 필요한 meta가 모두 surface되지 않으면 `GstH264Parser`/`GstH265Parser` 기반 custom `sei-codec` element로 fallback한다.

Decoder가 unknown SEI를 raw frame까지 보존한다고 가정하지 않는다. Docker image의 GStreamer version/plugin feature set을 기록하고 startup 시 encode→mux→demux→parse→extract round-trip test를 수행한다.

중간 relay/transcoder가 SEI를 제거하거나 AU를 재구성할 수 있으므로 기본 dataset 경로는 Edge→Receiver direct SRT다. relay/remux는 conformance test를 통과한 경우만 허용하고 재인코딩 relay는 기본 금지한다.

---

## 14. SRT transport 식별, Receiver 부트스트랩 및 멀티카메라 정렬

### 14.1 카메라별 독립 transport

카메라마다 독립 SRT caller 연결을 사용한다.

```text
srt_port = SRT_BASE_PORT + stable_stream_slot
```

예:

| camera | slot | port | role |
|---|---:|---:|---|
| `cam_front` | 0 | 10000 | anchor |
| `cam_left_wrist` | 1 | 10001 | secondary |
| `cam_right_wrist` | 2 | 10002 | secondary |

stable mapping은 Edge state directory에 보존한다. reconnect 시 같은 slot을 재사용하고 stream pipeline의 의미가 재설정되면 `stream_epoch`를 증가시킨다. 포트는 transport slot일 뿐 camera identity의 유일한 근거가 아니다.

여러 Edge가 하나의 Receiver를 사용하면 Edge별 port block을 분리하거나 별도 SRT gateway가 canonical stream ID로 routing한다.

### 14.2 SRT stream ID 계약

모든 caller 연결은 canonical `streamid`를 설정한다.

```text
rmc1;emb=<pct>;edge=<pct>;boot=<uuid>;sid=<uuid>;cid=<pct>;slot=<u16>;epoch=<u32>;role=<anchor|secondary>;codec=<h264|h265>;sig=<base64url>
```

- field order는 위와 같이 고정한다.
- string은 UTF-8 후 RFC 3986 percent-encoding한다.
- `sig` 제외 exact bytes에 HMAC-SHA256을 계산하고 앞 16 bytes를 base64url(no padding)로 encode한다.
- 최대 256 bytes다. embodiment ID와 stable camera ID는 각각 32/48 encoded bytes 이하를 권장한다.
- duplicate key, unknown required key, non-canonical encoding은 거부한다.
- `role`은 provisional hint일 뿐 authoritative 선언이 아니다.
- `sid`가 바뀌면 기존 SRT 연결을 닫고 새 stream ID로 reconnect한다.

SRT passphrase와 stream ID HMAC는 역할이 다르다. passphrase는 transport confidentiality/integrity를 위한 것이고 HMAC는 routing identity 위변조 검증용이다.

### 14.3 Receiver listener와 연결 수락

Receiver는 slot별 `srtsrc mode=listener`를 실행하고 caller 연결 callback에서 media pipeline 생성 전에 다음을 검증한다.

1. stream ID 길이/문법/canonical encoding
2. HMAC
3. expected embodiment/edge policy
4. `slot < MAX_CAMERAS`
5. `listen_port == base_port + slot`
6. codec 지원
7. duplicate camera/slot/session/epoch 충돌

통과한 연결만 provisional registry에 등록한다. 한 camera pipeline 오류가 다른 listener에 영향을 주지 않아야 한다.

### 14.4 Receiver 부트스트랩 상태 머신

```text
LISTENING
→ TRANSPORT_IDENTIFIED
→ MEDIA_PROBING
→ PROVISIONAL_STREAM
→ MANIFEST_VALIDATED
→ DATASET_READY
```

- `TRANSPORT_IDENTIFIED`: port와 stream ID 검증 완료
- `MEDIA_PROBING`: MPEG-TS/codec/AU parser 확인
- `PROVISIONAL_STREAM`: preview/raw recording 가능, anchor 미확정
- `MANIFEST_VALIDATED`: chunk reassembly/CRC/schema/transport 교차 검증 완료
- `DATASET_READY`: anchor context와 required cameras/devices 준비

Manifest가 timeout 내 도착하지 않으면 preview-only로 유지하며 dataset step을 생성하지 않는다.

### 14.5 Authoritative anchor 판정

`SessionManifestV1.anchor_camera_id`가 최종 기준이다. `role=anchor`는 manifest를 찾기 위한 힌트다. Manifest를 실은 stream에 대해 다음을 모두 검증한다.

```text
manifest.session_id == streamid.sid
manifest.edge_boot_id == streamid.boot
manifest.edge_instance_id == streamid.edge
manifest.embodiment_id == streamid.emb
manifest.anchor_camera_id == current_stream.camera_id
manifest camera.slot == current listen slot
manifest camera.epoch == streamid.epoch
manifest camera.codec/port == current transport
manifest camera.role == ANCHOR
current stream에 valid AnchorFrameContextPacket 존재
다른 stream에는 anchor context가 없음
```

다음은 session quarantine/reject 사유다.

- 둘 이상의 서로 다른 manifest가 자신을 anchor라고 선언
- manifest catalog에 없는 camera 연결
- `role` hint와 manifest 불일치
- 같은 camera/slot에 충돌하는 session/epoch
- `EXPECTED_ANCHOR_CAMERA_ID`와 불일치
- non-anchor stream에서 context/manifest SEI 발견

Episode 도중 anchor를 자동 변경하지 않는다.

### 14.6 카메라별 ingest pipeline

```text
srtsrc listener
  → tsparse/tsdemux
  → h264parse/h265parse
  → video/x-h264 또는 video/x-h265,alignment=au
  → SEI extractor
  ├─ encoded AU segment recorder
  └─ decoder → preview/frame queue
```

stream registry key:

```text
edge_instance_id + edge_boot_id + session_id + camera_id + stream_epoch
```

포트 번호만 key로 사용하지 않는다.

### 14.7 Anchor 정보 추출

SEI extractor는 encoded AU에서 다음 payload를 읽는다.

```text
SYNC_TIMESTAMP_UUID_V1
  → SyncTimestampV1

ANCHOR_CONTEXT_UUID_V1
  → AnchorFrameContextPacketV1
  → exact bytes CRC32C verification
  → AnchorFrameContextV1

SESSION_MANIFEST_UUID_V1
  → SessionManifestChunkV1
  → bounded chunk reassembly/decompression/CRC
  → SessionManifestV1
```

처리 순서:

1. parser output을 AU 단위로 만든다.
2. PTS/DTS/duration을 읽는다.
3. UUID와 payload size를 검증한다.
4. timestamp를 exactly-one 규칙으로 검증한다.
5. context/manifest CRC를 검증한다.
6. encoded AU와 metadata를 하나의 envelope로 만든다.
7. decoder output을 normalized PTS와 stream-local AU ordinal로 해당 envelope와 결합한다.
8. MPEG-TS 90 kHz PTS wrap은 demux/parser의 unwrapped timeline 또는 명시적 wrap counter로 처리하고 raw 33-bit 값끼리 직접 비교하지 않는다.
9. PTS가 없거나 ambiguous하면 preview는 허용할 수 있지만 dataset frame 결합은 금지한다.

```rust
struct EncodedFrameEnvelope {
    transport: StreamIdentity,
    pts_ns: u64,
    dts_ns: Option<u64>,
    sync: SyncTimestampV1,
    anchor_context: Option<AnchorFrameContextV1>,
    manifest_chunks: Vec<SessionManifestChunkV1>,
    encoded_au: Bytes,
}

struct DecodedFrameEnvelope {
    transport: StreamIdentity,
    pts_ns: u64,
    capture_time_edge_ns: u64,
    image: DecodedImage,
    anchor_context: Option<AnchorFrameContextV1>,
    anchor_context_packet: Option<AnchorFrameContextPacketV1>,
    manifest_revision: Option<u64>,
}
```

Anchor 프레임은 decoded context뿐 아니라 exact `AnchorFrameContextPacketV1` bytes/CRC도 frame envelope와 staging에 보존한다. 이로써 외부 API와 replay가 schema/quality/validity를 포함한 모든 anchor 정보를 복원하고 원본 payload를 감사할 수 있다. 비-anchor 프레임의 두 context 필드는 `None`이다. Manifest를 받기 전에는 vector semantic을 해석하지 않고 preview/raw 저장만 허용한다.

### 14.8 Manifest 반복, late join 및 pending buffer

Manifest 삽입 조건:

- session 첫 IDR에서 chunk sequence 시작
- `MANIFEST_REPEAT_SEC` 주기 IDR에서 repeat sequence 시작
- revision 변경 후 첫 IDR에서 새 sequence 시작
- SRT reconnect 직후 force-key-unit IDR에서 sequence 시작
- 기본 `MANIFEST_MAX_CHUNKS_PER_AU=1`; 후속 AU로 분산

Receiver가 manifest보다 context를 먼저 받을 수 있으므로 bounded pending queue를 둔다. revision을 확인할 수 없는 context는 timeout 후 폐기한다. Manifest reassembly memory는 connection/session별 cap과 global cap을 모두 둔다.

### 14.9 Raw stream 저장과 replay identity

SRT stream ID는 MPEG-TS payload 자체의 일부가 아니므로 `.ts`만 저장하면 camera/session identity를 잃을 수 있다. Receiver는 각 connection/epoch별로 다음 `stream-envelope.json`을 함께 저장한다.

```json
{
  "accepted_at_utc": "2026-07-21T00:00:00Z",
  "listen_port": 10000,
  "raw_stream_id": "rmc1;...",
  "stream_id_fields": {"camera_id": "cam_front", "slot": 0},
  "stream_id_auth": "valid",
  "peer_address": "192.0.2.10:45000",
  "gstreamer_version": "1.22.x"
}
```

이는 per-frame timestamp sidecar가 아니다. 프레임 timestamp/state/action은 계속 영상 내부 SEI에서 복원한다. 각 segment에는 connection/epoch, first/last normalized PTS, first/last capture timestamp, byte length, SHA-256을 기록한 `segments/index.jsonl` entry를 원자적으로 commit한다. replay tool은 envelope, segment index와 TS segment를 함께 열고 hash 및 동일 bootstrap validation을 수행한다.

### 14.10 Receiver 외부 접근 API

HTTP:

```text
GET /v1/sessions/{session_id}/cameras
GET /v1/sessions/{session_id}/anchor
GET /v1/sessions/{session_id}/manifest
GET /v1/sessions/{session_id}/quality
GET /v1/sessions/{session_id}/streams
```

gRPC:

```text
ReceiverMetadata.ListCameras
ReceiverMetadata.GetAnchor
ReceiverMetadata.GetSessionManifest
ReceiverMetadata.GetSessionQuality
ReceiverMetadata.SubscribeSynchronizedSteps
```

`SynchronizedDatasetStep`은 편의용 flattened observation/action/auxiliary 외에 canonical `AnchorFrameContextV1` 전체와 수신한 exact `AnchorFrameContextPacketV1`을 함께 반환한다. 따라서 `anchor_frame_seq`, schema IDs, feature validity bitmap, action source quality, device별 timestamp/interpolation/clock quality, validity flags, invalid reason 및 original CRC-protected bytes를 손실 없이 조회할 수 있다. convenience mirror와 canonical context가 다르면 protocol violation으로 step을 invalid 처리한다. 각 `FrameReference`는 camera ID, stream epoch, normalized PTS, access-unit ordinal, capture timestamp와 storage/preview reference를 제공한다.

API의 anchor는 manifest validation이 완료된 authoritative anchor만 반환한다. 실시간 step API는 기본적으로 대용량 raw image bytes 대신 storage/preview reference를 반환하고 요청한 경우만 encoded image를 포함한다.

### 14.11 Dataset step 시각과 cadence

```text
t[k] = anchor SyncTimestampV1.capture_time_edge_ns
```

동일 AU의 validated anchor context observation/action을 해당 step에 사용한다. 기본 `DATASET_CADENCE_MODE=anchor_native`는 accepted anchor coded-picture AU마다 한 step을 생성하며 영상 자체를 보간하거나 합성하지 않는다.

LeRobot export의 nominal FPS는 manifest/episode에 기록하되, builder는 anchor inter-frame interval, 누락, jitter를 검증한다. 정확한 fixed grid가 필요한 배포에서는 `fixed_grid_nearest` 모드로 `episode_start + k / DATASET_FPS`에 가장 가까운 실제 anchor frame을 tolerance 안에서 선택하며 frame reuse와 synthetic frame 생성을 금지한다. tolerance를 넘은 grid point는 drop하거나 episode를 invalid 처리한다.

### 14.12 다른 카메라 매칭

각 required non-anchor camera에서 `t[k]`에 가장 가까운 실제 frame을 선택한다.

```text
abs(non_anchor.capture_time_edge_ns - t[k]) <= MAX_CAMERA_SKEW_MS
```

기본 정책:

```dotenv
MAX_CAMERA_SKEW_MS=20
MISSING_CAMERA_POLICY=drop_step
ALLOW_FRAME_REUSE=false
```

공통 timestamp는 노출 동시성을 보장하지 않는다. per-camera timestamp accuracy/calibration offset과 실제 skew를 quality report에 기록한다.

### 14.13 Dataset sample

```text
Dataset step k
├─ timestamp = t[k] - episode_start_edge_ns
├─ observation.images.<anchor>
├─ observation.images.<secondary cameras>
├─ observation.state = anchor context observation
├─ action = anchor context action
├─ optional auxiliary
└─ camera skew/device quality
```

### 14.14 Missing 및 conflict policy

- anchor timestamp/context missing: step 생성 불가
- required camera missing: 기본 drop step
- optional camera missing: manifest policy 적용
- 같은 non-anchor frame 재사용: 기본 금지
- timestamp 비단조/duplicate/zero: stream health error
- anchor inter-frame interval/cadence tolerance 초과: step 또는 episode quality policy 적용
- encoded AU PTS와 decoder frame correlation ambiguous: preview-only, dataset frame 금지
- transport identity와 manifest 불일치: session quarantine
- 미확인 manifest revision context: pending 후 timeout drop
- anchor가 아닌 stream의 context/manifest: protocol violation
- unknown SEI UUID: bounded size 검사 후 무시하고 metric 증가

---

## 15. 타임라인과 제어

### 15.1 범용 Timeline Model

Timeline은 특정 로봇 API가 아니라 manifest의 command schema를 사용한다.

```text
track
├─ device_id
├─ command_mode
├─ feature values
├─ keyframe time
└─ interpolation policy
```

### 15.2 실행 방식

예약 실행은 기본 요구사항이 아니다.

- Receiver가 timeline을 적절한 command rate로 샘플링한다.
- Generic Control Gateway로 명령을 전송한다.
- Adapter가 vendor command로 변환한다.
- 실제 dataset action은 명령 송신 시각이 아니라 Adapter의 effective action sample에서 얻는다.

### 15.3 명령 주기와 state 주기 분리

```text
Timeline command: 20~100 Hz, 장치 capability에 따라
State receive:    장치가 허용하는 고주기
Dataset:          anchor camera FPS, 기본 30 Hz
```

### 15.4 Local smoothing/executor

네트워크 지터가 동작 품질을 해치는 경우 Edge Control Gateway 또는 Adapter 내부에 다음을 MAY 구현한다.

- long-lived command stream
- 짧은 bounded command queue
- rate limiting
- trajectory interpolation
- watchdog
- hold/abort

이는 정확한 dataset 정렬을 위한 필수 기능이 아니라 실제 demonstration 품질과 안전을 위한 기능이다.

### 15.5 Control lease

동시에 여러 controller가 같은 장치를 제어하지 못하게 한다.

```text
NONE
LEROBOT
TIMELINE
MANUAL
SAFETY
```

Timeline 실행 중에는 control owner가 `TIMELINE`이어야 한다. LeRobot UI는 카메라/상태 조회만 허용한다.

---

## 16. Session과 Episode

### 16.1 Session ID

다음 상황에서 새 session을 시작한다.

- Edge Core 재시작
- Edge clock discontinuity
- embodiment schema 변경
- anchor camera 변경
- operator 명시적 새 session

카메라 reconnect는 같은 session에서 처리할 수 있다. 단순 SRT socket reconnect로 encoder PTS/timestamp 의미가 연속이면 epoch를 유지할 수 있고, encoder/pipeline/PTS/codec/timestamp source가 reset되면 `stream_epoch`와 manifest revision을 증가시킨다. anchor reconnect가 episode 중 발생하면 episode는 실패한다.

### 16.2 Episode freeze

Episode 시작 시 고정:

- anchor camera
- required camera set
- camera catalog revision
- embodiment/adapter schema
- observation/action schema
- task

### 16.3 Pre-roll/Post-roll

- episode 시작 전 camera/state buffer pre-roll
- timeline 종료 후 post-roll
- success/failure/reason 기록

---

## 17. 장애 처리

| 장애 | Edge 동작 | Receiver/Dataset 동작 |
|---|---|---|
| 비-anchor camera unplug | 해당 virtual timeout, stream 종료 | required면 step drop/episode policy |
| anchor camera unplug | 다른 UI/stream 유지, readiness false | 진행 episode abort |
| Adapter state 단절 | 영상 유지, context invalid | step drop |
| optional component 단절 | partial invalid 정책 | episode policy |
| SRT 단절 | UI 유지, reconnect; encoded timeline이 reset된 경우만 epoch 증가 | gap 기록, listener 유지 |
| stream ID 인증/slot 불일치 | 명시적 오류와 retry backoff | 연결 전 reject |
| manifest timeout | anchor 영상 유지 | preview-only, readiness false |
| manifest chunk/CRC 오류 | 다음 IDR에 반복 | revision 미활성화 |
| context CRC 오류 | preview 유지 | 해당 step drop |
| timestamp SEI 없음/zero | preview 가능 | 해당 AU sync 불가 |
| non-anchor context/manifest | protocol violation metric | stream/session quarantine |
| schema revision 변화 | session/episode 경계 강제 | 기존 episode 종료 |
| disk low/full | stream/UI 우선, optional Edge spool 제한 | recording 중단/episode abort, preview 유지 |
| control lease 상실 | command 중단/hold | episode abort |
| clock jump/reboot | 새 boot/session/epoch | 이전 session과 결합 금지 |

Edge의 optional encoded spool은 네트워크 단절 복구용이며 기본 `off`다. 활성화 시 byte/time quota와 oldest-first deletion을 사용하고 UI/capture thread를 block하지 않는다. Receiver 저장 공간이 부족하면 silent overwrite하지 않고 recording readiness를 false로 만들고 진행 episode를 명시적으로 종료한다.

---

## 18. API 개요

### 18.1 Edge Core API

- `GET /health/live`
- `GET /health/ready`
- `GET /v1/cameras`
- `GET /v1/anchor`
- `GET /v1/adapters`
- `GET /v1/embodiment`
- `GET /v1/clock-models`
- `POST /v1/control/lease`
- `DELETE /v1/control/lease/{id}`
- generic gRPC `ControlGateway.CommandStream`

### 18.2 Receiver API

- session/episode create/start/end
- timeline upload/validate/execute/pause/stop
- camera preview/health
- dataset build/validate/export
- manifest/quality/stream report
- `GET /v1/sessions/{id}/cameras`
- `GET /v1/sessions/{id}/anchor`
- `GET /v1/sessions/{id}/manifest`
- `GET /v1/sessions/{id}/quality`
- `GET /v1/sessions/{id}/streams`
- gRPC `ReceiverMetadata.SubscribeSynchronizedSteps`

Receiver API가 반환하는 anchor는 transport role hint가 아니라 manifest validation이 끝난 authoritative anchor다.

### 18.3 Adapter registration

Edge Core는 `/run/robot-adapters/*.sock` 또는 config endpoint를 감시한다. Adapter 연결/해제는 camera hotplug와 유사한 상태 머신으로 처리한다.

---

## 19. 저장 구조와 보존 정책

```text
/data/sessions/{session_id}/
├─ manifest.pb
├─ manifest.json
├─ manifest-revisions/{revision}.pb
├─ streams/
│  ├─ {camera_id}/epoch-0001/
│  │  ├─ stream-envelope.json
│  │  ├─ segments/index.jsonl
│  │  └─ segments/*.ts
│  └─ ...
├─ connection-events.jsonl
├─ episodes/episode-000001.json
├─ staging/
│  ├─ synchronized-steps.parquet
│  └─ quality.parquet
└─ logs/
```

원본 encoded stream을 보존하면 timestamp/context/manifest SEI를 포함한 채 재처리할 수 있다. raw high-rate telemetry는 선택 사항이다.

```dotenv
RAW_TELEMETRY_RECORDING=off
RAW_STREAM_RETENTION_DAYS=30
MIN_FREE_DISK_GB=20
DISK_FULL_POLICY=abort_recording_keep_preview
EDGE_SPOOL_MODE=off
EDGE_SPOOL_MAX_GB=5
```

- 삭제는 session/episode commit 상태와 retention policy를 확인한 뒤 수행한다.
- active segment와 manifest/envelope는 삭제하지 않는다.
- disk pressure는 metrics/health/API에 노출한다.
- dataset export가 완료됐더라도 raw 삭제는 별도 운영 정책으로 명시한다.

### 19.1 LeRobot export transaction과 버전 고정

Receiver의 raw stream, manifest, synchronized staging, quality record가 source of truth다. Dataset Builder가 만드는 LeRobot 디렉터리는 재생성 가능한 파생 산출물이며 ingest 서비스의 성공 조건이 아니다.

구현 계약:

1. LeRobot과 dataset/video dependency를 lockfile에서 정확한 버전으로 pin한다. reference 환경은 `LEROBOT_VERSION=0.6.0`이며, 향후 버전 변경은 export compatibility test와 schema diff를 통과한 별도 변경으로만 허용한다.
2. bare `pip install lerobot` 같은 floating dependency를 금지한다. Dataset Builder는 시작 시 설치된 package version과 `LEROBOT_VERSION`을 비교하고 불일치하면 readiness를 실패시킨다.
3. 공개 LeRobotDataset API만 사용한다. 구현 버전에 존재하는 create/add-frame/save-episode/finalize 계열 public API로 작성하고 private module path에 의존하지 않는다. 실제 API 명칭은 pinned version의 공식 문서와 import test로 고정한다.
4. 출력은 임시 경로에 작성하고 모든 episode 저장과 `finalize()`가 성공한 뒤 pinned-version loader로 전체 scan한다. feature shape, episode boundary, nominal FPS, video decode, timestamp monotonicity를 검증한 후 checksum/provenance manifest를 만들고 최종 경로로 atomic commit한다.
5. export 실패나 loader validation 실패 시 기존 committed dataset을 덮어쓰지 않으며 temp output은 quarantine한다. Receiver ingest/preview/raw recording은 계속된다.
6. provenance에는 source `session_id`, episode IDs, manifest/schema revision, camera catalog, Edge/Receiver/Dataset Builder build IDs, LeRobot version, cadence policy, quality thresholds, source segment hashes를 저장한다.
7. LeRobot v3 export에서는 state/action/timestamp와 auxiliary tabular data를 Parquet 계층으로, 카메라 영상을 MP4 계층으로, feature/episode/video chunk 정보를 metadata로 구성한다. 원본 SEI와 raw TS는 별도의 source archive에 유지한다.

Cadence 계약:

- `DATASET_CADENCE_MODE=anchor_native`는 Receiver staging 방식이다. 최종 LeRobot export에서 nominal 30 Hz를 선언하려면 episode의 실제 anchor cadence가 `ANCHOR_RATE_TOLERANCE_PCT`와 `ANCHOR_MAX_FRAME_INTERVAL_MS` 기준을 만족해야 한다.
- 기본 `LEROBOT_CADENCE_POLICY=reject_irregular`은 기준을 벗어난 episode export를 거부한다.
- `fixed_grid_nearest`를 선택한 경우에도 tolerance 안의 실제 frame만 사용한다. synthetic image 생성과 기본 frame reuse는 금지하며, 누락 grid point를 조용히 건너뛰면서 30 Hz라고 선언해서는 안 된다. 누락이 생기면 정책에 따라 episode를 reject하거나 명시적인 lower-rate export로 새 manifest를 생성한다.

```dotenv
LEROBOT_VERSION=0.6.0
LEROBOT_DATASET_FORMAT=v3
LEROBOT_CADENCE_POLICY=reject_irregular
LEROBOT_EXPORT_COMMIT_MODE=atomic
LEROBOT_VALIDATE_AFTER_EXPORT=true
```

---

## 20. 관측성과 성능

### 20.1 주요 메트릭

카메라별:

- capture/virtual/encoder FPS와 latency
- queue depth/drop/reconnect
- timestamp source/quality/offset/calibration revision
- timestamp SEI missing/zero/non-monotonic
- stream slot/epoch 상태

anchor:

- context serialization bytes와 metadata kbps
- interpolation wait/gap
- invalid context count
- action source quality
- manifest size/chunk/repeat/reassembly

Adapter별:

- sample rate/reconnect
- clock residual/drift/jump
- command feedback latency
- descriptor/schema revision

Receiver:

- listening/provisional/validated stream 수
- stream ID parse/auth/reject/port mismatch
- manifest wait/reassembly/CRC failures
- authoritative anchor validation failures
- camera skew p50/p95/max
- accepted/dropped steps
- disk free/ingest write latency/retention queue

### 20.2 권장 성능 목표

- virtual camera 추가 지연 p95: 한 video frame 이내
- normal path timestamp/context 누락: 0
- accepted state/action gap: 30 ms 이하
- camera skew: configured threshold 이하
- anchor context soft budget: 2 KiB/frame
- anchor context hard cap: 8 KiB/frame
- manifest chunk: 8 KiB 이하, total 256 KiB 이하, 기본 1 chunk/AU
- stream ID: 256 bytes 이하
- Receiver bootstrap: 1 GOP + manifest repeat window 내
- 구조 stress target: 8×720p30, 단 실제 지원 수는 USB/encoder capacity test로 결정

---

## 21. 보안과 안전

- SRT passphrase, stream ID HMAC key, mTLS key를 image/repository에 저장하지 않는다.
- SRT encryption을 사용할 때 `pbkeylen`을 `16/24/32` 중 하나로 명시한다. passphrase만 설정하고 기본 `no-key` 상태로 두어서는 안 된다.
- stream ID를 HMAC 없이 camera identity 증명으로 단독 신뢰하지 않는다.
- SRT stream ID는 인증되지만 비밀 채널로 간주하지 않는다. serial, 사용자명, 작업명 등 민감 정보를 넣지 않고 non-secret stable ID만 사용한다.
- HMAC/SRT/mTLS key rotation은 session boundary에서 수행한다. Receiver는 제한된 overlap 기간 동안 active+previous verification key ring을 지원할 수 있지만 새 연결은 active key로만 발급한다.
- Receiver↔Edge Control API는 mTLS와 authorization policy를 사용한다.
- Adapter UDS는 전용 group/mode로 제한한다.
- command는 lease, device limits, mode validation 및 local safety를 통과한다.
- 모든 protobuf/SEI/manifest/streamid 입력에 size/count/time limit를 적용한다.
- malformed stream은 decoder에 넘기기 전에 reject/quarantine한다.
- `privileged` Compose는 reference 편의 설정이며 운영에서는 device/capability를 최소화한다.
- production image는 base image digest를 pin하고 SBOM/vulnerability scan 결과를 release artifact로 남긴다.

---

## 22. Docker 및 Docker Compose 설계

### 22.1 Edge Compose 서비스

```text
edge-core
adapter-rby1          profile: rby1
additional robot/gripper/tool adapters
```

Edge Core는 vendor SDK를 포함하지 않는다. Adapter image만 해당 SDK dependency를 가진다.

### 22.2 Adapter socket volume

```text
adapter-sockets:/run/robot-adapters
```

각 Adapter는 고유 socket을 만든다. production에서는 shared group ownership과 mode를 명시한다.

### 22.3 Receiver Compose 서비스

```text
data-init
receiver
dataset-builder
```

`data-init`은 shared volume ownership을 non-root UID/GID에 맞춘다. Receiver는 slot별 SRT UDP port block과 HTTP/gRPC/metrics port를 expose한다. Dataset Builder는 별도 `.env.dataset-builder`와 exact LeRobot version contract를 사용하고, 장애가 Receiver ingest/preview에 전파되지 않는다. Compose의 `ports:` 범위는 env 변경에 따라 자동 확장되지 않으므로 `SRT_LISTEN_BASE_PORT`/`MAX_CAMERAS`를 바꾸면 Compose port range도 재생성하거나 host networking을 사용해야 한다.

### 22.4 호스트 전제

- Docker Engine/Compose plugin
- v4l2loopback kernel module
- `/dev/video*`, `/run/udev`, `/sys` 접근
- hardware encoder device가 있으면 `/dev/dri` 또는 vendor device
- GStreamer 1.24 이상 권장; 1.22/1.23은 custom multi-SEI codec fallback과 startup round-trip 통과가 필수
- Receiver UDP port block firewall/NAT 허용

### 22.5 startup self-test

```text
gst-inspect: srtsrc/srtsink/mpegtsmux/tsdemux/h264parse/h265parse/v4l2src/v4l2sink
동일 anchor AU의 timestamp+context+manifest 복수 SEI encode→mux→demux→parse→extract round trip
protobuf schema/protocol constants 일치
port range와 MAX_CAMERAS 일치
secret/config 존재 및 permission
state/data directory writable/free-space
```

필수 capability가 없으면 silent fallback이 아니라 readiness failure로 보고한다.

---

## 23. 환경변수

### 23.1 Edge Core

```dotenv
EMBODIMENT_ID=robot-cell-001
EDGE_INSTANCE_ID=edge-robot-cell-001
EMBODIMENT_CONFIG=/etc/robot-edge/embodiment.yaml
CAMERA_POLICY_CONFIG=/etc/robot-edge/camera-policy.yaml
ADAPTER_SOCKET_DIR=/run/robot-adapters

CAMERA_WIDTH=1280
CAMERA_HEIGHT=720
CAMERA_FPS=30
CAMERA_STRICT_PROFILE=true
CAMERA_FORMAT_PREFERENCE=MJPG,NV12,YUY2
CAMERA_HOTPLUG=true
STABLE_CAMERA_ID_COLLISION_POLICY=fail
CAMERA_SLOT_RECLAIM_POLICY=manual
CAMERA_SLOT_TOMBSTONE_DAYS=30
CAMERA_STREAM_EXCLUDE=serial:EXAMPLE_SKIP;name_regex:^Integrated Camera$
CAMERA_DISABLE=
ANCHOR_CAMERA_SELECTOR=serial:FRONT_CAM_001
ANCHOR_MISSING_POLICY=degraded

VIRTUAL_CAMERA_START=40
VIRTUAL_CAMERA_POOL_SIZE=16
VIRTUAL_CAMERA_TIMEOUT_MS=3000

VIDEO_CODEC=h264
VIDEO_ENCODER=auto
VIDEO_BITRATE_KBPS=4000
VIDEO_KEYINT_FRAMES=30
VIDEO_BFRAMES=0
MAX_ACTIVE_STREAMS=16

SRT_TARGET_HOST=192.168.30.20
SRT_BASE_PORT=10000
MAX_CAMERAS=16
SRT_LATENCY_MS=120
SRT_PASSPHRASE_FILE=/run/secrets/srt_passphrase
SRT_PBKEYLEN=32
SRT_STREAMID_HMAC_KEY_FILE=/run/secrets/srt_streamid_hmac_key
SRT_STREAMID_MAX_BYTES=256
SRT_RECONNECT_ON_SESSION_CHANGE=true

MANIFEST_REPEAT_SEC=3
MANIFEST_MAX_CHUNK_BYTES=8192
MANIFEST_MAX_TOTAL_BYTES=262144
MANIFEST_MAX_CHUNKS_PER_AU=1
MANIFEST_MAX_COMPRESSION_RATIO=16
ANCHOR_CONTEXT_BUDGET_BYTES=2048
ANCHOR_CONTEXT_MAX_BYTES=8192
ANCHOR_AU_HOLD_MAX_MS=25
FRAME_CONTEXT_MAP_MAX_ENTRIES=256

STATE_BUFFER_SEC=5
STATE_INTERPOLATION_WAIT_MS=15
STATE_MAX_GAP_MS=30
CLOCK_MAPPER_WINDOW_SEC=60
CLOCK_RESIDUAL_REJECT_MS=15
RAW_TELEMETRY_RECORDING=off
EDGE_SPOOL_MODE=off
EDGE_SPOOL_MAX_GB=5

EDGE_STATE_DIR=/var/lib/robot-edge
EDGE_API_BIND=0.0.0.0:8081
EDGE_GRPC_BIND=0.0.0.0:8082
EDGE_METRICS_BIND=0.0.0.0:9091
```

### 23.2 RB-Y1 Adapter 예시

```dotenv
ADAPTER_INSTANCE_ID=rby1-main
ADAPTER_SOCKET=/run/robot-adapters/rby1.sock
RB_Y1_ADDRESS=192.168.30.1:50051
RB_Y1_STATE_HZ=100
```

### 23.3 Receiver

```dotenv
EMBODIMENT_ID=robot-cell-001
EXPECTED_EDGE_INSTANCE_ID=
SRT_LISTEN_BASE_PORT=10000
MAX_CAMERAS=16
SRT_KEEP_LISTENING=true
SRT_LATENCY_MS=120
SRT_PASSPHRASE_FILE=/run/secrets/srt_passphrase
SRT_PBKEYLEN=32
SRT_STREAMID_HMAC_KEY_FILE=/run/secrets/srt_streamid_hmac_key
SRT_STREAMID_MAX_BYTES=256

EXPECTED_ANCHOR_CAMERA_ID=
MANIFEST_WAIT_TIMEOUT_SEC=10
MANIFEST_PENDING_MAX_FRAMES=300
MANIFEST_REASSEMBLY_MAX_BYTES=262144
RECEIVER_BOOTSTRAP_POLICY=preview_until_manifest

DATA_ROOT=/data
DATASET_FPS=30
DATASET_CADENCE_MODE=anchor_native
ANCHOR_RATE_TOLERANCE_PCT=5
ANCHOR_MAX_FRAME_INTERVAL_MS=50
MAX_CAMERA_SKEW_MS=20
MISSING_CAMERA_POLICY=drop_step
ALLOW_FRAME_REUSE=false
MANIFEST_MAX_COMPRESSION_RATIO=16
SEGMENT_DURATION_SEC=10
SEGMENT_HASH_ALGORITHM=sha256
RAW_STREAM_RETENTION_DAYS=30
MIN_FREE_DISK_GB=20
DISK_FULL_POLICY=abort_recording_keep_preview

EDGE_CONTROL_ENDPOINT=https://robot-host:8082
RECEIVER_API_BIND=0.0.0.0:8080
RECEIVER_GRPC_BIND=0.0.0.0:8083
RECEIVER_METRICS_BIND=0.0.0.0:9090
```

### 23.4 Dataset Builder

```dotenv
DATA_ROOT=/data
API_BIND=0.0.0.0:8090
LEROBOT_VERSION=0.6.0
LEROBOT_DATASET_FORMAT=v3
LEROBOT_CADENCE_POLICY=reject_irregular
LEROBOT_EXPORT_COMMIT_MODE=atomic
LEROBOT_VALIDATE_AFTER_EXPORT=true
LEROBOT_PUSH_TO_HUB=false
EXPORT_TEMP_ROOT=/data/.exports
```

---

## 24. 권장 저장소 구조

```text
robot-multicam-backend/
├─ Cargo.toml
├─ Cargo.lock
├─ crates/
│  ├─ edge-core/
│  ├─ receiver/
│  ├─ camera-discovery/
│  ├─ virtual-camera/
│  ├─ timebase/
│  ├─ metadata-codec/
│  ├─ stream-identity/
│  ├─ adapter-client/
│  └─ protocol/
├─ adapters/
│  ├─ rby1/
│  └─ template/
├─ proto/
│  ├─ adapter_api.proto
│  ├─ backend_api.proto
│  ├─ frame_metadata.proto
│  └─ receiver_api.proto
├─ python/dataset_builder/
├─ config/
│  ├─ embodiment.example.yaml
│  ├─ camera-policy.example.yaml
│  └─ protocol_constants.toml
├─ docker/
├─ docs/
├─ scripts/
├─ compose.edge.yaml
├─ compose.receiver.yaml
├─ REVIEW_REPORT.md
└─ README.md
```

---

## 25. 핵심 알고리즘 의사코드

### 25.1 카메라 reconcile 및 anchor 해석

```rust
async fn reconcile_devices(snapshot: Vec<CameraCandidate>) -> Result<()> {
    let policy = CameraPolicies::from_env()?;
    let mut active = Vec::new();

    for candidate in snapshot {
        if candidate.is_managed_virtual() || !candidate.is_supported_capture() {
            registry.report_filtered(candidate);
            continue;
        }

        let id = stable_camera_id(&candidate)?;
        if policy.disabled.matches(&candidate, &id) {
            registry.report_disabled(id, candidate);
            continue;
        }

        let mapping = mapping_store.allocate_or_reuse(&id)?;
        virtual_camera_manager.ensure(&mapping).await?;

        let stream_enabled = !policy.stream_excluded.matches(&candidate, &id);
        active.push((candidate, id, mapping, stream_enabled));
    }

    let anchor = resolve_exactly_one(&active, &policy.anchor_selector)?;
    if !anchor.stream_enabled {
        bail!("anchor camera cannot be stream-excluded");
    }

    for item in active {
        let role = if item.id == anchor.id { CameraRole::Anchor } else { CameraRole::Secondary };
        pipeline_manager.ensure_running(item, role).await?;
    }

    Ok(())
}
```

### 25.2 Generic sample 수집

```rust
fn on_adapter_sample(sample: DeviceSample, received_edge_ns: u64) {
    let mapped = clock_registry
        .for_source(&sample.source_clock_id)
        .map(sample.source_time_ns, received_edge_ns);

    for block in sample.feature_blocks {
        feature_rings
            .entry(block.feature_id)
            .push(TimedFeature {
                edge_time_ns: mapped.edge_time_ns,
                values: block.values,
                quality: mapped.quality.combine(block.quality),
            });
    }
}
```

### 25.3 Anchor context 생성

```rust
fn build_anchor_context(t: u64, manifest: &Manifest, rings: &FeatureRings) -> AnchorContext {
    let observation = manifest.observation_layout
        .resample_all(t, rings);
    let action = manifest.action_layout
        .resample_all(t, rings);

    AnchorContext {
        observation_state: observation.flattened,
        action: action.flattened,
        validity: observation.validity.merge(action.validity),
        max_gap_ns: observation.max_gap_ns.max(action.max_gap_ns),
        action_source_quality: action.source_quality,
        ..context_header()
    }
}
```

### 25.4 SEI 정책

```rust
fn enrich_access_unit(role: CameraRole, au: &mut AccessUnit, raw: RawFrameContext) -> Result<()> {
    au.insert_sei(SYNC_TIMESTAMP_UUID_V1, encode_sync_timestamp(raw.capture_time_edge_ns)?)?;

    if role == CameraRole::Anchor {
        let context = anchor_context_map.take_by_pts(raw.pts)?;
        let exact = context.encode_to_vec();
        ensure!(exact.len() <= config.anchor_context_max_bytes);
        let packet = AnchorFrameContextPacketV1 {
            schema_version: 1,
            payload_crc32c: crc32c(&exact),
            serialized_context: exact,
        };
        au.insert_sei(ANCHOR_CONTEXT_UUID_V1, packet.encode_to_vec())?;
        maybe_insert_manifest_chunks(au)?;
    }
    Ok(())
}
```

### 25.5 SRT stream ID와 Receiver bootstrap

```rust
fn on_caller_connecting(port: u16, raw: &str) -> Result<ProvisionalIdentity> {
    ensure!(raw.as_bytes().len() <= config.streamid_max_bytes);
    let id = StreamIdV1::parse_canonical(raw)?;
    verify_hmac(&id, secrets.streamid_hmac_key())?;
    ensure!(id.embodiment_id == config.expected_embodiment_id);
    ensure!(id.slot < config.max_cameras);
    ensure!(port == config.listen_base_port + id.slot as u16);
    ensure_supported_codec(id.codec)?;
    connection_registry.reserve_unique(&id)?;
    Ok(id.into_provisional())
}

fn validate_manifest(stream: &ProvisionalStream, manifest: &SessionManifestV1) -> Result<()> {
    ensure!(manifest.session_id == stream.session_id);
    ensure!(manifest.edge_boot_id == stream.edge_boot_id);
    ensure!(manifest.edge_instance_id == stream.edge_instance_id);
    ensure!(manifest.embodiment_id == stream.embodiment_id);
    ensure!(manifest.anchor_camera_id == stream.camera_id);

    let camera = manifest.camera(&stream.camera_id)?;
    ensure!(camera.stream_slot == stream.slot);
    ensure!(camera.stream_epoch == stream.epoch);
    ensure!(camera.role == CameraRole::Anchor);
    ensure!(camera.transport_port == stream.listen_port as u32);
    ensure!(camera.video_codec == stream.codec);
    Ok(())
}
```

### 25.6 SEI 추출과 manifest reassembly

```rust
fn inspect_access_unit(buffer: &gst::BufferRef, au_ordinal: u64) -> Result<EncodedFrameEnvelope> {
    let pts = require_normalized_pts(buffer.pts())?;
    let mut out = EncodedFrameEnvelope::from_correlation(pts, au_ordinal, buffer.dts());

    for meta in sei_user_data_metas(buffer) {
        match meta.uuid() {
            SYNC_TIMESTAMP_UUID_V1 => out.set_sync(decode_bounded(meta.data())?)?,
            ANCHOR_CONTEXT_UUID_V1 => {
                let packet: AnchorFrameContextPacketV1 = decode_bounded(meta.data())?;
                verify_crc32c(&packet.serialized_context, packet.payload_crc32c)?;
                out.anchor_context = Some(decode_bounded(&packet.serialized_context)?);
            }
            SESSION_MANIFEST_UUID_V1 => {
                out.manifest_chunks.push(decode_bounded(meta.data())?);
            }
            _ => metrics::unknown_sei_uuid().inc(),
        }
    }

    ensure_exactly_one_sync_timestamp(&out)?;
    Ok(out)
}
```

### 25.7 Receiver 그룹 생성

```rust
fn assemble_step(anchor: DecodedFrame, session: &ValidatedSession) -> Option<DatasetStep> {
    let manifest = session.active_manifest()?;
    if anchor.camera_id != manifest.anchor_camera_id { return None; }

    let t = anchor.capture_time_edge_ns;
    let context = anchor.anchor_context?
        .require_revision(manifest.manifest_revision)?
        .require_valid()?;

    let mut images = HashMap::from([(anchor.camera_id.clone(), anchor.image)]);
    let mut skew = HashMap::new();

    for camera_id in manifest.required_non_anchor_cameras() {
        let frame = session.queue(camera_id).nearest_without_reuse(t)?;
        let dt = signed_diff(frame.capture_time_edge_ns, t);
        if dt.unsigned_abs() > config.max_camera_skew_ns { return None; }
        images.insert(camera_id.clone(), frame.image);
        skew.insert(camera_id.clone(), dt);
    }

    Some(DatasetStep {
        time_ns: t,
        images,
        observation_state: context.observation_state,
        action: context.action,
        auxiliary: context.auxiliary,
        camera_skew_ns: skew,
    })
}
```

---

## 26. RB-Y1 Adapter 참조 구현

RB-Y1 관련 로직은 `adapters/rby1`에만 존재한다.

### 26.1 State 매핑

RB-Y1 Adapter는 사용 중인 SDK/protobuf 버전을 startup self-test로 확인한 뒤 다음을 canonical feature로 변환한다.

```text
RB-Y1 actual position      → *.joint.position
RB-Y1 actual velocity      → *.joint.velocity
RB-Y1 target_position      → *.joint.effective_target_position
RB-Y1 target_velocity      → *.joint.effective_target_velocity
RobotState timestamp       → source clock timestamp
joint time_since_last_update → adapter 내부 sample time 보정
```

Core는 위 필드의 RB-Y1 이름이나 위치를 알지 못한다.

### 26.2 Command 매핑

Generic command mode를 RB-Y1 SDK의 command/stream API로 변환한다. control hold, minimum time, velocity/acceleration limit은 Adapter 설정이다.

### 26.3 Version 검증

Adapter는 다음을 self-test하고 descriptor/capability report로 제공한다.

- SDK/protobuf version
- state timestamp 단조성
- joint order
- target update pattern
- 실제 state 수신률
- command feedback capability

---

## 27. 새 로봇 또는 그리퍼 추가 절차

### 27.1 구현 절차

1. `adapters/template`를 복사한다.
2. 벤더 SDK dependency는 새 Adapter image에만 추가한다.
3. `HardwareAdapter` gRPC 서비스를 구현한다.
4. 장치와 feature descriptor를 정의한다.
5. source clock type과 timestamp quality를 선언한다.
6. state/effective action stream을 구현한다.
7. command mode와 feedback을 구현한다.
8. adapter-specific self-test를 구현한다.
9. Compose에 서비스와 shared socket volume을 추가한다.
10. `embodiment.yaml`에 device와 vector layout을 추가한다.
11. generic integration test fixture를 통과시킨다.

### 27.2 Core 수정이 허용되는 경우

다음 경우에만 Core 확장을 검토한다.

- 새로운 보간 방식이 범용적으로 필요함
- 새로운 dtype/shape가 canonical schema에 필요함
- 새로운 camera source class가 필요함
- Adapter API 버전 자체를 확장해야 함

특정 벤더 필드 하나를 지원하기 위해 Core에 조건문을 추가해서는 안 된다.

---

## 28. 테스트 전략

### 28.1 Unit Test

- camera selector/exclude/anchor precedence
- anchor selector zero/multiple match
- stable camera ID
- virtual slot persistence
- Clock Mapper offset/drift/jump
- feature layout canonicalization
- linear/ZOH/nearest resampling
- Adapter descriptor compatibility
- SyncTimestamp encode/decode
- AnchorContext encode/decode/CRC
- SEI insert/extract round trip
- fixed SEI UUID/protocol constants 일치
- exact-byte CRC32C 검증/변조 검출
- canonical SRT stream ID encode/decode/HMAC
- manifest chunk/reassembly/size/time/compression-ratio limits
- schema ID canonical JSON hash와 validity bitmap bit order
- PTS missing/duplicate/wrap 및 AU ordinal correlation
- anchor AU hold queue timeout/overflow/orphan cleanup
- multicamera nearest matching/no reuse와 anchor cadence modes

### 28.2 Synthetic Integration Test

```text
videotestsrc 30 Hz × 3
mock robot adapter 100 Hz
mock gripper adapter 200 Hz
anchor full context
secondary timestamp-only
SRT loopback
receiver dataset assembly
```

검증:

- 모든 AU에 exactly one timestamp SEI
- anchor AU에만 context SEI
- secondary AU에 context SEI가 절대 없음
- observation/action vector가 manifest layout과 일치
- anchor state 보간 오차 제한
- branch stall 격리
- port=`base+slot` 및 duplicate connection reject
- transport role hint와 authoritative manifest 교차 검증
- manifest 전 preview-only와 late-join recovery
- session 변경 시 reconnect/new stream ID
- raw TS + stream-envelope + segment index/hash replay
- 1.22 custom codec fallback와 1.24+ multiple-SEI path conformance

### 28.3 Adapter Contract Test

모든 Adapter는 공통 test suite를 통과해야 한다.

- descriptor schema validation
- sample timestamp monotonicity
- reconnect
- command/feedback correlation
- source clock mapping quality
- feature dimension stability

### 28.4 Hardware Test

- 카메라 1/N대
- 동일 모델과 serial 없는 카메라
- unplug/replug
- excluded/disabled camera
- anchor unplug/reconnect
- LeRobot/OpenCV virtual camera open
- receiver restart
- Adapter restart
- network loss/delay
- disk low/full 및 retention
- hardware encoder session exhaustion/USB bandwidth overload
- relay가 SEI를 보존/제거하는 conformance test
- timestamp-only secondary 스트림의 duplicate/PTS wrap/segment replay
- 로봇+외부 그리퍼 조합

### 28.5 Dataset Validation

- 10분 이상 연속 recording
- accepted step마다 anchor context 존재
- required cameras 존재
- timestamp 단조 증가와 anchor cadence tolerance
- state/action dimension/semantic 일치
- visual pose와 state trajectory 샘플 검증
- exact pinned LeRobot version 확인
- temp export → finalize → loader 전체 scan → checksum → atomic commit 성공
- cadence가 tolerance 밖이면 nominal 30 Hz export 거부 또는 명시적 lower-rate manifest 생성

---

## 29. 수용 기준

### 29.1 기능

- [ ] 연결된 모든 지원 logical camera가 자동 등록된다.
- [ ] 각 활성 카메라에 고유 virtual camera가 생성된다.
- [ ] `CAMERA_STREAM_EXCLUDE` 카메라는 UI에는 남고 송출되지 않는다.
- [ ] anchor가 환경변수로 정확히 한 카메라로 해석된다.
- [ ] anchor와 exclude/disable 충돌은 명시적 오류다.
- [ ] 모든 송출 AU에 exactly one timestamp SEI가 있다.
- [ ] anchor AU에만 CRC-protected observation/action context packet이 있다.
- [ ] non-anchor AU에는 timestamp 이외의 per-frame semantic metadata가 없다.
- [ ] 로봇/그리퍼 Adapter를 Core 수정 없이 추가할 수 있다.
- [ ] state/action이 anchor capture time으로 재샘플링된다.
- [ ] 각 camera가 독립 SRT port와 stable slot을 사용한다.
- [ ] Receiver가 stream ID+port로 camera를 provisional 식별한다.
- [ ] `SessionManifestV1.anchor_camera_id`가 최종 authoritative anchor다.
- [ ] stream role hint, manifest, slot/epoch/codec/port가 교차 검증된다.
- [ ] Receiver가 decoder 전에 timestamp/context/manifest SEI를 추출한다.
- [ ] manifest late join과 bounded pending buffer가 동작한다.
- [ ] Receiver가 anchor 기준 멀티카메라 dataset step을 생성한다.
- [ ] Receiver API로 camera catalog, anchor, manifest, quality, synchronized step을 조회할 수 있다.
- [ ] raw TS 저장 시 connection-level `stream-envelope.json`과 segment index/hash가 보존된다.
- [ ] anchor AU hold/correlation failure가 잘못된 state/action 결합이 아니라 invalid/drop으로 처리된다.
- [ ] dataset cadence mode가 실제 anchor frame만 사용하며 synthetic image를 만들지 않는다.
- [ ] 송출/Receiver 장애 시 LeRobot virtual camera가 유지된다.
- [ ] Docker Compose로 Edge/Receiver/Adapter를 실행할 수 있다.

### 29.2 비기능

- [ ] Edge Core/Receiver에 특정 vendor SDK dependency가 없다.
- [ ] Adapter/camera stream crash가 다른 UI/pipeline을 중단하지 않는다.
- [ ] queue/ring/pending/reassembly/spool이 모두 bounded다.
- [ ] restart/hotplug 후 stable camera mapping이 유지되고 collision은 fail closed 처리된다.
- [ ] 제거된 camera slot은 tombstone/reclaim policy 없이 다른 camera에 자동 재사용되지 않는다.
- [ ] secrets가 image/repository에 포함되지 않는다.
- [ ] stream ID HMAC와 port-slot 검증이 운영 모드에서 활성화된다.
- [ ] manifest/context size, CRC, timeout, compression-ratio 및 DoS limit가 적용된다.
- [ ] control lease와 safety validation이 있다.
- [ ] disk pressure가 silent data loss가 아니라 readiness/episode 상태로 반영된다.
- [ ] GStreamer/plugin/SEI round-trip self-test가 readiness에 포함된다.
- [ ] manifest/schema/protocol constants revision이 재현 가능하다.
- [ ] Dataset Builder가 exact LeRobot version을 검증하고 export를 temp/finalize/load-scan/atomic-commit 순서로 수행한다.
- [ ] irregular anchor cadence를 nominal 30 Hz로 조용히 오표기하지 않는다.

---

## 30. 구현 단계

### Phase 1 — Generic camera path

- discovery
- virtual camera
- timestamp-only SEI
- SRT receiver

### Phase 2 — Adapter framework

- Adapter API
- mock adapter
- generic Clock Mapper
- Embodiment Manifest

### Phase 3 — Anchor context

- anchor env resolution
- feature resampling
- anchor-only context SEI
- manifest insertion

### Phase 4 — RB-Y1 reference Adapter

- state/effective target mapping
- command mapping
- self-test

### Phase 5 — Component extension

- template Adapter
- sample gripper Adapter fixture
- composite observation/action schema

### Phase 6 — Timeline/Dataset

- generic control gateway
- episode/session
- LeRobot builder

### Phase 7 — 운영화

- Docker/Compose
- security
- metrics
- fault/stress tests
- operations docs

---

## 31. 구현 제출물

AI 코딩 에이전트는 다음을 모두 생성한다.

1. Generic Rust workspace
2. Adapter API protobuf와 generated code
3. timestamp/context/manifest SEI codec
4. generic camera discovery/hotplug/stable mapping
5. virtual camera manager
6. generic timebase/clock mapper
7. embodiment/feature resampler
8. generic Edge Control Gateway
9. Receiver와 timeline API
10. Python LeRobot dataset builder
11. RB-Y1 Adapter
12. Adapter template와 contract test
13. Dockerfile 및 Compose
14. `.env.example`
15. host bootstrap/verification script
16. unit/integration/hardware test scaffold
17. protocol/operations/troubleshooting 문서
18. sample session fixture와 end-to-end smoke test
19. Receiver transport/bootstrap API 및 replay tool
20. protocol constants, validation script, four-pass review report
21. disk retention/spool/capacity test scaffold
22. AU correlation/cadence/segment replay conformance fixtures
23. 선택적 H.264 pass-through Web Relay와 gRPC/HLS/SSE 동시-access test

---

## 32. 최종 설계 요약

```text
모든 지원 물리 카메라 자동 탐색
    ↓
카메라별 stable ID + virtual slot
    ↓
한 번 캡처하여 tee
    ├─ 즉시 virtual camera → 기존 LeRobot UI
    └─ low-latency encode
         ├─ 모든 카메라: 공통 Edge timestamp SEI
         └─ 환경변수로 지정된 anchor만:
              ├─ 로봇/그리퍼/부품 Adapter state 수집
              ├─ anchor capture 시각으로 재샘플링
              └─ observation/action context SEI

수신 서버
    ↓
anchor timestamp/context 추출
    ↓
비-anchor timestamp로 nearest frame 정렬
    ↓
anchor observation/action을 authoritative sample로 사용
    ↓
30 Hz LeRobotDataset 생성
```

이 구조는 특정 로봇이나 그리퍼 조합에 종속되지 않는다. RB-Y1은 독립 Adapter 중 하나이며, 공통 시간축과 카메라 동기화는 Generic Edge Core의 책임이다. 모든 카메라에는 timestamp만 넣고 전체 state/action은 anchor에만 넣어 성능, 일관성, 확장성을 동시에 확보한다.


---

## 부록 A. Receiver 부트스트랩 핵심 요약

```text
카메라별 독립 SRT connection
  → listen port + canonical stream ID/HMAC 검증
  → provisional camera/session/slot/epoch 등록
  → MPEG-TS/codec/AU probing
  → anchor 후보에서 manifest chunks 추출
  → SessionManifest CRC/schema 검증
  → manifest.anchor_camera_id로 authoritative anchor 확정
  → 모든 camera descriptor와 실제 connection 교차 검증
  → anchor context + required camera readiness
  → synchronized dataset steps 생성
```

Manifest 전에는 preview/raw capture만 가능하고 dataset은 생성하지 않는다. `role=anchor`는 hint이며 manifest와 불일치하면 거부한다.

## 부록 B. 공식 API 구현 전제

- GStreamer SRT source/sink는 stream ID 속성을 제공한다.
- listener의 caller 연결 callback에서 caller stream ID를 읽고 연결을 수락/거부할 수 있다.
- H.264/H.265 parser output은 AU alignment로 만들 수 있다.
- GStreamer 1.22 이상은 User Data Unregistered SEI codec parser 구조를 제공하며, parser element의 복수 unregistered SEI 지원은 1.24 이상을 권장한다.
- 실제 배포 image는 plugin/version 차이를 startup round-trip test로 검증한다.

본 문서의 transport 규범은 `docs/TRANSPORT_BOOTSTRAP.md`, wire 상수는 `docs/PROTOCOL_CONSTANTS.md`와 `config/protocol_constants.toml`을 함께 따른다.

## 부록 C. 선택적 Web Relay

외부 application용 canonical interface는 Receiver의 `ReceiverMetadata` gRPC다. browser/VLC URL이 필요하면 별도 `web-relay` container가 authoritative active session을 발견하고 `SubscribeSynchronizedSteps(include_encoded_images=true)`를 정확히 한 번 구독한다.

```text
Receiver :8083 ──gRPC synchronized H.264/metadata──► Web Relay :8091
       └──기존 external gRPC 유지                    ├── HLS MPEG-TS
                                                    └── bounded-history SSE
```

Relay는 H.264를 decode/encode하지 않고 `h264parse → mpegtsmux → hlssink`로 remux한다. camera별 appsrc, HLS file/window, SSE broadcast/history와 tmpfs는 bounded이며 slow viewer는 preview event를 drop할 수 있지만 Receiver ingest를 block하지 않는다. HLS pass-through에는 in-band SEI가 남을 수 있으므로 HLS와 SSE는 같은 metadata 보안 경계로 취급한다. 기본 사내망은 평문 8083/8091 direct access이고, TLS/auth reverse proxy나 VPN은 선택 배포다.
