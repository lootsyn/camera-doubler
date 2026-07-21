//! Per-camera v4l2loopback allocation boundary.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualCameraSpec {
    pub stable_camera_id: String,
    pub device_number: u32,
    pub timeout_ms: u32,
}

impl VirtualCameraSpec {
    #[must_use]
    pub fn device_path(&self) -> PathBuf {
        PathBuf::from(format!("/dev/video{}", self.device_number))
    }
}

#[derive(Debug, Error)]
pub enum VirtualCameraError {
    #[error("virtual camera I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is not a v4l2loopback output device")]
    NotLoopback(PathBuf),
    #[error("v4l2-ctl failed for {device}: {stderr}")]
    Control { device: PathBuf, stderr: String },
    #[error("virtual camera manager lock is poisoned")]
    LockPoisoned,
}

#[async_trait]
pub trait VirtualCameraManager: Send + Sync {
    async fn ensure(&self, spec: &VirtualCameraSpec) -> Result<PathBuf, VirtualCameraError>;
    async fn release(&self, stable_camera_id: &str) -> Result<(), VirtualCameraError>;
}

#[derive(Debug, Default)]
pub struct LinuxV4l2LoopbackManager;

#[async_trait]
impl VirtualCameraManager for LinuxV4l2LoopbackManager {
    async fn ensure(&self, spec: &VirtualCameraSpec) -> Result<PathBuf, VirtualCameraError> {
        let device = spec.device_path();
        verify_loopback(&device)?;
        let output = Command::new("v4l2-ctl")
            .arg("--device")
            .arg(&device)
            .arg(format!(
                "--set-ctrl=keep_format=1,sustain_framerate=0,timeout={}",
                spec.timeout_ms
            ))
            .output()?;
        if !output.status.success() {
            return Err(VirtualCameraError::Control {
                device,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(spec.device_path())
    }

    async fn release(&self, _stable_camera_id: &str) -> Result<(), VirtualCameraError> {
        // The persistent v4l2loopback pool stays loaded; pipeline teardown releases the writer.
        Ok(())
    }
}

fn verify_loopback(device: &Path) -> Result<(), VirtualCameraError> {
    let basename = device
        .file_name()
        .ok_or_else(|| VirtualCameraError::NotLoopback(device.to_path_buf()))?;
    let driver = Path::new("/sys/class/video4linux")
        .join(basename)
        .join("device/driver");
    let resolved = std::fs::canonicalize(driver)?;
    if resolved.file_name().and_then(|name| name.to_str()) != Some("v4l2loopback") {
        return Err(VirtualCameraError::NotLoopback(device.to_path_buf()));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct MockVirtualCameraManager {
    active: Mutex<BTreeSet<String>>,
}

impl MockVirtualCameraManager {
    pub fn contains(&self, stable_camera_id: &str) -> Result<bool, VirtualCameraError> {
        Ok(self
            .active
            .lock()
            .map_err(|_| VirtualCameraError::LockPoisoned)?
            .contains(stable_camera_id))
    }
}

#[async_trait]
impl VirtualCameraManager for MockVirtualCameraManager {
    async fn ensure(&self, spec: &VirtualCameraSpec) -> Result<PathBuf, VirtualCameraError> {
        self.active
            .lock()
            .map_err(|_| VirtualCameraError::LockPoisoned)?
            .insert(spec.stable_camera_id.clone());
        Ok(spec.device_path())
    }

    async fn release(&self, stable_camera_id: &str) -> Result<(), VirtualCameraError> {
        self.active
            .lock()
            .map_err(|_| VirtualCameraError::LockPoisoned)?
            .remove(stable_camera_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MockVirtualCameraManager, VirtualCameraManager, VirtualCameraSpec};

    #[tokio::test]
    async fn mock_manager_has_explicit_lifecycle() {
        let manager = MockVirtualCameraManager::default();
        let spec = VirtualCameraSpec {
            stable_camera_id: "cam_test".to_owned(),
            device_number: 40,
            timeout_ms: 3_000,
        };
        assert_eq!(
            manager
                .ensure(&spec)
                .await
                .expect("ensure")
                .to_string_lossy(),
            "/dev/video40"
        );
        assert!(manager.contains("cam_test").expect("contains"));
        manager.release("cam_test").await.expect("release");
        assert!(!manager.contains("cam_test").expect("contains"));
    }
}
