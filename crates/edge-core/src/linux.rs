//! Linux GStreamer runtime. The streaming callback only copies into a bounded latest queue.

use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use robot_multicam_metadata_codec::inject_timestamp_h264_annex_b;
use robot_multicam_metadata_codec::metadata_ext::inject_user_data_h264_annex_b;

use crate::{CameraPipelinePlan, EncodedAccessUnit, LatestQueue, SemanticMetadataProvider};

pub fn verify_gstreamer_runtime() -> Result<()> {
    gst::init().context("GStreamer initialization failed")?;
    for name in [
        "appsrc",
        "appsink",
        "avdec_h264",
        "h264parse",
        "mpegtsmux",
        "queue",
        "srtsink",
        "tee",
        "tsdemux",
        "v4l2src",
        "v4l2sink",
        "videotestsrc",
        "x264enc",
    ] {
        if gst::ElementFactory::find(name).is_none() {
            return Err(anyhow!("required GStreamer element is missing: {name}"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SrtOutput {
    pub target_host: String,
    pub port: u16,
    pub stream_id: String,
    pub latency_ms: u32,
    pub passphrase: String,
    pub pbkeylen: u16,
}

pub struct PipelineHandle {
    capture: gst::Pipeline,
    transport: gst::Pipeline,
    queue: Arc<LatestQueue<EncodedAccessUnit>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl PipelineHandle {
    pub fn start(
        plan: &CameraPipelinePlan,
        output: &SrtOutput,
        metadata: Option<Arc<dyn SemanticMetadataProvider>>,
    ) -> Result<Self> {
        gst::init().context("GStreamer initialization failed")?;
        validate_srt_output(output)?;
        let capture = gst::parse::launch(&plan.capture_description()?)
            .context("capture pipeline parse failed")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("capture description did not produce a pipeline"))?;
        let transport_description = format!(
            "appsrc name=sei_source ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au ! mpegtsmux alignment=7 ! srtsink uri=\"{}\" wait-for-connection=false",
            srt_uri(output)
        );
        let transport = gst::parse::launch(&transport_description)
            .context("transport pipeline parse failed")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("transport description did not produce a pipeline"))?;
        let appsrc = transport
            .by_name("sei_source")
            .context("transport appsrc missing")?
            .downcast::<gst_app::AppSrc>()
            .map_err(|_| anyhow!("sei_source is not an appsrc"))?;
        let caps = gst::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build();
        appsrc.set_caps(Some(&caps));
        appsrc.set_is_live(true);
        appsrc.set_format(gst::Format::Time);
        appsrc.set_block(false);
        appsrc.set_max_buffers(4);
        appsrc.set_leaky_type(gst_app::AppLeakyType::Downstream);

        let queue = Arc::new(LatestQueue::new(4)?);
        let callback_queue = Arc::clone(&queue);
        let callback_pipeline = capture.clone();
        let appsink = capture
            .by_name("encoded_au")
            .context("capture appsink missing")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| anyhow!("encoded_au is not an appsink"))?;
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let pts = buffer.pts().ok_or(gst::FlowError::Error)?;
                    let base = callback_pipeline.base_time().ok_or(gst::FlowError::Error)?;
                    let capture_time_edge_ns = base
                        .nseconds()
                        .checked_add(pts.nseconds())
                        .ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let item = EncodedAccessUnit {
                        bytes: map.as_slice().to_vec(),
                        pts_ns: pts.nseconds(),
                        dts_ns: buffer.dts().map(gst::ClockTime::nseconds),
                        duration_ns: buffer.duration().map(gst::ClockTime::nseconds),
                        capture_time_edge_ns,
                    };
                    callback_queue.push(item).map_err(|_| gst::FlowError::Eos)?;
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let worker_queue = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name("h264-sei-srt".to_owned())
            .spawn(move || {
                let mut ordinal = 0_u64;
                while let Ok(Some(au)) = worker_queue.pop_blocking() {
                    let mut enriched = match inject_timestamp_h264_annex_b(
                        &au.bytes,
                        au.capture_time_edge_ns,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::error!(%error, pts_ns = au.pts_ns, "dropping invalid encoded AU");
                            continue;
                        }
                    };
                    if let Some(provider) = metadata.as_ref() {
                        let additions = match provider
                            .for_access_unit(au.capture_time_edge_ns, ordinal)
                        {
                            Ok(value) => value,
                            Err(error) => {
                                tracing::error!(%error, ordinal, "dropping anchor AU without valid context");
                                ordinal = ordinal.saturating_add(1);
                                continue;
                            }
                        };
                        enriched = match inject_user_data_h264_annex_b(&enriched, &additions) {
                            Ok(value) => value,
                            Err(error) => {
                                tracing::error!(%error, ordinal, "dropping invalid anchor semantic metadata");
                                ordinal = ordinal.saturating_add(1);
                                continue;
                            }
                        };
                    }
                    let mut buffer = gst::Buffer::from_mut_slice(enriched);
                    let Some(buffer_ref) = buffer.get_mut() else {
                        tracing::error!("new GStreamer buffer was unexpectedly shared");
                        continue;
                    };
                    buffer_ref.set_pts(Some(gst::ClockTime::from_nseconds(au.pts_ns)));
                    buffer_ref.set_dts(au.dts_ns.map(gst::ClockTime::from_nseconds));
                    buffer_ref.set_duration(au.duration_ns.map(gst::ClockTime::from_nseconds));
                    if let Err(error) = appsrc.push_buffer(buffer) {
                        tracing::warn!(?error, "transport appsrc stopped accepting AUs");
                        break;
                    }
                    ordinal = ordinal.saturating_add(1);
                }
            })?;

        transport
            .set_state(gst::State::Playing)
            .context("transport pipeline failed to enter Playing")?;
        if let Err(error) = capture.set_state(gst::State::Playing) {
            let _ = transport.set_state(gst::State::Null);
            return Err(anyhow!(error).context("capture pipeline failed to enter Playing"));
        }
        Ok(Self {
            capture,
            transport,
            queue,
            worker: Some(worker),
        })
    }

    pub fn dropped_access_units(&self) -> Result<u64> {
        Ok(self.queue.dropped()?)
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        let _ = self.capture.set_state(gst::State::Null);
        let _ = self.transport.set_state(gst::State::Null);
        let _ = self.queue.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn validate_srt_output(output: &SrtOutput) -> Result<()> {
    if output.target_host.is_empty()
        || output
            .target_host
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || ".:-[]".contains(ch)))
    {
        return Err(anyhow!("invalid SRT target host"));
    }
    if output.port == 0 || !matches!(output.pbkeylen, 16 | 24 | 32) {
        return Err(anyhow!("invalid SRT port or pbkeylen"));
    }
    for value in [&output.stream_id, &output.passphrase] {
        if value.contains(['"', '\\', '\n', '\r', '\0', '&']) {
            return Err(anyhow!("SRT URI value contains an unsafe character"));
        }
    }
    Ok(())
}

fn srt_uri(output: &SrtOutput) -> String {
    format!(
        "srt://{}:{}?mode=caller&latency={}&pbkeylen={}&passphrase={}&streamid={}",
        output.target_host,
        output.port,
        output.latency_ms,
        output.pbkeylen,
        output.passphrase,
        output.stream_id
    )
}
