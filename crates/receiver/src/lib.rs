//! Authenticated Receiver bootstrap registry and bounded pre-decode ingest.

pub mod replay;
pub mod runtime;
pub mod session;
pub mod synchronize;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use robot_multicam_metadata_codec::{inspect_h264_annex_b, CodecError, UserDataUnregistered};
use robot_multicam_stream_identity::{IdentityError, Role, StreamIdentity};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapState {
    Listening,
    TransportIdentified,
    MediaProbing,
    ProvisionalStream,
    ManifestValidated,
    DatasetReady,
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamKey {
    pub session_id: uuid::Uuid,
    pub camera_id: String,
    pub epoch: u32,
}

#[derive(Debug, Clone)]
pub struct ReceiverPolicy {
    pub expected_embodiment_id: String,
    pub expected_edge_instance_id: Option<String>,
    pub base_port: u16,
    pub max_cameras: u16,
    pub max_ingest_frames: usize,
    pub max_ingest_bytes: usize,
}

#[derive(Debug, Error)]
pub enum ReceiverError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("unexpected embodiment ID")]
    Embodiment,
    #[error("unexpected edge instance ID")]
    EdgeInstance,
    #[error("stream slot exceeds configured camera count")]
    Slot,
    #[error("duplicate active camera or stream slot")]
    Duplicate,
    #[error("stale stream epoch")]
    StaleEpoch,
    #[error("registry capacity is exhausted")]
    Capacity,
    #[error("unknown stream")]
    Unknown,
    #[error("invalid bootstrap transition from {from:?} to {to:?}")]
    Transition {
        from: BootstrapState,
        to: BootstrapState,
    },
    #[error("receiver registry lock is poisoned")]
    Poisoned,
    #[error(transparent)]
    Ingest(#[from] IngestError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrameEnvelope {
    pub key: StreamKey,
    pub pts_ns: u64,
    pub access_unit_ordinal: u64,
    pub capture_time_edge_ns: u64,
    pub encoded_au: Vec<u8>,
    pub metadata_messages: Vec<UserDataUnregistered>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSnapshot {
    pub key: StreamKey,
    pub role: Role,
    pub slot: u16,
    pub listen_port: u16,
    pub state: BootstrapState,
    pub connected: bool,
    pub last_capture_time_edge_ns: Option<u64>,
    pub received_frames: u64,
    pub dropped_frames: u64,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("access unit has no normalized PTS")]
    MissingPts,
    #[error("access unit has no synchronization timestamp")]
    MissingTimestamp,
    #[error("PTS or capture timestamp is duplicate/non-monotonic")]
    NonMonotonic,
    #[error("access unit exceeds the bounded ingest byte capacity")]
    AccessUnitTooLarge,
}

#[derive(Debug)]
struct IngestQueue {
    items: VecDeque<EncodedFrameEnvelope>,
    bytes: usize,
    max_frames: usize,
    max_bytes: usize,
    dropped: u64,
}

impl IngestQueue {
    fn new(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(max_frames),
            bytes: 0,
            max_frames,
            max_bytes,
            dropped: 0,
        }
    }

    fn push(&mut self, item: EncodedFrameEnvelope) -> Result<(), IngestError> {
        let item_bytes = item.encoded_au.len();
        if item_bytes > self.max_bytes {
            return Err(IngestError::AccessUnitTooLarge);
        }
        while self.items.len() >= self.max_frames
            || self.bytes.saturating_add(item_bytes) > self.max_bytes
        {
            let Some(removed) = self.items.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(removed.encoded_au.len());
            self.dropped = self.dropped.saturating_add(1);
        }
        self.bytes += item_bytes;
        self.items.push_back(item);
        Ok(())
    }
}

#[derive(Debug)]
struct StreamRecord {
    identity: StreamIdentity,
    listen_port: u16,
    state: BootstrapState,
    connected: bool,
    last_pts_ns: Option<u64>,
    last_capture_time_edge_ns: Option<u64>,
    next_ordinal: u64,
    ingest: IngestQueue,
}

#[derive(Debug, Default)]
struct RegistryState {
    streams: BTreeMap<StreamKey, StreamRecord>,
    highest_epoch: BTreeMap<(uuid::Uuid, String), u32>,
}

#[derive(Debug)]
pub struct ReceiverRegistry {
    policy: ReceiverPolicy,
    key: Vec<u8>,
    state: Mutex<RegistryState>,
}

impl ReceiverRegistry {
    #[must_use]
    pub fn new(policy: ReceiverPolicy, hmac_key: Vec<u8>) -> Self {
        Self {
            policy,
            key: hmac_key,
            state: Mutex::new(RegistryState::default()),
        }
    }

