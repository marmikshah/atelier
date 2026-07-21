//! The episode recorder (lab.md item 11): an append-only JSONL event log per
//! episode, at `<episode_dir>/episode.jsonl`. The atelier journal is
//! insufficient for training data — it records mutation REQUESTS only, no
//! results and no reads — so the lab records at the env level, where the full
//! task → observation → action → compiled calls → tool results → observation
//! → accepted/rejected flow is visible. Rejected actions and failed tool
//! calls are events too, never silently dropped.
//!
//! Determinism rules: no wall-clock anywhere in the log (event ORDER is the
//! monotonic `seq`), and binary payloads (renders) are stored in the
//! artifact store and referenced by hash, never embedded.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::action::{Action, CompileError, CompiledCall};
use crate::artifacts::ArtifactRef;
use crate::env::{CheckpointId, EpisodeResult};
use crate::observation::{DocMetadata, LightObservation};
use crate::task::Task;
use crate::transition::ToolResult;

/// The event log file inside an episode directory.
pub const EPISODE_LOG_FILE: &str = "episode.jsonl";

/// The log's schema version (A3: versioned from day one — these records
/// become API response bodies unchanged). Bump on any incompatible change.
pub const FORMAT_VERSION: u32 = 1;

/// One line of the log: envelope + typed payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub format_version: u32,
    /// The episode id — opaque, matches the env's episode directory name.
    pub session_id: String,
    /// Monotonic within the session, starting at 0.
    pub seq: u64,
    pub event: EventKind,
}

/// What a recorded observation looks like in the log. Identical to the
/// runtime observation EXCEPT that Full's renders are artifact references:
/// the PNG bytes live in the artifact store, keyed by content hash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum RecordedObservation {
    Light(LightObservation),
    /// Boxed: the reports make this variant much larger than Light, and
    /// events move through the log writer one at a time.
    Full(Box<RecordedFullObservation>),
}

/// The full-observation payload with renders swapped for references.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedFullObservation {
    pub light: LightObservation,
    pub renders: RecordedRenders,
    pub palette_report: Value,
    pub components: Value,
    pub critique: Value,
    pub doc: DocMetadata,
}

/// The four render views as artifact references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRenders {
    pub native: ArtifactRef,
    pub enlarged: ArtifactRef,
    pub grayscale: ArtifactRef,
    pub notan: ArtifactRef,
}

/// The typed event payloads, in episode-flow order (lab.md item 11).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// `reset`: the task and the initial (empty-canvas) observation.
    Reset {
        task: Task,
        observation: LightObservation,
    },
    /// One `step`, accepted or rejected. `intent` is the model-reasoning
    /// placeholder (item 11) — always None until a policy provides reasoning
    /// separate from the action's own effect metadata.
    Step {
        observation_before: LightObservation,
        intent: Option<String>,
        action: Action,
        compiled: Vec<CompiledCall>,
        tool_results: Vec<ToolResult>,
        /// None exactly when the action was rejected at compile time.
        observation_after: Option<LightObservation>,
        accepted: bool,
        error: Option<CompileError>,
    },
    /// An explicit `observe` call — reads are recorded here precisely
    /// because the atelier journal omits them.
    Observation { observation: RecordedObservation },
    /// `checkpoint`: the document snapshot was saved.
    Checkpoint { checkpoint_id: CheckpointId },
    /// `restore`: the document was rolled back, plus the resulting state.
    Restore {
        checkpoint_id: CheckpointId,
        observation: LightObservation,
    },
    /// Human or critic judgement attached after the fact (item 11) — the
    /// label vocabulary is later phases' (`record_feedback` is the hook).
    Feedback { label: String, note: Option<String> },
    /// `finish`: the closing state, plus the final render as an artifact
    /// reference — the replay gate (item 12) compares against exactly this
    /// blob, so it must survive the episode.
    Finish {
        result: EpisodeResult,
        final_render: ArtifactRef,
    },
}

/// The append-only writer. One recorder per episode; lines are JSONL so a
/// killed process still leaves every completed line intact.
pub struct Recorder {
    path: PathBuf,
    file: std::fs::File,
    session_id: String,
    seq: u64,
}

