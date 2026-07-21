//! Bounded Unix-domain gRPC client for external Hardware Adapters.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use robot_multicam_protocol::adapter::hardware_adapter_client::HardwareAdapterClient;
use robot_multicam_protocol::adapter::{
    AdapterDescriptor, CommandEnvelope, CommandFeedback, DeviceSample, StreamSamplesRequest,
};
use thiserror::Error;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Code, Streaming};
use tower::service_fn;

use crate::{validate_descriptor, DescriptorError};

#[derive(Debug, Error)]
pub enum AdapterClientError {
    #[error("adapter socket path is not an absolute normalized path")]
    SocketPath,
    #[error("adapter transport failed: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("adapter RPC failed: {0}")]
    Status(#[from] tonic::Status),
    #[error("adapter RPC timed out")]
    Timeout,
    #[error("adapter descriptor is invalid: {0}")]
    Descriptor(#[from] DescriptorError),
    #[error("adapter sample violates identity, revision, shape, or clock invariants")]
    Sample,
}

pub struct AdapterConnection {
    client: HardwareAdapterClient<Channel>,
    descriptor: AdapterDescriptor,
    feature_ids: BTreeSet<u64>,
    feature_lengths: BTreeMap<u64, usize>,
    timeout: Duration,
    last_sequence: u64,
    last_source_time_ns: u64,
}

impl AdapterConnection {
    pub async fn connect(
        path: impl AsRef<Path>,
        timeout: Duration,
    ) -> Result<Self, AdapterClientError> {
        let path = path.as_ref();
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
            || timeout.is_zero()
        {
            return Err(AdapterClientError::SocketPath);
        }
        let socket = PathBuf::from(path);
        let channel = tokio::time::timeout(
            timeout,
            Endpoint::try_from("http://[::]:50051")?.connect_with_connector(service_fn(
                move |_: Uri| {
                    let socket = socket.clone();
                    async move { UnixStream::connect(socket).await.map(TokioIo::new) }
                },
            )),
        )
        .await
        .map_err(|_| AdapterClientError::Timeout)??;
        let mut client = HardwareAdapterClient::new(channel)
            .max_decoding_message_size(1_048_576)
            .max_encoding_message_size(1_048_576);
        let descriptor = tokio::time::timeout(timeout, client.get_descriptor(()))
            .await
            .map_err(|_| AdapterClientError::Timeout)??
            .into_inner();
        validate_descriptor(&descriptor)?;
        let feature_ids = descriptor
            .devices
            .iter()
            .flat_map(|device| device.features.iter().map(|feature| feature.feature_id))
            .collect();
        let feature_lengths = descriptor
            .devices
            .iter()
            .flat_map(|device| device.features.iter())
            .map(|feature| {
                let length = if feature.shape.is_empty() {
                    1
                } else {
                    feature
                        .shape
                        .iter()
                        .try_fold(1_usize, |total, value| total.checked_mul(*value as usize))
                        .unwrap_or(usize::MAX)
                };
                (feature.feature_id, length)
            })
            .collect();
        Ok(Self {
            client,
            descriptor,
            feature_ids,
            feature_lengths,
            timeout,
            last_sequence: 0,
            last_source_time_ns: 0,
        })
    }

    #[must_use]
    pub fn descriptor(&self) -> &AdapterDescriptor {
        &self.descriptor
    }

    pub async fn stream_samples(
        &mut self,
        device_ids: Vec<String>,
        requested_rate_hz: u32,
    ) -> Result<Streaming<DeviceSample>, AdapterClientError> {
        if requested_rate_hz == 0
            || requested_rate_hz > 1_000
            || device_ids.len() > self.descriptor.devices.len()
        {
            return Err(AdapterClientError::Sample);
        }
        Ok(tokio::time::timeout(
            self.timeout,
            self.client.stream_samples(StreamSamplesRequest {
                device_ids,
                requested_rate_hz,
            }),
        )
        .await
        .map_err(|_| AdapterClientError::Timeout)??
        .into_inner())
    }

    pub fn validate_sample(&mut self, sample: &DeviceSample) -> Result<(), AdapterClientError> {
        let valid_device = self
            .descriptor
            .devices
            .iter()
            .any(|device| device.device_id == sample.device_id);
        let total_values = sample
            .feature_blocks
            .iter()
            .try_fold(0_usize, |total, feature| {
                total.checked_add(feature.values.len())
            })
            .ok_or(AdapterClientError::Sample)?;
        let mut seen = BTreeSet::new();
        if sample.adapter_instance_id != self.descriptor.adapter_instance_id
            || sample.descriptor_revision != self.descriptor.descriptor_revision
            || !valid_device
            || sample.sample_seq <= self.last_sequence
            || sample.source_time_ns == 0
            || sample.source_time_ns <= self.last_source_time_ns
            || sample.feature_blocks.len() > self.feature_ids.len()
            || total_values > 65_536
            || sample.feature_blocks.iter().any(|block| {
                !self.feature_ids.contains(&block.feature_id)
                    || !seen.insert(block.feature_id)
                    || self.feature_lengths.get(&block.feature_id).copied()
                        != Some(block.values.len())
                    || block.values.iter().any(|value| !value.is_finite())
            })
        {
            return Err(AdapterClientError::Sample);
        }
        self.last_sequence = sample.sample_seq;
        self.last_source_time_ns = sample.source_time_ns;
        Ok(())
    }

    pub async fn next_sample(
        &mut self,
        stream: &mut Streaming<DeviceSample>,
    ) -> Result<DeviceSample, AdapterClientError> {
        let sample = tokio::time::timeout(self.timeout, stream.message())
            .await
            .map_err(|_| AdapterClientError::Timeout)??
            .ok_or_else(|| tonic::Status::new(Code::Unavailable, "sample stream closed"))?;
        self.validate_sample(&sample)?;
        Ok(sample)
    }

    pub async fn execute_command(
        &mut self,
        command: CommandEnvelope,
    ) -> Result<CommandFeedback, AdapterClientError> {
        let request = tokio_stream::iter([command]);
        let mut feedback = tokio::time::timeout(self.timeout, self.client.command_stream(request))
            .await
            .map_err(|_| AdapterClientError::Timeout)??
            .into_inner();
        tokio::time::timeout(self.timeout, feedback.message())
            .await
            .map_err(|_| AdapterClientError::Timeout)??
            .ok_or_else(|| tonic::Status::new(Code::Unavailable, "command feedback closed").into())
    }
}
