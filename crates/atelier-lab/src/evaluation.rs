//! Frozen pairwise-evaluation records and JSONL I/O (lab.md Phase 4).
//!
//! A comparison contains no image bytes and no privileged generation stats:
//! candidates refer to content-addressed artifacts, while the annotation UI
//! randomises presentation and records that order. This keeps model identity,
//! latency, and tool count out of the judgement surface without losing
//! provenance needed for later analysis.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::artifacts::{ArtifactKind, ArtifactRef};
use crate::task::Task;

/// Independent from the episode format: comparison records can evolve without
/// invalidating recorded trajectories.
pub const EVALUATION_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleSource {
    Model,
    Human,
    Corruption,
    Search,
}

impl PairwiseCandidate {
    fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.episode_id.trim().is_empty() {
            return Err("candidate id and episode_id cannot be empty".into());
        }
        validate_artifact(&self.id, "native", &self.native, ArtifactKind::RenderNative)?;
        validate_artifact(
            &self.id,
            "enlarged",
            &self.enlarged,
            ArtifactKind::RenderEnlarged,
        )?;
        if let Some(grayscale) = &self.grayscale {
            validate_artifact(
                &self.id,
                "grayscale",
                grayscale,
                ArtifactKind::RenderGrayscale,
            )?;
        }
        if let Some(notan) = &self.notan {
            validate_artifact(&self.id, "notan", notan, ArtifactKind::RenderNotan)?;
        }
        Ok(())
    }
}

fn validate_artifact(
    candidate_id: &str,
    view: &str,
    artifact: &ArtifactRef,
    expected_kind: ArtifactKind,
) -> Result<(), String> {
    let valid_hash = artifact.sha256.len() == 64
        && artifact
            .sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if !valid_hash {
        return Err(format!(
            "candidate '{candidate_id}' {view} artifact has an invalid hash"
        ));
    }
    if artifact.kind != expected_kind {
        return Err(format!(
            "candidate '{candidate_id}' {view} artifact has kind {:?}, expected {:?}",
            artifact.kind, expected_kind
        ));
    }
    Ok(())
}

/// Everything the UI may show for one candidate. `generator` remains in the
/// record for analysis but the bundled UI deliberately never renders it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairwiseCandidate {
    pub id: String,
    pub episode_id: String,
    pub source: SampleSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    pub native: ArtifactRef,
    pub enlarged: ArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grayscale: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notan: Option<ArtifactRef>,
}

/// One blinded A/B question. Candidate ordering here is canonical storage
/// order, not presentation order; clients must randomise it per annotation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairwiseComparison {
    pub format_version: u32,
    pub id: String,
    pub task: Task,
    pub candidate_a: PairwiseCandidate,
    pub candidate_b: PairwiseCandidate,
    /// Optional stable group for repeated/reversed consistency checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consistency_group: Option<String>,
}

impl PairwiseComparison {
    pub fn new(
        id: impl Into<String>,
        task: Task,
        candidate_a: PairwiseCandidate,
        candidate_b: PairwiseCandidate,
    ) -> Self {
        PairwiseComparison {
            format_version: EVALUATION_FORMAT_VERSION,
            id: id.into(),
            task,
            candidate_a,
            candidate_b,
            consistency_group: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.format_version != EVALUATION_FORMAT_VERSION {
            return Err(format!(
                "comparison '{}' has format version {}, expected {}",
                self.id, self.format_version, EVALUATION_FORMAT_VERSION
            ));
        }
        if self.id.trim().is_empty() {
            return Err("comparison id cannot be empty".into());
        }
        if self.candidate_a.id == self.candidate_b.id {
            return Err(format!(
                "comparison '{}' uses the same candidate on both sides",
                self.id
            ));
        }
        self.task.validate()?;
        self.candidate_a.validate()?;
        self.candidate_b.validate()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preference {
    Left,
    Right,
    Tie,
}

/// Fixed labels prevent every annotator inventing a different vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceReason {
    RequirementAdherence,
    Silhouette,
    Composition,
    Pose,
    Proportions,
    Palette,
    Lighting,
    PixelClusters,
    Readability,
    Personality,
    Style,
    Polish,
}

/// A judgement records the actual left/right presentation, so randomisation
/// never makes the label ambiguous when converted into canonical A/B pairs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairwiseAnnotation {
    pub format_version: u32,
    pub comparison_id: String,
    pub annotator_id: String,
    /// Candidate ids in the order the annotator saw them.
    pub presented: [String; 2],
    pub overall: Preference,
    pub requirement_adherence: Preference,
    pub native_readability: Preference,
    #[serde(default)]
    pub reasons: Vec<PreferenceReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

/// Canonical preference in stored candidate order. Unlike [`Preference`],
/// this is independent of whichever candidate the browser placed left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalPreference {
    CandidateA,
    CandidateB,
    Tie,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticLabelSource {
    Human,
    Synthetic,
}

/// Trainer-facing contract. It deliberately repeats the task and candidates
/// so each JSONL row is independently consumable, and omits annotator ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticExample {
    pub format_version: u32,
    pub id: String,
    pub comparison_id: String,
    pub task: Task,
    pub candidate_a: PairwiseCandidate,
    pub candidate_b: PairwiseCandidate,
    pub overall: CanonicalPreference,
    pub requirement_adherence: CanonicalPreference,
    pub native_readability: CanonicalPreference,
    pub reasons: Vec<PreferenceReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub label_source: CriticLabelSource,
}

impl PairwiseAnnotation {
    pub fn validate_against(&self, comparison: &PairwiseComparison) -> Result<(), String> {
        if self.format_version != EVALUATION_FORMAT_VERSION {
            return Err(format!(
                "annotation for '{}' has format version {}, expected {}",
                self.comparison_id, self.format_version, EVALUATION_FORMAT_VERSION
            ));
        }
        if self.comparison_id != comparison.id {
            return Err(format!(
                "annotation targets '{}', not comparison '{}'",
                self.comparison_id, comparison.id
            ));
        }
        let mut expected = [
            comparison.candidate_a.id.as_str(),
            comparison.candidate_b.id.as_str(),
        ];
        expected.sort_unstable();
        let mut presented = [self.presented[0].as_str(), self.presented[1].as_str()];
        presented.sort_unstable();
        if expected != presented {
            return Err(format!(
                "annotation for '{}' names candidates not in the comparison",
                comparison.id
            ));
        }
        if self.annotator_id.trim().is_empty() {
            return Err("annotator_id cannot be empty".into());
        }
        Ok(())
    }

