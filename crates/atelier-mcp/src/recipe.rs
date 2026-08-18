//! Replay recipes: the current per-document JSON Lines journal contract.

use atelier_studio::{JournalEntry, validate_journal};

/// Maximum encoded size accepted for an external replay recipe.
pub const MAX_RECIPE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum number of non-empty JSONL entries accepted in one replay.
pub const MAX_RECIPE_ENTRIES: usize = 100_000;

/// A replayable sequence of recorded tool calls.
#[derive(Debug)]
pub struct Recipe {
    pub steps: Vec<JournalEntry>,
}

impl Recipe {
    /// Parse one `{tool,args}` journal entry per non-empty line.
    ///
    /// A partial final line may be left behind when a process dies during an
    /// append. Completed lines remain replayable, but corruption before the
    /// final line is always an error.
    pub fn parse(src: &str) -> Result<Self, String> {
        parse_with_limits(src, MAX_RECIPE_BYTES, MAX_RECIPE_ENTRIES)
    }
}

fn parse_with_limits(src: &str, max_bytes: u64, max_entries: usize) -> Result<Recipe, String> {
    let encoded_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    if encoded_bytes > max_bytes {
        return Err(format!(
            "recipe is {encoded_bytes} bytes, over the {max_bytes}-byte replay limit"
        ));
    }

    let final_line_was_terminated = src.ends_with('\n');
    let mut nonempty = src
        .lines()
        .enumerate()
        .map(|(number, line)| (number, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .peekable();
    if nonempty.peek().is_none() {
        return Err("recipe has no steps — expected JSON Lines of {tool,args}".into());
    }

    let mut steps = Vec::new();
    while let Some((number, line)) = nonempty.next() {
        let is_last = nonempty.peek().is_none();
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(entry) if steps.len() < max_entries => steps.push(entry),
            Ok(_) => {
                return Err(format!(
                    "line {}: recipe has more than {max_entries} entries; split or archive it before replay",
                    number + 1
                ));
            }
            Err(error) if is_last && error.is_eof() && !final_line_was_terminated => {
                eprintln!(
                    "recipe: dropped a partial final line (line {}) — the last \
                         recorded step is missing from this replay",
                    number + 1
                );
                break;
            }
            Err(error) => {
                return Err(format!(
                    "line {}: {error} — expected {{tool,args}}",
                    number + 1
                ));
            }
        }
    }
    if steps.is_empty() {
        return Err("recipe has no complete steps".into());
    }
    validate_journal(&steps)?;
    Ok(Recipe { steps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelier_studio::ToolName;

    const ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn reads_a_document_journal() {
        let recipe = Recipe::parse(&format!(
            "{{\"tool\":\"doc_new\",\"args\":{{\"name\":\"x\",\"doc_id\":\"{ID}\"}}}}\n\
             \n\
             {{\"tool\":\"doc_draw\",\"args\":{{\"doc_id\":\"{ID}\",\"op\":\"rect\"}}}}\n"
        ))
        .unwrap();
        assert_eq!(recipe.steps.len(), 2, "blank lines are skipped");
        assert_eq!(recipe.steps[0].tool, ToolName::DocNew);
        assert_eq!(recipe.steps[1].args["op"], "rect");
    }

    #[test]
    fn a_torn_final_line_preserves_completed_steps() {
        let recipe = Recipe::parse(&format!(
            "{{\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"tool\":\"doc_dr"
        ))
        .unwrap();
        assert_eq!(recipe.steps.len(), 1);
        assert_eq!(recipe.steps[0].tool, ToolName::DocNew);
    }

    #[test]
    fn corruption_and_obsolete_shapes_are_rejected() {
        let torn_middle = format!(
            "{{\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"tool\":\n\
             torn\n"
        );
        assert!(Recipe::parse(&torn_middle).unwrap_err().contains("line 2"));

        let invalid_complete = format!(
            "{{\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"args\":{{}}}}\n"
        );
        assert!(Recipe::parse(&invalid_complete).is_err());
        assert!(
            Recipe::parse("{\"tool\":\"doc_new\",\"args\":{\"doc_id\":\"d_0000000000000000\"}}\n")
                .is_err()
        );
        assert!(Recipe::parse("\n\n").is_err());
        assert!(Recipe::parse(r#"{"name":"old","description":"wrapped","steps":[]}"#).is_err());
    }

    #[test]
    fn encoded_size_is_checked_before_json_parsing() {
        let error = parse_with_limits("not-json-at-all", 4, MAX_RECIPE_ENTRIES).unwrap_err();
        assert!(error.contains("over the 4-byte replay limit"), "{error}");
        assert!(
            !error.contains("line 1"),
            "size must be the first check: {error}"
        );
    }

    #[test]
    fn entry_count_is_bounded_with_the_overflowing_line_reported() {
        let source = format!(
            "{{\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"tool\":\"doc_draw\",\"args\":{{\"doc_id\":\"{ID}\",\"op\":\"rect\"}}}}\n"
        );
        let error = parse_with_limits(&source, MAX_RECIPE_BYTES, 1).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("more than 1 entries"), "{error}");
    }

    #[test]
    fn newline_terminated_incomplete_json_is_corruption_not_a_torn_tail() {
        let source = format!(
            "{{\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"tool\":\"doc_draw\"\n"
        );
        let error = Recipe::parse(&source).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
    }
}
