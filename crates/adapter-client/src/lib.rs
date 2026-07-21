//! Phase-2-ready boundary validation for vendor-neutral Adapter descriptors.

#[cfg(unix)]
pub mod client;
pub mod embodiment;
pub mod registry;

use std::collections::BTreeSet;

use robot_multicam_protocol::adapter::AdapterDescriptor;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DescriptorError {
    #[error("unsupported Adapter API version {0}")]
    ApiVersion(u32),
    #[error("adapter instance ID is empty")]
    EmptyAdapterId,
    #[error("source clock ID is empty")]
    EmptyClockId,
    #[error("duplicate device ID {0}")]
    DuplicateDevice(String),
    #[error("duplicate feature ID {0}")]
    DuplicateFeature(u64),
    #[error("feature ID zero is reserved")]
    ZeroFeatureId,
}

pub fn validate_descriptor(descriptor: &AdapterDescriptor) -> Result<(), DescriptorError> {
    if descriptor.api_version != 1 {
        return Err(DescriptorError::ApiVersion(descriptor.api_version));
    }
    if descriptor.adapter_instance_id.is_empty() {
        return Err(DescriptorError::EmptyAdapterId);
    }
    if descriptor
        .source_clock
        .as_ref()
        .is_none_or(|clock| clock.source_clock_id.is_empty())
    {
        return Err(DescriptorError::EmptyClockId);
    }
    let mut devices = BTreeSet::new();
    let mut features = BTreeSet::new();
    for device in &descriptor.devices {
        if !devices.insert(device.device_id.clone()) {
            return Err(DescriptorError::DuplicateDevice(device.device_id.clone()));
        }
        for feature in &device.features {
            if feature.feature_id == 0 {
                return Err(DescriptorError::ZeroFeatureId);
            }
            if !features.insert(feature.feature_id) {
                return Err(DescriptorError::DuplicateFeature(feature.feature_id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_descriptor, DescriptorError};
    use robot_multicam_protocol::adapter::{AdapterDescriptor, SourceClockDescriptor};

    #[test]
    fn descriptor_boundary_fails_closed() {
        let descriptor = AdapterDescriptor {
            api_version: 1,
            adapter_instance_id: "mock".to_owned(),
            source_clock: Some(SourceClockDescriptor {
                source_clock_id: "edge".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        };
        validate_descriptor(&descriptor).expect("valid descriptor");
        let invalid = AdapterDescriptor {
            api_version: 9,
            ..descriptor
        };
        assert_eq!(
            validate_descriptor(&invalid),
            Err(DescriptorError::ApiVersion(9))
        );
    }
}
