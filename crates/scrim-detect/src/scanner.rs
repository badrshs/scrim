//! The linear scanner.
//!
//! Ported from `livescan.py`, which is the scan path the project actually
//! trusts. It walks a movie once at 3 frames per second, roughly 15x faster
//! than playback, and publishes results as it goes so live mode can start
//! playing before the scan finishes.
//!
//! The **frontier** is the point the scanner has reached. Everything before it
//! has been looked at; everything after it has not. Live playback is fenced
//! against the frontier, and the plan is only marked complete when the
//! frontier reaches the end of the movie.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use scrim_core::{DetBox, Detection, Detector, Plan, Source, SCHEMA_VERSION};

use crate::nudenet::{is_explicit, NudeDetector};
use crate::probe::{probe, VideoInfo};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Frames per second fed to the detector. From `livescan.py::SAMPLE_FPS`.
pub const SAMPLE_FPS: f64 = 3.0;
/// Confidence cutoff. From `livescan.py::THRESHOLD`.
///
/// Kept as f64 as well, because widening the f32 for the plan file writes
/// `0.550000011920929` into a document people read.
pub const THRESHOLD_F64: f64 = 0.55;
pub const THRESHOLD: f32 = THRESHOLD_F64 as f32;

/// Detections weaker than this are not written down at all.
///
/// Deliberately below the covering threshold. Plans store what was *seen*, and
/// coverage is derived from them afterwards, so recording a margin below the
/// cutoff is what lets someone lower the threshold later and have the extra
/// detections already be there. Recording everything would bloat the file with
/// noise nobody will ever want.
pub const RECORD_FLOOR: f32 = 0.35;
/// The detector input is capped at this width to keep the pipe cheap.
const MAX_DETECT_WIDTH: i64 = 1280;

/// What the scan looks like from outside, safe to read at any time.
#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub frontier: f64,
    pub duration: f64,
    pub detections: usize,
    pub done: bool,
    pub stopped: bool,
    pub error: Option<String>,
    /// Multiple of realtime, for the live banner.
    pub speed: f64,
}

struct Shared {
    progress: Mutex<ScanProgress>,
    detections: Mutex<Vec<Detection>>,
    stop: AtomicBool,
}

/// A scan in flight. Dropping it stops the scan.
pub struct Scan {
    shared: Arc<Shared>,
    info: VideoInfo,
    video: PathBuf,
}

pub struct ScanConfig {
    pub ffmpeg: PathBuf,
    pub model: PathBuf,
    pub onnxruntime: Option<PathBuf>,
}

impl Scan {
    /// Start scanning on a background thread.
    pub fn start(video: &Path, cfg: ScanConfig) -> Result<Self, String> {
        let info = probe(&cfg.ffmpeg, video)?;
        if info.width <= 0 || info.height <= 0 || info.duration <= 0.0 {
            return Err("this file does not look like a video".into());
        }

        let shared = Arc::new(Shared {
            progress: Mutex::new(ScanProgress {
                duration: info.duration,
                ..Default::default()
            }),
            detections: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });

        let scan = Self {
            shared: shared.clone(),
            info: info.clone(),
            video: video.to_path_buf(),
        };

        let video = video.to_path_buf();
        std::thread::spawn(move || {
            let result = run(&video, &info, &cfg, &shared);
            let mut p = shared.progress.lock().unwrap();
            match result {
                Ok(()) => {
                    if !shared.stop.load(Ordering::Relaxed) {
                        p.frontier = info.duration;
                        p.done = true;
                    }
                }
                Err(e) => p.error = Some(e),
            }
        });

        Ok(scan)
    }

    pub fn progress(&self) -> ScanProgress {
        self.shared.progress.lock().unwrap().clone()
    }

