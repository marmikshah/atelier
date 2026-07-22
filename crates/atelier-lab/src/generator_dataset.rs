//! Curated episode-to-SFT export for the vision-conditioned action policy.
//! A manifest is an explicit approval boundary: arbitrary model attempts are
//! never silently promoted into generator training data.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::action::{Action, Stage};
use crate::artifacts::{ArtifactKind, ArtifactRef, ArtifactStore, ARTIFACTS_DIR};
use crate::evaluation::SampleSource;
use crate::observation::{IntegrityChecks, LightObservation};
use crate::recorder::{Event, EventKind, Recorder, EPISODE_LOG_FILE};
use crate::storage::Storage;
use crate::task::Task;

pub const GENERATOR_FORMAT_VERSION: u32 = 1;
pub const GENERATOR_EXAMPLES_FILE: &str = "generator.jsonl";

/// One explicitly approved episode. Relative paths resolve against the
/// manifest, keeping a research bundle portable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorEpisodeInput {
    pub format_version: u32,
    pub id: String,
    pub episode_dir: PathBuf,
    pub source: SampleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorContext {
    pub stage: Stage,
    pub palette: Vec<[u8; 4]>,
    pub layer_count: usize,
    pub recent_actions: Vec<String>,
    pub integrity: IntegrityChecks,
}

/// One accepted action target. The visual state is content-addressed and the
/// complete typed action remains the canonical target until a compact wire
/// DSL is deliberately versioned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeneratorExample {
    pub format_version: u32,
    pub id: String,
    pub task: Task,
    pub source: SampleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    pub episode_id: String,
    pub event_seq: u64,
    pub image: ArtifactRef,
    pub context: GeneratorContext,
    pub action: Action,
}

impl GeneratorExample {
    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != GENERATOR_FORMAT_VERSION {
            return Err(format!(
                "generator example '{}' has format version {}, expected {}",
                self.id, self.format_version, GENERATOR_FORMAT_VERSION
            ));
        }
        if self.id.trim().is_empty() || self.episode_id.trim().is_empty() {
            return Err("generator example id and episode id cannot be empty".into());
        }
        self.task.validate()?;
        if self.task.split == "frozen_test" {
            return Err("refusing to export frozen_test generator training data".into());
        }
        if self.source == SampleSource::Corruption {
            return Err("corruption records are critic data, not generator demonstrations".into());
        }
        if matches!(self.source, SampleSource::Model | SampleSource::Search)
            && self
                .generator
                .as_deref()
                .is_none_or(|generator| generator.trim().is_empty())
        {
            return Err("model/search generator examples require generator provenance".into());
        }
        if self.image.kind != ArtifactKind::RenderNative {
            return Err("generator state image must be a native render".into());
        }
        let valid_hash = self.image.sha256.len() == 64
            && self
                .image
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !valid_hash {
            return Err("generator state image has an invalid artifact hash".into());
        }
        if !self.context.integrity.on_palette || !self.context.integrity.palette_within_budget {
            return Err("generator example observation fails palette integrity".into());
        }
        if self.context.palette.len() > self.task.max_colors as usize {
            return Err("generator context exceeds the task palette budget".into());
        }
        Ok(())
    }
}

