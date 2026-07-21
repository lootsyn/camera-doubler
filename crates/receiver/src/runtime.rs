//! Manifest bootstrap, synchronized-step production, and the Receiver metadata API.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;
use robot_multicam_metadata_codec::metadata_ext::ManifestReassembler;
use robot_multicam_protocol::constants;
use robot_multicam_protocol::multicam::{SessionManifestChunkV1, SessionManifestV1};
use robot_multicam_protocol::receiver::receiver_metadata_server::{
    ReceiverMetadata, ReceiverMetadataServer,
};
use robot_multicam_protocol::receiver::{
    CameraQuality, CameraStatus, GetAnchorRequest, GetAnchorResponse, GetSessionManifestRequest,
    GetSessionManifestResponse, GetSessionQualityRequest, GetSessionQualityResponse,
    ListCamerasRequest, ListCamerasResponse, SubscribeSynchronizedStepsRequest,
    SynchronizedDatasetStep,
};
use robot_multicam_stream_identity::Role;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::synchronize::{
    validate_manifest, ConnectedCamera, StepSynchronizer, StoredFrame, SynchronizeError,
};
use crate::{BootstrapState, EncodedFrameEnvelope, ReceiverError, ReceiverRegistry};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Registry(#[from] ReceiverError),
    #[error("manifest chunk protobuf is invalid")]
    ManifestChunk,
    #[error("manifest reassembly failed: {0}")]
    Reassembly(String),
    #[error(transparent)]
    Synchronize(#[from] SynchronizeError),
    #[error("receiver runtime state lock is poisoned")]
    Poisoned,
    #[error("receiver metadata bind address is invalid")]
    Bind,
    #[error("receiver metadata server failed: {0}")]
    Server(#[from] tonic::transport::Error),
}

#[derive(Debug, Default)]
struct SessionRuntime {
    reassembly_revision: u64,
    reassembler: Option<ManifestReassembler>,
    manifest_bytes: Option<Vec<u8>>,
    manifest: Option<SessionManifestV1>,
    synchronizer: Option<StepSynchronizer>,
    accepted_steps: u64,
    dropped_steps: u64,
}

#[derive(Debug)]
pub struct ReceiverRuntime {
    registry: Arc<ReceiverRegistry>,
    base_port: u16,
    manifest_timeout: Duration,
    max_frames_per_camera: usize,
    max_skew_ns: u64,
    started: std::time::Instant,
    sessions: Mutex<BTreeMap<Uuid, SessionRuntime>>,
    steps: broadcast::Sender<SynchronizedDatasetStep>,
}

impl ReceiverRuntime {
    pub fn new(
        registry: Arc<ReceiverRegistry>,
        base_port: u16,
        manifest_timeout: Duration,
        max_frames_per_camera: usize,
        max_skew_ns: u64,
        step_capacity: usize,
    ) -> Result<Self, RuntimeError> {
        if manifest_timeout.is_zero()
            || max_frames_per_camera == 0
            || max_skew_ns == 0
            || step_capacity == 0
        {
            return Err(RuntimeError::Bind);
        }
        let (steps, _) = broadcast::channel(step_capacity);
        Ok(Self {
            registry,
            base_port,
            manifest_timeout,
            max_frames_per_camera,
            max_skew_ns,
            started: std::time::Instant::now(),
            sessions: Mutex::new(BTreeMap::new()),
            steps,
        })
    }

    pub fn process(&self, envelope: EncodedFrameEnvelope) -> Result<(), RuntimeError> {
        let manifest_uuid = Uuid::parse_str(constants::SESSION_MANIFEST_UUID)
            .expect("build-time validated manifest UUID");
        let context_uuid = Uuid::parse_str(constants::ANCHOR_CONTEXT_UUID)
            .expect("build-time validated context UUID");
        for message in envelope
            .metadata_messages
            .iter()
            .filter(|message| message.uuid == manifest_uuid)
        {
            let chunk = SessionManifestChunkV1::decode(message.payload.as_slice())
                .map_err(|_| RuntimeError::ManifestChunk)?;
            self.process_manifest_chunk(envelope.key.session_id, chunk)?;
        }

        let frame = StoredFrame {
            camera_id: envelope.key.camera_id.clone(),
            capture_time_edge_ns: envelope.capture_time_edge_ns,
            stream_epoch: envelope.key.epoch,
            normalized_pts_ns: envelope.pts_ns,
            access_unit_ordinal: envelope.access_unit_ordinal,
            storage_uri: String::new(),
            encoded_image: envelope.encoded_au,
        };
        let context = envelope
            .metadata_messages
            .iter()
            .find(|message| message.uuid == context_uuid)
            .map(|message| message.payload.clone());
        let mut sessions = self.sessions.lock().map_err(|_| RuntimeError::Poisoned)?;
        let Some(session) = sessions.get_mut(&envelope.key.session_id) else {
            return Ok(());
        };
        let Some(manifest) = session.manifest.as_ref() else {
            return Ok(());
        };
        let Some(synchronizer) = session.synchronizer.as_mut() else {
            return Ok(());
        };
        if frame.camera_id == manifest.anchor_camera_id {
            let Some(packet) = context else {
                session.dropped_steps = session.dropped_steps.saturating_add(1);
                return Ok(());
            };
            match synchronizer.anchor_step(frame, &packet, true) {
                Ok(step) => {
                    session.accepted_steps = session.accepted_steps.saturating_add(1);
                    let _ = self.steps.send(step);
                }
                Err(SynchronizeError::MissingCamera | SynchronizeError::Context) => {
                    session.dropped_steps = session.dropped_steps.saturating_add(1);
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            synchronizer.push_secondary(frame)?;
        }
        Ok(())
    }

    fn process_manifest_chunk(
        &self,
        session_id: Uuid,
        chunk: SessionManifestChunkV1,
    ) -> Result<(), RuntimeError> {
        if chunk.session_id != session_id.as_bytes() {
            return Err(RuntimeError::ManifestChunk);
        }
        let now = self.started.elapsed();
        let completed = {
            let mut sessions = self.sessions.lock().map_err(|_| RuntimeError::Poisoned)?;
            let session = sessions.entry(session_id).or_default();
            if session
                .manifest
                .as_ref()
                .is_some_and(|manifest| chunk.manifest_revision <= manifest.manifest_revision)
            {
                return Ok(());
            }
            if session.reassembler.is_none()
                || session.reassembly_revision != chunk.manifest_revision
            {
                session.reassembly_revision = chunk.manifest_revision;
                let duplicate = chunk.clone();
                let mut reassembler =
                    ManifestReassembler::new(chunk, now, self.manifest_timeout)
                        .map_err(|error| RuntimeError::Reassembly(error.to_string()))?;
                let completed = reassembler
                    .insert(duplicate, now)
                    .map_err(|error| RuntimeError::Reassembly(error.to_string()))?;
                session.reassembler = Some(reassembler);
                completed
            } else {
                session
                    .reassembler
                    .as_mut()
                    .expect("reassembler was checked")
                    .insert(chunk, now)
                    .map_err(|error| RuntimeError::Reassembly(error.to_string()))?
            }
        };
        let Some(bytes) = completed else {
            return Ok(());
        };
        let snapshots = self.registry.snapshots(Some(session_id))?;
        let transports = snapshots
            .iter()
            .filter(|snapshot| snapshot.connected)
            .map(|snapshot| ConnectedCamera {
                camera_id: snapshot.key.camera_id.clone(),
                stream_slot: u32::from(snapshot.slot),
                stream_epoch: snapshot.key.epoch,
                listen_port: snapshot.listen_port,
            })
            .collect::<Vec<_>>();
        let manifest = validate_manifest(&bytes, &transports, self.base_port)?;
        let synchronizer = StepSynchronizer::new(
            manifest.clone(),
            self.max_frames_per_camera,
            self.max_skew_ns,
        )?;
        {
            let mut sessions = self.sessions.lock().map_err(|_| RuntimeError::Poisoned)?;
            let session = sessions.entry(session_id).or_default();
            session.manifest_bytes = Some(bytes);
            session.manifest = Some(manifest);
            session.synchronizer = Some(synchronizer);
            session.reassembler = None;
        }
        for snapshot in snapshots {
            if snapshot.connected && snapshot.state == BootstrapState::ProvisionalStream {
                self.registry
                    .transition(&snapshot.key, BootstrapState::ManifestValidated)?;
                self.registry
                    .transition(&snapshot.key, BootstrapState::DatasetReady)?;
            }
        }
        Ok(())
    }

    fn manifest(
        &self,
        session_id: Uuid,
    ) -> Result<Option<(Vec<u8>, SessionManifestV1)>, RuntimeError> {
        let sessions = self.sessions.lock().map_err(|_| RuntimeError::Poisoned)?;
        Ok(sessions
            .get(&session_id)
            .and_then(|session| Some((session.manifest_bytes.clone()?, session.manifest.clone()?))))
    }

    fn quality(&self, session_id: Uuid) -> Result<(u64, u64), RuntimeError> {
        let sessions = self.sessions.lock().map_err(|_| RuntimeError::Poisoned)?;
        let session = sessions
            .get(&session_id)
            .ok_or(RuntimeError::ManifestChunk)?;
        Ok((session.accepted_steps, session.dropped_steps))
    }
}

#[derive(Debug, Clone)]
pub struct ReceiverMetadataService {
    runtime: Arc<ReceiverRuntime>,
}

impl ReceiverMetadataService {
    #[must_use]
    pub fn new(runtime: Arc<ReceiverRuntime>) -> Self {
        Self { runtime }
    }
}

type StepStream = Pin<
    Box<dyn tokio_stream::Stream<Item = Result<SynchronizedDatasetStep, Status>> + Send + 'static>,
>;

#[tonic::async_trait]
impl ReceiverMetadata for ReceiverMetadataService {
    type SubscribeSynchronizedStepsStream = StepStream;

    async fn list_cameras(
        &self,
        request: Request<ListCamerasRequest>,
    ) -> Result<Response<ListCamerasResponse>, Status> {
        let session_id = parse_session(&request.into_inner().session_id)?;
        let snapshots = self
            .runtime
            .registry
            .snapshots(Some(session_id))
            .map_err(internal)?;
        let cameras = snapshots
            .into_iter()
            .map(|snapshot| CameraStatus {
                camera_id: snapshot.key.camera_id,
                stream_slot: u32::from(snapshot.slot),
                stream_epoch: snapshot.key.epoch,
                listen_port: u32::from(snapshot.listen_port),
                provisional_role: match snapshot.role {
                    Role::Anchor => "anchor",
                    Role::Secondary => "secondary",
                }
                .to_owned(),
                manifest_validated: matches!(
                    snapshot.state,
                    BootstrapState::ManifestValidated | BootstrapState::DatasetReady
                ),
                connected: snapshot.connected,
                last_capture_time_edge_ns: snapshot.last_capture_time_edge_ns.unwrap_or_default(),
                health: format!("{:?}", snapshot.state).to_ascii_lowercase(),
                ..Default::default()
            })
            .collect();
        Ok(Response::new(ListCamerasResponse {
            session_id: session_id.as_bytes().to_vec(),
            cameras,
        }))
    }

    async fn get_anchor(
        &self,
        request: Request<GetAnchorRequest>,
    ) -> Result<Response<GetAnchorResponse>, Status> {
        let session_id = parse_session(&request.into_inner().session_id)?;
        let (_, manifest) = self
            .runtime
            .manifest(session_id)
            .map_err(internal)?
            .ok_or_else(|| Status::failed_precondition("authoritative manifest unavailable"))?;
        Ok(Response::new(GetAnchorResponse {
            session_id: session_id.as_bytes().to_vec(),
            anchor_camera_id: manifest.anchor_camera_id,
            authoritative: true,
            manifest_revision: manifest.manifest_revision,
        }))
    }

    async fn get_session_manifest(
        &self,
        request: Request<GetSessionManifestRequest>,
    ) -> Result<Response<GetSessionManifestResponse>, Status> {
        let request = request.into_inner();
        let session_id = parse_session(&request.session_id)?;
        let (bytes, manifest) = self
            .runtime
            .manifest(session_id)
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("manifest unavailable"))?;
        if request.manifest_revision != 0 && request.manifest_revision != manifest.manifest_revision
        {
            return Err(Status::not_found("manifest revision unavailable"));
        }
        Ok(Response::new(GetSessionManifestResponse {
            session_id: session_id.as_bytes().to_vec(),
            manifest_revision: manifest.manifest_revision,
            serialized_session_manifest: bytes,
        }))
    }

    async fn get_session_quality(
        &self,
        request: Request<GetSessionQualityRequest>,
    ) -> Result<Response<GetSessionQualityResponse>, Status> {
        let session_id = parse_session(&request.into_inner().session_id)?;
        let snapshots = self
            .runtime
            .registry
            .snapshots(Some(session_id))
            .map_err(internal)?;
        let cameras = snapshots
            .into_iter()
            .map(|snapshot| CameraQuality {
                camera_id: snapshot.key.camera_id,
                received_frames: snapshot.received_frames,
                missing_timestamp_frames: snapshot.dropped_frames,
                ..Default::default()
            })
            .collect();
        let (accepted_steps, dropped_steps) = self.runtime.quality(session_id).map_err(internal)?;
        Ok(Response::new(GetSessionQualityResponse {
            session_id: session_id.as_bytes().to_vec(),
            cameras,
            accepted_steps,
            dropped_steps,
            warnings: Vec::new(),
        }))
    }

    async fn subscribe_synchronized_steps(
        &self,
        request: Request<SubscribeSynchronizedStepsRequest>,
    ) -> Result<Response<Self::SubscribeSynchronizedStepsStream>, Status> {
        let request = request.into_inner();
        let session_id = parse_session(&request.session_id)?;
        if self
            .runtime
            .manifest(session_id)
            .map_err(internal)?
            .is_none()
        {
            return Err(Status::failed_precondition(
                "authoritative manifest unavailable",
            ));
        }
        let camera_filter = request.camera_ids.into_iter().collect::<BTreeSet<_>>();
        let include_images = request.include_encoded_images;
        let mut source = self.runtime.steps.subscribe();
        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            loop {
                let mut step = match source.recv().await {
                    Ok(step) => step,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if step.session_id != session_id.as_bytes() {
                    continue;
                }
                if !camera_filter.is_empty() {
                    step.frames
                        .retain(|frame| camera_filter.contains(&frame.camera_id));
                }
                if !include_images {
                    for frame in &mut step.frames {
                        frame.encoded_image.clear();
                        frame.encoded_image_media_type.clear();
                    }
                }
                if sender.send(Ok(step)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

pub async fn serve_metadata(bind: &str, runtime: Arc<ReceiverRuntime>) -> Result<(), RuntimeError> {
    let address: SocketAddr = bind.parse().map_err(|_| RuntimeError::Bind)?;
    tonic::transport::Server::builder()
        .add_service(ReceiverMetadataServer::new(ReceiverMetadataService::new(
            runtime,
        )))
        .serve(address)
        .await?;
    Ok(())
}

fn parse_session(bytes: &[u8]) -> Result<Uuid, Status> {
    Uuid::from_slice(bytes).map_err(|_| Status::invalid_argument("session_id must be 16 bytes"))
}

fn internal(error: impl std::fmt::Display) -> Status {
    Status::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use prost::Message;
    use robot_multicam_metadata_codec::metadata_ext::{
        chunk_manifest, manifest_chunk_sei, ManifestCompression,
    };
    use robot_multicam_metadata_codec::{encode_anchor_context_packet, UserDataUnregistered};
    use robot_multicam_protocol::constants;
    use robot_multicam_protocol::multicam::{
        AnchorFrameContextV1, CameraDescriptorV1, CameraRoleV1, FeatureSliceV1, FeatureVectorKind,
        SessionManifestV1,
    };
    use robot_multicam_stream_identity::{Codec, Role, StreamIdentity};
    use uuid::Uuid;

    use super::ReceiverRuntime;
    use crate::{BootstrapState, EncodedFrameEnvelope, ReceiverPolicy, ReceiverRegistry};

    #[test]
    fn manifest_bootstrap_drives_real_synchronized_step_channel() {
        let key = vec![7; 32];
        let session_id = Uuid::from_bytes([1; 16]);
        let registry = Arc::new(ReceiverRegistry::new(
            ReceiverPolicy {
                expected_embodiment_id: "cell".to_owned(),
                expected_edge_instance_id: Some("edge".to_owned()),
                base_port: 10_000,
                max_cameras: 2,
                max_ingest_frames: 8,
                max_ingest_bytes: 1_000_000,
            },
            key.clone(),
        ));
        for (camera, slot, role) in [("anchor", 0, Role::Anchor), ("side", 1, Role::Secondary)] {
            let identity = StreamIdentity {
                embodiment_id: "cell".to_owned(),
                edge_instance_id: "edge".to_owned(),
                edge_boot_id: Uuid::from_bytes([2; 16]),
                session_id,
                camera_id: camera.to_owned(),
                slot,
                epoch: 1,
                role,
                codec: Codec::H264,
            };
            let stream_key = registry
                .accept(
                    10_000 + slot,
                    &identity.encode_signed(&key).expect("identity"),
                )
                .expect("accept");
            registry
                .transition(&stream_key, BootstrapState::MediaProbing)
                .expect("probe");
            registry
                .transition(&stream_key, BootstrapState::ProvisionalStream)
                .expect("provisional");
        }
        let runtime = ReceiverRuntime::new(
            Arc::clone(&registry),
            10_000,
            Duration::from_secs(1),
            8,
            20,
            8,
        )
        .expect("runtime");
        let mut steps = runtime.steps.subscribe();
        let manifest = manifest();
        let chunk = chunk_manifest(
            &manifest.encode_to_vec(),
            session_id.as_bytes(),
            1,
            ManifestCompression::None,
        )
        .expect("chunk")
        .remove(0);
        runtime
            .process(envelope(
                session_id,
                "anchor",
                100,
                0,
                vec![manifest_chunk_sei(&chunk), context(session_id, 0)],
            ))
            .expect("manifest anchor");
        runtime
            .process(envelope(session_id, "side", 205, 0, Vec::new()))
            .expect("secondary");
        runtime
            .process(envelope(
                session_id,
                "anchor",
                200,
                1,
                vec![context(session_id, 1)],
            ))
            .expect("anchor step");
        let step = steps.try_recv().expect("broadcast step");
        assert_eq!(step.capture_time_edge_ns, 200);
        assert_eq!(step.frames.len(), 2);
        assert_eq!(step.observation_state, vec![1.0]);
    }

    fn manifest() -> SessionManifestV1 {
        SessionManifestV1 {
            schema_version: 1,
            session_id: vec![1; 16],
            edge_boot_id: vec![2; 16],
            manifest_revision: 1,
            anchor_camera_id: "anchor".to_owned(),
            stream_id_schema: "rmc1".to_owned(),
            schema_id_algorithm: constants::SCHEMA_ID_HASH.to_owned(),
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

    fn context(session_id: Uuid, ordinal: u64) -> UserDataUnregistered {
        let packet = encode_anchor_context_packet(&AnchorFrameContextV1 {
            schema_version: 1,
            session_id: session_id.as_bytes().to_vec(),
            anchor_frame_seq: ordinal,
            manifest_revision: 1,
            observation_schema_id: 11,
            action_schema_id: 12,
            observation_state: vec![1.0],
            action: vec![2.0],
            ..Default::default()
        })
        .expect("context");
        UserDataUnregistered {
            uuid: Uuid::parse_str(constants::ANCHOR_CONTEXT_UUID).expect("UUID"),
            payload: packet,
        }
    }

    fn envelope(
        session_id: Uuid,
        camera: &str,
        time: u64,
        ordinal: u64,
        metadata_messages: Vec<UserDataUnregistered>,
    ) -> EncodedFrameEnvelope {
        EncodedFrameEnvelope {
            key: crate::StreamKey {
                session_id,
                camera_id: camera.to_owned(),
                epoch: 1,
            },
            pts_ns: time,
            access_unit_ordinal: ordinal,
            capture_time_edge_ns: time,
            encoded_au: vec![1, 2, 3],
            metadata_messages,
        }
    }
}
