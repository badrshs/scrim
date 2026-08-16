# Project rules for Scrim

## README is the source of truth, keep it in sync
`README.md` documents this application's complete behaviour. **Any change to
what gets covered, when it gets covered, the scan pipeline, the fences,
casting, the tunables, file formats, or the bundled dependencies MUST update
the matching section of `README.md` in the same change.** New tunable, new row
in the settings table. New limitation, new bullet in the known-limitations
list. Never let the README describe behaviour the code no longer has.

## Non-negotiable design rule: fail closed
An uncovered frame must never reach the screen or a cast device. When in
doubt, over-cover (full frame) rather than risk showing anything.

- Missing, unreadable, or incomplete plan means **refuse to play** in
  scanned-plan mode.
- Live mode must never allow playback or seeking past the detection frontier.
- Casting requires a **complete** plan. A live scan in progress cannot be cast.
- A filtergraph that ffmpeg might reject is worse than no feature at all: mpv
  answers a broken filter by dropping it and playing the movie uncovered.
  Anything that changes graph size must keep `tests/ffmpeg_accepts.rs` passing.

## Scope of covering
Only explicit nudity and visible sex acts (the five `EXPLICIT_LABELS` in
`scrim-detect`). No kissing detection, no audio analysis, the audio track is
never modified. Do not widen this without the owner asking.

## Practical notes
- **`ui/` is embedded into the binary at compile time.** Editing HTML, CSS or
  JS does nothing until `cargo build` runs. This catches everyone once.
- `cl.exe` is only on PATH inside a developer prompt. Dot-source
  `tools/msvc-env.ps1` before any cargo command.
- Do not pipe cargo through `2>&1` in Windows PowerShell: it wraps stderr in
  ErrorRecords and reports a failure for a successful build. Redirect with
  `cmd /c "... > log 2>&1"` or let the output through.
- The single-chain filtergraph is a hard constraint: one crop/cover/overlay
  chain whose coordinates move via time expressions. Chains-per-window kills
  mpv's filter throughput (measured).
- The picture and the interface are two top-level windows, the video window
  owning the interface window. See `docs/compositing.md` before touching
  window code. `.app` and `.stage` must never paint a background, or the movie
  disappears behind the interface.
- Scans must produce plans identical to the reference implementation. If a
  change moves detections or windows, the golden fixtures need regenerating
  **and the reason recorded**, not silently overwritten.
- Test with the real files here: `sample.mp4` (2 min, nothing flagged) and
  `abc.mp4` (61 min, 87 covered windows).
- `legacy/` holds the original Python. It is not shipped and not maintained; it
  exists so `tools/export_fixtures.py` and `tools/regen_graphs.py` can
  regenerate the golden fixtures from the implementation they came from.
