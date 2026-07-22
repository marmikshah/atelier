//! atelier-lab: atelier wrapped as an RL-style pixel-art environment.
//!
//! Research crate — local-only, never published (`publish = false`). It
//! consumes atelier exclusively through the public `Studio` API and the
//! server crate's `dispatch`; nothing atelier-internal is forked here.
//!
//! The pieces: a [`Task`] record (what to draw), the [`Action`] DSL and its
//! [`compile`]r (what the policy may do), [`Observation`]s at two cost
//! levels (what the policy sees), the [`Transition`] record (what one step
//! did), and [`PixelArtEnv`] with its [`AtelierEnv`] implementation (the
//! episode loop). Every episode is recorded ([`Recorder`]: append-only JSONL
//! events; binary payloads content-addressed in the [`ArtifactStore`]) and
//! replayable with an exact-pixel gate ([`replay`]). Pairwise evaluation
//! records and the first deterministic corruption families provide the data
//! foundation for a learned critic; search and model training remain later
//! phases.

mod action;
mod artifacts;
mod batch;
mod corruption;
mod dataset;
mod env;
mod evaluation;
mod generator_dataset;
mod observation;
mod policy;
mod recorder;
mod replay;
mod runner;
mod storage;
mod task;
mod transition;

pub use action::{
    compile, Action, ActionKind, CompileError, CompiledCall, DocSnapshot, Stage, MAX_PATCH_PIXELS,
};
pub use artifacts::{sha256_hex, ArtifactKind, ArtifactRef, ArtifactStore, ARTIFACTS_DIR};
pub use batch::{
    append_batch_record, completed_batch_keys, plan_batch_runs, read_batch_records, BatchRecord,
    BatchRunKey, BatchRunSpec, BatchSelection, BATCH_FORMAT_VERSION, BATCH_RESULTS_FILE,
};
pub use corruption::{
    apply_operation, corrupt, CorruptionKind, CorruptionOperation, CorruptionRecord, IndexedSprite,
    PaletteEdit, PixelEdit, Severity,
};
pub use dataset::{
    bundle_episode_comparisons, export_annotated_critic_jsonl, ComparisonBundleInput,
    EpisodeCandidateInput, COMPARISONS_FILE,
};
pub use env::{AtelierEnv, CheckpointId, EpisodeResult, PixelArtEnv, Result};
pub use evaluation::{
    export_critic_examples, read_annotations_jsonl, read_comparisons_jsonl,
    write_annotations_jsonl, write_comparisons_jsonl, write_critic_examples_jsonl,
    CanonicalPreference, CriticExample, CriticLabelSource, PairwiseAnnotation, PairwiseCandidate,
    PairwiseComparison, Preference, PreferenceReason, SampleSource, EVALUATION_FORMAT_VERSION,
};
pub use generator_dataset::{
    export_generator_sft, GeneratorContext, GeneratorEpisodeInput, GeneratorExample,
    GENERATOR_EXAMPLES_FILE, GENERATOR_FORMAT_VERSION,
};
pub use observation::{
    DocMetadata, FullObservation, IntegrityChecks, LayerObservation, LightObservation, Observation,
    ObservationLevel, Renders,
};
pub use policy::{
    CommandPolicy, Policy, PolicyError, PolicyFeedback, PolicyOutcome, PolicyRequest,
    PolicyResponse, PolicyUsage, MAX_POLICY_RESPONSE_BYTES, POLICY_FORMAT_VERSION,
};
pub use recorder::{
    Event, EventKind, RecordedFullObservation, RecordedObservation, RecordedRenders, Recorder,
    EPISODE_LOG_FILE, FORMAT_VERSION,
};
pub use replay::{replay, Divergence, DivergenceKind, ReplayReport};
pub use runner::{run_policy_episode, RunnerConfig, RunnerReport, RunnerTermination};
pub use storage::Storage;
pub use task::{read_tasks_jsonl, write_tasks_jsonl, StyleSpec, Task};
pub use transition::{ToolResult, Transition};
