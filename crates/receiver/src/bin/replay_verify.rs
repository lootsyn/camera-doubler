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
use receiver::replay::{load_verified_archive, SegmentIndexEntry};
#[cfg(target_os = "linux")]
use receiver::synchronize::{validate_manifest, ConnectedCamera, StepSynchronizer, StoredFrame};
#[cfg(target_os = "linux")]
use robot_multicam_metadata_codec::inspect_h264_annex_b;
#[cfg(target_os = "linux")]
use robot_multicam_metadata_codec::metadata_ext::ManifestReassembler;
#[cfg(target_os = "linux")]
use robot_multicam_protocol::constants;
#[cfg(target_os = "linux")]
use robot_multicam_protocol::multicam::SessionManifestChunkV1;
#[cfg(target_os = "linux")]
use robot_multicam_stream_identity::{Codec, Role};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use uuid::Uuid;

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
struct ReplayedAccessUnit {
    capture_time_edge_ns: u64,
    normalized_pts_ns: u64,
    access_unit_ordinal: u64,
    context_packet: Option<Vec<u8>>,
    manifest_chunks: Vec<SessionManifestChunkV1>,
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("raw MPEG-TS replay verification requires Linux");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() -> Result<()> {
    gst::init().context("GStreamer initialization failed")?;
    let mut args = std::env::args_os().skip(1);
    let root = args
        .next()
        .context("usage: robot-replay-verify SESSION_ROOT ENVELOPE INDEX HMAC_KEY [BASE_PORT]")?;
    let envelope = args
        .next()
        .context("stream-envelope.json path is required")?;
    let index = args
        .next()
        .context("segments/index.jsonl path is required")?;
    let key_path = args.next().context("HMAC key file is required")?;
    let base_port = args
        .next()
        .map(|value| value.to_string_lossy().parse::<u16>())
        .transpose()
        .context("BASE_PORT is invalid")?
        .unwrap_or(10_000);
    if args.next().is_some() {
        return Err(anyhow!("unexpected replay verifier argument"));
    }
    let root = Path::new(&root);
    let key = std::fs::read(key_path).context("unable to read HMAC key")?;
    let key = key.strip_suffix(b"\n").unwrap_or(&key);
    if key.len() < 32 {
        return Err(anyhow!("HMAC key must contain at least 32 bytes"));
    }
    let (identity, entries) = load_verified_archive(
        root,
        Path::new(&envelope),
        Path::new(&index),
        key,
        base_port,
        1_000_000,
    )?;
    if identity.codec != Codec::H264 {
        return Err(anyhow!("only enabled H.264 archives can be replayed"));
    }
    let mut digest = Sha256::new();
    let mut access_units = 0_u64;
    let mut replayed = Vec::new();
    for entry in &entries {
        access_units = access_units
            .checked_add(replay_segment(
                &root.join(&entry.relative_path),
                entry,
                identity.role,
                &mut digest,
                &mut replayed,
            )?)
            .context("replay AU count overflow")?;
    }
    let (steps, step_hash) = if identity.role == Role::Anchor {
        let first = reconstruct_steps(&identity, &replayed, base_port)?;
        let second = reconstruct_steps(&identity, &replayed, base_port)?;
        if first != second {
            return Err(anyhow!("synchronized replay is not byte deterministic"));
        }
        let mut step_digest = Sha256::new();
        for step in &first {
            step_digest.update(step);
        }
        (first.len(), hex::encode(step_digest.finalize()))
    } else {
        (0, "not-applicable-secondary".to_owned())
    };
    println!(
        "raw replay PASS: camera={} epoch={} segments={} access_units={} synchronized_steps={} metadata_sha256={} step_sha256={} deterministic=bit-for-bit",
        identity.camera_id,
        identity.epoch,
        entries.len(),
        access_units,
        steps,
        hex::encode(digest.finalize()),
        step_hash,
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn reconstruct_steps(
    identity: &robot_multicam_stream_identity::StreamIdentity,
    access_units: &[ReplayedAccessUnit],
    base_port: u16,
) -> Result<Vec<Vec<u8>>> {
    let mut reassembler: Option<ManifestReassembler> = None;
    let mut manifest_bytes: Option<Vec<u8>> = None;
    for access_unit in access_units {
        for chunk in &access_unit.manifest_chunks {
            let now = Duration::from_nanos(access_unit.access_unit_ordinal);
            let completed = if let Some(value) = reassembler.as_mut() {
                value.insert(chunk.clone(), now)
            } else {
                let mut value =
                    ManifestReassembler::new(chunk.clone(), now, Duration::from_secs(10))?;
                let completed = value.insert(chunk.clone(), now);
                reassembler = Some(value);
                completed
            }?;
            if completed.is_some() {
                manifest_bytes = completed;
                break;
            }
        }
        if manifest_bytes.is_some() {
            break;
        }
    }
    let manifest_bytes = manifest_bytes.context("replayed manifest did not reassemble")?;
    let listen_port = identity.expected_port(base_port)?;
    let manifest = validate_manifest(
        &manifest_bytes,
        &[ConnectedCamera {
            camera_id: identity.camera_id.clone(),
            stream_slot: u32::from(identity.slot),
            stream_epoch: identity.epoch,
            listen_port,
        }],
        base_port,
    )?;
    let mut synchronizer = StepSynchronizer::new(manifest, 64, 1_000_000)?;
    let mut encoded_steps = Vec::with_capacity(access_units.len());
    for access_unit in access_units {
        let context = access_unit
            .context_packet
            .as_deref()
            .context("anchor replay AU lacks context packet")?;
        let step = synchronizer.anchor_step(
            StoredFrame {
                camera_id: identity.camera_id.clone(),
                capture_time_edge_ns: access_unit.capture_time_edge_ns,
                stream_epoch: identity.epoch,
                normalized_pts_ns: access_unit.normalized_pts_ns,
                access_unit_ordinal: access_unit.access_unit_ordinal,
                storage_uri: String::new(),
                encoded_image: Vec::new(),
            },
            context,
            false,
        )?;
        encoded_steps.push(step.encode_to_vec());
    }
    Ok(encoded_steps)
}

#[cfg(target_os = "linux")]
fn replay_segment(
    path: &Path,
    entry: &SegmentIndexEntry,
    role: Role,
    digest: &mut Sha256,
    replayed: &mut Vec<ReplayedAccessUnit>,
) -> Result<u64> {
    let pipeline = gst::parse::launch(
        "filesrc name=source ! tsdemux name=demux \
         demux. ! queue max-size-buffers=64 max-size-bytes=0 max-size-time=0 ! \
         h264parse ! video/x-h264,stream-format=byte-stream,alignment=au ! tee name=encoded \
         encoded. ! queue max-size-buffers=64 max-size-bytes=0 max-size-time=0 ! \
         appsink name=predecode max-buffers=64 drop=false sync=false \
         encoded. ! queue max-size-buffers=64 max-size-bytes=0 max-size-time=0 ! \
         avdec_h264 ! fakesink sync=false",
    )?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("replay pipeline type mismatch"))?;
    pipeline
        .by_name("source")
        .context("filesrc missing")?
        .set_property("location", path);
    let sink = pipeline
        .by_name("predecode")
        .context("predecode appsink missing")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow!("predecode element type mismatch"))?;
    let bus = pipeline.bus().context("replay bus missing")?;
    pipeline.set_state(gst::State::Playing)?;
    let mut count = 0_u64;
    let mut first_capture = None;
    let mut last_capture = None;
    let mut first_pts = None;
    let mut last_pts = None;
    let mut idle_polls = 0_u8;
    loop {
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(1)) {
            idle_polls = 0;
            let buffer = sample.buffer().context("replayed AU buffer missing")?;
            let pts = buffer
                .pts()
                .map(gst::ClockTime::nseconds)
                .context("replayed AU PTS missing")?;
            let map = buffer.map_readable()?;
            let inspected = inspect_h264_annex_b(map.as_slice(), role == Role::Secondary)?
                .context("replayed AU timestamp SEI missing")?;
            let capture = inspected.timestamp.capture_time_edge_ns;
            if last_capture.is_some_and(|prior| capture <= prior)
                || last_pts.is_some_and(|prior| pts <= prior)
            {
                return Err(anyhow!("replayed timestamp/PTS is not strictly monotonic"));
            }
            first_capture.get_or_insert(capture);
            first_pts.get_or_insert(pts);
            last_capture = Some(capture);
            last_pts = Some(pts);
            digest.update(entry.camera_id.as_bytes());
            digest.update(capture.to_be_bytes());
            digest.update(pts.to_be_bytes());
            for message in &inspected.messages {
                digest.update(message.uuid.as_bytes());
                digest.update(
                    u64::try_from(message.payload.len())
                        .context("metadata payload length overflow")?
                        .to_be_bytes(),
                );
                digest.update(&message.payload);
            }
            let context_uuid = Uuid::parse_str(constants::ANCHOR_CONTEXT_UUID)?;
            let manifest_uuid = Uuid::parse_str(constants::SESSION_MANIFEST_UUID)?;
            let context_packet = inspected
                .messages
                .iter()
                .find(|message| message.uuid == context_uuid)
                .map(|message| message.payload.clone());
            let manifest_chunks = inspected
                .messages
                .iter()
                .filter(|message| message.uuid == manifest_uuid)
                .map(|message| {
                    SessionManifestChunkV1::decode(message.payload.as_slice())
                        .context("replayed manifest chunk is invalid")
                })
                .collect::<Result<Vec<_>>>()?;
            replayed.push(ReplayedAccessUnit {
                capture_time_edge_ns: capture,
                normalized_pts_ns: pts,
                access_unit_ordinal: u64::try_from(replayed.len())
                    .context("replayed AU ordinal overflow")?,
                context_packet,
                manifest_chunks,
            });
            count = count.checked_add(1).context("replay AU count overflow")?;
            continue;
        }
        if let Some(message) = bus.timed_pop_filtered(
            gst::ClockTime::ZERO,
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            if let gst::MessageView::Error(error) = message.view() {
                return Err(anyhow!(
                    "replay/decode failed at {:?}: {} ({:?})",
                    error.src().map(|value| value.path_string()),
                    error.error(),
                    error.debug()
                ));
            }
            break;
        }
        if sink.is_eos() {
            break;
        }
        idle_polls = idle_polls.saturating_add(1);
        if idle_polls >= 10 {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(anyhow!("replay pipeline timed out"));
        }
    }
    pipeline.set_state(gst::State::Null)?;
    if count == 0
        || first_capture != Some(entry.first_capture_time_edge_ns)
        || last_capture != Some(entry.last_capture_time_edge_ns)
        || first_pts != Some(entry.first_normalized_pts_ns)
        || last_pts != Some(entry.last_normalized_pts_ns)
    {
        return Err(anyhow!(
            "segment index time range does not match replayed access units"
        ));
    }
    Ok(count)
}
