//! Does ffmpeg actually accept the filtergraph we build?
//!
//! The golden tests prove the graph string matches the reference
//! implementation. They cannot prove the string is *valid*, and it turned out
//! not to be: ffmpeg's expression evaluator abandons parsing at 99 levels of
//! recursion, and the graph nests one level per censor window. A real scan of
//! a feature film produced 319 windows, and ffmpeg rejected the whole thing.
//!
//! That is the most dangerous failure this project has. mpv responds to a
//! broken filter by playing the movie without it, so a graph that fails to
//! parse means an uncensored movie plays start to finish. Fail-closed demands
//! that we prove the graph parses, not assume it.
//!
//! These tests need `resources/ffmpeg.exe`. Fetch it with
//! `tools/fetch-resources.ps1`; without it they skip rather than fail, so the
//! suite still runs on a bare checkout.

use scrim_core::{build_graph, build_windows, CensorStyle, Coverage, Plan, WindowParams};

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn ffmpeg() -> Option<PathBuf> {
    let bundled = repo_root().join("resources/ffmpeg.exe");
    if bundled.exists() {
        return Some(bundled);
    }
    None
}

fn load_plan(stem: &str) -> Plan {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{stem}.plan.json"));
    let text = std::fs::read_to_string(&path).expect("fixture plan");
    serde_json::from_str(&text).expect("fixture plan parses")
}

/// Hand a filtergraph to ffmpeg and decode a single synthetic frame through it.
///
/// The graph goes in through a file because it can run to tens of thousands of
/// characters, well past the Windows command line limit.
fn ffmpeg_accepts(graph: &str, width: i64, height: i64) -> Result<(), String> {
    let Some(exe) = ffmpeg() else {
        return Ok(()); // skipped; the caller reports it
    };
    if graph.is_empty() {
        return Ok(());
    }

    // Unique per call. Cargo runs tests in threads, and a path keyed only on
    // the process id had two tests writing the same file, so one test's ffmpeg
    // read the other test's graph and failed for a reason that had nothing to
    // do with what it was checking.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let script = std::env::temp_dir().join(format!(
        "scrim-graph-test-{}-{}.txt",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&script, graph).map_err(|e| format!("writing filter script: {e}"))?;

    let out = Command::new(&exe)
        .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s={width}x{height}:d=0.1"))
        // `-/filter:v <file>` is the current spelling; `-filter_script` was
        // removed from ffmpeg and silently fails as an unknown option.
        .arg("-/filter:v")
        .arg(&script)
        .args(["-frames:v", "1", "-f", "null", "-"])
        .output()
        .map_err(|e| format!("running ffmpeg: {e}"))?;

    let _ = std::fs::remove_file(&script);

    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "ffmpeg rejected a {} character graph:\n{}",
            graph.len(),
            err.lines().take(4).collect::<Vec<_>>().join("\n")
        ))
    }
}

const ALL_STYLES: [CensorStyle; 5] = [
    CensorStyle::BlackBox,
    CensorStyle::WhiteBox,
    CensorStyle::BlurStrong,
    CensorStyle::BlurMedium,
    CensorStyle::BlurLight,
];

#[test]
fn every_censor_style_produces_a_graph_ffmpeg_accepts() {
    if ffmpeg().is_none() {
        eprintln!("skipping: resources/ffmpeg.exe not present, run tools/fetch-resources.ps1");
        return;
    }

    for stem in ["abc", "sample"] {
        let plan = load_plan(stem);
        let coverage = Coverage::from_plan(&plan, &WindowParams::default());

        for style in ALL_STYLES {
            let graph = coverage.graph(plan.source.width, plan.source.height, style);
            ffmpeg_accepts(&graph, plan.source.width, plan.source.height)
                .unwrap_or_else(|e| panic!("{stem} / {}: {e}", style.label()));
        }
    }
}

#[test]
fn the_default_window_cap_stays_inside_ffmpegs_recursion_budget() {
    let plan = load_plan("abc");
    let windows = build_windows(&plan, &WindowParams::default());

    assert!(
        windows.len() <= 90,
        "the default cap must hold: got {} windows",
        windows.len()
    );
    // The measured cliff is 99. Anything at or above it is rejected.
    assert!(
        windows.len() < 99,
        "{} windows would nest past ffmpeg's expression limit",
        windows.len()
    );
}

#[test]
fn a_graph_built_past_the_limit_is_actually_rejected() {
    // Guards the guard. If a future ffmpeg raises its recursion budget, or the
    // graph shape changes so nesting no longer tracks window count, this test
    // fails and the cap above can be revisited on evidence rather than guesswork.
    if ffmpeg().is_none() {
        eprintln!("skipping: resources/ffmpeg.exe not present");
        return;
    }

    let plan = load_plan("abc");
    let reckless = WindowParams {
        max_windows: 380, // the value inherited from livescan.py
        ..WindowParams::default()
    };
    let windows = build_windows(&plan, &reckless);
    assert!(
        windows.len() > 99,
        "this fixture is supposed to overflow the budget; got {}",
        windows.len()
    );

    let graph = build_graph(
        &[],
        &windows,
        plan.source.width,
        plan.source.height,
        CensorStyle::BlackBox,
    );
    assert!(
        ffmpeg_accepts(&graph, plan.source.width, plan.source.height).is_err(),
        "ffmpeg accepted {} nested windows; the cap may no longer be needed",
        windows.len()
    );
}
