//! The compositor: turns "these pictures, placed like this" into one frame.
//! Preview and export both go through [`Compositor`], so what is exported
//! is what was previewed.
//!
//! Compositing happens in linear light in an `Rgba16Float` target; an output
//! pass encodes it for the display or the encoder.

mod compositor;
mod plan;
pub mod text;
mod yuv;

pub use compositor::{Compositor, LayerDraw, Motion};
pub use plan::{PlanLayer, Source, load_luts, plan, plan_track, sequence_frames};
pub use yuv::{YuvImporter, YuvPlanes};

/// Errors the GPU reported outside any error scope, counted so that an
/// export can tell whether one happened while it ran.
static GPU_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many GPU errors have been reported since the program started.
pub fn gpu_errors() -> u64 {
    GPU_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Makes GPU errors (a rejected draw, memory running out) a line in the log
/// instead of the end of the program: the frame they hit may be wrong, the
/// next one is drawn as usual, and an export that was running fails.
pub fn report_gpu_errors(device: &wgpu::Device) {
    device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
        let seen = GPU_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The same error tends to come once per frame: the first few say enough.
        if seen < 5 {
            log::error!("the GPU reported an error: {error}");
        }
    }));
}
