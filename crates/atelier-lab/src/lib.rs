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
//! episode loop). Recording, replay, artifacts, corruption and evaluation
//! are later phases and deliberately absent.

mod action;
mod env;
mod observation;
mod task;
mod transition;

pub use action::{
    compile, Action, ActionKind, CompileError, CompiledCall, DocSnapshot, Stage, MAX_PATCH_PIXELS,
};
pub use env::{AtelierEnv, CheckpointId, EpisodeResult, PixelArtEnv, Result};
pub use observation::{
    DocMetadata, FullObservation, IntegrityChecks, LayerObservation, LightObservation, Observation,
    ObservationLevel, Renders,
};
pub use task::{StyleSpec, Task};
pub use transition::{ToolResult, Transition};
