#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive="${SYNTHETIC_ARCHIVE_ROOT:-${root}/validation/runtime/archive-conformance}"
host="${SRT_TARGET_HOST:-127.0.0.1}"
sleep_us="${SYNTHETIC_SRT_SLEEP_US:-50000}"
envelope="${archive}/stream-envelope.json"
segment="${archive}/segments/synthetic.ts"
passphrase_file="${SRT_PASSPHRASE_FILE:-${root}/secrets/srt_passphrase.txt}"

for command in python3 gst-launch-1.0; do
  command -v "${command}" >/dev/null || { echo "missing required command: ${command}" >&2; exit 2; }
done
for path in "${envelope}" "${segment}" "${passphrase_file}"; do
  test -f "${path}" || { echo "missing required file: ${path}" >&2; exit 2; }
done

readarray -t identity < <(python3 - "${envelope}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    envelope = json.load(stream)
print(envelope["raw_stream_id"])
print(envelope["listen_port"])
PY
)
stream_id="${identity[0]}"
port="${SRT_TARGET_PORT:-${identity[1]}}"
passphrase="$(tr -d '\r\n' < "${passphrase_file}")"

uri="srt://${host}:${port}?mode=caller&latency=120"
gst-launch-1.0 -q filesrc location="${segment}" blocksize=1316 \
  ! identity sleep-time="${sleep_us}" \
  ! srtsink uri="${uri}" pbkeylen=32 passphrase="${passphrase}" streamid="${stream_id}" wait-for-connection=true
echo "synthetic SRT fixture sent: host=${host} port=${port}"
