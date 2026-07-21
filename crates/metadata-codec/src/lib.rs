//! Bounded protobuf and H.264 user-data-unregistered SEI codec.

pub mod metadata_ext;

use prost::Message;
use robot_multicam_protocol::constants;
use robot_multicam_protocol::multicam::{
    AnchorFrameContextPacketV1, AnchorFrameContextV1, SyncTimestampV1,
};
use thiserror::Error;
use uuid::Uuid;

const H264_NAL_SEI: u8 = 6;
const SEI_USER_DATA_UNREGISTERED: usize = 5;
const MAX_SEI_MESSAGES_PER_AU: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("capture timestamp zero is reserved as invalid")]
    ZeroTimestamp,
    #[error("protobuf payload exceeds limit {limit}: {actual}")]
    PayloadTooLarge { limit: usize, actual: usize },
    #[error("malformed Annex-B access unit")]
    MalformedAccessUnit,
    #[error("malformed SEI RBSP")]
    MalformedSei,
    #[error("expected exactly one timestamp SEI, found {0}")]
    TimestampCount(usize),
    #[error("non-anchor AU contains semantic metadata UUID {0}")]
    SecondarySemanticMetadata(Uuid),
    #[error("protobuf decode failed: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("anchor context CRC32C mismatch")]
    ContextCrc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDataUnregistered {
    pub uuid: Uuid,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InspectedAccessUnit {
    pub timestamp: SyncTimestampV1,
    pub messages: Vec<UserDataUnregistered>,
}

pub fn encode_sync_timestamp(capture_time_edge_ns: u64) -> Result<Vec<u8>, CodecError> {
    if capture_time_edge_ns == 0 {
        return Err(CodecError::ZeroTimestamp);
    }
    let message = SyncTimestampV1 {
        capture_time_edge_ns,
    };
    Ok(message.encode_to_vec())
}

pub fn decode_sync_timestamp(bytes: &[u8]) -> Result<SyncTimestampV1, CodecError> {
    if bytes.len() >= 16 {
        return Err(CodecError::PayloadTooLarge {
            limit: 15,
            actual: bytes.len(),
        });
    }
    let timestamp = SyncTimestampV1::decode(bytes)?;
    if timestamp.capture_time_edge_ns == 0 {
        return Err(CodecError::ZeroTimestamp);
    }
    Ok(timestamp)
}

pub fn encode_anchor_context_packet(context: &AnchorFrameContextV1) -> Result<Vec<u8>, CodecError> {
    let exact = context.encode_to_vec();
    if exact.len() > constants::MAX_ANCHOR_CONTEXT_PACKET_BYTES {
        return Err(CodecError::PayloadTooLarge {
            limit: constants::MAX_ANCHOR_CONTEXT_PACKET_BYTES,
            actual: exact.len(),
        });
    }
    let packet = AnchorFrameContextPacketV1 {
        schema_version: 1,
        payload_crc32c: crc32c::crc32c(&exact),
        serialized_context: exact,
    };
    Ok(packet.encode_to_vec())
}

pub fn decode_anchor_context_packet(
    bytes: &[u8],
) -> Result<(AnchorFrameContextPacketV1, AnchorFrameContextV1), CodecError> {
    if bytes.len() > constants::MAX_ANCHOR_CONTEXT_PACKET_BYTES {
        return Err(CodecError::PayloadTooLarge {
            limit: constants::MAX_ANCHOR_CONTEXT_PACKET_BYTES,
            actual: bytes.len(),
        });
    }
    let packet = AnchorFrameContextPacketV1::decode(bytes)?;
    if crc32c::crc32c(&packet.serialized_context) != packet.payload_crc32c {
        return Err(CodecError::ContextCrc);
    }
    let context = AnchorFrameContextV1::decode(packet.serialized_context.as_slice())?;
    Ok((packet, context))
}

pub fn inject_timestamp_h264_annex_b(
    access_unit: &[u8],
    capture_time_edge_ns: u64,
) -> Result<Vec<u8>, CodecError> {
    if inspect_h264_annex_b(access_unit, false)?.is_some() {
        return Err(CodecError::TimestampCount(2));
    }
    let uuid = protocol_uuid(constants::SYNC_TIMESTAMP_UUID);
    let sei = make_user_data_sei(uuid, &encode_sync_timestamp(capture_time_edge_ns)?)?;
    let nals = split_annex_b(access_unit)?;
    let insert_at = nals
        .iter()
        .position(|nal| (1..=5).contains(&nal.payload.first().map_or(0, |byte| byte & 0x1f)))
        .unwrap_or(0);
    let mut out = Vec::with_capacity(access_unit.len() + sei.len());
    for (index, nal) in nals.iter().enumerate() {
        if index == insert_at {
            out.extend_from_slice(&sei);
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal.payload);
    }
    Ok(out)
}

pub fn inspect_h264_annex_b(
    access_unit: &[u8],
    secondary: bool,
) -> Result<Option<InspectedAccessUnit>, CodecError> {
    let timestamp_uuid = protocol_uuid(constants::SYNC_TIMESTAMP_UUID);
    let context_uuid = protocol_uuid(constants::ANCHOR_CONTEXT_UUID);
    let manifest_uuid = protocol_uuid(constants::SESSION_MANIFEST_UUID);
    let mut messages = Vec::new();
    for nal in split_annex_b(access_unit)? {
        if nal.payload.first().map(|byte| byte & 0x1f) == Some(H264_NAL_SEI) {
            messages.extend(parse_sei_nal(nal.payload)?);
            if messages.len() > MAX_SEI_MESSAGES_PER_AU {
                return Err(CodecError::MalformedSei);
            }
        }
    }
    let timestamps: Vec<_> = messages
        .iter()
        .filter(|message| message.uuid == timestamp_uuid)
        .collect();
    if timestamps.is_empty() {
        return Ok(None);
    }
    if timestamps.len() != 1 {
        return Err(CodecError::TimestampCount(timestamps.len()));
    }
    if secondary {
        if let Some(forbidden) = messages
            .iter()
            .find(|message| message.uuid == context_uuid || message.uuid == manifest_uuid)
        {
            return Err(CodecError::SecondarySemanticMetadata(forbidden.uuid));
        }
    }
    let timestamp = decode_sync_timestamp(&timestamps[0].payload)?;
    Ok(Some(InspectedAccessUnit {
        timestamp,
        messages,
    }))
}

fn protocol_uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("generated protocol UUID was build-time validated")
}

