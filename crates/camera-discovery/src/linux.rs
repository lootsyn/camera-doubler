//! Linux discovery combines the GStreamer `GstDeviceMonitor` CLI with sysfs/udev/V4L2.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;

use crate::{CameraCandidate, Discovery, DiscoveryError};

#[derive(Debug, Clone)]
pub struct LinuxDiscovery {
    sys_class_video4linux: PathBuf,
}

impl Default for LinuxDiscovery {
    fn default() -> Self {
        Self {
            sys_class_video4linux: PathBuf::from("/sys/class/video4linux"),
        }
    }
}

#[async_trait]
impl Discovery for LinuxDiscovery {
    async fn snapshot(&self) -> Result<Vec<CameraCandidate>, DiscoveryError> {
        let gst_devices = gst_device_monitor_paths()?;
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.sys_class_video4linux)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("video") {
                continue;
            }
            let device_path = Path::new("/dev").join(&name);
            let properties = udev_properties(&device_path)?;
            let capabilities = v4l2_capabilities(&device_path)?;
            let sys_device = entry.path().join("device");
            let driver = fs::canonicalize(sys_device.join("driver"))
                .ok()
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                });
            let product_name = read_trimmed(entry.path().join("name"))
                .unwrap_or_else(|| name.to_string_lossy().into_owned());
            let id_path = properties.get("ID_PATH").cloned();
            let serial = properties
                .get("ID_SERIAL_SHORT")
                .or_else(|| properties.get("ID_SERIAL"))
                .cloned();
            let usb_interface = properties.get("ID_USB_INTERFACE_NUM").cloned();
            let bus_info = capabilities.get("bus_info").cloned();
            let logical_parent = id_path
                .clone()
                .or_else(|| serial.clone().map(|value| format!("serial:{value}")))
                .or_else(|| bus_info.clone())
                .unwrap_or_else(|| {
                    fs::canonicalize(&sys_device)
                        .unwrap_or(sys_device)
                        .display()
                        .to_string()
                });
            let capture = capabilities.contains_key("video_capture")
                || capabilities.contains_key("video_capture_mplane");
            let metadata = capabilities.contains_key("metadata_capture");
            let output = !capture
                && (capabilities.contains_key("video_output")
                    || capabilities.contains_key("video_output_mplane"));
            result.push(CameraCandidate {
                device_path: device_path.clone(),
                product_name: product_name.clone(),
                vendor_id: properties.get("ID_VENDOR_ID").cloned(),
                product_id: properties.get("ID_MODEL_ID").cloned(),
                serial,
                usb_interface,
                udev_id_path: id_path,
                bus_info,
                media_entity: properties.get("ID_V4L_PRODUCT").cloned(),
                endpoint_role: if product_name.to_ascii_lowercase().contains("metadata") {
                    "metadata".to_owned()
                } else {
                    "primary".to_owned()
                },
                driver,
                logical_parent,
                capture_capable: capture,
                output_capable: output,
                metadata_only: metadata && !capture,
                supported_caps: gst_devices.contains(&device_path),
                managed_virtual_label: product_name.starts_with("LeRobot Virtual"),
            });
        }
        Ok(result)
    }
}

fn gst_device_monitor_paths() -> Result<BTreeSet<PathBuf>, DiscoveryError> {
    let output = Command::new("gst-device-monitor-1.0")
        .args(["--timeout=1", "Video/Source"])
        .output()?;
    if !output.status.success() {
        return Err(DiscoveryError::Command(format!(
            "gst-device-monitor-1.0 exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| ch == '\'' || ch == '"' || ch == ','))
        .filter(|token| token.starts_with("/dev/video"))
        .map(PathBuf::from)
        .collect())
}

fn udev_properties(device: &Path) -> Result<BTreeMap<String, String>, DiscoveryError> {
    let output = Command::new("udevadm")
        .args(["info", "--query=property", "--name"])
        .arg(device)
        .output()?;
    if !output.status.success() {
        return Err(DiscoveryError::Command(format!(
            "udevadm failed for {}: {}",
            device.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(parse_key_values(&String::from_utf8_lossy(&output.stdout)))
}

fn v4l2_capabilities(device: &Path) -> Result<BTreeMap<String, String>, DiscoveryError> {
    let output = Command::new("v4l2-ctl")
        .args(["--all", "--device"])
        .arg(device)
        .output()?;
    if !output.status.success() {
        return Err(DiscoveryError::Command(format!(
            "v4l2-ctl failed for {}: {}",
            device.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lower = text.to_ascii_lowercase();
    let mut values = BTreeMap::new();
    for key in [
        "video_capture",
        "video_capture_mplane",
        "video_output",
        "video_output_mplane",
        "metadata_capture",
    ] {
        let phrase = key.replace('_', " ");
        if lower.contains(&phrase) {
            values.insert(key.to_owned(), "true".to_owned());
        }
    }
    for line in text.lines() {
        if let Some((key, value)) = line.trim().split_once(':') {
            if key.eq_ignore_ascii_case("bus info") {
                values.insert("bus_info".to_owned(), value.trim().to_owned());
            }
        }
    }
    Ok(values)
}

fn parse_key_values(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
