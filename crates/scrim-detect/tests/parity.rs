//! Does the Rust detector see what the Python detector saw?
//!
//! The censor plans this project trusts were produced by the Python `nudenet`
//! package. Porting inference to Rust means re-implementing the preprocessing
//! (pad to a square, resize to 320, normalise, BGR to RGB) and the
//! postprocessing (threshold, rescale, class-agnostic NMS) by hand, and any
//! one of those steps can be subtly wrong in a way that still produces
//! plausible boxes.
//!
//! `tools/export_detector_fixture.py` recorded every detection the Python
//! found over 65 seconds of the real test movie, chosen as its densest span so
//! the explicit path is genuinely exercised. This test decodes the same frames
//! with the same ffmpeg command and holds the Rust to that record.
//!
//! Exact equality is not the bar and should not be: the two resize
//! implementations interpolate slightly differently, so scores wobble in the
//! third decimal and box edges by a pixel. What must hold is the part the app
//! depends on:
//!
//!   * **no explicit detection may go missing.** A frame Python covered and
//!     Rust did not is the failure this whole project exists to prevent.
//!   * boxes that do match must match closely, or the cover lands in the wrong
//!     place.
//!
//! Skips when `abc.mp4` or the bundled binaries are absent, so a bare checkout
//! still runs the suite.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use scrim_detect::nudenet::{is_explicit, NudeDetector};

const THRESHOLD: f32 = 0.55;
/// Matched boxes must overlap at least this much.
const MIN_IOU: f64 = 0.80;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

struct Fixture {
    start: f64,
    duration: f64,
    fps: f64,
    width: usize,
    height: usize,
    frames: Vec<Vec<Det>>,
}

#[derive(Debug, Clone)]
struct Det {
    class: String,
    score: f64,
    // x, y, w, h
    b: [f64; 4],
}

fn load_fixture() -> Fixture {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detector.json");
    let text = std::fs::read_to_string(path).expect("detector fixture");
    let v: serde_json::Value = serde_json::from_str(&text).expect("fixture parses");

    let frames = v["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .map(|f| {
            f["detections"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| Det {
                    class: d["class"].as_str().unwrap().to_string(),
                    score: d["score"].as_f64().unwrap(),
                    b: {
                        let a = d["box"].as_array().unwrap();
                        [
                            a[0].as_f64().unwrap(),
                            a[1].as_f64().unwrap(),
                            a[2].as_f64().unwrap(),
                            a[3].as_f64().unwrap(),
                        ]
                    },
                })
                .collect()
        })
        .collect();

    Fixture {
        start: v["start"].as_f64().unwrap(),
        duration: v["duration"].as_f64().unwrap(),
        fps: v["sample_fps"].as_f64().unwrap(),
        width: v["detect_width"].as_u64().unwrap() as usize,
        height: v["detect_height"].as_u64().unwrap() as usize,
        frames,
    }
}

