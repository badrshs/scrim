//! Golden tests against the Python engine this crate replaces.
//!
//! The fixtures in `tests/fixtures/` were produced by `tools/export_fixtures.py`
//! running the original `livescan.py` + `pfplay.py` over the two real test
//! movies. Every window and every filtergraph string here is what the working
//! Python actually emitted.
//!
//! This is the contract for the rewrite: the code may look however it likes,
//! but what ends up covered on screen must not move by a single pixel or a
//! single millisecond. A byte-identical 55,000 character filtergraph is a very
//! unforgiving thing to match by accident.

use scrim_core::{build_graph, build_windows, CensorStyle, Plan, WindowParams};

use std::collections::BTreeMap;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()))
}

/// The `<stem>.graphs.json` side of a fixture pair.
#[derive(serde::Deserialize)]
struct Golden {
    window_count: usize,
    /// (start, end, x, y, w, h)
    windows: Vec<(f64, f64, i64, i64, i64, i64)>,
    frame_width: i64,
    frame_height: i64,
    graphs: BTreeMap<String, String>,
}

fn style_for(key: &str) -> CensorStyle {
    match key {
        "black_box" => CensorStyle::BlackBox,
        "white_box" => CensorStyle::WhiteBox,
        "blur_strong" => CensorStyle::BlurStrong,
        "blur_medium" => CensorStyle::BlurMedium,
        "blur_light" => CensorStyle::BlurLight,
        other => panic!("unknown censor style in fixture: {other}"),
    }
}

/// Load a fixture pair, rebuild windows from the raw detections, and check both
/// the windows and every censor style's filtergraph against the Python output.
fn assert_matches_python(stem: &str) {
    let plan: Plan = serde_json::from_str(&fixture(&format!("{stem}.plan.json")))
        .expect("plan fixture should parse as schema v1");
    let golden: Golden = serde_json::from_str(&fixture(&format!("{stem}.graphs.json")))
        .expect("graphs fixture should parse");

    assert!(
        plan.schema_version >= scrim_core::MIN_SCHEMA_VERSION
            && plan.schema_version <= scrim_core::SCHEMA_VERSION,
        "{stem}: fixture schema {} is outside the supported range",
        plan.schema_version
    );
    assert_eq!(plan.source.width, golden.frame_width, "{stem}: frame width");
    assert_eq!(
        plan.source.height, golden.frame_height,
        "{stem}: frame height"
    );

    let windows = build_windows(&plan, &WindowParams::default());

    assert_eq!(
        windows.len(),
        golden.window_count,
        "{stem}: window count drifted from the Python engine"
    );

    for (i, (got, want)) in windows.iter().zip(&golden.windows).enumerate() {
        assert_eq!(
            (got.start, got.end, got.x, got.y, got.w, got.h),
            *want,
            "{stem}: window {i} differs from the Python engine"
        );
    }

    for (key, want) in &golden.graphs {
        let got = build_graph(
            &[],
            &windows,
            plan.source.width,
            plan.source.height,
            style_for(key),
        );
        assert_eq!(
            got.len(),
            want.len(),
            "{stem}/{key}: filtergraph length differs ({} vs {} chars)",
            got.len(),
            want.len()
        );
        assert!(
            got == *want,
            "{stem}/{key}: filtergraph differs from the Python engine\n\
             first difference at byte {}",
            got.bytes()
                .zip(want.bytes())
                .position(|(a, b)| a != b)
                .map(|p| p.to_string())
                .unwrap_or_else(|| "n/a".into())
        );
    }
}

#[test]
fn abc_matches_python_engine() {
    // 61 minutes, 129 detection frames, 319 windows, ~606s covered.
    // The filtergraphs here are roughly 56,000 characters each.
    assert_matches_python("abc");
}

#[test]
fn sample_matches_python_engine() {
    // 2 minutes with nothing flagged. The empty case matters just as much:
    // an empty graph means "play the file untouched", and if this crate ever
    // emitted a malformed empty graph mpv would refuse the file entirely.
    assert_matches_python("sample");
}

#[test]
fn the_recorded_scan_carries_reasons_and_reaches_below_the_cutoff() {
    // Plans record down to a floor beneath the covering threshold so the
    // threshold can be moved later without rescanning. If the fixture only
    // held detections at or above 0.55, the threshold tests would be
    // exercising a range that never occurs in practice.
    let plan: Plan = serde_json::from_str(&fixture("abc.plan.json")).unwrap();

    let labelled = plan
        .detections
        .iter()
        .flat_map(|d| &d.boxes)
        .filter(|b| !b.label.is_empty())
        .count();
    assert!(labelled > 0, "schema v2 fixtures must carry labels");

    let below = plan
        .detections
        .iter()
        .flat_map(|d| &d.boxes)
        .filter(|b| b.score < 0.55)
        .count();
    assert!(
        below > 0,
        "the fixture should include sub-threshold detections, or raising and \
         lowering the cutoff cannot be tested against real data"
    );
}

#[test]
fn a_clean_movie_produces_no_filtergraph() {
    let plan: Plan = serde_json::from_str(&fixture("sample.plan.json")).unwrap();
    let windows = build_windows(&plan, &WindowParams::default());
    assert!(windows.is_empty(), "sample.mp4 has no detections");

    let graph = build_graph(
        &[],
        &windows,
        plan.source.width,
        plan.source.height,
        CensorStyle::BlackBox,
    );
    assert_eq!(
        graph, "",
        "no windows and no full runs means no filter at all"
    );
}
