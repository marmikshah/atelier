//! The runnable data bridge: completed episode logs become portable blinded
//! comparison bundles, then browser annotations become critic-only JSONL.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use image::{imageops::FilterType, DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::artifacts::{ArtifactKind, ArtifactRef, ArtifactStore, ARTIFACTS_DIR};
use crate::evaluation::{
    export_critic_examples, read_annotations_jsonl, read_comparisons_jsonl,
    write_comparisons_jsonl, write_critic_examples_jsonl, PairwiseCandidate, PairwiseComparison,
    SampleSource, EVALUATION_FORMAT_VERSION,
};
use crate::recorder::{EventKind, Recorder, EPISODE_LOG_FILE};
use crate::storage::Storage;
use crate::task::Task;

pub const COMPARISONS_FILE: &str = "comparisons.jsonl";

/// One candidate named in a bundle manifest. Relative episode paths resolve
/// against the manifest file, so manifests remain portable as a directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeCandidateInput {
    pub id: String,
    pub episode_dir: PathBuf,
    pub source: SampleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
}

/// One explicit episode pair. Pairing is never inferred or quadratic: the
/// experiment author controls baseline-vs-model, parent-vs-child, and repeats.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonBundleInput {
    pub format_version: u32,
    pub id: String,
    pub candidate_a: EpisodeCandidateInput,
    pub candidate_b: EpisodeCandidateInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consistency_group: Option<String>,
}

/// Build `<output>/comparisons.jsonl` plus one deduplicated artifact store.
pub fn bundle_episode_comparisons(
    manifest_path: &Path,
    output_dir: &Path,
) -> Result<Vec<PairwiseComparison>, String> {
    let inputs: Vec<ComparisonBundleInput> = read_jsonl(manifest_path)?;
    if inputs.is_empty() {
        return Err("comparison manifest is empty".into());
    }
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("cannot create {}: {e}", output_dir.display()))?;
    let destination = ArtifactStore::new(output_dir.join(ARTIFACTS_DIR))?;
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut candidates: HashMap<String, (Task, PairwiseCandidate, EpisodeCandidateInput)> =
        HashMap::new();
    let mut comparison_ids = HashSet::new();
    let mut comparisons = Vec::with_capacity(inputs.len());

    for input in inputs {
        if input.format_version != EVALUATION_FORMAT_VERSION {
            return Err(format!(
                "bundle input '{}' has format version {}, expected {}",
                input.id, input.format_version, EVALUATION_FORMAT_VERSION
            ));
        }
        if !comparison_ids.insert(input.id.clone()) {
            return Err(format!("duplicate comparison id '{}'", input.id));
        }
        let (task_a, candidate_a) =
            materialize_candidate(&input.candidate_a, base, &destination, &mut candidates)?;
        let (task_b, candidate_b) =
            materialize_candidate(&input.candidate_b, base, &destination, &mut candidates)?;
        if task_a != task_b {
            return Err(format!(
                "comparison '{}' pairs different tasks '{}' and '{}'",
                input.id, task_a.id, task_b.id
            ));
        }
        let mut comparison = PairwiseComparison::new(input.id, task_a, candidate_a, candidate_b);
        comparison.consistency_group = input.consistency_group;
        comparison.validate()?;
        comparisons.push(comparison);
    }

    write_comparisons_jsonl(&output_dir.join(COMPARISONS_FILE), &comparisons)?;
    Ok(comparisons)
}

/// Canonicalize a browser download into the frozen trainer-facing rows.
pub fn export_annotated_critic_jsonl(
    comparisons_path: &Path,
    annotations_path: &Path,
    output_path: &Path,
) -> Result<usize, String> {
    let comparisons = read_comparisons_jsonl(comparisons_path)?;
    let annotations = read_annotations_jsonl(annotations_path)?;
    let examples = export_critic_examples(&comparisons, &annotations)?;
    write_critic_examples_jsonl(output_path, &examples)?;
    Ok(examples.len())
}

