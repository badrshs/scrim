# Contributing to Scrim

Thanks for looking. A few things are worth knowing before you start, because
this project has some sharp edges that are load-bearing rather than accidental.

## The rule everything defers to

**An uncovered frame must never reach the screen or a cast device.**

When a change could go either way, it over-covers. A box bigger than it needed
to be is a cosmetic complaint; a frame that slipped through is the failure the
project exists to prevent. Pull requests that trade safety for polish will get
a "no", kindly.

Concretely, these are not negotiable:

- A missing, unreadable, or incomplete plan **refuses to play** in scanned-plan
  mode. It does not play uncovered with a warning.
- Live mode fences playback behind the detection frontier.
- Casting needs a complete scan.
- A filtergraph must be one ffmpeg will accept. mpv responds to a broken filter
  by dropping it and playing the movie **uncovered**, so an invalid graph is
  the worst possible outcome. See [docs/expression-limit.md](docs/expression-limit.md).

## Getting set up

You need Rust and the MSVC C++ build tools. No Node, no bundler.

```powershell
powershell -File tools/fetch-resources.ps1   # ~276 MB, hash-pinned
. .\tools\msvc-env.ps1                       # cl.exe onto PATH
cargo test --workspace
cargo build -p scrim
```

Two things catch everyone once:

1. **`ui/` is embedded at compile time.** Editing HTML, CSS or JS does nothing
   until you `cargo build` again.
2. **Don't pipe cargo through `2>&1` in Windows PowerShell.** It wraps stderr
   in ErrorRecords and reports a failure for a build that succeeded.

## Tests

```powershell
cargo test --workspace
```

Three suites are the spine of the project:

- **`crates/scrim-core/tests/golden.rs`** holds the covering engine to the
  Python it replaced, byte for byte, across all five cover styles.
- **`crates/scrim-core/tests/ffmpeg_accepts.rs`** hands every graph to the real
  ffmpeg. The golden tests cannot catch an invalid graph, because the reference
  implementation was producing one too.
- **`crates/scrim-detect/tests/parity.rs`** checks the Rust detector reproduces
  every explicit region the Python found.

If a change moves detections or windows, regenerate the fixtures with
`tools/regen_graphs.py` **and say why in the commit message**. Silently
overwriting them turns the strongest tests here into decoration.

Tests needing `resources/` or the sample movies skip cleanly without them, so a
bare checkout still runs the suite.

## Changing behaviour

`README.md` documents this application's complete behaviour and is treated as
part of the code. A change to what gets covered, when, the fences, casting, the
tunables, or the bundled dependencies updates the matching README section **in
the same commit**.

## Scope

Scrim covers explicit nudity and visible sex acts. Not kissing, not suggestive
scenes, not violence, and it never touches audio. Requests to widen that are a
change to what the product is, and belong in an issue before any code.

## Style

Match the surrounding code. Comments explain *why*, especially where something
looks wrong and is not: the double colour-channel swap in the detector and the
window cap in `scrim-core` are both cases where the obvious "fix" quietly
breaks covering, and both say so in place.
