"""Export golden test fixtures for the Rust censoring engine.

Run with the venv python, from the repo root:

    .venv\\Scripts\\python.exe tools\\export_fixtures.py sample.mp4 abc.mp4

For each video this walks the movie exactly as `livescan.py` does (same ffmpeg
command, same 3 fps, same NudeNet, same threshold) and writes two files into
crates/scrim-core/tests/fixtures/:

  <stem>.plan.json    the scan in Scrim's native schema v2
  <stem>.graphs.json  the windows and filtergraphs the ORIGINAL livescan.py and
                      pfplay.py derive from that scan

The frame loop is written out here rather than reused from livescan.py for one
reason: livescan discards the label and confidence behind each box, and schema
v2 records them so the player can explain why it is covering something. The
detections are otherwise identical, and the windows and graphs are still built
by the untouched reference code, which is the part the golden tests exist to
pin down.

Nothing here ships. It runs against the Python that already worked, and its
output is checked into the Rust test suite.
"""

import json
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "legacy"))

import livescan  # noqa: E402
import pfplay  # noqa: E402

OUT = REPO / "crates" / "scrim-core" / "tests" / "fixtures"
FFMPEG = REPO / "resources" / "ffmpeg.exe"

CREATE_NO_WINDOW = 0x08000000

# Must match scrim-detect. RECORD_FLOOR is below the covering threshold on
# purpose: plans record what was seen so the threshold stays adjustable
# afterwards without a rescan.
RECORD_FLOOR = 0.35

CENSOR_CHOICES = {
    "black_box": ("black", "strong"),
    "white_box": ("white", "strong"),
    "blur_strong": ("blur", "strong"),
    "blur_medium": ("blur", "medium"),
    "blur_light": ("blur", "light"),
}


def scan(video: Path):
    """Walk the movie, returning (meta, detections) with labels intact."""
    from nudenet import NudeDetector
    from pureframe.pipeline.detect.nudity import EXPLICIT_LABELS
    from pureframe.pipeline.probe import probe_video
    import numpy as np

    meta = probe_video(video)
    src_w, src_h = meta.width, meta.height
    ex_w = min(src_w, 1280) // 2 * 2
    ex_h = int(src_h * ex_w / src_w) // 2 * 2
    sx, sy = src_w / ex_w, src_h / ex_h

    detector = NudeDetector()
    cmd = [str(FFMPEG), "-v", "error", "-i", str(video),
           "-an", "-sn",
           "-vf", f"fps={livescan.SAMPLE_FPS},scale={ex_w}:{ex_h}",
           "-f", "rawvideo", "-pix_fmt", "bgr24", "-"]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL,
                            creationflags=CREATE_NO_WINDOW)

    frame_bytes = ex_w * ex_h * 3
    detections = []
    index = 0
    started = time.time()

    while True:
        buf = proc.stdout.read(frame_bytes)
        if len(buf) < frame_bytes:
            break
        t = index / livescan.SAMPLE_FPS
        index += 1
        frame = np.frombuffer(buf, np.uint8).reshape(ex_h, ex_w, 3)

        boxes = []
        for d in detector.detect(frame):
            if d.get("class") in EXPLICIT_LABELS and d.get("score", 0) >= RECORD_FLOOR:
                x, y, w, h = d["box"]
                boxes.append({
                    "box": [int(x * sx), int(y * sy),
                            int((x + w) * sx), int((y + h) * sy)],
                    "label": d["class"],
                    "score": round(float(d["score"]), 3),
                })
        if boxes:
            detections.append({"t": round(t, 6), "boxes": boxes})

        if index % 300 == 0:
            el = time.time() - started
            print(f"\r  {t:8.1f}s  {t/el:5.1f}x realtime  "
                  f"{len(detections)} frames with detections", end="", flush=True)

    proc.kill()
    print()
    return meta, ex_w, ex_h, detections


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {Path(sys.argv[0]).name} <video> [video ...]")
    if not FFMPEG.exists():
        sys.exit("run tools/fetch-resources.ps1 first")
    OUT.mkdir(parents=True, exist_ok=True)

    for name in sys.argv[1:]:
        video = (REPO / name).resolve()
        if not video.exists():
            sys.exit(f"no such video: {video}")
        print(f"{video.name}:")

        meta, ex_w, ex_h, detections = scan(video)

        plan = {
            "schema_version": 2,
            "generator": "export_fixtures.py (python reference engine)",
            "created_at": datetime.now(timezone.utc).isoformat(),
            "source": {
                "name": video.name,
                "size_bytes": video.stat().st_size,
                "duration": round(meta.duration_seconds, 6),
                "fps": round(float(meta.fps), 6),
                "width": meta.width,
                "height": meta.height,
            },
            "detector": {
                "sample_fps": livescan.SAMPLE_FPS,
                "threshold": livescan.THRESHOLD,
                "detect_width": ex_w,
                "detect_height": ex_h,
            },
            "complete": True,
            "detections": detections,
        }

        # Windows and graphs still come from the untouched reference code, fed
        # the box-only view it expects and the same 0.55 covering threshold.
        ls = object.__new__(livescan.LiveScanner)
        ls.src_w, ls.src_h = meta.width, meta.height
        ls.duration = meta.duration_seconds
        ls._win_cache = (-1, [])
        import threading
        ls._lock = threading.Lock()
        ls._dets = [
            (d["t"], [tuple(b["box"]) for b in d["boxes"]
                      if b["score"] >= livescan.THRESHOLD])
            for d in detections
        ]
        ls._dets = [(t, boxes) for t, boxes in ls._dets if boxes]

        wins = ls.windows()
        graphs = {
            "window_count": len(wins),
            "windows": [list(w) for w in wins],
            "frame_width": meta.width,
            "frame_height": meta.height,
            "graphs": {
                label: pfplay.build_graph([], wins, meta.width, meta.height,
                                          style, strength)
                for label, (style, strength) in CENSOR_CHOICES.items()
            },
        }

        stem = video.stem
        (OUT / f"{stem}.plan.json").write_text(
            json.dumps(plan, indent=1), encoding="utf-8")
        (OUT / f"{stem}.graphs.json").write_text(
            json.dumps(graphs, indent=1), encoding="utf-8")

        above = sum(1 for d in detections
                    if any(b["score"] >= livescan.THRESHOLD for b in d["boxes"]))
        print(f"  {len(detections)} frames recorded "
              f"({above} at or above {livescan.THRESHOLD}) "
              f"-> {len(wins)} windows")
        print(f"  wrote {stem}.plan.json and {stem}.graphs.json\n")


if __name__ == "__main__":
    main()
