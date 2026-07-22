//! GStreamer SRT listener with authentication before media and SEI extraction before decode.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::glib::value::ToValue;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use receiver::runtime::ReceiverRuntime;
use receiver::{BootstrapState, ReceiverError, ReceiverRegistry, StreamKey};

pub struct ListenerHandle {
    pipeline: gst::Pipeline,
}

impl ListenerHandle {
    pub fn start(
        listen_port: u16,
        latency_ms: u32,
        passphrase: &str,
        pbkeylen: u16,
        registry: Arc<ReceiverRegistry>,
        runtime: Arc<ReceiverRuntime>,
    ) -> Result<Self> {
        gst::init().context("GStreamer initialization failed")?;
        if !matches!(pbkeylen, 16 | 24 | 32)
            || passphrase.contains(['"', '\\', '\n', '\r', '\0', '&'])
        {
            return Err(anyhow!("invalid SRT listener secret configuration"));
        }
        let description = format!(
            "srtsrc name=srt_source uri=\"srt://0.0.0.0:{listen_port}?mode=listener&latency={latency_ms}&pbkeylen={pbkeylen}&passphrase={passphrase}\" authentication=true keep-listening=true ! tsdemux ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au ! tee name=encoded_tee encoded_tee. ! queue max-size-buffers=8 max-size-bytes=0 max-size-time=0 leaky=downstream ! appsink name=encoded_au max-buffers=8 drop=true sync=false encoded_tee. ! queue max-size-buffers=2 max-size-bytes=0 max-size-time=0 leaky=downstream ! avdec_h264 ! videoconvert ! fakesink sync=false"
        );
        let pipeline = gst::parse::launch(&description)
            .context("receiver pipeline parse failed")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("receiver description did not produce a pipeline"))?;
        let source = pipeline.by_name("srt_source").context("srtsrc missing")?;
        let current_key: Arc<Mutex<Option<StreamKey>>> = Arc::new(Mutex::new(None));
        let connecting_key = Arc::clone(&current_key);
        let connecting_registry = Arc::clone(&registry);
        source.connect("caller-connecting", false, move |values| {
            let stream_id = values.get(2)?.get::<String>().ok()?;
            let accepted = connecting_registry
                .accept(listen_port, &stream_id)
                .and_then(|key| {
                    connecting_registry.transition(&key, BootstrapState::MediaProbing)?;
                    connecting_registry.transition(&key, BootstrapState::ProvisionalStream)?;
                    tracing::info!(
                        session_id = %key.session_id,
                        camera_id = %key.camera_id,
                        stream_epoch = key.epoch,
                        %listen_port,
                        "authenticated SRT stream accepted"
                    );
                    let mut current = connecting_key.lock().map_err(|_| ReceiverError::Poisoned)?;
                    *current = Some(key);
                    Ok(())
                })
                .is_ok();
            Some(accepted.to_value())
        });
        let removed_key = Arc::clone(&current_key);
        let removed_registry = Arc::clone(&registry);
        source.connect("caller-removed", false, move |_| {
            if let Ok(mut current) = removed_key.lock() {
                if let Some(key) = current.take() {
                    let _ = removed_registry.disconnect(&key);
                }
            }
            None
        });

        let appsink = pipeline
            .by_name("encoded_au")
            .context("encoded appsink missing")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| anyhow!("encoded_au is not an appsink"))?;
        let ingest_key = current_key;
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let pts_ns = buffer.pts().map(gst::ClockTime::nseconds);
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let key = ingest_key
                        .lock()
                        .map_err(|_| gst::FlowError::Error)?
                        .clone()
                        .ok_or(gst::FlowError::Error)?;
                    let envelope = registry
                        .ingest_h264_before_decode(&key, pts_ns, map.as_slice())
                        .map_err(|error| {
                            tracing::warn!(%error, %listen_port, "rejecting access unit before decode");
                            gst::FlowError::Error
                        })?;
                    runtime.process(envelope).map_err(|error| {
                        tracing::warn!(%error, %listen_port, "metadata bootstrap/synchronization rejected AU");
                        gst::FlowError::Error
                    })?;
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline
            .set_state(gst::State::Playing)
            .context("receiver listener failed to enter Playing")?;
        Ok(Self { pipeline })
    }
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

pub fn verify_gstreamer_runtime() -> Result<()> {
    gst::init().context("GStreamer initialization failed")?;
    for name in [
        "srtsrc",
        "mpegtsmux",
        "tsdemux",
        "h264parse",
        "h265parse",
        "avdec_h264",
        "appsink",
    ] {
        if gst::ElementFactory::find(name).is_none() {
            return Err(anyhow!("required GStreamer element {name} is unavailable"));
        }
    }
    let (major, minor, _, _) = gst::version();
    if major < 1 || (major == 1 && minor < 22) {
        return Err(anyhow!("GStreamer 1.22 or newer is required"));
    }
    Ok(())
}
