"""PureFrame Player - scan movies for explicit content, then watch with blur applied.

Run with the venv python (the desktop shortcut does this):
    .venv\\Scripts\\pythonw.exe player_app.py
"""

import ctypes
import json
import msvcrt
import os
import queue
import re
import subprocess
import sys
import threading
import time
import tkinter as tk
from pathlib import Path
from tkinter import filedialog, messagebox, ttk

APP_DIR = Path(__file__).resolve().parent
LIB_FILE = APP_DIR / "library.json"

sys.path.insert(0, str(APP_DIR))
import pfplay  # noqa: E402  (interval + filtergraph engine, already tested)
import livescan  # noqa: E402  (live NudeNet scanning for Live mode)

CREATE_NO_WINDOW = 0x08000000

ST_READY = "Ready"
ST_SCANNING = "Scanning"
ST_NOT_SCANNED = "Not scanned"
ST_ERROR = "Scan error"
ICONS = {ST_READY: "\u2705", ST_SCANNING: "\u23f3",
         ST_NOT_SCANNED: "\u2014", ST_ERROR: "\u274c"}


def fmt_time(seconds):
    if seconds is None or seconds < 0:
        return "--:--"
    s = int(seconds)
    if s >= 3600:
        return f"{s // 3600}:{s % 3600 // 60:02d}:{s % 60:02d}"
    return f"{s // 60}:{s % 60:02d}"


