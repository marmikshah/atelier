//! Deterministic replay with an exact-pixel gate (lab.md item 12).
//!
//! Replay rebuilds an episode from its event log in a FRESH env: the same
//! compile+dispatch path the original run took (never the raw journal, which
//! lacks results and reads), then two levels of verification:
//!
//! - after every replayed step, the new indexed raster is compared against
//!   the recorded `observation_after` — so a report names the FIRST
//!   divergent step instead of just failing at the end;
//! - at the end, the rebuilt final render is compared PIXEL-EXACT against
//!   the original episode's stored final-image artifact.
//!
//! Checkpoint/restore events are honoured (with recorded→minted id remaps,
//! the same discipline the journal replayer uses) — skipping a restore would
//! replay rolled-back edits and break pixel equality by construction.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::artifacts::{ArtifactStore, ARTIFACTS_DIR};
use crate::env::{AtelierEnv, PixelArtEnv};
use crate::observation::{LightObservation, Observation, ObservationLevel};
use crate::recorder::{EventKind, Recorder, EPISODE_LOG_FILE};
use crate::storage::Storage;

/// The outcome of a replay: how much was replayed, and where it diverged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub session_id: String,
    /// Events read from the log.
    pub events: usize,
    /// Accepted steps re-executed (rejected ones are skipped, as recorded).
    pub steps_replayed: usize,
    pub steps_rejected_skipped: usize,
    /// True only when nothing diverged — mid-episode rasters AND the final
    /// render (when the episode recorded one).
    pub matched: bool,
    pub divergence: Option<Divergence>,
}