fn make_user_data_sei(uuid: Uuid, payload: &[u8]) -> Result<Vec<u8>, CodecError> {
    let payload_size = 16usize
        .checked_add(payload.len())
        .ok_or(CodecError::MalformedSei)?;
    let mut rbsp = Vec::with_capacity(payload_size + 8);
    push_ff_encoded(&mut rbsp, SEI_USER_DATA_UNREGISTERED);
    push_ff_encoded(&mut rbsp, payload_size);
    rbsp.extend_from_slice(uuid.as_bytes());
    rbsp.extend_from_slice(payload);
    rbsp.push(0x80);
    let escaped = add_emulation_prevention(&rbsp);
    let mut nal = Vec::with_capacity(escaped.len() + 5);
    nal.extend_from_slice(&[0, 0, 0, 1, H264_NAL_SEI]);
    nal.extend_from_slice(&escaped);
    Ok(nal)
}

fn push_ff_encoded(target: &mut Vec<u8>, mut value: usize) {
    while value >= 255 {
        target.push(255);
        value -= 255;
    }
    target.push(u8::try_from(value).expect("remainder is below 255"));
}

fn add_emulation_prevention(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len());
    let mut zero_count = 0u8;
    for &byte in rbsp {
        if zero_count >= 2 && byte <= 3 {
            out.push(3);
            zero_count = 0;
        }
        out.push(byte);
        zero_count = if byte == 0 {
            zero_count.saturating_add(1)
        } else {
            0
        };
    }
    out
}

fn remove_emulation_prevention(data: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    let mut zero_count = 0u8;
    while index < data.len() {
        let byte = data[index];
        if zero_count >= 2 && byte == 3 {
            let next = data.get(index + 1).ok_or(CodecError::MalformedSei)?;
            if *next > 3 {
                return Err(CodecError::MalformedSei);
            }
            zero_count = 0;
            index += 1;
            continue;
        }
        out.push(byte);
        zero_count = if byte == 0 {
            zero_count.saturating_add(1)
        } else {
            0
        };
        index += 1;
    }
    Ok(out)
}

