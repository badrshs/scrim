//! Turning raw detections into censor windows.
//!
//! Ported from `livescan.py::LiveScanner.windows`, which is the algorithm that
//! actually shipped and was watched against real movies. The behaviour is
//! reproduced exactly, including its deliberate over-covering:
//!
//!   * censoring starts `pad_before` seconds ahead of the first detection in a
//!     run and holds `pad_after` seconds past the last one, because the
//!     detector samples at 3 fps and will miss the frames either side;
//!   * a window with no box data anywhere near it inherits the whole run's
//!     covered area rather than showing anything;
//!   * if the window count would blow past the filtergraph budget, windows get
//!     longer (merging harder, covering more) instead of being dropped.
//!
//! Every one of those trades a bigger black box for a smaller chance of
//! missing something. That is the intended direction.

use crate::plan::{Detection, Plan};
use crate::util::{floor_div, round3};

/// The tunables from `livescan.py`, surfaced in Settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowParams {
    /// Start covering this long before a run's first detection.
    pub pad_before: f64,
    /// Keep covering this long after a run's last detection.
    pub pad_after: f64,
    /// Detections closer together than this belong to the same run.
    pub gap_merge: f64,
    /// Runs are chopped into windows no longer than this.
    pub window_max: f64,
    /// Grow every box by this fraction of the frame on each side.
    pub margin: f64,
    /// Above this many windows, widen `window_max` and rebuild.
    ///
    /// This is not a performance knob, it is a correctness one. See the note
    /// on the default below.
    pub max_windows: usize,

    /// Ignore detections the model was less sure about than this.
    ///
    /// Applied here rather than during the scan, so moving it re-derives
    /// coverage from a plan already on disk instead of forcing a rescan. The
    /// useful band is narrow: on real footage genuine nudity scores roughly
    /// 0.55 to 0.72, so past about 0.70 this starts deleting true detections
    /// rather than false ones.
    pub threshold: f64,

    /// How many detections in a row a run needs before it is covered.
    ///
    /// The detector samples at 3 fps, so a run of 1 is a single frame: one
    /// twelfth of a second of evidence that still produces `pad_before +
    /// pad_after` seconds of covering. Isolated single-frame hits are the
    /// signature of a false positive, typically bare skin in a fight or a
    /// dim scene.
    ///
    /// Defaults to 1, which covers everything the detector saw. Raising it to
    /// 2 is the single most effective way to stop unrelated scenes being
    /// covered, at the cost of missing genuine nudity that is only ever
    /// visible for one sampled frame.
    pub min_run: usize,
}

impl Default for WindowParams {
    fn default() -> Self {
        // livescan.py: PAD_BEFORE, PAD_AFTER, GAP_MERGE, WINDOW_MAX, MARGIN,
        // MAX_WINDOWS. Changing any of these changes what viewers see, so they
        // are pinned to the values the golden fixtures were generated with.
        //
        // max_windows is the one value NOT inherited from Python. livescan.py
        // used 380 and pfplay.py used 400, but ffmpeg's expression evaluator
        // gives up at 99 levels of recursion, and this graph nests one level
        // per window. The old cap was never hit in practice because pureframe
        // plans produced around 66 windows for a feature film; the linear
        // scanner produces 319 for the same movie, and ffmpeg rejects that
        // outright with "Missing ')' or too many args".
        //
        // A rejected graph is the worst possible failure for this app: mpv
        // would drop the filter and play the movie uncovered. 90 leaves
        // headroom under the measured limit of 98. See docs/expression-limit.md.
        Self {
            pad_before: 5.0,
            pad_after: 10.0,
            gap_merge: 1.5,
            window_max: 2.0,
            margin: 0.08,
            max_windows: 90,
            threshold: 0.55,
            // 1 keeps the behaviour the golden fixtures were built against and
            // covers everything the detector reported. Deliberately not 2:
            // reducing coverage is the viewer's decision to make, not a
            // default this project changes on their behalf.
            min_run: 1,
        }
    }
}

/// One span of the movie with one rectangle covered.
#[derive(Debug, Clone, PartialEq)]
pub struct CensorWindow {
    pub start: f64,
    pub end: f64,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    /// What put this window here.
    pub reason: Reason,
}

/// Why a span is covered, in terms a viewer can weigh up.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    /// The label the run's strongest detection carried. Empty for plans
    /// written before reasons were recorded.
    pub label: String,
    /// That detection's confidence.
    pub peak_score: f64,
    /// Sampled frames in the run behind this window. One means a single
    /// glimpse, which is worth being sceptical about.
    pub detections: usize,
    /// When the run's first detection actually was, which is `pad_before`
    /// seconds after this window starts.
    pub first_seen: f64,
}

