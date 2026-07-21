use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use robot_multicam_protocol::adapter::{AdapterDescriptor, FeatureDescriptor};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct EmbodimentConfig {
    pub schema_version: u32,
    pub embodiment_id: String,
    pub adapters: Vec<AdapterRef>,
    pub devices: Vec<DeviceRef>,
    pub vector_layout: VectorLayout,
    pub policies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdapterRef {
    pub adapter_instance_id: String,
    pub endpoint: String,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceRef {
    pub device_id: String,
    pub adapter_instance_id: String,
    pub role: String,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VectorLayout {
    pub observation: Vec<FeatureRef>,
    pub action: Vec<FeatureRef>,
    #[serde(default)]
    pub auxiliary: Vec<FeatureRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureRef {
    pub feature: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorKind {
    Observation,
    Action,
    Auxiliary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompiledFeature {
    pub feature_id: u64,
    pub qualified_name: String,
    pub device_id: String,
    pub kind: VectorKind,
    pub offset: u32,
    pub length: u32,
    pub shape: Vec<u32>,
    pub required: bool,
    pub interpolation: i32,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledEmbodiment {
    pub embodiment_id: String,
    pub observation_schema_id: u64,
    pub action_schema_id: u64,
    pub observation_length: u32,
    pub action_length: u32,
    pub auxiliary_length: u32,
    pub features: Vec<CompiledFeature>,
}

#[derive(Debug, Error)]
pub enum EmbodimentError {
    #[error("embodiment I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("embodiment YAML is invalid: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("unsupported embodiment schema version {0}")]
    SchemaVersion(u32),
    #[error("empty or duplicate identifier {0}")]
    Identifier(String),
    #[error("adapter endpoint must be an absolute unix URI: {0}")]
    Endpoint(String),
    #[error("missing adapter descriptor {0}")]
    MissingAdapter(String),
    #[error("device {device} is absent from adapter {adapter}")]
    MissingDevice { adapter: String, device: String },
    #[error("unknown or duplicate feature {0}")]
    Feature(String),
    #[error("feature vector length overflow")]
    LengthOverflow,
    #[error("schema ID zero is reserved")]
    ZeroSchemaId,
    #[error("canonical JSON failed: {0}")]
    CanonicalJson(String),
}

impl EmbodimentConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, EmbodimentError> {
        Ok(serde_yaml::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn compile(
        &self,
        descriptors: &BTreeMap<String, AdapterDescriptor>,
    ) -> Result<CompiledEmbodiment, EmbodimentError> {
        self.validate_structure()?;
        let mut device_features: BTreeMap<(String, String), &FeatureDescriptor> = BTreeMap::new();
        for device in &self.devices {
            let descriptor = descriptors
                .get(&device.adapter_instance_id)
                .ok_or_else(|| {
                    EmbodimentError::MissingAdapter(device.adapter_instance_id.clone())
                })?;
            let described_device = descriptor
                .devices
                .iter()
                .find(|item| item.device_id == device.device_id)
                .ok_or_else(|| EmbodimentError::MissingDevice {
                    adapter: device.adapter_instance_id.clone(),
                    device: device.device_id.clone(),
                })?;
            for feature in &described_device.features {
                device_features.insert(
                    (device.device_id.clone(), feature.qualified_name.clone()),
                    feature,
                );
            }
        }

        let mut seen = BTreeSet::new();
        let mut compiled = Vec::new();
        let observation_length = compile_vector(
            VectorKind::Observation,
            &self.vector_layout.observation,
            &device_features,
            &mut seen,
            &mut compiled,
        )?;
        let action_length = compile_vector(
            VectorKind::Action,
            &self.vector_layout.action,
            &device_features,
            &mut seen,
            &mut compiled,
        )?;
        let auxiliary_length = compile_vector(
            VectorKind::Auxiliary,
            &self.vector_layout.auxiliary,
            &device_features,
            &mut seen,
            &mut compiled,
        )?;
        let observation_schema_id = schema_id(
            &compiled
                .iter()
                .filter(|feature| feature.kind == VectorKind::Observation)
                .collect::<Vec<_>>(),
        )?;
        let action_schema_id = schema_id(
            &compiled
                .iter()
                .filter(|feature| feature.kind == VectorKind::Action)
                .collect::<Vec<_>>(),
        )?;
        Ok(CompiledEmbodiment {
            embodiment_id: self.embodiment_id.clone(),
            observation_schema_id,
            action_schema_id,
            observation_length,
            action_length,
            auxiliary_length,
            features: compiled,
        })
    }

    fn validate_structure(&self) -> Result<(), EmbodimentError> {
        if self.schema_version != 1 {
            return Err(EmbodimentError::SchemaVersion(self.schema_version));
        }
        if self.embodiment_id.is_empty() || self.embodiment_id.len() > 64 {
            return Err(EmbodimentError::Identifier(self.embodiment_id.clone()));
        }
        let mut adapters = BTreeSet::new();
        for adapter in &self.adapters {
            if adapter.adapter_instance_id.is_empty()
                || !adapters.insert(adapter.adapter_instance_id.clone())
            {
                return Err(EmbodimentError::Identifier(
                    adapter.adapter_instance_id.clone(),
                ));
            }
            let endpoint = adapter.endpoint.strip_prefix("unix://").unwrap_or("");
            if !endpoint.starts_with('/') || endpoint.contains("..") {
                return Err(EmbodimentError::Endpoint(adapter.endpoint.clone()));
            }
        }
        let mut devices = BTreeSet::new();
        for device in &self.devices {
            if device.device_id.is_empty() || !devices.insert(device.device_id.clone()) {
                return Err(EmbodimentError::Identifier(device.device_id.clone()));
            }
            if !adapters.contains(&device.adapter_instance_id) {
                return Err(EmbodimentError::MissingAdapter(
                    device.adapter_instance_id.clone(),
                ));
            }
        }
        Ok(())
    }
}

fn compile_vector(
    kind: VectorKind,
    requested: &[FeatureRef],
    available: &BTreeMap<(String, String), &FeatureDescriptor>,
    seen: &mut BTreeSet<String>,
    output: &mut Vec<CompiledFeature>,
) -> Result<u32, EmbodimentError> {
    let mut offset = 0u32;
    for reference in requested {
        if !seen.insert(reference.feature.clone()) {
            return Err(EmbodimentError::Feature(reference.feature.clone()));
        }
        let (device_id, _) = reference
            .feature
            .split_once('.')
            .ok_or_else(|| EmbodimentError::Feature(reference.feature.clone()))?;
        let descriptor = available
            .get(&(device_id.to_owned(), reference.feature.clone()))
            .ok_or_else(|| EmbodimentError::Feature(reference.feature.clone()))?;
        let length = if descriptor.shape.is_empty() {
            1
        } else {
            descriptor
                .shape
                .iter()
                .try_fold(1u32, |acc, item| acc.checked_mul(*item))
                .ok_or(EmbodimentError::LengthOverflow)?
        };
        output.push(CompiledFeature {
            feature_id: descriptor.feature_id,
            qualified_name: descriptor.qualified_name.clone(),
            device_id: device_id.to_owned(),
            kind,
            offset,
            length,
            shape: descriptor.shape.clone(),
            required: reference.required,
            interpolation: descriptor.interpolation,
            unit: descriptor.unit.clone(),
        });
        offset = offset
            .checked_add(length)
            .ok_or(EmbodimentError::LengthOverflow)?;
    }
    Ok(offset)
}

fn schema_id<T: Serialize>(value: &T) -> Result<u64, EmbodimentError> {
    let canonical = serde_jcs::to_vec(value)
        .map_err(|error| EmbodimentError::CanonicalJson(error.to_string()))?;
    let digest = Sha256::digest(canonical);
    let id = u64::from_be_bytes(digest[..8].try_into().expect("eight-byte digest slice"));
    if id == 0 {
        return Err(EmbodimentError::ZeroSchemaId);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::EmbodimentConfig;
    use robot_multicam_protocol::adapter::{
        AdapterDescriptor, DeviceDescriptor, FeatureDescriptor, SourceClockDescriptor,
    };

    #[test]
    fn layout_is_deterministic_and_validated_against_descriptors() {
        let config = EmbodimentConfig::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/embodiment.example.yaml"
        ))
        .expect("config");
        let features = vec![
            feature(1, "body.joint.position", vec![2]),
            feature(2, "body.joint.velocity", vec![2]),
            feature(3, "body.joint.effective_target_position", vec![2]),
            feature(4, "body.control_status", vec![]),
        ];
        let descriptor = AdapterDescriptor {
            api_version: 1,
            adapter_instance_id: "rby1-main".to_owned(),
            source_clock: Some(SourceClockDescriptor {
                source_clock_id: "rby1".to_owned(),
                ..Default::default()
            }),
            devices: vec![DeviceDescriptor {
                device_id: "body".to_owned(),
                features,
                ..Default::default()
            }],
            ..Default::default()
        };
        let descriptors = BTreeMap::from([("rby1-main".to_owned(), descriptor)]);
        let first = config.compile(&descriptors).expect("compile");
        let second = config.compile(&descriptors).expect("compile");
        assert_eq!(first.observation_schema_id, second.observation_schema_id);
        assert_eq!(first.observation_length, 4);
        assert_eq!(first.action_length, 2);
        assert_eq!(first.auxiliary_length, 1);
    }

    fn feature(id: u64, name: &str, shape: Vec<u32>) -> FeatureDescriptor {
        FeatureDescriptor {
            feature_id: id,
            qualified_name: name.to_owned(),
            shape,
            interpolation: 1,
            ..Default::default()
        }
    }
}
