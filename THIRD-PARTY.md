# Third-party components

Scrim ships as a self-contained folder, which means it redistributes other
people's work. This is the record of what, why, and under which licence.

Exact versions and SHA-256 hashes for everything in `resources/` are pinned in
[`resources/resources.lock.json`](resources/resources.lock.json), and
`tools/fetch-resources.ps1` refuses any download that does not match.

## Bundled binaries

| Component | Licence | Why Scrim needs it |
|---|---|---|
| [mpv](https://mpv.io) | GPL-2.0-or-later | Plays the movie, and applies the censor filtergraph live so the source file is never re-encoded. |
| [FFmpeg](https://ffmpeg.org) (BtbN gpl build) | GPL-3.0 | Extracts frames for scanning, and transcodes the cast stream with the cover burned into the pixels. |
| [ONNX Runtime](https://onnxruntime.ai) | MIT | Runs the detection model. |
| [NudeNet](https://github.com/notAI-tech/NudeNet) weights (`640m.onnx`) | **AGPL-3.0** | Detects explicit regions and returns bounding boxes. |

## Why Scrim is AGPL-3.0

The detection weights are the reason. NudeNet is published under AGPL-3.0, and
Scrim bundles the weights so that the app works on a machine with no network
access at all, which matters for something whose whole point is that your
movies never leave your computer.

Downloading the model at first run instead would have allowed a permissive
licence, but it would have meant the installer no longer works offline, and it
rests on the untested position that model weights are data rather than part of
the licensed work. Bundling and matching the licence is the honest option.

Practically, for a desktop video player, AGPL costs users nothing: its network
clause only applies to software offered to others over a network, and Scrim
never is. It does mean anyone distributing a modified Scrim must publish their
changes.

## Fonts

[IBM Plex Sans and IBM Plex Mono](https://github.com/IBM/plex), SIL Open Font
License 1.1, vendored through the `@fontsource` packages and served from the
application itself. Scrim never requests a font, or anything else, from a CDN.

## Rust dependencies

Enumerate the full tree with licences using:

```powershell
cargo install cargo-about
cargo about generate about.hbs > docs/licenses.html
```
