# Generic Robot Multi-Camera Backend Specification 2.1 — Four-Pass Review Report

- Review date: 2026-07-21
- Reviewed artifact root: `robot_multicamera_backend_spec_v2_1/`
- Result: **Specification/static-package review passed; 2026-07-22 implementation follow-up completed with physical hardware gates remaining**
- Review model: four independent passes with different failure assumptions

## 1. Review scope

The review covered the full design, protobuf contracts, environment configuration, Docker/Compose scaffolding, operational scripts, receiver bootstrap rules, archive/replay rules, and package validation logic.

The target requirements were:

1. Keep the existing LeRobot UI path minimally affected through per-camera virtual cameras.
2. Discover all supported connected cameras and independently stream each camera.
3. Allow stream-only exclusion and full camera disable by configuration.
4. Select one anchor camera through environment/configuration.
5. Put only the synchronization timestamp on secondary cameras and put observation/action context on the anchor only.
6. Let the receiver identify camera streams, determine the authoritative anchor, extract in-band metadata before decode, synchronize all cameras, and build LeRobot data.
7. Keep vendor-specific robot/gripper logic in replaceable Hardware Adapters.
8. Run Edge, Receiver, Dataset Builder, and Adapters through Docker/Compose scaffolding.

---

## 2. Pass 1 — Architecture and requirement traceability

### Review question

Can every requested behavior be implemented without coupling Generic Edge Core or Receiver to RB-Y1, and can the receiver bootstrap itself without out-of-band per-frame metadata?

### Result

**Passed after corrections.**

### Findings and applied corrections

| Finding | Risk | Applied correction |
|---|---|---|
| Receiver had no sufficiently strict rule for deciding which incoming SRT connection represented which camera. | Port reuse or stale connections could be misidentified. | Added canonical HMAC-protected SRT stream ID, `port = base + slot`, provisional registry, and a receiver bootstrap state machine. |
| Transport `role=anchor` could be mistaken for the final anchor declaration. | A forged or stale transport hint could select the wrong authoritative state/action stream. | Defined `SessionManifestV1.anchor_camera_id` as authoritative and required transport/manifest cross-validation. |
| A physical USB camera may expose several `/dev/videoN` nodes. | The system could create duplicate virtual cameras and streams for metadata/alternate nodes. | Added physical/logical camera grouping using udev parent, USB interface, `bus_info`, and media-controller identity. |
| Receiver ingest could be coupled to Dataset Builder health. | Export failure could stop preview and raw ingest. | Removed Receiver dependency on Dataset Builder in Compose and documented degraded export behavior. |
| Anchor selection could conflict with stream exclusion or full disable. | Dataset readiness could silently fail. | Made anchor/exclude/disable and required-camera conflicts startup configuration errors. |
| A later RobotState sample may be needed to interpolate at the anchor capture time. | Waiting in the wrong branch could delay the LeRobot UI, while not waiting could bind the wrong state to an image. | Added an anchor-only bounded `AnchorAuHoldQueue`; virtual and secondary branches bypass it. |
| One camera may be present only for local UI. | Manifest could incorrectly require an unstreamed camera at the receiver. | Defined virtual-only cameras as `stream_excluded=true`, `required_for_dataset=false`, and `transport_port=0`. |

### Architecture invariants confirmed

- Generic Edge Core and Receiver contain no vendor SDK dependency.
- RB-Y1 logic is confined to `adapters/rby1`.
- A robot, gripper, base, tool, or sensor can be added through the common Adapter API and embodiment configuration.
- Every supported active camera receives a stable camera ID and virtual camera slot.
- Every externally streamed camera uses an independent SRT connection and stable stream slot.
- The LeRobot UI branch does not wait for state interpolation, network connection, manifest generation, or Dataset Builder.
- Only the configured anchor receives frame-level observation/action context.
- All streamed coded-picture AUs receive exactly one synchronization timestamp.

---

## 3. Pass 2 — Protocol, timestamp, and data-integrity review

### Review question

Can a receiver unambiguously reconstruct camera identity, session identity, frame capture time, anchor state/action, schema, and replay identity without guessing?

### Result