fn parse_sei_nal(nal: &[u8]) -> Result<Vec<UserDataUnregistered>, CodecError> {
    if nal.first().map(|byte| byte & 0x1f) != Some(H264_NAL_SEI) {
        return Err(CodecError::MalformedSei);
    }
    let rbsp = remove_emulation_prevention(&nal[1..])?;
    let mut cursor = 0usize;
    let mut result = Vec::new();
    while cursor < rbsp.len() && rbsp[cursor] != 0x80 {
        let payload_type = take_ff_encoded(&rbsp, &mut cursor)?;
        let payload_size = take_ff_encoded(&rbsp, &mut cursor)?;
        let end = cursor
            .checked_add(payload_size)
            .filter(|end| *end <= rbsp.len())
            .ok_or(CodecError::MalformedSei)?;
        if payload_type == SEI_USER_DATA_UNREGISTERED {
            if payload_size < 16 {
                return Err(CodecError::MalformedSei);
            }
            let uuid = Uuid::from_slice(&rbsp[cursor..cursor + 16])
                .map_err(|_| CodecError::MalformedSei)?;
            result.push(UserDataUnregistered {
                uuid,
                payload: rbsp[cursor + 16..end].to_vec(),
            });
        }
        cursor = end;
    }
    Ok(result)
}

fn take_ff_encoded(bytes: &[u8], cursor: &mut usize) -> Result<usize, CodecError> {
    let mut value = 0usize;
    loop {
        let byte = *bytes.get(*cursor).ok_or(CodecError::MalformedSei)?;
        *cursor += 1;
        value = value
            .checked_add(usize::from(byte))
            .ok_or(CodecError::MalformedSei)?;
        if byte != 255 {
            return Ok(value);
        }
    }
}

struct Nal<'a> {
    payload: &'a [u8],
}

fn split_annex_b(access_unit: &[u8]) -> Result<Vec<Nal<'_>>, CodecError> {
    let starts = start_codes(access_unit);
    if starts.is_empty() {
        return Err(CodecError::MalformedAccessUnit);
    }
    let mut result = Vec::with_capacity(starts.len());
    for (index, &(offset, prefix)) in starts.iter().enumerate() {
        let begin = offset + prefix;
        let end = starts
            .get(index + 1)
            .map_or(access_unit.len(), |next| next.0);
        if begin >= end {
            return Err(CodecError::MalformedAccessUnit);
        }
        result.push(Nal {
            payload: &access_unit[begin..end],
        });
    }
    Ok(result)
}

fn start_codes(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut index = 0usize;
    while index + 3 <= bytes.len() {
        if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            result.push((index, 4));
            index += 4;
        } else if bytes[index..].starts_with(&[0, 0, 1]) {
            result.push((index, 3));
            index += 3;
        } else {
            index += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        decode_anchor_context_packet, encode_anchor_context_packet, inject_timestamp_h264_annex_b,
        inspect_h264_annex_b, CodecError,
    };
    use robot_multicam_protocol::multicam::AnchorFrameContextV1;

    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];

    #[test]
    fn timestamp_round_trip_is_exactly_one() {
        let enriched = inject_timestamp_h264_annex_b(IDR, 42).expect("inject");
        let inspected = inspect_h264_annex_b(&enriched, true)
            .expect("inspect")
            .expect("timestamp");
        assert_eq!(inspected.timestamp.capture_time_edge_ns, 42);
        assert_eq!(inspected.messages.len(), 1);
        assert_eq!(
            inject_timestamp_h264_annex_b(&enriched, 43),
            Err(CodecError::TimestampCount(2))
        );
    }

    #[test]
    fn malformed_payload_is_rejected() {
        assert!(inspect_h264_annex_b(&[0, 0, 0, 1, 6, 5], false).is_err());
    }

    #[test]
    fn context_crc_detects_mutation() {
        let context = AnchorFrameContextV1 {
            schema_version: 1,
            anchor_frame_seq: 7,
            ..Default::default()
        };
        let encoded = encode_anchor_context_packet(&context).expect("encode");
        let (_, decoded) = decode_anchor_context_packet(&encoded).expect("decode");
        assert_eq!(decoded.anchor_frame_seq, 7);

        let mut changed = encoded;
        let last = changed.last_mut().expect("packet bytes");
        *last ^= 1;
        assert!(decode_anchor_context_packet(&changed).is_err());
    }
}
