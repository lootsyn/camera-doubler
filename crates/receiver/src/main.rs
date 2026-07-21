use std::env;
#[cfg(target_os = "linux")]
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
#[cfg(target_os = "linux")]
use receiver::{ReceiverPolicy, ReceiverRegistry};
use robot_multicam_common::{env_or, init_tracing, required_env};
use robot_multicam_protocol::RuntimeConstants;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod linux;

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct Config {
    constants_path: PathBuf,
    data_root: PathBuf,
    embodiment_id: String,
    edge_instance_id: Option<String>,
    base_port: u16,
    max_cameras: u16,
    latency_ms: u32,
    passphrase_file: PathBuf,
    hmac_key_file: PathBuf,
    pbkeylen: u16,
    pending_frames: usize,
    pending_bytes: usize,
    metrics_bind: String,
    grpc_bind: String,
    manifest_wait_sec: u64,
    max_camera_skew_ns: u64,
    synchronized_step_capacity: usize,
    minimum_free_disk_bytes: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        let edge_instance_id = env::var("EXPECTED_EDGE_INSTANCE_ID")
            .ok()
            .filter(|value| !value.is_empty());
        let value = Self {
            constants_path: env::var("PROTOCOL_CONSTANTS_CONFIG").map_or_else(
                |_| PathBuf::from("config/protocol_constants.toml"),
                PathBuf::from,
            ),
            data_root: env::var("DATA_ROOT").map_or_else(|_| PathBuf::from("/data"), PathBuf::from),
            embodiment_id: required_env("EMBODIMENT_ID")?,
            edge_instance_id,
            base_port: env_or("SRT_LISTEN_BASE_PORT", 10_000)?,
            max_cameras: env_or("MAX_CAMERAS", 16)?,
            latency_ms: env_or("SRT_LATENCY_MS", 120)?,
            passphrase_file: PathBuf::from(required_env("SRT_PASSPHRASE_FILE")?),
            hmac_key_file: PathBuf::from(required_env("SRT_STREAMID_HMAC_KEY_FILE")?),
            pbkeylen: env_or("SRT_PBKEYLEN", 32)?,
            pending_frames: env_or("MANIFEST_PENDING_MAX_FRAMES", 300)?,
            pending_bytes: env_or("MANIFEST_REASSEMBLY_MAX_BYTES", 262_144)?,
            metrics_bind: env::var("RECEIVER_METRICS_BIND")
                .unwrap_or_else(|_| "0.0.0.0:9090".to_owned()),
            grpc_bind: env::var("RECEIVER_GRPC_BIND").unwrap_or_else(|_| "0.0.0.0:8083".to_owned()),
            manifest_wait_sec: env_or("MANIFEST_WAIT_TIMEOUT_SEC", 10)?,
            max_camera_skew_ns: env_or::<u64>("MAX_CAMERA_SKEW_MS", 20)?.saturating_mul(1_000_000),
            synchronized_step_capacity: env_or("SYNCHRONIZED_STEP_BUFFER", 256)?,
            minimum_free_disk_bytes: env_or::<u64>("MIN_FREE_DISK_GB", 20)?
                .checked_mul(1_000_000_000)
                .context("MIN_FREE_DISK_GB overflow")?,
        };
        if value.max_cameras == 0
            || value.pending_frames == 0
            || value.pending_bytes == 0
            || value.manifest_wait_sec == 0
            || value.max_camera_skew_ns == 0
            || value.synchronized_step_capacity == 0
            || value.minimum_free_disk_bytes == 0
            || !matches!(value.pbkeylen, 16 | 24 | 32)
        {
            return Err(anyhow!("invalid bounded Receiver configuration"));
        }
        value
            .base_port
            .checked_add(value.max_cameras - 1)
            .context("SRT listener port block exceeds u16")?;
        Ok(value)
    }

    #[cfg(target_os = "linux")]
    fn policy(&self) -> ReceiverPolicy {
        ReceiverPolicy {
            expected_embodiment_id: self.embodiment_id.clone(),
            expected_edge_instance_id: self.edge_instance_id.clone(),
            base_port: self.base_port,
            max_cameras: self.max_cameras,
            max_ingest_frames: self.pending_frames,
            max_ingest_bytes: self.pending_bytes,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("robot-multicam-receiver");
    let command = env::args().nth(1).unwrap_or_else(|| "serve".to_owned());
    match command.as_str() {
        "serve" => serve().await,
        "healthcheck" => healthcheck(),
        "self-test" => self_test(),
        _ => Err(anyhow!("unknown command {command}")),
    }
}

fn healthcheck() -> Result<()> {
    let config = Config::from_env()?;
    RuntimeConstants::load_and_validate(&config.constants_path)?;
    if !config.data_root.is_dir() {
        return Err(anyhow!("DATA_ROOT is not a directory"));
    }
    #[cfg(target_os = "linux")]
    linux::verify_gstreamer_runtime()?;
    println!("healthy");
    Ok(())
}

fn self_test() -> Result<()> {
    use robot_multicam_metadata_codec::{inject_timestamp_h264_annex_b, inspect_h264_annex_b};
    let au = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
    let encoded = inject_timestamp_h264_annex_b(&au, 99)?;
    let inspected =
        inspect_h264_annex_b(&encoded, true)?.context("timestamp missing in Receiver self-test")?;
    if inspected.timestamp.capture_time_edge_ns != 99 {
        return Err(anyhow!("Receiver metadata self-test mismatch"));
    }
    println!("self-test passed");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn serve() -> Result<()> {
    Err(anyhow!("the production Receiver runtime requires Linux"))
}

#[cfg(target_os = "linux")]
async fn serve() -> Result<()> {
    use linux::{verify_gstreamer_runtime, ListenerHandle};
    use receiver::runtime::{serve_metadata, ReceiverRuntime};
    use robot_multicam_common::{serve_observability, CheckStatus, Observability};
    use std::sync::Arc;

    let config = Config::from_env()?;
    RuntimeConstants::load_and_validate(&config.constants_path)?;
    verify_gstreamer_runtime()?;
    fs::create_dir_all(&config.data_root)?;
    let observability = Arc::new(Observability::new());
    observability.set_check("protocol_constants", CheckStatus::Pass)?;
    observability.set_check("gstreamer_runtime", CheckStatus::Pass)?;
    observability.set_check("data_root", CheckStatus::Pass)?;
    update_disk_readiness(
        &config.data_root,
        config.minimum_free_disk_bytes,
        &observability,
    )?;
    let telemetry_bind = config.metrics_bind.clone();
    let telemetry_state = Arc::clone(&observability);
    let _observability_task = tokio::spawn(async move {
        if let Err(error) = serve_observability(&telemetry_bind, telemetry_state).await {
            tracing::error!(%error, "observability server stopped");
        }
    });
    let disk_root = config.data_root.clone();
    let disk_minimum = config.minimum_free_disk_bytes;
    let disk_state = Arc::clone(&observability);
    let _disk_task = tokio::spawn(async move {
        loop {
            if let Err(error) = update_disk_readiness(&disk_root, disk_minimum, &disk_state) {
                tracing::error!(%error, "disk readiness probe failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    let passphrase = read_secret(&config.passphrase_file, 10, 79)?;
    let hmac_key = read_secret_bytes(&config.hmac_key_file, 32, 4_096)?;
    let registry = Arc::new(ReceiverRegistry::new(config.policy(), hmac_key));
    let runtime = Arc::new(ReceiverRuntime::new(
        Arc::clone(&registry),
        config.base_port,
        std::time::Duration::from_secs(config.manifest_wait_sec),
        config.pending_frames,
        config.max_camera_skew_ns,
        config.synchronized_step_capacity,
    )?);
    let grpc_bind = config.grpc_bind.clone();
    let grpc_runtime = Arc::clone(&runtime);
    let _grpc_task = tokio::spawn(async move {
        if let Err(error) = serve_metadata(&grpc_bind, grpc_runtime).await {
            tracing::error!(%error, "Receiver metadata API stopped");
        }
    });
    observability.set_check("receiver_metadata_api", CheckStatus::Pass)?;
    let mut listeners = Vec::with_capacity(usize::from(config.max_cameras));
    for slot in 0..config.max_cameras {
        let port = config
            .base_port
            .checked_add(slot)
            .context("port overflow")?;
        listeners.push(ListenerHandle::start(
            port,
            config.latency_ms,
            &passphrase,
            config.pbkeylen,
            Arc::clone(&registry),
            Arc::clone(&runtime),
        )?);
        observability.increment("receiver_listeners_started_total", 1)?;
    }
    observability.set_check("srt_listeners", CheckStatus::Pass)?;
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for shutdown signal")?;
    drop(listeners);
    Ok(())
}

#[cfg(target_os = "linux")]
fn update_disk_readiness(
    data_root: &std::path::Path,
    minimum_free_bytes: u64,
    observability: &robot_multicam_common::Observability,
) -> Result<()> {
    use receiver::replay::{disk_readiness, DiskReadiness};
    use robot_multicam_common::CheckStatus;

    let free_bytes = fs2::available_space(data_root)?;
    let status = disk_readiness(free_bytes, minimum_free_bytes, 0, u64::MAX);
    observability.set_check(
        "disk_pressure",
        match status {
            DiskReadiness::Ready => CheckStatus::Pass,
            DiskReadiness::Low => CheckStatus::Fail(format!(
                "low disk: free={free_bytes} minimum={minimum_free_bytes}"
            )),
            DiskReadiness::Full => CheckStatus::Fail("disk full".to_owned()),
        },
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_secret(path: &std::path::Path, min: usize, max: usize) -> Result<String> {
    String::from_utf8(read_secret_bytes(path, min, max)?)
        .context("SRT passphrase is not valid UTF-8")
}

#[cfg(target_os = "linux")]
fn read_secret_bytes(path: &std::path::Path, min: usize, max: usize) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("unable to read {}", path.display()))?;
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    if !(min..=max).contains(&trimmed.len()) {
        return Err(anyhow!("secret {} has an invalid size", path.display()));
    }
    Ok(trimmed.to_vec())
}
