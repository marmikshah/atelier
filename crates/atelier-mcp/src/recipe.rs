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

/// True when the source is JSON Lines rather than one authored recipe object.
///
/// Decided by the first non-empty line: a journal line is an object carrying
/// `tool`, which an authored recipe's opening line (`{`, or `{"name": …`) never
/// is. Cheap, and it never confuses a malformed object for lines.
fn looks_like_jsonl(src: &str) -> bool {
    src.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .is_some_and(|v| v.get("tool").is_some())
}

impl Recipe {
    /// Parse a recipe from either supported shape, chosen by content rather
    /// than by file extension (a journal piped in over stdin has no name).
    ///
    /// - **JSON Lines** — one `{tool, args}` per line: what documents journal as
    ///   they are drawn, and what the recorder writes.
    /// - **JSON object** — `{name, description, steps}`: the hand-authored
    ///   recipes in `docs/examples`, where the prose is the point.
    pub fn parse(src: &str) -> Result<Recipe, String> {
        // Decide the shape FIRST, so each form reports its own error. Falling
        // back on a parse failure would answer a broken authored recipe with a
        // line-oriented complaint about JSON Lines, which sends the reader
        // looking in the wrong place.
        let recipe = if looks_like_jsonl(src) {
            Self::parse_jsonl(src)?
        } else {
            serde_json::from_str(src).map_err(|e| {
                format!("invalid recipe JSON: {e} — expected {{name, description, steps:[{{tool, args}}]}}, or JSON Lines of {{tool, args}}")
            })?
        };
        if recipe.steps.is_empty() {
            return Err("recipe has no steps — add at least one {tool, args}".into());
        }
        Ok(recipe)
    }

    /// Parse JSON Lines: one `{tool, args}` per line, blank lines ignored.
    fn parse_jsonl(src: &str) -> Result<Recipe, String> {
        let mut steps = Vec::new();
        for (n, line) in src.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let step: Step = serde_json::from_str(line)
                .map_err(|e| format!("line {}: {e} — expected {{tool, args}}", n + 1))?;
            steps.push(step);
        }
        Ok(Recipe {
            name: "journal".into(),
            description: "recorded from the document's journal".into(),
            steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_authored_object_form() {
        let r = Recipe::parse(
            r#"{"name":"n","description":"d","steps":[{"tool":"doc_create","args":{"name":"x"}}]}"#,
        )
        .unwrap();
        assert_eq!(r.name, "n");
        assert_eq!(r.steps[0].tool, "doc_create");
    }

    #[test]
    fn reads_a_document_journal() {
        // What journal_append writes: one call per line, no wrapper object.
        let r = Recipe::parse(
            "{\"tool\":\"doc_create\",\"args\":{\"name\":\"x\"}}\n\
             \n\
             {\"tool\":\"doc_draw\",\"args\":{\"op\":\"rect\"}}\n",
        )
        .unwrap();
        assert_eq!(r.steps.len(), 2, "blank lines are skipped, not counted");
        assert_eq!(r.steps[1].args["op"], "rect");
    }

    #[test]
    fn each_shape_reports_its_own_error() {
        // A broken authored recipe must not be answered with a complaint about
        // JSON Lines — that sends the reader looking in the wrong place.
        let e = Recipe::parse("{ not json").unwrap_err();
        assert!(e.contains("invalid recipe JSON"), "got: {e}");
        // A journal with a torn line names the line.
        let e = Recipe::parse("{\"tool\":\"doc_create\"}\n{\"tool\":\ntorn\n").unwrap_err();
        assert!(e.contains("line 2"), "got: {e}");
    }

    #[test]
    fn an_empty_recipe_is_an_error_either_way() {
        assert!(Recipe::parse(r#"{"name":"n","description":"d","steps":[]}"#).is_err());
        assert!(Recipe::parse("\n\n").is_err());
    }
}
