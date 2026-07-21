//! Canonical, HMAC-authenticated `rmc1` SRT stream identities.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use robot_multicam_protocol::constants;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const FIELD_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'=')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Anchor,
    Secondary,
}

impl Role {
    const fn wire(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Secondary => "secondary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}

impl Codec {
    const fn wire(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamIdentity {
    pub embodiment_id: String,
    pub edge_instance_id: String,
    pub edge_boot_id: Uuid,
    pub session_id: Uuid,
    pub camera_id: String,
    pub slot: u16,
    pub epoch: u32,
    pub role: Role,
    pub codec: Codec,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("stream ID exceeds {limit} bytes: {actual}")]
    TooLong { limit: usize, actual: usize },
    #[error("stream ID has invalid schema or field layout")]
    Layout,
    #[error("field {0} is empty")]
    Empty(&'static str),
    #[error("field {0} has invalid or non-canonical percent encoding")]
    Encoding(&'static str),
    #[error("field {0} is invalid")]
    Invalid(&'static str),
    #[error("stream ID signature is invalid")]
    Authentication,
    #[error("listen port {actual} does not equal base+slot {expected}")]
    PortSlotMismatch { expected: u16, actual: u16 },
    #[error("base port plus slot exceeds u16")]
    PortOverflow,
}

impl StreamIdentity {
    pub fn encode_signed(&self, key: &[u8]) -> Result<String, IdentityError> {
        validate_text("emb", &self.embodiment_id)?;
        validate_text("edge", &self.edge_instance_id)?;
        validate_text("cid", &self.camera_id)?;
        let unsigned = format!(
            "{};emb={};edge={};boot={};sid={};cid={};slot={};epoch={};role={};codec={}",
            constants::SRT_STREAM_ID_SCHEMA,
            encode_text(&self.embodiment_id),
            encode_text(&self.edge_instance_id),
            self.edge_boot_id.hyphenated(),
            self.session_id.hyphenated(),
            encode_text(&self.camera_id),
            self.slot,
            self.epoch,
            self.role.wire(),
            self.codec.wire(),
        );
        let signature = signature(key, unsigned.as_bytes())?;
        let signed = format!("{unsigned};sig={}", URL_SAFE_NO_PAD.encode(signature));
        validate_size(&signed)?;
        Ok(signed)
    }

    pub fn parse_and_verify(raw: &str, key: &[u8]) -> Result<Self, IdentityError> {
        validate_size(raw)?;
        let segments: Vec<_> = raw.split(';').collect();
        if segments.len() != 11 || segments[0] != constants::SRT_STREAM_ID_SCHEMA {
            return Err(IdentityError::Layout);
        }
        let names = [
            "emb", "edge", "boot", "sid", "cid", "slot", "epoch", "role", "codec", "sig",
        ];
        let mut values = Vec::with_capacity(names.len());
        for (segment, expected) in segments[1..].iter().zip(names) {
            let (name, value) = segment.split_once('=').ok_or(IdentityError::Layout)?;
            if name != expected || value.is_empty() {
                return Err(IdentityError::Layout);
            }
            values.push(value);
        }
        let unsigned_end = raw.rfind(";sig=").ok_or(IdentityError::Layout)?;
        let expected = signature(key, raw[..unsigned_end].as_bytes())?;
        let provided = URL_SAFE_NO_PAD
            .decode(values[9])
            .map_err(|_| IdentityError::Authentication)?;
        if provided.len() != 16 || expected.ct_eq(provided.as_slice()).unwrap_u8() != 1 {
            return Err(IdentityError::Authentication);
        }

        let embodiment_id = decode_text("emb", values[0])?;
        let edge_instance_id = decode_text("edge", values[1])?;
        let edge_boot_id = parse_uuid("boot", values[2])?;
        let session_id = parse_uuid("sid", values[3])?;
        let camera_id = decode_text("cid", values[4])?;
        let slot = parse_canonical_number("slot", values[5])?;
        let epoch = parse_canonical_number("epoch", values[6])?;
        let role = match values[7] {
            "anchor" => Role::Anchor,
            "secondary" => Role::Secondary,
            _ => return Err(IdentityError::Invalid("role")),
        };
        let codec = match values[8] {
            "h264" => Codec::H264,
            "h265" => Codec::H265,
            _ => return Err(IdentityError::Invalid("codec")),
        };
        Ok(Self {
            embodiment_id,
            edge_instance_id,
            edge_boot_id,
            session_id,
            camera_id,
            slot,
            epoch,
            role,
            codec,
        })
    }

    pub fn expected_port(&self, base_port: u16) -> Result<u16, IdentityError> {
        base_port
            .checked_add(self.slot)
            .ok_or(IdentityError::PortOverflow)
    }

    pub fn validate_listen_port(
        &self,
        base_port: u16,
        actual_port: u16,
    ) -> Result<(), IdentityError> {
        let expected = self.expected_port(base_port)?;
        if expected != actual_port {
            return Err(IdentityError::PortSlotMismatch {
                expected,
                actual: actual_port,
            });
        }
        Ok(())
    }
}

fn validate_size(raw: &str) -> Result<(), IdentityError> {
    if raw.len() > constants::MAX_STREAM_ID_BYTES {
        return Err(IdentityError::TooLong {
            limit: constants::MAX_STREAM_ID_BYTES,
            actual: raw.len(),
        });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::Empty(field));
    }
    if value.chars().any(char::is_control) {
        return Err(IdentityError::Encoding(field));
    }
    Ok(())
}

fn encode_text(value: &str) -> String {
    utf8_percent_encode(value, FIELD_ENCODE_SET).to_string()
}

fn decode_text(field: &'static str, raw: &str) -> Result<String, IdentityError> {
    let decoded = percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| IdentityError::Encoding(field))?
        .into_owned();
    validate_text(field, &decoded)?;
    if encode_text(&decoded) != raw {
        return Err(IdentityError::Encoding(field));
    }
    Ok(decoded)
}

fn parse_uuid(field: &'static str, value: &str) -> Result<Uuid, IdentityError> {
    let parsed = Uuid::parse_str(value).map_err(|_| IdentityError::Invalid(field))?;
    if parsed.hyphenated().to_string() != value {
        return Err(IdentityError::Invalid(field));
    }
    Ok(parsed)
}

fn parse_canonical_number<T>(field: &'static str, value: &str) -> Result<T, IdentityError>
where
    T: std::str::FromStr + std::fmt::Display,
{
    let parsed = value
        .parse::<T>()
        .map_err(|_| IdentityError::Invalid(field))?;
    if parsed.to_string() != value {
        return Err(IdentityError::Invalid(field));
    }
    Ok(parsed)
}

fn signature(key: &[u8], message: &[u8]) -> Result<[u8; 16], IdentityError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| IdentityError::Authentication)?;
    mac.update(message);
    let digest = mac.finalize().into_bytes();
    let mut truncated = [0u8; 16];
    truncated.copy_from_slice(&digest[..16]);
    Ok(truncated)
}

#[cfg(test)]
mod tests {
    use super::{IdentityError, StreamIdentity};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        hmac_key_hex: String,
        canonical_signed: String,
    }

    fn vector() -> (Vec<u8>, String) {
        let value: Vector =
            serde_json::from_str(include_str!("../../../testdata/streamid_vectors.json"))
                .expect("fixture");
        (
            hex::decode(value.hmac_key_hex).expect("hex"),
            value.canonical_signed,
        )
    }

    #[test]
    fn canonical_vector_round_trips() {
        let (key, raw) = vector();
        let parsed = StreamIdentity::parse_and_verify(&raw, &key).expect("parse");
        assert_eq!(parsed.encode_signed(&key).expect("encode"), raw);
        assert_eq!(parsed.expected_port(10_000).expect("port"), 10_000);
    }

    #[test]
    fn signature_and_port_mismatch_fail_closed() {
        let (key, raw) = vector();
        let changed = raw.replace("slot=0", "slot=1");
        assert_eq!(
            StreamIdentity::parse_and_verify(&changed, &key),
            Err(IdentityError::Authentication)
        );
        let parsed = StreamIdentity::parse_and_verify(&raw, &key).expect("parse");
        assert!(parsed.validate_listen_port(10_000, 10_001).is_err());
    }

    #[test]
    fn noncanonical_encoding_is_rejected_even_with_valid_signature() {
        let (key, raw) = vector();
        let parsed = StreamIdentity::parse_and_verify(&raw, &key).expect("parse");
        let encoded = parsed.encode_signed(&key).expect("encode");
        assert!(!encoded.contains("%"));
    }
}
