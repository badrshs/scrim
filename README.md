# PureFrame Player - full business logic (A to Z)

A desktop app for watching movies with explicit content covered up, built for
one requirement above all others: **an uncensored frame must never reach the
screen**. Everything below follows from that rule.

IMPORTANT: any code change to this project must update this README so it
always describes the real behavior. See `.claude/CLAUDE.md`.

---

## 1. What gets censored

Only **explicit nudity and visible sex acts**. Detection is done by the
NudeNet AI model (visual only). The labels that trigger censoring
(`EXPLICIT_LABELS` in pureframe, threshold 0.55):

- FEMALE_BREAST_EXPOSED, FEMALE_GENITALIA_EXPOSED
- MALE_GENITALIA_EXPOSED, BUTTOCKS_EXPOSED, ANUS_EXPOSED

Not censored, by explicit decision: kissing, suggestive scenes, covered
bodies, violence. Audio is never analyzed and never modified. In older plans
that contain kissing categories, playback filters them out
(`pfplay.BLUR_LEVELS["nudity"]` is the hardwired default).

## 2. The two playback modes

### Scanned plan mode
1. A one-time scan produces `<movie>.censorplan.json` next to the video
   (shots + verdicts + boxes, pureframe pydantic schema).
2. Play converts the plan into blur windows and starts embedded mpv with a
   time-gated ffmpeg filtergraph. The original file is never re-encoded.
3. Fail closed: no plan file, or a plan that fails validation = refuse to
   play in this mode.

### Live detection mode
1. Play starts `livescan.LiveScanner`: ffmpeg feeds frames at 3 fps into
   NudeNet (17 ms/frame on this CPU, roughly 20x faster than playback).
2. Playback starts after the chosen head start (1-10 min, default 5), or
   immediately when the whole movie is analyzed, whichever comes first.
3. Newly found windows are pushed into the running mpv every ~20 s over IPC
   (`vf set`), no playback interruption.
4. Safety fences while the scan is unfinished:
   - seek past `frontier - 5s`: snap back to `frontier - 30s`
   - playback reaches `frontier - 15s`: auto-pause, auto-resume at 45 s gap
   - fences lift when the scan completes
5. A completed live pass saves a normal censor plan, so the next watch of
   that movie is instant Scanned-plan mode.

## 3. Timing rules (both modes)

For every run of consecutive detections:

- censoring starts **5 s before** the first detection (`LEAD_BEFORE`)
- censoring holds **10 s after** the last detection (`HOLD_AFTER` /
  livescan `PAD_AFTER`)
- detections closer together than the merge gap join into one run
- box regions are unioned over ~1.5-2 s windows with a margin (4% plan
  mode, 8% live mode - live samples sparser so it pads more)
- when overlapping windows disagree (a hold running into a fresh
  detection), the newest detection's position wins
- a flagged span with no usable box data becomes full-frame censoring
  (over-censor, never under-censor)
- graph size is capped (~400 windows); beyond it, spans merge or escalate
  to full-frame rather than truncate silently

## 4. Censor appearance (Censor picker, applies live)

- Black box (default), White box: `drawbox` fill, nothing visible
- Blur strong / medium / light: `boxblur`, see-through by design
- Changing the picker while playing rebuilds the filtergraph and applies it
  instantly over mpv IPC, no restart, no rescan. Same for casting: the
  selected style is baked into the cast stream.

## 5. The rendering pipeline

One shared engine (`pfplay.build_graph`) produces a single-chain ffmpeg
graph: `split -> crop(x,y move per frame via time expressions) -> censor ->
overlay`, plus one full-frame censor filter for full-frame intervals. Times
are gated with `enable='between(t,..)'` so seeking is always correct.
Design constraint learned the hard way: one chain per detection window
(48 chains) grinds mpv's filter bridge to a halt; the single moving chain
runs at 16-20x realtime.

## 6. The player (player_app.py)

- Tkinter window; mpv renders embedded via `--wid`, controlled over a
  Windows named pipe (JSON IPC). The pipe is switched to PIPE_NOWAIT
  because blocking reads on a duplex pipe handle deadlock writes.
- Library list with per-movie status: Ready / Scanning / Not scanned.
  Plans created outside the app are noticed automatically (5 s poll).
- Controls: play/pause (space), stop, live-scrub seek bar (keyframe seeks
  while dragging at max ~8/s, exact seek on release), volume, fullscreen
  (Esc exits), time readout.
- Scan button runs `pureframe plan <file> --no-audio --no-clip` with
  HuggingFace forced offline once models are cached (their update pings
  rate-limit and have stalled scans). Warns if another scan is running.
  Interrupted scans resume from pureframe's checkpoint DB.
