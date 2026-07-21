//! Anchor-only feature correlation, resampling, and bounded AU holding.

use std::collections::{BTreeMap, VecDeque};

use prost::Message;
use robot_multicam_adapter_client::embodiment::{CompiledEmbodiment, VectorKind};
use robot_multicam_metadata_codec::metadata_ext::manifest_chunk_sei;
use robot_multicam_metadata_codec::{encode_anchor_context_packet, UserDataUnregistered};
use robot_multicam_protocol::adapter::InterpolationMethod;
use robot_multicam_protocol::constants;
use robot_multicam_protocol::multicam::{
    ActionSourceQuality, AnchorFrameContextV1, DeviceFrameQualityV1, FrameValidityFlag,
    SessionManifestChunkV1, TimestampQuality,
};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureSample {
    pub time_edge_ns: u64,
    pub values: Vec<f32>,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResampledFeature {
    pub values: Vec<f32>,
    pub previous_time_ns: u64,
    pub next_time_ns: u64,
    pub alpha: f32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    #[error("feature ring capacity and vector length must be nonzero")]
    InvalidCapacity,
    #[error("feature samples must be strictly monotonic and shape-stable")]
    InvalidSample,
    #[error("anchor PTS is zero, duplicate, or non-monotonic")]
    InvalidPts,
    #[error("anchor AU hold queue capacity exhausted")]
    HoldCapacity,
    #[error("anchor context exceeds configured byte budget")]
    ContextBudget,
}

#[derive(Debug)]
pub struct FeatureRing {
    capacity: usize,
    vector_len: usize,
    samples: VecDeque<FeatureSample>,
}

impl FeatureRing {
    pub fn new(capacity: usize, vector_len: usize) -> Result<Self, ContextError> {
        if capacity == 0 || vector_len == 0 {
            return Err(ContextError::InvalidCapacity);
        }
        Ok(Self {
            capacity,
            vector_len,
            samples: VecDeque::with_capacity(capacity),
        })
    }

    pub fn insert(&mut self, sample: FeatureSample) -> Result<(), ContextError> {
        if sample.time_edge_ns == 0
            || sample.values.len() != self.vector_len
            || self
                .samples
                .back()
                .is_some_and(|last| sample.time_edge_ns <= last.time_edge_ns)
        {
            return Err(ContextError::InvalidSample);
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        Ok(())
    }

    pub fn resample(
        &self,
        at_ns: u64,
        method: InterpolationMethod,
        max_gap_ns: u64,
    ) -> Option<ResampledFeature> {
        let previous = self
            .samples
            .iter()
            .rev()
            .find(|sample| sample.valid && sample.time_edge_ns <= at_ns);
        let next = self
            .samples
            .iter()
            .find(|sample| sample.valid && sample.time_edge_ns >= at_ns);
        match method {
            InterpolationMethod::Linear => {
                let (left, right) = (previous?, next?);
                let span = right.time_edge_ns.saturating_sub(left.time_edge_ns);
                if span > max_gap_ns {
                    return None;
                }
                let alpha = if span == 0 {
                    0.0
                } else {
                    (at_ns.saturating_sub(left.time_edge_ns) as f64 / span as f64) as f32
                };
                Some(ResampledFeature {
                    values: left
                        .values
                        .iter()
                        .zip(&right.values)
                        .map(|(a, b)| a + (b - a) * alpha)
                        .collect(),
                    previous_time_ns: left.time_edge_ns,
                    next_time_ns: right.time_edge_ns,
                    alpha,
                })
            }
            InterpolationMethod::ZeroOrderHold => {
                let selected = previous?;
                (at_ns.saturating_sub(selected.time_edge_ns) <= max_gap_ns)
                    .then(|| single(selected))
            }
            InterpolationMethod::Nearest => {
                let selected = match (previous, next) {
                    (Some(left), Some(right)) => {
                        if at_ns.saturating_sub(left.time_edge_ns)
                            <= right.time_edge_ns.saturating_sub(at_ns)
                        {
                            left
                        } else {
                            right
                        }
                    }
                    (Some(value), None) | (None, Some(value)) => value,
                    (None, None) => return None,
                };
                (selected.time_edge_ns.abs_diff(at_ns) <= max_gap_ns).then(|| single(selected))
            }
            InterpolationMethod::None => previous
                .filter(|sample| sample.time_edge_ns == at_ns)
                .map(single),
            InterpolationMethod::Unspecified => None,
        }
    }
}

fn single(sample: &FeatureSample) -> ResampledFeature {
    ResampledFeature {
        values: sample.values.clone(),
        previous_time_ns: sample.time_edge_ns,
        next_time_ns: sample.time_edge_ns,
        alpha: 0.0,
    }
}

#[derive(Debug)]
pub struct ContextAssembler {
    layout: CompiledEmbodiment,
    rings: BTreeMap<u64, FeatureRing>,
    max_gap_ns: u64,
    context_budget_bytes: usize,
}

impl ContextAssembler {
    pub fn new(
        layout: CompiledEmbodiment,
        ring_capacity: usize,
        max_gap_ns: u64,
        context_budget_bytes: usize,
    ) -> Result<Self, ContextError> {
        let rings = layout
            .features
            .iter()
            .map(|feature| {
                let length =
                    usize::try_from(feature.length).map_err(|_| ContextError::InvalidCapacity)?;
                Ok((feature.feature_id, FeatureRing::new(ring_capacity, length)?))
            })
            .collect::<Result<_, ContextError>>()?;
        Ok(Self {
            layout,
            rings,
            max_gap_ns,
            context_budget_bytes,
        })
    }

    pub fn push(&mut self, feature_id: u64, sample: FeatureSample) -> Result<(), ContextError> {
        self.rings
            .get_mut(&feature_id)
            .ok_or(ContextError::InvalidSample)?
            .insert(sample)
    }

    pub fn assemble(
        &self,
        session_id: [u8; 16],
        anchor_frame_seq: u64,
        manifest_revision: u64,
        capture_time_edge_ns: u64,
    ) -> Result<AnchorFrameContextV1, ContextError> {
        let mut observation = vec![0.0; self.layout.observation_length as usize];
        let mut action = vec![0.0; self.layout.action_length as usize];
        let mut auxiliary = vec![0.0; self.layout.auxiliary_length as usize];
        let mut validity = vec![0_u8; self.layout.features.len().div_ceil(8)];
        let mut quality_by_device: BTreeMap<String, DeviceFrameQualityV1> = BTreeMap::new();
        let mut invalid_reasons = Vec::new();

        for (index, feature) in self.layout.features.iter().enumerate() {
            let method = InterpolationMethod::try_from(feature.interpolation)
                .unwrap_or(InterpolationMethod::Unspecified);
            let value = self
                .rings
                .get(&feature.feature_id)
                .and_then(|ring| ring.resample(capture_time_edge_ns, method, self.max_gap_ns));
            if let Some(value) = value {
                let destination = match feature.kind {
                    VectorKind::Observation => &mut observation,
                    VectorKind::Action => &mut action,
                    VectorKind::Auxiliary => &mut auxiliary,
                };
                let start = feature.offset as usize;
                let end = start + feature.length as usize;
                destination[start..end].copy_from_slice(&value.values);
                validity[index / 8] |= 1 << (index % 8);
                let quality = quality_by_device
                    .entry(feature.device_id.clone())
                    .or_insert_with(|| DeviceFrameQualityV1 {
                        device_id: feature.device_id.clone(),
                        timestamp_quality: TimestampQuality::SourceClockMapped as i32,
                        valid: true,
                        action_source_quality: ActionSourceQuality::ControllerEffectiveTarget
                            as i32,
                        ..Default::default()
                    });
                quality.previous_sample_time_edge_ns = if quality.previous_sample_time_edge_ns == 0
                {
                    value.previous_time_ns
                } else {
                    quality
                        .previous_sample_time_edge_ns
                        .min(value.previous_time_ns)
                };
                quality.next_sample_time_edge_ns =
                    quality.next_sample_time_edge_ns.max(value.next_time_ns);
                quality.max_feature_gap_ns = quality
                    .max_feature_gap_ns
                    .max(value.next_time_ns.saturating_sub(value.previous_time_ns));
                quality.interpolation_alpha = value.alpha;
            } else if feature.required {
                invalid_reasons.push(feature.qualified_name.clone());
                let quality = quality_by_device
                    .entry(feature.device_id.clone())
                    .or_insert_with(|| DeviceFrameQualityV1 {
                        device_id: feature.device_id.clone(),
                        ..Default::default()
                    });
                quality.timestamp_quality = TimestampQuality::Invalid as i32;
                quality.action_source_quality = ActionSourceQuality::Unavailable as i32;
                quality.valid = false;
                quality.invalid_reason = "required feature unavailable".to_owned();
            }
        }
        let fully_valid = invalid_reasons.is_empty();
        let mut validity_flags = vec![
            FrameValidityFlag::SchemaMatched as i32,
            FrameValidityFlag::ContextCrcValid as i32,
        ];
        if fully_valid {
            validity_flags.extend([
                FrameValidityFlag::ObservationValid as i32,
                FrameValidityFlag::ActionValid as i32,
                FrameValidityFlag::RequiredDevicesPresent as i32,
                FrameValidityFlag::ClockMappingValid as i32,
                FrameValidityFlag::InterpolationValid as i32,
            ]);
        }
        let context = AnchorFrameContextV1 {
            schema_version: 1,
            session_id: session_id.to_vec(),
            anchor_frame_seq,
            manifest_revision,
            observation_schema_id: self.layout.observation_schema_id,
            action_schema_id: self.layout.action_schema_id,
            observation_state: observation,
            action,
            auxiliary,
            feature_validity_bitmap: validity,
            action_source_quality: if fully_valid {
                ActionSourceQuality::ControllerEffectiveTarget as i32
            } else {
                ActionSourceQuality::Unavailable as i32
            },
            device_quality: quality_by_device.into_values().collect(),
            validity_flags,
            invalid_reason: invalid_reasons.join(","),
        };
        if context.encoded_len() > self.context_budget_bytes {
            return Err(ContextError::ContextBudget);
        }
        Ok(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAnchorAu<T> {
    pub pts_ns: u64,
    pub capture_time_edge_ns: u64,
    pub ordinal: u64,
    pub inserted_at_ns: u64,
    pub access_unit: T,
}

#[derive(Debug)]
pub struct AnchorAuHoldQueue<T> {
    capacity: usize,
    max_hold_ns: u64,
    last_pts_ns: Option<u64>,
    pending: VecDeque<PendingAnchorAu<T>>,
}

impl<T> AnchorAuHoldQueue<T> {
    pub fn new(capacity: usize, max_hold_ns: u64) -> Result<Self, ContextError> {
        if capacity == 0 || max_hold_ns == 0 {
            return Err(ContextError::InvalidCapacity);
        }
        Ok(Self {
            capacity,
            max_hold_ns,
            last_pts_ns: None,
            pending: VecDeque::with_capacity(capacity),
        })
    }

    pub fn push(&mut self, item: PendingAnchorAu<T>) -> Result<(), ContextError> {
        if item.pts_ns == 0 || self.last_pts_ns.is_some_and(|last| item.pts_ns <= last) {
            return Err(ContextError::InvalidPts);
        }
        if self.pending.len() == self.capacity {
            return Err(ContextError::HoldCapacity);
        }
        self.last_pts_ns = Some(item.pts_ns);
        self.pending.push_back(item);
        Ok(())
    }

    pub fn take(&mut self, pts_ns: u64) -> Option<PendingAnchorAu<T>> {
        let index = self.pending.iter().position(|item| item.pts_ns == pts_ns)?;
        self.pending.remove(index)
    }

    pub fn expire(&mut self, now_ns: u64) -> Vec<PendingAnchorAu<T>> {
        let mut expired = Vec::new();
        while self
            .pending
            .front()
            .is_some_and(|item| now_ns.saturating_sub(item.inserted_at_ns) > self.max_hold_ns)
        {
            if let Some(item) = self.pending.pop_front() {
                expired.push(item);
            }
        }
        expired
    }
}

#[derive(Debug)]
pub struct ManifestSchedule {
    chunks: Vec<SessionManifestChunkV1>,
    next: usize,
    repeat_every_frames: u64,
    last_started_frame: Option<u64>,
}

impl ManifestSchedule {
    pub fn new(
        chunks: Vec<SessionManifestChunkV1>,
        repeat_every_frames: u64,
    ) -> Result<Self, ContextError> {
        if chunks.is_empty() || repeat_every_frames == 0 {
            return Err(ContextError::InvalidCapacity);
        }
        Ok(Self {
            chunks,
            next: 0,
            repeat_every_frames,
            last_started_frame: None,
        })
    }

    /// Returns at most one chunk for an AU and repeats a complete cycle on the
    /// configured cadence. This keeps large manifests out of one IDR burst.
    pub fn for_access_unit(&mut self, frame_seq: u64) -> Option<SessionManifestChunkV1> {
        let due = self
            .last_started_frame
            .is_none_or(|last| frame_seq.saturating_sub(last) >= self.repeat_every_frames);
        if self.next == 0 && !due {
            return None;
        }
        if self.next == 0 {
            self.last_started_frame = Some(frame_seq);
        }
        let chunk = self.chunks[self.next].clone();
        self.next += 1;
        if self.next == self.chunks.len() {
            self.next = 0;
        }
        Some(chunk)
    }
}

pub struct AnchorMetadataProvider {
    assembler: Arc<Mutex<ContextAssembler>>,
    session_id: [u8; 16],
    manifest_revision: u64,
    schedule: Mutex<ManifestSchedule>,
}

impl AnchorMetadataProvider {
    pub fn new(
        assembler: Arc<Mutex<ContextAssembler>>,
        session_id: [u8; 16],
        manifest_revision: u64,
        schedule: ManifestSchedule,
    ) -> Self {
        Self {
            assembler,
            session_id,
            manifest_revision,
            schedule: Mutex::new(schedule),
        }
    }
}

impl crate::SemanticMetadataProvider for AnchorMetadataProvider {
    fn for_access_unit(
        &self,
        capture_time_edge_ns: u64,
        access_unit_ordinal: u64,
    ) -> anyhow::Result<Vec<UserDataUnregistered>> {
        let context = self
            .assembler
            .lock()
            .map_err(|_| anyhow::anyhow!("context assembler lock poisoned"))?
            .assemble(
                self.session_id,
                access_unit_ordinal,
                self.manifest_revision,
                capture_time_edge_ns,
            )?;
        let mut messages = vec![UserDataUnregistered {
            uuid: Uuid::parse_str(constants::ANCHOR_CONTEXT_UUID)?,
            payload: encode_anchor_context_packet(&context)?,
        }];
        if let Some(chunk) = self
            .schedule
            .lock()
            .map_err(|_| anyhow::anyhow!("manifest schedule lock poisoned"))?
            .for_access_unit(access_unit_ordinal)
        {
            messages.push(manifest_chunk_sei(&chunk));
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnchorAuHoldQueue, ContextAssembler, FeatureRing, FeatureSample, ManifestSchedule,
        PendingAnchorAu,
    };
    use robot_multicam_adapter_client::embodiment::{
        CompiledEmbodiment, CompiledFeature, VectorKind,
    };
    use robot_multicam_protocol::adapter::InterpolationMethod;
    use robot_multicam_protocol::multicam::SessionManifestChunkV1;

    #[test]
    fn feature_interpolation_is_bounded_and_deterministic() {
        let mut ring = FeatureRing::new(3, 1).expect("ring");
        ring.insert(sample(100, 1.0)).expect("sample");
        ring.insert(sample(200, 3.0)).expect("sample");
        let value = ring
            .resample(150, InterpolationMethod::Linear, 100)
            .expect("interpolation");
        assert_eq!(value.values, vec![2.0]);
        assert!(ring
            .resample(150, InterpolationMethod::Linear, 99)
            .is_none());
    }

    #[test]
    fn context_composes_vectors_and_lsb0_validity() {
        let layout = CompiledEmbodiment {
            embodiment_id: "fixture".to_owned(),
            observation_schema_id: 7,
            action_schema_id: 8,
            observation_length: 1,
            action_length: 1,
            auxiliary_length: 0,
            features: vec![
                feature(1, VectorKind::Observation),
                feature(2, VectorKind::Action),
            ],
        };
        let mut assembler = ContextAssembler::new(layout, 4, 100, 8_192).expect("assembler");
        for id in [1, 2] {
            assembler.push(id, sample(100, id as f32)).expect("sample");
            assembler
                .push(id, sample(200, (id + 2) as f32))
                .expect("sample");
        }
        let result = assembler.assemble([4; 16], 1, 1, 150).expect("context");
        assert_eq!(result.observation_state, vec![2.0]);
        assert_eq!(result.action, vec![3.0]);
        assert_eq!(result.feature_validity_bitmap, vec![0b11]);
        assert!(result.invalid_reason.is_empty());
    }

    #[test]
    fn hold_queue_rejects_duplicate_pts_and_expires() {
        let mut queue = AnchorAuHoldQueue::new(2, 10).expect("queue");
        queue.push(pending(1, 0)).expect("push");
        assert!(queue.push(pending(1, 1)).is_err());
        assert_eq!(queue.expire(11).len(), 1);
    }

    #[test]
    fn manifest_scheduler_emits_one_chunk_per_access_unit() {
        let chunks = (0..3)
            .map(|index| SessionManifestChunkV1 {
                chunk_index: index,
                ..Default::default()
            })
            .collect();
        let mut schedule = ManifestSchedule::new(chunks, 30).expect("schedule");
        assert_eq!(schedule.for_access_unit(0).expect("chunk").chunk_index, 0);
        assert_eq!(schedule.for_access_unit(1).expect("chunk").chunk_index, 1);
        assert_eq!(schedule.for_access_unit(2).expect("chunk").chunk_index, 2);
        assert!(schedule.for_access_unit(3).is_none());
        assert_eq!(schedule.for_access_unit(30).expect("repeat").chunk_index, 0);
    }

    fn sample(time_edge_ns: u64, value: f32) -> FeatureSample {
        FeatureSample {
            time_edge_ns,
            values: vec![value],
            valid: true,
        }
    }

    fn feature(feature_id: u64, kind: VectorKind) -> CompiledFeature {
        CompiledFeature {
            feature_id,
            qualified_name: format!("body.{feature_id}"),
            device_id: "body".to_owned(),
            kind,
            offset: 0,
            length: 1,
            shape: vec![1],
            required: true,
            interpolation: InterpolationMethod::Linear as i32,
            unit: "rad".to_owned(),
        }
    }

    fn pending(pts_ns: u64, inserted_at_ns: u64) -> PendingAnchorAu<Vec<u8>> {
        PendingAnchorAu {
            pts_ns,
            capture_time_edge_ns: pts_ns,
            ordinal: pts_ns,
            inserted_at_ns,
            access_unit: vec![],
        }
    }
}
