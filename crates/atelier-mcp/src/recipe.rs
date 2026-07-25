//! Replay recipes: plain JSON Lines journals and authored JSON objects.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

use atelier_studio::{JournalEntry, ToolName, validate_journal};

#[derive(Debug, Default, PartialEq, Eq)]
enum RecipeSource {
    #[default]
    Authored,
    Journal,
}

/// A replay recipe: a named, described sequence of tool calls.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
    #[serde(skip)]
    source: RecipeSource,
}

/// One scripted tool call.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub tool: ToolName,
    pub args: Value,
    /// Authored recipes bind `doc_new`'s returned id under this name. Later
    /// document targets refer to it explicitly as `$name`. Journals never bind:
    /// they contain concrete ids captured from live calls.
    pub bind: Option<String>,
    /// Optional human note, echoed alongside the step for context.
    pub note: Option<String>,
}

fn valid_binding(binding: &str) -> bool {
    let mut chars = binding.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
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
        let mut bindings = HashSet::new();
        for (index, step) in recipe.steps.iter().enumerate() {
            if !step.args.is_object() {
                return Err(format!(
                    "step {} ({}) args must be a JSON object",
                    index + 1,
                    step.tool
                ));
            }
            if recipe.is_journal() && step.bind.is_some() {
                return Err(format!(
                    "journal step {} ({}) may not bind a result",
                    index + 1,
                    step.tool
                ));
            }
            if recipe.is_journal() {
                continue;
            }
            if step.tool == ToolName::DocNew {
                if step.args.get("doc_id").is_some() {
                    return Err(format!(
                        "step {} (doc_new) may not set doc_id; bind its returned id instead",
                        index + 1
                    ));
                }
                let binding = step.bind.as_deref().ok_or_else(|| {
                    format!(
                        "step {} (doc_new) needs `bind`; later calls use `$<bind>` as doc_id",
                        index + 1
                    )
                })?;
                if !valid_binding(binding) {
                    return Err(format!(
                        "step {} (doc_new) bind must match [a-z][a-z0-9_]*",
                        index + 1
                    ));
                }
                if !bindings.insert(binding) {
                    return Err(format!("step {} repeats bind '{binding}'", index + 1));
                }
                continue;
            }
            if step.bind.is_some() {
                return Err(format!(
                    "step {} ({}) cannot bind a result; only doc_new can",
                    index + 1,
                    step.tool
                ));
            }
            for key in ["doc_id", "set_doc"] {
                let Some(target) = step.args.get(key) else {
                    continue;
                };
                if target.is_null() {
                    continue;
                }
                let target = target.as_str().ok_or_else(|| {
                    format!(
                        "step {} ({}) {key} must be a `$<bind>` string",
                        index + 1,
                        step.tool
                    )
                })?;
                let binding = target.strip_prefix('$').ok_or_else(|| {
                    format!(
                        "step {} ({}) {key} must use `$<bind>`, not a concrete document id",
                        index + 1,
                        step.tool
                    )
                })?;
                if !valid_binding(binding) || !bindings.contains(binding) {
                    return Err(format!(
                        "step {} ({}) {key} refers to unknown or future binding '{target}'",
                        index + 1,
                        step.tool
                    ));
                }
            }
        }
        Ok(recipe)
    }

    /// Recorded journals carry concrete document ids; authored recipes bind
    /// the opaque id returned by `doc_new`.
    pub fn is_journal(&self) -> bool {
        self.source == RecipeSource::Journal
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
        let mut entries = Vec::new();
        for (idx, (n, line)) in nonempty.iter().enumerate() {
            match serde_json::from_str::<JournalEntry>(line) {
                Ok(entry) => entries.push(entry),
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
        validate_journal(&entries)?;
        let steps = entries
            .into_iter()
            .map(|entry| Step {
                tool: entry.tool,
                args: Value::Object(entry.args),
                bind: None,
                note: None,
            })
            .collect();
        Ok(Recipe {
            name: "journal".into(),
            description: "recorded from the document's journal".into(),
            steps,
            source: RecipeSource::Journal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_authored_object_form() {
        let r = Recipe::parse(
            r#"{"name":"n","description":"d","steps":[{"tool":"doc_new","bind":"doc","args":{"name":"x"}}]}"#,
        )
        .unwrap();
        assert_eq!(r.name, "n");
        assert_eq!(r.steps[0].tool, ToolName::DocNew);
    }

    #[test]
    fn reads_a_document_journal() {
        // What journal_append writes: one call per line, no wrapper object.
        let r = Recipe::parse(
            "{\"tool\":\"doc_new\",\"args\":{\"name\":\"x\",\"doc_id\":\"d_0000000000000000\"}}\n\
             \n\
             {\"tool\":\"doc_draw\",\"args\":{\"doc_id\":\"d_0000000000000000\",\"op\":\"rect\"}}\n",
        )
        .unwrap();
        assert!(r.is_journal());
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
        let e =
            Recipe::parse(
                "{\"tool\":\"doc_new\",\"args\":{\"doc_id\":\"d_0000000000000000\"}}\n{\"tool\":\ntorn\n",
            )
            .unwrap_err();
        assert!(e.contains("line 2"), "got: {e}");
    }

    #[test]
    fn a_torn_final_line_is_tolerated_not_fatal() {
        // A process killed mid-append leaves a partial last line; the completed
        // steps must still replay.
        let r = Recipe::parse(
            "{\"tool\":\"doc_new\",\"args\":{\"doc_id\":\"d_0000000000000000\"}}\n{\"tool\":\"doc_dr",
        )
        .unwrap();
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.steps[0].tool, ToolName::DocNew);

        // Complete but invalid JSON is authoring corruption, not a torn write.
        let invalid = "{\"tool\":\"doc_new\",\"args\":{}}\n{\"args\":{}}\n";
        assert!(Recipe::parse(invalid).is_err());
    }

    #[test]
    fn an_empty_recipe_is_an_error_either_way() {
        assert!(Recipe::parse(r#"{"name":"n","description":"d","steps":[]}"#).is_err());
        assert!(Recipe::parse("\n\n").is_err());
    }

    #[test]
    fn args_are_required_objects_and_unknown_fields_fail() {
        for src in [
            r#"{"name":"n","description":"d","steps":[{"tool":"list_docs"}]}"#,
            r#"{"name":"n","description":"d","steps":[{"tool":"list_docs","args":[]}]}"#,
            r#"{"name":"n","description":"d","steps":[{"tool":"list_docs","args":{},"old":true}]}"#,
            r#"{"name":"n","description":"d","steps":[],"version":1}"#,
        ] {
            assert!(
                Recipe::parse(src).is_err(),
                "accepted obsolete shape: {src}"
            );
        }
    }

    #[test]
    fn authored_targets_must_use_a_prior_binding() {
        let recipe = |target: &str| {
            format!(
                r#"{{"name":"n","description":"d","steps":[
                    {{"tool":"doc_new","bind":"doc","args":{{"name":"x"}}}},
                    {{"tool":"doc_info","args":{{"doc_id":"{target}"}}}}
                ]}}"#
            )
        };
        Recipe::parse(&recipe("$doc")).unwrap();
        for target in ["d_0000000000000000", "$missing", "$Bad"] {
            assert!(
                Recipe::parse(&recipe(target)).is_err(),
                "accepted unsafe target: {target}"
            );
        }

        let future = r#"{"name":"n","description":"d","steps":[
            {"tool":"doc_info","args":{"doc_id":"$doc"}},
            {"tool":"doc_new","bind":"doc","args":{"name":"x"}}
        ]}"#;
        assert!(Recipe::parse(future).is_err());

        let stamped = r#"{"name":"n","description":"d","steps":[
            {"tool":"doc_new","bind":"doc","args":{"name":"x","doc_id":"d_0000000000000000"}}
        ]}"#;
        assert!(Recipe::parse(stamped).is_err());
    }
}
