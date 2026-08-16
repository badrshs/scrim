//! Scrim's censor plan, schema version 1.
//!
//! A plan records what the detector *found*, not what the player will *cover*.
//! Those are different things, and keeping them separate matters: the lead,
//! hold, and margin tunables are editable in Settings, so storing raw
//! detections means changing one re-derives the coverage instantly instead of
//! forcing a rescan of the whole movie.
//!
//! Box coordinates are in source pixels, already scaled up from whatever
//! smaller frame the detector actually ran on.

use serde::{Deserialize, Serialize};

/// Written by this build.
///
/// v2 added the label and confidence behind every box, so the player can say
/// *why* it is covering something instead of only that it is. v1 plans still
/// load; they simply have no reason to report.
pub const SCHEMA_VERSION: u32 = 2;
/// The oldest plan this build will read.
pub const MIN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub schema_version: u32,
    #[serde(default)]
    pub generator: String,
    #[serde(default)]
    pub created_at: String,
    pub source: Source,
    pub detector: Detector,
    /// False while a live scan is still running. A plan that is not complete
    /// must never be used for casting, and playback past its frontier must be
    /// fenced. See the fail-closed rules in the player.
    #[serde(default)]
    pub complete: bool,
    pub detections: Vec<Detection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size_bytes: u64,
    pub duration: f64,
    pub fps: f64,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detector {
    pub sample_fps: f64,
    pub threshold: f64,
    #[serde(default)]
    pub detect_width: i64,
    #[serde(default)]
    pub detect_height: i64,
}

/// One sampled frame that had at least one explicit region in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    /// Seconds from the start of the movie.
    pub t: f64,
    pub boxes: Vec<DetBox>,
}

/// One region the detector found, and what it thought it was.
///
/// The label and score are kept so the player can explain itself. "Covering
/// because FEMALE_BREAST_EXPOSED scored 0.56 at 41:12" is something a viewer
/// can judge; an unexplained black box is not.
#[derive(Debug, Clone, PartialEq)]
pub struct DetBox {
    /// `[x1, y1, x2, y2]` in source pixels.
    pub bounds: [i64; 4],
    pub label: String,
    pub score: f64,
}

impl DetBox {
    pub fn width(&self) -> i64 {
        self.bounds[2] - self.bounds[0]
    }
    pub fn height(&self) -> i64 {
        self.bounds[3] - self.bounds[1]
    }
}

impl Serialize for DetBox {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("DetBox", 3)?;
        st.serialize_field("box", &self.bounds)?;
        st.serialize_field("label", &self.label)?;
        st.serialize_field("score", &self.score)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for DetBox {
    /// Accepts both shapes, so a plan written by an older build still loads:
    ///
    ///   v1:  [12, 34, 56, 78]
    ///   v2:  {"box": [12, 34, 56, 78], "label": "...", "score": 0.61}
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Labelled {
                #[serde(rename = "box")]
                bounds: [i64; 4],
                #[serde(default)]
                label: String,
                #[serde(default)]
                score: f64,
            },
            Bare([i64; 4]),
        }

        Ok(match Raw::deserialize(d)? {
            Raw::Labelled {
                bounds,
                label,
                score,
            } => Self {
                bounds,
                label,
                score,
            },
            // A v1 box carries no reason. Score 0 would read as "barely
            // detected", which is worse than admitting it is unknown, so
            // callers check `label.is_empty()` instead.
            Raw::Bare(bounds) => Self {
                bounds,
                label: String::new(),
                score: 0.0,
            },
        })
    }
}

/// Why a plan cannot be trusted. Every one of these means refuse to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    UnsupportedSchema(u32),
    BadDimensions,
    BadDuration,
    DetectionOutOfRange,
    MalformedBox,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema(v) => {
                write!(f, "plan schema version {v} is not supported by this build")
            }
            Self::BadDimensions => write!(f, "plan has non-positive frame dimensions"),
            Self::BadDuration => write!(f, "plan has a non-positive duration"),
            Self::DetectionOutOfRange => {
                write!(f, "plan has a detection outside the movie's duration")
            }
            Self::MalformedBox => write!(f, "plan has a box with inverted or negative bounds"),
        }
    }
}

impl std::error::Error for PlanError {}

impl Plan {
    /// Fail closed. Anything questionable here refuses playback rather than
    /// risking a frame slipping past, so this is intentionally strict.
    pub fn validate(&self) -> Result<(), PlanError> {
        if self.schema_version < MIN_SCHEMA_VERSION || self.schema_version > SCHEMA_VERSION {
            return Err(PlanError::UnsupportedSchema(self.schema_version));
        }
        if self.source.width <= 0 || self.source.height <= 0 {
            return Err(PlanError::BadDimensions);
        }
        if !(self.source.duration.is_finite() && self.source.duration > 0.0) {
            return Err(PlanError::BadDuration);
        }
        for d in &self.detections {
            if !d.t.is_finite() || d.t < 0.0 || d.t > self.source.duration + 1.0 {
                return Err(PlanError::DetectionOutOfRange);
            }
            for b in &d.boxes {
                let [x1, y1, x2, y2] = b.bounds;
                if x1 < 0 || y1 < 0 || x2 <= x1 || y2 <= y1 {
                    return Err(PlanError::MalformedBox);
                }
            }
        }
        Ok(())
    }

    /// Total seconds of the movie that carry at least one detection frame.
    pub fn detection_count(&self) -> usize {
        self.detections.len()
    }
}
