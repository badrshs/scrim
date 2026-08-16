"""pfplay - play a video in mpv with blur applied from a PureFrame censor plan.

Usage:
    python pfplay.py scan movie.mp4 [extra pureframe args]
    python pfplay.py play movie.mp4 [--dump] [--mpv PATH]

The plan JSON (movie.censorplan.json) is converted into an ffmpeg filtergraph
with time-gated blur filters, so mpv plays the original file untouched and the
blur is composited live at the exact flagged timestamps. Nothing is re-encoded.

Fail-closed: if the plan is missing or invalid, playback is refused.
"""

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# Tunables
GAP_FRAMES = 3          # merge flagged runs separated by gaps up to this size
PAD_SECONDS = 0.3       # widen every interval on both sides
LEAD_BEFORE = 5.0       # start censoring this long before a run of detections
HOLD_AFTER = 10.0       # keep censoring this long after a run of detections
WINDOW_SECONDS = 1.5    # union region boxes over windows of this length
BOX_MARGIN = 0.04       # grow each union box by this fraction of frame size
MAX_CHAINS = 400        # region windows allowed before escalating to full-frame
                        # (bounds the depth of the per-frame position expression)
CENSOR_STYLES = ("black", "white", "blur")
BLUR_STRENGTHS = {  # luma radius/power, chroma radius/power
    "light": (8, 1, 4, 1),
    "medium": (16, 2, 8, 2),
    "strong": (28, 2, 14, 2),
}


def censor_filter(style: str, strength: str) -> str:
    """The ffmpeg filter that censors a region (or the whole frame)."""
    if style in ("black", "white"):
        return f"drawbox=x=0:y=0:w=iw:h=ih:color={style}:t=fill"
    lr, lp, cr, cp = BLUR_STRENGTHS[strength]
    # radius is clamped by expression so tiny regions can't break boxblur
    return (f"boxblur=luma_radius='min({lr},(min(w,h)-1)/2)':luma_power={lp}"
            f":chroma_radius='min({cr},(min(cw,ch)-1)/2)':chroma_power={cp}")

# Which flagged categories actually get blurred at playback. The detector
# also tags kissing and suggestive scenes; by default only visible nudity
# and sex acts are hidden.
BLUR_LEVELS = {
    "nudity": {"NUDITY_EXPLICIT", "SEXUAL_ACT_VISIBLE"},
    "kissing": {"NUDITY_EXPLICIT", "SEXUAL_ACT_VISIBLE",
                "SEXUAL_CONTEXT_NO_NUDITY", "KISS_INTENSE"},
    "all": None,  # everything the scan flagged
}


def die(msg: str) -> None:
    print(f"pfplay: {msg}", file=sys.stderr)
    sys.exit(1)


def find_plan(video: Path):
    """Return the censor plan path for a video, or None if not scanned yet.

    pureframe names plans <file.ext>.censorplan.json; accept <stem> too.
    """
    candidates = [video.parent / f"{video.name}.censorplan.json",
                  video.parent / f"{video.stem}.censorplan.json"]
    return next((p for p in candidates if p.exists()), None)


def find_mpv(explicit: str | None = None):
    """Locate mpv.exe, or return None."""
    mpv = explicit or shutil.which("mpv")
    if not mpv:
        scoop_mpv = Path.home() / "scoop" / "apps" / "mpv" / "current" / "mpv.exe"
        if scoop_mpv.exists():
            mpv = str(scoop_mpv)
    return mpv


def load_plan(video: Path):
    try:
        from pureframe.pipeline.render.plan import CensorPlan
    except ImportError:
        die("pureframe is not importable. Run me with the venv python:\n"
            "  .\\.venv\\Scripts\\python.exe pfplay.py ...")
    plan_path = find_plan(video)
    if plan_path is None:
        die(f"no plan found at {video.name}.censorplan.json\n"
            f"Scan first:  python pfplay.py scan {video.name}")
    try:
        return CensorPlan.load(plan_path)
    except Exception as e:
        die(f"plan at {plan_path} failed validation, refusing to play.\n{e}")


def frames_to_runs(frames: list[int]) -> list[tuple[int, int]]:
    """Collapse sorted frame indices into (start, end) runs, bridging small gaps."""
    runs = []
    start = prev = frames[0]
    for f in frames[1:]:
        if f - prev > GAP_FRAMES:
            runs.append((start, prev))
            start = f
        prev = f
    runs.append((start, prev))
    return runs