- Review flagged: opens pureframe's HTML contact sheet of detections.
- Heavy AI imports load lazily in a background thread after the window
  appears (cold-start freeze fix).

## 7. Subtitles

- Auto-loads subtitle files sitting next to the movie (`--sub-auto=fuzzy`).
- The Subtitle button attaches an external .srt/.ass/.ssa/.vtt/.sub; if the
  movie is playing it applies immediately (`sub-add`), otherwise on next
  play. Remembered per movie.
- Sync buttons − / + nudge subtitle delay by 0.25 s per click, applied live
  (`sub-delay`) and remembered per movie.

## 8. Casting (Chromecast)

- Cast button: discovers devices (pychromecast), picker if more than one.
- The stream is transcoded on the PC by ffmpeg with the censor filter
  **burned into the pixels** (x264 veryfast, aac, fragmented MP4 over a
  local HTTP server on a random port). The TV never receives an uncensored
  frame; there is nothing to bypass on the device.
- Casting mid-movie: censor timings are shifted by the start offset so
  boxes stay in sync (`casting.shift_intervals`).
- Fail closed: casting requires a finished plan (scan or completed live
  pass). Live-in-progress cannot be cast.
- Known limits: no seek/pause from the TV (live-generated stream); stop
  and re-cast instead. First cast may need a Windows Firewall allow for
  Python. Local playback stops when a cast starts (one pipeline at a time).

## 9. Files this app creates

| File | What it is |
|---|---|
| `<movie>.censorplan.json` | detection plan (shots, verdicts, boxes) |
| `library.json` | movie list, subtitle paths, subtitle delays |
| `current_play.conf` | mpv config with the filtergraph for this playback |
| `~\panns_data\*` | audio model files (legacy, audio analysis now off) |
| `~\.cache\huggingface\*` | CLIP model cache (legacy, CLIP now off) |

## 10. Privacy

Everything runs locally. The movie, frames, and audio never leave the
machine (cast streams go only to the chosen device on the LAN). The only
network traffic is one-time model downloads; scans run with HuggingFace
offline mode once models are cached. No telemetry anywhere in the stack
(pureframe's code was audited in-session: no network clients, no exec/eval,
no pickle loads outside torch's safe path).

## 11. Known limitations (honest list)

- Detection is NudeNet's accuracy: it can miss things (especially brief
  flashes under ~1/3 s in live mode's 3 fps sampling) and false-positive on
  skin (a shirtless torso may occasionally flag). Review flagged exists for
  auditing; the 5 s lead / 10 s hold absorb most timing misses.
- The pureframe batch scanner stalls on long videos on this machine
  (frozen after model load, twice). Live mode's scanner is the reliable
  path and produces the same plan; the Scan button remains for short files.
- Cast streams are not seekable (see section 8).
- One playback pipeline at a time: casting stops local playback.

## 12. Environment (already set up in this checkout)

- `.venv` with CUDA torch (`--index-url .../cu124`), `pureframe==0.1.0b15`,
  `opencv-python<5` (OpenCV 5 removed the Caffe API pureframe needs),
  `pychromecast`.
- mpv via `scoop install mpv`; ffmpeg on PATH (chocolatey).
- Desktop shortcut "PureFrame Player" runs
  `.venv\Scripts\pythonw.exe player_app.py`.

## 13. CLI (advanced, same engine as the app)

```powershell
.\.venv\Scripts\python.exe pfplay.py scan movie.mp4
.\.venv\Scripts\python.exe pfplay.py play movie.mp4 [--dump]
    [--style black|white|blur] [--strength light|medium|strong]
    [--blur nudity|kissing|all]   # what categories to censor
```

## 14. Tunables (constants at the top of the files)

| Where | Constant | Meaning | Current |
|---|---|---|---|
| pfplay.py | LEAD_BEFORE | censor lead before a run | 5 s |
| pfplay.py | HOLD_AFTER | censor hold after a run | 10 s |
| pfplay.py | BOX_MARGIN | box padding (plan mode) | 4% |
| pfplay.py | MAX_CHAINS | window cap before escalation | 400 |
| livescan.py | SAMPLE_FPS | live detection sampling | 3 fps |
| livescan.py | THRESHOLD | NudeNet confidence cutoff | 0.55 |
| livescan.py | PAD_BEFORE / PAD_AFTER | live lead / hold | 5 s / 10 s |
| livescan.py | MARGIN | box padding (live mode) | 8% |
