"""Export golden test fixtures for the Rust port of the censoring engine.

Run with the venv python, from the repo root:

    .venv\\Scripts\\python.exe tools\\export_fixtures.py sample.mp4
    .venv\\Scripts\\python.exe tools\\export_fixtures.py abc.mp4

For each video this runs the trusted live scanner to completion and writes
two things into crates/scrim-core/tests/fixtures/:

  <stem>.plan.json    the scan result in Scrim's native schema v1
  <stem>.graphs.json  the filtergraph string the Python engine builds from
                      that plan, for each of the five censor styles

The Rust implementation must reproduce every graph string byte for byte from
the same plan file. That is the whole point: the rewrite is allowed to change
how the code looks, never what gets covered.

Nothing here ships. It runs once, against the Python that already works, and
its output is checked into the Rust test suite.
"""

import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO))

import livescan  # noqa: E402
import pfplay  # noqa: E402

OUT = REPO / "crates" / "scrim-core" / "tests" / "fixtures"

# The five entries of the Censor picker, as player_app.App.CENSOR_CHOICES
# maps them onto the engine's (style, strength) pair.
CENSOR_CHOICES = {
    "black_box": ("black", "strong"),
    "white_box": ("white", "strong"),
    "blur_strong": ("blur", "strong"),
    "blur_medium": ("blur", "medium"),
    "blur_light": ("blur", "light"),
}


def scan(video: Path) -> livescan.LiveScanner:
    """Run a full live scan, reporting progress on one line."""
    ls = livescan.LiveScanner(video)
    print(f"  source {ls.src_w}x{ls.src_h}  {ls.duration:.2f}s  "
          f"detect at {ls.ex_w}x{ls.ex_h}")
    started = time.time()
    ls.start()
    while not ls.done and not ls.error:
        time.sleep(2.0)
        el = time.time() - started
        speed = ls.frontier / el if el > 0 else 0
        pct = 100 * ls.frontier / ls.duration if ls.duration else 0
        print(f"\r  scanning {pct:5.1f}%  frontier {ls.frontier:8.1f}s  "
              f"{speed:5.1f}x realtime  {len(ls._dets)} detections",
              end="", flush=True)
    print()
    if ls.error:
        raise RuntimeError(f"scan failed: {ls.error}")
    print(f"  done in {time.time() - started:.0f}s")
    return ls


def native_plan(video: Path, ls: livescan.LiveScanner) -> dict:
    """The scan result in Scrim's native plan schema, version 1.

    Detections are stored raw rather than as built censor windows. Window
    building depends on the lead / hold / margin tunables, and those are
    editable in Settings, so keeping the plan at the detection layer means
    changing a tunable re-derives instantly instead of forcing a rescan.
    Boxes are in source pixel coordinates, already scaled up from the
    detector's smaller input frame by LiveScanner._run.
    """
    return {
        "schema_version": 1,
        "generator": "export_fixtures.py (python reference engine)",
        "created_at": datetime.now(timezone.utc).isoformat(),
        "source": {
            "name": video.name,
            "size_bytes": video.stat().st_size,
            "duration": round(ls.duration, 6),
            "fps": round(float(ls.meta.fps), 6),
            "width": ls.src_w,
            "height": ls.src_h,
        },
        "detector": {
            "sample_fps": livescan.SAMPLE_FPS,
            "threshold": livescan.THRESHOLD,
            "detect_width": ls.ex_w,
            "detect_height": ls.ex_h,
        },
        "complete": True,
        "detections": [
            {"t": round(t, 6), "boxes": [list(b) for b in boxes]}
            for t, boxes in ls._dets
        ],
    }


def graphs(ls: livescan.LiveScanner) -> dict:
    """The filtergraph for every censor style, plus the windows behind them."""
    wins = ls.windows()
    out = {
        "window_count": len(wins),
        "windows": [list(w) for w in wins],
        "frame_width": ls.src_w,
        "frame_height": ls.src_h,
        "graphs": {},
    }
    for label, (style, strength) in CENSOR_CHOICES.items():
        out["graphs"][label] = pfplay.build_graph(
            [], wins, ls.src_w, ls.src_h, style, strength)
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {Path(sys.argv[0]).name} <video> [video ...]")
    OUT.mkdir(parents=True, exist_ok=True)
    for name in sys.argv[1:]:
        video = (REPO / name).resolve()
        if not video.exists():
            sys.exit(f"no such video: {video}")
        print(f"{video.name}:")
        ls = scan(video)

        plan = native_plan(video, ls)
        g = graphs(ls)

        stem = video.stem
        p_out = OUT / f"{stem}.plan.json"
        g_out = OUT / f"{stem}.graphs.json"
        p_out.write_text(json.dumps(plan, indent=1), encoding="utf-8")
        g_out.write_text(json.dumps(g, indent=1), encoding="utf-8")

        det_frames = len(plan["detections"])
        covered = sum(w[1] - w[0] for w in g["windows"])
        print(f"  {det_frames} detection frames -> {g['window_count']} windows"
              f"  ({covered:.1f}s covered)")
        print(f"  wrote {p_out.relative_to(REPO)}")
        print(f"  wrote {g_out.relative_to(REPO)}")
        print()


if __name__ == "__main__":
    main()