impl Reason {
    /// A run seen in only one sampled frame. Most false positives look like
    /// this, and so do genuine flashes, which is why it is surfaced rather
    /// than silently dropped.
    pub fn is_single_frame(&self) -> bool {
        self.detections == 1
    }
}

impl CensorWindow {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t <= self.end
    }
}

pub fn build_windows(plan: &Plan, p: &WindowParams) -> Vec<CensorWindow> {
    let dets = &plan.detections;
    if dets.is_empty() {
        return Vec::new();
    }

    let src_w = plan.source.width;
    let src_h = plan.source.height;
    let duration = plan.source.duration;

    // Only detections the model was confident enough about. Filtering here
    // rather than at scan time is what lets the threshold be re-derived from a
    // plan already on disk.
    let confident: Vec<usize> = dets
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            d.boxes
                .iter()
                // A plan from before reasons were recorded has no score, and
                // was already filtered when it was written; keep those.
                .any(|b| b.label.is_empty() || b.score >= p.threshold)
        })
        .map(|(i, _)| i)
        .collect();
    if confident.is_empty() {
        return Vec::new();
    }

    // Group detections into runs, bridging gaps up to `gap_merge`.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for &i in &confident {
        if let Some(&last) = cur.last() {
            if dets[i].t - dets[last].t > p.gap_merge {
                runs.push(std::mem::take(&mut cur));
            }
        }
        cur.push(i);
    }
    if !cur.is_empty() {
        runs.push(cur);
    }

    // Drop runs with too little evidence behind them. A run of one is a single
    // sampled frame that would otherwise produce fifteen seconds of covering.
    if p.min_run > 1 {
        runs.retain(|r| r.len() >= p.min_run);
        if runs.is_empty() {
            return Vec::new();
        }
    }

    // `int(src * margin)` in Python truncates toward zero; so does `as i64`.
    let mx = (src_w as f64 * p.margin) as i64;
    let my = (src_h as f64 * p.margin) as i64;

    let mut window_len = p.window_max;
    loop {
        let mut wins: Vec<CensorWindow> = Vec::new();

        for run in &runs {
            let first_t = dets[run[0]].t;
            let last_t = dets[*run.last().unwrap()].t;
            let run_s = (first_t - p.pad_before).max(0.0);
            let run_e = (last_t + p.pad_after).min(duration);
            let reason = describe(dets, run, p.threshold, first_t);

            let mut s = run_s;
            while s < run_e {
                let e = (s + window_len).min(run_e);

                // Boxes from detections that fall inside this window, widened
                // by gap_merge on each side because the sampler is sparse.
                let mut acc: Option<[i64; 4]> = None;
                for &i in run {
                    let t = dets[i].t;
                    if t >= s - p.gap_merge && t <= e + p.gap_merge {
                        for b in &dets[i].boxes {
                            if b.label.is_empty() || b.score >= p.threshold {
                                acc = Some(union(acc, &b.bounds));
                            }
                        }
                    }
                }

                // Hold-tail windows sit past the last detection and match
                // nothing. Cover the run's entire detected area instead of
                // letting the cover disappear early.
                if acc.is_none() {
                    for &i in run {
                        for b in &dets[i].boxes {
                            if b.label.is_empty() || b.score >= p.threshold {
                                acc = Some(union(acc, &b.bounds));
                            }
                        }
                    }
                }

                if let Some([ax1, ay1, ax2, ay2]) = acc {
                    let x1 = floor_div((ax1 - mx).max(0), 2) * 2;
                    let y1 = floor_div((ay1 - my).max(0), 2) * 2;
                    let x2 = (ax2 + mx).min(src_w);
                    let y2 = (ay2 + my).min(src_h);
                    wins.push(CensorWindow {
                        start: round3(s),
                        end: round3(e),
                        x: x1,
                        y: y1,
                        w: (floor_div(x2 - x1, 2) * 2).max(4),
                        h: (floor_div(y2 - y1, 2) * 2).max(4),
                        reason: reason.clone(),
                    });
                }

                s = e;
            }
        }

        if wins.len() <= p.max_windows || window_len > duration {
            return wins;
        }
        // Too many windows for the filtergraph budget: merge harder. This
        // covers strictly more of the picture, never less.
        window_len *= 2.0;
    }
}

/// Summarise a run: the strongest label in it, and how much evidence there was.
fn describe(dets: &[Detection], run: &[usize], threshold: f64, first_t: f64) -> Reason {
    let mut label = String::new();
    let mut peak = 0.0f64;
    for &i in run {
        for b in &dets[i].boxes {
            if !b.label.is_empty() && b.score >= threshold && b.score > peak {
                peak = b.score;
                label = b.label.clone();
            }
        }
    }
    Reason {
        label,
        peak_score: peak,
        detections: run.len(),
        first_seen: round3(first_t),
    }
}

