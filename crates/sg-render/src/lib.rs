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
pub use plan::{PlanLayer, Source, plan, plan_track, sequence_frames};
pub use yuv::{YuvImporter, YuvPlanes};
