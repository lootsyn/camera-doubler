#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
client_python="${METADATA_CLIENT_PYTHON:-python3}"
receiver="robot-receiver-web-relay-validation-$$"
relay="robot-web-relay-validation-$$"
temporary="$(mktemp -d -t robot-web-relay-XXXXXX)"
archive="${temporary}/archive"

mkdir -p "${archive}"
docker run --rm \
  --user "$(id -u):$(id -g)" \
  -e HOME=/tmp \
  -e SYNTHETIC_FRAME_COUNT=48 \
  -e SYNTHETIC_ARCHIVE_ROOT=/archive \
  -v "${archive}:/archive" \
  --entrypoint robot-synthetic-roundtrip \
  robot-multicam-edge-core:local >/dev/null
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
    docker logs "${receiver}" >&2 || true
    docker logs "${relay}" >&2 || true
  fi
  docker rm -f "${relay}" "${receiver}" >/dev/null 2>&1 || true
  rm -rf -- "${temporary}"
  return "${status}"
}
trap cleanup EXIT

for command in curl docker gst-launch-1.0 python3; do
  command -v "${command}" >/dev/null || { echo "missing required command: ${command}" >&2; exit 2; }
done
"${client_python}" -c 'import grpc, google.protobuf' >/dev/null || {
  echo "metadata client Python is missing grpc/protobuf: ${client_python}" >&2
  exit 2
}

docker run -d --name "${receiver}" \
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

for _ in $(seq 1 50); do
  if curl -fsS http://127.0.0.1:19090/readyz >/dev/null 2>&1; then break; fi
  sleep 0.2
done
curl -fsS http://127.0.0.1:19090/readyz >/dev/null

docker run -d --name "${relay}" \
  --network host \
  --user 10002:10002 \
  --read-only \
  --tmpfs /tmp:size=64m,mode=1777 \
  --tmpfs /var/cache/robot-relay:uid=10002,gid=10002,mode=0770,size=512m \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -e HOME=/tmp \
  -e XDG_CACHE_HOME=/tmp/.cache \
  -e RECEIVER_GRPC_ENDPOINT=http://127.0.0.1:18083 \
  -e RELAY_HTTP_BIND=127.0.0.1:18091 \
  -e RELAY_OUTPUT_ROOT=/var/cache/robot-relay \
  -e RELAY_DISCOVERY_INTERVAL_MS=200 \
  -e RELAY_RECONNECT_MIN_MS=100 \
  -e RELAY_RECONNECT_MAX_MS=1000 \
  robot-multicam-web-relay:local serve >/dev/null

for _ in $(seq 1 50); do
  if curl -fsS http://127.0.0.1:18091/healthz >/dev/null 2>&1; then break; fi
  sleep 0.2
done
curl -fsS http://127.0.0.1:18091/healthz >/dev/null

env SYNTHETIC_ARCHIVE_ROOT="${archive}" SYNTHETIC_SRT_SLEEP_US=250000 \
  timeout 30s "${root}/scripts/send-synthetic-srt-fixture.sh" >/dev/null &
sender_pid=$!
sender_started_ms="$(date +%s%3N)"

stream_ready=false
for _ in $(seq 1 100); do
  if curl -fsS http://127.0.0.1:18091/api/v1/streams > "${temporary}/streams.json" 2>/dev/null \
    && python3 - "${temporary}/streams.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    value = json.load(stream)
raise SystemExit(0 if value else 1)
PY
  then
    stream_ready=true
    break
  fi
  sleep 0.2
done
if [[ "${stream_ready}" != "true" ]]; then
  echo "Relay stream catalog did not become ready" >&2
  exit 1
fi
curl -fsS http://127.0.0.1:18091/readyz >/dev/null

readarray -t urls < <(python3 - "${temporary}/streams.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    item = json.load(stream)[0]
print(item["playlist_url"])
print(item["metadata_url"])
print(item["camera_id"])
PY
)
playlist_url="http://127.0.0.1:18091${urls[0]}"
metadata_url="http://127.0.0.1:18091${urls[1]}"

"${client_python}" "${root}/scripts/receiver-metadata-client.py" \
  --endpoint 127.0.0.1:18083 sessions > "${temporary}/sessions.json"