def build_intervals(plan, categories=BLUR_LEVELS["nudity"]):
    """Split frame actions into full-frame blur runs and per-window region boxes.

    Only verdicts whose category is in `categories` are blurred (None means
    blur everything the scan flagged). Returns (full_runs_seconds,
    region_windows, frame_width, frame_height) where region_windows is a list
    of (start_s, end_s, x, y, w, h).
    """
    from pureframe.pipeline.shots import Action

    if categories is not None:
        for v in plan.verdicts:
            if v.category.value not in categories:
                v.action = Action.NONE
    fa = plan.build_frame_actions()
    meta = plan.input_metadata
    fps = float(meta.fps)
    fw, fh = meta.width, meta.height

    full_frames = []
    box_frames = {}  # every BLACK_BOX frame; boxes exist only on sampled frames
    for f, entry in fa.items():
        action, boxes = entry["action"], entry["boxes"]
        if action == Action.FULL_FRAME_BLUR:
            full_frames.append(f)
        elif action == Action.BLACK_BOX:
            box_frames[f] = boxes

    def to_seconds(run):
        s = max(0.0, run[0] / fps - PAD_SECONDS)
        e = min(meta.duration_seconds, (run[1] + 1) / fps + PAD_SECONDS)
        return (s, e)

    full_runs = [(max(0.0, s - LEAD_BEFORE),
                  min(meta.duration_seconds, e + HOLD_AFTER))
                 for s, e in (to_seconds(r)
                              for r in frames_to_runs(sorted(full_frames)))
                 ] if full_frames else []
    full_runs_extra = []

    # Region runs -> fixed windows, one union box per window
    windows = []
    margin_x, margin_y = int(fw * BOX_MARGIN), int(fh * BOX_MARGIN)
    win_frames = max(1, int(WINDOW_SECONDS * fps))
    if box_frames:
        for run_start, run_end in frames_to_runs(sorted(box_frames)):
            first_win_of_run = len(windows)
            f = run_start
            while f <= run_end:
                w_end = min(f + win_frames - 1, run_end)
                # Union boxes in the window; the detector samples only some
                # frames, so widen the search to half a window on each side
                # before giving up on region blur.
                slack = win_frames // 2
                lo = max(run_start, f - slack)
                hi = min(run_end, w_end + slack)
                xs1, ys1, xs2, ys2 = [], [], [], []
                for i in range(lo, hi + 1):
                    for (x1, y1, x2, y2) in box_frames.get(i, []):
                        xs1.append(x1); ys1.append(y1); xs2.append(x2); ys2.append(y2)
                s, e = to_seconds((f, w_end))
                if xs1:
                    x1 = max(0, min(xs1) - margin_x)
                    y1 = max(0, min(ys1) - margin_y)
                    x2 = min(fw, max(xs2) + margin_x)
                    y2 = min(fh, max(ys2) + margin_y)
                    # even coordinates keep chroma subsampling happy
                    x1, y1 = x1 // 2 * 2, y1 // 2 * 2
                    w = max(4, (x2 - x1) // 2 * 2)
                    h = max(4, (y2 - y1) // 2 * 2)
                    windows.append((s, e, x1, y1, w, h))
                else:
                    # No box data anywhere near this window: blur the whole
                    # frame for it rather than risk showing anything
                    full_runs_extra.append((s, e))
                f = w_end + 1
            # censor LEAD_BEFORE early and hold HOLD_AFTER past this run
            if len(windows) > first_win_of_run:
                s, e, x, y, w, h = windows[first_win_of_run]
                windows[first_win_of_run] = (max(0.0, s - LEAD_BEFORE), e,
                                             x, y, w, h)
                s, e, x, y, w, h = windows[-1]
                windows[-1] = (s, min(meta.duration_seconds, e + HOLD_AFTER),
                               x, y, w, h)

    full_runs.extend(full_runs_extra)

    # Escalation guard: too many chains would bloat the graph and CPU cost,
    # so demote the densest spans to full-frame blur instead. Over-blurring
    # is acceptable, under-blurring is not.
    if len(windows) > MAX_CHAINS:
        windows.sort(key=lambda w: w[1] - w[0])  # shortest first
        keep, escalate = windows[:MAX_CHAINS], windows[MAX_CHAINS:]
        print(f"pfplay: escalating {len(escalate)} dense region windows to full-frame blur")
        full_runs.extend((w[0], w[1]) for w in escalate)
        windows = keep

    full_runs = merge_overlaps(full_runs)
    windows.sort(key=lambda w: w[0])
    return full_runs, windows, fw, fh


def merge_overlaps(runs):
    if not runs:
        return []
    runs = sorted(runs)
    merged = [list(runs[0])]
    for s, e in runs[1:]:
        if s <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], e)
        else:
            merged.append([s, e])
    return [tuple(r) for r in merged]


def enable_expr(runs):
    return "+".join(f"between(t,{s:.3f},{e:.3f})" for s, e in runs)


def piecewise(windows, idx: int, default: int) -> str:
    """Nested if(between(t,..),v,...) expression selecting a value per window.

    Iterates forward so that when windows overlap (a 10s hold running into a
    fresh detection), the LATER-starting window wins: the newest detection's
    position takes precedence over a stale held position.
    """
    expr = str(default)
    for (s, e, *vals) in windows:
        expr = f"if(between(t,{s:.3f},{e:.3f}),{vals[idx]},{expr})"
    return expr