impl Recorder {
    /// Open (creating if needed) the event log for `episode_dir`. Appends,
    /// never truncates: a second recorder on the same episode continues the
    /// sequence from the existing line count.
    pub fn create(episode_dir: &Path, session_id: &str) -> Result<Self, String> {
        let path = episode_dir.join(EPISODE_LOG_FILE);
        let seq = std::fs::read_to_string(&path)
            .map(|body| body.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        Ok(Recorder {
            path,
            file,
            session_id: session_id.into(),
            seq,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Next sequence number — the count of events written so far.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Append one event. Unlike the atelier journal (best-effort by design),
    /// a failed write IS an error: this log is the training data — a record
    /// silently lost is worse than an episode stopped loud.
    pub fn append(&mut self, event: EventKind) -> Result<u64, String> {
        let envelope = Event {
            format_version: FORMAT_VERSION,
            session_id: self.session_id.clone(),
            seq: self.seq,
            event,
        };
        let mut line = serde_json::to_string(&envelope).map_err(|e| e.to_string())?;
        line.push('\n');
        use std::io::Write;
        self.file
            .write_all(line.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))?;
        self.seq += 1;
        Ok(envelope.seq)
    }

    /// Parse a log back into its events. Same policy as the atelier journal:
    /// a torn FINAL line is a crash mid-append and is dropped, but a
    /// malformed line with content after it is real corruption and errors.
    pub fn read(path: &Path) -> Result<Vec<Event>, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let nonempty: Vec<(usize, &str)> = body
            .lines()
            .enumerate()
            .map(|(n, l)| (n, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        let last = nonempty.len().saturating_sub(1);
        let mut out = Vec::with_capacity(nonempty.len());
        for (idx, (n, line)) in nonempty.iter().enumerate() {
            match serde_json::from_str(line) {
                Ok(e) => out.push(e),
                Err(_) if idx == last => break,
                Err(e) => return Err(format!("{} line {}: {e}", path.display(), n + 1)),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionKind;
    use crate::action::Stage;
    use crate::observation::{IntegrityChecks, LayerObservation};

    fn light() -> LightObservation {
        LightObservation {
            doc_id: "d".into(),
            width: 2,
            height: 2,
            palette: vec![],
            layers: vec![LayerObservation {
                index: 0,
                name: "Layer 1".into(),
                visible: true,
                opacity: 255,
                indices: vec![None; 4],
            }],
            stage: Stage::Specification,
            recent_actions: vec![],
            integrity: IntegrityChecks {
                on_palette: true,
                palette_within_budget: true,
                opaque_pixels: 0,
            },
        }
    }

    fn recorder(tag: &str) -> (PathBuf, Recorder) {
        let dir =
            std::env::temp_dir().join(format!("atelier-lab-rec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let r = Recorder::create(&dir, "episode-test-0").unwrap();
        (dir, r)
    }

    #[test]
    fn appends_carry_envelope_and_monotonic_seq() {
        let (dir, mut r) = recorder("seq");
        let s0 = r
            .append(EventKind::Checkpoint {
                checkpoint_id: "cp1".into(),
            })
            .unwrap();
        let s1 = r
            .append(EventKind::Step {
                observation_before: light(),
                intent: None,
                action: Action::new(ActionKind::Finish),
                compiled: vec![],
                tool_results: vec![],
                observation_after: Some(light()),
                accepted: true,
                error: None,
            })
            .unwrap();
        assert_eq!((s0, s1), (0, 1));
        let events = Recorder::read(r.path()).unwrap();
        assert_eq!(events.len(), 2);
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e.format_version, FORMAT_VERSION);
            assert_eq!(e.session_id, "episode-test-0");
            assert_eq!(e.seq, i as u64);
        }
        assert!(matches!(events[0].event, EventKind::Checkpoint { .. }));
        assert!(matches!(events[1].event, EventKind::Step { .. }));
        // A second recorder on the same episode continues the sequence.
        let r2 = Recorder::create(&dir, "episode-test-0").unwrap();
        assert_eq!(r2.seq(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tolerates_a_torn_final_line_only() {
        let (dir, mut r) = recorder("torn");
        r.append(EventKind::Checkpoint {
            checkpoint_id: "cp1".into(),
        })
        .unwrap();
        let path = r.path().to_path_buf();
        let clean = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{clean}{{\"format_version\":1,\"seq\"")).unwrap();
        assert_eq!(Recorder::read(&path).unwrap().len(), 1, "torn tail dropped");
        std::fs::write(&path, format!("not json\n{clean}")).unwrap();
        assert!(Recorder::read(&path).is_err(), "mid-file corruption errors");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