**Passed after protocol hardening.**

### Findings and applied corrections

| Finding | Risk | Applied correction |
|---|---|---|
| CRC over a decoded/re-serialized protobuf is ambiguous. | Different serialization order could produce false CRC failures or hide byte-level corruption. | Wrapped anchor context in `AnchorFrameContextPacketV1`; CRC32C covers the exact transmitted inner bytes. |
| A manifest may exceed a safe single-SEI payload. | IDR burst, parser limits, or memory abuse. | Added bounded `SessionManifestChunkV1`, optional zstd, full uncompressed CRC, chunk/total/time/reassembly limits, and one chunk per AU by default. |
| Compressed manifest size alone did not prevent a decompression bomb. | Small compressed input could allocate excessive memory/CPU. | Added declared-size, max total, max chunk count, max compression-ratio, and decompression time/CPU gates. |
| Schema IDs had no cross-language canonical construction. | Two implementations could assign different IDs to the same feature layout. | Defined RFC 8785 JCS + SHA-256 first 64 bits, big-endian; zero is reserved. |
| Feature validity bitmap bit order was implicit. | Cross-language consumers could reverse bits. | Fixed `lsb0`; bit `i` maps to `feature_slices[i]`. |
| Camera timestamp quality fields did not fully describe the source event. | “Capture time” could mean exposure, USB completion, or dequeue depending on driver. | Added `timestamp_source`, `timestamp_event`, and `source_clock_id` to `CameraDescriptorV1`. |
| PTS-only context lookup could collide or be missing. | A state/action context might be attached to the wrong encoded picture. | Added normalized PTS + internal AU/capture ordinal correlation, strict handling of missing/duplicate/non-monotonic PTS, bounded orphan cleanup, and no nearest-PTS guessing. |
| MPEG-TS PTS eventually wraps. | Long sessions/replay could compare raw 33-bit PTS incorrectly. | Required demux/parser unwrapped time or an explicit wrap counter; raw 33-bit values are not compared directly. |
| Multiple unregistered SEI messages vary by GStreamer parser version. | Timestamp, context, or manifest could be silently lost. | Recommended GStreamer 1.24+ and required a custom codec-parser fallback plus startup multi-SEI round trip for 1.22/1.23 or any failing plugin stack. |
| Raw `.ts` does not retain SRT connection identity. | Replay could lose camera/session/slot/epoch identity. | Added connection-level `stream-envelope.json` and per-segment `segments/index.jsonl` with time range, size, and SHA-256. |

### Wire-contract checks completed

- Three fixed, unique user-data-unregistered SEI UUIDs are present in the machine-readable constants and documents.
- The canonical SRT stream ID test vector produces signature `49Rh3qjMcdonlHpdKihB9A` and is 214 bytes, below the 256-byte limit.
- Protobuf messages/enums passed lightweight duplicate field/name/number and brace checks.
- `DeviceFrameQualityV1.clock_residual_ns` and receiver residual/skew values are signed.
- Non-anchor per-frame semantic metadata remains limited to `SyncTimestampV1.capture_time_edge_ns`.
- Anchor state/action comes from the same coded-picture AU as its synchronization timestamp.
- Manifest and context are extracted before the decoder; decoded output is joined to the encoded envelope using normalized PTS and AU ordinal.
- Non-finite observation/action values are invalid unless a feature explicitly defines another policy.

---

## 4. Pass 3 — Performance, failure isolation, storage, and security review

### Review question

Does the design stay bounded and keep the LeRobot UI responsive when cameras, encoders, adapters, network, receiver storage, or metadata paths fail?

### Result

**Passed after adding explicit budgets and failure behavior.**

### Findings and applied corrections

