//! Vendor-neutral control lease and command safety validation.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCommandPolicy {
    pub device_id: String,
    pub action_schema_id: u64,
    pub vector_length: usize,
    pub command_modes: BTreeSet<String>,
    pub minimum: Vec<f32>,
    pub maximum: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub lease_id: Uuid,
    pub client_id: String,
    pub devices: BTreeSet<String>,
    pub expires_at_edge_ns: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedCommand {
    pub command_id: Uuid,
    pub lease_id: Uuid,
    pub device_id: String,
    pub command_mode: String,
    pub action_schema_id: u64,
    pub values: Vec<f32>,
    pub accepted_at_edge_ns: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandRequest {
    pub command_id: Uuid,
    pub lease_id: Uuid,
    pub device_id: String,
    pub command_mode: String,
    pub action_schema_id: u64,
    pub values: Vec<f32>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("client and device identifiers must be nonempty and bounded")]
    Identifier,
    #[error("lease TTL is outside configured bounds")]
    Ttl,
    #[error("one or more devices are unknown or already leased")]
    DeviceUnavailable,
    #[error("lease is unknown, expired, or does not own the device")]
    Lease,
    #[error("command ID was already used")]
    DuplicateCommand,
    #[error("command mode, schema, shape, or value is unsafe")]
    UnsafeCommand,
}

#[derive(Debug)]
pub struct ControlGateway {
    policies: BTreeMap<String, DeviceCommandPolicy>,
    leases: BTreeMap<Uuid, Lease>,
    command_ids: BTreeSet<Uuid>,
    effective_action: BTreeMap<String, Vec<f32>>,
    minimum_ttl_ns: u64,
    maximum_ttl_ns: u64,
    max_command_history: usize,
}

impl ControlGateway {
    pub fn new(
        policies: Vec<DeviceCommandPolicy>,
        minimum_ttl_ns: u64,
        maximum_ttl_ns: u64,
        max_command_history: usize,
    ) -> Result<Self, ControlError> {
        if minimum_ttl_ns == 0 || maximum_ttl_ns < minimum_ttl_ns || max_command_history == 0 {
            return Err(ControlError::Ttl);
        }
        let mut indexed = BTreeMap::new();
        for policy in policies {
            if policy.device_id.is_empty()
                || policy.device_id.len() > 128
                || policy.action_schema_id == 0
                || policy.vector_length == 0
                || policy.minimum.len() != policy.vector_length
                || policy.maximum.len() != policy.vector_length
                || policy.command_modes.is_empty()
                || policy
                    .minimum
                    .iter()
                    .zip(&policy.maximum)
                    .any(|(minimum, maximum)| {
                        !minimum.is_finite() || !maximum.is_finite() || minimum > maximum
                    })
                || indexed.insert(policy.device_id.clone(), policy).is_some()
            {
                return Err(ControlError::UnsafeCommand);
            }
        }
        Ok(Self {
            policies: indexed,
            leases: BTreeMap::new(),
            command_ids: BTreeSet::new(),
            effective_action: BTreeMap::new(),
            minimum_ttl_ns,
            maximum_ttl_ns,
            max_command_history,
        })
    }

    pub fn acquire(
        &mut self,
        client_id: &str,
        devices: BTreeSet<String>,
        ttl_ns: u64,
        now_edge_ns: u64,
    ) -> Result<Lease, ControlError> {
        self.expire(now_edge_ns);
        if client_id.is_empty() || client_id.len() > 128 || devices.is_empty() {
            return Err(ControlError::Identifier);
        }
        if !(self.minimum_ttl_ns..=self.maximum_ttl_ns).contains(&ttl_ns) {
            return Err(ControlError::Ttl);
        }
        if devices.iter().any(|device| {
            !self.policies.contains_key(device)
                || self
                    .leases
                    .values()
                    .any(|lease| lease.devices.contains(device))
        }) {
            return Err(ControlError::DeviceUnavailable);
        }
        let lease = Lease {
            lease_id: Uuid::new_v4(),
            client_id: client_id.to_owned(),
            devices,
            expires_at_edge_ns: now_edge_ns.saturating_add(ttl_ns),
        };
        self.leases.insert(lease.lease_id, lease.clone());
        Ok(lease)
    }

    pub fn release(&mut self, lease_id: Uuid) -> bool {
        self.leases.remove(&lease_id).is_some()
    }

