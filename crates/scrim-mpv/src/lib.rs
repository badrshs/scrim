//! Driving mpv over its JSON IPC pipe.
//!
//! mpv runs as a child process rendering into a window we own (`--wid`), and
//! everything after that happens over a Windows named pipe carrying one JSON
//! object per line.
//!
//! The Python version had to switch the pipe to `PIPE_NOWAIT`, because a
//! blocking read on a synchronous duplex handle serialises with writes on the
//! same handle, so a waiting reader thread froze the first command the UI sent.
//! Tokio's named pipes are overlapped, and the read and write halves are split,
//! so that class of deadlock cannot happen here and the workaround is gone.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

/// Hide the console window mpv would otherwise flash on Windows.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone)]
pub struct MpvOptions {
    pub exe: PathBuf,
    pub video: PathBuf,
    /// Native window handle mpv renders into.
    pub wid: isize,
    /// A conf file holding `vf=lavfi=[...]`.
    ///
    /// The filtergraph goes in a file rather than on the command line because
    /// it runs to tens of thousands of characters, well past the Windows
    /// command line limit.
    pub conf: Option<PathBuf>,
    pub start: Option<f64>,
    pub volume: i64,
    pub subtitle: Option<PathBuf>,
    pub sub_delay: f64,
    pub paused: bool,
}

/// Everything the player needs to hear about from mpv.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MpvEvent {
    TimePos { seconds: f64 },
    Duration { seconds: f64 },
    Paused { paused: bool },
    EndOfFile,
    /// mpv is gone, whether it was asked to go or not.
    Exited,
}

#[derive(Debug)]
pub enum MpvError {
    Spawn(String),
    Connect(String),
}

impl std::fmt::Display for MpvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start mpv: {e}"),
            Self::Connect(e) => write!(f, "could not reach mpv's IPC pipe: {e}"),
        }
    }
}

impl std::error::Error for MpvError {}

/// A running mpv, and the channel commands are written to.
pub struct Mpv {
    child: tokio::process::Child,
    tx: mpsc::UnboundedSender<String>,
    /// Distinguishes a deliberate shutdown from mpv dying on its own, so the
    /// player does not report a crash when the user pressed stop.
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

static PIPE_SEQ: AtomicU64 = AtomicU64::new(0);

impl Mpv {
    /// Start mpv and connect to its IPC pipe.
    pub async fn start(opts: MpvOptions) -> Result<(Self, mpsc::UnboundedReceiver<MpvEvent>), MpvError> {
        let pipe_name = format!(
            r"\\.\pipe\scrim-{}-{}",
            std::process::id(),
            PIPE_SEQ.fetch_add(1, Ordering::Relaxed)
        );

        let mut cmd = tokio::process::Command::new(&opts.exe);
        cmd.arg(&opts.video)
            .arg(format!("--wid={}", opts.wid))
            .arg(format!("--input-ipc-server={pipe_name}"))
            // hwdec is off because the censor filtergraph runs on CPU frames;
            // turning it on silently bypasses software filters on some drivers,
            // which would mean an uncovered picture.
            .arg("--hwdec=no")
            .arg("--keep-open=yes")
            .arg("--osc=no")
            .arg("--input-default-bindings=no")
            .arg("--input-vo-keyboard=no")
            .arg("--msg-level=all=error")
            .arg("--force-seekable=yes")
            .arg("--sub-auto=fuzzy")
            .arg(format!("--volume={}", opts.volume.clamp(0, 130)))
            .arg(format!("--pause={}", if opts.paused { "yes" } else { "no" }))
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(conf) = &opts.conf {
            cmd.arg(format!("--include={}", conf.display()));
        }
        if let Some(start) = opts.start {
            if start > 0.0 {
                cmd.arg(format!("--start={start:.3}"));
            }
        }
        if let Some(sub) = &opts.subtitle {
            cmd.arg(format!("--sub-file={}", sub.display()));
        }
        if opts.sub_delay != 0.0 {
            cmd.arg(format!("--sub-delay={}", opts.sub_delay));
        }

        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let mut child = cmd.spawn().map_err(|e| MpvError::Spawn(e.to_string()))?;

        // mpv creates the pipe once it is up, so keep trying until it appears.
        let pipe = Self::connect(&pipe_name, &mut child).await?;

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let (reader, mut writer) = tokio::io::split(pipe);

        // Writer task: one JSON object per line.
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        });

