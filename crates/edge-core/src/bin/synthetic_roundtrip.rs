#[cfg(target_os = "linux")]
use anyhow::{anyhow, Context, Result};
#[cfg(target_os = "linux")]
use gstreamer as gst;
#[cfg(target_os = "linux")]
use gstreamer::prelude::*;
#[cfg(target_os = "linux")]
use gstreamer_app as gst_app;
#[cfg(target_os = "linux")]
use prost::Message;
#[cfg(target_os = "linux")]
use robot_multicam_metadata_codec::metadata_ext::{
    chunk_manifest, inject_user_data_h264_annex_b, manifest_chunk_sei, ManifestCompression,
};
#[cfg(target_os = "linux")]
use robot_multicam_metadata_codec::{
    decode_anchor_context_packet, encode_anchor_context_packet, inject_timestamp_h264_annex_b,
    inspect_h264_annex_b, UserDataUnregistered,
};
#[cfg(target_os = "linux")]
use robot_multicam_protocol::constants;
#[cfg(target_os = "linux")]
use robot_multicam_protocol::multicam::{
    AnchorFrameContextV1, CameraDescriptorV1, CameraRoleV1, FeatureDataType, FeatureSliceV1,
    FeatureVectorKind, InterpolationMethod, SessionManifestV1, VideoCodecV1,
};
#[cfg(target_os = "linux")]
use robot_multicam_stream_identity::{Codec, Role, StreamIdentity};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use uuid::Uuid;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("synthetic GStreamer round-trip requires Linux");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() -> Result<()> {
    gst::init().context("GStreamer initialization failed")?;
    let frame_count = std::env::var("SYNTHETIC_FRAME_COUNT")
        .unwrap_or_else(|_| "24".to_owned())
        .parse::<u64>()
        .context("SYNTHETIC_FRAME_COUNT must be an integer")?;
    if !(12..=300).contains(&frame_count) {
        return Err(anyhow!("SYNTHETIC_FRAME_COUNT must be between 12 and 300"));
    }
    let archive_root = std::env::var_os("SYNTHETIC_ARCHIVE_ROOT").map(std::path::PathBuf::from);
    let explicit_output = std::env::var_os("SYNTHETIC_ROUNDTRIP_OUTPUT");
    let path = if let Some(root) = archive_root.as_ref() {
        root.join("segments/synthetic.ts")
    } else {
        explicit_output.clone().map_or_else(
            || {
                std::env::temp_dir().join(format!(
                    "robot-multicam-roundtrip-{}.ts",
                    std::process::id()
                ))
            },
            std::path::PathBuf::from,
        )
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = run(&path, frame_count);
    if let (Ok(report), Some(root)) = (&result, archive_root.as_ref()) {
        write_archive_fixture(root, &path, report)?;
    }
    if result.is_ok() {
        if archive_root.is_none() && explicit_output.is_none() && path.exists() {
            std::fs::remove_file(&path).context("failed to remove exact round-trip fixture")?;
        } else if path.exists() {
            eprintln!("successful round-trip TS preserved at {}", path.display());
        }
    } else if path.exists() {
        eprintln!("failed round-trip TS retained at {}", path.display());
    }
    result.map(|_| ())
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct RoundTripReport {
    first_capture_time_edge_ns: u64,
    last_capture_time_edge_ns: u64,
    first_normalized_pts_ns: u64,
    last_normalized_pts_ns: u64,
}

#[cfg(target_os = "linux")]
fn run(path: &std::path::Path, frame_count: u64) -> Result<RoundTripReport> {
    let capture = gst::parse::launch(&format!(
        "videotestsrc num-buffers={frame_count} is-live=false pattern=ball ! \
         video/x-raw,width=320,height=240,framerate=30/1 ! videoconvert ! \
         x264enc tune=zerolatency speed-preset=ultrafast key-int-max=12 bframes=0 aud=true ! \
         h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au ! \
         appsink name=encoded max-buffers=4 drop=false sync=false"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("capture pipeline type mismatch"))?;
    let output = path
        .to_str()
        .filter(|value| !value.contains(['"', '\n', '\r', '\0']))
        .context("unsafe temporary output path")?;
    let mux = gst::parse::launch(&format!(
        "appsrc name=source format=time block=true ! \
         h264parse config-interval=-1 ! \
         video/x-h264,stream-format=byte-stream,alignment=au ! \
         mpegtsmux alignment=7 ! filesink location=\"{output}\" sync=false"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("mux pipeline type mismatch"))?;
    let source = mux
        .by_name("source")
        .context("appsrc missing")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| anyhow!("source element type mismatch"))?;
    source.set_caps(Some(
        &gst::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build(),
    ));
    let sink = capture
        .by_name("encoded")
        .context("appsink missing")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow!("encoded element type mismatch"))?;
    mux.set_state(gst::State::Playing)?;
    capture.set_state(gst::State::Playing)?;

    let session_id = [7_u8; 16];
    let manifest = SessionManifestV1 {
        schema_version: 1,
        session_id: session_id.to_vec(),
        edge_boot_id: vec![8; 16],
        manifest_revision: 1,
        anchor_camera_id: "synthetic-anchor".to_owned(),
        stream_id_schema: "rmc1".to_owned(),
        schema_id_algorithm: constants::SCHEMA_ID_HASH.to_owned(),
        observation_schema_id: 11,
        action_schema_id: 12,
        observation_vector_length: 1,
        action_vector_length: 1,
        feature_slices: vec![
            FeatureSliceV1 {
                feature_id: 1,
                qualified_name: "synthetic.observation".to_owned(),
                semantic: "synthetic_observation".to_owned(),
                source_device_id: "synthetic".to_owned(),
                vector_kind: FeatureVectorKind::Observation as i32,
                data_type: FeatureDataType::Float32 as i32,
                shape: vec![1],
                length: 1,
                interpolation: InterpolationMethod::Linear as i32,
                required: true,
                ..Default::default()
            },
            FeatureSliceV1 {
                feature_id: 2,
                qualified_name: "synthetic.action".to_owned(),
                semantic: "synthetic_action".to_owned(),
                source_device_id: "synthetic".to_owned(),
                vector_kind: FeatureVectorKind::Action as i32,
                data_type: FeatureDataType::Float32 as i32,
                shape: vec![1],
                length: 1,
                interpolation: InterpolationMethod::ZeroOrderHold as i32,
                required: true,
                ..Default::default()
            },
        ],
        cameras: vec![CameraDescriptorV1 {
            stable_camera_id: "synthetic-anchor".to_owned(),
            stream_slot: 0,
            stream_epoch: 1,
            width: 320,
            height: 240,
            fps_num: 30,
            fps_den: 1,
            required_for_dataset: true,
            role: CameraRoleV1::Anchor as i32,
            video_codec: VideoCodecV1::H264 as i32,
            transport: "srt-mpegts".to_owned(),
            transport_port: 10_000,
            stream_id_schema: "rmc1".to_owned(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let manifest_chunk = chunk_manifest(
        &manifest.encode_to_vec(),
        &session_id,
        1,
        ManifestCompression::None,
    )?
    .into_iter()
    .next()
    .context("synthetic manifest produced no chunk")?;
    let mut encoded_count = 0_u64;
    for frame_index in 0..frame_count {
        let sample = sink
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .with_context(|| format!("capture timed out at AU {frame_index}"))?;
        let input = sample.buffer().context("encoded buffer missing")?;
        let pts = input.pts().context("encoded PTS missing")?;
        let map = input.map_readable()?;
        let capture_time = 1_000_000_000_u64
            .checked_add(pts.nseconds())
            .context("synthetic capture timestamp overflow")?;
        let timestamp_only = inject_timestamp_h264_annex_b(map.as_slice(), capture_time)?;
        let secondary = inspect_h264_annex_b(&timestamp_only, true)?
            .context("secondary timestamp-only conformance failed")?;
        if secondary.timestamp.capture_time_edge_ns != capture_time {
            return Err(anyhow!("secondary timestamp changed before mux"));
        }
        let context = AnchorFrameContextV1 {
            schema_version: 1,
            session_id: session_id.to_vec(),
            anchor_frame_seq: frame_index,
            manifest_revision: 1,
            observation_schema_id: 11,
            action_schema_id: 12,
            observation_state: vec![frame_index as f32],
            action: vec![(frame_index as f32) * 0.5],
            ..Default::default()
        };
        let mut additions = vec![UserDataUnregistered {
            uuid: Uuid::parse_str(constants::ANCHOR_CONTEXT_UUID)
                .context("anchor context UUID invalid")?,
            payload: encode_anchor_context_packet(&context)?,
        }];
        if frame_index == 0 {
            additions.push(manifest_chunk_sei(&manifest_chunk));
        }
        let enriched = inject_user_data_h264_annex_b(&timestamp_only, &additions)?;
        let mut buffer = gst::Buffer::from_mut_slice(enriched);
        let writable = buffer.get_mut().context("new buffer unexpectedly shared")?;
        writable.set_pts(Some(pts));
        writable.set_dts(input.dts());
        writable.set_duration(input.duration());
        source.push_buffer(buffer)?;
        encoded_count = encoded_count.saturating_add(1);
    }
    source.end_of_stream()?;
    wait_for_eos(&mux, "mux")?;
    capture.set_state(gst::State::Null)?;
    mux.set_state(gst::State::Null)?;
    if encoded_count != frame_count {
        return Err(anyhow!(
            "expected {frame_count} encoded AUs, got {encoded_count}"
        ));
    }
    let transport_bytes = std::fs::metadata(path)?.len();
    if transport_bytes == 0 {
        return Err(anyhow!("mux produced an empty transport stream"));
    }
    eprintln!("mux produced {transport_bytes} bytes");

    let receive = gst::parse::launch(&format!(
        "filesrc location=\"{output}\" ! tsdemux name=demux \
         demux. ! queue max-size-buffers=64 max-size-bytes=0 max-size-time=0 ! h264parse ! \
         video/x-h264,stream-format=byte-stream,alignment=au ! tee name=split \
         split. ! queue max-size-buffers=64 max-size-bytes=0 max-size-time=0 ! appsink name=predecode max-buffers=64 drop=false sync=false \
         split. ! queue max-size-buffers=64 max-size-bytes=0 max-size-time=0 ! avdec_h264 ! fakesink sync=false"
    ))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("receive pipeline type mismatch"))?;
    let predecode = receive
        .by_name("predecode")
        .context("predecode appsink missing")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow!("predecode element type mismatch"))?;
    receive.set_state(gst::State::Playing)?;
    let mut decoded_count = 0_u64;
    let mut prior_timestamp = 0_u64;
    let mut first_timestamp = None;
    let mut first_pts = None;
    let mut last_pts = None;
    let context_uuid = Uuid::parse_str(constants::ANCHOR_CONTEXT_UUID)?;
    let manifest_uuid = Uuid::parse_str(constants::SESSION_MANIFEST_UUID)?;
    let mut manifest_count = 0_u64;
    for frame_index in 0..encoded_count {
        let sample = predecode
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .with_context(|| format!("predecode receive timed out at AU {frame_index}"))?;
        let buffer = sample.buffer().context("demuxed buffer missing")?;
        let pts = buffer
            .pts()
            .map(gst::ClockTime::nseconds)
            .context("demuxed normalized PTS missing")?;
        let map = buffer.map_readable()?;
        let inspected = inspect_h264_annex_b(map.as_slice(), false)?
            .context("timestamp missing after demux")?;
        if inspected.timestamp.capture_time_edge_ns <= prior_timestamp {
            return Err(anyhow!("timestamp order/exactly-one invariant failed"));
        }
        if last_pts.is_some_and(|prior| pts <= prior) {
            return Err(anyhow!("normalized PTS is not strictly monotonic"));
        }
        let contexts = inspected
            .messages
            .iter()
            .filter(|message| message.uuid == context_uuid)
            .collect::<Vec<_>>();
        if contexts.len() != 1 {
            return Err(anyhow!("anchor AU must contain exactly one context packet"));
        }
        let (_, context) = decode_anchor_context_packet(&contexts[0].payload)?;
        if context.anchor_frame_seq != frame_index || context.session_id != session_id {
            return Err(anyhow!("anchor context changed across transport"));
        }
        let manifests = inspected
            .messages
            .iter()
            .filter(|message| message.uuid == manifest_uuid)
            .count();
        if manifests > 1
            || (frame_index == 0 && manifests != 1)
            || (frame_index > 0 && manifests != 0)
        {
            return Err(anyhow!("manifest schedule changed across transport"));
        }
        manifest_count = manifest_count.saturating_add(u64::try_from(manifests)?);
        first_timestamp.get_or_insert(inspected.timestamp.capture_time_edge_ns);
        first_pts.get_or_insert(pts);
        prior_timestamp = inspected.timestamp.capture_time_edge_ns;
        last_pts = Some(pts);
        decoded_count = decoded_count.saturating_add(1);
    }
    wait_for_eos(&receive, "receive/decode")?;
    receive.set_state(gst::State::Null)?;
    if decoded_count != encoded_count || manifest_count != 1 {
        return Err(anyhow!(
            "AU count changed across mux/demux: encoded={encoded_count} received={decoded_count}"
        ));
    }
    println!(
        "synthetic round-trip PASS: {decoded_count} AUs, secondary timestamp-only, anchor CRC context, manifest, decoder branch"
    );
    Ok(RoundTripReport {
        first_capture_time_edge_ns: first_timestamp.context("first capture timestamp missing")?,
        last_capture_time_edge_ns: prior_timestamp,
        first_normalized_pts_ns: first_pts.context("first normalized PTS missing")?,
        last_normalized_pts_ns: last_pts.context("last normalized PTS missing")?,
    })
}

#[cfg(target_os = "linux")]
fn write_archive_fixture(
    root: &std::path::Path,
    transport_path: &std::path::Path,
    report: &RoundTripReport,
) -> Result<()> {
    let bytes = std::fs::read(transport_path)?;
    let key = [9_u8; 32];
    let session_id = Uuid::from_bytes([7; 16]);
    let identity = StreamIdentity {
        embodiment_id: "synthetic-cell".to_owned(),
        edge_instance_id: "synthetic-edge".to_owned(),
        edge_boot_id: Uuid::from_bytes([8; 16]),
        session_id,
        camera_id: "synthetic-anchor".to_owned(),
        slot: 0,
        epoch: 1,
        role: Role::Anchor,
        codec: Codec::H264,
    };
    let envelope = serde_json::json!({
        "accepted_at_utc": "2026-07-22T00:00:00Z",
        "listen_port": 10_000,
        "raw_stream_id": identity.encode_signed(&key)?,
        "stream_id_fields": {
            "embodiment_id": identity.embodiment_id,
            "edge_instance_id": identity.edge_instance_id,
            "edge_boot_id": identity.edge_boot_id.to_string(),
            "session_id": identity.session_id.to_string(),
            "camera_id": identity.camera_id,
            "slot": identity.slot,
            "epoch": identity.epoch,
            "role": "anchor",
            "codec": "h264"
        },
        "stream_id_auth": "valid",
        "peer_address": "127.0.0.1:40000",
        "gstreamer_version": gst::version_string().to_string()
    });
    let index = serde_json::json!({
        "connection_id": session_id.to_string(),
        "camera_id": "synthetic-anchor",
        "stream_epoch": 1,
        "first_normalized_pts_ns": report.first_normalized_pts_ns,
        "last_normalized_pts_ns": report.last_normalized_pts_ns,
        "first_capture_time_edge_ns": report.first_capture_time_edge_ns,
        "last_capture_time_edge_ns": report.last_capture_time_edge_ns,
        "relative_path": "segments/synthetic.ts",
        "sha256": hex::encode(Sha256::digest(&bytes)),
        "bytes": bytes.len()
    });
    write_atomic(
        &root.join("stream-envelope.json"),
        &serde_json::to_vec_pretty(&envelope)?,
    )?;
    let mut index_line = serde_json::to_vec(&index)?;
    index_line.push(b'\n');
    write_atomic(&root.join("segments/index.jsonl"), &index_line)?;
    write_atomic(&root.join("hmac-key.bin"), &key)?;
    println!("synthetic raw archive fixture: {}", root.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("archive fixture path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn wait_for_eos(pipeline: &gst::Pipeline, stage: &str) -> Result<()> {
    let bus = pipeline.bus().context("pipeline bus missing")?;
    let message = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(15),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .with_context(|| format!("{stage} pipeline timed out"))?;
    if let gst::MessageView::Error(error) = message.view() {
        return Err(anyhow!(
            "{stage} pipeline error from {:?}: {} ({:?})",
            error.src().map(|value| value.path_string()),
            error.error(),
            error.debug()
        ));
    }
    Ok(())
}
