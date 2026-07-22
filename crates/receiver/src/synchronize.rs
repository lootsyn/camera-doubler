//! Authoritative manifest validation and bounded synchronized-step grouping.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use prost::Message;
use robot_multicam_metadata_codec::decode_anchor_context_packet;
use robot_multicam_protocol::multicam::{AnchorFrameContextV1, CameraRoleV1, SessionManifestV1};
use robot_multicam_protocol::receiver::{FrameReference, SynchronizedDatasetStep};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedCamera {
    pub camera_id: String,
    pub stream_slot: u32,
    pub stream_epoch: u32,
    pub listen_port: u16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SynchronizeError {
    #[error("manifest protobuf or identity is invalid")]
    Manifest,
    #[error("manifest camera catalog conflicts with authenticated transports")]
    CameraCatalog,
    #[error("manifest feature slices are not deterministic and contiguous")]
    FeatureLayout,
    #[error("anchor context packet is invalid or conflicts with the manifest")]
    Context,
    #[error("required camera frame is outside skew tolerance")]
    MissingCamera,
    #[error("frame queue is duplicate/non-monotonic")]
    NonMonotonic,
}

pub fn validate_manifest(
    bytes: &[u8],
    transports: &[ConnectedCamera],
    base_port: u16,
) -> Result<SessionManifestV1, SynchronizeError> {
    let manifest = SessionManifestV1::decode(bytes).map_err(|_| SynchronizeError::Manifest)?;
    if manifest.schema_version != 1
        || manifest.session_id.len() != 16
        || manifest.edge_boot_id.len() != 16
        || manifest.manifest_revision == 0
        || manifest.anchor_camera_id.is_empty()
        || manifest.stream_id_schema != "rmc1"
        || manifest.schema_id_algorithm != robot_multicam_protocol::constants::SCHEMA_ID_HASH
    {
        return Err(SynchronizeError::Manifest);
    }
    let transport_index: BTreeMap<_, _> = transports
        .iter()
        .map(|camera| (camera.camera_id.as_str(), camera))
        .collect();
    let mut camera_ids = BTreeSet::new();
    let mut anchor_count = 0;
    for camera in &manifest.cameras {
        if camera.stable_camera_id.is_empty() || !camera_ids.insert(&camera.stable_camera_id) {
            return Err(SynchronizeError::CameraCatalog);
        }
        if camera.role == CameraRoleV1::Anchor as i32 {
            anchor_count += 1;
            if camera.stable_camera_id != manifest.anchor_camera_id || camera.stream_excluded {
                return Err(SynchronizeError::CameraCatalog);
            }
        }
        if !camera.stream_excluded {
            let connected = transport_index
                .get(camera.stable_camera_id.as_str())
                .ok_or(SynchronizeError::CameraCatalog)?;
            let expected_port = u32::from(base_port)
                .checked_add(camera.stream_slot)
                .ok_or(SynchronizeError::CameraCatalog)?;
            if connected.stream_slot != camera.stream_slot
                || connected.stream_epoch != camera.stream_epoch
                || u32::from(connected.listen_port) != expected_port
                || camera.transport_port != expected_port
            {
                return Err(SynchronizeError::CameraCatalog);
            }
        }
    }
    if anchor_count != 1
        || transports.len()
            != manifest
                .cameras
                .iter()
                .filter(|c| !c.stream_excluded)
                .count()
    {
        return Err(SynchronizeError::CameraCatalog);
    }
    validate_layout(&manifest)?;
    Ok(manifest)
}

fn validate_layout(manifest: &SessionManifestV1) -> Result<(), SynchronizeError> {
    let mut expected = BTreeMap::from([(1, 0_u32), (2, 0), (3, 0)]);
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for feature in &manifest.feature_slices {
        let offset = expected
            .get_mut(&feature.vector_kind)
            .ok_or(SynchronizeError::FeatureLayout)?;
        if feature.feature_id == 0
            || feature.qualified_name.is_empty()
            || feature.length == 0
            || feature.offset != *offset
            || !names.insert(feature.qualified_name.as_str())
            || !ids.insert(feature.feature_id)
        {
            return Err(SynchronizeError::FeatureLayout);
        }
        *offset = offset
            .checked_add(feature.length)
            .ok_or(SynchronizeError::FeatureLayout)?;
    }
    if expected[&1] != manifest.observation_vector_length
        || expected[&2] != manifest.action_vector_length
        || expected[&3] != manifest.auxiliary_vector_length
    {
        return Err(SynchronizeError::FeatureLayout);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFrame {
    pub camera_id: String,
    pub capture_time_edge_ns: u64,
    pub stream_epoch: u32,
    pub normalized_pts_ns: u64,
    pub access_unit_ordinal: u64,
    pub storage_uri: String,
    pub encoded_image: Vec<u8>,
}

#[derive(Debug)]
pub struct StepSynchronizer {
    manifest: SessionManifestV1,
    max_frames_per_camera: usize,
    max_skew_ns: u64,
    queues: BTreeMap<String, VecDeque<StoredFrame>>,
}

impl StepSynchronizer {
    pub fn new(
        manifest: SessionManifestV1,
        max_frames_per_camera: usize,
        max_skew_ns: u64,
    ) -> Result<Self, SynchronizeError> {
        if max_frames_per_camera == 0 || max_skew_ns == 0 {
            return Err(SynchronizeError::Manifest);
        }
        let queues = manifest
            .cameras
            .iter()
            .filter(|camera| !camera.stream_excluded)
            .map(|camera| (camera.stable_camera_id.clone(), VecDeque::new()))
            .collect();
        Ok(Self {
            manifest,
            max_frames_per_camera,
            max_skew_ns,
            queues,
        })
    }

    pub fn push_secondary(&mut self, frame: StoredFrame) -> Result<(), SynchronizeError> {
        if frame.camera_id == self.manifest.anchor_camera_id {
            return Err(SynchronizeError::NonMonotonic);
        }
        let queue = self
            .queues
            .get_mut(&frame.camera_id)
            .ok_or(SynchronizeError::CameraCatalog)?;
        if queue.back().is_some_and(|last| {
            frame.capture_time_edge_ns <= last.capture_time_edge_ns
                || frame.access_unit_ordinal <= last.access_unit_ordinal
        }) {
            return Err(SynchronizeError::NonMonotonic);
        }
        if queue.len() == self.max_frames_per_camera {
            queue.pop_front();
        }
        queue.push_back(frame);
        Ok(())
    }

    pub fn anchor_step(
        &mut self,
        anchor: StoredFrame,
        context_packet_bytes: &[u8],
        include_encoded_images: bool,
    ) -> Result<SynchronizedDatasetStep, SynchronizeError> {
        if anchor.camera_id != self.manifest.anchor_camera_id {
            return Err(SynchronizeError::Context);
        }
        let (packet, context) = decode_anchor_context_packet(context_packet_bytes)
            .map_err(|_| SynchronizeError::Context)?;
        validate_context(&self.manifest, &anchor, &context)?;
        let mut frames = vec![reference(&anchor, 0, include_encoded_images)];
        for camera in self.manifest.cameras.iter().filter(|camera| {
            !camera.stream_excluded && camera.stable_camera_id != self.manifest.anchor_camera_id
        }) {
            let queue = self
                .queues
                .get_mut(&camera.stable_camera_id)
                .ok_or(SynchronizeError::MissingCamera)?;
            let (index, skew) = queue
                .iter()
                .enumerate()
                .map(|(index, frame)| {
                    let signed = i128::from(frame.capture_time_edge_ns)
                        - i128::from(anchor.capture_time_edge_ns);
                    (index, signed)
                })
                .min_by_key(|(_, skew)| skew.unsigned_abs())
                .ok_or(SynchronizeError::MissingCamera)?;
            if skew.unsigned_abs() > u128::from(self.max_skew_ns) {
                return Err(SynchronizeError::MissingCamera);
            }
            let selected = queue.remove(index).ok_or(SynchronizeError::MissingCamera)?;
            while queue
                .front()
                .is_some_and(|frame| frame.capture_time_edge_ns <= selected.capture_time_edge_ns)
            {
                queue.pop_front();
            }
            let skew = i64::try_from(skew).map_err(|_| SynchronizeError::MissingCamera)?;
            frames.push(reference(&selected, skew, include_encoded_images));
        }
        frames.sort_by(|left, right| left.camera_id.cmp(&right.camera_id));
        Ok(SynchronizedDatasetStep {
            session_id: self.manifest.session_id.clone(),
            manifest_revision: self.manifest.manifest_revision,
            capture_time_edge_ns: anchor.capture_time_edge_ns,
            frames,
            observation_state: context.observation_state.clone(),
            action: context.action.clone(),
            auxiliary: context.auxiliary.clone(),
            valid: context.invalid_reason.is_empty(),
            invalid_reason: context.invalid_reason.clone(),
            anchor_context: Some(context),
            anchor_context_packet: Some(packet),
            ..Default::default()
        })
    }
}

fn validate_context(
    manifest: &SessionManifestV1,
    anchor: &StoredFrame,
    context: &AnchorFrameContextV1,
) -> Result<(), SynchronizeError> {
    if context.schema_version != 1
        || context.session_id != manifest.session_id
        || context.manifest_revision != manifest.manifest_revision
        || context.observation_schema_id != manifest.observation_schema_id
        || context.action_schema_id != manifest.action_schema_id
        || context.observation_state.len() != manifest.observation_vector_length as usize
        || context.action.len() != manifest.action_vector_length as usize
        || context.auxiliary.len() != manifest.auxiliary_vector_length as usize
        || context.anchor_frame_seq != anchor.access_unit_ordinal
    {
        return Err(SynchronizeError::Context);
    }
    Ok(())
}

fn reference(frame: &StoredFrame, skew: i64, include: bool) -> FrameReference {
    FrameReference {
        camera_id: frame.camera_id.clone(),
        capture_time_edge_ns: frame.capture_time_edge_ns,
        skew_from_anchor_ns: skew,
        storage_uri: frame.storage_uri.clone(),
        encoded_image: if include {
            frame.encoded_image.clone().into()
        } else {
            bytes::Bytes::new()
        },
        encoded_image_media_type: if include {
            "video/h264".to_owned()
        } else {
            String::new()
        },
        stream_epoch: frame.stream_epoch,
        normalized_pts_ns: frame.normalized_pts_ns,
        access_unit_ordinal: frame.access_unit_ordinal,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use robot_multicam_metadata_codec::encode_anchor_context_packet;
    use robot_multicam_protocol::multicam::{
        AnchorFrameContextV1, CameraDescriptorV1, CameraRoleV1, FeatureSliceV1, FeatureVectorKind,
        SessionManifestV1,
    };

    use super::{validate_manifest, ConnectedCamera, StepSynchronizer, StoredFrame};

    #[test]
    fn authoritative_manifest_and_nearest_frame_form_a_step() {
        let manifest = manifest();
        let connections = vec![connected("anchor", 0), connected("side", 1)];
        let encoded = manifest.encode_to_vec();
        let validated = validate_manifest(&encoded, &connections, 10_000).expect("manifest");
        let mut sync = StepSynchronizer::new(validated, 4, 20).expect("synchronizer");
        sync.push_secondary(frame("side", 105, 0))
            .expect("side frame");
        let context = AnchorFrameContextV1 {
            schema_version: 1,
            session_id: vec![1; 16],
            anchor_frame_seq: 0,
            manifest_revision: 1,
            observation_schema_id: 11,
            action_schema_id: 12,
            observation_state: vec![1.0],
            action: vec![2.0],
            ..Default::default()
        };
        let packet = encode_anchor_context_packet(&context).expect("packet");
        let step = sync
            .anchor_step(frame("anchor", 100, 0), &packet, false)
            .expect("step");
        assert_eq!(step.frames.len(), 2);
        assert_eq!(step.observation_state, vec![1.0]);
        assert!(step
            .frames
            .iter()
            .all(|frame| frame.encoded_image.is_empty()));
    }

    #[test]
    fn role_port_epoch_or_anchor_conflicts_quarantine_manifest() {
        let mut value = manifest();
        value.cameras[1].stream_epoch = 2;
        assert!(validate_manifest(
            &value.encode_to_vec(),
            &[connected("anchor", 0), connected("side", 1)],
            10_000,
        )
        .is_err());
    }

    fn manifest() -> SessionManifestV1 {
        SessionManifestV1 {
            schema_version: 1,
            session_id: vec![1; 16],
            edge_boot_id: vec![2; 16],
            manifest_revision: 1,
            anchor_camera_id: "anchor".to_owned(),
            stream_id_schema: "rmc1".to_owned(),
            schema_id_algorithm: robot_multicam_protocol::constants::SCHEMA_ID_HASH.to_owned(),
            observation_schema_id: 11,
            action_schema_id: 12,
            observation_vector_length: 1,
            action_vector_length: 1,
            feature_slices: vec![
                feature(1, "body.position", FeatureVectorKind::Observation),
                feature(2, "body.target", FeatureVectorKind::Action),
            ],
            cameras: vec![camera("anchor", 0, true), camera("side", 1, false)],
            ..Default::default()
        }
    }

    fn feature(id: u64, name: &str, kind: FeatureVectorKind) -> FeatureSliceV1 {
        FeatureSliceV1 {
            feature_id: id,
            qualified_name: name.to_owned(),
            vector_kind: kind as i32,
            length: 1,
            ..Default::default()
        }
    }

    fn camera(id: &str, slot: u32, anchor: bool) -> CameraDescriptorV1 {
        CameraDescriptorV1 {
            stable_camera_id: id.to_owned(),
            stream_slot: slot,
            stream_epoch: 1,
            transport_port: 10_000 + slot,
            role: if anchor {
                CameraRoleV1::Anchor as i32
            } else {
                CameraRoleV1::Secondary as i32
            },
            ..Default::default()
        }
    }

    fn connected(id: &str, slot: u32) -> ConnectedCamera {
        ConnectedCamera {
            camera_id: id.to_owned(),
            stream_slot: slot,
            stream_epoch: 1,
            listen_port: u16::try_from(10_000 + slot).expect("port"),
        }
    }

    fn frame(id: &str, time: u64, ordinal: u64) -> StoredFrame {
        StoredFrame {
            camera_id: id.to_owned(),
            capture_time_edge_ns: time,
            stream_epoch: 1,
            normalized_pts_ns: time,
            access_unit_ordinal: ordinal,
            storage_uri: format!("file:///{id}"),
            encoded_image: vec![1],
        }
    }
}
