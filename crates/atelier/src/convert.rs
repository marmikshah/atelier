//! `atelier convert <format-4.recipe> --out <format-5.recipe>` — rewrite a
//! recipe that recorded calls as one that records their effects.
//!
//! A format 4 recipe is a list of tool calls. Converting it means making those
//! calls again, in an isolated store, while the current journal writer
//! records what each one did; the resulting `doc.recipe` is the format 5
//! recipe. The header keeps the original document id.
//!
//! This command exists to re-encode the showcase once and is removed after.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use atelier_mcp::server::{self, Atelier};
use atelier_studio::{Studio, ToolName};

const USAGE: &str = "usage: atelier convert <format-4.recipe> --out <file.recipe>";

pub async fn run(args: &[String]) -> i32 {
    let mut source: Option<&str> = None;
    let mut out: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                let Some(path) = args.get(i + 1) else {
                    eprintln!("convert: --out needs a file argument");
                    return 2;
                };
                out = Some(path);
                i += 2;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("convert: unknown flag {other}");
                return 2;
            }
            other => {
                if source.is_some() {
                    eprintln!("convert: unexpected extra argument {other}");
                    return 2;
                }
                source = Some(other);
                i += 1;
            }
        }
    }
    let (Some(source), Some(out)) = (source, out) else {
        eprintln!("{USAGE}");
        return 2;
    };
    match convert(Path::new(source), Path::new(out)).await {
        Ok(count) => {
            eprintln!("convert: {count} call(s) re-encoded into {out}");
            0
        }
        Err(error) => {
            eprintln!("convert: {error}");
            1
        }
    }
}