fn materialize_candidate(
    input: &EpisodeCandidateInput,
    manifest_base: &Path,
    destination: &ArtifactStore,
    cache: &mut HashMap<String, (Task, PairwiseCandidate, EpisodeCandidateInput)>,
) -> Result<(Task, PairwiseCandidate), String> {
    if input.id.trim().is_empty() {
        return Err("candidate id cannot be empty".into());
    }
    if let Some((task, candidate, original)) = cache.get(&input.id) {
        if original != input {
            return Err(format!(
                "candidate id '{}' is reused with different episode metadata",
                input.id
            ));
        }
        return Ok((task.clone(), candidate.clone()));
    }

    let episode_dir = if input.episode_dir.is_absolute() {
        input.episode_dir.clone()
    } else {
        manifest_base.join(&input.episode_dir)
    };
    let (task, episode_id, final_render) = completed_episode(&episode_dir)?;
    let source = ArtifactStore::new(episode_dir.join(ARTIFACTS_DIR))?;
    let native_png = source.get(&final_render.sha256)?;
    let image = image::load_from_memory_with_format(&native_png, ImageFormat::Png)
        .map_err(|e| format!("cannot decode final PNG for '{}': {e}", input.id))?
        .to_rgba8();
    if image.dimensions() != (task.width, task.height) {
        return Err(format!(
            "candidate '{}' final render is {}x{}, task requires {}x{}",
            input.id,
            image.width(),
            image.height(),
            task.width,
            task.height
        ));
    }

    let enlarged =
        image::imageops::resize(&image, task.width * 8, task.height * 8, FilterType::Nearest);
    let grayscale = map_pixels(&image, |[r, g, b, a]| {
        let value = ((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8;
        [value, value, value, a]
    });
    let notan_native = map_pixels(&image, |[_, _, _, a]| [0, 0, 0, a]);
    let notan = image::imageops::resize(
        &notan_native,
        task.width * 8,
        task.height * 8,
        FilterType::Nearest,
    );

    let candidate = PairwiseCandidate {
        id: input.id.clone(),
        episode_id,
        source: input.source,
        generator: input.generator.clone(),
        native: store(destination, ArtifactKind::RenderNative, &native_png)?,
        enlarged: store(
            destination,
            ArtifactKind::RenderEnlarged,
            &encode_png(&enlarged)?,
        )?,
        grayscale: Some(store(
            destination,
            ArtifactKind::RenderGrayscale,
            &encode_png(&grayscale)?,
        )?),
        notan: Some(store(
            destination,
            ArtifactKind::RenderNotan,
            &encode_png(&notan)?,
        )?),
    };
    cache.insert(
        input.id.clone(),
        (task.clone(), candidate.clone(), input.clone()),
    );
    Ok((task, candidate))
}

fn completed_episode(episode_dir: &Path) -> Result<(Task, String, ArtifactRef), String> {
    let log = episode_dir.join(EPISODE_LOG_FILE);
    let events = Recorder::read(&log)?;
    let mut active_task: Option<Task> = None;
    let mut completed = None;
    for event in events {
        match event.event {
            EventKind::Reset { task, .. } => active_task = Some(task),
            EventKind::Finish {
                result,
                final_render,
            } => {
                let task = active_task.clone().ok_or_else(|| {
                    format!("finish event in {} has no preceding reset", log.display())
                })?;
                if result.task_id != task.id || result.episode_id != event.session_id {
                    return Err(format!(
                        "finish provenance does not match {}",
                        log.display()
                    ));
                }
                if final_render.kind != ArtifactKind::FinalImage {
                    return Err(format!(
                        "finish event in {} references a non-final artifact",
                        log.display()
                    ));
                }
                completed = Some((task, event.session_id, final_render));
            }
            _ => {}
        }
    }
    completed.ok_or_else(|| format!("episode {} has no finish event", episode_dir.display()))
}

fn store(
    destination: &ArtifactStore,
    kind: ArtifactKind,
    bytes: &[u8],
) -> Result<ArtifactRef, String> {
    Ok(ArtifactRef {
        sha256: destination.put(bytes)?,
        kind,
    })
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|e| format!("cannot encode derived PNG: {e}"))?;
    Ok(bytes.into_inner())
}

fn map_pixels(image: &RgbaImage, f: impl Fn([u8; 4]) -> [u8; 4]) -> RgbaImage {
    RgbaImage::from_fn(image.width(), image.height(), |x, y| {
        Rgba(f(image.get_pixel(x, y).0))
    })
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (line_no, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(&line)
                .map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?,
        );
    }
    Ok(out)
}
