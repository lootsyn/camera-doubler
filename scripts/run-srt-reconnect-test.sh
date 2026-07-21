#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive_root="${repo_root}/validation/runtime/archive-conformance"
container_name="robot-receiver-srt-validation-$$"

for command in docker gst-launch-1.0 python3 curl; do
  command -v "${command}" >/dev/null || {
    echo "missing required command: ${command}" >&2
    exit 2
  }
done

for path in \
  "${archive_root}/segments/synthetic.ts" \
  "${archive_root}/stream-envelope.json" \
  "${archive_root}/hmac-key.bin" \
  "${repo_root}/secrets/srt_passphrase.txt"; do
  test -f "${path}" || {
    echo "missing required fixture: ${path}" >&2
    exit 2
  }
done

cleanup() {
  docker rm -f "${container_name}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

stream_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["raw_stream_id"])' "${archive_root}/stream-envelope.json")"
passphrase="$(tr -d '\r\n' < "${repo_root}/secrets/srt_passphrase.txt")"

docker run -d --name "${container_name}" \
  --network host \
  --user 10001:10001 \
  --read-only \
  --tmpfs /tmp:size=64m,mode=1777 \
  --tmpfs /data:uid=10001,gid=10001,mode=0770,size=2g \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -e EMBODIMENT_ID=synthetic-cell \
  -e EXPECTED_EDGE_INSTANCE_ID=synthetic-edge \
  -e PROTOCOL_CONSTANTS_CONFIG=/etc/robot-receiver/protocol_constants.toml \
  -e SRT_LISTEN_BASE_PORT=10000 \
  -e MAX_CAMERAS=1 \
  -e SRT_LATENCY_MS=120 \
  -e SRT_PASSPHRASE_FILE=/run/secrets/srt_passphrase \
  -e SRT_STREAMID_HMAC_KEY_FILE=/run/secrets/srt_streamid_hmac_key \
  -e SRT_PBKEYLEN=32 \
  -e DATA_ROOT=/data \
  -e MIN_FREE_DISK_GB=1 \
  -e RECEIVER_METRICS_BIND=127.0.0.1:19090 \
  -e RECEIVER_GRPC_BIND=127.0.0.1:18083 \
  -v "${repo_root}/config:/etc/robot-receiver:ro" \
  -v "${repo_root}/secrets/srt_passphrase.txt:/run/secrets/srt_passphrase:ro" \
  -v "${archive_root}/hmac-key.bin:/run/secrets/srt_streamid_hmac_key:ro" \
  robot-multicam-receiver:local serve >/dev/null

for _ in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:19090/readyz >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
curl -fsS http://127.0.0.1:19090/readyz >/dev/null

srt_uri="srt://127.0.0.1:10000?mode=caller&latency=120&pbkeylen=32&passphrase=${passphrase}&streamid=${stream_id}"
for _attempt in 1 2; do
  timeout 20s gst-launch-1.0 -q \
    filesrc location="${archive_root}/segments/synthetic.ts" \
    ! srtsink uri="${srt_uri}" wait-for-connection=true
  sleep 0.5
  docker inspect -f '{{.State.Running}}' "${container_name}" | grep -qx true
done

if docker logs "${container_name}" 2>&1 | grep -Eq 'rejecting access unit|authentication failed|metadata bootstrap/synchronization rejected'; then
  docker logs "${container_name}" >&2
  exit 1
fi

echo "encrypted SRT reconnect PASS: authenticated stream accepted twice; Receiver remained ready"
