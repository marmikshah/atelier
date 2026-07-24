//! `atelier recipe` — inspect and convert replay recipes without running them.
//!
//! Compact/expand are deliberately explicit file transforms; replay itself
//! auto-detects every supported shape. stdout is the transformed document when
//! `--output` is absent, so the commands compose with shell pipes.

use std::io::Read;
use std::path::{Path, PathBuf};

use atelier_mcp::recipe::Recipe;
use serde_json::json;

const USAGE: &str = "\
usage:
    atelier recipe compact <INPUT|-> [-o PATH]
    atelier recipe expand  <INPUT|-> [-o PATH]
    atelier recipe stats   <INPUT|-> [--json]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Compact,
    Expand,
    Stats,
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    operation: Operation,
    input: String,
    output: Option<PathBuf>,
    json: bool,
}

enum Command {
    Run(Options),
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    let Some(operation) = args.first() else {
        return Err("missing operation".into());
    };
    if matches!(operation.as_str(), "--help" | "-h") {
        return Ok(Command::Help);
    }
    let operation = match operation.as_str() {
        "compact" => Operation::Compact,
        "expand" => Operation::Expand,
        "stats" => Operation::Stats,
        other => return Err(format!("unknown operation '{other}'")),
    };

    let mut input = None;
    let mut output = None;
    let mut json = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                if output.is_some() {
                    return Err("--output may be passed only once".into());
                }
                let Some(path) = args.get(i + 1).filter(|value| !value.starts_with('-')) else {
                    return Err("--output needs a path".into());
                };
                output = Some(path.into());
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "-" => {
                if input.replace("-".into()).is_some() {
                    return Err("pass exactly one input".into());
                }
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag '{other}'"));
            }
            other => {
                if input.replace(other.to_string()).is_some() {
                    return Err("pass exactly one input".into());
                }
                i += 1;
            }
        }
    }
    let input = input.ok_or("missing input path (use - for stdin)")?;
    match operation {
        Operation::Stats if output.is_some() => {
            return Err("stats writes to stdout and does not accept --output".into());
        }
        Operation::Compact | Operation::Expand if json => {
            return Err("--json is available only for stats".into());
        }
        _ => {}
    }
    Ok(Command::Run(Options {
        operation,
        input,
        output,
        json,
    }))
}

fn read_input(input: &str) -> Result<String, String> {
    if input == "-" {
        let mut source = String::new();
        std::io::stdin()
            .read_to_string(&mut source)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        Ok(source)
    } else {
        std::fs::read_to_string(input).map_err(|error| format!("cannot read {input}: {error}"))
    }
}

fn write_output(output: Option<&Path>, body: &str) -> Result<(), String> {
    match output {
        Some(path) => {
            crate::fsutil::write_text(path, body)?;
            eprintln!("wrote {} bytes to {}", body.len(), path.display());
        }
        None => print!("{body}"),
    }
    Ok(())
}

fn stats(source: &str, recipe: &Recipe, json_output: bool) -> Result<String, String> {
    let compact = recipe.to_compact_jsonl()?;
    let input_bytes = source.len();
    let compact_bytes = compact.len();
    let saved_bytes = input_bytes as i64 - compact_bytes as i64;
    let reduction = if input_bytes == 0 {
        0.0
    } else {
        saved_bytes as f64 * 100.0 / input_bytes as f64
    };
    if json_output {
        let mut output = serde_json::to_string_pretty(&json!({
            "format": Recipe::source_format(source),
            "steps": recipe.steps.len(),
            "batch_ops": recipe.batch_ops(),
            "tuple_ops": recipe.tuple_ops(),
            "input_bytes": input_bytes,
            "compact_bytes": compact_bytes,
            "saved_bytes": saved_bytes,
            "reduction_percent": (reduction * 10.0).round() / 10.0,
        }))
        .map_err(|error| format!("cannot encode recipe stats: {error}"))?;
        output.push('\n');
        return Ok(output);
    }
    Ok(format!(
        "format          {}\n\
         steps           {}\n\
         batch ops       {}\n\
         tuple ops       {}\n\
         input bytes     {}\n\
         compact bytes   {}\n\
         saved bytes     {}\n\
         reduction       {:.1}%\n",
        Recipe::source_format(source),
        recipe.steps.len(),
        recipe.batch_ops(),
        recipe.tuple_ops(),
        input_bytes,
        compact_bytes,
        saved_bytes,
        reduction,
    ))
}

/// Entry point for `atelier recipe`.
pub(crate) fn run(args: &[String]) -> i32 {
    let options = match parse(args) {
        Ok(Command::Run(options)) => options,
        Ok(Command::Help) => {
            println!("{USAGE}");
            return 0;
        }
        Err(error) => {
            eprintln!("atelier recipe: {error}\n{USAGE}");
            return 2;
        }
    };
    let source = match read_input(&options.input) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("atelier recipe: {error}");
            return 1;
        }
    };
    let recipe = match Recipe::parse(&source) {
        Ok(recipe) => recipe,
        Err(error) => {
            eprintln!("atelier recipe: {error}");
            return 1;
        }
    };
    let body = match options.operation {
        Operation::Compact => recipe.to_compact_jsonl(),
        Operation::Expand => recipe.to_pretty_json(),
        Operation::Stats => stats(&source, &recipe, options.json),
    };
    let body = match body {
        Ok(body) => body,
        Err(error) => {
            eprintln!("atelier recipe: {error}");
            return 1;
        }
    };
    if options.operation == Operation::Stats {
        print!("{body}");
        return 0;
    }
    match write_output(options.output.as_deref(), &body) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("atelier recipe: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn parses_each_operation_and_rejects_mixed_flags() {
        assert_eq!(
            parse(&argv(&["compact", "in.json", "-o", "out.jsonl"]))
                .ok()
                .and_then(|command| match command {
                    Command::Run(options) => Some(options),
                    Command::Help => None,
                }),
            Some(Options {
                operation: Operation::Compact,
                input: "in.json".into(),
                output: Some("out.jsonl".into()),
                json: false,
            })
        );
        assert!(parse(&argv(&["stats", "-", "--json"])).is_ok());
        assert!(parse(&argv(&["stats", "x", "-o", "y"])).is_err());
        assert!(parse(&argv(&["expand", "x", "--json"])).is_err());
        assert!(parse(&argv(&["compact"])).is_err());
        assert!(parse(&argv(&["wat", "x"])).is_err());
    }

    #[test]
    fn stats_reports_real_compaction() {
        let source = r#"{"name":"n","description":"d","steps":[{"tool":"doc_create","args":{"name":"x","width":8,"height":8}}]}"#;
        let recipe = Recipe::parse(source).unwrap();
        let report = stats(source, &recipe, true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(value["format"], "authored-json");
        assert_eq!(value["steps"], 1);
        assert!(value["compact_bytes"].as_u64().unwrap() > 0);
    }
}