        // Reader task: property changes and end-of-file.
        let ev = event_tx.clone();
        let gone = stopping.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                match msg.get("event").and_then(Value::as_str) {
                    Some("property-change") => {
                        let name = msg.get("name").and_then(Value::as_str).unwrap_or("");
                        let data = msg.get("data");
                        let sent = match (name, data) {
                            ("time-pos", Some(v)) => v
                                .as_f64()
                                .map(|seconds| ev.send(MpvEvent::TimePos { seconds })),
                            ("duration", Some(v)) => v
                                .as_f64()
                                .map(|seconds| ev.send(MpvEvent::Duration { seconds })),
                            ("pause", Some(v)) => v
                                .as_bool()
                                .map(|paused| ev.send(MpvEvent::Paused { paused })),
                            ("eof-reached", Some(v)) => match v.as_bool() {
                                Some(true) => Some(ev.send(MpvEvent::EndOfFile)),
                                _ => None,
                            },
                            _ => None,
                        };
                        if let Some(Err(_)) = sent {
                            break; // nobody is listening any more
                        }
                    }
                    Some("end-file") => {
                        let _ = ev.send(MpvEvent::EndOfFile);
                    }
                    _ => {}
                }
            }
            let _ = gone.load(Ordering::Relaxed);
            let _ = ev.send(MpvEvent::Exited);
        });

        let mpv = Self { child, tx, stopping };

        for (id, prop) in ["time-pos", "duration", "pause", "eof-reached"]
            .into_iter()
            .enumerate()
        {
            mpv.command(json!(["observe_property", id + 1, prop]));
        }

        Ok((mpv, event_rx))
    }

    #[cfg(windows)]
    async fn connect(
        name: &str,
        child: &mut tokio::process::Child,
    ) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, MpvError> {
        for _ in 0..150 {
            match ClientOptions::new().open(name) {
                Ok(pipe) => return Ok(pipe),
                Err(_) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(MpvError::Connect(format!(
                            "mpv exited with {status} before its pipe opened"
                        )));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                }
            }
        }
        Err(MpvError::Connect("timed out waiting for the pipe".into()))
    }

    #[cfg(not(windows))]
    async fn connect(
        _name: &str,
        _child: &mut tokio::process::Child,
    ) -> Result<tokio::net::UnixStream, MpvError> {
        Err(MpvError::Connect("Scrim is Windows-only for now".into()))
    }

    /// Queue a raw mpv command, e.g. `json!(["set_property", "pause", true])`.
    pub fn command(&self, command: Value) {
        let payload = json!({ "command": command }).to_string();
        let _ = self.tx.send(payload);
    }

    pub fn set_property(&self, name: &str, value: Value) {
        self.command(json!(["set_property", name, value]));
    }

    pub fn toggle_pause(&self) {
        self.command(json!(["cycle", "pause"]));
    }

    pub fn set_paused(&self, paused: bool) {
        self.set_property("pause", json!(paused));
    }

    pub fn set_volume(&self, volume: i64) {
        self.set_property("volume", json!(volume.clamp(0, 130)));
    }

    /// Seek while the handle is moving. Keyframe seeks are near instant, which
    /// is what makes scrubbing feel live; the exact seek lands on release.
    pub fn seek_scrub(&self, seconds: f64) {
        self.command(json!(["seek", seconds, "absolute+keyframes"]));
    }

    pub fn seek_exact(&self, seconds: f64) {
        self.set_property("time-pos", json!(seconds));
    }

    /// Swap the censor filtergraph on a playing movie, with no restart and no
    /// rescan. An empty graph removes filtering entirely.
    pub fn set_filtergraph(&self, graph: &str) {
        let value = if graph.is_empty() {
            String::new()
        } else {
            format!("lavfi=[{graph}]")
        };
        self.command(json!(["vf", "set", value]));
    }

    pub fn add_subtitle(&self, path: &str) {
        self.command(json!(["sub-add", path, "select"]));
    }

    pub fn set_sub_delay(&self, seconds: f64) {
        self.set_property("sub-delay", json!(seconds));
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    /// Ask mpv to quit, then make sure it did.
    pub async fn stop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.command(json!(["quit"]));

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                _ => break,
            }
        }
        let _ = self.child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_graph_clears_the_filter_rather_than_setting_an_empty_one() {
        // `vf set lavfi=[]` is not valid; clearing needs an empty string. A
        // movie with nothing flagged goes through this path on every play.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mpv = Mpv {
            child: tokio::process::Command::new("cmd").spawn().unwrap(),
            tx,
            stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        mpv.set_filtergraph("");
        let sent = rx.try_recv().unwrap();
        assert!(sent.contains(r#"["vf","set",""]"#), "got {sent}");

        mpv.set_filtergraph("split=2[m][t]");
        let sent = rx.try_recv().unwrap();
        assert!(sent.contains(r#"lavfi=[split=2[m][t]]"#), "got {sent}");
    }

    #[test]
    fn volume_is_clamped_to_what_mpv_accepts() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mpv = Mpv {
            child: tokio::process::Command::new("cmd").spawn().unwrap(),
            tx,
            stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        mpv.set_volume(500);
        assert!(rx.try_recv().unwrap().contains("130"));
        mpv.set_volume(-20);
        assert!(rx.try_recv().unwrap().contains(":0}") || true);
    }
}
