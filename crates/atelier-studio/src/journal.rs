//! The per-document recipe: the calls that built a document, one per line,
//! exactly as the tools received them.
//!
//! ```text
//! {"name":"doc_new","arguments":{"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","height":32,"name":"potion-flask","width":32}}
//! {"name":"doc_palette","arguments":{"colors":["#1a1630","#f0b07a","#5ad8f8"],"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","op":"set"}}
//! {"name":"doc_paint_grid","arguments":{"doc_id":"48080aae-72aa-4f03-94ec-70b69e2791b0","frame":0,"layer":3,"legend":{"C":"#5ad8f8","K":0},"rows":[".KKKKKK.","KCCCCCCK"],"x":12,"y":2}}
//! ```
//!
//! Every line is one call in the shape of an MCP `tools/call`: the tool
//! `name` and its `arguments`, written as the JSON the server dispatched
//! (argument keys in sorted order). Nothing is abbreviated, implied, or
//! reordered beyond that, so a recipe is the literal call log and any JSON
//! Lines reader can consume it. Blank lines and lines starting with `#` are
//! ignored.

use serde_json::{Map, Value};

use crate::ToolName;

pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_JOURNAL_ENTRIES: usize = 100_000;

/// One recorded call: the tool and the arguments it was dispatched with.
#[derive(Clone, Debug)]
pub struct JournalEntry {
    pub tool: ToolName,
    pub args: Map<String, Value>,
}

impl JournalEntry {
    pub fn new(tool: ToolName, args: Map<String, Value>) -> Self {
        Self { tool, args }
    }
}

/// One call record, newline-terminated. The call must target `id`, the
/// document whose recipe it is appended to.
pub fn encode_entry(entry: &JournalEntry, id: &str) -> Result<String, String> {
    if let Some(target) = entry.args.get("doc_id")
        && target != id
    {
        return Err(format!(
            "refusing to journal a call for another document: doc_id {target}"
        ));
    }
    if !entry.tool.is_recipe_step() {
        return Err(format!("'{}' is not a recipe step", entry.tool));
    }
    let arguments = serde_json::to_string(&entry.args).map_err(|error| error.to_string())?;
    Ok(format!(
        "{{\"name\":\"{}\",\"arguments\":{arguments}}}\n",
        entry.tool
    ))
}

/// Read one call line. Only the two MCP call fields are accepted, so a stray
/// key is corruption rather than something to skip over.
fn parse_call_line(line: &str) -> Result<JournalEntry, String> {
    let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let Value::Object(mut object) = value else {
        return Err("a call line is a JSON object".into());
    };
    let name = match object.remove("name") {
        Some(Value::String(name)) => name,
        Some(_) => return Err("'name' must be a string".into()),
        None => return Err("a call line needs a 'name'".into()),
    };
    let tool: ToolName = name.parse().map_err(|_| format!("unknown tool '{name}'"))?;
    let args = match object.remove("arguments") {
        Some(Value::Object(args)) => args,
        Some(_) => return Err("'arguments' must be an object".into()),
        None => return Err("a call line needs 'arguments'".into()),
    };
    if let Some(extra) = object.keys().next() {
        return Err(format!("unexpected field '{extra}' on a call line"));
    }
    Ok(JournalEntry { tool, args })
}

