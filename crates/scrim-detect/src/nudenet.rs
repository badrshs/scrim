//! The NudeNet detector, ported from the Python `nudenet` package.
//!
//! Reproduced deliberately step for step, because the golden censor plans were
//! produced by that implementation and a change here moves every box. The
//! pipeline is:
//!
//!   1. pad the frame to a square by extending the **bottom and right** with
//!      black (not centred, which is what you would expect and would be wrong)
//!   2. resize that square to 320x320, bilinear
//!   3. scale to 0..1, swap BGR to RGB, lay out as NCHW
//!   4. run the model, which returns 4 box values plus 18 class scores per
//!      candidate
//!   5. keep candidates over 0.2, map back to source pixels, then run one
//!      class-agnostic NMS pass at score 0.25 / IoU 0.45
//!
//! Anything that looks arbitrary here is arbitrary in the original too.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

pub const INPUT_SIZE: usize = 320;

/// Candidates below this are discarded before NMS even sees them.
const RAW_SCORE_FLOOR: f32 = 0.2;
/// `cv2.dnn.NMSBoxes(boxes, scores, score_threshold, nms_threshold)`.
const NMS_SCORE_THRESHOLD: f32 = 0.25;
const NMS_IOU_THRESHOLD: f32 = 0.45;

/// Class order is the model's, so it is fixed, not a preference.
pub const LABELS: [&str; 18] = [
    "FEMALE_GENITALIA_COVERED",
    "FACE_FEMALE",
    "BUTTOCKS_EXPOSED",
    "FEMALE_BREAST_EXPOSED",
    "FEMALE_GENITALIA_EXPOSED",
    "MALE_BREAST_EXPOSED",
    "ANUS_EXPOSED",
    "FEET_EXPOSED",
    "BELLY_COVERED",
    "FEET_COVERED",
    "ARMPITS_COVERED",
    "ARMPITS_EXPOSED",
    "FACE_MALE",
    "BELLY_EXPOSED",
    "MALE_GENITALIA_EXPOSED",
    "ANUS_COVERED",
    "FEMALE_BREAST_COVERED",
    "BUTTOCKS_COVERED",
];

/// The only classes Scrim covers: explicit nudity and visible sex acts.
///
/// Matches pureframe's `EXPLICIT_LABELS`. Kissing, suggestive scenes, covered
/// bodies and violence are deliberately not in this list, and widening it is a
/// product decision, not a tuning one.
pub const EXPLICIT_LABELS: [&str; 5] = [
    "FEMALE_BREAST_EXPOSED",
    "FEMALE_GENITALIA_EXPOSED",
    "MALE_GENITALIA_EXPOSED",
    "BUTTOCKS_EXPOSED",
    "ANUS_EXPOSED",
];

pub fn is_explicit(label: &str) -> bool {
    EXPLICIT_LABELS.contains(&label)
}

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub class_id: usize,
    pub score: f32,
    /// Source pixels, `[x, y, w, h]`, matching the Python's `box`.
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Detection {
    pub fn label(&self) -> &'static str {
        LABELS[self.class_id]
    }

    /// `[x1, y1, x2, y2]`, which is what the plan stores.
    pub fn corners(&self) -> [i64; 4] {
        [self.x, self.y, self.x + self.w, self.y + self.h]
    }
}

pub struct NudeDetector {
    session: Session,
    input_name: String,
    /// Scratch buffer, reused across frames so a long scan does not spend all
    /// its time allocating 1.2 MB tensors.
    input: Vec<f32>,
}

impl NudeDetector {
    /// `dylib` is the ONNX Runtime DLL Scrim bundles; `model` is `320n.onnx`.
    pub fn new(model: &Path, dylib: Option<&Path>) -> Result<Self, String> {
        if let Some(dylib) = dylib {
            // `ort` reads this when built with the load-dynamic feature, which
            // is how Scrim ships one known ONNX Runtime rather than depending
            // on whatever happens to be installed.
            std::env::set_var("ORT_DYLIB_PATH", dylib);
        }

        let session = Session::builder()
            .map_err(|e| format!("onnxruntime unavailable: {e}"))?
            .commit_from_file(model)
            .map_err(|e| format!("could not load {}: {e}", model.display()))?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or("the model reports no inputs")?;

        Ok(Self {
            session,
            input_name,
            input: vec![0.0; 3 * INPUT_SIZE * INPUT_SIZE],
        })
    }

