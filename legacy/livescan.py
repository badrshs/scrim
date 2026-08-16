"""Live nudity detection for PureFrame Player.

Scans a video linearly with NudeNet (~20x faster than playback on this
machine), publishing blur windows while the movie plays. When the scan
reaches the end it saves a normal censor plan so future playback needs
no live pass at all.
"""

import subprocess
import threading
from pathlib import Path

# NOTE: numpy / nudenet / onnxruntime are imported lazily inside _run().
# Importing them at module level froze the app window for seconds on a
# cold start, since player_app imports this module at startup.

CREATE_NO_WINDOW = 0x08000000

SAMPLE_FPS = 3          # frames per second fed to the detector
THRESHOLD = 0.55        # same default nudity threshold as pureframe medium
PAD_BEFORE = 5.0        # start censoring this long before a detection
PAD_AFTER = 10.0        # hold censoring this long after the last detection
GAP_MERGE = 1.5         # detections closer than this join into one run
WINDOW_MAX = 2.0        # runs are split into windows of this length
MARGIN = 0.08           # box margin as a fraction of frame size
MAX_WINDOWS = 380       # keep the filtergraph expression bounded


class LiveScanner:
    def __init__(self, video: Path):
        from pureframe.pipeline.probe import probe_video
        self.video = Path(video)
        self.meta = probe_video(self.video)
        self.src_w, self.src_h = self.meta.width, self.meta.height
        self.duration = self.meta.duration_seconds
        # detector input is capped at 1280 wide to keep the pipe cheap
        self.ex_w = min(self.src_w, 1280) // 2 * 2
        self.ex_h = int(self.src_h * self.ex_w / self.src_w) // 2 * 2

        self.frontier = 0.0
        self.done = False
        self.stopped = False
        self.error: str | None = None
        self._dets: list[tuple[float, list]] = []   # (t, [(x1,y1,x2,y2)])
        self._lock = threading.Lock()
        self._win_cache = (-1, [])
        self._proc = None

    # ---------- scanning ----------

    def start(self):
        threading.Thread(target=self._run, daemon=True).start()

    def _run(self):
        try:
            import numpy as np
            from nudenet import NudeDetector
            from pureframe.pipeline.detect.nudity import EXPLICIT_LABELS
            detector = NudeDetector()
            cmd = ["ffmpeg", "-v", "error", "-i", str(self.video),
                   "-vf", f"fps={SAMPLE_FPS},scale={self.ex_w}:{self.ex_h}",
                   "-f", "rawvideo", "-pix_fmt", "bgr24", "-"]
            self._proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                          stderr=subprocess.DEVNULL,
                                          creationflags=CREATE_NO_WINDOW)
            frame_bytes = self.ex_w * self.ex_h * 3
            sx = self.src_w / self.ex_w
            sy = self.src_h / self.ex_h
            idx = 0
            while not self.stopped:
                buf = self._proc.stdout.read(frame_bytes)
                if len(buf) < frame_bytes:
                    break
                t = idx / SAMPLE_FPS
                idx += 1
                frame = np.frombuffer(buf, np.uint8).reshape(
                    self.ex_h, self.ex_w, 3)
                boxes = []
                for d in detector.detect(frame):
                    if (d.get("class") in EXPLICIT_LABELS
                            and d.get("score", 0) >= THRESHOLD
                            and len(d.get("box", [])) == 4):
                        x, y, w, h = d["box"]
                        boxes.append((int(x * sx), int(y * sy),
                                      int((x + w) * sx), int((y + h) * sy)))
                if boxes:
                    with self._lock:
                        self._dets.append((t, boxes))
                self.frontier = t
            if not self.stopped:
                self.frontier = self.duration
                self.done = True
        except Exception as e:  # surfaced in the app UI
            self.error = str(e)
        finally:
            if self._proc and self._proc.poll() is None:
                self._proc.kill()

    def stop(self):
        self.stopped = True
        if self._proc and self._proc.poll() is None:
            self._proc.kill()

    # ---------- window building ----------

    def windows(self):
        """Merged blur windows (s, e, x, y, w, h) from detections so far."""
        with self._lock:
            n = len(self._dets)
            if n == self._win_cache[0]:
                return self._win_cache[1]
            dets = list(self._dets)

        runs, cur = [], []
        for t, boxes in dets:
            if cur and t - cur[-1][0] > GAP_MERGE:
                runs.append(cur)
                cur = []
            cur.append((t, boxes))
        if cur:
            runs.append(cur)

        window_len = WINDOW_MAX
        while True:
            wins = []
            mx, my = int(self.src_w * MARGIN), int(self.src_h * MARGIN)
            for run in runs:
                run_s = max(0.0, run[0][0] - PAD_BEFORE)
                run_e = min(self.duration, run[-1][0] + PAD_AFTER)
                s = run_s
                while s < run_e:
                    e = min(s + window_len, run_e)
                    xs1, ys1, xs2, ys2 = [], [], [], []
                    for t, boxes in run:
                        if s - GAP_MERGE <= t <= e + GAP_MERGE:
                            for (x1, y1, x2, y2) in boxes:
                                xs1.append(x1); ys1.append(y1)
                                xs2.append(x2); ys2.append(y2)
                    if not xs1:
                        # hold-tail window past the last detection: keep the
                        # run's whole detected area covered
                        for t, boxes in run:
                            for (x1, y1, x2, y2) in boxes:
                                xs1.append(x1); ys1.append(y1)
                                xs2.append(x2); ys2.append(y2)
                    if xs1:
                        x1 = max(0, min(xs1) - mx) // 2 * 2
                        y1 = max(0, min(ys1) - my) // 2 * 2
                        x2 = min(self.src_w, max(xs2) + mx)
                        y2 = min(self.src_h, max(ys2) + my)
                        w = max(4, (x2 - x1) // 2 * 2)
                        h = max(4, (y2 - y1) // 2 * 2)
                        wins.append((round(s, 3), round(e, 3), x1, y1, w, h))
                    s = e
            if len(wins) <= MAX_WINDOWS or window_len > self.duration:
                break
            window_len *= 2  # merge harder rather than overflow the graph
        with self._lock:
            self._win_cache = (n, wins)
        return wins

    # ---------- plan export ----------

    def save_plan(self):
        """Write a pureframe-compatible censor plan (only after a full scan)."""
        if not self.done:
            return None
        from datetime import datetime
        from pureframe.pipeline.render.plan import CensorPlan
        from pureframe.pipeline.shots import (Shot, ShotVerdict, Action,
                                              Category, Box)
        fps = float(self.meta.fps)
        wins = self.windows()
        # group touching windows into shots
        shots, verdicts, group = [], [], []
        for w in wins:
            if group and w[0] > group[-1][1] + 0.01:
                self._emit_shot(shots, verdicts, group, fps, Shot,
                                ShotVerdict, Action, Category, Box)
                group = []
            group.append(w)
        if group:
            self._emit_shot(shots, verdicts, group, fps, Shot,
                            ShotVerdict, Action, Category, Box)
        plan = CensorPlan(
            pureframe_version="live-scan",
            input_metadata=self.meta,
            config_snapshot={"source": "pureframe-player live mode",
                             "sample_fps": SAMPLE_FPS,
                             "threshold": THRESHOLD},
            shots=shots,
            verdicts=verdicts,
            total_censored_frames=sum(
                s.end_frame - s.start_frame for s in shots),
            total_blur_frames=0,
            generated_at=datetime.now(),
        )
        out = self.video.parent / f"{self.video.name}.censorplan.json"
        plan.serialize(out)
        return out

    @staticmethod
    def _emit_shot(shots, verdicts, group, fps, Shot, ShotVerdict,
                   Action, Category, Box):
        idx = len(shots)
        s, e = group[0][0], group[-1][1]
        boxes = []
        for (ws, we, x, y, w, h) in group:
            boxes.append(Box(x1=x, y1=y, x2=x + w, y2=y + h,
                             frame_idx=int((ws + we) / 2 * fps)))
        shots.append(Shot(index=idx, start_frame=int(s * fps),
                          end_frame=int(e * fps), start_time=s, end_time=e))
        verdicts.append(ShotVerdict(
            shot_index=idx, category=Category.NUDITY_EXPLICIT,
            action=Action.BLACK_BOX, confidence=1.0, boxes=boxes,
            reasoning="live NudeNet detection"))
