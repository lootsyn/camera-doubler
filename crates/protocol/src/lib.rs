//! Generated protobuf types and fixed wire-protocol constants.

pub mod constants {
    include!(concat!(env!("OUT_DIR"), "/protocol_constants.rs"));
}

pub mod robot {
    pub mod adapter {
        pub mod v1 {
            tonic::include_proto!("robot.adapter.v1");
        }
    }

    pub mod backend {
        pub mod v1 {
            tonic::include_proto!("robot.backend.v1");
        }
    }

    pub mod multicam {
        pub mod v2 {
            tonic::include_proto!("robot.multicam.v2");
        }
    }

    pub mod receiver {
        pub mod v1 {
            tonic::include_proto!("robot.receiver.v1");
        }
    }
}

pub use robot::adapter::v1 as adapter;
pub use robot::backend::v1 as backend;
pub use robot::multicam::v2 as multicam;
pub use robot::receiver::v1 as receiver;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeConstants {
    pub protocol_version: String,
    pub srt_stream_id_schema: String,
    pub sync_timestamp_uuid: String,
    pub anchor_context_uuid: String,
    pub session_manifest_uuid: String,
    pub crc: String,
    pub stream_id_hmac: String,
    pub max_stream_id_bytes: usize,
    pub max_anchor_context_packet_bytes: usize,
    pub anchor_context_budget_bytes: usize,
    pub max_manifest_chunk_bytes: usize,
    pub max_manifest_total_bytes: usize,
    pub schema_id_hash: String,
    pub feature_validity_bit_order: String,
    pub recommended_gstreamer: String,
    pub minimum_gstreamer_with_custom_codec: String,
    pub manifest_max_compression_ratio: usize,
}

#[derive(Debug, Error)]
pub enum ConstantsError {
    #[error("unable to read protocol constants: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid protocol constants TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("runtime protocol constants differ from compiled constants: {0}")]
    Drift(&'static str),
}

impl RuntimeConstants {
    pub fn load_and_validate(path: impl AsRef<std::path::Path>) -> Result<Self, ConstantsError> {
        let value: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        value.validate_compiled()?;
        Ok(value)
    }

    pub fn validate_compiled(&self) -> Result<(), ConstantsError> {
        macro_rules! same {
            ($field:ident, $constant:expr) => {
                if self.$field != $constant {
                    return Err(ConstantsError::Drift(stringify!($field)));
                }
            };
        }
        same!(protocol_version, constants::PROTOCOL_VERSION);
        same!(srt_stream_id_schema, constants::SRT_STREAM_ID_SCHEMA);
        same!(sync_timestamp_uuid, constants::SYNC_TIMESTAMP_UUID);
        same!(anchor_context_uuid, constants::ANCHOR_CONTEXT_UUID);
        same!(session_manifest_uuid, constants::SESSION_MANIFEST_UUID);
        same!(crc, constants::CRC_ALGORITHM);
        same!(stream_id_hmac, constants::STREAM_ID_HMAC_ALGORITHM);
        same!(max_stream_id_bytes, constants::MAX_STREAM_ID_BYTES);
        same!(
            max_anchor_context_packet_bytes,
            constants::MAX_ANCHOR_CONTEXT_PACKET_BYTES
        );
        same!(
            anchor_context_budget_bytes,
            constants::ANCHOR_CONTEXT_BUDGET_BYTES
        );
        same!(
            max_manifest_chunk_bytes,
            constants::MAX_MANIFEST_CHUNK_BYTES
        );
        same!(
            max_manifest_total_bytes,
            constants::MAX_MANIFEST_TOTAL_BYTES
        );
        same!(schema_id_hash, constants::SCHEMA_ID_HASH);
        same!(
            feature_validity_bit_order,
            constants::FEATURE_VALIDITY_BIT_ORDER
        );
        same!(recommended_gstreamer, constants::RECOMMENDED_GSTREAMER);
        same!(
            minimum_gstreamer_with_custom_codec,
            constants::MINIMUM_GSTREAMER_WITH_CUSTOM_CODEC
        );
        same!(
            manifest_max_compression_ratio,
            constants::MANIFEST_MAX_COMPRESSION_RATIO
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeConstants;

    #[test]
    fn checked_in_constants_match_generated_code() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/protocol_constants.toml"
        );
        RuntimeConstants::load_and_validate(path).expect("constants must agree");
    }
}
