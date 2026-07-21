//! Edge camera branch primitives and platform runtime.

pub mod context;
pub mod control;
pub mod gateway;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use serde::Serialize;
use thiserror::Error;

pub trait SemanticMetadataProvider: Send + Sync {
    fn for_access_unit(
        &self,
        capture_time_edge_ns: u64,
        access_unit_ordinal: u64,
    ) -> anyhow::Result<Vec<robot_multicam_metadata_codec::UserDataUnregistered>>;
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum QueueError {
    #[error("queue capacity must be greater than zero")]
    ZeroCapacity,
    #[error("queue is closed")]
    Closed,
    #[error("queue lock is poisoned")]
    Poisoned,
}

#[derive(Debug)]
struct QueueState<T> {
    items: VecDeque<T>,
    closed: bool,
    dropped: u64,
}

#[derive(Debug)]
pub struct LatestQueue<T> {
    capacity: usize,
    state: Mutex<QueueState<T>>,
    available: Condvar,
}

impl<T> LatestQueue<T> {
    pub fn new(capacity: usize) -> Result<Self, QueueError> {
        if capacity == 0 {
            return Err(QueueError::ZeroCapacity);
        }
        Ok(Self {
            capacity,
            state: Mutex::new(QueueState {
                items: VecDeque::with_capacity(capacity),
                closed: false,
                dropped: 0,
            }),
            available: Condvar::new(),
        })
    }

    pub fn push(&self, item: T) -> Result<(), QueueError> {
        let mut state = self.state.lock().map_err(|_| QueueError::Poisoned)?;
        if state.closed {
            return Err(QueueError::Closed);
        }
        if state.items.len() == self.capacity {
            state.items.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
        state.items.push_back(item);
        self.available.notify_one();
        Ok(())
    }

    pub fn pop_blocking(&self) -> Result<Option<T>, QueueError> {
        let mut state = self.state.lock().map_err(|_| QueueError::Poisoned)?;
        loop {
            if let Some(item) = state.items.pop_front() {
                return Ok(Some(item));
            }
            if state.closed {
                return Ok(None);
            }
            state = self
                .available
                .wait(state)
                .map_err(|_| QueueError::Poisoned)?;
        }
    }

    pub fn close(&self) -> Result<(), QueueError> {
        let mut state = self.state.lock().map_err(|_| QueueError::Poisoned)?;
        state.closed = true;
        self.available.notify_all();
        Ok(())
    }

    pub fn dropped(&self) -> Result<u64, QueueError> {
        Ok(self.state.lock().map_err(|_| QueueError::Poisoned)?.dropped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAccessUnit {
    pub bytes: Vec<u8>,
    pub pts_ns: u64,
    pub dts_ns: Option<u64>,
    pub duration_ns: Option<u64>,
    pub capture_time_edge_ns: u64,
}

#[derive(Debug, Clone)]
pub struct CameraPipelinePlan {
    pub physical_device: PathBuf,
    pub virtual_device: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyint_frames: u32,
}

impl CameraPipelinePlan {
    pub fn capture_description(&self) -> Result<String, PipelinePlanError> {
        let source = quote_path(&self.physical_device)?;
        let sink = quote_path(&self.virtual_device)?;
        if self.width == 0
            || self.height == 0
            || self.fps == 0
            || self.keyint_frames == 0
            || self.bitrate_kbps == 0
        {
            return Err(PipelinePlanError::ZeroValue);
        }
        Ok(format!(
            "v4l2src device={source} do-timestamp=true ! videoconvert ! video/x-raw,width={},height={},framerate={}/1 ! tee name=camera_tee camera_tee. ! queue max-size-buffers=2 max-size-bytes=0 max-size-time=0 leaky=downstream ! videoconvert ! v4l2sink device={sink} sync=false camera_tee. ! queue max-size-buffers=4 max-size-bytes=0 max-size-time=0 leaky=downstream ! x264enc tune=zerolatency speed-preset=veryfast bitrate={} key-int-max={} bframes=0 aud=true ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au ! appsink name=encoded_au max-buffers=4 drop=true sync=false",
            self.width, self.height, self.fps, self.bitrate_kbps, self.keyint_frames
        ))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PipelinePlanError {
    #[error("pipeline path contains an unsafe character")]
    UnsafePath,
    #[error("pipeline numeric values must be nonzero")]
    ZeroValue,
}

fn quote_path(path: &Path) -> Result<String, PipelinePlanError> {
    let raw = path.to_str().ok_or(PipelinePlanError::UnsafePath)?;
    if raw.contains(['"', '\'', '\\', '\n', '\r', '\0']) {
        return Err(PipelinePlanError::UnsafePath);
    }
    Ok(format!("\"{raw}\""))
}

#[derive(Debug, Clone, Serialize)]
pub struct CameraExport {
    pub stable_camera_id: String,
    pub physical_device: PathBuf,
    pub virtual_device: PathBuf,
    pub stream_slot: u16,
    pub stream_enabled: bool,
    pub anchor: bool,
}

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(test)]
mod tests {
    use super::{CameraPipelinePlan, LatestQueue};
    use std::path::PathBuf;

    #[test]
    fn bounded_queue_drops_oldest_without_blocking_producer() {
        let queue = LatestQueue::new(2).expect("queue");
        queue.push(1).expect("push");
        queue.push(2).expect("push");
        queue.push(3).expect("push");
        assert_eq!(queue.dropped().expect("metric"), 1);
        assert_eq!(queue.pop_blocking().expect("pop"), Some(2));
        assert_eq!(queue.pop_blocking().expect("pop"), Some(3));
    }

    #[test]
    fn pipeline_has_independent_leaky_ui_and_stream_queues() {
        let plan = CameraPipelinePlan {
            physical_device: PathBuf::from("/dev/video0"),
            virtual_device: PathBuf::from("/dev/video40"),
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4_000,
            keyint_frames: 30,
        };
        let description = plan.capture_description().expect("description");
        assert_eq!(description.matches("leaky=downstream").count(), 2);
        assert!(description.contains("v4l2sink"));
        assert!(description.contains("appsink name=encoded_au"));
    }
}
