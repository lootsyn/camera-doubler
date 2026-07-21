//! Hash-verified raw segment indexing and deterministic replay admission.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use robot_multicam_stream_identity::{Codec, Role, StreamIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_INDEX_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SegmentIndexEntry {
    pub connection_id: String,
    pub camera_id: String,
    pub stream_epoch: u32,
    pub first_normalized_pts_ns: u64,
    pub last_normalized_pts_ns: u64,
    pub first_capture_time_edge_ns: u64,
    pub last_capture_time_edge_ns: u64,
    pub relative_path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamEnvelopeFields {
    pub embodiment_id: String,
    pub edge_instance_id: String,
    pub edge_boot_id: String,
    pub session_id: String,
    pub camera_id: String,
    pub slot: u16,
    pub epoch: u32,
    pub role: String,
    pub codec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StreamEnvelope {
    pub accepted_at_utc: String,
    pub listen_port: u16,
    pub raw_stream_id: String,
    pub stream_id_fields: StreamEnvelopeFields,
    pub stream_id_auth: String,
    pub peer_address: String,
    pub gstreamer_version: String,
}

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("index or segment I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("index JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("index line, identifier, time range, or relative path is invalid")]
    InvalidIndex,
    #[error("segment byte count or SHA-256 digest mismatch")]
    HashMismatch,
    #[error("stream envelope or authenticated transport identity is invalid")]
    InvalidEnvelope,
    #[error("stream identity authentication failed: {0}")]
    Identity(#[from] robot_multicam_stream_identity::IdentityError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub removed: Vec<PathBuf>,
}

pub fn load_verified_index(
    session_root: &Path,
    index_path: &Path,
    max_entries: usize,
) -> Result<Vec<SegmentIndexEntry>, ReplayError> {
    if max_entries == 0 {
        return Err(ReplayError::InvalidIndex);
    }
    let mut entries = Vec::new();
    for line in BufReader::new(File::open(index_path)?).lines() {
        let line = line?;
        if line.is_empty() || line.len() > MAX_INDEX_LINE_BYTES || entries.len() == max_entries {
            return Err(ReplayError::InvalidIndex);
        }
        let entry: SegmentIndexEntry = serde_json::from_str(&line)?;
        validate_entry(session_root, &entry)?;
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(ReplayError::InvalidIndex);
    }
    Ok(entries)
}

pub fn load_verified_archive(
    session_root: &Path,
    envelope_path: &Path,
    index_path: &Path,
    hmac_key: &[u8],
    base_port: u16,
    max_entries: usize,
) -> Result<(StreamIdentity, Vec<SegmentIndexEntry>), ReplayError> {
    let envelope_bytes = fs::read(envelope_path)?;
    if envelope_bytes.is_empty() || envelope_bytes.len() > 32 * 1024 {
        return Err(ReplayError::InvalidEnvelope);
    }
    let envelope: StreamEnvelope = serde_json::from_slice(&envelope_bytes)?;
    let identity = StreamIdentity::parse_and_verify(&envelope.raw_stream_id, hmac_key)?;
    let fields = &envelope.stream_id_fields;
    let expected_port = base_port
        .checked_add(identity.slot)
        .ok_or(ReplayError::InvalidEnvelope)?;
    if envelope.stream_id_auth != "valid"
        || envelope.accepted_at_utc.len() < 20
        || !envelope.accepted_at_utc.contains('T')
        || !envelope.accepted_at_utc.ends_with('Z')
        || envelope.peer_address.is_empty()
        || envelope.peer_address.len() > 256
        || envelope.gstreamer_version.is_empty()
        || envelope.gstreamer_version.len() > 64
        || envelope.listen_port != expected_port
        || fields.embodiment_id != identity.embodiment_id
        || fields.edge_instance_id != identity.edge_instance_id
        || fields.edge_boot_id != identity.edge_boot_id.to_string()
        || fields.session_id != identity.session_id.to_string()
        || fields.camera_id != identity.camera_id
        || fields.slot != identity.slot
        || fields.epoch != identity.epoch
        || fields.role != role_name(identity.role)
        || fields.codec != codec_name(identity.codec)
    {
        return Err(ReplayError::InvalidEnvelope);
    }
    let entries = load_verified_index(session_root, index_path, max_entries)?;
    if entries.iter().any(|entry| {
        entry.connection_id != identity.session_id.to_string()
            || entry.camera_id != identity.camera_id
            || entry.stream_epoch != identity.epoch
    }) {
        return Err(ReplayError::InvalidEnvelope);
    }
    Ok((identity, entries))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Anchor => "anchor",
        Role::Secondary => "secondary",
    }
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::H265 => "h265",
    }
}