/// Export every accepted action from each explicitly curated, completed
/// episode. Rejected attempts stay in the source log for diagnostics but are
/// not positive SFT targets.
pub fn export_generator_sft(
    manifest_path: &Path,
    output_dir: &Path,
) -> Result<Vec<GeneratorExample>, String> {
    let inputs: Vec<GeneratorEpisodeInput> = read_jsonl(manifest_path)?;
    if inputs.is_empty() {
        return Err("generator episode manifest is empty".into());
    }
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("cannot create {}: {e}", output_dir.display()))?;
    let artifacts = ArtifactStore::new(output_dir.join(ARTIFACTS_DIR))?;
    let manifest_base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut input_ids = HashSet::new();
    let mut episode_paths = HashSet::new();
    let mut example_ids = HashSet::new();
    let mut examples = Vec::new();

    for input in inputs {
        validate_input(&input)?;
        if !input_ids.insert(input.id.clone()) {
            return Err(format!("duplicate generator manifest id '{}'", input.id));
        }
        let episode_dir = if input.episode_dir.is_absolute() {
            input.episode_dir.clone()
        } else {
            manifest_base.join(&input.episode_dir)
        };
        let canonical = episode_dir.canonicalize().map_err(|e| {
            format!(
                "cannot resolve generator episode {}: {e}",
                episode_dir.display()
            )
        })?;
        if !episode_paths.insert(canonical) {
            return Err(format!(
                "generator manifest reuses episode {}",
                episode_dir.display()
            ));
        }

        let events = Recorder::read(&episode_dir.join(EPISODE_LOG_FILE))?;
        let (task, episode_id) = validate_completed_episode(&episode_dir, &events)?;
        if task.split == "frozen_test" {
            return Err(format!(
                "episode {} uses frozen_test task '{}'",
                episode_dir.display(),
                task.id
            ));
        }
        let committed_steps = committed_steps(&events)?;
        for (event_seq, observation_before, action) in committed_steps {
            validate_observation(&task, &observation_before)?;
            let png = render_observation_png(&observation_before)?;
            let image = ArtifactRef {
                sha256: artifacts.put(&png)?,
                kind: ArtifactKind::RenderNative,
            };
            let id = format!("{}:{event_seq:06}", input.id);
            if !example_ids.insert(id.clone()) {
                return Err(format!("duplicate generator example id '{id}'"));
            }
            let example = GeneratorExample {
                format_version: GENERATOR_FORMAT_VERSION,
                id,
                task: task.clone(),
                source: input.source,
                generator: input.generator.clone(),
                episode_id: episode_id.clone(),
                event_seq,
                image,
                context: GeneratorContext {
                    stage: observation_before.stage,
                    palette: observation_before.palette.clone(),
                    layer_count: observation_before.layers.len(),
                    recent_actions: observation_before.recent_actions.clone(),
                    integrity: observation_before.integrity.clone(),
                },
                action,
            };
            example.validate()?;
            examples.push(example);
        }
    }
    if examples.is_empty() {
        return Err("curated episodes contain no accepted generator actions".into());
    }
    write_jsonl(&output_dir.join(GENERATOR_EXAMPLES_FILE), &examples)?;
    Ok(examples)
}

/// Keep only the accepted actions on the branch that survives restores.
/// A checkpoint captures the current target prefix; restoring it discards
/// every later action, even though those actions were individually accepted.
fn committed_steps(events: &[Event]) -> Result<Vec<(u64, LightObservation, Action)>, String> {
    let mut steps = Vec::new();
    let mut checkpoints = HashMap::new();
    for event in events {
        match &event.event {
            EventKind::Step {
                observation_before,
                action,
                accepted: true,
                ..
            } => steps.push((event.seq, observation_before.clone(), action.clone())),
            EventKind::Checkpoint { checkpoint_id } => {
                checkpoints.insert(checkpoint_id.clone(), steps.len());
            }
            EventKind::Restore { checkpoint_id, .. } => {
                let length = checkpoints.get(checkpoint_id).copied().ok_or_else(|| {
                    format!("restore references unknown checkpoint '{checkpoint_id}'")
                })?;
                steps.truncate(length);
                checkpoints.retain(|_, checkpoint_length| *checkpoint_length <= length);
            }
            _ => {}
        }
    }
    Ok(steps)
}

fn validate_input(input: &GeneratorEpisodeInput) -> Result<(), String> {
    if input.format_version != GENERATOR_FORMAT_VERSION {
        return Err(format!(
            "generator manifest '{}' has format version {}, expected {}",
            input.id, input.format_version, GENERATOR_FORMAT_VERSION
        ));
    }
    if input.id.trim().is_empty() {
        return Err("generator manifest id cannot be empty".into());
    }
    if input.source == SampleSource::Corruption {
        return Err("corruption episodes cannot be generator demonstrations".into());
    }
    if matches!(input.source, SampleSource::Model | SampleSource::Search)
        && input
            .generator
            .as_deref()
            .is_none_or(|generator| generator.trim().is_empty())
    {
        return Err("model/search generator episodes require generator provenance".into());
    }
    Ok(())
}

fn validate_completed_episode(
    episode_dir: &Path,
    events: &[Event],
) -> Result<(Task, String), String> {
    let mut resets = Vec::new();
    let mut finishes = Vec::new();
    for event in events {
        if events
            .first()
            .is_some_and(|first| event.session_id != first.session_id)
        {
            return Err(format!(
                "episode {} mixes session ids",
                episode_dir.display()
            ));
        }
        match &event.event {
            EventKind::Reset { task, .. } => resets.push((event.session_id.clone(), task.clone())),
            EventKind::Finish { result, .. } => {
                finishes.push((event.session_id.clone(), result.clone()))
            }
            _ => {}
        }
    }
    if resets.len() != 1 || finishes.len() != 1 {
        return Err(format!(
            "episode {} must contain exactly one reset and one finish",
            episode_dir.display()
        ));
    }
    let (reset_session, task) = resets.pop().unwrap();
    let (finish_session, result) = finishes.pop().unwrap();
    if reset_session != finish_session
        || result.episode_id != finish_session
        || result.task_id != task.id
        || !result.completed
    {
        return Err(format!(
            "episode {} is incomplete or has inconsistent provenance",
            episode_dir.display()
        ));
    }
    Ok((task, finish_session))
}

