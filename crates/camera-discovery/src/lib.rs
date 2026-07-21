//! Logical-camera discovery, selectors, and stable slot persistence.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use data_encoding::BASE32_NOPAD;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraCandidate {
    pub device_path: PathBuf,
    pub product_name: String,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub serial: Option<String>,
    pub usb_interface: Option<String>,
    pub udev_id_path: Option<String>,
    pub bus_info: Option<String>,
    pub media_entity: Option<String>,
    pub endpoint_role: String,
    pub driver: Option<String>,
    pub logical_parent: String,
    pub capture_capable: bool,
    pub output_capable: bool,
    pub metadata_only: bool,
    pub supported_caps: bool,
    pub managed_virtual_label: bool,
}

impl CameraCandidate {
    #[must_use]
    pub fn is_generated_virtual(&self) -> bool {
        self.driver.as_deref() == Some("v4l2loopback")
            || self
                .device_path
                .to_string_lossy()
                .contains("/devices/virtual/video4linux")
            || self.managed_virtual_label
    }

    #[must_use]
    pub fn is_supported_capture(&self) -> bool {
        self.capture_capable
            && !self.output_capable
            && !self.metadata_only
            && self.supported_caps
            && !self.is_generated_virtual()
    }

    #[must_use]
    pub fn canonical_identity(&self) -> String {
        let fields = [
            self.vendor_id.as_deref().unwrap_or(""),
            self.product_id.as_deref().unwrap_or(""),
            self.serial.as_deref().unwrap_or(""),
            self.usb_interface.as_deref().unwrap_or(""),
            self.udev_id_path.as_deref().unwrap_or(""),
            self.bus_info.as_deref().unwrap_or(""),
            &self.endpoint_role,
        ];
        fields
            .iter()
            .map(|field| format!("{}:{field}", field.len()))
            .collect::<Vec<_>>()
            .join("|")
    }

