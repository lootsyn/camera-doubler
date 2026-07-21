# ADR-0003: 제어 mTLS와 session-boundary key rotation

- 상태: Accepted
- 날짜: 2026-07-22

Edge Control Gateway는 client CA를 요구하는 mTLS server로만 기동한다. command는 lease/schema/shape/finite/range 검증 뒤 해당 device의 Adapter UDS로 전달한다. 인증서와 SRT/HMAC key는 Compose secret으로만 주입한다.

HMAC/SRT/mTLS key 교체는 session boundary에서 수행해 stream epoch와 manifest를 함께 갱신한다. 기존 session을 새 key로 조용히 계속하지 않으며, 새 session 시작 전 Receiver와 Edge secret을 원자적으로 교체한다.