    fn canonicalize(
        &self,
        comparison: &PairwiseComparison,
        preference: Preference,
    ) -> CanonicalPreference {
        let preferred = match preference {
            Preference::Tie => return CanonicalPreference::Tie,
            Preference::Left => &self.presented[0],
            Preference::Right => &self.presented[1],
        };
        if preferred == &comparison.candidate_a.id {
            CanonicalPreference::CandidateA
        } else {
            CanonicalPreference::CandidateB
        }
    }
}

pub fn write_comparisons_jsonl(
    path: &Path,
    comparisons: &[PairwiseComparison],
) -> Result<(), String> {
    validate_comparison_set(comparisons)?;
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    for comparison in comparisons {
        serde_json::to_writer(&mut writer, comparison).map_err(|e| e.to_string())?;
        writer
            .write_all(b"\n")
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    writer
        .flush()
        .map_err(|e| format!("cannot flush {}: {e}", path.display()))
}

pub fn read_comparisons_jsonl(path: &Path) -> Result<Vec<PairwiseComparison>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (line_no, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let comparison: PairwiseComparison = serde_json::from_str(&line)
            .map_err(|e| format!("{} line {}: {e}", path.display(), line_no + 1))?;
        comparison.validate()?;
        out.push(comparison);
    }
    validate_comparison_set(&out)?;
    Ok(out)
}

fn validate_comparison_set(comparisons: &[PairwiseComparison]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for comparison in comparisons {
        comparison.validate()?;
        if !ids.insert(comparison.id.as_str()) {
            return Err(format!("duplicate comparison id '{}'", comparison.id));
        }
    }
    Ok(())
}

pub fn write_annotations_jsonl(
    path: &Path,
    annotations: &[PairwiseAnnotation],
) -> Result<(), String> {
    write_jsonl(path, annotations)
}

pub fn read_annotations_jsonl(path: &Path) -> Result<Vec<PairwiseAnnotation>, String> {
    read_jsonl(path)
}

/// Join blinded browser annotations to comparisons and remove presentation
/// order from all labels. Duplicate judgements by one annotator are rejected:
/// repeated/reversed consistency pairs need distinct comparison ids.
pub fn export_critic_examples(
    comparisons: &[PairwiseComparison],
    annotations: &[PairwiseAnnotation],
) -> Result<Vec<CriticExample>, String> {
    let mut by_id = HashMap::new();
    for comparison in comparisons {
        comparison.validate()?;
        if by_id.insert(comparison.id.as_str(), comparison).is_some() {
            return Err(format!("duplicate comparison id '{}'", comparison.id));
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(annotations.len());
    for annotation in annotations {
        let comparison = by_id
            .get(annotation.comparison_id.as_str())
            .ok_or_else(|| {
                format!(
                    "annotation targets missing comparison '{}'",
                    annotation.comparison_id
                )
            })?;
        annotation.validate_against(comparison)?;
        if !seen.insert((
            annotation.comparison_id.as_str(),
            annotation.annotator_id.as_str(),
        )) {
            return Err(format!(
                "duplicate annotation for comparison '{}' by annotator '{}'",
                annotation.comparison_id, annotation.annotator_id
            ));
        }
        let fingerprint = serde_json::to_vec(annotation).map_err(|e| e.to_string())?;
        let fingerprint = crate::artifacts::sha256_hex(&fingerprint);
        out.push(CriticExample {
            format_version: EVALUATION_FORMAT_VERSION,
            id: format!("human-{}", &fingerprint[..16]),
            comparison_id: comparison.id.clone(),
            task: comparison.task.clone(),
            candidate_a: comparison.candidate_a.clone(),
            candidate_b: comparison.candidate_b.clone(),
            overall: annotation.canonicalize(comparison, annotation.overall),
            requirement_adherence: annotation
                .canonicalize(comparison, annotation.requirement_adherence),
            native_readability: annotation.canonicalize(comparison, annotation.native_readability),
            reasons: annotation.reasons.clone(),
            explanation: annotation.explanation.clone(),
            label_source: CriticLabelSource::Human,
        });
    }
    Ok(out)
}

pub fn write_critic_examples_jsonl(path: &Path, examples: &[CriticExample]) -> Result<(), String> {
    write_jsonl(path, examples)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactKind;
    use crate::task::StyleSpec;

    fn artifact(kind: ArtifactKind, c: char) -> ArtifactRef {
        ArtifactRef {
            sha256: c.to_string().repeat(64),
            kind,
        }
    }

    fn task() -> Task {
        Task {
            id: "item-001".into(),
            prompt: "A chipped red potion bottle".into(),
            category: "item".into(),
            width: 32,
            height: 32,
            max_colors: 16,
            must_include: vec!["chipped bottle".into()],
            must_avoid: vec![],
            style: StyleSpec {
                outline: "selective".into(),
                lighting: "upper-left".into(),
                detail: "medium".into(),
            },
            split: "development".into(),
        }
    }

    fn candidate(id: &str, c: char) -> PairwiseCandidate {
        PairwiseCandidate {
            id: id.into(),
            episode_id: format!("episode-{id}"),
            source: SampleSource::Model,
            generator: Some("hidden-model".into()),
            native: artifact(ArtifactKind::RenderNative, c),
            enlarged: artifact(ArtifactKind::RenderEnlarged, c),
            grayscale: None,
            notan: None,
        }
    }

    #[test]
    fn comparison_jsonl_roundtrips_and_validates_annotations() {
        let comparison = PairwiseComparison::new(
            "cmp-1",
            task(),
            candidate("draft-a", 'a'),
            candidate("draft-b", 'b'),
        );
        let path = std::env::temp_dir().join(format!(
            "atelier-lab-comparisons-{}.jsonl",
            std::process::id()
        ));
        write_comparisons_jsonl(&path, std::slice::from_ref(&comparison)).unwrap();
        assert_eq!(
            read_comparisons_jsonl(&path).unwrap(),
            vec![comparison.clone()]
        );

        let annotation = PairwiseAnnotation {
            format_version: EVALUATION_FORMAT_VERSION,
            comparison_id: comparison.id.clone(),
            annotator_id: "reviewer-1".into(),
            presented: ["draft-b".into(), "draft-a".into()],
            overall: Preference::Left,
            requirement_adherence: Preference::Left,
            native_readability: Preference::Right,
            reasons: vec![PreferenceReason::Silhouette],
            explanation: None,
        };
        annotation.validate_against(&comparison).unwrap();
        let examples = export_critic_examples(
            std::slice::from_ref(&comparison),
            std::slice::from_ref(&annotation),
        )
        .unwrap();
        assert_eq!(examples[0].overall, CanonicalPreference::CandidateB);
        assert_eq!(
            examples[0].native_readability,
            CanonicalPreference::CandidateA
        );
        assert_eq!(examples[0].label_source, CriticLabelSource::Human);
        assert_eq!(examples[0].explanation, annotation.explanation);
        let value = serde_json::to_value(&examples[0]).unwrap();
        assert!(value.get("annotator_id").is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn duplicate_candidate_is_rejected() {
        let candidate = candidate("same", 'a');
        let comparison = PairwiseComparison::new("cmp", task(), candidate.clone(), candidate);
        assert!(comparison.validate().is_err());
    }

    #[test]
    fn duplicate_annotation_is_rejected() {
        let comparison = PairwiseComparison::new(
            "cmp-1",
            task(),
            candidate("draft-a", 'a'),
            candidate("draft-b", 'b'),
        );
        let annotation = PairwiseAnnotation {
            format_version: EVALUATION_FORMAT_VERSION,
            comparison_id: comparison.id.clone(),
            annotator_id: "reviewer-1".into(),
            presented: ["draft-a".into(), "draft-b".into()],
            overall: Preference::Left,
            requirement_adherence: Preference::Left,
            native_readability: Preference::Left,
            reasons: vec![],
            explanation: None,
        };
        assert!(export_critic_examples(&[comparison], &[annotation.clone(), annotation]).is_err());
    }
}