    pub fn detections(&self) -> Vec<Detection> {
        self.shared.detections.lock().unwrap().clone()
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.progress.lock().unwrap().stopped = true;
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    /// Build a plan from what has been found so far.
    ///
    /// `complete` is only true once the whole movie has been looked at, and
    /// the player refuses to cast or to play in scanned-plan mode on anything
    /// else. A partial plan is a record of progress, never a guarantee.
    pub fn plan(&self) -> Plan {
        let p = self.progress();
        let (dw, dh) = detect_size(&self.info);
        Plan {
            schema_version: SCHEMA_VERSION,
            generator: concat!("scrim ", env!("CARGO_PKG_VERSION")).to_string(),
            created_at: String::new(),
            source: Source {
                name: self
                    .video
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                size_bytes: std::fs::metadata(&self.video).map(|m| m.len()).unwrap_or(0),
                duration: self.info.duration,
                fps: self.info.fps,
                width: self.info.width,
                height: self.info.height,
            },
            detector: Detector {
                sample_fps: SAMPLE_FPS,
                threshold: THRESHOLD_F64,
                detect_width: dw,
                detect_height: dh,
            },
            complete: p.done,
            detections: self.detections(),
        }
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The detector runs on a smaller frame; boxes are scaled back afterwards.
fn detect_size(info: &VideoInfo) -> (i64, i64) {
    let w = info.width.min(MAX_DETECT_WIDTH) / 2 * 2;
    let h = (info.height as f64 * w as f64 / info.width as f64) as i64 / 2 * 2;
    (w.max(2), h.max(2))
}

fn run(
    video: &Path,
    info: &VideoInfo,
    cfg: &ScanConfig,
    shared: &Arc<Shared>,
) -> Result<(), String> {
    let (dw, dh) = detect_size(info);
    let mut detector = NudeDetector::new(&cfg.model, cfg.onnxruntime.as_deref())?;

    let mut child = spawn_ffmpeg(&cfg.ffmpeg, video, dw, dh)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("ffmpeg produced no output pipe")?;

    let frame_bytes = (dw * dh * 3) as usize;
    let mut buf = vec![0u8; frame_bytes];

    // Boxes come back in detector pixels and have to land in source pixels.
    let sx = info.width as f64 / dw as f64;
    let sy = info.height as f64 / dh as f64;

    let started = std::time::Instant::now();
    let mut index = 0u64;
    let mut covering = 0usize;

    loop {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        if !read_exact_or_eof(&mut stdout, &mut buf)? {
            break; // clean end of stream
        }

        let t = index as f64 / SAMPLE_FPS;
        index += 1;

        let found = detector.detect(&buf, dw as usize, dh as usize)?;
        let boxes: Vec<DetBox> = found
            .iter()
            .filter(|d| is_explicit(d.label()) && d.score >= RECORD_FLOOR)
            .map(|d| {
                let [x1, y1, x2, y2] = d.corners();
                DetBox {
                    bounds: [
                        (x1 as f64 * sx) as i64,
                        (y1 as f64 * sy) as i64,
                        (x2 as f64 * sx) as i64,
                        (y2 as f64 * sy) as i64,
                    ],
                    label: d.label().to_string(),
                    score: round3(d.score as f64),
                }
            })
            .collect();

        // Counted separately from what gets stored. The plan records down to
        // RECORD_FLOOR so the threshold stays adjustable afterwards, but
        // "found" in the interface should mean "would be covered", not
        // "was noticed and filed away".
        if boxes.iter().any(|b| b.score >= THRESHOLD_F64) {
            covering += 1;
        }
        if !boxes.is_empty() {
            shared.detections.lock().unwrap().push(Detection {
                t: round3(t),
                boxes,
            });
        }

        let mut p = shared.progress.lock().unwrap();
        p.frontier = t;
        p.detections = covering;
        let elapsed = started.elapsed().as_secs_f64();
        p.speed = if elapsed > 0.0 { t / elapsed } else { 0.0 };
    }

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn spawn_ffmpeg(ffmpeg: &Path, video: &Path, w: i64, h: i64) -> Result<Child, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-i"])
        .arg(video)
        .args(["-an", "-sn", "-vf"])
        .arg(format!("fps={SAMPLE_FPS},scale={w}:{h}"))
        .args(["-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn()
        .map_err(|e| format!("could not run ffmpeg: {e}"))
}

/// Fill `buf` completely, or report a clean end of stream.
///
/// A short read is end of stream, not a frame. Treating a partial buffer as a
/// frame would feed the detector a torn image, and a missed detection there is
/// exactly the failure this project cannot have.
fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("reading frames from ffmpeg: {e}")),
        }
    }
    Ok(true)
}

fn round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: i64, h: i64) -> VideoInfo {
        VideoInfo {
            width: w,
            height: h,
            duration: 100.0,
            fps: 24.0,
        }
    }

    #[test]
    fn detect_size_matches_livescan() {
        // livescan.py: ex_w = min(src_w, 1280) // 2 * 2, height keeps aspect.
        assert_eq!(detect_size(&info(1280, 720)), (1280, 720));
        assert_eq!(detect_size(&info(1920, 1080)), (1280, 720));
        assert_eq!(detect_size(&info(640, 480)), (640, 480));
    }

    #[test]
    fn detect_size_is_always_even() {
        // Odd dimensions break chroma subsampling in the ffmpeg pipe.
        for (w, h) in [(1921, 1081), (1279, 719), (853, 481)] {
            let (dw, dh) = detect_size(&info(w, h));
            assert_eq!(dw % 2, 0, "{w}x{h} produced odd width {dw}");
            assert_eq!(dh % 2, 0, "{w}x{h} produced odd height {dh}");
        }
    }

    #[test]
    fn a_short_read_is_end_of_stream_not_a_torn_frame() {
        let data = vec![7u8; 10];
        let mut buf = vec![0u8; 16];
        assert_eq!(read_exact_or_eof(&mut data.as_slice(), &mut buf), Ok(false));

        let full = vec![7u8; 16];
        let mut buf = vec![0u8; 16];
        assert_eq!(read_exact_or_eof(&mut full.as_slice(), &mut buf), Ok(true));
        assert!(buf.iter().all(|b| *b == 7));
    }
}