    /// Detect on one packed BGR frame.
    pub fn detect(&mut self, bgr: &[u8], width: usize, height: usize) -> Result<Vec<Detection>, String> {
        if bgr.len() < width * height * 3 {
            return Err("frame buffer is smaller than its stated dimensions".into());
        }
        self.preprocess(bgr, width, height);

        let tensor = Tensor::from_array((
            [1_usize, 3, INPUT_SIZE, INPUT_SIZE],
            self.input.clone().into_boxed_slice(),
        ))
        .map_err(|e| format!("building the input tensor: {e}"))?;

        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| format!("inference failed: {e}"))?;

        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("reading the model output: {e}"))?;

        Ok(postprocess(shape, data, width, height))
    }

    /// Steps 1 to 3: pad to a square, resize to 320, normalise, BGR to RGB, NCHW.
    fn preprocess(&mut self, bgr: &[u8], width: usize, height: usize) {
        let max_size = width.max(height);
        let scale = max_size as f32 / INPUT_SIZE as f32;
        let plane = INPUT_SIZE * INPUT_SIZE;

        for dy in 0..INPUT_SIZE {
            // OpenCV's INTER_LINEAR sampling grid: centre of the destination
            // pixel mapped back into source space.
            let fy = (dy as f32 + 0.5) * scale - 0.5;
            let (sy0, wy) = split(fy, max_size);
            let sy1 = (sy0 + 1).min(max_size.saturating_sub(1));

            for dx in 0..INPUT_SIZE {
                let fx = (dx as f32 + 0.5) * scale - 0.5;
                let (sx0, wx) = split(fx, max_size);
                let sx1 = (sx0 + 1).min(max_size.saturating_sub(1));

                // The padding is virtual: anything outside the real frame
                // reads as black rather than being materialised into a
                // full-size square buffer every frame.
                let p00 = sample(bgr, width, height, sx0, sy0);
                let p10 = sample(bgr, width, height, sx1, sy0);
                let p01 = sample(bgr, width, height, sx0, sy1);
                let p11 = sample(bgr, width, height, sx1, sy1);

                let out = dy * INPUT_SIZE + dx;
                for c in 0..3 {
                    let top = p00[c] * (1.0 - wx) + p10[c] * wx;
                    let bottom = p01[c] * (1.0 - wx) + p11[c] * wx;
                    let value = (top * (1.0 - wy) + bottom * wy) / 255.0;
                    // Channels pass straight through, BGR and all.
                    //
                    // This looks wrong and is not. The Python swaps twice and
                    // ends up where it started: `cvtColor(COLOR_RGBA2BGR)` on a
                    // three channel image swaps R and B, and then
                    // `blobFromImage(swapRB=True)` swaps them back. Verified
                    // against the real thing: a BGR pixel (10, 100, 250) reaches
                    // the model as (10, 100, 250).
                    //
                    // So the model is fed BGR. Whether NudeNet meant that is
                    // beside the point; it is how the weights were trained and
                    // how every plan this project trusts was produced. Helpfully
                    // "correcting" it to RGB costs about 0.05 of confidence,
                    // which is enough to drop borderline regions under the 0.55
                    // threshold and leave them uncovered.
                    self.input[c * plane + out] = value;
                }
            }
        }
    }
}

/// Split a source coordinate into its integer part and interpolation weight,
/// clamped the way OpenCV clamps at the edges.
fn split(f: f32, limit: usize) -> (usize, f32) {
    if f <= 0.0 {
        return (0, 0.0);
    }
    let i = f.floor();
    let idx = i as usize;
    if idx >= limit.saturating_sub(1) {
        return (limit.saturating_sub(1), 0.0);
    }
    (idx, f - i)
}

#[inline]
fn sample(bgr: &[u8], width: usize, height: usize, x: usize, y: usize) -> [f32; 3] {
    if x >= width || y >= height {
        return [0.0, 0.0, 0.0]; // the padded region
    }
    let i = (y * width + x) * 3;
    [bgr[i] as f32, bgr[i + 1] as f32, bgr[i + 2] as f32]
}