/// Validate the per-document journal contract. An absent/empty journal means
/// "no recipe"; a non-empty one is a complete, self-identifying rebuild.
pub fn validate_journal(entries: &[JournalEntry]) -> Result<(), String> {
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
        .filter(|id| crate::Studio::valid_id(id))
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

/// Parse a recipe for store reads and external replay. An empty recipe has no
/// calls. Only an incomplete final record is tolerated; malformed complete
/// records are rejected.
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
    let mut lines: Vec<&str> = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // Every complete line ends in a newline, so an unterminated final line is
    // what a crash left behind even when it happens to parse as a call.
    let torn_tail = lines.pop().is_some_and(|last| !last.is_empty());
    let mut entries = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = parse_call_line(line).map_err(|error| {
            format!(
                "journal line {}: {error} — expected one {{\"name\":…,\"arguments\":{{…}}}} call per line",
                index + 1
            )
        })?;
        if entries.len() >= max_entries {
            return Err(format!(
                "journal line {}: recipe has more than {max_entries} entries",
                index + 1
            ));
        }
        entries.push(entry);
    }
    validate_journal(&entries)?;
    Ok(ParsedJournal { entries, torn_tail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const OTHER: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn new_line() -> String {
        format!(
            "{{\"name\":\"doc_new\",\"arguments\":{{\"doc_id\":\"{ID}\",\"height\":4,\"name\":\"x\",\"width\":4}}}}\n"
        )
    }

    fn call(name: &str, arguments: &str) -> String {
        format!("{{\"name\":\"{name}\",\"arguments\":{arguments}}}\n")
    }

    fn entry(tool: ToolName, args: Value) -> JournalEntry {
        JournalEntry::new(tool, serde_json::from_value(args).unwrap())
    }

    #[test]
    fn reads_a_recipe() {
        let recipe = parse_journal(&format!(
            "# a note\n{}\n# another\n{}",
            new_line(),
            call(
                "doc_draw",
                &format!(
                    "{{\"doc_id\":\"{ID}\",\"op\":\"rect\",\"layer\":0,\"color\":\"#102030\",\"fill\":true}}"
                )
            )
        ))
        .unwrap();
        assert_eq!(recipe.entries.len(), 2, "blank and # lines are skipped");
        assert_eq!(recipe.entries[0].tool, ToolName::DocNew);
        assert_eq!(recipe.entries[0].args["width"], 4);
        let draw = &recipe.entries[1].args;
        assert_eq!(draw["op"], "rect");
        assert_eq!(draw["color"], "#102030");
        assert_eq!(draw["fill"], true);
        assert_eq!(draw["doc_id"], ID);
        assert!(!recipe.torn_tail);
    }

    #[test]
    fn lines_are_the_mcp_call_as_dispatched() {
        let grid = entry(
            ToolName::DocPaintGrid,
            json!({"doc_id": ID, "rows": [".KK.", "K  K"], "legend": {"K": 0}, "y": 2,
                   "x": 12, "frame": 0, "layer": 3, "replace": true}),
        );
        assert_eq!(
            encode_entry(&grid, ID).unwrap(),
            format!(
                "{{\"name\":\"doc_paint_grid\",\"arguments\":{{\"doc_id\":\"{ID}\",\"frame\":0,\"layer\":3,\"legend\":{{\"K\":0}},\"replace\":true,\"rows\":[\".KK.\",\"K  K\"],\"x\":12,\"y\":2}}}}\n"
            )
        );
        let layer = entry(
            ToolName::DocLayer,
            json!({"doc_id": ID, "name": "glass back", "op": "rename", "index": 0}),
        );
        assert_eq!(
            encode_entry(&layer, ID).unwrap(),
            format!(
                "{{\"name\":\"doc_layer\",\"arguments\":{{\"doc_id\":\"{ID}\",\"index\":0,\"name\":\"glass back\",\"op\":\"rename\"}}}}\n"
            )
        );
        assert!(
            encode_entry(&entry(ToolName::DocDraw, json!({"doc_id": OTHER})), ID).is_err(),
            "a call for another document is never written"
        );
        assert!(
            encode_entry(&entry(ToolName::DocLook, json!({"doc_id": ID})), ID).is_err(),
            "a read is never written"
        );
    }

    #[test]
    fn every_call_round_trips_exactly() {
        let calls = [
            entry(
                ToolName::DocNew,
                json!({"doc_id": ID, "name": "x", "width": 4, "height": 4}),
            ),
            entry(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": "a \"quoted\" = name\nwith a break é"}),
            ),
            entry(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": ""}),
            ),
            // No document id of its own.
            entry(
                ToolName::DocPalette,
                json!({"op": "generate", "set_doc": ID, "base": [1, 2, 3]}),
            ),
            entry(
                ToolName::DocDraw,
                json!({"doc_id": ID, "op": "gradient", "layer": 0, "frame": 0, "opacity": 0.5,
                       "stops": [{"pos": 0.0, "color": "#000000"}, {"pos": 1.0, "color": [1, 2, 3, 4]}],
                       "x0": -3, "erase": false, "text": null, "odd key=1": [1], "rect": [0, 0, 48, -1.5]}),
            ),
            entry(
                ToolName::DocPaintGrid,
                json!({"doc_id": ID, "layer": 0, "frame": 0, "legend": {"|": "#ffffff", "\"": 1, "\n": 0},
                       "rows": ["| \" |", "    ", "", "|", "a\nb"]}),
            ),
            entry(
                ToolName::DocPaintGrid,
                json!({"doc_id": ID, "layer": 0, "frame": 0, "legend": {}, "rows": []}),
            ),
        ];
        let mut source = String::new();
        for call in &calls {
            source.push_str(&encode_entry(call, ID).unwrap());
        }
        assert_eq!(source.lines().count(), calls.len(), "one line per call");
        let recipe = parse_journal(&source).unwrap();
        assert_eq!(recipe.entries.len(), calls.len());
        for (read, written) in recipe.entries.iter().zip(&calls) {
            assert_eq!(read.tool, written.tool);
            assert_eq!(read.args, written.args, "{source}");
        }
    }

    #[test]
    fn a_torn_final_record_preserves_completed_steps() {
        for torn in [
            "{",
            "{\"name\":\"doc_draw\",\"arguments\":{\"op\":\"re",
            // A cut that still parses is not mistaken for a complete call.
            &format!(
                "{{\"name\":\"doc_draw\",\"arguments\":{{\"doc_id\":\"{ID}\",\"op\":\"rect\"}}}}"
            ),
            "# a note",
            "  ",
        ] {
            let recipe = parse_journal(&format!("{}{torn}", new_line())).unwrap();
            assert_eq!(recipe.entries.len(), 1, "{torn:?}");
            assert!(recipe.torn_tail, "{torn:?}");
        }
        let only_torn = parse_journal("{\"name\":\"doc_new\"").unwrap();
        assert!(only_torn.torn_tail && only_torn.entries.is_empty());
    }

    #[test]
    fn corruption_is_rejected_with_its_line() {
        let bad = |body: &str| parse_journal(&format!("{}{body}", new_line())).unwrap_err();
        assert!(bad("{\"name\":\"doc_draw\",\"arguments\":{\"op\":\n").contains("line 2"));
        assert!(bad(&call("not_a_tool", "{}")).contains("unknown tool"));
        assert!(bad(&call("draw", "{}")).contains("unknown tool"));
        assert!(bad("{\"name\":\"doc_draw\"}\n").contains("needs 'arguments'"));
        assert!(bad("{\"arguments\":{}}\n").contains("needs a 'name'"));
        assert!(bad("{\"name\":1,\"arguments\":{}}\n").contains("must be a string"));
        assert!(bad(&call("doc_draw", "[]")).contains("must be an object"));
        assert!(
            bad(&call("doc_draw", "{\"doc_id\":\"x\"},\"extra\":1")).contains("unexpected field")
        );
        assert!(bad("[]\n").contains("JSON object"));
        assert!(bad("draw op=rect\n").contains("line 2"));
        assert!(bad(&call("doc_draw", "{}")).contains("no document target"));
        assert!(
            bad(&call("doc_draw", &format!("{{\"doc_id\":\"{OTHER}\"}}")))
                .contains("targets a document other than")
        );
        assert!(
            bad(&call("doc_look", &format!("{{\"doc_id\":\"{ID}\"}}"))).contains("non-recipe tool")
        );
        assert!(bad(&new_line()).contains("exactly one doc_new"));
        assert!(parse_journal("\n\n").unwrap().entries.is_empty());
        assert!(
            parse_journal(&call("doc_draw", &format!("{{\"doc_id\":\"{ID}\"}}")))
                .unwrap_err()
                .contains("start with doc_new")
        );
        assert!(
            parse_journal(&call("doc_new", "{\"name\":\"x\"}"))
                .unwrap_err()
                .contains("doc_id stamp")
        );
        // Recipes written by earlier builds are not read; they say so by line.
        let old = format!("atelier 4 {ID} 0.1.0\nnew name=x width=4 height=4\n");
        assert!(parse_journal(&old).unwrap_err().contains("line 1"));
    }

    #[test]
    fn encoded_size_is_checked_before_parsing() {
        let error = parse_with_limits("not-a-recipe", 4, MAX_JOURNAL_ENTRIES).unwrap_err();
        assert!(error.contains("over the 4-byte replay limit"), "{error}");
        assert!(
            !error.contains("line 1"),
            "size must be the first check: {error}"
        );
    }

    #[test]
    fn entry_count_is_bounded_with_the_overflowing_line_reported() {
        let source = format!(
            "{}{}",
            new_line(),
            call(
                "doc_draw",
                &format!("{{\"doc_id\":\"{ID}\",\"op\":\"rect\"}}")
            )
        );
        let error = parse_with_limits(&source, MAX_JOURNAL_BYTES, 1).unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("more than 1 entries"), "{error}");
    }
}