def build_graph(full_runs, windows, fw: int, fh: int,
                style: str = "black", strength: str = "strong") -> str:
    """Build a single-input single-output lavfi graph for mpv's vf=lavfi=[...].

    All region windows share ONE split/crop/blur/overlay chain whose position
    moves over time via per-frame expressions. Long chains of overlay filters
    (one per window) grind mpv's filter bridge to a halt, so the chain count
    must stay constant no matter how much of the movie is flagged.
    """
    censor = censor_filter(style, strength)
    parts = []
    cur = ""  # empty label = graph input on the first filter
    if windows:
        # one fixed crop size fits every window; recenter each window in it
        bw = min(fw, max(w[4] for w in windows))
        bh = min(fh, max(w[5] for w in windows))
        moved = []
        for (s, e, x, y, w, h) in windows:
            nx = min(max(0, x + (w - bw) // 2), fw - bw) // 2 * 2
            ny = min(max(0, y + (h - bh) // 2), fh - bh) // 2 * 2
            moved.append((s, e, nx, ny))
        xe, ye = piecewise(moved, 0, 0), piecewise(moved, 1, 0)
        gate = f"enable='{enable_expr([(s, e) for s, e, *_ in moved])}'"
        parts.append(f"{cur}split=2[m][t]")
        parts.append(f"[t]crop=w={bw}:h={bh}:x='{xe}':y='{ye}',{censor}[b]")
        parts.append(f"[m][b]overlay=x='{xe}':y='{ye}':eval=frame:{gate}[o]")
        cur = "[o]"
    if full_runs:
        parts.append(f"{cur}{censor}:enable='{enable_expr(full_runs)}'")
    elif windows:
        # strip the label from the last overlay so the graph output is unlabeled
        parts[-1] = parts[-1][: -len(cur)]
    return ";".join(parts)


def cmd_play(args):
    video = Path(args.video).resolve()
    if not video.exists():
        die(f"{video} not found")
    plan = load_plan(video)
    full_runs, windows, fw, fh = build_intervals(plan, BLUR_LEVELS[args.blur])
    graph = build_graph(full_runs, windows, fw, fh, args.style, args.strength)

    total_blur = sum(e - s for s, e in full_runs) + sum(w[1] - w[0] for w in windows)
    print(f"pfplay: {len(full_runs)} full-frame intervals, {len(windows)} region windows, "
          f"~{total_blur:.1f}s of playback carries blur")

    if args.dump:
        print(graph if graph else "(no filters: plan has zero flagged frames)")
        return

    mpv = find_mpv(args.mpv)
    if not mpv:
        die("mpv not found on PATH (install it, or pass --mpv C:\\path\\to\\mpv.exe)")

    cmd = [mpv, str(video)]
    if graph:
        # A conf file sidesteps the Windows command-line length limit
        conf = Path(tempfile.gettempdir()) / f"pfplay-{video.stem}.conf"
        conf.write_text(f"vf=lavfi=[{graph}]\nhwdec=no\n", encoding="utf-8")
        cmd.append(f"--include={conf}")
    else:
        print("pfplay: plan flags nothing, playing unfiltered")
    cmd += args.mpv_args
    sys.exit(subprocess.call(cmd))


def cmd_scan(args):
    exe = Path(sys.executable).parent / ("pureframe.exe" if sys.platform == "win32" else "pureframe")
    if not exe.exists():
        die(f"pureframe CLI not found at {exe}, is the venv set up?")
    sys.exit(subprocess.call([str(exe), "plan", args.video, *args.pureframe_args]))


def main():
    p = argparse.ArgumentParser(prog="pfplay", description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("play", help="play a video with its censor plan applied")
    sp.add_argument("video")
    sp.add_argument("--dump", action="store_true", help="print the filtergraph and exit")
    sp.add_argument("--blur", choices=list(BLUR_LEVELS), default="nudity",
                    help="what to blur: nudity (sex/nudity only, default), "
                         "kissing (also suggestive scenes and intense kissing), "
                         "all (everything the scan flagged)")
    sp.add_argument("--style", choices=list(CENSOR_STYLES), default="black",
                    help="cover flagged regions with a black box (default), "
                         "white box, or a see-through blur")
    sp.add_argument("--strength", choices=list(BLUR_STRENGTHS),
                    default="strong", help="blur strength when --style blur")
    sp.add_argument("--mpv", help="path to mpv.exe if not on PATH")
    sp.add_argument("mpv_args", nargs="*", help="extra args passed through to mpv")
    sp.set_defaults(func=cmd_play)

    ss = sub.add_parser("scan", help="run pureframe plan on a video")
    ss.add_argument("video")
    ss.add_argument("pureframe_args", nargs="*", help="extra args for pureframe plan")
    ss.set_defaults(func=cmd_scan)

    args = p.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
