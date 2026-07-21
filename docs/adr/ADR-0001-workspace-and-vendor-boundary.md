# ADR-0001: Generic workspace와 vendor SDK 경계

- 상태: Accepted
- 날짜: 2026-07-22

Rust의 Edge/Receiver/common crates는 vendor-neutral protobuf만 사용한다. 공식 RB-Y1 SDK는 Python `adapters/rby1/mapping.py`와 RB-Y1 image에만 설치한다. 다른 장치는 독립 Adapter process와 UDS gRPC endpoint로 연결한다.

이 결정으로 vendor ABI failure가 카메라 및 Receiver process를 오염시키지 않으며, gripper/base/tool 추가가 Generic Core rebuild를 요구하지 않는다. `scripts/verify-vendor-boundary.py`가 이 경계를 회귀 검사한다.
