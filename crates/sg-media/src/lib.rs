//! Media tooling: probing, the frame-rate scan, and the files derived from an
//! asset at import (poster, conformed audio, peaks, conform, proxy).
//!
//! In M2 this drives the `ffprobe` and `ffmpeg` programs with the command
//! lines measured in spike S4. Everything here blocks and runs on job-pool
//! threads; every call takes a [`Cancel`] and returns the tool's stderr on
//! failure. In-process decoding for playback is added in M3.

pub mod decode;
pub mod derive;
pub mod peaks;
pub mod probe;
pub mod scan;
pub mod tool;

pub use probe::{Probed, probe};
pub use tool::{Cancel, ToolError};
