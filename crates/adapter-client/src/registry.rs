use std::collections::BTreeMap;

use robot_multicam_protocol::adapter::AdapterDescriptor;
use thiserror::Error;

use crate::{validate_descriptor, DescriptorError};

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error(transparent)]
    Descriptor(#[from] DescriptorError),
    #[error("adapter registry capacity is exhausted")]
    Capacity,
    #[error("duplicate adapter instance ID {0}")]
    Duplicate(String),
    #[error("adapter descriptor changed while an episode is active")]
    ActiveEpisodeSchemaChange,
    #[error("adapter {0} is not registered")]
    Unknown(String),
}

#[derive(Debug)]
pub struct AdapterRegistry {
    max_adapters: usize,
    descriptors: BTreeMap<String, AdapterDescriptor>,
    episode_active: bool,
}

impl AdapterRegistry {
    pub fn new(max_adapters: usize) -> Result<Self, RegistryError> {
        if max_adapters == 0 {
            return Err(RegistryError::Capacity);
        }
        Ok(Self {
            max_adapters,
            descriptors: BTreeMap::new(),
            episode_active: false,
        })
    }

    pub fn register(&mut self, descriptor: AdapterDescriptor) -> Result<(), RegistryError> {
        validate_descriptor(&descriptor)?;
        let id = descriptor.adapter_instance_id.clone();
        if let Some(previous) = self.descriptors.get(&id) {
            if previous.descriptor_revision == descriptor.descriptor_revision {
                return Err(RegistryError::Duplicate(id));
            }
            if self.episode_active {
                return Err(RegistryError::ActiveEpisodeSchemaChange);
            }
        } else if self.descriptors.len() == self.max_adapters {
            return Err(RegistryError::Capacity);
        }
        self.descriptors.insert(id, descriptor);
        Ok(())
    }

    pub fn disconnect(&mut self, adapter_id: &str) -> Result<AdapterDescriptor, RegistryError> {
        self.descriptors
            .remove(adapter_id)
            .ok_or_else(|| RegistryError::Unknown(adapter_id.to_owned()))
    }

    pub fn set_episode_active(&mut self, active: bool) {
        self.episode_active = active;
    }

    #[must_use]
    pub fn descriptors(&self) -> &BTreeMap<String, AdapterDescriptor> {
        &self.descriptors
    }
}

#[cfg(test)]
mod tests {
    use super::{AdapterRegistry, RegistryError};
    use robot_multicam_protocol::adapter::{AdapterDescriptor, SourceClockDescriptor};

    fn descriptor(revision: u64) -> AdapterDescriptor {
        AdapterDescriptor {
            api_version: 1,
            adapter_instance_id: "mock".to_owned(),
            descriptor_revision: revision,
            source_clock: Some(SourceClockDescriptor {
                source_clock_id: "edge".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn schema_change_aborts_at_episode_boundary() {
        let mut registry = AdapterRegistry::new(2).expect("registry");
        registry.register(descriptor(1)).expect("register");
        registry.set_episode_active(true);
        assert!(matches!(
            registry.register(descriptor(2)),
            Err(RegistryError::ActiveEpisodeSchemaChange)
        ));
        registry.set_episode_active(false);
        registry.register(descriptor(2)).expect("new revision");
    }
}