| Finding | Risk | Applied correction |
|---|---|---|
| Repeating full state/action on every camera scales serialization and correlation work with camera count. | CPU/memory/lock pressure on the robot host. | Retained timestamp-only secondary streams and one authoritative anchor context. |
| Anchor interpolation wait lacked an AU-level resource bound. | Slow state sources could accumulate encoded frames. | Added max hold time, frame/byte/entry caps, timeout behavior, and orphan cleanup. |
| Camera count was not sufficient to determine feasible stream count. | USB bandwidth, encoder-session exhaustion, or CPU fallback could affect the UI. | Added admission control for USB controller bandwidth, encoder sessions, CPU/GPU, virtual slots, queue memory, and metadata budget. |
| Manifest/context limits did not have an operational bitrate interpretation. | Metadata could consume a significant fraction of stream bitrate. | Added 2 KiB soft/8 KiB hard context budget and metadata-kbps metrics. |
| SRT passphrase alone could be configured while key length remains disabled. | Operators could believe encryption was active when `pbkeylen` was `no-key`. | Required `SRT_PBKEYLEN` to be 16/24/32; examples use 32. |
| Stream ID authentication does not provide secrecy. | Sensitive serials or task names might be exposed in handshake metadata. | Restricted stream IDs to non-secret stable IDs and documented session-boundary key rotation. |
| Receiver disk pressure behavior was underspecified. | Silent data loss or preview outage. | Added free-space readiness, retention, explicit recording abort while preview continues, bounded Edge spool, and active-file protection. |
| Nominal 30 Hz did not define behavior when real anchor cadence is irregular. | A LeRobot export could advertise 30 Hz while silently duplicating/synthesizing frames. | Added `anchor_native` and optional `fixed_grid_nearest`; both use only real frames, default no reuse, and record cadence quality. |
| A segment file alone did not prove archive integrity. | Corrupted replay could yield plausible but wrong timestamps. | Added atomic segment index records and SHA-256 verification. |
| Stable camera ID collision and automatic slot reuse were not fail-closed. | Two identical/unstable devices could inherit the wrong stream/virtual-camera identity after reconnect. | Added collision failure, persistent mapping generation, slot tombstones, and manual reclaim as defaults. |
| Final LeRobot export lifecycle and dependency pin were not transactional. | A package upgrade, partial finalize, or irregular cadence could create a corrupt/mislabeled dataset. | Added exact version verification, public-API-only implementation, cadence gate, temp/finalize/load-scan/checksum/atomic commit, and provenance. |
| Receiver API convenience vectors omitted some anchor context fields and original packet bytes. | External consumers could not retrieve every quality/schema/validity field or audit the exact CRC-protected payload. | Added canonical decoded `AnchorFrameContextV1`, exact `AnchorFrameContextPacketV1`, and frame epoch/PTS/AU ordinal to the Receiver API. |

### Failure-isolation invariants confirmed

- Virtual-camera output is independent from SRT connection and state interpolation.
- A secondary-camera stream failure does not stop other camera pipelines.
- Anchor/Adapter failure marks dataset readiness/context invalid but keeps unaffected UI streams alive.
- Manifest timeout leaves preview/raw ingest in provisional mode and prevents dataset steps.
- Dataset Builder failure does not stop Receiver ingest/preview/raw recording.
- Disk full stops recording/episode creation explicitly while keeping preview when possible.
- All queues, rings, pending manifest buffers, reassembly buffers, and optional spool are bounded.
- Session, boot, camera, stream epoch, and schema discontinuities are not silently joined.

---

## 5. Pass 4 — Build, deployment, and package-completeness review

### Review question

Does the package contain a coherent implementation contract and runnable scaffolding without missing references, malformed configuration, or generated secrets?

### Result

**Static package checks passed after corrections. Runtime builds remain implementation-dependent.**

### Findings and applied corrections

| Finding | Risk | Applied correction |
|---|---|---|
| Earlier package variants referenced Dockerfiles and receiver configuration that were not present. | AI implementation agent could not follow a complete scaffold. | Added all referenced Dockerfiles, Receiver env, camera policy, protocol constants, API proto, operations docs, and scripts. |
| A preloaded but differently configured v4l2loopback module caused `prepare-host.sh` to report success. | Requested virtual devices could be absent while deployment continued. | Script now verifies the exact requested device pool and refuses success unless matched or explicitly reloaded. |
| Static checks did not cover TOML, Markdown fence balance, env duplicate keys, protocol additions, or compression-ratio consistency. | Packaging edits could introduce subtle contract drift. | Expanded `scripts/validate-package.py` with these checks. |
| Docker port ranges do not expand automatically when env values change. | Receiver listeners could be unreachable. | Added validator consistency check and operational warning to regenerate Compose range or use host networking. |
| Generated development secrets could accidentally enter the ZIP. | Credential disclosure. | Validator/package script exclude local env and generated secret files; only templates and secret documentation remain. |

