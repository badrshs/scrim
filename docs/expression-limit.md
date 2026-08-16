# The ffmpeg expression limit, and why Scrim caps censor windows at 90

## What happened

The first end-to-end run of Scrim played `abc.mp4` and mpv printed:

```
[ffmpeg] Eval: Missing ')' or too many args in 'if(between(t,2333.000,2335.000),228,if(...
```

The filtergraph was rejected outright. mpv's response to a filter it cannot
build is to carry on without it, which for this application means **a movie
that should have been covered plays completely uncovered**. It is the worst
failure mode the project has, and it happened on the very first real movie.

## Why

The graph positions one moving crop box using a nested conditional, one level
per censor window:

```
if(between(t,s1,e1),x1, if(between(t,s2,e2),x2, if(... ,0)))
```

ffmpeg's expression evaluator (`libavutil/eval.c`) parses by recursive descent
with a fixed budget of 100 levels. Every nested `if(` consumes one.

Measured on the bundled ffmpeg build, using the exact expression shape Scrim
emits:

| nested `if()` | parses |
|---|---|
| 98 | yes |
| 99 | **no** |

A real linear scan of a 61 minute film produces **319** windows, so the graph
nested 319 deep and was thrown out.

Flat forms do not help. `between(...)*x + between(...)*y + ...` spends the same
budget on the `+` chain, and worse, because each multiplication adds a level:

| form | terms | parses |
|---|---|---|
| `between()+between()+...` | 100 | yes |
| `between()*v+between()*v+...` | 100 | **no** |

The ceiling is the recursion budget itself, not the operator.

## Why the Python never hit it

`livescan.py` set `MAX_WINDOWS = 380` and `pfplay.py` set `MAX_CHAINS = 400`,
both far above the real limit. The cap was never reached in practice because
the pureframe scanner produced about 66 windows for a feature film, comfortably
under 98. The linear scanner that replaced it produces 319 for the same movie,
which is what exposed the latent bug.

So this was never a safe limit. It was an untested one that happened not to
fire.

## The fix

`WindowParams::max_windows` is **90**, with eight levels of headroom under the
measured cliff.

When a scan produces more windows than the cap, the window length doubles and
the windows are rebuilt, repeatedly, until the count fits. Merging harder means
each box is the union of detections over a longer span, so the covered
rectangle grows. For `abc.mp4`:

| cap | windows | merge window | graph | ffmpeg |
|---|---|---|---|---|
| 380 (old) | 319 | 2 s | 55,899 chars | rejected |
| **90** | **87** | **8 s** | **15,349 chars** | **accepted** |
| 80 | 49 | 16 s | 8,713 chars | accepted |

Bigger boxes are a cosmetic cost. An unparseable graph is an uncovered movie.
The trade goes one way.

## The tests that hold it

`crates/scrim-core/tests/ffmpeg_accepts.rs` runs the real ffmpeg over every
graph Scrim can build, for all five censor styles and both test movies. The
golden tests could never have caught this: they prove the string matches the
Python reference, and the Python was producing an invalid string too.

There is also a test asserting that a graph built past the cap **is** rejected.
If a future ffmpeg raises its budget, that test fails and the cap can be
revisited on evidence instead of guesswork.

## If the boxes ever feel too large

Options, in rough order of preference:

1. Split the windows across a small number of parallel chains, each under the
   limit. The original notes warn that one chain per window (48 of them) grinds
   mpv's filter bridge to a halt, but three or four chains is a different scale
   and worth measuring.
2. Pre-render the coverage into a sidecar file and drive the position from it.
3. Accept longer merge windows on dense films only, which is what happens now.
