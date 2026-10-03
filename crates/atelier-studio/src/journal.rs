//! The current per-document JSON Lines contract.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Studio, ToolName};

/// Current JSONL journal entry format.
pub const JOURNAL_FORMAT_VERSION: u32 = 1;

pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_JOURNAL_ENTRIES: usize = 100_000;
/// The one current journal-line shape, shared by the writer, store reader, and
/// replay parser.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub format_version: u32,
    pub tool: ToolName,
    pub args: Map<String, Value>,
}

impl JournalEntry {
    pub fn new(tool: ToolName, args: Map<String, Value>) -> Self {
        Self {
            format_version: JOURNAL_FORMAT_VERSION,
            tool,
            args,
        }
    }
}

/// Validate the current per-document journal contract. An absent/empty journal
/// means "no recipe"; a non-empty one is a complete, self-identifying rebuild.
pub fn validate_journal(entries: &[JournalEntry]) -> Result<(), String> {
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.format_version != JOURNAL_FORMAT_VERSION)
    {
        return Err(format!(
            "unsupported journal format {} (this build supports {})",
            entry.format_version, JOURNAL_FORMAT_VERSION
        ));
    }
    let Some(first) = entries.first() else {
        return Ok(());
    };
    if first.tool != ToolName::DocNew {
        return Err("journal must start with doc_new".into());
    }
    if entries
        .iter()
        .skip(1)
        .any(|entry| entry.tool == ToolName::DocNew)
    {
        return Err("journal may contain exactly one doc_new".into());
    }
    let recorded_id = first
        .args
        .get("doc_id")
        .and_then(Value::as_str)
        .filter(|id| Studio::valid_id(id))
        .ok_or("journal doc_new requires a valid args.doc_id stamp")?;
    for (index, entry) in entries.iter().enumerate().skip(1) {
        if !entry.tool.is_recipe_step() {
            return Err(format!(
                "journal line {} uses non-recipe tool '{}'",
                index + 1,
                entry.tool
            ));
        }
        let mut targets = Vec::new();
        for key in ["doc_id", "set_doc"] {
            if let Some(value) = entry.args.get(key) {
                targets.push(value.as_str().ok_or_else(|| {
                    format!(
                        "journal line {} ({}) has a non-string {key}",
                        index + 1,
                        entry.tool
                    )
                })?);
            }
        }
        if targets.is_empty() {
            return Err(format!(
                "journal line {} ({}) has no document target",
                index + 1,
                entry.tool
            ));
        }
        if targets.iter().any(|target| *target != recorded_id) {
            return Err(format!(
                "journal line {} ({}) targets a document other than '{}'",
                index + 1,
                entry.tool,
                recorded_id
            ));
        }
    }
    Ok(())
}

/// Completed calls and whether a crash left an incomplete final append.
#[derive(Debug)]
pub struct ParsedJournal {
    pub entries: Vec<JournalEntry>,
    pub torn_tail: bool,
}

/// Parse the current JSON Lines contract for store reads and external replay.
/// An empty journal has no recipe. Only an unterminated, incomplete final line
/// is tolerated; malformed complete lines and obsolete shapes are rejected.
pub fn parse_journal(source: &str) -> Result<ParsedJournal, String> {
    parse_with_limits(source, MAX_JOURNAL_BYTES, MAX_JOURNAL_ENTRIES)
}

fn parse_with_limits(
    source: &str,
    max_bytes: u64,
    max_entries: usize,
) -> Result<ParsedJournal, String> {
    if source.len() as u64 > max_bytes {
        return Err(format!(
            "journal is {} bytes, over the {max_bytes}-byte replay limit",
            source.len()
        ));
    }
    let terminated = source.ends_with('\n');
    let mut lines = source
        .lines()
        .enumerate()
        .map(|(number, line)| (number, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .peekable();
    let mut entries = Vec::new();
    let mut torn_tail = false;
    while let Some((number, line)) = lines.next() {
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(entry) if entries.len() < max_entries => entries.push(entry),
            Ok(_) => {
                return Err(format!(
                    "journal line {}: recipe has more than {max_entries} entries",
                    number + 1
                ));
            }
            Err(error) if lines.peek().is_none() && error.is_eof() && !terminated => {
                torn_tail = true;
                break;
            }
            Err(error) => {
                return Err(format!(
                    "journal line {}: {error} — expected a versioned {{tool,args}} entry",
                    number + 1
                ));
            }
        }
    }
    validate_journal(&entries)?;
    Ok(ParsedJournal { entries, torn_tail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolName;

    const ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn reads_a_document_journal() {
        let recipe = parse_journal(&format!(
            "{{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{{\"name\":\"x\",\"doc_id\":\"{ID}\"}}}}\n\
             \n\
             {{\"format_version\":1,\"tool\":\"doc_draw\",\"args\":{{\"doc_id\":\"{ID}\",\"op\":\"rect\"}}}}\n"
        ))
        .unwrap();
        assert_eq!(recipe.entries.len(), 2, "blank lines are skipped");
        assert_eq!(recipe.entries[0].tool, ToolName::DocNew);
        assert_eq!(recipe.entries[1].args["op"], "rect");
    }

    #[test]
    fn a_torn_final_line_preserves_completed_steps() {
        let recipe = parse_journal(&format!(
            "{{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"format_version\":1,\"tool\":\"doc_dr"
        ))
        .unwrap();
        assert_eq!(recipe.entries.len(), 1);
        assert_eq!(recipe.entries[0].tool, ToolName::DocNew);
    }

    #[test]
    fn corruption_and_obsolete_shapes_are_rejected() {
        let torn_middle = format!(
            "{{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"format_version\":1,\"tool\":\n\
             torn\n"
        );
        assert!(parse_journal(&torn_middle).unwrap_err().contains("line 2"));

        let invalid_complete = format!(
            "{{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"args\":{{}}}}\n"
        );
        assert!(parse_journal(&invalid_complete).is_err());
        assert!(
            parse_journal("{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{\"doc_id\":\"d_0000000000000000\"}}\n")
                .is_err()
        );
        assert!(parse_journal("\n\n").unwrap().entries.is_empty());
        assert!(parse_journal(r#"{"name":"old","description":"wrapped","steps":[]}"#).is_err());
    }

    #[test]
    fn encoded_size_is_checked_before_json_parsing() {
        let error = parse_with_limits("not-json-at-all", 4, MAX_JOURNAL_ENTRIES).unwrap_err();
        assert!(error.contains("over the 4-byte replay limit"), "{error}");
        assert!(
            !error.contains("line 1"),
            "size must be the first check: {error}"
        );
    }

    #[test]
    fn entry_count_is_bounded_with_the_overflowing_line_reported() {
        let source = format!(
            "{{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"format_version\":1,\"tool\":\"doc_draw\",\"args\":{{\"doc_id\":\"{ID}\",\"op\":\"rect\"}}}}\n"
        );
        let error = parse_with_limits(&source, MAX_JOURNAL_BYTES, 1).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("more than 1 entries"), "{error}");
    }

    #[test]
    fn newline_terminated_incomplete_json_is_corruption_not_a_torn_tail() {
        let source = format!(
            "{{\"format_version\":1,\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n\
             {{\"format_version\":1,\"tool\":\"doc_draw\"\n"
        );
        let error = parse_journal(&source).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
    }
}
