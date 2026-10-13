//! The per-document recipe: a readable text journal of every call.
//!
//! ```text
//! atelier 3 48080aae-72aa-4f03-94ec-70b69e2791b0 0.1.0
//! doc_new name="potion-flask" width=32 height=32
//! doc_layer op=rename index=0 name=glass_back
//! doc_paint_grid layer=3 frame=0 x=12 y=2 legend={"C":5,"K":0} rows:4
//!   |.KKKKKK.|
//!   |KCCCCCCK|
//!   |KCCCCCCK|
//!   |.KKKKKK.|
//! ```
//!
//! The first line names the format, the document, and the Atelier version
//! that created the file. Every other record is one call: the tool, then
//! `key=value` arguments. Values are JSON, except that a plain word may drop
//! its quotes. `doc_id` is implied by the header. A `rows:N` marker ends the
//! line of a grid call and is followed by N rows between `|` bars, so the
//! picture reads as drawn and trailing spaces survive an editor.

use serde_json::{Map, Value};

use crate::{Studio, ToolName};

/// The recipe format number written in the header line.
pub const JOURNAL_SCHEMA: u32 = 3;

pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_JOURNAL_ENTRIES: usize = 100_000;

/// One recorded call as the store reader and replay parser see it: `args`
/// carries the full document id, which the file states once in its header.
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

/// Arguments written first, in this order, so every line leads with what the
/// call does and where. The rest follow alphabetically; `legend` and `rows`
/// come last.
const LEADING_KEYS: [&str; 10] = [
    "op", "layer", "frame", "frame_to", "index", "name", "x", "y", "width", "height",
];

