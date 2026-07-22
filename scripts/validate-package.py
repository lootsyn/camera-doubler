#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import re
import sys
import tomllib
import zipfile
from pathlib import Path

try:
    import yaml
except ImportError as exc:
    print("ERROR: PyYAML is required for package validation", file=sys.stderr)
    raise SystemExit(2) from exc

ROOT = Path(__file__).resolve().parents[1]
errors: list[str] = []
warnings: list[str] = []


def error(message: str) -> None:
    errors.append(message)


def warn(message: str) -> None:
    warnings.append(message)


def require(path: str) -> Path:
    p = ROOT / path
    if not p.exists():
        error(f"missing required path: {path}")
    return p


required = [
    "ROBOT_MULTICAMERA_BACKEND_DESIGN.md",
    "README.md",
    "CHANGELOG.md",
    "REVIEW_REPORT.md",
    "compose.edge.yaml",
    "compose.receiver.yaml",
    ".env.edge.example",
    ".env.receiver.example",
    ".env.dataset-builder.example",
    ".env.adapter-rby1.example",
    "proto/frame_metadata.proto",
    "proto/adapter_api.proto",
    "proto/backend_api.proto",
    "proto/receiver_api.proto",
    "config/embodiment.example.yaml",
    "config/camera-policy.example.yaml",
    "config/protocol_constants.toml",
    "docker/Dockerfile.edge-core",
    "docker/Dockerfile.receiver",
    "docker/Dockerfile.dataset-builder",
    "adapters/rby1/docker/Dockerfile",
    "adapters/template/docker/Dockerfile",
    "docs/TRANSPORT_BOOTSTRAP.md",
    "docs/PROTOCOL_CONSTANTS.md",
    "docs/OPERATIONS.md",
    "scripts/prepare-host.sh",
    "scripts/verify-environment.sh",
    "scripts/bootstrap-example-config.sh",
    "scripts/generate-dev-secrets.sh",
    "scripts/package.sh",
    "scripts/validate-package.py",
    "testdata/streamid_vectors.json",
    "validation/four_pass_results.json",
    "validation/STATIC_CHECKS.txt",
]
for rel in required:
    require(rel)

# YAML syntax.
for rel in [
    "compose.edge.yaml",
    "compose.receiver.yaml",
    "config/embodiment.example.yaml",
    "config/camera-policy.example.yaml",
]:
    p = ROOT / rel
    if p.exists():
        try:
            yaml.safe_load(p.read_text(encoding="utf-8"))
        except Exception as exc:  # noqa: BLE001
            error(f"invalid YAML {rel}: {exc}")

