"""Record what the Python detector sees, so the Rust port can be held to it.

    .venv\\Scripts\\python.exe tools\\export_detector_fixture.py

Pulls a span of frames out of abc.mp4 with exactly the command the Rust
scanner uses, runs the original `nudenet` over them, and writes every raw
detection to crates/scrim-detect/tests/fixtures/detector.json.

The frames themselves are not stored: at 2.7 MB each they have no business in
a repository, and both sides can decode them from the same movie with the same
ffmpeg invocation. The Rust test skips when the movie is not present.

The span is chosen to straddle real content: it starts before the first
detection in abc.mp4 so the fixture contains empty frames as well as hits.
A detector that finds nothing would otherwise pass a fixture of only misses.
"""

import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "crates" / "scrim-detect" / "tests" / "fixtures"

VIDEO = REPO / "abc.mp4"
FFMPEG = REPO / "resources" / "ffmpeg.exe"

# Must match scrim-detect: 3 fps, capped at 1280 wide, bgr24.
SAMPLE_FPS = 3
DETECT_W, DETECT_H = 1280, 720
START = 2265.0
DURATION = 65.0

CREATE_NO_WINDOW = 0x08000000


def main():
    if not VIDEO.exists():
        sys.exit(f"{VIDEO.name} is needed to build this fixture")
    if not FFMPEG.exists():
        sys.exit("run tools/fetch-resources.ps1 first")

    sys.path.insert(0, str(REPO / "legacy"))
    from nudenet import NudeDetector
    import numpy as np

    detector = NudeDetector()

    cmd = [
        str(FFMPEG), "-v", "error",
        "-ss", f"{START:.3f}", "-t", f"{DURATION:.3f}",
        "-i", str(VIDEO),
        "-an", "-sn",
        "-vf", f"fps={SAMPLE_FPS},scale={DETECT_W}:{DETECT_H}",
        "-f", "rawvideo", "-pix_fmt", "bgr24", "-",
    ]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL,
                            creationflags=CREATE_NO_WINDOW)

    frame_bytes = DETECT_W * DETECT_H * 3
    frames = []
    index = 0
    while True:
        buf = proc.stdout.read(frame_bytes)
        if len(buf) < frame_bytes:
            break
        frame = np.frombuffer(buf, np.uint8).reshape(DETECT_H, DETECT_W, 3)
        dets = detector.detect(frame)
        frames.append({
            "index": index,
            "detections": [
                {
                    "class": d["class"],
                    "score": round(float(d["score"]), 6),
                    "box": [int(v) for v in d["box"]],
                }
                for d in sorted(dets, key=lambda d: -d["score"])
            ],
        })
        index += 1
    proc.kill()

    total = sum(len(f["detections"]) for f in frames)
    explicit = sum(
        1 for f in frames for d in f["detections"]
        if d["class"] in {
            "FEMALE_BREAST_EXPOSED", "FEMALE_GENITALIA_EXPOSED",
            "MALE_GENITALIA_EXPOSED", "BUTTOCKS_EXPOSED", "ANUS_EXPOSED",
        } and d["score"] >= 0.55
    )

    OUT.mkdir(parents=True, exist_ok=True)
    dest = OUT / "detector.json"
    dest.write_text(json.dumps({
        "source": VIDEO.name,
        "start": START,
        "duration": DURATION,
        "sample_fps": SAMPLE_FPS,
        "detect_width": DETECT_W,
        "detect_height": DETECT_H,
        "frames": frames,
    }, indent=1), encoding="utf-8")

    print(f"{len(frames)} frames, {total} detections, "
          f"{explicit} explicit above threshold")
    print(f"wrote {dest.relative_to(REPO)}")


if __name__ == "__main__":
    main()
