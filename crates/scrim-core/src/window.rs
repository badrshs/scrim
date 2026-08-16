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

use crate::plan::Plan;
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
    pub max_windows: usize,
}

impl Default for WindowParams {
    fn default() -> Self {
        // livescan.py: PAD_BEFORE, PAD_AFTER, GAP_MERGE, WINDOW_MAX, MARGIN,
        // MAX_WINDOWS. Changing any of these changes what viewers see, so they
        // are pinned to the values the golden fixtures were generated with.
        Self {
            pad_before: 5.0,
            pad_after: 10.0,
            gap_merge: 1.5,
            window_max: 2.0,
            margin: 0.08,
            max_windows: 380,
        }
    }
}

/// One span of the movie with one rectangle covered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CensorWindow {
    pub start: f64,
    pub end: f64,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl CensorWindow {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
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

    // Group detections into runs, bridging gaps up to `gap_merge`.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (i, d) in dets.iter().enumerate() {
        if let Some(&last) = cur.last() {
            if d.t - dets[last].t > p.gap_merge {
                runs.push(std::mem::take(&mut cur));
            }
        }
        cur.push(i);
    }
    if !cur.is_empty() {
        runs.push(cur);
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
                            acc = Some(union(acc, b));
                        }
                    }
                }

                // Hold-tail windows sit past the last detection and match
                // nothing. Cover the run's entire detected area instead of
                // letting the cover disappear early.
                if acc.is_none() {
                    for &i in run {
                        for b in &dets[i].boxes {
                            acc = Some(union(acc, b));
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

fn union(acc: Option<[i64; 4]>, b: &[i64; 4]) -> [i64; 4] {
    match acc {
        None => *b,
        Some([x1, y1, x2, y2]) => [x1.min(b[0]), y1.min(b[1]), x2.max(b[2]), y2.max(b[3])],
    }
}
