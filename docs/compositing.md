# Putting the interface on top of the picture

Scrim's design floats controls, a library drawer, and dialogs over the video,
with gradients and translucency. That requires the picture and the HTML to
share one visual surface, and on Windows that is not free.

## What was tried first, and why it failed

One window, mpv in a child HWND, a transparent WebView2 above it:

```text
Tauri window (transparent, WS_CLIPCHILDREN)
├─ WebView2 child HWND      the interface
└─ video child HWND         mpv --wid, forced to HWND_BOTTOM
```

The stage came out solid black.

The cause was isolated with a switch (`SCRIM_STAGE_TOP=1`) that flips the video
child between `HWND_TOP` and `HWND_BOTTOM`:

| video child z-order | result |
|---|---|
| `HWND_TOP` | picture visible, correctly positioned, filtergraph applied |
| `HWND_BOTTOM` | stage solid black |

So mpv was rendering correctly the whole time. WebView2 in windowed mode
composites through DWM against whatever is behind the **top-level window**, not
against sibling child HWNDs, so a child underneath it is simply painted over.
Its transparency cannot reveal a sibling.

## What Scrim does instead

Two top-level windows, with the video window owning the interface window:

```text
ScrimVideoStage    WS_POPUP, WS_EX_TOOLWINDOW, WS_EX_NOACTIVATE   <- mpv --wid
     ^ owns
Tauri window       transparent WebView2                           <- the interface
```

`SetWindowLongPtrW(ui, GWLP_HWNDPARENT, video)` makes the video window the
*owner* of the Tauri window. Windows guarantees an owned window stays above its
owner, which gives exactly the z-order needed without a global always-on-top
that would sit over unrelated applications.

The video window carries:

- `WS_EX_TOOLWINDOW` so it never appears in the taskbar or alt-tab. The user
  should never know it exists.
- `WS_EX_NOACTIVATE` so clicking near it cannot steal focus from the interface.
- a black background brush, so letterbox bars and the moment before the first
  frame match the stage instead of flashing white.

## Keeping them together

The interface measures its own stage rectangle and reports it through
`set_stage_bounds`, in CSS pixels plus `devicePixelRatio`. Rust converts to
physical pixels, maps through `ClientToScreen`, and moves the video window.

A window *move* changes no layout, so the interface never notices it. Rust
therefore remembers the last reported rectangle and re-applies it on both
`Moved` and `Resized`.

Verified by driving the interface window around with `MoveWindow`:

| | interface | video |
|---|---|---|
| start | 130,130 1456x909 | 138,171 1440x834 |
| move + resize | 400,200 1000x700 | 408,241 984x625 |
| again | 60,60 1300x850 | 68,101 1284x775 |

The video window stayed inside the interface's bounds throughout, inset by the
title bar and status bar.

## Consequences to remember

- **The CSS must not paint the stage.** `.app` and `.stage` are transparent;
  anything opaque declares its own background. `.stage.is-idle` paints only
  when there is nothing playing. A background on `.app` hides the movie, and
  that mistake looks exactly like the compositing being broken.
- Closing tears down in order: kill mpv, unhook `GWLP_HWNDPARENT`, then destroy
  the video window. Destroying an owner first can take the owned window with it.
- Fullscreen and minimise both need the video window carried along, since it is
  a separate top-level window rather than a child.