### Static checks executed

- Python syntax compilation of `scripts/validate-package.py`
- Bash syntax check of all `scripts/*.sh`
- YAML parsing of both Compose files and example configs
- TOML parsing of protocol constants
- Compose Dockerfile-reference validation
- Markdown fenced-code balance
- Environment key uniqueness and required-key checks
- Edge/Receiver port and protocol-limit consistency checks
- Canonical stream-ID HMAC test vector
- Lightweight protobuf structural/field-number checks
- Required design phrase and wire-schema declaration checks
- Generated-secret and local-env exclusion checks

### Tools unavailable in this review runtime

The review environment did not contain `docker`, `protoc`, `gst-launch-1.0`, or `shellcheck`. Therefore the following were **not** claimed as completed:

- `docker compose config` against a live Docker installation
- Docker image build or container startup
- actual protobuf compilation/code generation
- Rust `cargo build/test`
- real GStreamer multi-SEI encode/mux/demux/extract round trip
- shellcheck semantic lint
- v4l2loopback, physical camera, hardware encoder, SRT network, robot, or LeRobot integration tests

The Dockerfiles are intentionally scaffolds for source code that the AI implementation agent must generate. Until that source tree exists, image builds are expected not to complete.

---

## 6. Requirement traceability matrix

| Requirement | Primary specification location | Machine/config artifact | Verification status |
|---|---|---|---|
| All supported connected cameras | Sections 3, 8 | `camera-policy.example.yaml` | Static passed; hardware pending |
| Per-camera virtual camera | Sections 9, 12 | `compose.edge.yaml`, `prepare-host.sh` | Static passed; host pending |
| Per-camera independent stream | Section 14.1 | `.env.edge.example`, `.env.receiver.example` | Static passed; SRT pending |
| Stream exclusion | Section 8.4 | `CAMERA_STREAM_EXCLUDE` | Passed |
| Full disable | Section 8.4 | `CAMERA_DISABLE` | Passed |
| Environment-selected anchor | Section 8.5 | `ANCHOR_CAMERA_SELECTOR` | Passed |
| Receiver knows camera identity | Sections 14.1–14.4 | stream-ID vector/constants | Passed |
| Receiver knows authoritative anchor | Section 14.5 | `SessionManifestV1.anchor_camera_id` | Passed |
| Receiver extracts/exposes all anchor information | Sections 13.7, 14.7, 14.10 | `frame_metadata.proto`, `receiver_api.proto` | Static passed; GStreamer/gRPC pending |
| Timestamp on every camera | Sections 2, 13.2 | `SyncTimestampV1` | Static passed; codec pending |
| Full context only on anchor | Sections 2, 12, 13 | `AnchorFrameContextPacketV1` | Static passed; codec pending |
| Generic robot/gripper support | Sections 5–7, 27 | `adapter_api.proto`, embodiment config | Passed; adapter implementations pending |
| Edge-generic timestamp basis | Section 10 | manifest clock fields | Passed; clock calibration pending |
| 30 Hz LeRobot assembly | Sections 11, 14.11–14.13 | Receiver cadence env | Static passed; loader pending |
| Docker/Compose scaffolding | Section 22 | Dockerfiles/Compose | References passed; builds pending |
| Archive/replay identity | Sections 14.9, 19 | envelope/index contract | Static passed; replay tool pending |
| Stable camera mapping lifecycle | Section 8.3 | Edge env/camera policy | Static passed; hotplug hardware pending |
| Transactional LeRobot export | Section 19.1 | `.env.dataset-builder.example`, Dataset Builder Docker scaffold | Static passed; loader/build pending |

---

## 7. Remaining mandatory implementation gates

These are not unresolved design ambiguities; they are evidence that must be produced by the implementation project before production acceptance.

