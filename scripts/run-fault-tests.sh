#!/usr/bin/env bash
set -euo pipefail

cargo test --locked -p edge-core bounded_queue_drops_oldest_without_blocking_producer
cargo test --locked -p robot-multicam-camera-discovery slots_persist_and_tombstones_are_not_reused
cargo test --locked -p receiver reconnect_preserves_identity_and_allows_epoch_refresh
cargo test --locked -p receiver disk_pressure_is_never_silent
cargo test --locked -p robot-multicam-metadata-codec timeout_and_conflicting_duplicate_fail_closed
python3 scripts/verify-vendor-boundary.py

# A bounded live pipeline exercises producer/consumer shutdown without hardware.
timeout 10s gst-launch-1.0 -q videotestsrc num-buffers=90 is-live=true \
  ! video/x-raw,framerate=30/1,width=320,height=240 \
  ! queue max-size-buffers=2 leaky=downstream \
  ! fakesink sync=false
echo "fault/stress contract tests PASS"