# TOML syntax.
constants_path = ROOT / "config/protocol_constants.toml"
if constants_path.exists():
    try:
        protocol_constants = tomllib.loads(constants_path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        error(f"invalid TOML config/protocol_constants.toml: {exc}")
        protocol_constants = {}
else:
    protocol_constants = {}

# Markdown fenced-code balance catches common packaging/edit mistakes.
for p in sorted([ROOT / "ROBOT_MULTICAMERA_BACKEND_DESIGN.md", ROOT / "README.md", ROOT / "CHANGELOG.md"] + list((ROOT / "docs").glob("*.md"))):
    if p.exists():
        fence_count = sum(1 for line in p.read_text(encoding="utf-8").splitlines() if line.startswith("```"))
        if fence_count % 2:
            error(f"unbalanced Markdown code fences: {p.relative_to(ROOT)}")

# Compose build references and service independence.
for rel in ["compose.edge.yaml", "compose.receiver.yaml"]:
    p = ROOT / rel
    if not p.exists():
        continue
    data = yaml.safe_load(p.read_text(encoding="utf-8")) or {}
    for service_name, service in (data.get("services") or {}).items():
        build = service.get("build")
        if isinstance(build, dict) and build.get("dockerfile"):
            dockerfile = ROOT / build["dockerfile"]
            if not dockerfile.exists():
                error(f"{rel}:{service_name} references missing Dockerfile {build['dockerfile']}")

receiver_compose = yaml.safe_load((ROOT / "compose.receiver.yaml").read_text()) if (ROOT / "compose.receiver.yaml").exists() else {}
receiver_depends = (((receiver_compose or {}).get("services") or {}).get("receiver") or {}).get("depends_on") or {}
if "dataset-builder" in receiver_depends:
    error("receiver must not depend on dataset-builder; ingest/preview must remain independent")
dataset_service = (((receiver_compose or {}).get("services") or {}).get("dataset-builder") or {})
dataset_env_files = [
    entry.get("path") if isinstance(entry, dict) else entry
    for entry in (dataset_service.get("env_file") or [])
]
if ".env.dataset-builder" not in dataset_env_files:
    error("dataset-builder must use .env.dataset-builder exact-version/export contract")

# Lightweight protobuf structural checks. CI MUST still compile with protoc.
def iter_named_blocks(source: str):
    token = re.compile(r"\b(message|enum)\s+(\w+)\s*\{")
    for match in token.finditer(source):
        depth = 1
        i = match.end()
        while i < len(source) and depth:
            if source[i] == "{": depth += 1
            elif source[i] == "}": depth -= 1
            i += 1
        if depth:
            yield match.group(1), match.group(2), source[match.end():]
        else:
            yield match.group(1), match.group(2), source[match.end():i-1]

for p in sorted((ROOT / "proto").glob("*.proto")):
    source = p.read_text(encoding="utf-8")
    no_comments = re.sub(r"//.*", "", source)
    if not re.search(r'^syntax\s*=\s*"proto3"\s*;', source, re.M):
        error(f"{p.relative_to(ROOT)} missing proto3 syntax")
    if not re.search(r'^package\s+[A-Za-z0-9_.]+\s*;', source, re.M):
        error(f"{p.relative_to(ROOT)} missing package")
    if source.count("{") != source.count("}"):
        error(f"{p.relative_to(ROOT)} has unbalanced braces")
    for kind, name, body in iter_named_blocks(no_comments):
        numbers: set[int] = set()
        names: set[str] = set()
        if kind == "message":
            fields = re.findall(r"(?:repeated\s+|optional\s+)?[.\w<>]+\s+(\w+)\s*=\s*(\d+)\s*(?:\[[^]]*\])?\s*;", body)
        else:
            fields = re.findall(r"\b([A-Z][A-Z0-9_]*)\s*=\s*(\d+)\s*;", body)
        for field_name, raw_num in fields:
            num = int(raw_num)
            if num in numbers:
                error(f"{p.relative_to(ROOT)} duplicate number {num} in {kind} {name}")
            if field_name in names:
                error(f"{p.relative_to(ROOT)} duplicate name {field_name} in {kind} {name}")
            if kind == "message" and (num <= 0 or 19000 <= num <= 19999 or num > 536870911):
                error(f"{p.relative_to(ROOT)} invalid field number {num} in message {name}")
            numbers.add(num); names.add(field_name)

# Environment contracts.
def read(rel: str) -> str:
    p = ROOT / rel
    return p.read_text(encoding="utf-8") if p.exists() else ""

edge_env = read(".env.edge.example")
receiver_env = read(".env.receiver.example")
dataset_env = read(".env.dataset-builder.example")

def validate_env_keys(rel: str, text: str) -> None:
    seen: set[str] = set()
    for lineno, line in enumerate(text.splitlines(), start=1):
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key = line.split("=", 1)[0]
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", key):
            error(f"{rel}:{lineno} invalid environment key: {key}")
        if key in seen:
            error(f"{rel}:{lineno} duplicate environment key: {key}")
        seen.add(key)

validate_env_keys(".env.edge.example", edge_env)
validate_env_keys(".env.receiver.example", receiver_env)
validate_env_keys(".env.dataset-builder.example", dataset_env)
validate_env_keys(".env.adapter-rby1.example", read(".env.adapter-rby1.example"))
for key in [
    "ANCHOR_CAMERA_SELECTOR", "SRT_BASE_PORT", "MAX_CAMERAS",
    "SRT_STREAMID_HMAC_KEY_FILE", "SRT_PBKEYLEN", "MANIFEST_MAX_CHUNKS_PER_AU", "ANCHOR_CONTEXT_BUDGET_BYTES",
    "ANCHOR_CONTEXT_MAX_BYTES", "ANCHOR_AU_HOLD_MAX_MS",
    "FRAME_CONTEXT_MAP_MAX_ENTRIES", "MANIFEST_MAX_COMPRESSION_RATIO", "STATE_MISSING_POLICY", "EDGE_SPOOL_MODE",
    "STABLE_CAMERA_ID_COLLISION_POLICY", "CAMERA_SLOT_RECLAIM_POLICY", "CAMERA_SLOT_TOMBSTONE_DAYS",
]:
    if not re.search(rf"^{re.escape(key)}=", edge_env, re.M):
        error(f".env.edge.example missing {key}")
for key in [
    "SRT_LISTEN_BASE_PORT", "MAX_CAMERAS", "MANIFEST_WAIT_TIMEOUT_SEC",
    "SRT_STREAMID_HMAC_KEY_FILE", "SRT_PBKEYLEN", "RECEIVER_GRPC_BIND", "MIN_FREE_DISK_GB",
    "MANIFEST_MAX_COMPRESSION_RATIO", "DATASET_CADENCE_MODE",
    "ANCHOR_MAX_FRAME_INTERVAL_MS", "SEGMENT_HASH_ALGORITHM",
]:
    if not re.search(rf"^{re.escape(key)}=", receiver_env, re.M):
        error(f".env.receiver.example missing {key}")
for key in [
    "DATA_ROOT", "API_BIND", "LEROBOT_VERSION", "LEROBOT_DATASET_FORMAT",
    "LEROBOT_CADENCE_POLICY", "LEROBOT_EXPORT_COMMIT_MODE", "LEROBOT_VALIDATE_AFTER_EXPORT",
]:
    if not re.search(rf"^{re.escape(key)}=", dataset_env, re.M):
        error(f".env.dataset-builder.example missing {key}")


def env_int(text: str, key: str) -> int | None:
    m = re.search(rf"^{re.escape(key)}=(\d+)$", text, re.M)
    return int(m.group(1)) if m else None

base = env_int(receiver_env, "SRT_LISTEN_BASE_PORT")
max_cameras = env_int(receiver_env, "MAX_CAMERAS")
compose_text = read("compose.receiver.yaml")
if base is not None and max_cameras is not None:
    expected = f'{base}-{base + max_cameras - 1}:{base}-{base + max_cameras - 1}/udp'
    if expected not in compose_text:
        error(f"compose.receiver.yaml UDP range mismatch; expected {expected}")

budget = env_int(edge_env, "ANCHOR_CONTEXT_BUDGET_BYTES")
hard_cap = env_int(edge_env, "ANCHOR_CONTEXT_MAX_BYTES")
if budget is not None and hard_cap is not None and budget > hard_cap:
    error("ANCHOR_CONTEXT_BUDGET_BYTES exceeds hard cap")

edge_ratio = env_int(edge_env, "MANIFEST_MAX_COMPRESSION_RATIO")
receiver_ratio = env_int(receiver_env, "MANIFEST_MAX_COMPRESSION_RATIO")
if edge_ratio is not None and receiver_ratio is not None and edge_ratio != receiver_ratio:
    error("Edge/Receiver MANIFEST_MAX_COMPRESSION_RATIO mismatch")
if env_int(edge_env, "SRT_PBKEYLEN") not in {16, 24, 32}:
    error("Edge SRT_PBKEYLEN must be 16, 24, or 32")
if env_int(receiver_env, "SRT_PBKEYLEN") not in {16, 24, 32}:
    error("Receiver SRT_PBKEYLEN must be 16, 24, or 32")

# Protocol constants must agree across machine-readable config and docs/design.
constants_text = read("config/protocol_constants.toml")
uuids = re.findall(r'^[a-z_]+_uuid\s*=\s*"([0-9a-f-]{36})"', constants_text, re.M)
if len(uuids) != 3 or len(set(uuids)) != 3:
    error("protocol_constants.toml must contain three unique UUIDs")
for uuid in uuids:
    for rel in ["ROBOT_MULTICAMERA_BACKEND_DESIGN.md", "docs/PROTOCOL_CONSTANTS.md"]:
        if uuid not in read(rel):
            error(f"{rel} missing protocol UUID {uuid}")
for key in [
    "schema_id_hash", "feature_validity_bit_order", "recommended_gstreamer",
    "minimum_gstreamer_with_custom_codec", "manifest_max_compression_ratio",
]:
    if key not in protocol_constants:
        error(f"protocol_constants.toml missing {key}")
if protocol_constants.get("feature_validity_bit_order") != "lsb0":
    error("feature_validity_bit_order must be lsb0")
if protocol_constants.get("manifest_max_compression_ratio") != edge_ratio:
    error("protocol constant manifest_max_compression_ratio must match Edge env")

# Known stream-ID test vector integrity.
vector_path = ROOT / "testdata/streamid_vectors.json"
if vector_path.exists():
    v = json.loads(vector_path.read_text())
    import base64, hmac
    key = bytes.fromhex(v["hmac_key_hex"])
    digest = hmac.new(key, v["canonical_unsigned"].encode(), hashlib.sha256).digest()[:16]
    actual = base64.urlsafe_b64encode(digest).rstrip(b"=").decode()
    if actual != v["signature_base64url_no_padding"]:
        error("streamid_vectors.json signature mismatch")
    if len(v["canonical_signed"].encode()) != v["expected_length_bytes"]:
        error("streamid_vectors.json length mismatch")
    if v["expected_length_bytes"] > 256:
        error("reference canonical stream ID exceeds 256-byte limit")

# Four-pass evidence must be machine readable and all passes must be green.
review_json_path = ROOT / "validation/four_pass_results.json"
if review_json_path.exists():
    try:
        review_json = json.loads(review_json_path.read_text(encoding="utf-8"))
        if review_json.get("overall") != "PASS":
            error("four_pass_results.json overall status is not PASS")
        passes = review_json.get("passes") or []
        if len(passes) != 4 or any(p.get("status") != "PASS" for p in passes):
            error("four_pass_results.json must contain exactly four PASS reviews")
    except Exception as exc:  # noqa: BLE001
        error(f"invalid validation/four_pass_results.json: {exc}")

# Design requirements from the Receiver bootstrap revision.
design = read("ROBOT_MULTICAMERA_BACKEND_DESIGN.md")
for phrase in [
    "SessionManifestV1.anchor_camera_id",
    "stream-envelope.json",
    "SRT stream ID 계약",
    "Receiver 부트스트랩 상태 머신",
    "decoder 전에 추출",
    "SessionManifestChunkV1",
    "AnchorFrameContextPacketV1",
    "logical camera grouping",
    "metadata_kbps",
    "disk low/full",
    "AnchorAuHoldQueue",
    "DATASET_CADENCE_MODE",
    "segments/index.jsonl",
    "compression bomb",
    "RFC 8785",
    "LeRobot export transaction",
    "CAMERA_SLOT_RECLAIM_POLICY=manual",
    "atomic commit",
]:
    if phrase not in design:
        error(f"design missing required phrase: {phrase}")

# Wire-schema additions required by review.
frame_proto = read("proto/frame_metadata.proto")
receiver_proto = read("proto/receiver_api.proto")
for phrase in [
    "message AnchorFrameContextPacketV1",
    "message SessionManifestChunkV1",
    "string schema_id_algorithm = 19",
    "string timestamp_source = 25",
    "string timestamp_event = 26",
    "string source_clock_id = 27",
]:
    if phrase not in frame_proto:
        error(f"frame_metadata.proto missing required declaration: {phrase}")
for phrase in [
    'import "frame_metadata.proto";',
    "robot.multicam.v2.AnchorFrameContextV1 anchor_context = 11",
    "robot.multicam.v2.AnchorFrameContextPacketV1 anchor_context_packet = 12",
    "fixed64 access_unit_ordinal = 10",
]:
    if phrase not in receiver_proto:
        error(f"receiver_api.proto missing required declaration: {phrase}")

# Host-preparation script must not silently succeed with a mismatched loaded pool.
prepare_host = read("scripts/prepare-host.sh")
if "requested pool" not in prepare_host or "Refusing to report success" not in prepare_host:
    error("prepare-host.sh does not verify an already-loaded v4l2loopback pool")

# Packaging must exclude local state even when it is present for runtime tests.
package_script = read("scripts/package.sh")
for phrase in [
    '".cargo-home"',
    '".rustup-home"',
    '".venv-rby1"',
    '".venv-dataset"',
    '".venv-tools"',
    '"target"',
    '("validation", "runtime")',
    'rel.parts[0] == "secrets"',
    'excluded_env',
]:
    if phrase not in package_script:
        error(f"package.sh missing local-state exclusion: {phrase}")

if len(sys.argv) > 2:
    error("usage: validate-package.py [ARCHIVE.zip]")
elif len(sys.argv) == 2:
    archive_path = Path(sys.argv[1]).resolve()
    if not archive_path.is_file():
        error(f"package archive does not exist: {archive_path}")
    elif not zipfile.is_zipfile(archive_path):
        error(f"package archive is not a ZIP file: {archive_path}")
    else:
        forbidden_roots = {
            ".git",
            ".cargo-home",
            ".rustup-home",
            ".tools",
            ".venv-rby1",
            ".venv-dataset",
            ".venv-tools",
            "target",
        }
        forbidden_env = {
            ".env.edge",
            ".env.receiver",
            ".env.dataset-builder",
            ".env.adapter-rby1",
        }
        with zipfile.ZipFile(archive_path) as archive:
            members = [Path(name) for name in archive.namelist() if not name.endswith("/")]
        for member in members:
            parts = member.parts[1:]  # archive is rooted under the project directory
            if not parts:
                continue
            if parts[0] in forbidden_roots:
                error(f"package contains local cache: {member.as_posix()}")
            if tuple(parts[:2]) == ("validation", "runtime"):
                error(f"package contains runtime fixture: {member.as_posix()}")
            if parts[0] == "secrets" and parts[-1] not in {".gitignore", "README.md"}:
                error(f"package contains generated secret: {member.as_posix()}")
            if parts[-1] in forbidden_env:
                error(f"package contains local environment file: {member.as_posix()}")
            if "__pycache__" in parts or member.suffix == ".pyc":
                error(f"package contains Python cache: {member.as_posix()}")

# Scripts expected to execute must be executable.
for p in sorted((ROOT / "scripts").glob("*.sh")):
    if not p.stat().st_mode & 0o111:
        error(f"script is not executable: {p.relative_to(ROOT)}")

for item in warnings:
    print(f"WARNING: {item}")
if errors:
    for item in errors:
        print(f"ERROR: {item}", file=sys.stderr)
    print(f"validation failed with {len(errors)} error(s)", file=sys.stderr)
    raise SystemExit(1)
print("package validation passed")
