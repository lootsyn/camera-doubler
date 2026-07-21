#[cfg(target_os = "linux")]
use std::collections::{BTreeMap, BTreeSet};
use std::env;
#[cfg(target_os = "linux")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use robot_multicam_common::{env_or, init_tracing, required_env};
use robot_multicam_metadata_codec::{inject_timestamp_h264_annex_b, inspect_h264_annex_b};
use robot_multicam_protocol::RuntimeConstants;

#[cfg(target_os = "linux")]
mod ui_runtime;

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct EdgeConfig {
    constants_path: PathBuf,
    embodiment_path: PathBuf,
    state_dir: PathBuf,
    anchor_selector: String,
    disable: String,
    stream_exclude: String,
    virtual_start: u32,
    virtual_pool_size: u16,
    virtual_timeout_ms: u32,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    keyint_frames: u32,
    target_host: String,
    base_port: u16,
    latency_ms: u32,
    passphrase_file: PathBuf,
    hmac_key_file: PathBuf,
    pbkeylen: u16,
    embodiment_id: String,
    edge_instance_id: String,
    metrics_bind: String,
    adapter_connect_timeout_sec: u64,
    state_buffer_samples: usize,
    state_max_gap_ns: u64,
    context_budget_bytes: usize,
    manifest_repeat_sec: u64,
    grpc_bind: String,
    control_min_ttl_ms: u64,
    control_max_ttl_ms: u64,
    command_history: usize,
    control_tls_cert: PathBuf,
    control_tls_key: PathBuf,
    control_tls_ca: PathBuf,
}