"${client_python}" "${root}/scripts/receiver-metadata-client.py" \
  --endpoint 127.0.0.1:18083 snapshot --session "${session_id}" \
  > "${temporary}/snapshot.json"
"${client_python}" "${root}/scripts/receiver-metadata-client.py" \
  --endpoint 127.0.0.1:18083 watch --session "${session_id}" \
  --dump-dir "${temporary}/grpc-h264" --vectors --max-steps 1 --stream-timeout 20 \
  > "${temporary}/grpc-step.json"

playlist_ready=false
for _ in $(seq 1 100); do
  if curl -fsS "${playlist_url}" > "${temporary}/index.m3u8" 2>/dev/null \
    && grep -q '^#EXTM3U' "${temporary}/index.m3u8" \
    && grep -q '^segment[0-9].*\.ts$' "${temporary}/index.m3u8"; then
    playlist_ready=true
    break
  fi
  sleep 0.2
done
if [[ "${playlist_ready}" != "true" ]]; then
  echo "HLS playlist did not become ready" >&2
  exit 1
fi
playlist_ready_ms="$(( $(date +%s%3N) - sender_started_ms ))"
curl -fsS http://127.0.0.1:18091/api/v1/streams > "${temporary}/catalog-before-sse.json"
timeout 15s curl -Ns "${metadata_url}" > "${temporary}/metadata.sse" &
sse_pid=$!
segment="$(grep '^segment[0-9].*\.ts$' "${temporary}/index.m3u8" | head -1 | tr -d '\r')"
curl -fsS "${playlist_url%/*}/${segment}" > "${temporary}/segment.ts"
test -s "${temporary}/segment.ts"
test -s "${temporary}/grpc-h264/synthetic-anchor.h264"
gst-launch-1.0 -q filesrc location="${temporary}/segment.ts" \
  ! tsdemux ! h264parse ! fakesink
docker stats --no-stream \
  --format '{{.Name}} cpu={{.CPUPerc}} memory={{.MemUsage}} net={{.NetIO}}' \
  "${receiver}" "${relay}" > "${temporary}/container-stats.txt"

wait "${sender_pid}"
wait "${sse_pid}" || true
grep -q '^id:' "${temporary}/metadata.sse"
grep -q '^data:' "${temporary}/metadata.sse"
python3 - "${temporary}/metadata.sse" "${temporary}/sessions.json" \
  "${temporary}/snapshot.json" "${temporary}/grpc-step.json" "${urls[2]}" \
  "${temporary}/catalog-before-sse.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    event = next(json.loads(line.removeprefix("data:").strip()) for line in stream if line.startswith("data:"))
with open(sys.argv[2], encoding="utf-8") as stream:
    sessions = json.load(stream)["sessions"]
with open(sys.argv[3], encoding="utf-8") as stream:
    snapshot = json.load(stream)
with open(sys.argv[4], encoding="utf-8") as stream:
    grpc_step = json.loads(stream.readline())
with open(sys.argv[6], encoding="utf-8") as stream:
    catalog_before_sse = json.load(stream)[0]

assert sessions and sessions[0]["authoritative"]
assert snapshot["anchor"]["authoritative"]
assert grpc_step["valid"] and grpc_step["frames"][0]["encoded_bytes"] > 0
assert event["camera_id"] == sys.argv[5]
assert event["encoded_bytes"] > 0
assert event["access_unit_ordinal"] <= catalog_before_sse["last_access_unit_ordinal"]
assert abs(event["media_pts_seconds"] - event["normalized_pts_ns"] / 1_000_000_000) < 1e-9
assert catalog_before_sse["last_media_pts_seconds"] >= event["media_pts_seconds"]
assert event["context_crc32c"] > 0
assert event["named_features"]
PY

curl -fsS http://127.0.0.1:18091/healthz >/dev/null
curl -fsS http://127.0.0.1:18091/metrics | grep -q 'relay_access_units_received_total'
cat "${temporary}/container-stats.txt"
echo "HLS first-playlist latency=${playlist_ready_ms}ms"
echo "Web Relay PASS: external gRPC, HLS URL, TS demux, and correlated metadata SSE"
