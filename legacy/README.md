# legacy/

The original Python implementation that Scrim replaced.

**This is not shipped, not maintained, and not the application.** It is kept
for one reason: the golden test fixtures were produced by it, and a record of
what the code did is worth much more than a record of what someone remembers it
doing.

`tools/export_fixtures.py` and `tools/regen_graphs.py` import from here to
regenerate `crates/scrim-core/tests/fixtures/`. Deleting this directory would
make those fixtures unreproducible, which would quietly turn the strongest
tests in the project into unfalsifiable ones.

Running any of it needs the old environment: a virtualenv with `pureframe`,
`nudenet` and CUDA torch. Scrim itself needs none of that.

| File | What it was |
|---|---|
| `pfplay.py` | interval building and the ffmpeg filtergraph → `scrim-core` |
| `livescan.py` | the linear NudeNet scanner → `scrim-detect` |
| `player_app.py` | the Tkinter interface and the safety fences → `src-tauri` and `ui/` |
| `casting.py` | Chromecast and the censored transcode → `scrim-cast` |