/// The first place replay stopped agreeing with the record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    /// The recorded event's seq.
    pub seq: u64,
    pub kind: DivergenceKind,
    pub detail: String,
    /// Differing cell/pixel count, when the kind has one.
    pub differing_pixels: Option<u64>,
    /// Inclusive `[x0, y0, x1, y1]` of the differing area, when countable.
    pub diff_bbox: Option<[u32; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceKind {
    /// An action recorded as accepted was rejected on replay (or failed
    /// mid-dispatch) — the compiler/env drifted from the record.
    AcceptanceMismatch,
    /// The indexed raster after a step differs from the recorded one.
    RasterMismatch,
    /// The rebuilt final render differs from the stored final-image artifact.
    FinalImageMismatch,
}

/// Cell-level diff of two light observations' indexed rasters. Returns None
/// when identical; otherwise (differing cells, inclusive bbox). A shape
/// mismatch (dimensions, layer count, an unindexable layer) counts as a
/// total mismatch of the recorded canvas — there is no meaningful partial
/// answer when the two sides don't describe the same grid.
fn raster_diff(
    recorded: &LightObservation,
    replayed: &LightObservation,
) -> Option<(u64, [u32; 4])> {
    let full = [0, 0, recorded.width - 1, recorded.height - 1];
    if recorded.width != replayed.width
        || recorded.height != replayed.height
        || recorded.layers.len() != replayed.layers.len()
    {
        let cells =
            recorded.width as u64 * recorded.height as u64 * recorded.layers.len().max(1) as u64;
        return Some((cells, full));
    }
    let mut count = 0u64;
    let mut bbox: Option<[u32; 4]> = None;
    for (la, lb) in recorded.layers.iter().zip(&replayed.layers) {
        for (i, (ca, cb)) in la.indices.iter().zip(&lb.indices).enumerate() {
            if ca != cb {
                count += 1;
                let (x, y) = (i as u32 % recorded.width, i as u32 / recorded.width);
                bbox = Some(match bbox {
                    None => [x, y, x, y],
                    Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                });
            }
        }
        // One side failed to index (empty raster): the missing tail differs.
        count += la.indices.len().abs_diff(lb.indices.len()) as u64;
    }
    (count > 0).then(|| (count, bbox.unwrap_or(full)))
}

/// Pixel-exact diff of two PNG byte streams. Identical bytes are identical
/// pixels (fast path); otherwise both are decoded and compared RGBA per
/// pixel. Returns None on exact equality.
fn png_diff(original: &[u8], replayed: &[u8]) -> Result<Option<(u64, [u32; 4])>, String> {
    if original == replayed {
        return Ok(None);
    }
    let a = image::load_from_memory(original)
        .map_err(|e| format!("cannot decode the original final render: {e}"))?
        .to_rgba8();
    let b = image::load_from_memory(replayed)
        .map_err(|e| format!("cannot decode the replayed final render: {e}"))?
        .to_rgba8();
    if (a.width(), a.height()) != (b.width(), b.height()) {
        let cells = a.width() as u64 * a.height() as u64;
        return Ok(Some((cells, [0, 0, a.width() - 1, a.height() - 1])));
    }
    let mut count = 0u64;
    let mut bbox: Option<[u32; 4]> = None;
    for (x, y, pa) in a.enumerate_pixels() {
        if pa != b.get_pixel(x, y) {
            count += 1;
            bbox = Some(match bbox {
                None => [x, y, x, y],
                Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
            });
        }
    }
    Ok((count > 0).then(|| (count, bbox.unwrap())))
}

/// Replay the episode recorded in `episode_dir` into a fresh env under
/// `replay_root`, verifying as it goes. The report's `matched` is the
/// exact-pixel gate lab.md item 12 makes a precondition for training.
pub fn replay(episode_dir: &Path, replay_root: &Path) -> Result<ReplayReport, String> {
    let events = Recorder::read(&episode_dir.join(EPISODE_LOG_FILE))?;
    let artifacts = ArtifactStore::new(episode_dir.join(ARTIFACTS_DIR))?;
    let session_id = events
        .first()
        .map(|e| e.session_id.clone())
        .unwrap_or_default();
    let task = events
        .iter()
        .find_map(|e| match &e.event {
            EventKind::Reset { task, .. } => Some(task.clone()),
            _ => None,
        })
        .ok_or("episode log has no reset event")?;
    let finish = events.iter().find_map(|e| match &e.event {
        EventKind::Finish {
            result,
            final_render,
        } => Some((e.seq, result.seed, final_render.clone())),
        _ => None,
    });

    let mut report = ReplayReport {
        session_id,
        events: events.len(),
        steps_replayed: 0,
        steps_rejected_skipped: 0,
        matched: true,
        divergence: None,
    };

    let mut env = AtelierEnv::new(
        replay_root,
        finish.as_ref().map(|(_, seed, _)| *seed).unwrap_or(0),
    )?;
    env.reset(&task)?;
    // Recorded checkpoint id → the id THIS replay minted (a fresh store may
    // mint differently, exactly like the journal replayer's doc-id remaps).
    let mut checkpoints: HashMap<String, String> = HashMap::new();
    for e in &events {
        match &e.event {
            EventKind::Step {
                action,
                accepted,
                observation_after,
                ..
            } => {
                if !accepted {
                    report.steps_rejected_skipped += 1;
                    continue;
                }
                let t = env.step(action)?;
                report.steps_replayed += 1;
                if !t.accepted {
                    report.divergence = Some(Divergence {
                        seq: e.seq,
                        kind: DivergenceKind::AcceptanceMismatch,
                        detail: format!(
                            "'{}' was recorded as accepted but replayed as rejected: {:?}",
                            action.summarize(),
                            t.error
                        ),
                        differing_pixels: None,
                        diff_bbox: None,
                    });
                    break;
                }
                let recorded = observation_after.as_ref().ok_or_else(|| {
                    format!("event {}: accepted step has no observation_after", e.seq)
                })?;
                let replayed = t
                    .observation_after
                    .as_ref()
                    .expect("an accepted step always observes")
                    .light();
                if let Some((count, bbox)) = raster_diff(recorded, replayed) {
                    report.divergence = Some(Divergence {
                        seq: e.seq,
                        kind: DivergenceKind::RasterMismatch,
                        detail: format!("indexed raster differs after '{}'", action.summarize()),
                        differing_pixels: Some(count),
                        diff_bbox: Some(bbox),
                    });
                    break;
                }
            }
            EventKind::Checkpoint { checkpoint_id } => {
                let minted = env.checkpoint()?;
                checkpoints.insert(checkpoint_id.clone(), minted);
            }
            EventKind::Restore { checkpoint_id, .. } => {
                let minted = checkpoints.get(checkpoint_id).ok_or_else(|| {
                    format!(
                        "event {}: restore of '{checkpoint_id}' before any checkpoint",
                        e.seq
                    )
                })?;
                env.restore(minted)?;
            }
            _ => {}
        }
    }

    // The final gate: rebuild the closing render and compare it pixel-exact
    // against the artifact the original episode stored. An episode without a
    // Finish event has no stored reference — the per-step raster checks are
    // all it gets (and all it can get).
    if report.divergence.is_none() {
        if let Some((seq, _, reference)) = finish {
            let original = artifacts.get(&reference.sha256)?;
            let obs = env.observe(ObservationLevel::Full)?;
            let Observation::Full(full) = obs else {
                return Err("full observation expected".into());
            };
            if let Some((count, bbox)) = png_diff(&original, &full.renders.native)? {
                report.divergence = Some(Divergence {
                    seq,
                    kind: DivergenceKind::FinalImageMismatch,
                    detail: "rebuilt final render differs from the stored artifact".into(),
                    differing_pixels: Some(count),
                    diff_bbox: Some(bbox),
                });
            }
        }
    }
    report.matched = report.divergence.is_none();
    Ok(report)
}
