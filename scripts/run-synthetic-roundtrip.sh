#!/usr/bin/env bash
set -euo pipefail

command -v gst-launch-1.0 >/dev/null
gst-inspect-1.0 x264enc >/dev/null
gst-inspect-1.0 mpegtsmux >/dev/null
gst-inspect-1.0 tsdemux >/dev/null
gst-inspect-1.0 avdec_h264 >/dev/null

if command -v robot-synthetic-roundtrip >/dev/null; then
  exec robot-synthetic-roundtrip
fi
exec cargo run --locked --release -p edge-core --bin synthetic_roundtrip