impl EdgeConfig {
    fn from_env() -> Result<Self> {
        let video_codec = env::var("VIDEO_CODEC").unwrap_or_else(|_| "h264".to_owned());
        if video_codec != "h264" {
            return Err(anyhow!(
                "Phase 1 production path supports VIDEO_CODEC=h264; requested {video_codec}"
            ));
        }
        let bframes: u32 = env_or("VIDEO_BFRAMES", 0)?;
        if bframes != 0 {
            return Err(anyhow!(
                "VIDEO_BFRAMES must be zero for exact AU correlation"
            ));
        }
        let config = Self {
            constants_path: env::var("PROTOCOL_CONSTANTS_CONFIG").map_or_else(
                |_| PathBuf::from("config/protocol_constants.toml"),
                PathBuf::from,
            ),
            embodiment_path: env::var("EMBODIMENT_CONFIG").map_or_else(
                |_| PathBuf::from("config/embodiment.example.yaml"),
                PathBuf::from,
            ),
            state_dir: env::var("EDGE_STATE_DIR")
                .map_or_else(|_| PathBuf::from("/var/lib/robot-edge"), PathBuf::from),
            anchor_selector: required_env("ANCHOR_CAMERA_SELECTOR")?,
            disable: env::var("CAMERA_DISABLE").unwrap_or_default(),
            stream_exclude: env::var("CAMERA_STREAM_EXCLUDE").unwrap_or_default(),
            virtual_start: env_or("VIRTUAL_CAMERA_START", 40)?,
            virtual_pool_size: env_or("VIRTUAL_CAMERA_POOL_SIZE", 16)?,
            virtual_timeout_ms: env_or("VIRTUAL_CAMERA_TIMEOUT_MS", 3_000)?,
            width: env_or("CAMERA_WIDTH", 1_280)?,
            height: env_or("CAMERA_HEIGHT", 720)?,
            fps: env_or("CAMERA_FPS", 30)?,
            bitrate_kbps: env_or("VIDEO_BITRATE_KBPS", 4_000)?,
            keyint_frames: env_or("VIDEO_KEYINT_FRAMES", 30)?,
            target_host: required_env("SRT_TARGET_HOST")?,
            base_port: env_or("SRT_BASE_PORT", 10_000)?,
            latency_ms: env_or("SRT_LATENCY_MS", 120)?,
            passphrase_file: PathBuf::from(required_env("SRT_PASSPHRASE_FILE")?),
            hmac_key_file: PathBuf::from(required_env("SRT_STREAMID_HMAC_KEY_FILE")?),
            pbkeylen: env_or("SRT_PBKEYLEN", 32)?,
            embodiment_id: required_env("EMBODIMENT_ID")?,
            edge_instance_id: required_env("EDGE_INSTANCE_ID")?,
            metrics_bind: env::var("EDGE_METRICS_BIND")
                .unwrap_or_else(|_| "0.0.0.0:9091".to_owned()),
            adapter_connect_timeout_sec: env_or("ADAPTER_CONNECT_TIMEOUT_SEC", 30)?,
            state_buffer_samples: env_or("STATE_BUFFER_MAX_SAMPLES", 1_024)?,
            state_max_gap_ns: env_or::<u64>("STATE_MAX_GAP_MS", 30)?.saturating_mul(1_000_000),
            context_budget_bytes: env_or("ANCHOR_CONTEXT_BUDGET_BYTES", 2_048)?,
            manifest_repeat_sec: env_or("MANIFEST_REPEAT_SEC", 3)?,
            grpc_bind: env::var("EDGE_GRPC_BIND").unwrap_or_else(|_| "0.0.0.0:8082".to_owned()),
            control_min_ttl_ms: env_or("CONTROL_MIN_LEASE_TTL_MS", 100)?,
            control_max_ttl_ms: env_or("CONTROL_MAX_LEASE_TTL_MS", 30_000)?,
            command_history: env_or("CONTROL_COMMAND_HISTORY", 4_096)?,
            control_tls_cert: PathBuf::from(required_env("EDGE_CONTROL_TLS_CERT")?),
            control_tls_key: PathBuf::from(required_env("EDGE_CONTROL_TLS_KEY")?),
            control_tls_ca: PathBuf::from(required_env("EDGE_CONTROL_TLS_CA")?),
        };
        if config.virtual_pool_size == 0
            || config.width == 0
            || config.height == 0
            || config.fps == 0
            || config.adapter_connect_timeout_sec == 0
            || config.state_buffer_samples == 0
            || config.state_max_gap_ns == 0
            || config.context_budget_bytes == 0
            || config.manifest_repeat_sec == 0
            || config.control_min_ttl_ms == 0
            || config.control_max_ttl_ms < config.control_min_ttl_ms
            || config.command_history == 0
            || !matches!(config.pbkeylen, 16 | 24 | 32)
        {
            return Err(anyhow!(
                "invalid zero-sized camera profile or SRT key length"
            ));
        }
        Ok(config)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("robot-edge-core");
    let command = env::args().nth(1).unwrap_or_else(|| "serve".to_owned());
    match command.as_str() {
        "serve" => serve().await,
        "healthcheck" => healthcheck(),
        "self-test" => self_test(),
        _ => Err(anyhow!("unknown command {command}")),
    }
}

fn healthcheck() -> Result<()> {
    let config = EdgeConfig::from_env()?;
    RuntimeConstants::load_and_validate(&config.constants_path)?;
    if !config.state_dir.is_dir() {
        return Err(anyhow!("EDGE_STATE_DIR is not a directory"));
    }
    #[cfg(target_os = "linux")]
    production_media_self_test()?;
    println!("healthy");
    Ok(())
}

#[cfg(target_os = "linux")]
fn production_media_self_test() -> Result<()> {
    use std::process::Command;

    edge_core::linux::verify_gstreamer_runtime()?;
    self_test()?;
    let current = env::current_exe().context("unable to resolve Edge executable")?;
    let directory = current.parent().context("Edge executable has no parent")?;
    let configured = env::var_os("EDGE_SYNTHETIC_SELF_TEST_BIN").map(PathBuf::from);
    let packaged = directory.join("robot-synthetic-roundtrip");
    let development = directory.join("synthetic_roundtrip");
    let binary = configured.unwrap_or_else(|| {
        if packaged.is_file() {
            packaged
        } else {
            development
        }
    });
    if !binary.is_file() {
        return Err(anyhow!(
            "synthetic media self-test binary is missing: {}",
            binary.display()
        ));
    }
    let status = Command::new(&binary)
        .env_remove("SYNTHETIC_ARCHIVE_ROOT")
        .status()
        .with_context(|| format!("unable to run {}", binary.display()))?;
    if !status.success() {
        return Err(anyhow!("synthetic media round-trip failed: {status}"));
    }
    Ok(())
}

fn self_test() -> Result<()> {
    let au = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
    let enriched = inject_timestamp_h264_annex_b(&au, 123_456)?;
    let inspected = inspect_h264_annex_b(&enriched, true)?
        .context("timestamp was not found after insertion")?;
    if inspected.timestamp.capture_time_edge_ns != 123_456 || inspected.messages.len() != 1 {
        return Err(anyhow!("timestamp-only H.264 self-test failed"));
    }
    println!("self-test passed");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn serve() -> Result<()> {
    Err(anyhow!("the production Edge runtime requires Linux"))
}

#[cfg(target_os = "linux")]
async fn serve() -> Result<()> {
    use edge_core::linux::{PipelineHandle, SrtOutput};
    use edge_core::{CameraExport, CameraPipelinePlan};
    use robot_multicam_camera_discovery::linux::LinuxDiscovery;
    use robot_multicam_camera_discovery::{
        group_logical_cameras, CameraMap, CameraPolicy, Discovery, MappingError, Selector,
        SelectorError,
    };
    use robot_multicam_common::{serve_observability, CheckStatus, Observability};
    use robot_multicam_stream_identity::{Codec, Role, StreamIdentity};
    use robot_multicam_timebase::system_boot_id;
    use robot_multicam_virtual_camera::{
        LinuxV4l2LoopbackManager, VirtualCameraManager, VirtualCameraSpec,
    };
    use std::sync::Arc;
    use uuid::Uuid;

    // The fields are lifetime guards: dropping a variant stops its pipeline.
    #[allow(dead_code)]
    enum Running {
        Stream(PipelineHandle),
        Ui(ui_runtime::UiOnlyHandle),
    }

    let config = EdgeConfig::from_env()?;
    RuntimeConstants::load_and_validate(&config.constants_path)?;
    production_media_self_test()?;
    fs::create_dir_all(&config.state_dir)?;
    let observability = Arc::new(Observability::new());
    observability.set_check("protocol_constants", CheckStatus::Pass)?;
    observability.set_check("gstreamer_runtime", CheckStatus::Pass)?;
    observability.set_check("sei_roundtrip", CheckStatus::Pass)?;
    observability.set_check(
        "camera_anchor",
        CheckStatus::Fail("not discovered".to_owned()),
    )?;
    let telemetry_bind = config.metrics_bind.clone();
    let telemetry_state = Arc::clone(&observability);
    let _observability_task = tokio::spawn(async move {
        if let Err(error) = serve_observability(&telemetry_bind, telemetry_state).await {
            tracing::error!(%error, "observability server stopped");
        }
    });
    let mapping_path = config.state_dir.join("camera-map.json");
    let mut mapping = match CameraMap::load(&mapping_path) {
        Ok(value) => value,
        Err(MappingError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            CameraMap::default()
        }
        Err(error) => return Err(error.into()),
    };
    let policy = CameraPolicy {
        disable: parse_selector_list(&config.disable)?,
        stream_exclude: parse_selector_list(&config.stream_exclude)?,
        anchor: Selector::parse(&config.anchor_selector)?,
    };
    let discovery = LinuxDiscovery::default();
    let virtual_manager = LinuxV4l2LoopbackManager;
    let passphrase = read_secret(&config.passphrase_file, 10, 79)?;
    let hmac_key = read_secret_bytes(&config.hmac_key_file, 32, 4_096)?;
    let edge_boot_id = system_boot_id()?;
    let session_id = Uuid::new_v4();
    let adapter_runtime = initialize_adapters(&config).await?;
    let _adapter_tasks = &adapter_runtime.tasks;
    let manifest_store = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let control = build_control_gateway(&config, &adapter_runtime)?;
    let router = Arc::new(edge_core::gateway::UnixAdapterRouter::new(
        adapter_runtime.command_routes.clone(),
        Duration::from_secs(3),
    )?);
    let control_service = edge_core::gateway::ControlGatewayService::new(
        control,
        router,
        Arc::clone(&manifest_store),
    );
    let control_tls = edge_core::gateway::MutualTls {
        certificate_pem: read_secret_bytes(&config.control_tls_cert, 32, 1_048_576)?,
        private_key_pem: read_secret_bytes(&config.control_tls_key, 32, 1_048_576)?,
        client_ca_pem: read_secret_bytes(&config.control_tls_ca, 32, 1_048_576)?,
    };
    let control_bind = config.grpc_bind.clone();
    let _control_task = tokio::spawn(async move {
        if let Err(error) =
            edge_core::gateway::serve_control(&control_bind, control_service, control_tls).await
        {
            tracing::error!(%error, "Edge control API stopped");
        }
    });
    observability.set_check("control_gateway", CheckStatus::Pass)?;
    let mut running: BTreeMap<String, Running> = BTreeMap::new();
    let mut previous_active = BTreeSet::new();
    let mut stream_epochs: BTreeMap<String, u32> = BTreeMap::new();
    let mut manifest_revision = 0_u64;

    loop {
        let logical = group_logical_cameras(discovery.snapshot().await?)?;
        let evaluated = match policy.evaluate(&logical) {
            Ok(value) => value,
            Err(SelectorError::AnchorMissing) => {
                tracing::warn!("anchor camera is missing; running UI/secondary paths degraded");
                evaluate_without_anchor(&logical, &policy)
            }
            Err(error) => return Err(error.into()),
        };
        let active: BTreeSet<_> = evaluated
            .iter()
            .map(|item| item.camera.stable_camera_id.clone())
            .collect();
        if evaluated.iter().any(|item| item.anchor) {
            observability.set_check("camera_anchor", CheckStatus::Pass)?;
        } else {
            observability.set_check(
                "camera_anchor",
                CheckStatus::Fail("configured anchor is absent".to_owned()),
            )?;
        }
        observability.increment("camera_reconcile_total", 1)?;

        let topology_changed = active != previous_active;
        for removed in previous_active.difference(&active) {
            running.remove(removed);
            virtual_manager.release(removed).await?;
        }
        if topology_changed {
            running.clear();
            manifest_revision = manifest_revision
                .checked_add(1)
                .context("manifest revision overflow")?;
            for id in &active {
                let epoch = stream_epochs.entry(id.clone()).or_default();
                *epoch = epoch.checked_add(1).context("stream epoch overflow")?;
            }
        }
        let mut planned = Vec::new();
        for item in evaluated {
            let id = item.camera.stable_camera_id.clone();
            let allocation = mapping
                .allocate_or_reuse(&item.camera, config.virtual_pool_size, config.virtual_start)?
                .clone();
            let virtual_spec = VirtualCameraSpec {
                stable_camera_id: id.clone(),
                device_number: allocation.virtual_device_number,
                timeout_ms: config.virtual_timeout_ms,
            };
            let virtual_device = virtual_manager.ensure(&virtual_spec).await?;
            planned.push((item, allocation, virtual_device));
        }
        let manifest_chunks = if planned.iter().any(|(item, _, _)| item.anchor) {
            let (manifest_bytes, chunks) = build_manifest_chunks(
                &config,
                session_id,
                edge_boot_id,
                manifest_revision,
                &planned,
                &stream_epochs,
                &adapter_runtime,
            )?;
            *manifest_store.write().await = manifest_bytes;
            Some(chunks)
        } else {
            manifest_store.write().await.clear();
            None
        };
        let mut exports = Vec::new();
        for (item, allocation, virtual_device) in planned {
            let id = item.camera.stable_camera_id.clone();
            if !running.contains_key(&id) {
                let plan = CameraPipelinePlan {
                    physical_device: item.camera.endpoint.device_path.clone(),
                    virtual_device: virtual_device.clone(),
                    width: config.width,
                    height: config.height,
                    fps: config.fps,
                    bitrate_kbps: config.bitrate_kbps,
                    keyint_frames: config.keyint_frames,
                };
                let handle = if item.stream_enabled {
                    let identity = StreamIdentity {
                        embodiment_id: config.embodiment_id.clone(),
                        edge_instance_id: config.edge_instance_id.clone(),
                        edge_boot_id,
                        session_id,
                        camera_id: id.clone(),
                        slot: allocation.stream_slot,
                        epoch: stream_epochs.get(&id).copied().unwrap_or(1),
                        role: if item.anchor {
                            Role::Anchor
                        } else {
                            Role::Secondary
                        },
                        codec: Codec::H264,
                    };
                    let output = SrtOutput {
                        target_host: config.target_host.clone(),
                        port: config
                            .base_port
                            .checked_add(allocation.stream_slot)
                            .context("SRT base+slot overflow")?,
                        stream_id: identity.encode_signed(&hmac_key)?,
                        latency_ms: config.latency_ms,
                        passphrase: passphrase.clone(),
                        pbkeylen: config.pbkeylen,
                    };
                    let metadata = if item.anchor {
                        let schedule = edge_core::context::ManifestSchedule::new(
                            manifest_chunks.clone().context("anchor manifest missing")?,
                            u64::from(config.fps).saturating_mul(config.manifest_repeat_sec),
                        )?;
                        Some(Arc::new(edge_core::context::AnchorMetadataProvider::new(
                            Arc::clone(&adapter_runtime.assembler),
                            *session_id.as_bytes(),
                            manifest_revision,
                            schedule,
                        ))
                            as Arc<dyn edge_core::SemanticMetadataProvider>)
                    } else {
                        None
                    };
                    Running::Stream(PipelineHandle::start(&plan, &output, metadata)?)
                } else {
                    Running::Ui(ui_runtime::UiOnlyHandle::start(&plan)?)
                };
                running.insert(id.clone(), handle);
            }
            exports.push(CameraExport {
                stable_camera_id: id,
                physical_device: item.camera.endpoint.device_path,
                virtual_device,
                stream_slot: allocation.stream_slot,
                stream_enabled: item.stream_enabled,
                anchor: item.anchor,
            });
        }
        if topology_changed {
            mapping.tombstone_missing(&active);
            mapping.save_atomic(&mapping_path)?;
            write_exports(&config.state_dir, &exports)?;
            previous_active = active;
        }

        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to wait for shutdown signal")?;
                break;
            }
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
    drop(running);
    Ok(())
}

#[cfg(target_os = "linux")]
struct AdapterRuntime {
    compiled: robot_multicam_adapter_client::embodiment::CompiledEmbodiment,
    descriptors: BTreeMap<String, robot_multicam_protocol::adapter::AdapterDescriptor>,
    assembler: std::sync::Arc<std::sync::Mutex<edge_core::context::ContextAssembler>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    command_routes: BTreeMap<String, PathBuf>,
}

#[cfg(target_os = "linux")]
async fn initialize_adapters(config: &EdgeConfig) -> Result<AdapterRuntime> {
    use robot_multicam_adapter_client::client::AdapterConnection;
    use robot_multicam_adapter_client::embodiment::EmbodimentConfig;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    gstreamer::init().context("GStreamer initialization failed before Adapter mapping")?;
    let embodiment = EmbodimentConfig::load(&config.embodiment_path)?;
    if embodiment.embodiment_id != config.embodiment_id {
        return Err(anyhow!("embodiment ID conflicts with EMBODIMENT_CONFIG"));
    }
    let deadline = Instant::now() + Duration::from_secs(config.adapter_connect_timeout_sec);
    let mut descriptors = BTreeMap::new();
    for adapter in &embodiment.adapters {
        let socket = adapter
            .endpoint
            .strip_prefix("unix://")
            .context("only absolute unix Adapter endpoints are supported")?;
        loop {
            match AdapterConnection::connect(socket, Duration::from_secs(2)).await {
                Ok(connection) => {
                    let descriptor = connection.descriptor().clone();
                    if descriptor.adapter_instance_id != adapter.adapter_instance_id {
                        return Err(anyhow!("Adapter descriptor instance ID mismatch"));
                    }
                    descriptors.insert(adapter.adapter_instance_id.clone(), descriptor);
                    break;
                }
                Err(error) if Instant::now() < deadline => {
                    tracing::warn!(
                        adapter = %adapter.adapter_instance_id,
                        %error,
                        "waiting for required Adapter"
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(error) if !adapter.required => {
                    tracing::warn!(adapter = %adapter.adapter_instance_id, %error, "optional Adapter unavailable");
                    break;
                }
                Err(error) => return Err(error).context("required Adapter unavailable"),
            }
        }
    }
    let compiled = embodiment.compile(&descriptors)?;
    let assembler = Arc::new(Mutex::new(edge_core::context::ContextAssembler::new(
        compiled.clone(),
        config.state_buffer_samples,
        config.state_max_gap_ns,
        config.context_budget_bytes,
    )?));
    let mut tasks = Vec::new();
    let mut command_routes = BTreeMap::new();
    for adapter in &embodiment.adapters {
        let Some(expected) = descriptors.get(&adapter.adapter_instance_id) else {
            continue;
        };
        let socket = adapter
            .endpoint
            .strip_prefix("unix://")
            .context("only unix Adapter endpoints are supported")?
            .to_owned();
        let device_ids: Vec<String> = embodiment
            .devices
            .iter()
            .filter(|device| device.adapter_instance_id == adapter.adapter_instance_id)
            .map(|device| device.device_id.clone())
            .collect();
        for device_id in &device_ids {
            command_routes.insert(device_id.clone(), PathBuf::from(&socket));
        }
        let expected_revision = expected.descriptor_revision;
        let target = Arc::clone(&assembler);
        tasks.push(tokio::spawn(async move {
            collect_adapter_samples(socket, device_ids, expected_revision, target).await;
        }));
    }
    Ok(AdapterRuntime {
        compiled,
        descriptors,
        assembler,
        tasks,
        command_routes,
    })
}

#[cfg(target_os = "linux")]
async fn collect_adapter_samples(
    socket: String,
    device_ids: Vec<String>,
    expected_revision: u64,
    assembler: std::sync::Arc<std::sync::Mutex<edge_core::context::ContextAssembler>>,
) {
    use gstreamer::prelude::*;
    use robot_multicam_adapter_client::client::AdapterConnection;
    use robot_multicam_timebase::clock_mapper::{ClockMapper, ClockMapperConfig};

    loop {
        let mut connection = match AdapterConnection::connect(&socket, Duration::from_secs(3)).await
        {
            Ok(value) if value.descriptor().descriptor_revision == expected_revision => value,
            Ok(_) => {
                tracing::error!(
                    socket,
                    "Adapter descriptor changed; a new session is required"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(error) => {
                tracing::warn!(socket, %error, "Adapter reconnect failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let source_clock_kind = connection
            .descriptor()
            .source_clock
            .as_ref()
            .map_or(0, |clock| clock.kind);
        let mut mapper = match ClockMapper::new(ClockMapperConfig {
            max_samples: 128,
            residual_reject_ns: 15_000_000,
            jump_reset_ns: 2_000_000_000,
        }) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(%error, "Clock Mapper configuration failed");
                return;
            }
        };
        let mut stream = match connection.stream_samples(device_ids.clone(), 100).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(socket, %error, "Adapter sample stream failed to start");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        loop {
            let sample = match connection.next_sample(&mut stream).await {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(socket, %error, "Adapter sample stream interrupted");
                    break;
                }
            };
            let edge_receive_ns = gstreamer::SystemClock::obtain()
                .time()
                .map(gstreamer::ClockTime::nseconds)
                .unwrap_or(0);
            if edge_receive_ns == 0 {
                tracing::error!("GStreamer system clock returned no timestamp");
                break;
            }
            let _ = mapper.observe(sample.source_time_ns, edge_receive_ns);
            let mapped_sample_time = if source_clock_kind
                == robot_multicam_protocol::adapter::SourceClockKind::EdgeMonotonic as i32
            {
                sample.source_time_ns
            } else {
                mapper.map(sample.source_time_ns).unwrap_or(edge_receive_ns)
            };
            let mut target = match assembler.lock() {
                Ok(value) => value,
                Err(_) => {
                    tracing::error!("context assembler lock poisoned");
                    return;
                }
            };
            for block in sample.feature_blocks {
                let source_time = if block.source_time_ns == 0 {
                    mapped_sample_time
                } else if source_clock_kind
                    == robot_multicam_protocol::adapter::SourceClockKind::EdgeMonotonic as i32
                {
                    block.source_time_ns
                } else {
                    mapper.map(block.source_time_ns).unwrap_or(edge_receive_ns)
                };
                if let Err(error) = target.push(
                    block.feature_id,
                    edge_core::context::FeatureSample {
                        time_edge_ns: source_time,
                        values: block.values,
                        valid: block.valid,
                    },
                ) {
                    tracing::warn!(feature_id = block.feature_id, %error, "Adapter feature sample rejected");
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(target_os = "linux")]
fn build_control_gateway(
    config: &EdgeConfig,
    adapter_runtime: &AdapterRuntime,
) -> Result<edge_core::control::ControlGateway> {
    use edge_core::control::{ControlGateway, DeviceCommandPolicy};
    use robot_multicam_adapter_client::embodiment::VectorKind;

    let mut policies = Vec::new();
    for descriptor in adapter_runtime.descriptors.values() {
        for device in &descriptor.devices {
            let vector_length = adapter_runtime
                .compiled
                .features
                .iter()
                .filter(|feature| {
                    feature.kind == VectorKind::Action && feature.device_id == device.device_id
                })
                .try_fold(0_usize, |total, feature| {
                    total.checked_add(usize::try_from(feature.length).ok()?)
                })
                .context("device action vector length overflow")?;
            if vector_length == 0 || device.command_modes.is_empty() {
                continue;
            }
            policies.push(DeviceCommandPolicy {
                device_id: device.device_id.clone(),
                action_schema_id: adapter_runtime.compiled.action_schema_id,
                vector_length,
                command_modes: device.command_modes.iter().cloned().collect(),
                minimum: vec![-3.2; vector_length],
                maximum: vec![3.2; vector_length],
            });
        }
    }
    let minimum_ttl_ns = config
        .control_min_ttl_ms
        .checked_mul(1_000_000)
        .context("minimum control lease TTL overflow")?;
    let maximum_ttl_ns = config
        .control_max_ttl_ms
        .checked_mul(1_000_000)
        .context("maximum control lease TTL overflow")?;
    Ok(ControlGateway::new(
        policies,
        minimum_ttl_ns,
        maximum_ttl_ns,
        config.command_history,
    )?)
}

#[cfg(target_os = "linux")]
fn build_manifest_chunks(
    config: &EdgeConfig,
    session_id: uuid::Uuid,
    edge_boot_id: uuid::Uuid,
    manifest_revision: u64,
    planned: &[(
        robot_multicam_camera_discovery::EvaluatedCamera,
        robot_multicam_camera_discovery::CameraMapping,
        PathBuf,
    )],
    stream_epochs: &BTreeMap<String, u32>,
    adapter_runtime: &AdapterRuntime,
) -> Result<(
    Vec<u8>,
    Vec<robot_multicam_protocol::multicam::SessionManifestChunkV1>,
)> {
    use gstreamer::prelude::*;
    use prost::Message;
    use robot_multicam_adapter_client::embodiment::VectorKind;
    use robot_multicam_metadata_codec::metadata_ext::{chunk_manifest, ManifestCompression};
    use robot_multicam_protocol::multicam::{
        CameraDescriptorV1, CameraRoleV1, DeviceDescriptorV1, FeatureDataType, FeatureSliceV1,
        FeatureVectorKind, SessionManifestV1, TimestampQuality, VideoCodecV1,
    };

    let devices = adapter_runtime
        .descriptors
        .values()
        .flat_map(|adapter| {
            adapter
                .devices
                .iter()
                .map(move |device| DeviceDescriptorV1 {
                    adapter_instance_id: adapter.adapter_instance_id.clone(),
                    device_id: device.device_id.clone(),
                    device_kind: device.kind,
                    role: device.role.clone(),
                    vendor: device.vendor.clone(),
                    model: device.model.clone(),
                    adapter_version: adapter.adapter_version.clone(),
                    sdk_version: adapter.vendor_sdk_version.clone(),
                    source_clock_id: adapter
                        .source_clock
                        .as_ref()
                        .map_or_else(String::new, |clock| clock.source_clock_id.clone()),
                    required: device.required,
                })
        })
        .collect();
    let features = adapter_runtime
        .compiled
        .features
        .iter()
        .map(|feature| FeatureSliceV1 {
            feature_id: feature.feature_id,
            qualified_name: feature.qualified_name.clone(),
            semantic: adapter_runtime
                .descriptors
                .values()
                .flat_map(|adapter| adapter.devices.iter())
                .flat_map(|device| device.features.iter())
                .find(|candidate| candidate.feature_id == feature.feature_id)
                .map_or_else(String::new, |candidate| candidate.semantic.clone()),
            source_device_id: feature.device_id.clone(),
            vector_kind: match feature.kind {
                VectorKind::Observation => FeatureVectorKind::Observation as i32,
                VectorKind::Action => FeatureVectorKind::Action as i32,
                VectorKind::Auxiliary => FeatureVectorKind::Auxiliary as i32,
            },
            data_type: FeatureDataType::Float32 as i32,
            unit: feature.unit.clone(),
            shape: feature.shape.clone(),
            offset: feature.offset,
            length: feature.length,
            interpolation: feature.interpolation,
            required: feature.required,
        })
        .collect();
    let cameras = planned
        .iter()
        .map(|(item, allocation, virtual_device)| {
            let digest = blake3::hash(item.camera.stable_camera_id.as_bytes());
            let camera_id_hash = u64::from_be_bytes(
                digest.as_bytes()[..8]
                    .try_into()
                    .expect("BLAKE3 digest has eight bytes"),
            );
            let port = config
                .base_port
                .checked_add(allocation.stream_slot)
                .context("camera transport port overflow")?;
            Ok(CameraDescriptorV1 {
                camera_id_hash,
                stable_camera_id: item.camera.stable_camera_id.clone(),
                product_name: item.camera.endpoint.product_name.clone(),
                serial: item.camera.endpoint.serial.clone().unwrap_or_default(),
                physical_device: item.camera.endpoint.device_path.display().to_string(),
                virtual_device: virtual_device.display().to_string(),
                stream_slot: u32::from(allocation.stream_slot),
                stream_epoch: stream_epochs
                    .get(&item.camera.stable_camera_id)
                    .copied()
                    .unwrap_or(1),
                width: config.width,
                height: config.height,
                fps_num: config.fps,
                fps_den: 1,
                pixel_format: "I420".to_owned(),
                stream_excluded: !item.stream_enabled,
                required_for_dataset: item.stream_enabled,
                role: if item.anchor {
                    CameraRoleV1::Anchor as i32
                } else {
                    CameraRoleV1::Secondary as i32
                },
                video_codec: VideoCodecV1::H264 as i32,
                transport: "srt-mpegts".to_owned(),
                transport_port: u32::from(port),
                stream_id_schema: "rmc1".to_owned(),
                timestamp_quality: TimestampQuality::GstreamerRunningTime as i32,
                timestamp_source: "GStreamer pipeline clock".to_owned(),
                timestamp_event: "capture-buffer running time".to_owned(),
                source_clock_id: "edge-monotonic".to_owned(),
                ..Default::default()
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let anchor_camera_id = planned
        .iter()
        .find(|(item, _, _)| item.anchor)
        .map(|(item, _, _)| item.camera.stable_camera_id.clone())
        .context("anchor camera disappeared while building manifest")?;
    let session_epoch_edge_ns = gstreamer::SystemClock::obtain()
        .time()
        .map(gstreamer::ClockTime::nseconds)
        .context("GStreamer system clock unavailable")?;
    let manifest = SessionManifestV1 {
        schema_version: 1,
        session_id: session_id.as_bytes().to_vec(),
        embodiment_id: config.embodiment_id.clone(),
        edge_core_version: env!("CARGO_PKG_VERSION").to_owned(),
        clock_domain_id: "edge-monotonic".to_owned(),
        session_epoch_edge_ns,
        manifest_revision,
        camera_catalog_revision: manifest_revision,
        anchor_camera_id,
        observation_schema_id: adapter_runtime.compiled.observation_schema_id,
        action_schema_id: adapter_runtime.compiled.action_schema_id,
        observation_vector_length: adapter_runtime.compiled.observation_length,
        action_vector_length: adapter_runtime.compiled.action_length,
        auxiliary_vector_length: adapter_runtime.compiled.auxiliary_length,
        edge_boot_id: edge_boot_id.as_bytes().to_vec(),
        edge_instance_id: config.edge_instance_id.clone(),
        stream_id_schema: "rmc1".to_owned(),
        schema_id_algorithm: robot_multicam_protocol::constants::SCHEMA_ID_HASH.to_owned(),
        devices,
        feature_slices: features,
        cameras,
    };
    let serialized = manifest.encode_to_vec();
    let chunks = chunk_manifest(
        &serialized,
        session_id.as_bytes(),
        manifest_revision,
        ManifestCompression::Zstd,
    )?;
    Ok((serialized, chunks))
}

#[cfg(target_os = "linux")]
fn evaluate_without_anchor(
    cameras: &[robot_multicam_camera_discovery::LogicalCamera],
    policy: &robot_multicam_camera_discovery::CameraPolicy,
) -> Vec<robot_multicam_camera_discovery::EvaluatedCamera> {
    cameras
        .iter()
        .filter(|camera| {
            !policy
                .disable
                .iter()
                .any(|selector| selector.matches(camera))
        })
        .map(|camera| robot_multicam_camera_discovery::EvaluatedCamera {
            camera: camera.clone(),
            stream_enabled: !policy
                .stream_exclude
                .iter()
                .any(|selector| selector.matches(camera)),
            anchor: false,
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_selector_list(value: &str) -> Result<Vec<robot_multicam_camera_discovery::Selector>> {
    value
        .split(';')
        .filter(|item| !item.is_empty())
        .map(robot_multicam_camera_discovery::Selector::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[cfg(target_os = "linux")]
fn read_secret(path: &Path, min: usize, max: usize) -> Result<String> {
    let bytes = read_secret_bytes(path, min, max)?;
    String::from_utf8(bytes).context("secret is not valid UTF-8")
}

#[cfg(target_os = "linux")]
fn read_secret_bytes(path: &Path, min: usize, max: usize) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("unable to read {}", path.display()))?;
    let trimmed = bytes
        .strip_suffix(b"\n")
        .and_then(|value| value.strip_suffix(b"\r").or(Some(value)))
        .unwrap_or(&bytes);
    if !(min..=max).contains(&trimmed.len()) {
        return Err(anyhow!("secret {} has an invalid size", path.display()));
    }
    Ok(trimmed.to_vec())
}

#[cfg(target_os = "linux")]
fn write_exports(state_dir: &Path, exports: &[edge_core::CameraExport]) -> Result<()> {
    let json = serde_json::to_vec_pretty(exports)?;
    write_atomic(&state_dir.join("cameras.json"), &json)?;
    let mut snippet = String::from("cameras:\n");
    for camera in exports {
        snippet.push_str(&format!(
            "  {}:\n    path: {}\n",
            camera.stable_camera_id,
            camera.virtual_device.display()
        ));
    }
    write_atomic(
        &state_dir.join("lerobot-camera-snippet.yaml"),
        snippet.as_bytes(),
    )
}

#[cfg(target_os = "linux")]
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut handle = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    handle.write_all(bytes)?;
    handle.write_all(b"\n")?;
    handle.sync_all()?;
    drop(handle);
    fs::rename(temp, path)?;
    Ok(())
}