/// The header line for a new recipe, newline-terminated.
pub fn encode_header(id: &str) -> String {
    format!(
        "atelier {JOURNAL_SCHEMA} {id} {}\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn is_plain_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && key != "rows"
}

/// A string that reads unambiguously without quotes.
fn is_bare_word(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && !matches!(text, "true" | "false" | "null")
}

/// Rows that can be written as a block: strings with no line break.
fn block_rows(value: &Value) -> Option<Vec<&str>> {
    value
        .as_array()?
        .iter()
        .map(|row| row.as_str().filter(|row| !row.contains(['\n', '\r'])))
        .collect()
}

/// One call record, newline-terminated, including any rows block.
pub fn encode_entry(entry: &JournalEntry, id: &str) -> Result<String, String> {
    let mut args = entry.args.clone();
    match args.get("doc_id") {
        Some(Value::String(target)) if target == id => {
            args.remove("doc_id");
        }
        None => {
            args.insert("doc_id".into(), Value::Null);
        }
        Some(other) => {
            return Err(format!(
                "refusing to journal a call for another document: doc_id {other}"
            ));
        }
    }
    let json = |value: &Value| serde_json::to_string(value).map_err(|error| error.to_string());
    let mut out = entry.tool.as_str().to_string();
    let rows = args.get("rows").and_then(block_rows);
    if !args
        .keys()
        .all(|key| is_plain_key(key) || (key == "rows" && rows.is_some()))
    {
        // An argument name the line grammar cannot carry: keep the call exact.
        out.push(' ');
        out.push_str(&json(&Value::Object(args))?);
        out.push('\n');
        return Ok(out);
    }
    let rank = |key: &str| match key {
        "legend" => (2, 0),
        _ => match LEADING_KEYS.iter().position(|leading| *leading == key) {
            Some(position) => (0, position),
            None => (1, 0),
        },
    };
    let mut keys: Vec<&String> = args.keys().filter(|key| *key != "rows").collect();
    keys.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    for key in keys {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        match &args[key] {
            Value::String(text) if is_bare_word(text) => out.push_str(text),
            value => out.push_str(&json(value)?),
        }
    }
    if let Some(rows) = rows {
        out.push_str(&format!(" rows:{}\n", rows.len()));
        for row in rows {
            out.push_str("  |");
            out.push_str(row);
            out.push_str("|\n");
        }
    } else {
        out.push('\n');
    }
    Ok(out)
}

/// A parsed call line: the tool, its arguments, and how many block rows follow.
type CallLine = (ToolName, Map<String, Value>, Option<usize>);

fn parse_call_line(line: &str) -> Result<CallLine, String> {
    let (tool, mut rest) = line.split_once(' ').unwrap_or((line, ""));
    let tool: ToolName = serde_json::from_value(Value::String(tool.to_string()))
        .map_err(|_| format!("unknown tool '{tool}'"))?;
    rest = rest.trim_start_matches(' ');
    if rest.starts_with('{') {
        let args = serde_json::from_str(rest).map_err(|error| error.to_string())?;
        return Ok((tool, args, None));
    }
    let mut args = Map::new();
    while !rest.is_empty() {
        if let Some(count) = rest.strip_prefix("rows:") {
            let count = count
                .parse::<usize>()
                .map_err(|_| format!("'rows:{count}' must end the line with a row count"))?;
            return Ok((tool, args, Some(count)));
        }
        let (key, after) = rest
            .split_once('=')
            .ok_or_else(|| format!("expected key=value, found '{rest}'"))?;
        if !is_plain_key(key) {
            return Err(format!("'{key}' is not an argument name"));
        }
        let quoted = after
            .bytes()
            .next()
            .is_some_and(|first| matches!(first, b'"' | b'[' | b'{' | b'-' | b'0'..=b'9'));
        let (value, used) = if quoted {
            let mut stream = serde_json::Deserializer::from_str(after).into_iter::<Value>();
            let value = stream
                .next()
                .ok_or_else(|| format!("'{key}=' has no value"))?
                .map_err(|error| format!("{key}: {error}"))?;
            (value, stream.byte_offset())
        } else {
            let word = after.split(' ').next().unwrap_or("");
            let value = match word {
                "" => return Err(format!("'{key}=' has no value")),
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "null" => Value::Null,
                word => Value::String(word.to_string()),
            };
            (value, word.len())
        };
        let remainder = &after[used..];
        if !remainder.is_empty() && !remainder.starts_with(' ') {
            return Err(format!("{key}: unexpected text after the value"));
        }
        if args.insert(key.to_string(), value).is_some() {
            return Err(format!("argument '{key}' is given twice"));
        }
        rest = remainder.trim_start_matches(' ');
    }
    Ok((tool, args, None))
}

/// Restore the document id the header implies.
fn expand_args(mut args: Map<String, Value>, id: &str) -> Result<Map<String, Value>, String> {
    match args.get("doc_id") {
        None => {
            args.insert("doc_id".into(), Value::String(id.to_string()));
        }
        Some(Value::Null) => {
            args.remove("doc_id");
        }
        Some(_) => return Err("a recipe line must not repeat doc_id".into()),
    }
    Ok(args)
}

fn parse_header(line: &str) -> Result<String, String> {
    let unsupported = |found: &dyn std::fmt::Display| {
        format!("unsupported journal format {found} (this build supports {JOURNAL_SCHEMA})")
    };
    // A line of an earlier JSON format is a version mismatch, not corruption.
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line)
        && let Some(schema) = object.get("schema")
    {
        return Err(unsupported(schema));
    }
    let mut words = line.split(' ');
    if words.next() != Some("atelier") {
        return Err("a recipe starts with an 'atelier <format> <doc-id> <version>' line".into());
    }
    let format = words.next().ok_or("the header has no format number")?;
    if format.parse::<u32>().ok() != Some(JOURNAL_SCHEMA) {
        if format.parse::<u32>().is_ok() {
            return Err(unsupported(&format));
        }
        return Err(format!("'{format}' is not a format number"));
    }
    let id = words.next().ok_or("the header has no document id")?;
    if !Studio::valid_id(id) {
        return Err("the header requires a valid document id".into());
    }
    match (words.next(), words.next()) {
        (Some(version), None) if !version.is_empty() => Ok(id.to_string()),
        _ => Err("the header ends with one Atelier version".into()),
    }
}

/// Validate the current per-document journal contract. An absent/empty journal
/// means "no recipe"; a non-empty one is a complete, self-identifying rebuild.
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
    /// Whether the file already starts with its header line.
    pub headed: bool,
}

