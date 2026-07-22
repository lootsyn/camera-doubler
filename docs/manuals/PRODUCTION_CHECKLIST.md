# Production 배포 체크리스트

## 보안/구성

- [ ] dev certificate가 아닌 production PKI를 사용한다.
- [ ] CA private key는 Edge/Receiver에 배포하지 않는다.
- [ ] SRT passphrase/HMAC key는 Edge/Receiver에서 동일하고 file mount만 사용한다.
- [ ] gRPC 8083과 metrics 9090/9091은 VPN/사설망/source firewall 뒤에 있다.
- [ ] Web Relay를 쓰면 TCP 8091, CORS, HLS의 in-band SEI 노출 경계를 검토했다.
- [ ] SRT UDP port range는 지정 Edge IP만 허용한다.
- [ ] 실제 env/config/secret/runtime가 `git check-ignore`에 잡힌다.
- [ ] container가 non-root, read-only, cap-drop, no-new-privileges로 실행된다.
- [ ] image digest, SDK, LeRobot, Cargo/Python lock이 승인 revision과 일치한다.

## Physical Edge

- [ ] `/dev/video*` logical grouping과 serial/stable ID를 기록했다.
- [ ] anchor selector가 정확히 한 camera이며 exclude/disable과 충돌하지 않는다.
- [ ] v4l2loopback pool, label, exclusive caps와 UI open을 확인했다.
- [ ] unplug/replug 후 stable ID/slot이 유지되고 collision은 fail closed한다.
- [ ] 목표 camera 수에서 USB controller, encoder session, CPU/GPU, memory를 측정했다.
- [ ] 공통 visual event로 camera timestamp offset/skew p95/max를 측정했다.

## Robot/Adapter

- [ ] exact vendor SDK version과 model/DOF/joint order를 확인했다.
- [ ] state clock domain과 probe mapping 근거가 있다.
- [ ] requested command와 effective target field를 실제 hardware에서 비교했다.
- [ ] E-stop/protective stop/disconnect/lease expiry 시 command가 fail closed한다.
- [ ] 실제 motion test는 안전 담당자와 낮은 범위로 승인됐다.

## Receiver/Dataset

- [ ] Receiver volume의 free-space, throughput, retention, backup을 측정했다.
- [ ] 모든 camera의 signed stream ID/port/epoch와 authoritative manifest를 확인했다.
- [ ] external metadata client로 catalog/anchor/quality/step을 조회했다.
- [ ] H.264 AU를 재생하고 frame skew/metadata vector를 같은 step에서 확인했다.
- [ ] Relay 사용 시 VLC/hls.js HLS와 SSE history/live metadata를 같은 PTS/epoch/ordinal로 확인했다.
- [ ] Relay-off/on CPU/RSS/network, viewer 수, HLS tmpfs/history 상한을 실제 bitrate로 측정했다.
- [ ] irregular cadence가 30 Hz로 조용히 오표기되지 않는다.
- [ ] temp/finalize/full load scan/checksum/provenance/atomic commit을 통과했다.
- [ ] raw envelope/index/hash replay가 deterministic step hash를 재현한다.

## Fault/복구

- [ ] network loss와 Receiver restart 중 Edge local virtual UI가 유지된다.
- [ ] anchor/secondary unplug, Adapter restart, disk low/full을 실행했다.
- [ ] key rotation을 session boundary에서 rehearsal했다.
- [ ] Edge camera mapping state와 Receiver data를 별도 backup했다.
- [ ] `down --volumes` 금지와 복구 담당자/절차가 운영 문서에 있다.

## Release evidence

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
python3 scripts/validate-package.py
python3 scripts/verify-vendor-boundary.py
./scripts/run-synthetic-roundtrip.sh
./scripts/run-srt-reconnect-test.sh
METADATA_CLIENT_PYTHON="$PWD/.venv-tools/bin/python" ./scripts/run-metadata-client-test.sh
METADATA_CLIENT_PYTHON="$PWD/.venv-tools/bin/python" ./scripts/run-web-relay-test.sh
docker compose -f compose.receiver.yaml config -q
docker compose --profile web -f compose.receiver.yaml config -q
docker compose --profile rby1 -f compose.edge.yaml config -q
```

- [ ] `docs/audit/ACCEPTANCE_EVIDENCE.csv`와 `validation/final_release_audit.json`을 현 배포 revision으로 갱신했다.
- [ ] SBOM과 vulnerability scan을 최종 image digest로 재생성했다.
- [ ] FAIL/NOT_IMPLEMENTED/BLOCKED_ENVIRONMENT가 0이다.
- [ ] 남은 BLOCKED_HARDWARE를 production 승인자가 명시적으로 검토했다.
