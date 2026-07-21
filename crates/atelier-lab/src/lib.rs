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
//! replayable with an exact-pixel gate ([`replay`]). Corruption, evaluation
//! and search are later phases and deliberately absent.

mod action;
mod artifacts;
mod env;
mod observation;
mod recorder;
mod replay;
mod storage;
mod task;
mod transition;

pub use action::{
    compile, Action, ActionKind, CompileError, CompiledCall, DocSnapshot, Stage, MAX_PATCH_PIXELS,
};
pub use artifacts::{sha256_hex, ArtifactKind, ArtifactRef, ArtifactStore, ARTIFACTS_DIR};
pub use env::{AtelierEnv, CheckpointId, EpisodeResult, PixelArtEnv, Result};
pub use observation::{
    DocMetadata, FullObservation, IntegrityChecks, LayerObservation, LightObservation, Observation,
    ObservationLevel, Renders,
};
pub use recorder::{
    Event, EventKind, RecordedFullObservation, RecordedObservation, RecordedRenders, Recorder,
    EPISODE_LOG_FILE, FORMAT_VERSION,
};
pub use replay::{replay, Divergence, DivergenceKind, ReplayReport};
pub use storage::Storage;
pub use task::{StyleSpec, Task};
pub use transition::{ToolResult, Transition};