    pub fn accept(
        &self,
        listen_port: u16,
        raw_stream_id: &str,
    ) -> Result<StreamKey, ReceiverError> {
        let identity = StreamIdentity::parse_and_verify(raw_stream_id, &self.key)?;
        if identity.embodiment_id != self.policy.expected_embodiment_id {
            return Err(ReceiverError::Embodiment);
        }
        if self
            .policy
            .expected_edge_instance_id
            .as_ref()
            .is_some_and(|expected| *expected != identity.edge_instance_id)
        {
            return Err(ReceiverError::EdgeInstance);
        }
        if identity.slot >= self.policy.max_cameras {
            return Err(ReceiverError::Slot);
        }
        identity.validate_listen_port(self.policy.base_port, listen_port)?;
        let key = StreamKey {
            session_id: identity.session_id,
            camera_id: identity.camera_id.clone(),
            epoch: identity.epoch,
        };
        let mut state = self.state.lock().map_err(|_| ReceiverError::Poisoned)?;
        if state
            .streams
            .values()
            .filter(|record| record.connected)
            .count()
            >= usize::from(self.policy.max_cameras)
        {
            return Err(ReceiverError::Capacity);
        }
        if state.streams.values().any(|record| {
            record.connected
                && record.identity.session_id == identity.session_id
                && (record.identity.camera_id == identity.camera_id
                    || record.identity.slot == identity.slot)
        }) {
            return Err(ReceiverError::Duplicate);
        }
        let history_key = (identity.session_id, identity.camera_id.clone());
        if state
            .highest_epoch
            .get(&history_key)
            .is_some_and(|epoch| identity.epoch < *epoch)
        {
            return Err(ReceiverError::StaleEpoch);
        }
        state.highest_epoch.insert(history_key, identity.epoch);
        state.streams.insert(
            key.clone(),
            StreamRecord {
                identity,
                listen_port,
                state: BootstrapState::TransportIdentified,
                connected: true,
                last_pts_ns: None,
                last_capture_time_edge_ns: None,
                next_ordinal: 0,
                ingest: IngestQueue::new(
                    self.policy.max_ingest_frames,
                    self.policy.max_ingest_bytes,
                ),
            },
        );
        Ok(key)
    }

    pub fn transition(&self, key: &StreamKey, to: BootstrapState) -> Result<(), ReceiverError> {
        let mut state = self.state.lock().map_err(|_| ReceiverError::Poisoned)?;
        let record = state.streams.get_mut(key).ok_or(ReceiverError::Unknown)?;
        let valid = matches!(
            (record.state, to),
            (
                BootstrapState::TransportIdentified,
                BootstrapState::MediaProbing
            ) | (
                BootstrapState::MediaProbing,
                BootstrapState::ProvisionalStream
            ) | (
                BootstrapState::ProvisionalStream,
                BootstrapState::ManifestValidated
            ) | (
                BootstrapState::ManifestValidated,
                BootstrapState::DatasetReady
            ) | (_, BootstrapState::Quarantined)
        );
        if !valid {
            return Err(ReceiverError::Transition {
                from: record.state,
                to,
            });
        }
        record.state = to;
        Ok(())
    }

    pub fn disconnect(&self, key: &StreamKey) -> Result<(), ReceiverError> {
        let mut state = self.state.lock().map_err(|_| ReceiverError::Poisoned)?;
        let record = state.streams.get_mut(key).ok_or(ReceiverError::Unknown)?;
        record.connected = false;
        record.state = BootstrapState::Listening;
        Ok(())
    }

    pub fn ingest_h264_before_decode(
        &self,
        key: &StreamKey,
        pts_ns: Option<u64>,
        access_unit: &[u8],
    ) -> Result<EncodedFrameEnvelope, ReceiverError> {
        let pts_ns = pts_ns.ok_or(IngestError::MissingPts)?;
        let mut state = self.state.lock().map_err(|_| ReceiverError::Poisoned)?;
        let record = state.streams.get_mut(key).ok_or(ReceiverError::Unknown)?;
        let inspected = inspect_h264_annex_b(access_unit, record.identity.role == Role::Secondary)
            .map_err(IngestError::from)?
            .ok_or(IngestError::MissingTimestamp)?;
        let capture = inspected.timestamp.capture_time_edge_ns;
        if record.last_pts_ns.is_some_and(|last| pts_ns <= last)
            || record
                .last_capture_time_edge_ns
                .is_some_and(|last| capture <= last)
        {
            return Err(IngestError::NonMonotonic.into());
        }
        let envelope = EncodedFrameEnvelope {
            key: key.clone(),
            pts_ns,
            access_unit_ordinal: record.next_ordinal,
            capture_time_edge_ns: capture,
            encoded_au: access_unit.to_vec(),
            metadata_messages: inspected.messages,
        };
        record.ingest.push(envelope.clone())?;
        record.last_pts_ns = Some(pts_ns);
        record.last_capture_time_edge_ns = Some(capture);
        record.next_ordinal = record.next_ordinal.saturating_add(1);
        Ok(envelope)
    }

