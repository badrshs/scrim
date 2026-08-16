//! Reading a movie's dimensions, duration and frame rate.
//!
//! Scrim does not bundle ffprobe: it is a second 139 MB static binary that
//! duplicates ffmpeg entirely, and everything needed is already in the stream
//! report ffmpeg prints when asked to open a file with no output.

use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub width: i64,
    pub height: i64,
    pub duration: f64,
    pub fps: f64,
}

pub fn probe(ffmpeg: &Path, video: &Path) -> Result<VideoInfo, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-i")
        .arg(video)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;

    // ffmpeg exits non-zero here because no output file was given. That is
    // expected; the stream report is what we came for.
    let text = String::from_utf8_lossy(&out.stderr);
    parse(&text).ok_or_else(|| {
        format!(
            "could not read video properties from {}",
            video.file_name().unwrap_or_default().to_string_lossy()
        )
    })
}

/// Pull dimensions, duration and fps out of ffmpeg's stream report.
pub fn parse(text: &str) -> Option<VideoInfo> {
    let mut duration = None;
    let mut dims = None;
    let mut fps = None;

    for line in text.lines() {
        let t = line.trim();

        if duration.is_none() {
            if let Some(rest) = t.strip_prefix("Duration:") {
                let field = rest.split(',').next()?.trim();
                duration = parse_hms(field);
            }
        }

        // "Stream #0:0[0x1](und): Video: h264 (High) ..., 1280x720 [SAR 1:1
        //  DAR 16:9], 1257 kb/s, 23.98 fps, 23.98 tbr, 90k tbn"
        if t.contains(": Video:") {
            for field in t.split(',') {
                let field = field.trim();
                if dims.is_none() {
                    // Take the first WxH, ignoring any [SAR ...] that follows.
                    if let Some(d) = parse_dims(field.split_whitespace().next().unwrap_or(field)) {
                        dims = Some(d);
                    } else if let Some(d) = field.split_whitespace().find_map(parse_dims) {
                        dims = Some(d);
                    }
                }
                if fps.is_none() {
                    if let Some(v) = field.strip_suffix(" fps") {
                        fps = v.trim().parse::<f64>().ok();
                    }
                }
            }
        }
    }

    let (width, height) = dims?;
    Some(VideoInfo {
        width,
        height,
        duration: duration?,
        // Frame rate only labels the plan; nothing about coverage depends on
        // it, so a missing value is not worth refusing to scan over.
        fps: fps.unwrap_or(0.0),
    })
}

fn parse_dims(field: &str) -> Option<(i64, i64)> {
    let (w, h) = field.split_once('x')?;
    let w: i64 = w.parse().ok()?;
    let h: i64 = h
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()?;
    // Guard against matching things like "0x1" in a stream id.
    if w >= 16 && h >= 16 {
        Some((w, h))
    } else {
        None
    }
}

fn parse_hms(v: &str) -> Option<f64> {
    let mut parts = v.split(':');
    let h: f64 = parts.next()?.trim().parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_OUTPUT: &str = r#"
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'abc.mp4':
  Metadata:
    major_brand     : isom
  Duration: 01:01:37.00, start: 0.000000, bitrate: 1389 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1280x720 [SAR 1:1 DAR 16:9], 1257 kb/s, 23.98 fps, 23.98 tbr, 90k tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 127 kb/s (default)
"#;

    #[test]
    fn reads_the_real_ffmpeg_report_for_the_test_movie() {
        let info = parse(REAL_OUTPUT).expect("should parse");
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 720);
        assert_eq!(info.fps, 23.98);
        // 01:01:37 is 3697 seconds, which is what the fixture plan records.
        assert_eq!(info.duration, 3697.0);
    }

    #[test]
    fn the_stream_id_is_not_mistaken_for_dimensions() {
        // "0x1" and "0x31637661" both look like WxH if you are careless.
        let info = parse(REAL_OUTPUT).unwrap();
        assert_eq!((info.width, info.height), (1280, 720));
    }

    #[test]
    fn a_report_with_no_video_stream_is_refused() {
        let audio_only =
            "  Duration: 00:03:21.00, start: 0.000000\n  Stream #0:0: Audio: mp3, 44100 Hz\n";
        assert!(parse(audio_only).is_none());
    }
}
