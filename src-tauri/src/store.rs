//! The library and the settings, on disk.
//!
//! Both live next to the user's other application data rather than beside the
//! executable, so a portable copy on a memory stick does not carry one
//! person's movie list to another machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Per-movie state the player remembers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MovieState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sub_delay: f64,
    /// Where the viewer stopped, so a long film can be resumed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub position: f64,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// `#[serde(default)]` on the container is load-bearing: a settings file
/// written by an older build has no entry for a field added since, and without
/// this the whole file fails to parse and everyone's configuration silently
/// resets to defaults on upgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// "light", "dark" or "system".
    pub theme: String,
    pub censor_style: String,
    pub volume: i64,
    /// Minutes detection runs ahead before live playback starts.
    pub head_start_minutes: u32,
    pub lead_before: f64,
    pub hold_after: f64,
    pub margin: f64,
    pub threshold: f64,
    /// Detections in a row a run needs before anything is covered.
    ///
    /// 1 covers every glimpse the detector had. 2 ignores runs seen in a
    /// single sampled frame, which is what most false positives look like.
    pub min_run: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            censor_style: "black_box".into(),
            volume: 100,
            head_start_minutes: 5,
            lead_before: 5.0,
            hold_after: 10.0,
            margin: 0.08,
            threshold: 0.55,
            min_run: 1,
        }
    }
}

impl Settings {
    /// Clamp anything a hand-edited settings file could put out of range.
    ///
    /// The threshold ceiling matters: on real footage genuine nudity scores
    /// around 0.55 to 0.72, so a value near 1.0 would quietly disable covering
    /// altogether while the interface still claimed to be working.
    pub fn sanitised(mut self) -> Self {
        self.volume = self.volume.clamp(0, 130);
        self.head_start_minutes = self.head_start_minutes.clamp(1, 10);
        self.lead_before = self.lead_before.clamp(0.0, 30.0);
        self.hold_after = self.hold_after.clamp(0.0, 60.0);
        self.margin = self.margin.clamp(0.0, 0.5);
        self.threshold = self.threshold.clamp(0.30, 0.90);
        self.min_run = self.min_run.clamp(1, 10);
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub movies: Vec<PathBuf>,
    #[serde(default)]
    pub state: BTreeMap<String, MovieState>,
}

pub struct Store {
    pub library: Library,
    pub settings: Settings,
    library_path: PathBuf,
    settings_path: PathBuf,
}

impl Store {
    pub fn load(library_path: PathBuf, settings_path: PathBuf) -> Self {
        if let Some(dir) = library_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut library: Library = read_json(&library_path).unwrap_or_default();
        // Movies that have been moved or deleted are dropped rather than left
        // in the list to fail on play.
        library.movies.retain(|p| p.exists());

        Self {
            library,
            settings: read_json(&settings_path).unwrap_or_default(),
            library_path,
            settings_path,
        }
    }

    pub fn save(&self) {
        write_json(&self.library_path, &self.library);
        write_json(&self.settings_path, &self.settings);
    }

    pub fn add(&mut self, paths: Vec<PathBuf>) -> usize {
        let before = self.library.movies.len();
        for p in paths {
            if p.exists() && !self.library.movies.contains(&p) {
                self.library.movies.push(p);
            }
        }
        self.library.movies.len() - before
    }

    pub fn remove(&mut self, path: &Path) {
        self.library.movies.retain(|p| p != path);
        self.library.state.remove(&key(path));
    }

    pub fn state_of(&self, path: &Path) -> MovieState {
        self.library
            .state
            .get(&key(path))
            .cloned()
            .unwrap_or_default()
    }

    pub fn update_state(&mut self, path: &Path, f: impl FnOnce(&mut MovieState)) {
        let entry = self.library.state.entry(key(path)).or_default();
        f(entry);
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    // Strip a UTF-8 byte order mark. serde_json treats one as a syntax error,
    // and plenty of things write them: PowerShell's `Set-Content -Encoding
    // utf8` does by default, as do several editors. Silently losing someone's
    // library to an invisible character is not acceptable.
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Ok(text) = serde_json::to_string_pretty(value) {
        // Write beside the target then rename, so a crash mid-write cannot
        // leave a truncated library behind.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Where a movie's plan lives: next to the movie, as before.
pub fn plan_path(video: &Path) -> PathBuf {
    let name = video.file_name().unwrap_or_default().to_string_lossy();
    video.with_file_name(format!("{name}.scrimplan.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_sit_next_to_the_movie_and_keep_its_full_name() {
        // "a.mkv" and "a.mp4" in one folder must not share a plan.
        let mkv = plan_path(Path::new(r"C:\films\a.mkv"));
        let mp4 = plan_path(Path::new(r"C:\films\a.mp4"));
        assert_ne!(mkv, mp4);
        assert!(mkv.to_string_lossy().ends_with("a.mkv.scrimplan.json"));
    }

    #[test]
    fn settings_round_trip_through_json() {
        let s = Settings::default();
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.threshold, 0.55);
        assert_eq!(back.head_start_minutes, 5);
        assert_eq!(back.theme, "system");
    }

    #[test]
    fn a_byte_order_mark_does_not_wipe_the_library() {
        let dir = std::env::temp_dir().join(format!("scrim-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.json");

        // Exactly what PowerShell's `Set-Content -Encoding utf8` produces.
        let json = "\u{feff}{\"movies\":[],\"state\":{\"x\":{\"subDelay\":0.5}}}";
        std::fs::write(&path, json).unwrap();

        let parsed: Option<Library> = read_json(&path);
        assert!(parsed.is_some(), "a BOM must not make the file unreadable");
        assert_eq!(parsed.unwrap().state.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_settings_file_from_a_future_version_still_loads() {
        // Unknown fields must not wipe someone's configuration.
        let text = r#"{"theme":"dark","censorStyle":"white_box","volume":80,
            "headStartMinutes":3,"leadBefore":5.0,"holdAfter":10.0,
            "margin":0.08,"threshold":0.55,"minRun":2,"somethingNew":true}"#;
        let s: Settings = serde_json::from_str(text).unwrap();
        assert_eq!(s.theme, "dark");
        assert_eq!(s.volume, 80);
        assert_eq!(s.min_run, 2);
    }

    #[test]
    fn a_settings_file_from_an_older_version_keeps_what_it_had() {
        // The upgrade case, which is the one that loses people's settings if
        // it is wrong: no minRun at all, because that build predates it.
        let text = r#"{"theme":"dark","censorStyle":"blur_medium","volume":70,
            "headStartMinutes":3,"leadBefore":2.0,"holdAfter":4.0,
            "margin":0.08,"threshold":0.55}"#;
        let s: Settings = serde_json::from_str(text).unwrap();
        assert_eq!(s.theme, "dark", "the theme they chose must survive");
        assert_eq!(s.censor_style, "blur_medium");
        assert_eq!(s.lead_before, 2.0);
        assert_eq!(s.min_run, 1, "the new field takes its default");
    }

    #[test]
    fn hand_edited_settings_are_clamped_to_something_workable() {
        // A threshold near 1.0 would disable covering while the interface
        // still claimed to be working, which is the worst kind of wrong.
        let wild = Settings {
            threshold: 0.99,
            min_run: 999,
            lead_before: -5.0,
            volume: 9999,
            ..Settings::default()
        }
        .sanitised();
        assert_eq!(wild.threshold, 0.90);
        assert_eq!(wild.min_run, 10);
        assert_eq!(wild.lead_before, 0.0);
        assert_eq!(wild.volume, 130);
    }
}
