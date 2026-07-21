//! The transition record: one `step` from the env, accepted or rejected
//! (lab.md item 11 — rejected edits are recorded too; the invalid-action
//! rate is a tracked metric).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::action::{Action, CompileError, CompiledCall};
use crate::observation::Observation;

/// The outcome of one dispatched tool call. `result` is the call's JSON
/// payload, or `{"error": ...}` when the tool itself failed — kept as Value
/// because the payload shape is each atelier tool's own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool: String,
    pub ok: bool,
    pub result: Value,
}

/// One environment step: state before, what the policy proposed, what it
/// compiled to, what the tools answered, and the state after.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub observation_before: Observation,
    pub action: Action,
    /// Empty when the action was rejected at compile time — nothing ran.
    pub compiled: Vec<CompiledCall>,
    /// One entry per dispatched call, in order; stops at the first failure.
    pub tool_results: Vec<ToolResult>,
    /// `None` exactly when `error` is `Some` (a rejected action leaves the
    /// document untouched, so there is nothing new to observe).
    pub observation_after: Option<Observation>,
    /// True when every compiled call succeeded. A rejected action, or a tool
    /// error mid-sequence, is `accepted: false` — not an env error.
    pub accepted: bool,
    /// The compile-time rejection, when that is why `accepted` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CompileError>,
}
