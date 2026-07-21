use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Constants {
    protocol_version: String,
    srt_stream_id_schema: String,
    sync_timestamp_uuid: String,
    anchor_context_uuid: String,
    session_manifest_uuid: String,
    crc: String,
    stream_id_hmac: String,
    max_stream_id_bytes: usize,
    max_anchor_context_packet_bytes: usize,
    anchor_context_budget_bytes: usize,
    max_manifest_chunk_bytes: usize,
    max_manifest_total_bytes: usize,
    schema_id_hash: String,
    feature_validity_bit_order: String,
    recommended_gstreamer: String,
    minimum_gstreamer_with_custom_codec: String,
    manifest_max_compression_ratio: usize,
}

fn quoted(value: &str) -> String {
    format!("{value:?}")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../..");
    let proto_dir = root.join("proto");
    let constants_path = root.join("config/protocol_constants.toml");
    println!("cargo:rerun-if-changed={}", constants_path.display());
    println!("cargo:rerun-if-changed={}", proto_dir.display());

    let constants: Constants = toml::from_str(&fs::read_to_string(&constants_path)?)?;
    for raw in [
        &constants.sync_timestamp_uuid,
        &constants.anchor_context_uuid,
        &constants.session_manifest_uuid,
    ] {
        uuid::Uuid::parse_str(raw)?;
    }
    if constants.anchor_context_budget_bytes > constants.max_anchor_context_packet_bytes {
        return Err("anchor context soft budget exceeds the hard cap".into());
    }
    if constants.feature_validity_bit_order != "lsb0" {
        return Err("only lsb0 feature validity order is supported".into());
    }

    let generated = format!(
        r#"pub const PROTOCOL_VERSION: &str = {};
pub const SRT_STREAM_ID_SCHEMA: &str = {};
pub const SYNC_TIMESTAMP_UUID: &str = {};
pub const ANCHOR_CONTEXT_UUID: &str = {};
pub const SESSION_MANIFEST_UUID: &str = {};
pub const CRC_ALGORITHM: &str = {};
pub const STREAM_ID_HMAC_ALGORITHM: &str = {};
pub const MAX_STREAM_ID_BYTES: usize = {};
pub const MAX_ANCHOR_CONTEXT_PACKET_BYTES: usize = {};
pub const ANCHOR_CONTEXT_BUDGET_BYTES: usize = {};
pub const MAX_MANIFEST_CHUNK_BYTES: usize = {};
pub const MAX_MANIFEST_TOTAL_BYTES: usize = {};
pub const SCHEMA_ID_HASH: &str = {};
pub const FEATURE_VALIDITY_BIT_ORDER: &str = {};
pub const RECOMMENDED_GSTREAMER: &str = {};
pub const MINIMUM_GSTREAMER_WITH_CUSTOM_CODEC: &str = {};
pub const MANIFEST_MAX_COMPRESSION_RATIO: usize = {};
"#,
        quoted(&constants.protocol_version),
        quoted(&constants.srt_stream_id_schema),
        quoted(&constants.sync_timestamp_uuid),
        quoted(&constants.anchor_context_uuid),
        quoted(&constants.session_manifest_uuid),
        quoted(&constants.crc),
        quoted(&constants.stream_id_hmac),
        constants.max_stream_id_bytes,
        constants.max_anchor_context_packet_bytes,
        constants.anchor_context_budget_bytes,
        constants.max_manifest_chunk_bytes,
        constants.max_manifest_total_bytes,
        quoted(&constants.schema_id_hash),
        quoted(&constants.feature_validity_bit_order),
        quoted(&constants.recommended_gstreamer),
        quoted(&constants.minimum_gstreamer_with_custom_codec),
        constants.manifest_max_compression_ratio,
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    fs::write(out_dir.join("protocol_constants.rs"), generated)?;

    let vendored_include = protoc_bin_vendored::include_path()?;
    env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    let protos = [
        proto_dir.join("adapter_api.proto"),
        proto_dir.join("backend_api.proto"),
        proto_dir.join("frame_metadata.proto"),
        proto_dir.join("receiver_api.proto"),
    ];
    let includes: [&Path; 2] = [&proto_dir, vendored_include.as_path()];
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&protos, &includes)?;
    Ok(())
}
