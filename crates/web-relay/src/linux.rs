use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::env;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::get;
use axum::{Json, Router};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use prost::Message;
use robot_multicam_common::{env_or, init_tracing, CheckStatus, Observability};
use robot_multicam_protocol::multicam::SessionManifestV1;
use robot_multicam_protocol::receiver::receiver_metadata_client::ReceiverMetadataClient;
use robot_multicam_protocol::receiver::{
    GetSessionManifestRequest, ListSessionsRequest, SubscribeSynchronizedStepsRequest,
    SynchronizedDatasetStep,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use tracing::{info, warn};
use uuid::Uuid;
use web_relay::{camera_key, project_metadata, select_session, FrameMetadataEvent, StreamInfo};

const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8091";

#[derive(Debug, Clone)]
struct Config {
    receiver_endpoint: String,
    session_override: Option<Uuid>,
    http_bind: SocketAddr,
    output_root: PathBuf,
    target_duration_sec: u32,
    playlist_length: u32,
    max_files: u32,
    event_buffer: usize,
    history_per_stream: usize,
    grpc_max_message_bytes: usize,
    discovery_interval: Duration,
    reconnect_min: Duration,
    reconnect_max: Duration,
    cors_origin: HeaderValue,
}

impl Config {
    fn from_env() -> Result<Self> {
        let receiver_endpoint = env::var("RECEIVER_GRPC_ENDPOINT")
            .unwrap_or_else(|_| "http://receiver:8083".to_owned());
        if !receiver_endpoint.starts_with("http://") {
            return Err(anyhow!(
                "RECEIVER_GRPC_ENDPOINT must use explicit plaintext http://"
            ));
        }
        let session_override = env::var("RELAY_SESSION_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| Uuid::parse_str(&value))
            .transpose()
            .context("RELAY_SESSION_ID must be a canonical UUID")?;
        let http_bind = env::var("RELAY_HTTP_BIND")
            .unwrap_or_else(|_| DEFAULT_HTTP_BIND.to_owned())
            .parse()
            .context("RELAY_HTTP_BIND must be a socket address")?;
        let output_root = env::var("RELAY_OUTPUT_ROOT")
            .map_or_else(|_| PathBuf::from("/var/cache/robot-relay"), PathBuf::from);
        if !output_root.is_absolute() || output_root == FsPath::new("/") {
            return Err(anyhow!(
                "RELAY_OUTPUT_ROOT must be an absolute non-root directory"
            ));
        }
        let target_duration_sec = env_or("RELAY_HLS_TARGET_DURATION_SEC", 1_u32)?;
        let playlist_length = env_or("RELAY_HLS_PLAYLIST_LENGTH", 6_u32)?;
        let max_files = env_or("RELAY_HLS_MAX_FILES", 8_u32)?;
        let event_buffer = env_or("RELAY_EVENT_BUFFER", 256_usize)?;
        let history_per_stream = env_or("RELAY_METADATA_HISTORY_PER_STREAM", 512_usize)?;
        let grpc_max_message_mib = env_or("RELAY_GRPC_MAX_MESSAGE_MIB", 64_usize)?;
        let discovery_interval =
            Duration::from_millis(env_or("RELAY_DISCOVERY_INTERVAL_MS", 1_000_u64)?);
        let reconnect_min = Duration::from_millis(env_or("RELAY_RECONNECT_MIN_MS", 500_u64)?);
        let reconnect_max = Duration::from_millis(env_or("RELAY_RECONNECT_MAX_MS", 10_000_u64)?);
        if !(1..=30).contains(&target_duration_sec)
            || !(3..=120).contains(&playlist_length)
            || !(playlist_length..=256).contains(&max_files)
            || !(16..=4_096).contains(&event_buffer)
            || !(32..=4_096).contains(&history_per_stream)
            || !(4..=256).contains(&grpc_max_message_mib)
            || discovery_interval < Duration::from_millis(50)
            || discovery_interval > Duration::from_secs(60)
            || reconnect_min.is_zero()
            || reconnect_min > reconnect_max
            || reconnect_max > Duration::from_secs(300)
        {
            return Err(anyhow!("invalid bounded Relay configuration"));
        }
        let grpc_max_message_bytes = grpc_max_message_mib
            .checked_mul(1024 * 1024)
            .context("RELAY_GRPC_MAX_MESSAGE_MIB overflow")?;
        let cors_origin = HeaderValue::from_str(
            &env::var("RELAY_CORS_ALLOW_ORIGIN").unwrap_or_else(|_| "*".to_owned()),
        )
        .context("RELAY_CORS_ALLOW_ORIGIN is not a valid HTTP header value")?;
        Ok(Self {
            receiver_endpoint,
            session_override,
            http_bind,
            output_root,
            target_duration_sec,
            playlist_length,
            max_files,
            event_buffer,
            history_per_stream,
            grpc_max_message_bytes,
            discovery_interval,
            reconnect_min,
            reconnect_max,
            cors_origin,
        })
    }
}

#[derive(Clone)]
struct HttpState {
    streams: Arc<RwLock<BTreeMap<String, StreamInfo>>>,
    histories: Arc<RwLock<BTreeMap<String, VecDeque<Arc<FrameMetadataEvent>>>>>,
    events: broadcast::Sender<Arc<FrameMetadataEvent>>,
    history_per_stream: usize,
    observability: Arc<Observability>,
    live_root: PathBuf,
    cors_origin: HeaderValue,
}

struct HlsPipeline {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    epoch: u32,
    last_ordinal: Option<u64>,
    frame_duration_ns: Option<u64>,
}

impl HlsPipeline {
    fn start(
        directory: &FsPath,
        epoch: u32,
        frame_duration_ns: Option<u64>,
        config: &Config,
    ) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        let playlist = directory.join("index.m3u8");
        let segments = directory.join("segment%05d.ts");
        let playlist = playlist.to_str().context("playlist path is not UTF-8")?;
        let segments = segments.to_str().context("segment path is not UTF-8")?;

        let pipeline = gst::Pipeline::new();
        let appsrc = gst::ElementFactory::make("appsrc")
            .build()?
            .downcast::<gst_app::AppSrc>()
            .map_err(|_| anyhow!("appsrc factory returned the wrong type"))?;
        let parser = gst::ElementFactory::make("h264parse").build()?;
        let muxer = gst::ElementFactory::make("mpegtsmux").build()?;
        let sink = gst::ElementFactory::make("hlssink").build()?;

        sink.set_property("location", segments);
        sink.set_property("playlist-location", playlist);
        sink.set_property("target-duration", config.target_duration_sec);
        sink.set_property("playlist-length", config.playlist_length);
        sink.set_property("max-files", config.max_files);
        parser.set_property("config-interval", -1_i32);

        let caps = gst::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build();
        appsrc.set_caps(Some(&caps));
        appsrc.set_is_live(true);
        appsrc.set_format(gst::Format::Time);
        appsrc.set_block(false);
        appsrc.set_max_buffers(8);
        appsrc.set_leaky_type(gst_app::AppLeakyType::Downstream);

        pipeline.add(&appsrc)?;
        pipeline.add(&parser)?;
        pipeline.add(&muxer)?;
        pipeline.add(&sink)?;
        appsrc.link(&parser)?;
        parser.link(&muxer)?;
        muxer.link(&sink)?;
        pipeline
            .set_state(gst::State::Playing)
            .context("HLS pipeline failed to enter Playing")?;
        Ok(Self {
            pipeline,
            appsrc,
            epoch,
            last_ordinal: None,
            frame_duration_ns,
        })
    }

    fn push(&mut self, frame: &robot_multicam_protocol::receiver::FrameReference) -> Result<bool> {
        let gap = self
            .last_ordinal
            .is_some_and(|last| frame.access_unit_ordinal != last.saturating_add(1));
        let mut buffer = gst::Buffer::from_slice(frame.encoded_image.clone());
        let buffer_ref = buffer
            .get_mut()
            .context("new HLS buffer was unexpectedly shared")?;
        let pts = gst::ClockTime::from_nseconds(frame.normalized_pts_ns);
        buffer_ref.set_pts(Some(pts));
        buffer_ref.set_dts(Some(pts));
        buffer_ref.set_duration(self.frame_duration_ns.map(gst::ClockTime::from_nseconds));
        self.appsrc
            .push_buffer(buffer)
            .map_err(|error| anyhow!("HLS appsrc rejected AU: {error:?}"))?;
        self.last_ordinal = Some(frame.access_unit_ordinal);
        Ok(gap)
    }
}

impl Drop for HlsPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct RelayManager {
    config: Config,
    http: HttpState,
    pipelines: HashMap<String, HlsPipeline>,
    active_session: Option<Uuid>,
}

impl RelayManager {
    fn new(config: Config, http: HttpState) -> Self {
        Self {
            config,
            http,
            pipelines: HashMap::new(),
            active_session: None,
        }
    }

    async fn reset_session(&mut self, session_id: Uuid) -> Result<()> {
        if self.active_session == Some(session_id) {
            return Ok(());
        }
        self.pipelines.clear();
        self.http.streams.write().await.clear();
        self.http.histories.write().await.clear();
        if self.http.live_root.exists() {
            tokio::fs::remove_dir_all(&self.http.live_root).await?;
        }
        tokio::fs::create_dir_all(self.http.live_root.join(session_id.to_string())).await?;
        self.active_session = Some(session_id);
        self.http
            .observability
            .increment("relay_session_switch_total", 1)?;
        Ok(())
    }

    async fn clear_active_session(&mut self) -> Result<()> {
        self.pipelines.clear();
        self.http.streams.write().await.clear();
        self.http.histories.write().await.clear();
        self.active_session = None;
        self.http.observability.set_check(
            "active_session",
            CheckStatus::Fail("no authoritative connected session".to_owned()),
        )?;
        self.http.observability.set_check(
            "hls_output",
            CheckStatus::Fail("no active HLS stream".to_owned()),
        )?;
        Ok(())
    }

    async fn process_step(
        &mut self,
        step: SynchronizedDatasetStep,
        manifest: &SessionManifestV1,
    ) -> Result<()> {
        let session_id = Uuid::from_slice(&step.session_id).context("step session UUID invalid")?;
        self.reset_session(session_id).await?;
        let durations = manifest
            .cameras
            .iter()
            .filter_map(|camera| {
                let numerator = u64::from(camera.fps_num);
                let denominator = u64::from(camera.fps_den);
                (numerator > 0 && denominator > 0).then(|| {
                    (
                        camera.stable_camera_id.clone(),
                        1_000_000_000_u64
                            .saturating_mul(denominator)
                            .checked_div(numerator),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        self.http
            .observability
            .increment("relay_steps_received_total", 1)?;

        for frame in &step.frames {
            if frame.encoded_image.is_empty() {
                continue;
            }
            let key = camera_key(&frame.camera_id);
            let needs_pipeline = self
                .pipelines
                .get(&key)
                .is_none_or(|pipeline| pipeline.epoch != frame.stream_epoch);
            if needs_pipeline {
                if self.pipelines.remove(&key).is_some() {
                    self.http
                        .observability
                        .increment("relay_pipeline_restart_total", 1)?;
                }
                let directory = self.http.live_root.join(session_id.to_string()).join(&key);
                let pipeline = HlsPipeline::start(
                    &directory,
                    frame.stream_epoch,
                    durations.get(&frame.camera_id).copied().flatten(),
                    &self.config,
                )?;
                self.pipelines.insert(key.clone(), pipeline);
            }
            let push_result = self
                .pipelines
                .get_mut(&key)
                .context("HLS pipeline disappeared")?
                .push(frame);
            match push_result {
                Ok(true) => self
                    .http
                    .observability
                    .increment("relay_access_unit_gap_total", 1)?,
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, camera_id = %frame.camera_id, "restarting failed HLS pipeline");
                    self.pipelines.remove(&key);
                    self.http
                        .observability
                        .increment("relay_pipeline_restart_total", 1)?;
                    continue;
                }
            }
            self.http
                .observability
                .increment("relay_access_units_received_total", 1)?;
            self.http.observability.increment(
                "relay_encoded_bytes_received_total",
                u64::try_from(frame.encoded_image.len()).unwrap_or(u64::MAX),
            )?;
            let playlist = self
                .http
                .live_root
                .join(session_id.to_string())
                .join(&key)
                .join("index.m3u8");
            self.http.streams.write().await.insert(
                format!("{session_id}/{key}"),
                StreamInfo {
                    session_id: session_id.to_string(),
                    camera_id: frame.camera_id.clone(),
                    camera_key: key.clone(),
                    stream_epoch: frame.stream_epoch,
                    playlist_url: format!("/live/{session_id}/{key}/index.m3u8"),
                    metadata_url: format!("/metadata/{session_id}/{key}"),
                    playlist_ready: playlist.is_file(),
                    last_capture_time_edge_ns: frame.capture_time_edge_ns,
                    last_media_pts_seconds: frame.normalized_pts_ns as f64 / 1_000_000_000.0,
                    last_access_unit_ordinal: frame.access_unit_ordinal,
                },
            );
        }

        for event in project_metadata(&step, manifest)? {
            let history_key = format!("{}/{}", event.session_id, event.camera_key);
            let event = Arc::new(event);
            {
                let mut histories = self.http.histories.write().await;
                let history = histories.entry(history_key).or_default();
                history.push_back(Arc::clone(&event));
                while history.len() > self.http.history_per_stream {
                    history.pop_front();
                }
            }
            let _ = self.http.events.send(event);
        }
        self.http
            .observability
            .set_check("active_session", CheckStatus::Pass)?;
        self.http
            .observability
            .set_check("hls_output", CheckStatus::Pass)?;
        Ok(())
    }
}

pub async fn run() -> Result<()> {
    init_tracing("robot-multicam-web-relay");
    let config = Config::from_env()?;
    verify_gstreamer_runtime()?;
    let live_root = config.output_root.join("live");
    tokio::fs::create_dir_all(&config.output_root).await?;
    let observability = Arc::new(Observability::new());
    observability.set_check("gstreamer_runtime", CheckStatus::Pass)?;
    observability.set_check(
        "receiver_grpc",
        CheckStatus::Fail("not connected".to_owned()),
    )?;
    observability.set_check(
        "active_session",
        CheckStatus::Fail("not discovered".to_owned()),
    )?;
    observability.set_check("hls_output", CheckStatus::Fail("not started".to_owned()))?;
    let (events, _) = broadcast::channel(config.event_buffer);
    let http = HttpState {
        streams: Arc::new(RwLock::new(BTreeMap::new())),
        histories: Arc::new(RwLock::new(BTreeMap::new())),
        events,
        history_per_stream: config.history_per_stream,
        observability,
        live_root,
        cors_origin: config.cors_origin.clone(),
    };
    let listener = tokio::net::TcpListener::bind(config.http_bind).await?;
    info!(bind = %config.http_bind, "Web Relay HTTP server listening");
    let app = router(http.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let manager = RelayManager::new(config.clone(), http);
    tokio::select! {
        result = relay_forever(config, manager) => result,
        result = server => result.context("Relay HTTP task join failed")?.context("Relay HTTP server failed"),
        signal = tokio::signal::ctrl_c() => signal.context("signal handler failed"),
    }
}

pub async fn healthcheck() -> Result<()> {
    let bind = env::var("RELAY_HTTP_BIND").unwrap_or_else(|_| DEFAULT_HTTP_BIND.to_owned());
    let address: SocketAddr = bind.parse().context("RELAY_HTTP_BIND invalid")?;
    let target = SocketAddr::new("127.0.0.1".parse()?, address.port());
    let mut stream = tokio::net::TcpStream::connect(target).await?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    if !response.starts_with(b"HTTP/1.1 200") {
        return Err(anyhow!("Relay liveness endpoint is not healthy"));
    }
    println!("healthy");
    Ok(())
}

async fn relay_forever(config: Config, mut manager: RelayManager) -> Result<()> {
    let mut delay = config.reconnect_min;
    loop {
        match ReceiverMetadataClient::connect(config.receiver_endpoint.clone()).await {
            Ok(client) => {
                let mut client = client
                    .max_decoding_message_size(config.grpc_max_message_bytes)
                    .max_encoding_message_size(1024 * 1024);
                manager
                    .http
                    .observability
                    .set_check("receiver_grpc", CheckStatus::Pass)?;
                delay = config.reconnect_min;
                if let Err(error) = connected_loop(&config, &mut manager, &mut client).await {
                    warn!(%error, "Receiver gRPC connection interrupted");
                    manager
                        .http
                        .observability
                        .increment("relay_grpc_reconnect_total", 1)?;
                }
            }
            Err(error) => warn!(%error, "Receiver gRPC connection failed"),
        }
        manager.http.observability.set_check(
            "receiver_grpc",
            CheckStatus::Fail("disconnected".to_owned()),
        )?;
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(config.reconnect_max);
    }
}

async fn connected_loop(
    config: &Config,
    manager: &mut RelayManager,
    client: &mut ReceiverMetadataClient<Channel>,
) -> Result<()> {
    loop {
        let session_id = loop {
            let sessions = client
                .list_sessions(ListSessionsRequest {})
                .await?
                .into_inner()
                .sessions;
            if let Some(session) = select_session(&sessions, config.session_override)? {
                break session;
            }
            manager.clear_active_session().await?;
            tokio::time::sleep(config.discovery_interval).await;
        };
        let manifest_reply = client
            .get_session_manifest(GetSessionManifestRequest {
                session_id: session_id.as_bytes().to_vec(),
                manifest_revision: 0,
            })
            .await?
            .into_inner();
        let manifest =
            SessionManifestV1::decode(manifest_reply.serialized_session_manifest.as_slice())?;
        manager.reset_session(session_id).await?;
        info!(%session_id, cameras = manifest.cameras.len(), "Relay subscribed to session");
        let mut stream = client
            .subscribe_synchronized_steps(SubscribeSynchronizedStepsRequest {
                session_id: session_id.as_bytes().to_vec(),
                include_encoded_images: true,
                camera_ids: Vec::new(),
            })
            .await?
            .into_inner();
        let mut discovery = tokio::time::interval(config.discovery_interval);
        discovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        discovery.tick().await;
        loop {
            tokio::select! {
                step = stream.message() => match step? {
                    Some(step) => manager.process_step(step, &manifest).await?,
                    None => return Err(anyhow!("Receiver synchronized-step stream closed")),
                },
                _ = discovery.tick() => {
                    let sessions = client
                        .list_sessions(ListSessionsRequest {})
                        .await?
                        .into_inner()
                        .sessions;
                    if select_session(&sessions, config.session_override)? != Some(session_id) {
                        info!(%session_id, "Relay session selection changed");
                        break;
                    }
                }
            }
        }
    }
}

fn verify_gstreamer_runtime() -> Result<()> {
    gst::init().context("GStreamer initialization failed")?;
    for name in ["appsrc", "h264parse", "mpegtsmux", "hlssink"] {
        if gst::ElementFactory::find(name).is_none() {
            return Err(anyhow!("required GStreamer element is missing: {name}"));
        }
    }
    Ok(())
}

fn router(state: HttpState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/api/v1/streams", get(streams))
        .route("/live/:session/:camera/:file", get(hls_file))
        .route("/metadata/:session/:camera", get(metadata_sse))
        .with_state(state)
}

async fn healthz(State(state): State<HttpState>) -> Response {
    health_response(&state, false)
}

async fn readyz(State(state): State<HttpState>) -> Response {
    health_response(&state, true)
}

fn health_response(state: &HttpState, readiness: bool) -> Response {
    match state.observability.snapshot() {
        Ok(snapshot) => {
            let status = if readiness && !snapshot.ready {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::OK
            };
            with_cors((status, Json(snapshot)).into_response(), state)
        }
        Err(error) => with_cors(
            (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
            state,
        ),
    }
}

async fn metrics(State(state): State<HttpState>) -> Response {
    match state.observability.prometheus() {
        Ok(body) => with_cors(
            ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response(),
            &state,
        ),
        Err(error) => with_cors(
            (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
            &state,
        ),
    }
}

async fn streams(State(state): State<HttpState>) -> Response {
    let value = state
        .streams
        .read()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    with_cors(Json(value).into_response(), &state)
}

async fn hls_file(
    State(state): State<HttpState>,
    Path((session, camera, file)): Path<(String, String, String)>,
) -> Response {
    if Uuid::parse_str(&session).is_err()
        || hex::decode(&camera).is_err()
        || camera.is_empty()
        || camera.len() > 512
        || !valid_hls_file(&file)
        || !state
            .streams
            .read()
            .await
            .contains_key(&format!("{session}/{camera}"))
    {
        return with_cors(StatusCode::NOT_FOUND.into_response(), &state);
    }
    let path = state.live_root.join(&session).join(&camera).join(&file);
    match tokio::fs::read(path).await {
        Ok(body) => {
            let content_type = if file.ends_with(".m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            let cache = if file.ends_with(".m3u8") {
                "no-store"
            } else {
                "public, max-age=30"
            };
            with_cors(
                (
                    [
                        (header::CONTENT_TYPE, content_type),
                        (header::CACHE_CONTROL, cache),
                    ],
                    body,
                )
                    .into_response(),
                &state,
            )
        }
        Err(_) => with_cors(StatusCode::NOT_FOUND.into_response(), &state),
    }
}

async fn metadata_sse(
    State(state): State<HttpState>,
    Path((session, camera)): Path<(String, String)>,
) -> Response {
    let stream_key = format!("{session}/{camera}");
    if Uuid::parse_str(&session).is_err() || !state.streams.read().await.contains_key(&stream_key) {
        return with_cors(StatusCode::NOT_FOUND.into_response(), &state);
    }
    let receiver = state.events.subscribe();
    let history = state
        .histories
        .read()
        .await
        .get(&stream_key)
        .map_or_else(Vec::new, |events| {
            events.iter().cloned().collect::<Vec<_>>()
        });
    let history_cursor = history
        .last()
        .map(|event| (event.stream_epoch, event.access_unit_ordinal));
    let initial = tokio_stream::iter(history.into_iter().filter_map(sse_frame));
    let observability = Arc::clone(&state.observability);
    let live = BroadcastStream::new(receiver).filter_map(move |item| match item {
        Ok(event)
            if event.session_id == session
                && event.camera_key == camera
                && history_cursor.is_none_or(|cursor| {
                    (event.stream_epoch, event.access_unit_ordinal) > cursor
                }) =>
        {
            sse_frame(event)
        }
        Ok(_) => None,
        Err(_) => {
            let _ = observability.increment("relay_sse_lag_total", 1);
            None
        }
    });
    let response = Sse::new(initial.chain(live))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("keepalive"),
        )
        .into_response();
    with_cors(response, &state)
}

fn sse_frame(event: Arc<FrameMetadataEvent>) -> Option<Result<Event, Infallible>> {
    Event::default()
        .event("frame")
        .id(format!(
            "{}:{}",
            event.stream_epoch, event.access_unit_ordinal
        ))
        .json_data(event.as_ref())
        .ok()
        .map(Ok)
}

fn valid_hls_file(file: &str) -> bool {
    file == "index.m3u8"
        || file
            .strip_prefix("segment")
            .and_then(|value| value.strip_suffix(".ts"))
            .is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            })
}

fn with_cors(mut response: Response, state: &HttpState) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        state.cors_origin.clone(),
    );
    response
}