fn validate_entry(root: &Path, entry: &SegmentIndexEntry) -> Result<(), ReplayError> {
    if entry.camera_id.is_empty()
        || entry.camera_id.len() > 128
        || entry.connection_id.is_empty()
        || entry.connection_id.len() > 128
        || entry.last_normalized_pts_ns < entry.first_normalized_pts_ns
        || entry.first_capture_time_edge_ns == 0
        || entry.last_capture_time_edge_ns < entry.first_capture_time_edge_ns
        || entry.relative_path.is_absolute()
        || entry
            .relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || entry.sha256.len() != 64
        || entry.bytes == 0
    {
        return Err(ReplayError::InvalidIndex);
    }
    let path = root.join(&entry.relative_path);
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(count).map_err(|_| ReplayError::HashMismatch)?)
            .ok_or(ReplayError::HashMismatch)?;
        hasher.update(&buffer[..count]);
    }
    if bytes != entry.bytes || hex::encode(hasher.finalize()) != entry.sha256 {
        return Err(ReplayError::HashMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskReadiness {
    Ready,
    Low,
    Full,
}

#[must_use]
pub fn disk_readiness(
    free_bytes: u64,
    minimum_free_bytes: u64,
    spool_bytes: u64,
    spool_cap: u64,
) -> DiskReadiness {
    if free_bytes == 0 || spool_bytes >= spool_cap {
        DiskReadiness::Full
    } else if free_bytes < minimum_free_bytes || spool_bytes >= spool_cap.saturating_mul(9) / 10 {
        DiskReadiness::Low
    } else {
        DiskReadiness::Ready
    }
}

pub fn enforce_retention(
    session_root: &Path,
    entries: &[SegmentIndexEntry],
    spool_cap_bytes: u64,
    protected_paths: &BTreeSet<PathBuf>,
) -> Result<RetentionReport, ReplayError> {
    if spool_cap_bytes == 0 {
        return Err(ReplayError::InvalidIndex);
    }
    let mut candidates = entries.to_vec();
    for entry in &candidates {
        if entry.relative_path.is_absolute()
            || entry
                .relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(ReplayError::InvalidIndex);
        }
    }
    candidates.sort_by_key(|entry| {
        (
            entry.first_capture_time_edge_ns,
            entry.camera_id.clone(),
            entry.relative_path.clone(),
        )
    });
    let bytes_before = candidates.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.bytes)
            .ok_or(ReplayError::HashMismatch)
    })?;
    let mut bytes_after = bytes_before;
    let mut removed = Vec::new();
    for entry in candidates {
        if bytes_after <= spool_cap_bytes {
            break;
        }
        if protected_paths.contains(&entry.relative_path) {
            continue;
        }
        let target = session_root.join(&entry.relative_path);
        match fs::remove_file(&target) {
            Ok(()) => {
                bytes_after = bytes_after.saturating_sub(entry.bytes);
                removed.push(entry.relative_path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ReplayError::HashMismatch)
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(RetentionReport {
        bytes_before,
        bytes_after,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        disk_readiness, enforce_retention, load_verified_archive, load_verified_index,
        DiskReadiness, SegmentIndexEntry, StreamEnvelope, StreamEnvelopeFields,
    };
    use robot_multicam_stream_identity::{Codec, Role, StreamIdentity};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn replay_verifies_exact_segment_hash() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("one.ts"), b"segment").expect("segment");
        let digest = Sha256::digest(b"segment");
        let line = serde_json::json!({
            "connection_id": "00000000-0000-0000-0000-000000000001",
            "camera_id": "cam-a",
            "stream_epoch": 1,
            "first_normalized_pts_ns": 0,
            "last_normalized_pts_ns": 1,
            "first_capture_time_edge_ns": 1,
            "last_capture_time_edge_ns": 2,
            "relative_path": "one.ts",
            "sha256": hex::encode(digest),
            "bytes": 7
        });
        let index = root.path().join("index.jsonl");
        fs::write(&index, format!("{line}\n")).expect("index");
        assert_eq!(
            load_verified_index(root.path(), &index, 10)
                .expect("verify")
                .len(),
            1
        );
        fs::write(root.path().join("one.ts"), b"changed").expect("mutate");
        assert!(load_verified_index(root.path(), &index, 10).is_err());
    }

    #[test]
    fn disk_pressure_is_never_silent() {
        assert_eq!(disk_readiness(100, 10, 1, 10), DiskReadiness::Ready);
        assert_eq!(disk_readiness(5, 10, 1, 10), DiskReadiness::Low);
        assert_eq!(disk_readiness(100, 10, 10, 10), DiskReadiness::Full);
    }

    #[test]
    fn archive_envelope_authenticates_and_cross_checks_index() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("one.ts"), b"segment").expect("segment");
        let session = uuid::Uuid::from_bytes([1; 16]);
        let identity = StreamIdentity {
            embodiment_id: "cell".to_owned(),
            edge_instance_id: "edge".to_owned(),
            edge_boot_id: uuid::Uuid::from_bytes([2; 16]),
            session_id: session,
            camera_id: "cam-a".to_owned(),
            slot: 3,
            epoch: 4,
            role: Role::Secondary,
            codec: Codec::H264,
        };
        let key = vec![9; 32];
        let raw_stream_id = identity.encode_signed(&key).expect("signed identity");
        let envelope = StreamEnvelope {
            accepted_at_utc: "2026-07-22T00:00:00Z".to_owned(),
            listen_port: 10_003,
            raw_stream_id,
            stream_id_fields: StreamEnvelopeFields {
                embodiment_id: identity.embodiment_id.clone(),
                edge_instance_id: identity.edge_instance_id.clone(),
                edge_boot_id: identity.edge_boot_id.to_string(),
                session_id: session.to_string(),
                camera_id: identity.camera_id.clone(),
                slot: identity.slot,
                epoch: identity.epoch,
                role: "secondary".to_owned(),
                codec: "h264".to_owned(),
            },
            stream_id_auth: "valid".to_owned(),
            peer_address: "127.0.0.1:40000".to_owned(),
            gstreamer_version: "1.24.2".to_owned(),
        };
        let envelope_path = root.path().join("stream-envelope.json");
        fs::write(
            &envelope_path,
            serde_json::to_vec(&envelope).expect("envelope json"),
        )
        .expect("envelope");
        let digest = Sha256::digest(b"segment");
        let entry = SegmentIndexEntry {
            connection_id: session.to_string(),
            camera_id: "cam-a".to_owned(),
            stream_epoch: 4,
            first_normalized_pts_ns: 1,
            last_normalized_pts_ns: 2,
            first_capture_time_edge_ns: 3,
            last_capture_time_edge_ns: 4,
            relative_path: PathBuf::from("one.ts"),
            sha256: hex::encode(digest),
            bytes: 7,
        };
        let index_path = root.path().join("index.jsonl");
        fs::write(
            &index_path,
            format!("{}\n", serde_json::to_string(&entry).expect("entry json")),
        )
        .expect("index");
        let (loaded, entries) =
            load_verified_archive(root.path(), &envelope_path, &index_path, &key, 10_000, 8)
                .expect("verified archive");
        assert_eq!(loaded, identity);
        assert_eq!(entries, vec![entry]);

        let mut tampered = envelope;
        tampered.listen_port = 10_004;
        fs::write(
            &envelope_path,
            serde_json::to_vec(&tampered).expect("tampered json"),
        )
        .expect("tampered envelope");
        assert!(
            load_verified_archive(root.path(), &envelope_path, &index_path, &key, 10_000, 8,)
                .is_err()
        );
    }

    #[test]
    fn retention_deletes_oldest_unprotected_segment_within_exact_root() {
        let root = tempdir().expect("root");
        fs::write(root.path().join("old.ts"), b"old").expect("old");
        fs::write(root.path().join("active.ts"), b"active").expect("active");
        let entries = vec![entry("old.ts", 1, 3), entry("active.ts", 2, 6)];
        let report = enforce_retention(
            root.path(),
            &entries,
            6,
            &BTreeSet::from([PathBuf::from("active.ts")]),
        )
        .expect("retention");
        assert_eq!(report.removed, vec![PathBuf::from("old.ts")]);
        assert!(!root.path().join("old.ts").exists());
        assert!(root.path().join("active.ts").exists());
    }

    fn entry(path: &str, time: u64, bytes: u64) -> SegmentIndexEntry {
        SegmentIndexEntry {
            connection_id: "00000000-0000-0000-0000-000000000001".to_owned(),
            camera_id: "cam".to_owned(),
            stream_epoch: 1,
            first_normalized_pts_ns: time,
            last_normalized_pts_ns: time,
            first_capture_time_edge_ns: time,
            last_capture_time_edge_ns: time,
            relative_path: PathBuf::from(path),
            sha256: "0".repeat(64),
            bytes,
        }
    }
}