async fn convert(source: &Path, out: &Path) -> Result<usize, String> {
    let text = crate::replay::read_source(source, &source.display().to_string())?;
    let (recorded_id, calls) = legacy::parse(&text)?;
    let home = std::env::temp_dir().join(format!(
        "atelier-convert-{}-{}",
        std::process::id(),
        source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&home);
    let studio = Studio::with_home(home.clone());
    let atelier = Atelier::with_studio(studio);
    let mut minted: Option<String> = None;
    for (index, (tool, mut args)) in calls.into_iter().enumerate() {
        if tool == ToolName::DocNew {
            args.remove("doc_id");
        } else {
            for key in ["doc_id", "set_doc"] {
                if args.contains_key(key) {
                    let id = minted.clone().ok_or("a call before doc_new")?;
                    args.insert(key.into(), json!(id));
                }
            }
        }
        let result = atelier
            .dispatch(tool, Value::Object(args), "convert")
            .await
            .map_err(|error| format!("call {} ({tool}) failed: {error}", index + 1))?;
        if server::is_error_result(&result) {
            return Err(format!(
                "call {} ({tool}) failed: {}",
                index + 1,
                crate::replay::summarize(&result)
            ));
        }
        if tool == ToolName::DocNew {
            minted = server::result_json(&result).and_then(|report| {
                report
                    .get("doc_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        }
    }
    let minted = minted.ok_or("the recipe created no document")?;
    let journal: PathBuf = home
        .join("documents")
        .join(&minted)
        .join(atelier_studio::JOURNAL_FILE);
    let written = std::fs::read_to_string(&journal).map_err(|error| error.to_string())?;
    let body = written
        .split_once('\n')
        .map(|(_, body)| body)
        .ok_or("the converted recipe has no body")?;
    let count = body.lines().count();
    std::fs::write(
        out,
        format!("{}{body}", atelier_studio::encode_header(&recorded_id)),
    )
    .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
    let _ = std::fs::remove_dir_all(&home);
    Ok(count)
}

/// The format 4 reader: `tool key=value...` lines with `rows:N` grid blocks.
mod legacy {
    use super::*;

    fn is_plain_key(key: &str) -> bool {
        let mut bytes = key.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && key != "rows"
    }

    fn is_bare_word(text: &str) -> bool {
        let mut bytes = text.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, b'_' | b'#'))
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            && !matches!(text, "true" | "false" | "null")
    }

    fn is_map_key(key: &str) -> bool {
        !key.is_empty()
            && !key.starts_with(['"', '[', '{'])
            && key
                .chars()
                .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, ':' | ','))
    }

    fn is_pair_token(token: &str) -> bool {
        token.contains(',') && !token.contains([':', '='])
    }

    fn parse_atom(word: &str) -> Result<Value, String> {
        match word {
            "" => Err("a value is missing".into()),
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "null" => Ok(Value::Null),
            word if word.starts_with(|c: char| c.is_ascii_digit() || c == '-') => {
                match serde_json::from_str(word) {
                    Ok(number @ Value::Number(_)) => Ok(number),
                    _ => Err(format!("'{word}' is not a number")),
                }
            }
            word if is_bare_word(word) => Ok(Value::String(word.to_string())),
            word => Err(format!("'{word}' must be quoted")),
        }
    }

    fn parse_value(word: &str) -> Result<Value, String> {
        if word.contains(':') {
            let mut map = Map::new();
            for pair in word.split(',') {
                let (key, value) = pair
                    .split_once(':')
                    .filter(|(key, _)| is_map_key(key))
                    .ok_or_else(|| format!("'{pair}' is not a key:value pair"))?;
                map.insert(key.to_string(), parse_atom(value)?);
            }
            Ok(Value::Object(map))
        } else if word.contains(',') {
            word.split(',').map(parse_atom).collect()
        } else {
            parse_atom(word)
        }
    }

    type CallLine = (ToolName, Map<String, Value>, Option<usize>);

    fn parse_call_line(line: &str) -> Result<CallLine, String> {
        let (tool, mut rest) = line.split_once(' ').unwrap_or((line, ""));
        let tool: ToolName = serde_json::from_value(Value::String(format!("doc_{tool}")))
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
                    .map_err(|_| format!("'rows:{count}' must end the line"))?;
                return Ok((tool, args, Some(count)));
            }
            let (key, after) = rest
                .split_once('=')
                .ok_or_else(|| format!("expected key=value, found '{rest}'"))?;
            if !is_plain_key(key) {
                return Err(format!("'{key}' is not an argument name"));
            }
            let (value, used) = if after.starts_with(['"', '[', '{']) {
                let mut stream = serde_json::Deserializer::from_str(after).into_iter::<Value>();
                let value = stream
                    .next()
                    .ok_or_else(|| format!("'{key}=' has no value"))?
                    .map_err(|error| format!("{key}: {error}"))?;
                (value, stream.byte_offset())
            } else {
                let word = after.split(' ').next().unwrap_or("");
                let mut lists = vec![parse_value(word).map_err(|error| format!("{key}: {error}"))?];
                let mut used = word.len();
                if is_pair_token(word) {
                    for token in after[used..].split(' ').skip(1) {
                        if !is_pair_token(token) {
                            break;
                        }
                        lists.push(parse_value(token).map_err(|error| format!("{key}: {error}"))?);
                        used += 1 + token.len();
                    }
                }
                let value = match lists.len() {
                    1 => lists.remove(0),
                    _ => Value::Array(lists),
                };
                (value, used)
            };
            let remainder = &after[used..];
            if !remainder.is_empty() && !remainder.starts_with(' ') {
                return Err(format!("{key}: unexpected text after the value"));
            }
            args.insert(key.to_string(), value);
            rest = remainder.trim_start_matches(' ');
        }
        Ok((tool, args, None))
    }

    /// A recorded call: the tool and its arguments.
    pub type Call = (ToolName, Map<String, Value>);

    /// The recorded document id and the calls, each with its `doc_id` (or
    /// `set_doc`) restored to that id.
    pub fn parse(source: &str) -> Result<(String, Vec<Call>), String> {
        let lines: Vec<&str> = source.lines().collect();
        let mut words = lines.first().ok_or("empty recipe")?.split(' ');
        if words.next() != Some("atelier") || words.next() != Some("4") {
            return Err("convert reads format 4 recipes only".into());
        }
        let id = words
            .next()
            .ok_or("the header has no document id")?
            .to_string();
        let mut calls = Vec::new();
        let mut index = 1;
        while index < lines.len() {
            let line = lines[index];
            index += 1;
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let (tool, mut args, block) =
                parse_call_line(line).map_err(|error| format!("line {index}: {error}"))?;
            if let Some(count) = block {
                let mut rows = Vec::new();
                for _ in 0..count {
                    let row = lines
                        .get(index)
                        .and_then(|row| row.strip_prefix("  |"))
                        .and_then(|row| row.strip_suffix('|'))
                        .ok_or_else(|| format!("line {}: a grid row is missing", index + 1))?;
                    rows.push(Value::String(row.to_string()));
                    index += 1;
                }
                args.insert("rows".into(), Value::Array(rows));
            }
            match args.get("doc_id") {
                None => {
                    args.insert("doc_id".into(), Value::String(id.clone()));
                }
                Some(Value::Null) => {
                    args.remove("doc_id");
                }
                Some(_) => return Err(format!("line {index}: doc_id repeated")),
            }
            calls.push((tool, args));
        }
        Ok((id, calls))
    }
}
