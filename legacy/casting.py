"""Chromecast support for PureFrame Player.

The censored video is transcoded live by ffmpeg (censor filter baked into
the pixels), served as fragmented MP4 over a tiny local HTTP server, and
the cast device is pointed at that URL. The device never sees the original
file, so the censoring cannot be bypassed on the TV side.
"""

import socket
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

CREATE_NO_WINDOW = 0x08000000


def discover(timeout: float = 6.0):
    """Return a list of Chromecast objects found on the network."""
    import pychromecast
    casts, browser = pychromecast.get_chromecasts(timeout=timeout)
    pychromecast.discovery.stop_discovery(browser)
    return casts


def _local_ip(peer_host: str) -> str:
    """The LAN IP the cast device can reach us on."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect((peer_host, 8009))
        return s.getsockname()[0]
    finally:
        s.close()


def shift_intervals(full, wins, offset: float):
    """Shift censor timings so they stay correct when casting mid-movie."""
    if offset <= 0:
        return full, wins
    f2 = [(max(0.0, s - offset), e - offset) for s, e in full if e > offset]
    w2 = [(max(0.0, s - offset), e - offset, x, y, w, h)
          for (s, e, x, y, w, h) in wins if e > offset]
    return f2, w2


class CastSession:
    def __init__(self, cast, video: Path, graph: str, start: float = 0.0):
        self.cast = cast
        self.video = Path(video)
        self.graph = graph
        self.start = max(0.0, start)
        self.httpd = None
        self._procs = []

    @property
    def device_name(self):
        return self.cast.cast_info.friendly_name

    def _spawn_ffmpeg(self):
        cmd = ["ffmpeg", "-v", "error"]
        if self.start > 1:
            cmd += ["-ss", f"{self.start:.2f}"]
        cmd += ["-i", str(self.video)]
        if self.graph:
            cmd += ["-filter_complex", f"[0:v]{self.graph}[vout]",
                    "-map", "[vout]", "-map", "0:a:0?"]
        else:
            cmd += ["-map", "0:v:0", "-map", "0:a:0?"]
        cmd += ["-c:v", "libx264", "-preset", "veryfast", "-crf", "21",
                "-maxrate", "8M", "-bufsize", "16M", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-b:a", "160k", "-ac", "2",
                "-movflags", "frag_keyframe+empty_moov+default_base_moof",
                "-f", "mp4", "pipe:1"]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL,
                                creationflags=CREATE_NO_WINDOW)
        self._procs.append(proc)
        return proc

    def start_cast(self) -> str:
        outer = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_GET(self):  # noqa: N802
                if not self.path.startswith("/stream"):
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "video/mp4")
                self.send_header("Cache-Control", "no-store")
                self.send_header("Connection", "close")
                self.end_headers()
                proc = outer._spawn_ffmpeg()
                try:
                    while True:
                        chunk = proc.stdout.read(64 * 1024)
                        if not chunk:
                            break
                        self.wfile.write(chunk)
                except (ConnectionError, OSError):
                    pass
                finally:
                    if proc.poll() is None:
                        proc.kill()

            def log_message(self, *args):  # silence request logging
                pass

        self.httpd = ThreadingHTTPServer(("0.0.0.0", 0), Handler)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        port = self.httpd.server_address[1]
        url = f"http://{_local_ip(self.cast.cast_info.host)}:{port}/stream.mp4"

        self.cast.wait()
        mc = self.cast.media_controller
        mc.play_media(url, "video/mp4", title=self.video.name)
        mc.block_until_active(timeout=10)
        return url

    def pause(self):
        self.cast.media_controller.pause()

    def resume(self):
        self.cast.media_controller.play()

    def stop(self):
        try:
            self.cast.media_controller.stop()
        except Exception:
            pass
        if self.httpd:
            threading.Thread(target=self.httpd.shutdown, daemon=True).start()
            self.httpd = None
        for p in self._procs:
            if p.poll() is None:
                p.kill()
        self._procs.clear()
