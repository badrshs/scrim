# resources/

The third-party binaries Scrim bundles so it runs on a machine with nothing
installed. They are not checked into git; fetch them with:

```powershell
pwsh tools/fetch-resources.ps1
```

| File | What it does |
|---|---|
| `mpv.exe` | plays the movie with the censor filtergraph applied live |
| `ffmpeg.exe` | extracts frames for scanning, transcodes the cast stream |
| `ffprobe.exe` | reads duration, fps, and dimensions before a scan |
| `onnxruntime.dll` | runs the detection model |
| `320n.onnx` | NudeNet detector weights |

Versions and SHA-256 hashes are pinned in `resources.lock.json`, and the
fetcher refuses anything that does not match. Licences are explained in
[THIRD-PARTY.md](../THIRD-PARTY.md).

`320n.onnx` specifically is the model the Python `nudenet` package loads by
default, and therefore the one the golden test fixtures were generated with.
Substituting `640m.onnx` would change every detection, so the choice is pinned
rather than incidental.
