//! Casting to a Chromecast, without ever handing it the original file.
//!
//! The device is pointed at a local HTTP server. That server does not serve
//! the movie: it serves the output of an ffmpeg transcode with the censor
//! filtergraph applied, so the cover is **burned into the pixels** before
//! anything leaves this machine.
//!
//! That is the whole design. If the device received the original file plus
//! instructions about what to hide, hiding it would be the device's decision,
//! and there would be something to bypass. This way there is not.
//!
//! Two rules follow from the fail-closed principle and are enforced by the
//! caller, not here:
//!
//!   * casting requires a **complete** plan, so a live scan in progress cannot
//!     be cast;
//!   * local playback stops first, because one heavy pipeline at a time is all
//!     the machine reliably handles.

#![forbid(unsafe_code)]

use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_cast::channels::media::{Media, StreamType};
use rust_cast::channels::receiver::CastDeviceApp;
use rust_cast::CastDevice;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const CHROMECAST_SERVICE: &str = "_googlecast._tcp.local.";
const DEFAULT_RECEIVER: &str = "CC1AD845";

#[derive(Debug, Clone, serde::Serialize)]
pub struct Device {
    pub name: String,
    pub host: String,
    pub port: u16,
}

/// Find Chromecasts on the local network.
pub fn discover(timeout: Duration) -> Result<Vec<Device>, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| format!("mDNS unavailable: {e}"))?;
    let receiver = daemon
        .browse(CHROMECAST_SERVICE)
        .map_err(|e| format!("mDNS browse failed: {e}"))?;

    let deadline = std::time::Instant::now() + timeout;
    let mut devices: Vec<Device> = Vec::new();

    while std::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(left) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let Some(addr) = info.get_addresses().iter().find(|a| a.is_ipv4()) else {
                    continue;
                };
                // "Living Room TV" rather than "Chromecast-abc123._googlecast..."
                let name = info
                    .get_property_val_str("fn")
                    .map(str::to_owned)
                    .unwrap_or_else(|| info.get_fullname().to_owned());
                let host = addr.to_string();
                if !devices.iter().any(|d| d.host == host) {
                    devices.push(Device { name, host, port: info.get_port() });
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let _ = daemon.shutdown();
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

/// Shift censor timings so they stay correct when casting from part way in.
///
/// ffmpeg is told to seek with `-ss`, so its clock restarts at zero while the
/// plan's timings are relative to the start of the movie. Ported from
/// `casting.py::shift_intervals`.
pub fn shift_windows(
    windows: &[scrim_window::Window],
    offset: f64,
) -> Vec<scrim_window::Window> {
    if offset <= 0.0 {
        return windows.to_vec();
    }
    windows
        .iter()
        .filter(|w| w.end > offset)
        .map(|w| scrim_window::Window {
            start: (w.start - offset).max(0.0),
            end: w.end - offset,
            ..*w
        })
        .collect()
}

/// Minimal shape so this crate does not depend on scrim-core just for a struct.
pub mod scrim_window {
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Window {
        pub start: f64,
        pub end: f64,
        pub x: i64,
        pub y: i64,
        pub w: i64,
        pub h: i64,
    }
}

pub struct CastConfig {
    pub ffmpeg: PathBuf,
    pub video: PathBuf,
    /// Censor filtergraph, already shifted for `start`.
    pub graph: String,
    pub start: f64,
}

/// A cast in progress: an HTTP server, an ffmpeg transcode, and a device.
pub struct CastSession {
    device_name: String,
    url: String,
    stop: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<Child>>>,
    device: Option<CastDevice<'static>>,
    transport: Option<String>,
    session: Option<i32>,
}

impl CastSession {
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Serve the censored transcode and tell the device to play it.
    pub fn start(target: &Device, cfg: CastConfig) -> Result<Self, String> {
        if !cfg.video.exists() {
            return Err("the movie file has moved or been deleted".into());
        }

        // Bind to every interface: the device has to reach us, and 127.0.0.1
        // is exactly the address it cannot use.
        let server = tiny_http::Server::http("0.0.0.0:0")
            .map_err(|e| format!("could not open the local stream server: {e}"))?;
        let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);

        let local_ip = local_ip_towards(&target.host)?;
        let url = format!("http://{local_ip}:{port}/stream.mp4");

        let stop = Arc::new(AtomicBool::new(false));
        let children: Arc<Mutex<Vec<Child>>> = Arc::new(Mutex::new(Vec::new()));

        {
            let stop = stop.clone();
            let children = children.clone();
            std::thread::spawn(move || serve(server, cfg, stop, children));
        }

        // Connect and hand over the URL.
        let device = CastDevice::connect_without_host_verification(target.host.clone(), target.port)
            .map_err(|e| format!("could not reach {}: {e}", target.name))?;

        device
            .connection
            .connect(DEFAULT_RECEIVER.to_string())
            .map_err(|e| format!("cast connect failed: {e}"))?;
        device.heartbeat.ping().ok();

        let app = device
            .receiver
            .launch_app(&CastDeviceApp::DefaultMediaReceiver)
            .map_err(|e| format!("could not start the receiver app: {e}"))?;

        device
            .connection
            .connect(app.transport_id.clone())
            .map_err(|e| format!("cast session connect failed: {e}"))?;

        let media = Media {
            content_id: url.clone(),
            content_type: "video/mp4".to_string(),
            stream_type: StreamType::Buffered,
            duration: None,
            metadata: None,
        };

        let status = device
            .media
            .load(app.transport_id.clone(), app.session_id.clone(), &media)
            .map_err(|e| format!("the device refused the stream: {e}"))?;

        Ok(Self {
            device_name: target.name.clone(),
            url,
            stop,
            children,
            transport: Some(app.transport_id),
            session: status.entries.first().map(|e| e.media_session_id),
            device: Some(device),
        })
    }

    pub fn stop_cast(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        if let (Some(device), Some(transport), Some(session)) =
            (&self.device, &self.transport, self.session)
        {
            let _ = device.media.stop(transport.as_str(), session);
        }
        self.device = None;

        for mut child in self.children.lock().unwrap().drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for CastSession {
    fn drop(&mut self) {
        self.stop_cast();
    }
}

/// Answer the device's request with a live transcode.
fn serve(
    server: tiny_http::Server,
    cfg: CastConfig,
    stop: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<Child>>>,
) {
    for request in server.incoming_requests() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if !request.url().starts_with("/stream") {
            let _ = request.respond(tiny_http::Response::empty(404));
            continue;
        }

        let Ok(mut child) = spawn_transcode(&cfg) else {
            let _ = request.respond(tiny_http::Response::empty(500));
            continue;
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            continue;
        };
        children.lock().unwrap().push(child);

        // Length is unknown because the transcode is still happening, so this
        // is a streaming response the device reads until we stop producing.
        let response = tiny_http::Response::new(
            tiny_http::StatusCode(200),
            vec![
                header("Content-Type", "video/mp4"),
                header("Cache-Control", "no-store"),
            ],
            stdout,
            None,
            None,
        );
        let _ = request.respond(response);
    }
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes())
        .expect("static header is well formed")
}