class Mpv:
    """Embedded mpv instance controlled over a Windows named pipe (JSON IPC)."""

    def __init__(self, wid: int, events: queue.Queue):
        self.wid = wid
        self.events = events
        self.proc = None
        self.pipe = None
        self.pipe_name = rf"\\.\pipe\pureframe-player-{os.getpid()}"

    def start(self, video: Path, conf: Path | None, extra_args=None):
        mpv_exe = pfplay.find_mpv()
        if not mpv_exe:
            raise RuntimeError("mpv not found. Install it with: scoop install mpv")
        cmd = [mpv_exe, str(video),
               f"--wid={self.wid}",
               f"--input-ipc-server={self.pipe_name}",
               "--hwdec=no", "--keep-open=yes", "--pause=no",
               "--osc=no", "--input-default-bindings=no",
               "--msg-level=all=error", "--force-seekable=yes"]
        if conf:
            cmd.insert(2, f"--include={conf}")
        if extra_args:
            cmd += extra_args
        self.proc = subprocess.Popen(cmd, creationflags=CREATE_NO_WINDOW,
                                     stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
        # mpv creates the pipe once it is up; retry until it appears
        for _ in range(100):
            try:
                self.pipe = open(self.pipe_name, "r+b", buffering=0)
                break
            except OSError:
                if self.proc.poll() is not None:
                    raise RuntimeError("mpv exited before the IPC pipe opened")
                time.sleep(0.1)
        else:
            raise RuntimeError("could not connect to mpv IPC pipe")
        # Switch the pipe to non-blocking reads. A blocking read on a
        # synchronous duplex pipe handle serializes with writes on the same
        # handle, so a waiting reader thread would freeze the UI thread's
        # first send() indefinitely.
        handle = msvcrt.get_osfhandle(self.pipe.fileno())
        nowait = ctypes.c_ulong(1)  # PIPE_NOWAIT
        ctypes.windll.kernel32.SetNamedPipeHandleState(
            ctypes.c_void_p(handle), ctypes.byref(nowait), None, None)
        for i, prop in enumerate(["time-pos", "duration", "pause", "eof-reached"], 1):
            self.send(["observe_property", i, prop])
        threading.Thread(target=self._reader, daemon=True).start()

    def send(self, command: list):
        if not self.pipe:
            return
        try:
            self.pipe.write(json.dumps({"command": command}).encode() + b"\n")
        except OSError:
            pass

    def _reader(self):
        pipe = self.pipe  # local ref: stop() may null the attribute mid-read
        buf = b""
        while True:
            try:
                chunk = pipe.read(4096)
            except OSError as e:
                # non-blocking pipe with nothing to read -> ERROR_NO_DATA (232)
                if getattr(e, "winerror", None) == 232:
                    time.sleep(0.05)
                    continue
                break  # 109 ERROR_BROKEN_PIPE or handle closed: mpv is gone
            except ValueError:
                break  # pipe object closed by stop()
            if chunk is None:
                time.sleep(0.05)
                continue
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                if msg.get("event") == "property-change":
                    self.events.put((msg.get("name"), msg.get("data")))
        self.events.put(("mpv-gone", None))

    def alive(self):
        return self.proc is not None and self.proc.poll() is None

    def stop(self):
        if self.alive():
            self.send(["quit"])
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.pipe:
            try:
                self.pipe.close()
            except OSError:
                pass
            self.pipe = None
        self.proc = None


class App:
    CENSOR_CHOICES = {  # label -> (style, blur strength)
        "Black box": ("black", "strong"),
        "White box": ("white", "strong"),
        "Blur strong": ("blur", "strong"),
        "Blur medium": ("blur", "medium"),
        "Blur light": ("blur", "light"),
    }

    def _censor_args(self):
        return self.CENSOR_CHOICES.get(self.censor.get(), ("black", "strong"))

    def __init__(self, root: tk.Tk):
        self.root = root
        root.title("PureFrame Player")
        root.geometry("1100x640")
        root.minsize(900, 520)

        self.movies: list[Path] = self._load_library()
        self.scans: dict[Path, str] = {}      # video -> progress text
        self._stats_cache: dict = {}
        self.time_pos = None
        # live mode state
        self.live: livescan.LiveScanner | None = None
        self.live_want = False        # user asked for live playback
        self.live_playing = False
        self.live_saved = False
        self.live_deadline = 0.0
        self.live_win_count = -1
        self.live_paused_safety = False
        self._last_vf = 0.0
        self._scrub_last = 0.0
        self.play_intervals = None    # (full, wins, fw, fh) of current plan
        self.cast_session = None
        self.mpv: Mpv | None = None
        self.mpv_events: queue.Queue = queue.Queue()
        self.now_playing: Path | None = None
        self.duration = None
        self.user_seeking = False
        self.paused = False
        self.fullscreen = False

        self._build_ui()
        self._refresh_library()
        self._tick()
        # warm the heavy imports off the UI thread so the first click on
        # Play/Scan/Live doesn't stutter (they'd otherwise load lazily)
        root.after(300, lambda: threading.Thread(
            target=self._preload_libs, daemon=True).start())

    @staticmethod
    def _preload_libs():
        try:
            import numpy  # noqa: F401
            import nudenet  # noqa: F401
            from pureframe.pipeline.render.plan import CensorPlan  # noqa: F401
        except Exception:
            pass  # features degrade to lazy loading on demand

    # ---------- persistence ----------

    def _load_library(self):
        self.subs: dict = {}
        self.sub_delays: dict = {}
        try:
            data = json.loads(LIB_FILE.read_text())
        except (OSError, ValueError):
            return []
        if isinstance(data, dict):  # current format
            self.subs = {Path(k): Path(v) for k, v in
                         data.get("subs", {}).items() if Path(v).exists()}
            self.sub_delays = {Path(k): float(v) for k, v in
                               data.get("sub_delays", {}).items()}
            paths = [Path(p) for p in data.get("movies", [])]
        else:  # legacy: plain list
            paths = [Path(p) for p in data]
        return [p for p in paths if p.exists()]

    def _save_library(self):
        LIB_FILE.write_text(json.dumps(
            {"movies": [str(p) for p in self.movies],
             "subs": {str(k): str(v) for k, v in self.subs.items()},
             "sub_delays": {str(k): v for k, v in self.sub_delays.items()
                            if v}},
            indent=1))

    # ---------- UI construction ----------

    def _build_ui(self):
        top = ttk.Frame(self.root, padding=(8, 8, 8, 0))
        top.pack(fill="x")
        ttk.Button(top, text="+ Add movie", command=self.add_movie).pack(side="left")
        ttk.Label(top, text="   Strictness:").pack(side="left")
        self.strictness = ttk.Combobox(top, values=["low", "medium", "high"],
                                       width=8, state="readonly")
        self.strictness.set("medium")
        self.strictness.pack(side="left")
        ttk.Label(top, text="  Content type:").pack(side="left")
        self.content_type = ttk.Combobox(
            top, values=["live-action", "animation", "anime", "low-light"],
            width=11, state="readonly")
        self.content_type.set("live-action")
        self.content_type.pack(side="left")
        ttk.Label(top, text="  Mode:").pack(side="left")
        self.mode = ttk.Combobox(top, values=["Scanned plan", "Live detection"],
                                 width=13, state="readonly")
        self.mode.set("Scanned plan")
        self.mode.pack(side="left")
        self.mode.bind("<<ComboboxSelected>>", lambda e: self._update_buttons())
        ttk.Label(top, text="  Head start:").pack(side="left")
        self.head_start = ttk.Spinbox(top, from_=1, to=10, width=3)
        self.head_start.set(5)
        self.head_start.pack(side="left")
        ttk.Label(top, text="min").pack(side="left")
        ttk.Label(top, text="  Censor:").pack(side="left")
        self.censor = ttk.Combobox(top, values=list(self.CENSOR_CHOICES),
                                   width=12, state="readonly")
        self.censor.set("Black box")
        self.censor.pack(side="left")
        self.censor.bind("<<ComboboxSelected>>",
                         lambda e: self._apply_censor_now())

        body = ttk.Frame(self.root, padding=8)
        body.pack(fill="both", expand=True)

        self.sidebar = ttk.Frame(body)
        self.sidebar.pack(side="left", fill="y")
        self.tree = ttk.Treeview(self.sidebar, columns=("status",), show="tree headings",
                                 selectmode="browse", height=20)
        self.tree.heading("#0", text="Movie")
        self.tree.heading("status", text="Status")
        self.tree.column("#0", width=210)
        self.tree.column("status", width=130)
        self.tree.pack(fill="y", expand=True)
        self.tree.bind("<<TreeviewSelect>>", lambda e: self._update_buttons())
        self.detail = ttk.Label(self.sidebar, text="", wraplength=330, justify="left")
        self.detail.pack(fill="x", pady=(6, 0))

        right = ttk.Frame(body)
        right.pack(side="left", fill="both", expand=True, padx=(8, 0))
        self.video_frame = tk.Frame(right, bg="black")
        self.video_frame.pack(fill="both", expand=True)

        controls = ttk.Frame(right)
        controls.pack(fill="x", pady=(6, 0))
        self.btn_pause = ttk.Button(controls, text="\u23f8", width=3,
                                    command=self.toggle_pause, state="disabled")
        self.btn_pause.pack(side="left")
        self.btn_stop = ttk.Button(controls, text="\u23f9", width=3,
                                   command=self.stop_playback, state="disabled")
        self.btn_stop.pack(side="left", padx=(4, 8))
        self.time_lbl = ttk.Label(controls, text="--:-- / --:--", width=16)
        self.time_lbl.pack(side="right")
        ttk.Label(controls, text="\U0001f50a").pack(side="right")
        self.volume = ttk.Scale(controls, from_=0, to=130, length=90,
                                command=self._on_volume)
        self.volume.set(100)
        self.volume.pack(side="right", padx=(0, 6))
        self.btn_full = ttk.Button(controls, text="\u26f6 Fullscreen", width=12,
                                   command=self.toggle_fullscreen)
        self.btn_full.pack(side="right", padx=(0, 8))
        self.seek = ttk.Scale(controls, from_=0, to=1000, command=self._on_seek_drag)
        self.seek.pack(fill="x", expand=True, side="left", padx=(0, 8))
        self.seek.bind("<ButtonPress-1>", lambda e: setattr(self, "user_seeking", True))
        self.seek.bind("<ButtonRelease-1>", self._on_seek_release)

        bottom = ttk.Frame(self.root, padding=8)
        bottom.pack(fill="x")
        self.btn_scan = ttk.Button(bottom, text="\U0001f50d Scan", command=self.scan)
        self.btn_scan.pack(side="left")
        self.btn_review = ttk.Button(bottom, text="\U0001f5bc Review flagged",
                                     command=self.review)
        self.btn_review.pack(side="left", padx=6)
        self.btn_play = ttk.Button(bottom, text="\u25b6 Play", command=self.play)
        self.btn_play.pack(side="left")
        self.btn_cast = ttk.Button(bottom, text="\U0001f4fa Cast",
                                   command=self.toggle_cast)
        self.btn_cast.pack(side="left", padx=6)
        self.btn_sub = ttk.Button(bottom, text="\U0001f4ac Subtitle",
                                  command=self.add_subtitle)
        self.btn_sub.pack(side="left")
        ttk.Button(bottom, text="−", width=2,
                   command=lambda: self.adjust_sub_delay(-0.25)).pack(side="left", padx=(6, 0))
        self.sub_delay_lbl = ttk.Label(bottom, text="+0.00s", width=7,
                                       anchor="center")
        self.sub_delay_lbl.pack(side="left")
        ttk.Button(bottom, text="+", width=2,
                   command=lambda: self.adjust_sub_delay(+0.25)).pack(side="left")
        self.status_lbl = ttk.Label(bottom, text="")
        self.status_lbl.pack(side="left", padx=12)

        self.root.bind("<Escape>", lambda e: self.exit_fullscreen())
        self.root.bind("<space>", lambda e: self.toggle_pause())
        self.root.protocol("WM_DELETE_WINDOW", self.on_close)

    # ---------- library ----------

    def status_of(self, video: Path):
        if video in self.scans:
            return ST_SCANNING, self.scans[video]
        plan = pfplay.find_plan(video)
        if plan:
            return ST_READY, ""
        return ST_NOT_SCANNED, ""

    def _refresh_library(self):
        sel = self.selected()
        self.tree.delete(*self.tree.get_children())
        for v in self.movies:
            st, extra = self.status_of(v)
            label = f"{ICONS[st]} {st}" + (f" {extra}" if extra else "")
            self.tree.insert("", "end", iid=str(v), text=v.name, values=(label,))
        if sel and str(sel) in self.tree.get_children():
            self.tree.selection_set(str(sel))
        self._update_buttons()

    def selected(self) -> Path | None:
        sel = self.tree.selection()
        return Path(sel[0]) if sel else None

    def _update_buttons(self):
        v = self.selected()
        st = self.status_of(v)[0] if v else None
        live_mode = self.mode.get() == "Live detection"
        playable = st == ST_READY or (live_mode and v is not None)
        self.btn_play.config(state="normal" if playable else "disabled")
        self.btn_review.config(state="normal" if st == ST_READY else "disabled")
        self.btn_scan.config(
            state="normal" if v and st in (ST_NOT_SCANNED, ST_ERROR, ST_READY)
            else "disabled")
        if v and st == ST_READY:
            self._show_plan_stats(v)
        elif v:
            self.detail.config(text="Not scanned yet. Press Scan (takes a while, "
                                    "you can keep using the app).")

    def _show_plan_stats(self, video: Path):
        plan_path = pfplay.find_plan(video)
        key = (str(plan_path), plan_path.stat().st_mtime)
        if key not in self._stats_cache:
            try:
                from pureframe.pipeline.render.plan import CensorPlan
                plan = CensorPlan.load(plan_path)
                full, wins, _, _ = pfplay.build_intervals(plan)
                total = sum(e - s for s, e in full) + sum(w[1] - w[0] for w in wins)
                self._stats_cache[key] = (
                    f"Ready. {len(full)} full-screen blur segments, "
                    f"{len(wins)} region blur windows, "
                    f"~{total:.0f}s of playback carries blur.")
            except Exception as e:
                self._stats_cache[key] = f"Plan could not be read: {e}"
        self.detail.config(text=self._stats_cache[key])

    def add_movie(self):
        names = filedialog.askopenfilenames(
            title="Add movies",
            filetypes=[("Videos", "*.mp4 *.mkv *.avi *.webm *.mov"), ("All", "*.*")])
        for n in names:
            p = Path(n)
            if p not in self.movies:
                self.movies.append(p)
        self._save_library()
        self._refresh_library()

    # ---------- scanning ----------

    def scan(self):
        video = self.selected()
        if not video:
            return
        if pfplay.find_plan(video):
            if not messagebox.askyesno("Rescan?",
                                       f"{video.name} is already scanned. Scan again?"):
                return
        if self._pureframe_running():
            if not messagebox.askyesno(
                    "Scan already running?",
                    "Another PureFrame scan seems to be running on this PC. "
                    "Running two at once is slow and can conflict.\n\nStart anyway?"):
                return
        exe = Path(sys.executable).parent / "pureframe.exe"
        if not exe.exists():
            messagebox.showerror("Missing", f"pureframe not found at {exe}")
            return
        # Visual nudity detection only: no audio analysis, no scene/kissing
        # classifier. Faster, and playback never used those categories anyway.
        cmd = [str(exe), "plan", str(video),
               "--content-type", self.content_type.get(),
               "--strictness", self.strictness.get(),
               "--no-audio", "--no-clip"]
        env = os.environ.copy()
        # Once the CLIP model is cached, keep HuggingFace fully offline:
        # its update checks are rate limited and have stalled scans before.
        hf_cache = Path.home() / ".cache" / "huggingface" / "hub"
        if any(hf_cache.glob("models--openai--clip*")):
            env["HF_HUB_OFFLINE"] = "1"
            env["TRANSFORMERS_OFFLINE"] = "1"
        self.scans[video] = "starting"
        self._refresh_library()
        threading.Thread(target=self._scan_worker, args=(video, cmd, env),
                         daemon=True).start()

    @staticmethod
    def _pureframe_running():
        try:
            out = subprocess.check_output(
                ["tasklist", "/FI", "IMAGENAME eq pureframe.exe"],
                creationflags=CREATE_NO_WINDOW, text=True)
            return "pureframe.exe" in out
        except OSError:
            return False

    def _scan_worker(self, video: Path, cmd: list, env: dict):
        started = time.time()
        try:
            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, text=True,
                                    encoding="utf-8", errors="replace",
                                    env=env, creationflags=CREATE_NO_WINDOW)
            for line in proc.stdout:
                m = re.search(r"(\d{1,3})%", line)
                mins = int(time.time() - started) // 60
                self.scans[video] = (f"{m.group(1)}%" if m else f"{mins}m elapsed")
            proc.wait()
            ok = proc.returncode == 0 and pfplay.find_plan(video)
        except OSError:
            ok = False
        del self.scans[video]
        if not ok:
            self.scans.pop(video, None)
            self.root.after(0, lambda: messagebox.showwarning(
                "Scan failed",
                f"Scanning {video.name} failed or was interrupted.\n"
                "Scanning again resumes from where it stopped."))
        self.root.after(0, self._refresh_library)

    # ---------- review ----------

    def review(self):
        video = self.selected()
        plan = pfplay.find_plan(video) if video else None
        if not plan:
            return
        exe = Path(sys.executable).parent / "pureframe.exe"
        self.status_lbl.config(text="Building review page...")

        def worker():
            try:
                out = subprocess.check_output([str(exe), "preview", str(plan)],
                                              stderr=subprocess.STDOUT, text=True,
                                              encoding="utf-8", errors="replace",
                                              creationflags=CREATE_NO_WINDOW)
                m = re.search(r"(\S+\.html)", out)
                html = Path(m.group(1)) if m else None
                if html and not html.is_absolute():
                    html = Path.cwd() / html
                if not (html and html.exists()):
                    candidates = sorted(video.parent.glob("*.html"),
                                        key=lambda p: p.stat().st_mtime)
                    html = candidates[-1] if candidates else None
                if html:
                    os.startfile(html)  # noqa: S606
                    msg = "Review page opened in your browser."
                else:
                    msg = "Preview ran but no HTML page was found."
            except (OSError, subprocess.CalledProcessError) as e:
                msg = f"Preview failed: {e}"
            self.root.after(0, lambda: self.status_lbl.config(text=msg))

        threading.Thread(target=worker, daemon=True).start()

    # ---------- playback ----------

    def play(self):
        video = self.selected()
        if not video:
            return
        if self.mode.get() == "Live detection":
            self.play_live(video)
            return
        plan_path = pfplay.find_plan(video)
        if not plan_path:  # fail closed
            messagebox.showerror(
                "Not scanned",
                "Scan this movie first, or switch Mode to Live detection.")
            return
        try:
            from pureframe.pipeline.render.plan import CensorPlan
            plan = CensorPlan.load(plan_path)
            # nudity and visible sex acts only (pfplay's default filter)
            full, wins, fw, fh = pfplay.build_intervals(plan)
            self.play_intervals = (full, wins, fw, fh)
            graph = pfplay.build_graph(full, wins, fw, fh, *self._censor_args())
        except Exception as e:
            messagebox.showerror("Bad plan", f"Refusing to play, plan invalid:\n{e}")
            return
        if not self._start_mpv(video, graph):
            return
        self.status_lbl.config(
            text=f"Playing {video.name} with blur applied"
                 + ("" if graph else " (nothing was flagged in this movie)"))

    def _start_mpv(self, video: Path, graph: str) -> bool:
        conf = None
        if graph:
            conf = APP_DIR / "current_play.conf"
            conf.write_text(f"vf=lavfi=[{graph}]\n", encoding="utf-8")
        self.stop_playback()
        self.mpv = Mpv(self.video_frame.winfo_id(), self.mpv_events)
        # subtitles: auto-load same-named files, plus any registered one
        extra = ["--sub-auto=fuzzy"]
        sub = self.subs.get(video)
        if sub and sub.exists():
            extra.append(f"--sub-file={sub}")
        delay = self.sub_delays.get(video, 0.0)
        if delay:
            extra.append(f"--sub-delay={delay}")
        self.sub_delay_lbl.config(text=f"{delay:+.2f}s")
        try:
            self.mpv.start(video, conf, extra)
        except RuntimeError as e:
            messagebox.showerror("Playback failed", str(e))
            self.mpv = None
            return False
        self.mpv.send(["set_property", "volume", int(self.volume.get())])
        self.now_playing = video
        self.duration = None
        self.time_pos = None
        self.paused = False
        self.btn_pause.config(state="normal", text="\u23f8")
        self.btn_stop.config(state="normal")
        return True

    def _apply_censor_now(self):
        """Swap the censor style on the running video without restarting."""
        if not (self.mpv and self.now_playing):
            return
        if (self.live and self.live_playing
                and self.now_playing == self.live.video):
            wins = self.live.windows()
            self.live_win_count = len(wins)
            full, fw, fh = [], self.live.src_w, self.live.src_h
        elif self.play_intervals:
            full, wins, fw, fh = self.play_intervals
        else:
            return
        graph = pfplay.build_graph(full, wins, fw, fh, *self._censor_args())
        self.mpv.send(["vf", "set", f"lavfi=[{graph}]" if graph else ""])
        self.status_lbl.config(text=f"Censor style: {self.censor.get()}")

    # ---------- live mode ----------

    def play_live(self, video: Path):
        reuse = (self.live and self.live.video == video
                 and not self.live.error and not self.live.stopped)
        if not reuse:
            if self.live:
                self.live.stop()
            try:
                self.live = livescan.LiveScanner(video)
            except Exception as e:
                messagebox.showerror("Live mode failed", str(e))
                self.live = None
                return
            self.live.start()
            self.live_saved = False
        self.stop_playback()
        try:
            head = max(1, min(10, int(self.head_start.get())))
        except ValueError:
            head = 5
        self.live_deadline = time.time() + head * 60
        self.live_playing = False
        self.live_want = True
        self.status_lbl.config(text="Live detection starting...")

    def stop_playback(self):
        if self.mpv:
            self.mpv.stop()
            self.mpv = None
        self.live_want = False
        self.live_playing = False
        self.now_playing = None
        self.btn_pause.config(state="disabled")
        self.btn_stop.config(state="disabled")
        self.time_lbl.config(text="--:-- / --:--")
        self.seek.set(0)

    def toggle_pause(self):
        if self.mpv:
            self.mpv.send(["cycle", "pause"])

    def _on_volume(self, _):
        if self.mpv:
            self.mpv.send(["set_property", "volume", int(self.volume.get())])

    def _on_seek_drag(self, value):
        # live scrubbing: follow the handle while it moves
        if not (self.user_seeking and self.mpv and self.duration):
            return
        target = float(value) / 1000 * self.duration
        self.time_lbl.config(text=f"{fmt_time(target)} / {fmt_time(self.duration)}")
        now = time.time()
        if now - self._scrub_last > 0.12:
            self._scrub_last = now
            # keyframe seeks are near-instant, good enough while moving
            self.mpv.send(["seek", round(target, 2), "absolute+keyframes"])

    def _on_seek_release(self, _):
        self.user_seeking = False
        if self.mpv and self.duration:
            # exact seek once the handle is dropped
            self.mpv.send(["set_property", "time-pos",
                           self.seek.get() / 1000 * self.duration])

    def toggle_fullscreen(self):
        self.fullscreen = not self.fullscreen
        self.root.attributes("-fullscreen", self.fullscreen)
        if self.fullscreen:
            self.sidebar.pack_forget()
        else:
            self.sidebar.pack(side="left", fill="y", before=self.video_frame.master)

    def exit_fullscreen(self):
        if self.fullscreen:
            self.toggle_fullscreen()

    def add_subtitle(self):
        """Attach an external subtitle file to the current/selected movie."""
        video = self.now_playing or self.selected()
        if not video:
            return
        f = filedialog.askopenfilename(
            title=f"Subtitle for {video.name}",
            filetypes=[("Subtitles", "*.srt *.ass *.ssa *.vtt *.sub"),
                       ("All", "*.*")])
        if not f:
            return
        self.subs[video] = Path(f)
        self._save_library()
        if self.mpv and self.now_playing == video:
            self.mpv.send(["sub-add", f, "select"])
            self.status_lbl.config(text=f"Subtitle on: {Path(f).name}")
        else:
            self.status_lbl.config(
                text=f"Subtitle saved for {video.name}; loads on play")

    def adjust_sub_delay(self, delta: float):
        """Nudge subtitle timing: + shows subs later, − earlier."""
        video = self.now_playing or self.selected()
        if not video:
            return
        d = round(self.sub_delays.get(video, 0.0) + delta, 2)
        self.sub_delays[video] = d
        self._save_library()
        self.sub_delay_lbl.config(text=f"{d:+.2f}s")
        if self.mpv and self.now_playing == video:
            self.mpv.send(["set_property", "sub-delay", d])

    # ---------- casting ----------

    def toggle_cast(self):
        if self.cast_session:
            self.cast_session.stop()
            self.cast_session = None
            self.btn_cast.config(text="\U0001f4fa Cast")
            self.status_lbl.config(text="Cast stopped")
            return
        video = self.selected()
        if not video:
            return
        plan_path = pfplay.find_plan(video)
        if not plan_path:  # fail closed: never cast unscanned video
            messagebox.showerror(
                "Not scanned",
                "Casting needs a finished scan (or a completed Live pass) "
                "so the whole movie is covered before it leaves this PC.")
            return
        # cast from where local playback is, if this movie is playing
        start = self.time_pos if (self.now_playing == video
                                  and self.time_pos) else 0.0
        self.btn_cast.config(state="disabled")
        self.status_lbl.config(text="Searching for cast devices...")
        threading.Thread(target=self._cast_worker,
                         args=(video, plan_path, start), daemon=True).start()

    def _cast_worker(self, video: Path, plan_path: Path, start: float):
        import casting
        try:
            devices = casting.discover()
        except Exception as e:
            devices, err = [], str(e)
        else:
            err = None
        if not devices:
            self.root.after(0, lambda: (
                self.btn_cast.config(state="normal"),
                self.status_lbl.config(
                    text="No cast devices found"
                         + (f" ({err})" if err else "")),
            ))
            return
        self.root.after(0, lambda: self._pick_device(devices, video, start))

    def _pick_device(self, devices, video: Path, start: float):
        self.btn_cast.config(state="normal")
        if len(devices) == 1:
            self._start_cast(devices[0], video, start)
            return
        win = tk.Toplevel(self.root)
        win.title("Cast to...")
        win.geometry("280x200")
        lb = tk.Listbox(win)
        for d in devices:
            lb.insert("end", d.cast_info.friendly_name)
        lb.pack(fill="both", expand=True, padx=8, pady=8)

        def choose():
            sel = lb.curselection()
            if sel:
                win.destroy()
                self._start_cast(devices[sel[0]], video, start)
        ttk.Button(win, text="Cast", command=choose).pack(pady=(0, 8))

    def _start_cast(self, device, video: Path, start: float):
        import casting
        try:
            from pureframe.pipeline.render.plan import CensorPlan
            plan = CensorPlan.load(pfplay.find_plan(video))
            full, wins, fw, fh = pfplay.build_intervals(plan)
            full, wins = casting.shift_intervals(full, wins, start)
            graph = pfplay.build_graph(full, wins, fw, fh,
                                       *self._censor_args())
        except Exception as e:
            messagebox.showerror("Cast failed", f"Plan problem:\n{e}")
            return
        self.stop_playback()  # one heavy video pipeline at a time
        self.cast_session = casting.CastSession(device, video, graph, start)
        self.status_lbl.config(text=f"Connecting to "
                                    f"{self.cast_session.device_name}...")

        def worker():
            try:
                self.cast_session.start_cast()
                msg = (f"Casting {video.name} to "
                       f"{self.cast_session.device_name} (censored)")
                self.root.after(0, lambda: self.btn_cast.config(
                    text="⏹ Stop cast"))
            except Exception as e:
                msg = f"Cast failed: {e}"
                self.cast_session = None
            self.root.after(0, lambda: self.status_lbl.config(text=msg))

        threading.Thread(target=worker, daemon=True).start()

    # ---------- event pump ----------

    def _tick(self):
        try:
            while True:
                name, data = self.mpv_events.get_nowait()
                if name == "duration" and data:
                    self.duration = data
                elif name == "pause":
                    self.paused = bool(data)
                    self.btn_pause.config(text="\u25b6" if self.paused else "\u23f8")
                elif name == "time-pos" and data is not None:
                    self.time_pos = data
                    if self.duration and not self.user_seeking:
                        self.seek.set(data / self.duration * 1000)
                    self.time_lbl.config(
                        text=f"{fmt_time(data)} / {fmt_time(self.duration)}")
                elif name == "mpv-gone":
                    self.stop_playback()
        except queue.Empty:
            pass
        self._tick_live()
        # pick up plans finished by scans running outside the app too
        if int(time.time()) % 5 == 0:
            self._refresh_library()
        self.root.after(250, self._tick)

    def _tick_live(self):
        ls = self.live
        if not ls:
            return
        if ls.error:
            messagebox.showerror("Live detection failed", ls.error)
            self.live = None
            self.stop_playback()
            return
        # a completed live pass becomes a normal plan for instant replays
        if ls.done and not ls.stopped and not self.live_saved:
            self.live_saved = True
            try:
                ls.save_plan()
                self._refresh_library()
            except Exception:
                pass  # playback safety does not depend on the saved plan
        if not self.live_want:
            return
        if not self.live_playing:
            if ls.done or time.time() >= self.live_deadline:
                wins = ls.windows()
                graph = pfplay.build_graph([], wins, ls.src_w, ls.src_h,
                                           *self._censor_args())
                if self._start_mpv(ls.video, graph):
                    self.live_want = True  # _start_mpv's stop cleared it
                    self.live_playing = True
                    self.live_win_count = len(wins)
                    self._last_vf = time.time()
            else:
                rem = int(self.live_deadline - time.time())
                self.status_lbl.config(
                    text=f"Live: analyzed {fmt_time(ls.frontier)} of "
                         f"{fmt_time(ls.duration)} - playback starts in "
                         f"{rem // 60}:{rem % 60:02d}"
                         + (" (or when analysis finishes)"))
            return
        if not self.mpv or self.now_playing != ls.video:
            return
        # push newly detected blur windows into the running mpv
        if not ls.stopped and time.time() - self._last_vf > 20:
            self._last_vf = time.time()
            wins = ls.windows()
            if len(wins) != self.live_win_count:
                self.live_win_count = len(wins)
                graph = pfplay.build_graph([], wins, ls.src_w, ls.src_h,
                                           *self._censor_args())
                self.mpv.send(["vf", "set", f"lavfi=[{graph}]" if graph else ""])
        if ls.done:
            return  # whole movie analyzed: no fences needed anymore
        # fail-closed fences against the detection frontier
        tp = self.time_pos
        if tp is None:
            return
        if tp > ls.frontier - 5:
            self.mpv.send(["set_property", "time-pos",
                           max(0.0, ls.frontier - 30)])
            self.status_lbl.config(
                text=f"Not analyzed past {fmt_time(ls.frontier)} yet - "
                     f"jumped back to the safe zone")
        elif tp > ls.frontier - 15:
            if not self.paused:
                self.mpv.send(["set_property", "pause", True])
                self.live_paused_safety = True
                self.status_lbl.config(text="Paused: waiting for detection "
                                            "to get further ahead...")
        elif self.live_paused_safety and ls.frontier - tp > 45:
            self.live_paused_safety = False
            self.mpv.send(["set_property", "pause", False])

    def on_close(self):
        if self.scans and not messagebox.askyesno(
                "Scan in progress",
                "A scan is still running. It will keep running in the background "
                "and resume-able progress is saved.\n\nClose the player anyway?"):
            return
        if self.live:
            self.live.stop()
        if self.cast_session:
            self.cast_session.stop()
        self.stop_playback()
        self.root.destroy()


def main():
    root = tk.Tk()
    App(root)
    root.mainloop()


if __name__ == "__main__":
    main()
