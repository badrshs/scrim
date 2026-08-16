//! Building the ffmpeg filtergraph that mpv applies during playback.
//!
//! Ported from `pfplay.py::build_graph`. The hard constraint, learned the hard
//! way and measured, is that the graph must contain a **constant** number of
//! filters no matter how much of the movie is flagged. One overlay chain per
//! window (48 chains on a real film) grinds mpv's filter bridge to a halt; a
//! single chain whose crop and overlay coordinates move via per-frame time
//! expressions runs at 16-20x realtime.
//!
//! So the shape is always the same:
//!
//!   split=2[m][t];
//!   [t]crop=<fixed size at a moving x,y>,<censor>[b];
//!   [m][b]overlay=<same moving x,y>:enable='<union of window spans>'
//!
//! plus one optional full-frame censor filter gated to the full-frame spans.

use crate::util::floor_div;
use crate::window::CensorWindow;

/// The five entries of the Censor picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CensorStyle {
    BlackBox,
    WhiteBox,
    BlurStrong,
    BlurMedium,
    BlurLight,
}

impl CensorStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::BlackBox => "Black box",
            Self::WhiteBox => "White box",
            Self::BlurStrong => "Blur strong",
            Self::BlurMedium => "Blur medium",
            Self::BlurLight => "Blur light",
        }
    }

    /// Blur styles are see-through by design; boxes are not. Worth being able
    /// to ask, because the UI says so out loud in the picker.
    pub fn is_opaque(self) -> bool {
        matches!(self, Self::BlackBox | Self::WhiteBox)
    }

    /// The ffmpeg filter that covers a region, or a whole frame.
    ///
    /// The blur radius is clamped by expression so that a region smaller than
    /// the radius cannot make boxblur fail and drop the filter entirely.
    fn filter(self) -> String {
        match self {
            Self::BlackBox => "drawbox=x=0:y=0:w=iw:h=ih:color=black:t=fill".into(),
            Self::WhiteBox => "drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill".into(),
            // pfplay.py BLUR_STRENGTHS: luma radius/power, chroma radius/power
            Self::BlurLight => blur(8, 1, 4, 1),
            Self::BlurMedium => blur(16, 2, 8, 2),
            Self::BlurStrong => blur(28, 2, 14, 2),
        }
    }
}

fn blur(lr: i32, lp: i32, cr: i32, cp: i32) -> String {
    format!(
        "boxblur=luma_radius='min({lr},(min(w,h)-1)/2)':luma_power={lp}\
         :chroma_radius='min({cr},(min(cw,ch)-1)/2)':chroma_power={cp}"
    )
}

/// `between(t,a,b)+between(t,c,d)+...` - the gate for a set of spans.
fn enable_expr(runs: &[(f64, f64)]) -> String {
    runs.iter()
        .map(|(s, e)| format!("between(t,{s:.3},{e:.3})"))
        .collect::<Vec<_>>()
        .join("+")
}

/// Nested `if(between(t,..),value,else)` selecting a coordinate per window.
///
/// Built forward so that later entries wrap earlier ones, which means that
/// when two windows overlap - a ten second hold running into a fresh
/// detection - the later-starting window wins. The newest detection's position
/// takes precedence over a stale held one.
fn piecewise(moved: &[(f64, f64, i64, i64)], idx: usize, default: i64) -> String {
    let mut expr = default.to_string();
    for (s, e, nx, ny) in moved {
        let v = if idx == 0 { nx } else { ny };
        expr = format!("if(between(t,{s:.3},{e:.3}),{v},{expr})");
    }
    expr
}

/// Build the complete `vf=lavfi=[...]` body.
///
/// Returns an empty string when there is nothing to cover, which the player
/// reads as "play this file with no filter at all".
pub fn build_graph(
    full_runs: &[(f64, f64)],
    windows: &[CensorWindow],
    fw: i64,
    fh: i64,
    style: CensorStyle,
) -> String {
    let censor = style.filter();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();

    if !windows.is_empty() {
        // One crop size big enough for every window, then each window is
        // recentred inside it. Keeping the size fixed is what keeps the graph
        // a constant number of filters.
        let bw = fw.min(windows.iter().map(|w| w.w).max().unwrap_or(0));
        let bh = fh.min(windows.iter().map(|w| w.h).max().unwrap_or(0));

        let moved: Vec<(f64, f64, i64, i64)> = windows
            .iter()
            .map(|win| {
                // NOTE: `win.w - bw` is normally negative, so this needs
                // Python's floor division. See util::floor_div.
                let nx = floor_div(
                    (win.x + floor_div(win.w - bw, 2)).max(0).min(fw - bw),
                    2,
                ) * 2;
                let ny = floor_div(
                    (win.y + floor_div(win.h - bh, 2)).max(0).min(fh - bh),
                    2,
                ) * 2;
                (win.start, win.end, nx, ny)
            })
            .collect();

        let xe = piecewise(&moved, 0, 0);
        let ye = piecewise(&moved, 1, 0);
        let spans: Vec<(f64, f64)> = moved.iter().map(|(s, e, _, _)| (*s, *e)).collect();
        let gate = format!("enable='{}'", enable_expr(&spans));

        parts.push(format!("{cur}split=2[m][t]"));
        parts.push(format!(
            "[t]crop=w={bw}:h={bh}:x='{xe}':y='{ye}',{censor}[b]"
        ));
        parts.push(format!(
            "[m][b]overlay=x='{xe}':y='{ye}':eval=frame:{gate}[o]"
        ));
        cur = "[o]".to_string();
    }

    if !full_runs.is_empty() {
        parts.push(format!(
            "{cur}{censor}:enable='{}'",
            enable_expr(full_runs)
        ));
    } else if !windows.is_empty() {
        // Strip the trailing label so the graph's output pad is unnamed,
        // which is what mpv's lavfi wrapper expects.
        let last = parts.len() - 1;
        let keep = parts[last].len() - cur.len();
        parts[last].truncate(keep);
    }

    parts.join(";")
}
