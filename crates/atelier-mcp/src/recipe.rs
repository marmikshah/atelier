//! Replay recipes: plain JSON Lines journals and authored JSON objects.

use serde::Deserialize;
use serde_json::{Value, json};

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
    /// - **JSON Lines** — one `{tool, args}` per line, as document journals write.
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
        // The last non-empty line may be a partial write from a process killed
        // mid-append — tolerate that (matching how the journal writer/reader
        // treat it), but a broken line with content after it is real corruption
        // that would silently drop a step from the replay, so error on it.
        let nonempty: Vec<(usize, &str)> = src
            .lines()
            .enumerate()
            .map(|(n, l)| (n, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        let last = nonempty.len().saturating_sub(1);
        let mut steps = Vec::new();
        for (idx, (n, line)) in nonempty.iter().enumerate() {
            match serde_json::from_str::<Step>(line) {
                Ok(step) => steps.push(step),
                Err(error) if idx == last && error.is_eof() => {
                    // Tolerated, but never silently: a rebuild missing its
                    // last mutation would otherwise look like a clean run.
                    eprintln!(
                        "recipe: dropped a partial final line (line {}) — the last \
                         recorded step is missing from this replay",
                        n + 1
                    );
                    break;
                }
                Err(e) => return Err(format!("line {}: {e} — expected {{tool, args}}", n + 1)),
            }
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
        // A journal with a torn MIDDLE line (content after it) names the line.
        let e = Recipe::parse("{\"tool\":\"doc_create\"}\n{\"tool\":\ntorn\n").unwrap_err();
        assert!(e.contains("line 2"), "got: {e}");
    }

    #[test]
    fn a_torn_final_line_is_tolerated_not_fatal() {
        // A process killed mid-append leaves a partial last line; the completed
        // steps must still replay.
        let r = Recipe::parse("{\"tool\":\"doc_create\",\"args\":{}}\n{\"tool\":\"doc_dr").unwrap();
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.steps[0].tool, "doc_create");

        // Complete but invalid JSON is authoring corruption, not a torn write.
        let invalid = "{\"tool\":\"doc_create\",\"args\":{}}\n{\"args\":{}}\n";
        assert!(Recipe::parse(invalid).is_err());
    }

    #[test]
    fn an_empty_recipe_is_an_error_either_way() {
        assert!(Recipe::parse(r#"{"name":"n","description":"d","steps":[]}"#).is_err());
        assert!(Recipe::parse("\n\n").is_err());
    }
}
