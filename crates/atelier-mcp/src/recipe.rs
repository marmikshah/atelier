//! Replay recipes: the current per-document JSON Lines journal contract.

use atelier_studio::{JournalEntry, validate_journal};

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
        let nonempty: Vec<(usize, &str)> = src
            .lines()
            .enumerate()
            .map(|(number, line)| (number, line.trim()))
            .filter(|(_, line)| !line.is_empty())
            .collect();
        if nonempty.is_empty() {
            return Err("recipe has no steps — expected JSON Lines of {tool,args}".into());
        }

        let last = nonempty.len() - 1;
        let mut steps = Vec::new();
        for (index, (number, line)) in nonempty.iter().enumerate() {
            match serde_json::from_str::<JournalEntry>(line) {
                Ok(entry) => steps.push(entry),
                Err(error) if index == last && error.is_eof() => {
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
        Ok(Self { steps })
    }
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
}
