//! Keeping live playback behind the detection frontier.
//!
//! In live mode the movie is being watched and analysed at the same time. The
//! scanner runs perhaps fifteen times faster than playback, so it normally
//! stays well ahead, but "normally" is not a guarantee and the viewer can seek
//! anywhere they like.
//!
//! The frontier is how far detection has looked. Past it, nothing is known,
//! and showing an unexamined frame is the one thing this project must never
//! do. So playback is fenced:
//!
//! ```text
//!   .............................|--- 15s ---|-5s-|
//!   safe to watch                 pause here      frontier
//!                                                 |
//!   a seek past this point snaps back to frontier - 30s
//! ```
//!
//! The thresholds are inherited from `player_app.py::_tick_live`. The decision
//! is a pure function so every branch can be tested without a movie, a
//! scanner, or a window.

/// How close to the frontier a seek may land before it is refused.
const SEEK_LIMIT: f64 = 5.0;
/// Where a refused seek is sent instead.
const SNAP_BACK: f64 = 30.0;
/// Playback pauses this far short of the frontier.
const PAUSE_LIMIT: f64 = 15.0;
/// And resumes once the scanner has opened up this much of a lead.
const RESUME_GAP: f64 = 45.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FenceAction {
    /// Carry on.
    None,
    /// Playback is too close to the frontier; send it back here.
    SnapBack { to: f64 },
    /// Hold until detection gets further ahead.
    Pause,
    /// Detection has enough of a lead; carry on.
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FenceInput {
    pub position: f64,
    pub frontier: f64,
    pub scan_complete: bool,
    /// True when the fence, rather than the viewer, did the pausing.
    pub paused_by_fence: bool,
}

pub fn decide(input: FenceInput) -> FenceAction {
    // A finished scan has looked at the whole movie, so there is no frontier
    // left to defend and the viewer gets their seek bar back.
    if input.scan_complete {
        return if input.paused_by_fence {
            FenceAction::Resume
        } else {
            FenceAction::None
        };
    }

    if input.position > input.frontier - SEEK_LIMIT {
        return FenceAction::SnapBack {
            to: (input.frontier - SNAP_BACK).max(0.0),
        };
    }

    if input.position > input.frontier - PAUSE_LIMIT {
        return if input.paused_by_fence {
            FenceAction::None // already holding
        } else {
            FenceAction::Pause
        };
    }

    if input.paused_by_fence && input.frontier - input.position > RESUME_GAP {
        return FenceAction::Resume;
    }

    FenceAction::None
}

/// Seconds until playback is expected to resume, for the dialog's countdown.
///
/// An estimate only: it assumes the scanner keeps its current pace, and the
/// fence resumes on the real gap regardless of what this says.
pub fn resume_estimate(input: FenceInput, speed: f64) -> Option<f64> {
    if !input.paused_by_fence || input.scan_complete {
        return None;
    }
    let needed = RESUME_GAP - (input.frontier - input.position);
    if needed <= 0.0 {
        return Some(0.0);
    }
    // While paused the gap closes at the scanner's full rate.
    if speed <= 0.01 {
        return None;
    }
    Some(needed / speed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(position: f64, frontier: f64) -> FenceInput {
        FenceInput {
            position,
            frontier,
            scan_complete: false,
            paused_by_fence: false,
        }
    }

    #[test]
    fn playing_well_behind_the_frontier_is_left_alone() {
        assert_eq!(decide(at(100.0, 600.0)), FenceAction::None);
    }

    #[test]
    fn seeking_past_the_frontier_snaps_back_thirty_seconds_behind_it() {
        // The viewer dragged to 700s but only 600s has been looked at.
        assert_eq!(
            decide(at(700.0, 600.0)),
            FenceAction::SnapBack { to: 570.0 }
        );
        // Just inside the five second limit is still refused.
        assert_eq!(
            decide(at(596.0, 600.0)),
            FenceAction::SnapBack { to: 570.0 }
        );
    }

    #[test]
    fn snapping_back_never_goes_before_the_start_of_the_movie() {
        // Early in a live scan, frontier - 30 is negative.
        assert_eq!(decide(at(18.0, 20.0)), FenceAction::SnapBack { to: 0.0 });
    }

    #[test]
    fn playback_pauses_fifteen_seconds_short_of_the_frontier() {
        assert_eq!(decide(at(590.0, 600.0)), FenceAction::Pause);
    }

    #[test]
    fn a_fence_pause_is_not_repeated_while_it_holds() {
        let held = FenceInput {
            paused_by_fence: true,
            ..at(590.0, 600.0)
        };
        assert_eq!(decide(held), FenceAction::None);
    }

    #[test]
    fn playback_resumes_once_the_scanner_is_forty_five_seconds_ahead() {
        let held = FenceInput {
            paused_by_fence: true,
            ..at(590.0, 700.0)
        };
        assert_eq!(decide(held), FenceAction::Resume);

        // Not yet: only a forty second lead.
        let still_held = FenceInput {
            paused_by_fence: true,
            ..at(590.0, 630.0)
        };
        assert_eq!(decide(still_held), FenceAction::None);
    }

    #[test]
    fn every_fence_lifts_when_the_scan_finishes() {
        // Position past the frontier would normally snap back; a complete scan
        // means the frontier is meaningless and seeking is free again.
        let done = FenceInput {
            scan_complete: true,
            ..at(3000.0, 600.0)
        };
        assert_eq!(decide(done), FenceAction::None);

        let held = FenceInput {
            scan_complete: true,
            paused_by_fence: true,
            ..at(590.0, 600.0)
        };
        assert_eq!(decide(held), FenceAction::Resume);
    }

    #[test]
    fn the_pause_and_snap_thresholds_do_not_overlap() {
        // Between 15s and 5s of the frontier we pause; inside 5s we snap. A
        // position cannot be eligible for both, or playback would oscillate.
        for pos in [586.0, 590.0, 594.0, 594.9] {
            assert_eq!(decide(at(pos, 600.0)), FenceAction::Pause, "at {pos}");
        }
        for pos in [595.1, 599.0, 620.0] {
            assert!(
                matches!(decide(at(pos, 600.0)), FenceAction::SnapBack { .. }),
                "at {pos}"
            );
        }
    }

    #[test]
    fn snapping_back_lands_somewhere_that_will_not_immediately_pause() {
        // frontier - 30 must be comfortably behind the pause line at
        // frontier - 15, otherwise a refused seek would pause instantly and
        // the viewer would be stuck.
        let FenceAction::SnapBack { to } = decide(at(700.0, 600.0)) else {
            panic!("expected a snap back");
        };
        assert_eq!(decide(at(to, 600.0)), FenceAction::None);
    }

    #[test]
    fn resume_countdown_reflects_the_scanners_pace() {
        let held = FenceInput {
            paused_by_fence: true,
            ..at(590.0, 600.0)
        };
        // 35 more seconds of lead needed, at 7x realtime, is five seconds.
        assert_eq!(resume_estimate(held, 7.0), Some(5.0));
        // A stalled scanner has no answer rather than a wrong one.
        assert_eq!(resume_estimate(held, 0.0), None);
        // Nothing to report when playback was not fenced.
        assert_eq!(resume_estimate(at(100.0, 600.0), 7.0), None);
    }
}