fn spawn_transcode(cfg: &CastConfig) -> Result<Child, String> {
    let mut cmd = Command::new(&cfg.ffmpeg);
    cmd.args(["-v", "error"]);

    // Seeking before -i is the fast path; the graph has already been shifted
    // to match, so the cover still lands on the right frames.
    if cfg.start > 1.0 {
        cmd.arg("-ss").arg(format!("{:.2}", cfg.start));
    }
    cmd.arg("-i").arg(&cfg.video);

    if cfg.graph.is_empty() {
        cmd.args(["-map", "0:v:0", "-map", "0:a:0?"]);
    } else {
        cmd.arg("-filter_complex")
            .arg(format!("[0:v]{}[vout]", cfg.graph))
            .args(["-map", "[vout]", "-map", "0:a:0?"]);
    }

    cmd.args([
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "21",
        "-maxrate", "8M", "-bufsize", "16M", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "160k", "-ac", "2",
        // Fragmented MP4 so playback can begin before the file exists.
        "-movflags", "frag_keyframe+empty_moov+default_base_moof",
        "-f", "mp4", "pipe:1",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn().map_err(|e| format!("could not run ffmpeg: {e}"))
}

/// The address on this machine that the given device can actually reach.
///
/// A machine with a VPN, a VM bridge, or several adapters has many addresses,
/// and most of them are useless to the television. Asking the routing table
/// which one it would use to reach that specific host picks the right one.
fn local_ip_towards(peer: &str) -> Result<IpAddr, String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("socket: {e}"))?;
    let target: SocketAddr = format!("{peer}:8009")
        .parse()
        .map_err(|_| format!("{peer} is not an address we can route to"))?;
    // UDP connect assigns a local address without sending anything.
    socket.connect(target).map_err(|e| format!("connect: {e}"))?;
    socket
        .local_addr()
        .map(|a| a.ip())
        .map_err(|e| format!("local_addr: {e}"))
}

/// Can the device be reached at all? Used to give a better error than a
/// timeout deep inside the cast protocol.
pub fn reachable(host: &str, port: u16, timeout: Duration) -> bool {
    format!("{host}:{port}")
        .parse::<SocketAddr>()
        .ok()
        .map(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
        .unwrap_or(false)
}

/// Drain a reader, used only by tests.
#[doc(hidden)]
pub fn drain(mut r: impl Read) -> usize {
    let mut buf = [0u8; 4096];
    let mut total = 0;
    while let Ok(n) = r.read(&mut buf) {
        if n == 0 {
            break;
        }
        total += n;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::scrim_window::Window;
    use super::*;

    fn w(start: f64, end: f64) -> Window {
        Window { start, end, x: 10, y: 20, w: 30, h: 40 }
    }

    #[test]
    fn shifting_drops_windows_entirely_behind_the_start_point() {
        let windows = vec![w(0.0, 10.0), w(50.0, 60.0), w(100.0, 110.0)];
        let shifted = shift_windows(&windows, 55.0);
        // The first is gone, the second is clipped, the third moves back.
        assert_eq!(shifted.len(), 2);
        assert_eq!((shifted[0].start, shifted[0].end), (0.0, 5.0));
        assert_eq!((shifted[1].start, shifted[1].end), (45.0, 55.0));
    }

    #[test]
    fn shifting_keeps_the_box_untouched() {
        // Only time moves. Moving the rectangle too would uncover the subject.
        let shifted = shift_windows(&[w(100.0, 110.0)], 50.0);
        assert_eq!((shifted[0].x, shifted[0].y, shifted[0].w, shifted[0].h), (10, 20, 30, 40));
    }

    #[test]
    fn no_offset_is_a_passthrough() {
        let windows = vec![w(0.0, 10.0), w(50.0, 60.0)];
        assert_eq!(shift_windows(&windows, 0.0), windows);
        assert_eq!(shift_windows(&windows, -5.0), windows);
    }

    #[test]
    fn a_window_ending_exactly_at_the_start_point_is_dropped() {
        // It covers nothing of what will be sent, and keeping it would produce
        // a zero-length window in the graph.
        assert!(shift_windows(&[w(10.0, 50.0)], 50.0).is_empty());
    }
}
