<div align="center">

# Scrim

**Watch movies with explicit content covered up. Everything runs on your own machine.**

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-4FE3C1.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows_10%2F11-8A959E.svg)](#installing)

</div>

Scrim plays a movie and covers explicit nudity with a black box, a white box,
or a blur, composited live as the film plays. The original file is never
modified and never re-encoded.

It is built around one rule, and every design decision in it defers to that
rule:

> **An uncovered frame must never reach the screen or a cast device.**

Where Scrim has to choose, it over-covers. A box that is bigger than it needed
to be is a cosmetic complaint. A frame that slipped through is the only bug
that matters.

---

## Contents

- [What gets covered](#what-gets-covered)
- [The two ways to watch](#the-two-ways-to-watch)
- [Timing](#timing)
- [How the covering works](#how-the-covering-works)
- [Casting](#casting)
- [Subtitles](#subtitles)
- [Privacy](#privacy)
- [Installing](#installing)
- [Building from source](#building-from-source)
- [How it is put together](#how-it-is-put-together)
- [Tests](#tests)
- [Settings](#settings)
- [Files Scrim creates](#files-scrim-creates)
- [Known limitations](#known-limitations)
- [Licence](#licence)

---

## What gets covered

Explicit nudity and visible sex acts. Nothing else.

Detection is the NudeNet model, running locally. These five classes trigger
covering, above a confidence of 0.55:

| | |
|---|---|
| `FEMALE_BREAST_EXPOSED` | `FEMALE_GENITALIA_EXPOSED` |
| `MALE_GENITALIA_EXPOSED` | `BUTTOCKS_EXPOSED` |
| `ANUS_EXPOSED` | |

**Not** covered, by deliberate decision: kissing, suggestive scenes, clothed
bodies, and violence. Audio is never analysed and never altered. Widening this
list is a change to what the product is, not a setting.

## The two ways to watch

### Scanned plan

1. Scan the movie once. Scrim walks it at 3 frames per second, roughly fifteen
   times faster than playback, and writes `<movie>.scrimplan.json` beside it.
2. Press play. The plan becomes a filtergraph and the movie starts immediately,
   fully covered from the first frame.

**Fails closed.** No plan, an unreadable plan, or a scan that never finished
means Scrim refuses to play in this mode. It will offer live detection instead
rather than guess.

### Live detection

For when you want to start now.

1. Detection starts, and playback waits for it to build a head start
   (1 to 10 minutes, 5 by default), or until the scan finishes, whichever comes
   first.
2. Newly found regions are pushed into the running movie every 20 seconds. No
   interruption, no restart.
3. Playback is **fenced** behind the detection frontier the whole time:

   ```
   ...........................|--- 15s ---|-5s-|
   safe to watch               pause here       frontier
                                                |
                     a seek past here snaps back to frontier - 30s
   ```

   - seek past `frontier - 5s` → snapped back to `frontier - 30s`
   - playback reaches `frontier - 15s` → pauses on its own
   - resumes once detection is 45 seconds ahead again
   - every fence lifts the moment the scan completes

4. A completed live pass saves a normal plan, so the next viewing starts
   instantly.

The seek bar shows all of this: covered spans as ticks, and the region
detection has not reached as a hatched area you can see and cannot enter.

## Timing

For each run of detections:

- covering starts **5 seconds before** the first detection
- covering holds **10 seconds after** the last one
- detections less than 1.5 seconds apart join into one run
- boxes are unioned over a window and grown by 8% of the frame on each side
- where windows overlap, the newer detection's position wins
- a flagged span with no usable box becomes a full-frame cover

Every one of these trades a larger box for a smaller chance of missing
something.

## How the covering works

One ffmpeg filtergraph, applied by mpv during playback:

```
split ─┬─────────────────────────────► overlay ──► out
       └─ crop(x(t), y(t)) ─► cover ─┘
```

A **single** chain whose crop and overlay coordinates move over time through
per-frame expressions. This matters: one chain per covered region (48 of them
on a real film) grinds mpv's filter bridge to a halt, while one moving chain
runs at 16-20x realtime.

There is a hard limit here worth knowing about. ffmpeg's expression parser
gives up at 99 levels of recursion, and this graph nests one level per covered
window, so Scrim caps windows at 90 and merges harder above that. This is not a
performance tuning knob: a graph ffmpeg rejects makes mpv drop the filter and
play the movie **uncovered**. See [docs/expression-limit.md](docs/expression-limit.md).

Changing the cover style while watching rebuilds the graph and applies it over
mpv's IPC socket. No restart, no rescan.

## Casting

Scrim casts to Chromecast by transcoding on this machine with the cover
**burned into the pixels**, and serving that over your local network.

The device never receives the original file. There is nothing on the
television to bypass, because the television is not making the decision.

- Requires a **finished** scan. A live scan in progress cannot be cast: the end
  of the movie has not been looked at, and a stream cannot be fenced once it has
  left this machine.
- Local playback stops first. One heavy pipeline at a time.
- Casting from part way in shifts the cover timings to match.
- The stream is not seekable from the TV. Stop and cast again instead.

## Subtitles

Files sitting next to the movie load automatically. The **SUB** button attaches
an `.srt`, `.ass`, `.ssa`, `.vtt` or `.sub` by hand, applied immediately if
something is playing. The sync buttons nudge timing by 0.25 s per press. Both
are remembered per movie.

## Privacy

Everything happens on your machine. The movie, the frames, and the audio never
leave it. Cast streams go only to the device you pick, on your own network.

Scrim makes **no** network requests of any kind: no telemetry, no update
checks, no model downloads at runtime, not even a web font. The typefaces are
part of the application.

## Installing

Download the installer from [Releases](../../releases), or take the portable
zip and run `scrim.exe` from anywhere. Nothing else is needed: Scrim carries
its own mpv, ffmpeg, ONNX Runtime and detection model.

Windows 10 or 11, 64-bit. WebView2 is already present on Windows 11 and on
current Windows 10; the installer fetches it if it is not.

## Building from source

You need the **Rust toolchain** and the **MSVC C++ build tools**. There is no
Node, no npm and no bundler: the interface is plain static files.

```powershell
git clone https://github.com/<you>/scrim
cd scrim

# mpv, ffmpeg, onnxruntime and the model (about 276 MB, hash-pinned)
powershell -File tools/fetch-resources.ps1

# cl.exe is only on PATH inside a developer prompt
. .\tools\msvc-env.ps1

cargo test --workspace
cargo build --release -p scrim
```

Bundle an installer and a portable folder:

```powershell
powershell -File tools/package.ps1
```

> **Note when changing the interface:** `ui/` is embedded into the executable
> at compile time, so editing HTML, CSS or JS requires `cargo build` before the
> change appears. This surprises everyone once.

## How it is put together

```
crates/
  scrim-core/     what gets covered: plan schema, window building, filtergraph
  scrim-detect/   ffmpeg frame pipe → NudeNet ONNX → detections
  scrim-mpv/      mpv process and its JSON IPC pipe
  scrim-cast/     Chromecast discovery and the censored transcode
src-tauri/        commands, playback state machine, the safety fences
ui/               the interface: HTML, CSS, plain modules
resources/        bundled mpv, ffmpeg, onnxruntime, model (not in git)
```

`scrim-core` decides what gets covered and depends on nothing but `serde`, so
all of it is testable from a JSON fixture with no video, no GPU and no window.

The picture and the interface live in **two** windows: mpv renders into its own
borderless window, which *owns* the interface window so Windows keeps the
interface above it. The obvious single-window arrangement does not work, and
[docs/compositing.md](docs/compositing.md) explains why in detail.

## Tests

```powershell
cargo test --workspace
```

Three of these are load-bearing:

**Golden filtergraphs.** The Python this project replaced was run over both
real test movies and its output recorded. The Rust must reproduce every window
and every filtergraph **byte for byte**, across all five cover styles. Shifting
the censor lead by 100 ms fails it immediately.

**ffmpeg acceptance.** Every graph Scrim can build is handed to the real ffmpeg
and must be accepted. The golden tests cannot catch an invalid graph, because
the Python was producing one too.

**Detector parity.** The Rust detector is run over the same frames as the
Python `nudenet`, and every explicit region the Python found must be
reproduced. Currently 32 of 32, mean IoU 0.9916.

Tests that need `resources/` or the sample movies skip cleanly without them.

## Settings

| Setting | Meaning | Default |
|---|---|---|
| Theme | light, dark, or follow the system | system |
| Lead before | start covering this long before a detection | 5.0 s |
| Hold after | keep covering this long after | 10.0 s |
| Live head start | how far ahead detection gets before playback | 5 min |
| Window cap | maximum covered windows in one graph | 90 |
| Confidence cutoff | NudeNet score below which a region is ignored | 0.55 |
| Live sampling | frames per second fed to the detector | 3 fps |
| Box margin | how much each box grows on every side | 8% |

Timing settings re-derive coverage instantly, because plans store raw
detections rather than pre-built windows. Changing the lead does not mean
rescanning a film.

**The video area stays dark in every theme.** Light theme applies to the window
chrome, library, settings and dialogs. A white control bar over a movie frame
blows out the picture and hurts in a dark room.

## Files Scrim creates

| File | Where | What |
|---|---|---|
| `<movie>.scrimplan.json` | next to the movie | detections from a scan |
| `library.json` | `%APPDATA%\app.scrim.player` | your movie list and per-movie state |
| `settings.json` | `%APPDATA%\app.scrim.player` | your settings |
| `current_play.conf` | `%APPDATA%\app.scrim.player` | the filtergraph for this playback |

## Known limitations

- **Detection is only as good as NudeNet.** It can miss things, especially
  flashes shorter than a third of a second at 3 fps sampling, and it
  occasionally flags a bare torso. The 5 s lead and 10 s hold absorb most
  timing misses, but this is a tool, not a guarantee about a specific film.
- **Dense films get larger boxes.** Above 90 covered windows Scrim merges into
  longer spans, so the box grows to the union of a longer stretch. Forced by
  ffmpeg's expression limit; the alternative is a graph that covers nothing.
- **Cast streams are not seekable** and cannot be paused from the television.
- **One pipeline at a time.** Casting stops local playback.
- **Windows only** for now. The core crates are platform-agnostic; the window
  embedding and the IPC pipe are not.

## Licence

**AGPL-3.0-or-later.** See [LICENSE](LICENSE).

Scrim bundles the NudeNet detection weights, which are AGPL-3.0, so Scrim is
too. For a desktop video player this costs users nothing: the network clause
only applies to software offered to others over a network, and Scrim never is.
It does mean anyone distributing a modified Scrim must publish their changes.

Bundled components and their licences are listed in
[THIRD-PARTY.md](THIRD-PARTY.md).
