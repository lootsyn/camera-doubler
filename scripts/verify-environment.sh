#!/usr/bin/env bash
set -euo pipefail

fail=0
check_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "OK command: $1"
  else
    echo "MISSING command: $1" >&2
    fail=1
  fi
}

version_ge() {
  # Compare dotted numeric versions: version_ge 1.24.0 1.22.0
  [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]
}

check_cmd docker
check_cmd gst-inspect-1.0
check_cmd gst-launch-1.0
check_cmd v4l2-ctl
check_cmd python3
check_cmd openssl

if command -v docker >/dev/null 2>&1; then
  docker compose version >/dev/null 2>&1 || { echo "MISSING Docker Compose plugin" >&2; fail=1; }
fi

if command -v gst-launch-1.0 >/dev/null 2>&1; then
  gst_version="$(gst-launch-1.0 --version | awk '/GStreamer/{print $2; exit}')"
  if [[ -z "$gst_version" ]]; then
    echo "UNABLE to determine GStreamer version" >&2
    fail=1
  elif ! version_ge "$gst_version" "1.22.0"; then
    echo "GStreamer $gst_version is below the minimum 1.22.0" >&2
    fail=1
  elif version_ge "$gst_version" "1.24.0"; then
    echo "OK GStreamer version: $gst_version (recommended multiple-SEI path)"
  else
    echo "WARNING GStreamer version: $gst_version; custom multi-SEI codec fallback and startup round-trip are mandatory" >&2
  fi
fi

if command -v gst-inspect-1.0 >/dev/null 2>&1; then
  for element in srtsrc srtsink mpegtsmux tsdemux h264parse h265parse v4l2src v4l2sink; do
    if gst-inspect-1.0 "$element" >/dev/null 2>&1; then
      echo "OK GStreamer element: $element"
    else
      echo "MISSING GStreamer element: $element" >&2
      fail=1
    fi
  done
fi

if [[ -d /sys/module/v4l2loopback ]]; then
  echo "OK kernel module: v4l2loopback"
else
  echo "MISSING kernel module: v4l2loopback" >&2
  fail=1
fi

printf 'Physical/video endpoints:\n'
ls -1 /dev/video* 2>/dev/null || true
exit "$fail"
