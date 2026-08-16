//! Scrim's censoring engine.
//!
//! This crate decides what gets covered and produces the ffmpeg filtergraph
//! that covers it. It touches no files, no video, and no window, so all of it
//! is testable from a JSON fixture.
//!
//! The governing rule of the whole project lives here: **an uncensored frame
//! must never reach the screen or a cast device.** Where this code has to
//! choose, it over-covers. A bigger black box is a cosmetic complaint; a
//! missed frame is the one bug that matters.

#![forbid(unsafe_code)]

mod graph;
mod plan;
mod util;
mod window;

pub use graph::{build_graph, CensorStyle};
pub use plan::{
    DetBox, Detection, Detector, Plan, PlanError, Source, MIN_SCHEMA_VERSION, SCHEMA_VERSION,
};
pub use window::{build_windows, CensorWindow, Reason, WindowParams};

/// Everything the player needs to start a movie, derived from a plan.
#[derive(Debug, Clone)]
pub struct Coverage {
    pub windows: Vec<CensorWindow>,
    /// Spans covered edge to edge rather than by a moving box.
    pub full_runs: Vec<(f64, f64)>,
}

impl Coverage {
    pub fn from_plan(plan: &Plan, params: &WindowParams) -> Self {
        Self {
            windows: build_windows(plan, params),
            full_runs: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.full_runs.is_empty()
    }

    /// Seconds of playback that carry a cover. Shown in the library card so a
    /// viewer knows what they are in for before pressing play.
    pub fn covered_seconds(&self) -> f64 {
        self.windows.iter().map(|w| w.duration()).sum::<f64>()
            + self
                .full_runs
                .iter()
                .map(|(s, e)| (e - s).max(0.0))
                .sum::<f64>()
    }

    pub fn graph(&self, fw: i64, fh: i64, style: CensorStyle) -> String {
        build_graph(&self.full_runs, &self.windows, fw, fh, style)
    }

    /// Why the picture is covered at this moment, if it is.
    pub fn reason_at(&self, t: f64) -> Option<&Reason> {
        // Windows can overlap where a hold runs into a fresh detection. The
        // graph gives the later one priority, so this does too.
        self.windows
            .iter()
            .filter(|w| w.contains(t))
            .max_by(|a, b| {
                a.start
                    .partial_cmp(&b.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|w| &w.reason)
    }
}

/// Merge overlapping or touching spans. Ported from `pfplay.py::merge_overlaps`.
pub fn merge_overlaps(runs: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if runs.is_empty() {
        return Vec::new();
    }
    let mut sorted = runs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut merged: Vec<(f64, f64)> = vec![sorted[0]];
    for &(s, e) in &sorted[1..] {
        let last = merged.last_mut().unwrap();
        if s <= last.1 {
            last.1 = last.1.max(e);
        } else {
            merged.push((s, e));
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overlaps_joins_touching_and_overlapping_spans() {
        assert_eq!(
            merge_overlaps(&[(0.0, 5.0), (4.0, 9.0), (20.0, 22.0)]),
            vec![(0.0, 9.0), (20.0, 22.0)]
        );
        // Touching exactly is still one span.
        assert_eq!(merge_overlaps(&[(0.0, 5.0), (5.0, 8.0)]), vec![(0.0, 8.0)]);
        // A span fully inside another does not shorten it.
        assert_eq!(
            merge_overlaps(&[(0.0, 10.0), (2.0, 4.0)]),
            vec![(0.0, 10.0)]
        );
        assert!(merge_overlaps(&[]).is_empty());
    }

    #[test]
    fn blur_styles_are_see_through_and_boxes_are_not() {
        assert!(CensorStyle::BlackBox.is_opaque());
        assert!(CensorStyle::WhiteBox.is_opaque());
        assert!(!CensorStyle::BlurStrong.is_opaque());
        assert!(!CensorStyle::BlurLight.is_opaque());
    }
}
