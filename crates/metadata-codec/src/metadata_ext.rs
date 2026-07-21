use std::io::Read;
use std::time::Duration;

use prost::Message;
use robot_multicam_protocol::constants;
use robot_multicam_protocol::multicam::{ManifestCompressionV1, SessionManifestChunkV1};
use thiserror::Error;
use uuid::Uuid;

use super::{
    inspect_h264_annex_b, make_user_data_sei, split_annex_b, CodecError, UserDataUnregistered,
};

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest session ID must contain exactly 16 bytes")]
    SessionId,
    #[error("manifest is empty or exceeds the configured total size")]
    TotalSize,
    #[error("manifest chunk size or count is invalid")]
    ChunkLimit,
    #[error("manifest chunk fields conflict")]
    Conflict,
    #[error("manifest reassembly timed out")]
    Timeout,
    #[error("manifest compression ratio exceeds the configured limit")]
    CompressionRatio,
    #[error("manifest decompression failed: {0}")]
    Decompression(String),
    #[error("manifest exact-byte CRC32C mismatch")]
    Crc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestCompression {
    None,
    Zstd,
}

pub fn inject_user_data_h264_annex_b(
    access_unit: &[u8],
    additions: &[UserDataUnregistered],
) -> Result<Vec<u8>, CodecError> {
    let inspected = inspect_h264_annex_b(access_unit, false)?;
    if let Some(existing) = inspected {
        for addition in additions {
            if existing
                .messages
                .iter()
                .any(|message| message.uuid == addition.uuid)
            {
                return Err(CodecError::TimestampCount(2));
            }
        }
    }
    let nals = split_annex_b(access_unit)?;
    let insert_at = nals
        .iter()
        .position(|nal| (1..=5).contains(&nal.payload.first().map_or(0, |byte| byte & 0x1f)))
        .unwrap_or(0);
    let estimated = access_unit.len()
        + additions
            .iter()
            .map(|addition| addition.payload.len() + 32)
            .sum::<usize>();
    let mut output = Vec::with_capacity(estimated);
    for (index, nal) in nals.iter().enumerate() {
        if index == insert_at {
            for addition in additions {
                output.extend_from_slice(&make_user_data_sei(addition.uuid, &addition.payload)?);
            }
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(nal.payload);
    }
    Ok(output)
}

pub fn chunk_manifest(
    serialized_manifest: &[u8],
    session_id: &[u8],
    revision: u64,
    compression: ManifestCompression,
) -> Result<Vec<SessionManifestChunkV1>, ManifestError> {
    if session_id.len() != 16 {
        return Err(ManifestError::SessionId);
    }
    if serialized_manifest.is_empty()
        || serialized_manifest.len() > constants::MAX_MANIFEST_TOTAL_BYTES
    {
        return Err(ManifestError::TotalSize);
    }
    let encoded = match compression {
        ManifestCompression::None => serialized_manifest.to_vec(),
        ManifestCompression::Zstd => zstd::stream::encode_all(serialized_manifest, 3)
            .map_err(|error| ManifestError::Decompression(error.to_string()))?,
    };
    validate_ratio(serialized_manifest.len(), encoded.len())?;
    let chunks: Vec<_> = encoded
        .chunks(constants::MAX_MANIFEST_CHUNK_BYTES)
        .map(ToOwned::to_owned)
        .collect();
    let max_chunks =
        constants::MAX_MANIFEST_TOTAL_BYTES.div_ceil(constants::MAX_MANIFEST_CHUNK_BYTES);
    if chunks.is_empty() || chunks.len() > max_chunks {
        return Err(ManifestError::ChunkLimit);
    }
    let chunk_count = u32::try_from(chunks.len()).map_err(|_| ManifestError::ChunkLimit)?;
    let crc = crc32c::crc32c(serialized_manifest);
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk_payload)| SessionManifestChunkV1 {
            schema_version: 1,
            session_id: session_id.to_vec(),
            manifest_revision: revision,
            chunk_index: u32::try_from(index).expect("bounded chunk index"),
            chunk_count,
            compression: match compression {
                ManifestCompression::None => ManifestCompressionV1::None as i32,
                ManifestCompression::Zstd => ManifestCompressionV1::Zstd as i32,
            },
            uncompressed_size: u32::try_from(serialized_manifest.len())
                .expect("manifest total bound fits u32"),
            full_payload_crc32c: crc,
            chunk_payload,
        })
        .collect())
}