    #[must_use]
    pub fn stable_camera_id(&self) -> String {
        let digest = blake3::hash(self.canonical_identity().as_bytes());
        let encoded = BASE32_NOPAD.encode(digest.as_bytes()).to_ascii_lowercase();
        format!("cam_{}", &encoded[..12])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalCamera {
    pub stable_camera_id: String,
    pub canonical_identity: String,
    pub endpoint: CameraCandidate,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("camera discovery I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("camera discovery command failed: {0}")]
    Command(String),
    #[error("multiple active cameras resolve to stable ID {0}")]
    StableIdCollision(String),
}

pub fn group_logical_cameras(
    candidates: impl IntoIterator<Item = CameraCandidate>,
) -> Result<Vec<LogicalCamera>, DiscoveryError> {
    let mut groups: BTreeMap<String, Vec<CameraCandidate>> = BTreeMap::new();
    for candidate in candidates {
        if candidate.is_supported_capture() {
            groups
                .entry(candidate.logical_parent.clone())
                .or_default()
                .push(candidate);
        }
    }
    let mut cameras = Vec::with_capacity(groups.len());
    let mut identities: BTreeMap<String, String> = BTreeMap::new();
    for (_, mut endpoints) in groups {
        endpoints.sort_by_key(endpoint_preference);
        let endpoint = endpoints.remove(0);
        let stable_camera_id = endpoint.stable_camera_id();
        let canonical_identity = endpoint.canonical_identity();
        if let Some(existing) =
            identities.insert(stable_camera_id.clone(), canonical_identity.clone())
        {
            if existing != canonical_identity {
                return Err(DiscoveryError::StableIdCollision(stable_camera_id));
            }
        }
        cameras.push(LogicalCamera {
            stable_camera_id,
            canonical_identity,
            endpoint,
        });
    }
    Ok(cameras)
}

fn endpoint_preference(candidate: &CameraCandidate) -> (u8, PathBuf) {
    let role_rank = match candidate.endpoint_role.as_str() {
        "rgb" | "primary" => 0,
        "alternate" => 1,
        _ => 2,
    };
    (role_rank, candidate.device_path.clone())
}

#[derive(Debug, Clone)]
pub enum Selector {
    Id(String),
    Serial(String),
    Path(PathBuf),
    Name(String),
    NameRegex(Regex),
    UsbPath(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SelectorError {
    #[error("selector is empty")]
    Empty,
    #[error("unsupported selector kind {0}")]
    Unsupported(String),
    #[error("invalid regular expression: {0}")]
    Regex(String),
    #[error("anchor selector matched no active cameras")]
    AnchorMissing,
    #[error("anchor selector matched multiple active cameras: {0:?}")]
    AnchorAmbiguous(Vec<String>),
    #[error("anchor camera cannot be disabled or stream-excluded")]
    AnchorPolicyConflict,
}

impl Selector {
    pub fn parse(raw: &str) -> Result<Self, SelectorError> {
        let (kind, value) = raw.split_once(':').ok_or(SelectorError::Empty)?;
        if value.is_empty() {
            return Err(SelectorError::Empty);
        }
        match kind {
            "id" => Ok(Self::Id(value.to_owned())),
            "serial" => Ok(Self::Serial(value.to_owned())),
            "path" => Ok(Self::Path(PathBuf::from(value))),
            "name" => Ok(Self::Name(value.to_owned())),
            "name_regex" => Regex::new(value)
                .map(Self::NameRegex)
                .map_err(|error| SelectorError::Regex(error.to_string())),
            "usb_path" => Ok(Self::UsbPath(value.to_owned())),
            _ => Err(SelectorError::Unsupported(kind.to_owned())),
        }
    }

    #[must_use]
    pub fn matches(&self, camera: &LogicalCamera) -> bool {
        match self {
            Self::Id(value) => camera.stable_camera_id == *value,
            Self::Serial(value) => camera.endpoint.serial.as_deref() == Some(value),
            Self::Path(value) => camera.endpoint.device_path == *value,
            Self::Name(value) => camera.endpoint.product_name == *value,
            Self::NameRegex(regex) => regex.is_match(&camera.endpoint.product_name),
            Self::UsbPath(value) => camera.endpoint.udev_id_path.as_deref() == Some(value),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CameraPolicy {
    pub disable: Vec<Selector>,
    pub stream_exclude: Vec<Selector>,
    pub anchor: Selector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedCamera {
    pub camera: LogicalCamera,
    pub stream_enabled: bool,
    pub anchor: bool,
}

impl CameraPolicy {
    pub fn evaluate(
        &self,
        cameras: &[LogicalCamera],
    ) -> Result<Vec<EvaluatedCamera>, SelectorError> {
        let mut enabled = Vec::new();
        let mut disabled_anchor = false;
        for camera in cameras {
            if self.disable.iter().any(|selector| selector.matches(camera)) {
                disabled_anchor |= self.anchor.matches(camera);
                continue;
            }
            let stream_enabled = !self
                .stream_exclude
                .iter()
                .any(|selector| selector.matches(camera));
            enabled.push(EvaluatedCamera {
                camera: camera.clone(),
                stream_enabled,
                anchor: false,
            });
        }
        let matches: Vec<_> = enabled
            .iter()
            .enumerate()
            .filter(|(_, item)| self.anchor.matches(&item.camera))
            .map(|(index, item)| (index, item.camera.stable_camera_id.clone()))
            .collect();
        if disabled_anchor {
            return Err(SelectorError::AnchorPolicyConflict);
        }
        let [(anchor_index, _)] = matches.as_slice() else {
            return if matches.is_empty() {
                Err(SelectorError::AnchorMissing)
            } else {
                Err(SelectorError::AnchorAmbiguous(
                    matches.into_iter().map(|(_, id)| id).collect(),
                ))
            };
        };
        if !enabled[*anchor_index].stream_enabled {
            return Err(SelectorError::AnchorPolicyConflict);
        }
        enabled[*anchor_index].anchor = true;
        Ok(enabled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingState {
    Active,
    Tombstone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraMapping {
    pub stable_camera_id: String,
    pub canonical_identity: String,
    pub stream_slot: u16,
    pub virtual_device_number: u32,
    pub state: MappingState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MapPayload {
    generation: u64,
    entries: Vec<CameraMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MapFile {
    generation: u64,
    entries: Vec<CameraMapping>,
    checksum_blake3: String,
}

#[derive(Debug, Error)]
pub enum MappingError {
    #[error("mapping I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("mapping JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("mapping checksum is invalid")]
    Checksum,
    #[error("stable ID collision for {0}")]
    Collision(String),
    #[error("no free camera slot is available")]
    SlotExhausted,
    #[error("mapping generation overflow")]
    GenerationOverflow,
}

#[derive(Debug, Clone, Default)]
pub struct CameraMap {
    generation: u64,
    entries: Vec<CameraMapping>,
}

impl CameraMap {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MappingError> {
        let file: MapFile = serde_json::from_slice(&fs::read(path)?)?;
        let payload = MapPayload {
            generation: file.generation,
            entries: file.entries.clone(),
        };
        if checksum(&payload)? != file.checksum_blake3 {
            return Err(MappingError::Checksum);
        }
        Ok(Self {
            generation: file.generation,
            entries: file.entries,
        })
    }

    #[must_use]
    pub fn entries(&self) -> &[CameraMapping] {
        &self.entries
    }

    pub fn allocate_or_reuse(
        &mut self,
        camera: &LogicalCamera,
        max_slots: u16,
        virtual_start: u32,
    ) -> Result<&CameraMapping, MappingError> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.stable_camera_id == camera.stable_camera_id)
        {
            if self.entries[index].canonical_identity != camera.canonical_identity {
                return Err(MappingError::Collision(camera.stable_camera_id.clone()));
            }
            self.entries[index].state = MappingState::Active;
            return Ok(&self.entries[index]);
        }
        let occupied: BTreeSet<_> = self.entries.iter().map(|entry| entry.stream_slot).collect();
        let slot = (0..max_slots)
            .find(|slot| !occupied.contains(slot))
            .ok_or(MappingError::SlotExhausted)?;
        self.entries.push(CameraMapping {
            stable_camera_id: camera.stable_camera_id.clone(),
            canonical_identity: camera.canonical_identity.clone(),
            stream_slot: slot,
            virtual_device_number: virtual_start + u32::from(slot),
            state: MappingState::Active,
        });
        Ok(self.entries.last().expect("entry was just inserted"))
    }

    pub fn tombstone_missing(&mut self, active_ids: &BTreeSet<String>) {
        for entry in &mut self.entries {
            if !active_ids.contains(&entry.stable_camera_id) {
                entry.state = MappingState::Tombstone;
            }
        }
    }

    pub fn reclaim_tombstone(&mut self, stable_camera_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.stable_camera_id != stable_camera_id || entry.state != MappingState::Tombstone
        });
        before != self.entries.len()
    }

    pub fn save_atomic(&mut self, path: impl AsRef<Path>) -> Result<(), MappingError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(MappingError::GenerationOverflow)?;
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let payload = MapPayload {
            generation: self.generation,
            entries: self.entries.clone(),
        };
        let file = MapFile {
            generation: payload.generation,
            entries: payload.entries.clone(),
            checksum_blake3: checksum(&payload)?,
        };
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let mut handle = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut handle, &file)?;
        handle.write_all(b"\n")?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temp, path)?;
        sync_parent(parent)?;
        Ok(())
    }
}

fn checksum(payload: &MapPayload) -> Result<String, serde_json::Error> {
    Ok(blake3::hash(&serde_json::to_vec(payload)?)
        .to_hex()
        .to_string())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[async_trait]
pub trait Discovery: Send + Sync {
    async fn snapshot(&self) -> Result<Vec<CameraCandidate>, DiscoveryError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotplugEvent {
    Added(String),
    Removed(String),
}

pub struct HotplugMonitor<D> {
    discovery: D,
    interval: Duration,
}

impl<D: Discovery> HotplugMonitor<D> {
    #[must_use]
    pub const fn new(discovery: D, interval: Duration) -> Self {
        Self {
            discovery,
            interval,
        }
    }

    pub async fn run(self, sender: mpsc::Sender<HotplugEvent>) -> Result<(), DiscoveryError> {
        let mut known = BTreeSet::new();
        loop {
            let current: BTreeSet<_> = group_logical_cameras(self.discovery.snapshot().await?)?
                .into_iter()
                .map(|camera| camera.stable_camera_id)
                .collect();
            for added in current.difference(&known) {
                if sender
                    .send(HotplugEvent::Added(added.clone()))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            for removed in known.difference(&current) {
                if sender
                    .send(HotplugEvent::Removed(removed.clone()))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            known = current;
            tokio::time::sleep(self.interval).await;
        }
    }
}

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(test)]
mod tests;
