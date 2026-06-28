//! The replay-recipe format: the on-disk contract shared by the session
//! [`Recorder`](crate::server) (which writes recipes as tool calls happen) and
//! the `atelier replay` runner (which reads them back). Kept in the library crate
//! so anything embedding atelier can read/write recipes without the binary.

use serde::Deserialize;
use serde_json::{json, Value};

/// A replay recipe: a named, described sequence of tool calls.
#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
}

/// One scripted tool call.
#[derive(Debug, Deserialize)]
pub struct Step {
    pub tool: String,
    #[serde(default = "empty_obj")]
    pub args: Value,
    /// Optional human note, echoed alongside the step for context.
    #[serde(default)]
    pub note: Option<String>,
}

fn empty_obj() -> Value {
    json!({})
}

impl Recipe {
    /// Parse a recipe from JSON text, with actionable errors.
    pub fn parse(src: &str) -> Result<Recipe, String> {
        let recipe: Recipe = serde_json::from_str(src).map_err(|e| {
            format!(
                "invalid recipe JSON: {e} — expected {{name, description, steps:[{{tool, args}}]}}"
            )
        })?;
        if recipe.steps.is_empty() {
            return Err("recipe has no steps — add at least one {tool, args} to `steps`".into());
        }
        Ok(recipe)
    }
}