/// Steps 4 and 5: read candidates, map back to source pixels, then NMS.
fn postprocess(shape: &[i64], data: &[f32], width: usize, height: usize) -> Vec<Detection> {
    // Output is [1, 4 + classes, candidates]; the Python squeezes and
    // transposes it so each row is one candidate.
    let (attrs, candidates) = match shape {
        [_, a, c] => (*a as usize, *c as usize),
        [a, c] => (*a as usize, *c as usize),
        _ => return Vec::new(),
    };
    if attrs < 5 || data.len() < attrs * candidates {
        return Vec::new();
    }
    let classes = attrs - 4;

    let max_size = width.max(height) as f32;
    let scale = max_size / INPUT_SIZE as f32;
    let at = |attr: usize, i: usize| data[attr * candidates + i];

    let mut kept: Vec<Detection> = Vec::new();
    let mut scored: Vec<(f32, usize, [f32; 4])> = Vec::new();

    for i in 0..candidates {
        let mut best = 0usize;
        let mut best_score = f32::MIN;
        for c in 0..classes {
            let s = at(4 + c, i);
            if s > best_score {
                best_score = s;
                best = c;
            }
        }
        if best_score < RAW_SCORE_FLOOR {
            continue;
        }

        let (cx, cy, bw, bh) = (at(0, i), at(1, i), at(2, i), at(3, i));
        let mut x = (cx - bw / 2.0) * scale;
        let mut y = (cy - bh / 2.0) * scale;
        let mut w = bw * scale;
        let mut h = bh * scale;

        x = x.clamp(0.0, width as f32);
        y = y.clamp(0.0, height as f32);
        w = w.min(width as f32 - x);
        h = h.min(height as f32 - y);

        scored.push((best_score, best, [x, y, w, h]));
    }

    // cv2.dnn.NMSBoxes: drop below score_threshold, sort by score, then greedy
    // suppression by IoU. It is class-agnostic, and matching that matters:
    // a face overlapping a torso can suppress it.
    scored.retain(|(s, _, _)| *s >= NMS_SCORE_THRESHOLD);
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut suppressed = vec![false; scored.len()];
    for i in 0..scored.len() {
        if suppressed[i] {
            continue;
        }
        let (score, class_id, b) = scored[i];
        kept.push(Detection {
            class_id,
            score,
            x: b[0] as i64,
            y: b[1] as i64,
            w: b[2] as i64,
            h: b[3] as i64,
        });
        for j in (i + 1)..scored.len() {
            if !suppressed[j] && iou(&b, &scored[j].2) > NMS_IOU_THRESHOLD {
                suppressed[j] = true;
            }
        }
    }

    kept
}

/// Intersection over union for `[x, y, w, h]` boxes.
fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ax2 = a[0] + a[2];
    let ay2 = a[1] + a[3];
    let bx2 = b[0] + b[2];
    let by2 = b[1] + b[3];

    let ix = (ax2.min(bx2) - a[0].max(b[0])).max(0.0);
    let iy = (ay2.min(by2) - a[1].max(b[1])).max(0.0);
    let inter = ix * iy;

    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_labels_are_exactly_the_five_pureframe_used() {
        assert!(is_explicit("FEMALE_BREAST_EXPOSED"));
        assert!(is_explicit("ANUS_EXPOSED"));
        // Deliberately not covered.
        assert!(!is_explicit("FEMALE_BREAST_COVERED"));
        assert!(!is_explicit("BELLY_EXPOSED"));
        assert!(!is_explicit("FACE_FEMALE"));
        assert!(!is_explicit("ARMPITS_EXPOSED"));
        assert!(!is_explicit("FEET_EXPOSED"));
    }

    #[test]
    fn every_explicit_label_exists_in_the_model_class_list() {
        // A typo here would silently stop covering a whole category.
        for label in EXPLICIT_LABELS {
            assert!(LABELS.contains(&label), "{label} is not a model class");
        }
    }

    #[test]
    fn iou_matches_hand_computed_overlap() {
        let a = [0.0, 0.0, 10.0, 10.0];
        assert_eq!(iou(&a, &a), 1.0);
        // Half overlap: intersection 50, union 150.
        let b = [5.0, 0.0, 10.0, 10.0];
        assert!((iou(&a, &b) - 50.0 / 150.0).abs() < 1e-6);
        // Disjoint.
        assert_eq!(iou(&a, &[20.0, 20.0, 5.0, 5.0]), 0.0);
    }

    #[test]
    fn sampling_outside_the_frame_reads_as_black_padding() {
        let frame = vec![255u8; 4 * 4 * 3];
        assert_eq!(sample(&frame, 4, 4, 0, 0), [255.0, 255.0, 255.0]);
        // NudeNet pads bottom and right, so those reads must be black.
        assert_eq!(sample(&frame, 4, 4, 9, 0), [0.0, 0.0, 0.0]);
        assert_eq!(sample(&frame, 4, 4, 0, 9), [0.0, 0.0, 0.0]);
    }
}
