#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive="${root}/validation/runtime/archive-conformance"
client_python="${METADATA_CLIENT_PYTHON:-python3}"
container="robot-receiver-metadata-validation-$$"
temporary="$(mktemp -d -t robot-metadata-client-XXXXXX)"
session_id="$(python3 - "${archive}/stream-envelope.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    print(json.load(stream)["stream_id_fields"]["session_id"])
PY
)"

cleanup() {
  status=$?
  if [[ ${status} -ne 0 ]]; then
    docker logs "${container}" >&2 || true
  fi
  docker rm -f "${container}" >/dev/null 2>&1 || true
  rm -rf -- "${temporary}"
  return "${status}"
}
trap cleanup EXIT

"${client_python}" -c 'import grpc, google.protobuf' >/dev/null || {
  echo "metadata client Python is missing grpc/protobuf: ${client_python}" >&2
  exit 2
}

docker run -d --name "${container}" \
  --network host \
  --user 10001:10001 \
  --read-only \
  --tmpfs /tmp:size=128m,mode=1777 \
  --tmpfs /data:uid=10001,gid=10001,mode=0770,size=2g \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -e HOME=/tmp \
  -e XDG_CACHE_HOME=/tmp/.cache \
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
  -v "${root}/config:/etc/robot-receiver:ro" \
  -v "${root}/secrets/srt_passphrase.txt:/run/secrets/srt_passphrase:ro" \
  -v "${archive}/hmac-key.bin:/run/secrets/srt_streamid_hmac_key:ro" \
  robot-multicam-receiver:local serve >/dev/null

for _ in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:19090/readyz >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done
curl -fsS http://127.0.0.1:19090/readyz >/dev/null

env SYNTHETIC_SRT_SLEEP_US=500000 \
  timeout 30s "${root}/scripts/send-synthetic-srt-fixture.sh" >/dev/null &
sender_pid=$!
snapshot_ready=false
for _ in $(seq 1 30); do
  if "${client_python}" "${root}/scripts/receiver-metadata-client.py" \
    --endpoint 127.0.0.1:18083 snapshot --session "${session_id}" \
    > "${temporary}/snapshot.json" 2>/dev/null; then
    snapshot_ready=true
    break
  fi
  sleep 0.2
done
if [[ "${snapshot_ready}" != "true" ]]; then
  "${client_python}" "${root}/scripts/receiver-metadata-client.py" \
    --endpoint 127.0.0.1:18083 snapshot --session "${session_id}"
  exit 1
fi
python3 - "${temporary}/snapshot.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    snapshot = json.load(stream)
assert snapshot["anchor"]["anchor_camera_id"] == "synthetic-anchor"
assert snapshot["manifest"]["cameras"][0]["stable_camera_id"] == "synthetic-anchor"
PY

"${client_python}" "${root}/scripts/receiver-metadata-client.py" \
  --endpoint 127.0.0.1:18083 watch --session "${session_id}" \
  --dump-dir "${temporary}/h264" --vectors --max-steps 1 --stream-timeout 30 \
  > "${temporary}/step.json" &
watch_pid=$!
wait "${sender_pid}"
wait "${watch_pid}"

test -s "${temporary}/h264/synthetic-anchor.h264"
python3 - "${temporary}/step.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    step = json.loads(stream.readline())
assert step["valid"]
assert step["observation_length"] == 1
assert step["action_length"] == 1
assert step["frames"][0]["encoded_bytes"] > 0
assert len(step["anchor_context_packet_sha256"]) == 64
PY

if docker logs "${container}" 2>&1 | grep -Eq 'rejecting access unit|authentication failed|metadata bootstrap/synchronization rejected'; then
  docker logs "${container}" >&2
  exit 1
fi

echo "Receiver metadata client PASS: snapshot, vectors, synchronized H.264 AU dump"
