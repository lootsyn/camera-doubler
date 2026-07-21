#[cfg(target_os = "linux")]
use anyhow::{anyhow, Context, Result};
#[cfg(target_os = "linux")]
use gstreamer as gst;
#[cfg(target_os = "linux")]
use gstreamer::prelude::*;

#[cfg(target_os = "linux")]
use edge_core::CameraPipelinePlan;

#[cfg(target_os = "linux")]
pub struct UiOnlyHandle {
    pipeline: gst::Pipeline,
}

#[cfg(target_os = "linux")]
impl UiOnlyHandle {
    pub fn start(plan: &CameraPipelinePlan) -> Result<Self> {
        gst::init().context("GStreamer initialization failed")?;
        let pipeline = gst::parse::launch(&plan.capture_description()?)
            .context("UI-only pipeline parse failed")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("UI-only description did not produce a pipeline"))?;
        pipeline
            .set_state(gst::State::Playing)
            .context("UI-only pipeline failed to enter Playing")?;
        Ok(Self { pipeline })
    }
}

#[cfg(target_os = "linux")]
impl Drop for UiOnlyHandle {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