fn validate_observation(task: &Task, observation: &LightObservation) -> Result<(), String> {
    if (observation.width, observation.height) != (task.width, task.height) {
        return Err(format!(
            "task '{}' observation has wrong dimensions {}x{}",
            task.id, observation.width, observation.height
        ));
    }
    if !observation.integrity.on_palette || !observation.integrity.palette_within_budget {
        return Err(format!(
            "task '{}' observation fails palette integrity",
            task.id
        ));
    }
    let cells = (observation.width * observation.height) as usize;
    for layer in &observation.layers {
        if layer.indices.len() != cells {
            return Err(format!(
                "task '{}' layer {} has {} cells, expected {cells}",
                task.id,
                layer.index,
                layer.indices.len()
            ));
        }
        if let Some(index) = layer.indices.iter().flatten().find(|index| {
            usize::try_from(**index)
                .map(|index| index >= observation.palette.len())
                .unwrap_or(true)
        }) {
            return Err(format!(
                "task '{}' layer {} uses missing palette index {index}",
                task.id, layer.index
            ));
        }
    }
    Ok(())
}

fn render_observation_png(observation: &LightObservation) -> Result<Vec<u8>, String> {
    let mut image = RgbaImage::new(observation.width, observation.height);
    for layer in observation.layers.iter().filter(|layer| layer.visible) {
        for (cell, index) in layer.indices.iter().enumerate() {
            let Some(index) = index else { continue };
            let mut source = observation.palette[*index as usize];
            source[3] = ((source[3] as u16 * layer.opacity as u16 + 127) / 255) as u8;
            let x = cell as u32 % observation.width;
            let y = cell as u32 / observation.width;
            let destination = image.get_pixel(x, y).0;
            image.put_pixel(x, y, Rgba(source_over(source, destination)));
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|e| format!("cannot encode generator state PNG: {e}"))?;
    Ok(bytes.into_inner())
}

fn source_over(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
    let sa = source[3] as u32;
    let da = destination[3] as u32;
    let out_a = sa + (da * (255 - sa) + 127) / 255;
    if out_a == 0 {
        return [0, 0, 0, 0];
    }
    let mut out = [0u8; 4];
    for channel in 0..3 {
        let source_premultiplied = source[channel] as u32 * sa;
        let destination_premultiplied = (destination[channel] as u32 * da * (255 - sa) + 127) / 255;
        out[channel] =
            ((source_premultiplied + destination_premultiplied + out_a / 2) / out_a).min(255) as u8;
    }
    out[3] = out_a.min(255) as u8;
    out
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut records = Vec::new();
    for (line_no, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(
            serde_json::from_str(&line)
                .map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?,
        );
    }
    Ok(records)
}

fn write_jsonl<T: Serialize>(path: &Path, records: &[T]) -> Result<(), String> {
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    for record in records {
        serde_json::to_writer(&mut writer, record).map_err(|e| e.to_string())?;
        writer
            .write_all(b"\n")
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    writer
        .flush()
        .map_err(|e| format!("cannot flush {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::LayerObservation;

    #[test]
    fn render_flattens_visible_layers_and_opacity() {
        let observation = LightObservation {
            doc_id: "d".into(),
            width: 1,
            height: 1,
            palette: vec![[255, 0, 0, 255], [0, 0, 255, 255]],
            layers: vec![
                LayerObservation {
                    index: 0,
                    name: "bottom".into(),
                    visible: true,
                    opacity: 255,
                    indices: vec![Some(0)],
                },
                LayerObservation {
                    index: 1,
                    name: "top".into(),
                    visible: true,
                    opacity: 128,
                    indices: vec![Some(1)],
                },
            ],
            stage: Stage::Detail,
            recent_actions: vec![],
            integrity: IntegrityChecks {
                on_palette: true,
                palette_within_budget: true,
                opaque_pixels: 2,
            },
        };
        let png = render_observation_png(&observation).unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [127, 0, 128, 255]);
    }
}
