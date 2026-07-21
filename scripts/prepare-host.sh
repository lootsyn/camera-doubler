#!/usr/bin/env bash
set -euo pipefail

[[ "$(uname -s)" == "Linux" ]] || { echo "Linux is required" >&2; exit 1; }
[[ ${EUID:-$(id -u)} -eq 0 ]] || { echo "run as root: sudo $0" >&2; exit 1; }

START="${VIRTUAL_CAMERA_START:-40}"
POOL="${VIRTUAL_CAMERA_POOL_SIZE:-16}"
FORCE_RELOAD="${FORCE_RELOAD_V4L2LOOPBACK:-false}"

[[ "$START" =~ ^[0-9]+$ && "$POOL" =~ ^[0-9]+$ && "$POOL" -gt 0 ]] || {
  echo "VIRTUAL_CAMERA_START and VIRTUAL_CAMERA_POOL_SIZE must be positive integers" >&2
  exit 1
}
command -v modprobe >/dev/null || { echo "modprobe not found" >&2; exit 1; }

requested_devices=()
video_numbers=()
labels=()
for ((i=0; i<POOL; i++)); do
  n=$((START+i))
  requested_devices+=("/dev/video${n}")
  video_numbers+=("${n}")
  labels+=("LeRobot Virtual ${i}")
done

is_loopback_device() {
  local dev="$1" base
  base="$(basename "$dev")"
  [[ -e "/sys/class/video4linux/${base}" ]] || return 1
  local driver
  driver="$(basename "$(readlink -f "/sys/class/video4linux/${base}/device/driver" 2>/dev/null || true)")"
  [[ "$driver" == "v4l2loopback" ]]
}

pool_matches() {
  local dev
  for dev in "${requested_devices[@]}"; do
    [[ -e "$dev" ]] || return 1
    is_loopback_device "$dev" || return 1
  done
}

if lsmod | grep -q '^v4l2loopback'; then
  if pool_matches; then
    echo "v4l2loopback already provides requested pool; keeping current module parameters"
  elif [[ "$FORCE_RELOAD" == "true" ]]; then
    echo "reloading v4l2loopback; this may interrupt current virtual-camera users" >&2
    modprobe -r v4l2loopback
  else
    cat >&2 <<EOF
v4l2loopback is already loaded, but the requested pool
/dev/video${START}..$((START+POOL-1)) is not fully present as v4l2loopback devices.
Refusing to report success because module parameters cannot be changed in place.
Stop virtual-camera users and rerun with FORCE_RELOAD_V4L2LOOPBACK=true, or adjust
VIRTUAL_CAMERA_START/VIRTUAL_CAMERA_POOL_SIZE to the existing pool.
EOF
    exit 1
  fi
fi

if ! lsmod | grep -q '^v4l2loopback'; then
  join_by_comma() { local IFS=,; echo "$*"; }
  modprobe v4l2loopback devices="$POOL" \
    video_nr="$(join_by_comma "${video_numbers[@]}")" \
    card_label="$(join_by_comma "${labels[@]}")" \
    exclusive_caps=1
fi

pool_matches || { echo "requested v4l2loopback pool verification failed" >&2; exit 1; }
echo "verified v4l2loopback pool: /dev/video${START}..$((START+POOL-1))"
echo "Edge Core must set keep_format/sustain_framerate/timeout controls after format negotiation."