fn iou(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let ix = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let iy = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    let inter = ix * iy;
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[test]
fn rust_detector_agrees_with_the_python_it_replaces() {
    let repo = repo();
    let video = repo.join("abc.mp4");
    let ffmpeg = repo.join("resources/ffmpeg.exe");
    let model = repo.join("resources/320n.onnx");
    let dylib = repo.join("resources/onnxruntime.dll");

    for (what, path) in [
        ("abc.mp4", &video),
        ("ffmpeg", &ffmpeg),
        ("the model", &model),
    ] {
        if !path.exists() {
            eprintln!("skipping detector parity: {what} is not present");
            return;
        }
    }

    let fx = load_fixture();
    let mut detector = match NudeDetector::new(&model, Some(&dylib)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping detector parity: {e}");
            return;
        }
    };

    let mut child = Command::new(&ffmpeg)
        .args(["-v", "error", "-ss"])
        .arg(format!("{:.3}", fx.start))
        .arg("-t")
        .arg(format!("{:.3}", fx.duration))
        .arg("-i")
        .arg(&video)
        .args(["-an", "-sn", "-vf"])
        .arg(format!("fps={},scale={}:{}", fx.fps, fx.width, fx.height))
        .args(["-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg runs");

    let mut stdout = child.stdout.take().unwrap();
    let frame_bytes = fx.width * fx.height * 3;
    let mut buf = vec![0u8; frame_bytes];

    let (mut matched, mut missed, mut extra, mut checked) = (0usize, 0usize, 0usize, 0usize);
    let mut iou_total = 0.0f64;
    let mut worst: Option<(usize, String, f64)> = None;
    let mut missing_report: Vec<String> = Vec::new();

    for (index, want_frame) in fx.frames.iter().enumerate() {
        if !read_full(&mut stdout, &mut buf) {
            panic!("ffmpeg produced only {index} of {} frames", fx.frames.len());
        }

        let got = detector
            .detect(&buf, fx.width, fx.height)
            .expect("inference succeeds");

        // Only the explicit classes above threshold drive coverage; the rest
        // (faces, bellies, feet) are never acted on, so they are not the
        // contract.
        let want: Vec<&Det> = want_frame
            .iter()
            .filter(|d| is_explicit(&d.class) && d.score as f32 >= THRESHOLD)
            .collect();
        let mine: Vec<_> = got
            .iter()
            .filter(|d| is_explicit(d.label()) && d.score >= THRESHOLD)
            .collect();

        let mut taken = vec![false; mine.len()];
        for w in &want {
            checked += 1;
            let mut best = (0.0f64, usize::MAX);
            for (j, m) in mine.iter().enumerate() {
                if taken[j] || m.label() != w.class {
                    continue;
                }
                let mb = [m.x as f64, m.y as f64, m.w as f64, m.h as f64];
                let score = iou(&w.b, &mb);
                if score > best.0 {
                    best = (score, j);
                }
            }
            if best.0 >= MIN_IOU {
                taken[best.1] = true;
                matched += 1;
                iou_total += best.0;
                if worst.as_ref().map(|w| best.0 < w.2).unwrap_or(true) {
                    worst = Some((index, w.class.clone(), best.0));
                }
            } else {
                missed += 1;
                missing_report.push(format!(
                    "  frame {index}: {} score {:.3} box {:?} (best overlap {:.2})",
                    w.class, w.score, w.b, best.0
                ));
            }
        }
        extra += taken.iter().filter(|t| !**t).count();
    }

    let _ = child.kill();
    let _ = child.wait();

    println!(
        "explicit detections: {checked} in the fixture, {matched} matched, {missed} missed, {extra} extra"
    );
    if matched > 0 {
        println!("mean IoU on matches: {:.4}", iou_total / matched as f64);
    }
    if let Some((frame, class, score)) = worst {
        println!("weakest match: frame {frame} {class} IoU {score:.3}");
    }

    assert!(
        checked > 20,
        "the fixture must contain enough explicit detections to be meaningful, had {checked}"
    );

    // The direction that matters. Anything Python covered, Rust must cover.
    assert!(
        missed == 0,
        "{missed} of {checked} explicit detections were not reproduced.\n\
         Every one of these is a region the Python covered and this port would \
         leave on screen:\n{}",
        missing_report.join("\n")
    );

    // Extra detections over-cover, which is safe, but a flood of them means
    // the postprocessing is wrong rather than merely different.
    assert!(
        extra <= checked / 4,
        "{extra} detections appeared that the Python never saw (from {checked}); \
         NMS or the score threshold is probably wrong"
    );
}

fn read_full(r: &mut impl std::io::Read, buf: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => return false,
            Ok(n) => filled += n,
            Err(_) => return false,
        }
    }
    true
}

#[test]
fn the_fixture_itself_contains_explicit_detections() {
    // Guards the guard: a fixture of only misses would let a detector that
    // finds nothing at all pass the parity test above.
    let fx = load_fixture();
    let explicit = fx
        .frames
        .iter()
        .flatten()
        .filter(|d| is_explicit(&d.class) && d.score as f32 >= THRESHOLD)
        .count();
    assert!(
        explicit > 20,
        "fixture has only {explicit} explicit detections; regenerate it over a denser span"
    );
}
