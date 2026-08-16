"""Rebuild the golden graph fixtures from an existing plan, without rescanning.

    .venv\\Scripts\\python.exe tools\\regen_graphs.py [max_windows]

The plan fixtures hold raw detections, so windows and filtergraphs can be
re-derived from them in a second rather than re-running a four minute scan.
This exists because the window cap had to change: see docs/expression-limit.md.

Still driven by the original livescan and pfplay code, so the fixtures remain
a record of what the reference implementation produces, not of what the Rust
happens to do.
"""

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "legacy"))

import livescan  # noqa: E402
import pfplay  # noqa: E402

FIX = REPO / "crates" / "scrim-core" / "tests" / "fixtures"

CENSOR_CHOICES = {
    "black_box": ("black", "strong"),
    "white_box": ("white", "strong"),
    "blur_strong": ("blur", "strong"),
    "blur_medium": ("blur", "medium"),
    "blur_light": ("blur", "light"),
}


def scanner_from_plan(plan: dict, threshold: float) -> livescan.LiveScanner:
    """A LiveScanner with its detections restored, skipping __init__'s probe.

    Handles both plan schemas: v1 stored each box as a bare [x1,y1,x2,y2]
    array, v2 stores {"box": [...], "label": ..., "score": ...} so the player
    can say why it is covering something. Plans also record below the covering
    threshold, so filtering happens here rather than at scan time.
    """
    ls = object.__new__(livescan.LiveScanner)
    src = plan["source"]
    ls.src_w = src["width"]
    ls.src_h = src["height"]
    ls.duration = src["duration"]

    dets = []
    for d in plan["detections"]:
        boxes = []
        for b in d["boxes"]:
            if isinstance(b, dict):
                if b.get("score", 0.0) >= threshold:
                    boxes.append(tuple(b["box"]))
            else:
                boxes.append(tuple(b))  # v1 was filtered when written
        if boxes:
            dets.append((d["t"], boxes))
    ls._dets = dets

    ls._win_cache = (-1, [])
    import threading
    ls._lock = threading.Lock()
    return ls


def main():
    cap = int(sys.argv[1]) if len(sys.argv) > 1 else livescan.MAX_WINDOWS
    livescan.MAX_WINDOWS = cap
    print(f"rebuilding graph fixtures with MAX_WINDOWS={cap}\n")

    for plan_path in sorted(FIX.glob("*.plan.json")):
        stem = plan_path.name.replace(".plan.json", "")
        plan = json.loads(plan_path.read_text(encoding="utf-8"))
        ls = scanner_from_plan(plan, livescan.THRESHOLD)

        wins = ls.windows()
        out = {
            "window_count": len(wins),
            "windows": [list(w) for w in wins],
            "frame_width": ls.src_w,
            "frame_height": ls.src_h,
            "graphs": {
                label: pfplay.build_graph([], wins, ls.src_w, ls.src_h, style, strength)
                for label, (style, strength) in CENSOR_CHOICES.items()
            },
        }

        dest = FIX / f"{stem}.graphs.json"
        dest.write_text(json.dumps(out, indent=1), encoding="utf-8")

        covered = sum(w[1] - w[0] for w in wins)
        longest = max((w[1] - w[0] for w in wins), default=0)
        biggest = max((w[4] * w[5] for w in wins), default=0)
        frame_px = ls.src_w * ls.src_h
        print(f"{stem}: {len(wins)} windows, {covered:.1f}s covered, "
              f"longest {longest:.1f}s, largest box "
              f"{100 * biggest / frame_px:.0f}% of frame")
        print(f"  graph {len(out['graphs']['black_box'])} chars -> {dest.name}")


if __name__ == "__main__":
    main()
