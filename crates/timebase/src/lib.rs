//! Vendor-neutral Edge Timebase primitives.

pub mod clock_mapper;

use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use thiserror::Error;
use uuid::Uuid;

static ORIGIN: OnceLock<Instant> = OnceLock::new();

#[derive(Debug, Error)]
pub enum TimebaseError {
    #[error("unable to read Linux boot ID: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid Linux boot ID: {0}")]
    InvalidBootId(#[from] uuid::Error),
    #[error("monotonic nanosecond value exceeds u64")]
    Overflow,
}

/// Process-monotonic clock suitable for tests and fallback correlation.
/// Production Linux capture uses GStreamer/CLOCK_MONOTONIC at the source probe.
pub fn monotonic_now_ns() -> Result<u64, TimebaseError> {
    let elapsed = ORIGIN.get_or_init(Instant::now).elapsed().as_nanos();
    u64::try_from(elapsed).map_err(|_| TimebaseError::Overflow)
}

pub fn read_boot_id(path: impl AsRef<Path>) -> Result<Uuid, TimebaseError> {
    Ok(Uuid::parse_str(fs::read_to_string(path)?.trim())?)
}

pub fn system_boot_id() -> Result<Uuid, TimebaseError> {
    read_boot_id("/proc/sys/kernel/random/boot_id")
}

#[cfg(test)]
mod tests {
    use super::{monotonic_now_ns, read_boot_id};
    use std::fs;

    #[test]
    fn process_clock_is_monotonic() {
        let a = monotonic_now_ns().expect("clock");
        let b = monotonic_now_ns().expect("clock");
        assert!(b >= a);
    }

    #[test]
    fn boot_id_parser_rejects_non_uuid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("boot_id");
        fs::write(&path, "not-a-uuid\n").expect("fixture");
        assert!(read_boot_id(path).is_err());
    }
}