    pub fn validate_command(
        &mut self,
        command: CommandRequest,
        now_edge_ns: u64,
    ) -> Result<ValidatedCommand, ControlError> {
        self.expire(now_edge_ns);
        if self.command_ids.contains(&command.command_id) {
            return Err(ControlError::DuplicateCommand);
        }
        let lease = self
            .leases
            .get(&command.lease_id)
            .ok_or(ControlError::Lease)?;
        if !lease.devices.contains(&command.device_id) {
            return Err(ControlError::Lease);
        }
        let policy = self
            .policies
            .get(&command.device_id)
            .ok_or(ControlError::DeviceUnavailable)?;
        if command.action_schema_id != policy.action_schema_id
            || command.values.len() != policy.vector_length
            || !policy.command_modes.contains(&command.command_mode)
            || command
                .values
                .iter()
                .zip(policy.minimum.iter().zip(&policy.maximum))
                .any(|(value, (minimum, maximum))| {
                    !value.is_finite() || value < minimum || value > maximum
                })
        {
            return Err(ControlError::UnsafeCommand);
        }
        if self.command_ids.len() == self.max_command_history {
            if let Some(oldest) = self.command_ids.first().copied() {
                self.command_ids.remove(&oldest);
            }
        }
        self.command_ids.insert(command.command_id);
        self.effective_action
            .insert(command.device_id.clone(), command.values.clone());
        Ok(ValidatedCommand {
            command_id: command.command_id,
            lease_id: command.lease_id,
            device_id: command.device_id,
            command_mode: command.command_mode,
            action_schema_id: command.action_schema_id,
            values: command.values,
            accepted_at_edge_ns: now_edge_ns,
        })
    }

    #[must_use]
    pub fn effective_action(&self, device_id: &str) -> Option<&[f32]> {
        self.effective_action.get(device_id).map(Vec::as_slice)
    }

    pub fn expire(&mut self, now_edge_ns: u64) -> usize {
        let before = self.leases.len();
        self.leases
            .retain(|_, lease| lease.expires_at_edge_ns > now_edge_ns);
        before.saturating_sub(self.leases.len())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{CommandRequest, ControlError, ControlGateway, DeviceCommandPolicy};
    use uuid::Uuid;

    #[test]
    fn lease_is_exclusive_and_expiry_fails_closed() {
        let mut gateway = gateway();
        let lease = gateway
            .acquire("client-a", BTreeSet::from(["body".to_owned()]), 100, 1)
            .expect("lease");
        assert_eq!(
            gateway.acquire("client-b", BTreeSet::from(["body".to_owned()]), 100, 1),
            Err(ControlError::DeviceUnavailable)
        );
        assert_eq!(gateway.expire(101), 1);
        assert_eq!(
            gateway.validate_command(command(Uuid::new_v4(), lease.lease_id, vec![0.0, 0.0]), 102,),
            Err(ControlError::Lease)
        );
    }

    #[test]
    fn unsafe_values_and_duplicate_commands_are_rejected() {
        let mut gateway = gateway();
        let lease = gateway
            .acquire("client", BTreeSet::from(["body".to_owned()]), 100, 1)
            .expect("lease");
        let command_id = Uuid::new_v4();
        assert!(gateway
            .validate_command(command(command_id, lease.lease_id, vec![0.5, -0.5]), 2,)
            .is_ok());
        assert_eq!(gateway.effective_action("body"), Some(&[0.5, -0.5][..]));
        assert_eq!(
            gateway.validate_command(command(command_id, lease.lease_id, vec![0.0, 0.0]), 3,),
            Err(ControlError::DuplicateCommand)
        );
        assert_eq!(
            gateway.validate_command(
                command(Uuid::new_v4(), lease.lease_id, vec![f32::NAN, 0.0]),
                3,
            ),
            Err(ControlError::UnsafeCommand)
        );
    }

    fn gateway() -> ControlGateway {
        ControlGateway::new(
            vec![DeviceCommandPolicy {
                device_id: "body".to_owned(),
                action_schema_id: 7,
                vector_length: 2,
                command_modes: BTreeSet::from(["position".to_owned()]),
                minimum: vec![-1.0; 2],
                maximum: vec![1.0; 2],
            }],
            10,
            1_000,
            16,
        )
        .expect("gateway")
    }

    fn command(command_id: Uuid, lease_id: Uuid, values: Vec<f32>) -> CommandRequest {
        CommandRequest {
            command_id,
            lease_id,
            device_id: "body".to_owned(),
            command_mode: "position".to_owned(),
            action_schema_id: 7,
            values,
        }
    }
}