#[derive(Debug)]
pub struct ManifestReassembler {
    session_id: Vec<u8>,
    revision: u64,
    chunk_count: u32,
    compression: i32,
    uncompressed_size: u32,
    crc: u32,
    chunks: Vec<Option<Vec<u8>>>,
    received_bytes: usize,
    started_at: Duration,
    timeout: Duration,
}

impl ManifestReassembler {
    pub fn new(
        first: SessionManifestChunkV1,
        now: Duration,
        timeout: Duration,
    ) -> Result<Self, ManifestError> {
        validate_chunk(&first)?;
        let chunk_count = first.chunk_count;
        let mut value = Self {
            session_id: first.session_id.clone(),
            revision: first.manifest_revision,
            chunk_count,
            compression: first.compression,
            uncompressed_size: first.uncompressed_size,
            crc: first.full_payload_crc32c,
            chunks: vec![
                None;
                usize::try_from(chunk_count).map_err(|_| ManifestError::ChunkLimit)?
            ],
            received_bytes: 0,
            started_at: now,
            timeout,
        };
        value.insert(first, now)?;
        Ok(value)
    }

    pub fn insert(
        &mut self,
        chunk: SessionManifestChunkV1,
        now: Duration,
    ) -> Result<Option<Vec<u8>>, ManifestError> {
        if now.saturating_sub(self.started_at) > self.timeout {
            return Err(ManifestError::Timeout);
        }
        validate_chunk(&chunk)?;
        if chunk.session_id != self.session_id
            || chunk.manifest_revision != self.revision
            || chunk.chunk_count != self.chunk_count
            || chunk.compression != self.compression
            || chunk.uncompressed_size != self.uncompressed_size
            || chunk.full_payload_crc32c != self.crc
        {
            return Err(ManifestError::Conflict);
        }
        let index = usize::try_from(chunk.chunk_index).map_err(|_| ManifestError::ChunkLimit)?;
        match &self.chunks[index] {
            Some(existing) if *existing != chunk.chunk_payload => {
                return Err(ManifestError::Conflict)
            }
            Some(_) => {}
            None => {
                self.received_bytes = self
                    .received_bytes
                    .checked_add(chunk.chunk_payload.len())
                    .ok_or(ManifestError::TotalSize)?;
                if self.received_bytes > constants::MAX_MANIFEST_TOTAL_BYTES {
                    return Err(ManifestError::TotalSize);
                }
                self.chunks[index] = Some(chunk.chunk_payload);
            }
        }
        if self.chunks.iter().any(Option::is_none) {
            return Ok(None);
        }
        let encoded = self
            .chunks
            .iter()
            .flatten()
            .flat_map(|chunk| chunk.iter().copied())
            .collect::<Vec<_>>();
        validate_ratio(self.uncompressed_size as usize, encoded.len())?;
        let decoded = match ManifestCompressionV1::try_from(self.compression) {
            Ok(ManifestCompressionV1::None) => encoded,
            Ok(ManifestCompressionV1::Zstd) => decode_zstd_bounded(
                &encoded,
                usize::try_from(self.uncompressed_size).map_err(|_| ManifestError::TotalSize)?,
            )?,
            _ => return Err(ManifestError::Conflict),
        };
        if decoded.len() != self.uncompressed_size as usize {
            return Err(ManifestError::TotalSize);
        }
        if crc32c::crc32c(&decoded) != self.crc {
            return Err(ManifestError::Crc);
        }
        Ok(Some(decoded))
    }
}