fn union(acc: Option<[i64; 4]>, b: &[i64; 4]) -> [i64; 4] {
    match acc {
        None => *b,
        Some([x1, y1, x2, y2]) => [x1.min(b[0]), y1.min(b[1]), x2.max(b[2]), y2.max(b[3])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{DetBox, Detector, Source};

    fn plan_with(times: &[(f64, f64)]) -> Plan {
        Plan {
            schema_version: crate::plan::SCHEMA_VERSION,
            generator: String::new(),
            created_at: String::new(),
            source: Source {
                name: "t.mp4".into(),
                size_bytes: 0,
                duration: 600.0,
                fps: 24.0,
                width: 1280,
                height: 720,
            },
            detector: Detector {
                sample_fps: 3.0,
                threshold: 0.55,
                detect_width: 1280,
                detect_height: 720,
            },
            complete: true,
            detections: times
                .iter()
                .map(|(t, score)| Detection {
                    t: *t,
                    boxes: vec![DetBox {
                        bounds: [100, 100, 300, 300],
                        label: "FEMALE_BREAST_EXPOSED".into(),
                        score: *score,
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn a_single_frame_run_still_produces_the_full_lead_and_hold() {
        // The reason a stray detection is so expensive: one sampled frame of
        // evidence buys fifteen seconds of covering.
        let plan = plan_with(&[(100.0, 0.6)]);
        let wins = build_windows(&plan, &WindowParams::default());
        assert!(!wins.is_empty());
        let covered: f64 = wins.last().unwrap().end - wins[0].start;
        assert!(
            (covered - 15.0).abs() < 0.01,
            "expected 5s lead + 10s hold, got {covered}"
        );
    }

    #[test]
    fn min_run_drops_isolated_detections_and_keeps_sustained_ones() {
        // One stray frame at 100s, a sustained run at 300s.
        let plan = plan_with(&[(100.0, 0.6), (300.0, 0.6), (300.333, 0.6), (300.667, 0.6)]);

        let lenient = build_windows(&plan, &WindowParams::default());
        let strict = build_windows(
            &plan,
            &WindowParams {
                min_run: 2,
                ..WindowParams::default()
            },
        );

        assert!(
            lenient.iter().any(|w| w.contains(100.0)),
            "the default must still cover a single frame"
        );
        assert!(
            !strict.iter().any(|w| w.contains(100.0)),
            "min_run 2 must drop the isolated frame"
        );
        assert!(
            strict.iter().any(|w| w.contains(300.5)),
            "min_run 2 must keep the sustained run"
        );
    }

    #[test]
    fn the_threshold_re_derives_from_a_plan_without_rescanning() {
        // The point of storing raw detections: moving the threshold changes
        // coverage immediately.
        let plan = plan_with(&[(100.0, 0.58), (300.0, 0.80)]);

        let low = build_windows(&plan, &WindowParams::default());
        let high = build_windows(
            &plan,
            &WindowParams {
                threshold: 0.70,
                ..WindowParams::default()
            },
        );

        assert!(low.iter().any(|w| w.contains(100.0)));
        assert!(!high.iter().any(|w| w.contains(100.0)));
        assert!(high.iter().any(|w| w.contains(300.0)));
    }

    #[test]
    fn windows_carry_the_reason_they_exist() {
        let plan = plan_with(&[(300.0, 0.61), (300.333, 0.74)]);
        let wins = build_windows(&plan, &WindowParams::default());
        let r = &wins[0].reason;
        assert_eq!(r.label, "FEMALE_BREAST_EXPOSED");
        // The strongest detection in the run, not the first.
        assert!((r.peak_score - 0.74).abs() < 1e-9);
        assert_eq!(r.detections, 2);
        assert!(!r.is_single_frame());
        // Covering starts before the detection actually happened.
        assert!(r.first_seen > wins[0].start);
    }

    #[test]
    fn a_plan_written_before_reasons_existed_still_builds_windows() {
        // v1 boxes are bare arrays with no label or score. They must not be
        // filtered out by a threshold they never carried.
        let json = r#"{
            "schema_version": 1,
            "source": {"name":"t.mp4","size_bytes":0,"duration":600.0,"fps":24.0,"width":1280,"height":720},
            "detector": {"sample_fps":3.0,"threshold":0.55,"detect_width":1280,"detect_height":720},
            "complete": true,
            "detections": [{"t": 300.0, "boxes": [[100,100,300,300]]}]
        }"#;
        let plan: Plan = serde_json::from_str(json).expect("v1 plan should load");
        assert!(plan.validate().is_ok());

        let wins = build_windows(&plan, &WindowParams::default());
        assert!(
            !wins.is_empty(),
            "a v1 plan must still cover its detections"
        );
        assert_eq!(wins[0].reason.label, "", "v1 carries no reason to report");
    }
}
