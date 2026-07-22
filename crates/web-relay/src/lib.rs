use robot_multicam_protocol::multicam::{FeatureVectorKind, SessionManifestV1};
use robot_multicam_protocol::receiver::{SessionStatus, SynchronizedDatasetStep};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("session UUID is invalid")]
    Session,
    #[error("anchor context is missing")]
    Context,
    #[error("feature slice is outside its vector")]
    FeatureSlice,
    #[error("feature vector kind is unsupported")]
    FeatureKind,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NamedFeature {
    pub feature_id: u64,
    pub qualified_name: String,
    pub semantic: String,
    pub source_device_id: String,
    pub vector_kind: String,
    pub unit: String,
    pub shape: Vec<u32>,
    pub values: Vec<f32>,
    pub valid: bool,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeviceFrameQuality {
    pub device_id: String,
    pub timestamp_quality: String,
    pub previous_sample_time_edge_ns: u64,
    pub next_sample_time_edge_ns: u64,
    pub max_feature_gap_ns: u64,
    pub clock_model_revision: u64,
    pub clock_residual_ns: i64,
    pub valid: bool,
    pub invalid_reason: String,
    pub action_source_quality: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FrameMetadataEvent {
    pub session_id: String,
    pub manifest_revision: u64,
    pub anchor_camera_id: String,
    pub anchor_frame_seq: u64,
    pub step_capture_time_edge_ns: u64,
    pub camera_id: String,
    pub camera_key: String,
    pub capture_time_edge_ns: u64,
    pub skew_from_anchor_ns: i64,
    pub stream_epoch: u32,
    pub normalized_pts_ns: u64,
    pub media_pts_seconds: f64,
    pub access_unit_ordinal: u64,
    pub encoded_bytes: usize,
    pub valid: bool,
    pub invalid_reason: String,
    pub observation_schema_id: u64,
    pub action_schema_id: u64,
    pub action_source_quality: String,
    pub validity_flags: Vec<String>,
    pub context_crc32c: u32,
    pub named_features: Vec<NamedFeature>,
    pub device_quality: Vec<DeviceFrameQuality>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StreamInfo {
    pub session_id: String,
    pub camera_id: String,
    pub camera_key: String,
    pub stream_epoch: u32,
    pub playlist_url: String,
    pub metadata_url: String,
    pub playlist_ready: bool,
    pub last_capture_time_edge_ns: u64,
    pub last_media_pts_seconds: f64,
    pub last_access_unit_ordinal: u64,
}

#[must_use]
pub fn camera_key(camera_id: &str) -> String {
    hex::encode(camera_id.as_bytes())
}

pub fn select_session(
    sessions: &[SessionStatus],
    configured: Option<Uuid>,
) -> Result<Option<Uuid>, ProjectionError> {
    if let Some(configured) = configured {
        return Ok(sessions
            .iter()
            .find(|status| status.session_id == configured.as_bytes())
            .filter(|status| status.authoritative && status.connected_cameras > 0)
            .map(|_| configured));
    }
    sessions
        .iter()
        .filter(|status| status.authoritative && status.connected_cameras > 0)
        .max_by_key(|status| status.last_capture_time_edge_ns)
        .map(|status| Uuid::from_slice(&status.session_id).map_err(|_| ProjectionError::Session))
        .transpose()
}

pub fn project_metadata(
    step: &SynchronizedDatasetStep,
    manifest: &SessionManifestV1,
) -> Result<Vec<FrameMetadataEvent>, ProjectionError> {
    let session_id = Uuid::from_slice(&step.session_id)
        .map_err(|_| ProjectionError::Session)?
        .to_string();
    let context = step
        .anchor_context
        .as_ref()
        .ok_or(ProjectionError::Context)?;
    let packet = step
        .anchor_context_packet
        .as_ref()
        .ok_or(ProjectionError::Context)?;
    let named_features = project_features(step, manifest, &context.feature_validity_bitmap)?;
    let device_quality = context
        .device_quality
        .iter()
        .map(|quality| DeviceFrameQuality {
            device_id: quality.device_id.clone(),
            timestamp_quality: enum_name_timestamp(quality.timestamp_quality),
            previous_sample_time_edge_ns: quality.previous_sample_time_edge_ns,
            next_sample_time_edge_ns: quality.next_sample_time_edge_ns,
            max_feature_gap_ns: quality.max_feature_gap_ns,
            clock_model_revision: quality.clock_model_revision,
            clock_residual_ns: quality.clock_residual_ns,
            valid: quality.valid,
            invalid_reason: quality.invalid_reason.clone(),
            action_source_quality: enum_name_action(quality.action_source_quality),
        })
        .collect::<Vec<_>>();
    let validity_flags = context
        .validity_flags
        .iter()
        .map(|value| {
            robot_multicam_protocol::multicam::FrameValidityFlag::try_from(*value).map_or_else(
                |_| format!("UNKNOWN_{value}"),
                |flag| flag.as_str_name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    Ok(step
        .frames
        .iter()
        .map(|frame| FrameMetadataEvent {
            session_id: session_id.clone(),
            manifest_revision: step.manifest_revision,
            anchor_camera_id: manifest.anchor_camera_id.clone(),
            anchor_frame_seq: context.anchor_frame_seq,
            step_capture_time_edge_ns: step.capture_time_edge_ns,
            camera_id: frame.camera_id.clone(),
            camera_key: camera_key(&frame.camera_id),
            capture_time_edge_ns: frame.capture_time_edge_ns,
            skew_from_anchor_ns: frame.skew_from_anchor_ns,
            stream_epoch: frame.stream_epoch,
            normalized_pts_ns: frame.normalized_pts_ns,
            media_pts_seconds: frame.normalized_pts_ns as f64 / 1_000_000_000.0,
            access_unit_ordinal: frame.access_unit_ordinal,
            encoded_bytes: frame.encoded_image.len(),
            valid: step.valid,
            invalid_reason: step.invalid_reason.clone(),
            observation_schema_id: context.observation_schema_id,
            action_schema_id: context.action_schema_id,
            action_source_quality: enum_name_action(context.action_source_quality),
            validity_flags: validity_flags.clone(),
            context_crc32c: packet.payload_crc32c,
            named_features: named_features.clone(),
            device_quality: device_quality.clone(),
        })
        .collect())
}

fn project_features(
    step: &SynchronizedDatasetStep,
    manifest: &SessionManifestV1,
    validity: &[u8],
) -> Result<Vec<NamedFeature>, ProjectionError> {
    manifest
        .feature_slices
        .iter()
        .enumerate()
        .map(|(index, feature)| {
            let kind = FeatureVectorKind::try_from(feature.vector_kind)
                .map_err(|_| ProjectionError::FeatureKind)?;
            let vector = match kind {
                FeatureVectorKind::Observation => &step.observation_state,
                FeatureVectorKind::Action => &step.action,
                FeatureVectorKind::Auxiliary => &step.auxiliary,
                FeatureVectorKind::Unspecified => return Err(ProjectionError::FeatureKind),
            };
            let begin =
                usize::try_from(feature.offset).map_err(|_| ProjectionError::FeatureSlice)?;
            let length =
                usize::try_from(feature.length).map_err(|_| ProjectionError::FeatureSlice)?;
            let end = begin
                .checked_add(length)
                .filter(|end| *end <= vector.len())
                .ok_or(ProjectionError::FeatureSlice)?;
            let valid = validity
                .get(index / 8)
                .is_some_and(|byte| byte & (1 << (index % 8)) != 0);
            Ok(NamedFeature {
                feature_id: feature.feature_id,
                qualified_name: feature.qualified_name.clone(),
                semantic: feature.semantic.clone(),
                source_device_id: feature.source_device_id.clone(),
                vector_kind: kind.as_str_name().to_owned(),
                unit: feature.unit.clone(),
                shape: feature.shape.clone(),
                values: vector[begin..end].to_vec(),
                valid,
                required: feature.required,
            })
        })
        .collect()
}

fn enum_name_timestamp(value: i32) -> String {
    robot_multicam_protocol::multicam::TimestampQuality::try_from(value).map_or_else(
        |_| format!("UNKNOWN_{value}"),
        |item| item.as_str_name().to_owned(),
    )
}

fn enum_name_action(value: i32) -> String {
    robot_multicam_protocol::multicam::ActionSourceQuality::try_from(value).map_or_else(
        |_| format!("UNKNOWN_{value}"),
        |item| item.as_str_name().to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use robot_multicam_protocol::multicam::{
        AnchorFrameContextPacketV1, AnchorFrameContextV1, FeatureSliceV1, FeatureVectorKind,
        SessionManifestV1,
    };
    use robot_multicam_protocol::receiver::{
        FrameReference, SessionStatus, SynchronizedDatasetStep,
    };
    use uuid::Uuid;

    use super::{camera_key, project_metadata, select_session};

    #[test]
    fn latest_connected_authoritative_session_wins() {
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        let sessions = vec![
            SessionStatus {
                session_id: old.as_bytes().to_vec(),
                authoritative: true,
                connected_cameras: 1,
                last_capture_time_edge_ns: 10,
                ..Default::default()
            },
            SessionStatus {
                session_id: new.as_bytes().to_vec(),
                authoritative: true,
                connected_cameras: 2,
                last_capture_time_edge_ns: 20,
                ..Default::default()
            },
        ];
        assert_eq!(
            select_session(&sessions, None).expect("selection"),
            Some(new)
        );
        assert_eq!(
            select_session(&sessions, Some(old)).expect("override"),
            Some(old)
        );
    }

    #[test]
    fn metadata_projection_uses_manifest_slice_and_validity_bit() {
        let session = Uuid::new_v4();
        let manifest = SessionManifestV1 {
            session_id: session.as_bytes().to_vec(),
            anchor_camera_id: "front/cam".to_owned(),
            feature_slices: vec![FeatureSliceV1 {
                feature_id: 9,
                qualified_name: "arm.position".to_owned(),
                vector_kind: FeatureVectorKind::Observation as i32,
                offset: 1,
                length: 2,
                shape: vec![2],
                ..Default::default()
            }],
            ..Default::default()
        };
        let step = SynchronizedDatasetStep {
            session_id: session.as_bytes().to_vec(),
            observation_state: vec![1.0, 2.0, 3.0],
            frames: vec![FrameReference {
                camera_id: "front/cam".to_owned(),
                encoded_image: Bytes::from_static(&[1, 2, 3]),
                ..Default::default()
            }],
            anchor_context: Some(AnchorFrameContextV1 {
                feature_validity_bitmap: vec![1],
                ..Default::default()
            }),
            anchor_context_packet: Some(AnchorFrameContextPacketV1 {
                payload_crc32c: 7,
                ..Default::default()
            }),
            ..Default::default()
        };
        let events = project_metadata(&step, &manifest).expect("projection");
        assert_eq!(events[0].camera_key, camera_key("front/cam"));
        assert_eq!(events[0].named_features[0].values, vec![2.0, 3.0]);
        assert!(events[0].named_features[0].valid);
        assert_eq!(events[0].encoded_bytes, 3);
    }
}