fn validate_chunk(chunk: &SessionManifestChunkV1) -> Result<(), ManifestError> {
    let max_chunks =
        constants::MAX_MANIFEST_TOTAL_BYTES.div_ceil(constants::MAX_MANIFEST_CHUNK_BYTES);
    if chunk.schema_version != 1
        || chunk.session_id.len() != 16
        || chunk.chunk_count == 0
        || usize::try_from(chunk.chunk_count).map_or(true, |count| count > max_chunks)
        || chunk.chunk_index >= chunk.chunk_count
        || chunk.chunk_payload.is_empty()
        || chunk.chunk_payload.len() > constants::MAX_MANIFEST_CHUNK_BYTES
        || chunk.uncompressed_size == 0
        || chunk.uncompressed_size as usize > constants::MAX_MANIFEST_TOTAL_BYTES
    {
        return Err(ManifestError::ChunkLimit);
    }
    Ok(())
}

fn validate_ratio(uncompressed: usize, compressed: usize) -> Result<(), ManifestError> {
    if compressed == 0
        || uncompressed > compressed.saturating_mul(constants::MANIFEST_MAX_COMPRESSION_RATIO)
    {
        return Err(ManifestError::CompressionRatio);
    }
    Ok(())
}

fn decode_zstd_bounded(encoded: &[u8], expected: usize) -> Result<Vec<u8>, ManifestError> {
    let decoder = zstd::stream::read::Decoder::new(encoded)
        .map_err(|error| ManifestError::Decompression(error.to_string()))?;
    let limit = u64::try_from(expected)
        .map_err(|_| ManifestError::TotalSize)?
        .saturating_add(1);
    let mut decoded = Vec::with_capacity(expected);
    decoder
        .take(limit)
        .read_to_end(&mut decoded)
        .map_err(|error| ManifestError::Decompression(error.to_string()))?;
    if decoded.len() > expected {
        return Err(ManifestError::TotalSize);
    }
    Ok(decoded)
}

pub fn manifest_chunk_sei(chunk: &SessionManifestChunkV1) -> UserDataUnregistered {
    UserDataUnregistered {
        uuid: Uuid::parse_str(constants::SESSION_MANIFEST_UUID).expect("build-time validated UUID"),
        payload: chunk.encode_to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        chunk_manifest, inject_user_data_h264_annex_b, manifest_chunk_sei, ManifestCompression,
        ManifestError, ManifestReassembler,
    };
    use crate::{inject_timestamp_h264_annex_b, inspect_h264_annex_b};

    #[test]
    fn manifest_chunks_reassemble_with_crc_and_bounds() {
        let payload = vec![42_u8; 20_000];
        let chunks =
            chunk_manifest(&payload, &[7; 16], 4, ManifestCompression::None).expect("chunks");
        assert!(chunks.len() > 1);
        let mut reassembler =
            ManifestReassembler::new(chunks[0].clone(), Duration::ZERO, Duration::from_secs(1))
                .expect("reassembler");
        let mut result = None;
        for chunk in chunks.into_iter().skip(1) {
            result = reassembler
                .insert(chunk, Duration::from_millis(10))
                .expect("insert");
        }
        assert_eq!(result.expect("complete"), payload);
    }

    #[test]
    fn timeout_and_conflicting_duplicate_fail_closed() {
        let chunks = chunk_manifest(&vec![1; 9_000], &[8; 16], 1, ManifestCompression::None)
            .expect("chunks");
        let mut reassembler =
            ManifestReassembler::new(chunks[0].clone(), Duration::ZERO, Duration::from_millis(5))
                .expect("reassembler");
        assert!(matches!(
            reassembler.insert(chunks[1].clone(), Duration::from_secs(1)),
            Err(ManifestError::Timeout)
        ));
    }

    #[test]
    fn secondary_rejects_manifest_semantics() {
        let base = inject_timestamp_h264_annex_b(&[0, 0, 0, 1, 0x65, 1], 10).expect("timestamp");
        let chunk = chunk_manifest(b"manifest", &[9; 16], 1, ManifestCompression::None)
            .expect("chunk")
            .remove(0);
        let enriched = inject_user_data_h264_annex_b(&base, &[manifest_chunk_sei(&chunk)])
            .expect("manifest SEI");
        assert!(inspect_h264_annex_b(&enriched, true).is_err());
    }
}
