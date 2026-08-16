//! Nudity detection for Scrim.
//!
//! One linear pass over a movie: ffmpeg decodes frames at 3 fps into the
//! NudeNet ONNX model, and explicit regions become detections in a plan.
//!
//! The same pass serves both modes. Pre-scanning runs it to the end and saves
//! a complete plan; live mode reads its results while it is still running, and
//! fences playback against how far it has got.

#![forbid(unsafe_code)]

pub mod nudenet;
pub mod probe;
pub mod scanner;

pub use nudenet::{is_explicit, Detection as RawDetection, NudeDetector, EXPLICIT_LABELS, LABELS};
pub use probe::{probe, VideoInfo};
pub use scanner::{Scan, ScanConfig, ScanProgress, SAMPLE_FPS, THRESHOLD};