/// Parse a recipe for store reads and external replay. An empty recipe has no
/// calls. Only an incomplete final record is tolerated; malformed complete
/// records and other formats are rejected.
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
    let is_blank = |line: &str| line.trim().is_empty();
    // Every complete line ends in a newline, so an unterminated final line is
    // what a crash left behind even when it happens to read as a shorter call.
    let mut torn_tail = lines.pop().is_some_and(|last| !is_blank(last));
    let located = |index: usize, error: String| {
        format!(
            "journal line {}: {error} — expected a header, then one 'tool key=value' call per line",
            index + 1
        )
    };

    let mut entries = Vec::new();
    let mut header: Option<String> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if is_blank(line) {
            index += 1;
            continue;
        }
        let Some(id) = &header else {
            match parse_header(line) {
                Ok(id) => header = Some(id),
                Err(error) if error.starts_with("unsupported journal format") => {
                    return Err(error);
                }
                Err(error) => return Err(located(index, error)),
            }
            index += 1;
            continue;
        };
        let (tool, mut args, block) =
            parse_call_line(line).map_err(|error| located(index, error))?;
        let call_index = index;
        index += 1;
        if let Some(count) = block {
            let mut rows = Vec::with_capacity(count.min(lines.len()));
            let mut complete = true;
            for _ in 0..count {
                // A block that stops at the end of the file was cut by a crash.
                let Some(row) = lines.get(index) else {
                    complete = false;
                    break;
                };
                let row = row
                    .strip_prefix("  |")
                    .and_then(|row| row.strip_suffix('|'))
                    .ok_or_else(|| {
                        located(
                            index,
                            "a grid row must be written as two spaces and |row|".into(),
                        )
                    })?;
                rows.push(Value::String(row.to_string()));
                index += 1;
            }
            if !complete {
                torn_tail = true;
                break;
            }
            args.insert("rows".into(), Value::Array(rows));
        }
        if entries.len() >= max_entries {
            return Err(format!(
                "journal line {}: recipe has more than {max_entries} entries",
                call_index + 1
            ));
        }
        let args = expand_args(args, id).map_err(|error| located(call_index, error))?;
        entries.push(JournalEntry { tool, args });
    }
    validate_journal(&entries)?;
    Ok(ParsedJournal {
        entries,
        torn_tail,
        headed: header.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const OTHER: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn header() -> String {
        format!("atelier 3 {ID} 0.1.0\n")
    }

    const NEW: &str = "doc_new name=x width=4 height=4\n";

    fn entry(tool: ToolName, args: Value) -> JournalEntry {
        JournalEntry::new(tool, serde_json::from_value(args).unwrap())
    }

    #[test]
    fn reads_a_recipe() {
        let recipe = parse_journal(&format!(
            "{}{NEW}\ndoc_draw op=rect layer=0 frame=0 x0=1 color=\"#102030\" fill=true\n",
            header()
        ))
        .unwrap();
        assert_eq!(recipe.entries.len(), 2, "blank lines are skipped");
        assert_eq!(recipe.entries[0].tool, ToolName::DocNew);
        assert_eq!(recipe.entries[0].args["width"], 4);
        let draw = &recipe.entries[1].args;
        assert_eq!(draw["op"], "rect");
        assert_eq!(draw["color"], "#102030");
        assert_eq!(draw["fill"], true);
        assert_eq!(draw["doc_id"], ID, "the header id is restored");
        assert!(recipe.headed && !recipe.torn_tail);
    }

    #[test]
    fn lines_lead_with_what_and_where_and_show_grids_as_blocks() {
        let grid = entry(
            ToolName::DocPaintGrid,
            json!({"doc_id": ID, "rows": [".KK.", "K  K"], "legend": {"K": 0}, "y": 2,
                   "x": 12, "frame": 0, "layer": 3, "replace": true}),
        );
        assert_eq!(
            encode_entry(&grid, ID).unwrap(),
            "doc_paint_grid layer=3 frame=0 x=12 y=2 replace=true legend={\"K\":0} rows:2\n  |.KK.|\n  |K  K|\n"
        );
        let layer = entry(
            ToolName::DocLayer,
            json!({"doc_id": ID, "name": "glass back", "op": "rename", "index": 0}),
        );
        assert_eq!(
            encode_entry(&layer, ID).unwrap(),
            "doc_layer op=rename index=0 name=\"glass back\"\n"
        );
    }

    #[test]
    fn every_call_round_trips_exactly() {
        let calls = [
            entry(
                ToolName::DocNew,
                json!({"doc_id": ID, "name": "x", "width": 4, "height": 4}),
            ),
            // Words that would read as other JSON types keep their quotes.
            entry(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "rename", "index": 0, "name": "true"}),
            ),
            entry(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": "a \"quoted\" = name\nwith a break é"}),
            ),
            entry(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": "12"}),
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
                       "x0": -3, "erase": false, "text": null}),
            ),
            // Rows with spaces, bars, quotes, and no rows at all.
            entry(
                ToolName::DocPaintGrid,
                json!({"doc_id": ID, "layer": 0, "frame": 0, "legend": {"|": "#ffffff", "\"": 1},
                       "rows": ["| \" |", "    ", "", "|"]}),
            ),
            entry(
                ToolName::DocPaintGrid,
                json!({"doc_id": ID, "layer": 0, "frame": 0, "legend": {}, "rows": []}),
            ),
            // A row with a line break cannot be a block; an odd key cannot be a token.
            entry(
                ToolName::DocPaintGrid,
                json!({"doc_id": ID, "layer": 0, "frame": 0, "legend": {"\n": 0}, "rows": ["a\nb"]}),
            ),
            entry(
                ToolName::DocDraw,
                json!({"doc_id": ID, "op": "rect", "odd key=1": [1]}),
            ),
        ];
        let mut source = header();
        for call in &calls {
            source.push_str(&encode_entry(call, ID).unwrap());
        }
        assert!(!source[header().len()..].contains(ID) || source.contains("set_doc"));
        let recipe = parse_journal(&source).unwrap();
        assert_eq!(recipe.entries.len(), calls.len());
        for (read, written) in recipe.entries.iter().zip(&calls) {
            assert_eq!(read.tool, written.tool);
            assert_eq!(read.args, written.args, "{source}");
        }
        assert!(
            encode_entry(&entry(ToolName::DocDraw, json!({"doc_id": OTHER})), ID).is_err(),
            "a call for another document is never written"
        );
    }

    #[test]
    fn a_torn_final_record_preserves_completed_steps() {
        for torn in [
            "doc_dr",
            "doc_draw op=rect color=\"#10",
            "doc_draw op=rect points=[[1,2],[3",
            // A cut that still reads as a shorter call is not mistaken for one.
            "doc_draw op=rect layer=0",
            "doc_paint_grid layer=0 frame=0 legend={} rows:3\n  |..|\n  |.",
            // Cut exactly between two rows of a block: the file still ends in a newline.
            "doc_paint_grid layer=0 frame=0 legend={} rows:3\n  |..|\n",
            "doc_paint_grid layer=0 frame=0 legend={} rows:3\n",
        ] {
            let recipe = parse_journal(&format!("{}{NEW}{torn}", header())).unwrap();
            assert_eq!(recipe.entries.len(), 1, "{torn:?}");
            assert!(recipe.torn_tail, "{torn:?}");
        }
        let only_header = parse_journal("atelier 3 550e84").unwrap();
        assert!(only_header.torn_tail && !only_header.headed);
    }

    #[test]
    fn corruption_and_other_formats_are_rejected() {
        let bad = |body: &str| parse_journal(&format!("{}{NEW}{body}", header())).unwrap_err();
        assert!(bad("doc_draw op=\ntorn\n").contains("line 3"));
        assert!(bad("doc_draw op=rect color=\"#10\n").contains("line 3"));
        assert!(bad("not_a_tool op=rect\n").contains("unknown tool"));
        assert!(bad("doc_draw op=rect op=line\n").contains("given twice"));
        assert!(bad("doc_draw op=rect x=1y=2\n").contains("line 3"));
        assert!(bad("doc_draw op=rect name=\"a\"b\n").contains("unexpected text"));
        assert!(bad("doc_draw op=rect stray\n").contains("key=value"));
        assert!(bad(&format!("doc_draw doc_id=\"{ID}\"\n")).contains("must not repeat doc_id"));
        assert!(
            bad("doc_paint_grid layer=0 frame=0 legend={} rows:2\n  |..|\n..\ndoc_layer op=add\n")
                .contains("line 5")
        );
        assert!(parse_journal("\n\n").unwrap().entries.is_empty());
        assert!(parse_journal(NEW).unwrap_err().contains("line 1"));
        assert!(parse_journal("atelier 3 d_0000000000000000 0.1.0\n").is_err());
        assert!(parse_journal(&format!("atelier 3 {ID}\n")).is_err());
        // Earlier and later formats are version mismatches, named as such.
        for (source, number) in [
            (format!("atelier 4 {ID} 9.0.0\n"), "4"),
            (
                format!("{{\"schema\":2,\"doc_id\":\"{ID}\",\"atelier\":\"0.1.0\"}}\n"),
                "2",
            ),
            (
                format!("{{\"schema\":1,\"tool\":\"doc_new\",\"args\":{{\"doc_id\":\"{ID}\"}}}}\n"),
                "1",
            ),
        ] {
            let error = parse_journal(&source).unwrap_err();
            assert!(
                error.contains(&format!("unsupported journal format {number}")),
                "{error}"
            );
        }
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
        let source = format!("{}{NEW}doc_draw op=rect\n", header());
        let error = parse_with_limits(&source, MAX_JOURNAL_BYTES, 1).unwrap_err();
        assert!(error.contains("line 3"), "{error}");
        assert!(error.contains("more than 1 entries"), "{error}");
    }
}
