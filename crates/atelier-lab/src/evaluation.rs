//! Frozen pairwise-evaluation records and JSONL I/O (lab.md Phase 4).
//!
//! A comparison contains no image bytes and no privileged generation stats:
//! candidates refer to content-addressed artifacts, while the annotation UI
//! randomises presentation and records that order. This keeps model identity,
//! latency, and tool count out of the judgement surface without losing
//! provenance needed for later analysis.

use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::artifacts::ArtifactRef;
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
}

pub fn write_comparisons_jsonl(
    path: &Path,
    comparisons: &[PairwiseComparison],
) -> Result<(), String> {
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    for comparison in comparisons {
        comparison.validate()?;
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
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn duplicate_candidate_is_rejected() {
        let candidate = candidate("same", 'a');
        let comparison = PairwiseComparison::new("cmp", task(), candidate.clone(), candidate);
        assert!(comparison.validate().is_err());
    }
}