1. Compile all protobufs with the pinned `protoc` version and run backward/forward compatibility checks.
2. Generate the Rust/Python source tree and pass locked `cargo build`, unit tests, formatter, linter, and dependency audit.
3. Build both Docker Compose stacks and run health/readiness tests with non-root Receiver/Dataset Builder.
4. Run a startup conformance test using the exact deployed GStreamer image:
   - timestamp + context + manifest on the same anchor AU,
   - timestamp only on secondary AU,
   - MPEG-TS mux/demux,
   - decoder-before/after correlation,
   - H.264 and every enabled H.265 path.
5. Test at least the configured maximum practical camera count against USB and encoder limits.
6. Measure camera timestamp semantics and systematic offsets with a common visual event; write calibration revision into the manifest.
7. Execute Adapter contract tests for every robot, gripper, base, and tool combination.
8. Verify actual/effective target semantics and state clock mapping for each vendor SDK version.
9. Replay raw TS using envelope/index/hash and reproduce synchronized steps bit-for-bit where applicable.
10. Build with the exact pinned LeRobot version, then exercise temp export, finalize, loader full scan, checksum/provenance, atomic commit, cadence rejection, and visual pose/state validation.
11. Exercise network loss, Receiver restart, anchor unplug, Adapter restart, disk-full, key rotation, and control-lease loss.
12. Perform production security review for secrets, firewall, mTLS authorization, container capabilities, image pinning, SBOM, and vulnerability findings.

## 8. Post-revision four-pass rerun

The complete package was rechecked after the final Receiver-API, camera-slot lifecycle, and LeRobot-export changes. Machine-readable evidence is stored in `validation/four_pass_results.json`.

- Pass 1 — architecture and requirements: **PASS** (6 checks)
- Pass 2 — protocol, time, and integrity: **PASS** (7 checks)
- Pass 3 — performance, failure, security, and export: **PASS** (8 checks)
- Pass 4 — build, deployment, and package consistency: **PASS** (7 checks)

The same runtime limitations remain: Docker, protoc, gst-launch-1.0, and shellcheck were unavailable in this review environment. The report therefore distinguishes static evidence from the mandatory implementation/runtime gates above.

## 9. Final review conclusion

No additional specification blocker was found after the fourth pass. The revised package now defines:

- how every camera connection is provisionally identified,
- how the authoritative anchor is proven,
- how each camera stream is received independently,
- how all anchor information is extracted in-band before decode,
- how state/action is correlated without PTS guessing,
- how secondary metadata stays timestamp-only,
- how fixed/irregular dataset cadence is handled without synthetic video,
- how archive replay preserves transport identity and integrity,
- and how vendor-specific robot/component logic remains replaceable.

Production readiness still depends on the mandatory implementation and hardware gates listed above.

## 10. Implementation follow-up — 2026-07-22

The implementation project subsequently closed every environment/tool gate and all software-executable mandatory gates. Docker 29.1.3, Compose 2.40.3, GStreamer 1.24.2, protoc, shellcheck, official `rby1-sdk==0.10.0`, and exact `lerobot==0.6.0` were installed and run.

- Locked workspace format/Clippy/tests passed: 46 Rust tests, zero failures.
- Five Docker images built and ran under least-privilege constraints.
- The deployed Edge healthcheck ran the real 24-AU H.264/SEI/MPEG-TS/predecode/decode conformance path.
- Authenticated encrypted SRT accepted the same signed raw stream twice while Receiver remained ready.
- Raw replay verified envelope/index/hash and reproduced 24 synchronized protobuf steps bit-for-bit on two passes.
- Dataset Builder passed five exact-version transactional/export/loader tests.
- Five CycloneDX SBOMs were generated; Trivy reported zero fixable HIGH/CRITICAL findings.
- Actual package ZIP contents were inspected for toolchain, target, venv, local env, and secret exclusion.

The authoritative final classification is in `docs/audit/FINAL_RELEASE_AUDIT.md`. Remaining gates require physical USB cameras/v4l2loopback, actual RB-Y1 motion, target-host camera-capacity measurement, or common-event timestamp calibration; none is `BLOCKED_ENVIRONMENT`.
