//! Finding the binaries and the model Scrim ships with.
//!
//! Scrim is meant to be copied to a machine that has nothing installed, so it
//! never looks on PATH for mpv or ffmpeg. It looks next to itself.
//!
//! Three layouts have to work: the installed app, a portable folder, and a
//! developer running `cargo run` from the repo.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub mpv: PathBuf,
    pub ffmpeg: PathBuf,
    pub model: PathBuf,
    pub onnxruntime: PathBuf,
    /// Where per-user state lives: library, settings, playback conf.
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissingResource {
    pub name: String,
    pub expected_at: String,
}

impl Paths {
    pub fn discover(data_dir: PathBuf) -> Self {
        let roots = search_roots();
        Self {
            mpv: find(&roots, "mpv.exe"),
            ffmpeg: find(&roots, "ffmpeg.exe"),
            model: find(&roots, "320n.onnx"),
            onnxruntime: find(&roots, "onnxruntime.dll"),
            data_dir,
        }
    }

    /// Everything that is missing, so the interface can say precisely what to
    /// do instead of failing later with a confusing error from ffmpeg.
    pub fn missing(&self) -> Vec<MissingResource> {
        [
            ("mpv", &self.mpv),
            ("ffmpeg", &self.ffmpeg),
            ("detection model", &self.model),
            ("ONNX Runtime", &self.onnxruntime),
        ]
        .into_iter()
        .filter(|(_, p)| !p.exists())
        .map(|(name, p)| MissingResource {
            name: name.to_string(),
            expected_at: p.display().to_string(),
        })
        .collect()
    }

    /// The mpv conf holding this playback's filtergraph.
    ///
    /// A file rather than a command line argument, because the graph runs to
    /// tens of thousands of characters.
    pub fn playback_conf(&self) -> PathBuf {
        self.data_dir.join("current_play.conf")
    }

    pub fn library_file(&self) -> PathBuf {
        self.data_dir.join("library.json")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("resources")); // installed and portable
            roots.push(dir.to_path_buf());
            // cargo puts the binary in target/debug, four levels under the repo
            for up in [2, 3] {
                if let Some(anc) = dir.ancestors().nth(up) {
                    roots.push(anc.join("resources"));
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("resources"));
    }
    roots
}

fn find(roots: &[PathBuf], name: &str) -> PathBuf {
    roots
        .iter()
        .map(|r| r.join(name))
        .find(|p| p.exists())
        .unwrap_or_else(|| {
            roots
                .first()
                .map(|r| r.join(name))
                .unwrap_or_else(|| Path::new(name).to_path_buf())
        })
}
