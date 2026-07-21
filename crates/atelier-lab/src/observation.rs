//! Observations (lab.md item 9): what the policy sees, at two cost levels.
//!
//! Light runs after every action — the indexed raster is the cheap exact
//! state. Full runs only at candidate-selection points; it adds renders and
//! the expensive audits, which must not fire per small edit.
//!
//! Everything here is structured serde data with one deliberate exception:
//! the audit reports (`palette_report`, `components`, `critique`) stay
//! `serde_json::Value`. Their shape is atelier's own versioned output —
//! mirroring it field-by-field here would fork the schema and rot.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::action::Stage;

/// How much state an `observe` call assembles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationLevel {
    Light,
    Full,
}

/// One layer in a light observation: its meta plus its indexed raster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerObservation {
    pub index: usize,
    pub name: String,
    pub visible: bool,
    pub opacity: u8,
    /// Row-major palette indices of the frame-0 cel (`None` = transparent).
    /// Empty when the cel could not be indexed — off-palette pixels; see
    /// `IntegrityChecks::on_palette`.
    pub indices: Vec<Option<u32>>,
}

/// The cheap sanity checks a light observation always carries (lab.md item
/// 9, "basic integrity checks") — the invariants every later phase assumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityChecks {
    /// Every opaque pixel of every layer matched a palette swatch exactly.
    /// False means the indexed rasters in this observation are unreliable
    /// (empty) and the episode needs a snap before it can be trusted.
    pub on_palette: bool,
    /// Palette length within the task's `max_colors` budget.
    pub palette_within_budget: bool,
    /// Total opaque pixels across all layers (frame 0).
    pub opaque_pixels: usize,
}

/// After-every-action state (lab.md item 9): the indexed raster, palette,
/// layers, stage, recent actions, and integrity checks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LightObservation {
    pub doc_id: String,
    pub width: u32,
    pub height: u32,
    pub palette: Vec<[u8; 4]>,
    pub layers: Vec<LayerObservation>,
    pub stage: Stage,
    /// One-line summaries of the most recent accepted actions, oldest first.
    pub recent_actions: Vec<String>,
    pub integrity: IntegrityChecks,
}

/// PNG byte streams of the same frame at four views (lab.md item 9). Raw
/// bytes, not base64: the artifact store (Phase 2, item 10) hashes bytes —
/// encoding is a transport concern, not state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renders {
    /// Flattened frame at native resolution (scale 1).
    pub native: Vec<u8>,
    /// Nearest-neighbour upscale (scale 8) — what a human judges.
    pub enlarged: Vec<u8>,
    /// Value/grayscale view (native size).
    pub grayscale: Vec<u8>,
    /// Notan 3-value massing view (upscaled) — the silhouette read.
    pub notan: Vec<u8>,
}

/// Document-level facts pulled from `doc_info`, typed so the env and later
/// phases don't re-parse the structure blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMetadata {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub layer_count: usize,
    pub frame_count: usize,
    pub palette_len: usize,
}

/// Candidate-selection state: everything Light has, plus renders and the
/// full audit battery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FullObservation {
    pub light: LightObservation,
    pub renders: Renders,
    /// `doc_palette_report` output (atelier's shape — see module docs).
    pub palette_report: Value,
    /// `doc_components` output.
    pub components: Value,
    /// `doc_critique` output.
    pub critique: Value,
    pub doc: DocMetadata,
}

/// The observation sum type the env hands back. Full is boxed: it carries
/// four PNG byte streams next to Light's flat raster, and observations pass
/// through every transition — the box keeps `Observation` cheap to move.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Observation {
    Light(LightObservation),
    Full(Box<FullObservation>),
}

impl Observation {
    /// The light half of any observation — cheap access for callers that
    /// don't care which level they hold.
    pub fn light(&self) -> &LightObservation {
        match self {
            Observation::Light(l) => l,
            Observation::Full(f) => &f.light,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_serde_roundtrip_keeps_the_level_tag() {
        let light = LightObservation {
            doc_id: "d".into(),
            width: 2,
            height: 2,
            palette: vec![[1, 2, 3, 255]],
            layers: vec![LayerObservation {
                index: 0,
                name: "Layer 1".into(),
                visible: true,
                opacity: 255,
                indices: vec![Some(0), None, None, Some(0)],
            }],
            stage: Stage::Silhouette,
            recent_actions: vec!["set_palette 1 colours".into()],
            integrity: IntegrityChecks {
                on_palette: true,
                palette_within_budget: true,
                opaque_pixels: 2,
            },
        };
        let obs = Observation::Light(light.clone());
        let v = serde_json::to_value(&obs).unwrap();
        assert_eq!(v["level"], "light");
        let back: Observation = serde_json::from_value(v).unwrap();
        assert_eq!(back, obs);
        assert_eq!(back.light(), &light);
    }
}