    pub fn state(&self, key: &StreamKey) -> Result<BootstrapState, ReceiverError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ReceiverError::Poisoned)?
            .streams
            .get(key)
            .ok_or(ReceiverError::Unknown)?
            .state)
    }

    pub fn dropped_frames(&self, key: &StreamKey) -> Result<u64, ReceiverError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ReceiverError::Poisoned)?
            .streams
            .get(key)
            .ok_or(ReceiverError::Unknown)?
            .ingest
            .dropped)
    }

    pub fn listen_port(&self, key: &StreamKey) -> Result<u16, ReceiverError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ReceiverError::Poisoned)?
            .streams
            .get(key)
            .ok_or(ReceiverError::Unknown)?
            .listen_port)
    }

    pub fn snapshots(
        &self,
        session_id: Option<uuid::Uuid>,
    ) -> Result<Vec<StreamSnapshot>, ReceiverError> {
        let state = self.state.lock().map_err(|_| ReceiverError::Poisoned)?;
        Ok(state
            .streams
            .iter()
            .filter(|(key, _)| session_id.is_none_or(|session| key.session_id == session))
            .map(|(key, record)| StreamSnapshot {
                key: key.clone(),
                role: record.identity.role,
                slot: record.identity.slot,
                listen_port: record.listen_port,
                state: record.state,
                connected: record.connected,
                last_capture_time_edge_ns: record.last_capture_time_edge_ns,
                received_frames: record.next_ordinal,
                dropped_frames: record.ingest.dropped,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{BootstrapState, ReceiverPolicy, ReceiverRegistry};
    use robot_multicam_metadata_codec::inject_timestamp_h264_annex_b;
    use serde_json::Value;

    fn fixture() -> (Vec<u8>, String) {
        let value: Value =
            serde_json::from_str(include_str!("../../../testdata/streamid_vectors.json"))
                .expect("fixture");
        (
            hex::decode(value["hmac_key_hex"].as_str().expect("key")).expect("hex"),
            value["canonical_signed"]
                .as_str()
                .expect("stream ID")
                .to_owned(),
        )
    }

    fn registry() -> ReceiverRegistry {
        let (key, _) = fixture();
        ReceiverRegistry::new(
            ReceiverPolicy {
                expected_embodiment_id: "robot-cell-001".to_owned(),
                expected_edge_instance_id: Some("edge-robot-cell-001".to_owned()),
                base_port: 10_000,
                max_cameras: 4,
                max_ingest_frames: 2,
                max_ingest_bytes: 1_024,
            },
            key,
        )
    }

    #[test]
    fn identity_port_and_lifecycle_are_validated_before_media() {
        let (_, raw) = fixture();
        let registry = registry();
        let key = registry.accept(10_000, &raw).expect("accept");
        assert_eq!(registry.listen_port(&key).expect("port"), 10_000);
        registry
            .transition(&key, BootstrapState::MediaProbing)
            .expect("probe");
        registry
            .transition(&key, BootstrapState::ProvisionalStream)
            .expect("provisional");
        assert_eq!(
            registry.state(&key).expect("state"),
            BootstrapState::ProvisionalStream
        );
        assert!(registry.accept(10_001, &raw).is_err());
    }

    #[test]
    fn bounded_ingest_drops_oldest_and_rejects_non_monotonic_frames() {
        let (_, raw) = fixture();
        let registry = registry();
        let key = registry.accept(10_000, &raw).expect("accept");
        let idr = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
        for index in 1..=3 {
            let au = inject_timestamp_h264_annex_b(&idr, index * 100).expect("inject");
            registry
                .ingest_h264_before_decode(&key, Some(index * 10), &au)
                .expect("ingest");
        }
        assert_eq!(registry.dropped_frames(&key).expect("drops"), 1);
        let duplicate = inject_timestamp_h264_annex_b(&idr, 300).expect("inject");
        assert!(registry
            .ingest_h264_before_decode(&key, Some(30), &duplicate)
            .is_err());
    }

    #[test]
    fn reconnect_preserves_identity_and_allows_epoch_refresh() {
        let (key_bytes, raw) = fixture();
        let registry = registry();
        let first = registry.accept(10_000, &raw).expect("accept");
        registry.disconnect(&first).expect("disconnect");
        let same = registry.accept(10_000, &raw).expect("reconnect");
        assert_eq!(same.epoch, 1);
        registry.disconnect(&same).expect("disconnect");

        let parsed =
            robot_multicam_stream_identity::StreamIdentity::parse_and_verify(&raw, &key_bytes)
                .expect("parse");
        let next = robot_multicam_stream_identity::StreamIdentity { epoch: 2, ..parsed }
            .encode_signed(&key_bytes)
            .expect("encode");
        let refreshed = registry.accept(10_000, &next).expect("new epoch");
        assert_eq!(refreshed.epoch, 2);
    }
}
