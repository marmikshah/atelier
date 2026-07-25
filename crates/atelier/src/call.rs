//! `atelier call` — the CLI front door: one tool call, straight through the
//! same `Atelier::dispatch` path the MCP server uses (journaling, write
//! ordering and logging included), no transport at all. The whole op surface
//! is scriptable from any shell — and from any agent that has one:
//!
//! ```text
//! atelier call doc_new '{"name":"cat","width":32,"height":32}'
//! atelier call doc_paint_grid --file grid.json
//! atelier call doc_look '{"doc_id":"d_…","out_path":"/tmp/cat.png"}'
//! ```
//!
//! stdout carries the tool's JSON report; the exit code carries the verdict:
//! 0 ok, 1 the tool ran and failed (`{"error": ...}` payload), 2 the call
//! itself was malformed (bad JSON, unknown tool, bad params).

use serde_json::Value;

use atelier_mcp::server::{self, Atelier};
use atelier_studio::{Studio, ToolName};

/// One parsed invocation: which tool, with what args, rooted where.
struct Call {
    tool: ToolName,
    args: Value,
    home: Option<String>,
}

/// Parse `<tool> ['<json>' | --file PATH | --stdin]`. The three
/// arg sources are mutually exclusive — a paint-grid legend can outgrow a
/// comfortable shell argument, which is what --file/--stdin are for.
/// Every tool argument is explicit JSON; the CLI carries no active document,
/// layer, or frame.
/// Errors are usage errors (exit 2), never tool results.
fn parse(args: &[String]) -> Result<Call, String> {
    const USAGE: &str = "usage: atelier call <tool> ['<json>' | --file PATH | --stdin] \
                         [--home DIR]";
    let mut tool = None;
    let mut positional_json = None;
    let mut file = None;
    let mut stdin = false;
    let mut home = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                i += 1;
                file = Some(args.get(i).ok_or("--file needs a path")?.clone());
            }
            "--stdin" => stdin = true,
            "--home" => {
                i += 1;
                home = Some(args.get(i).ok_or("--home needs a directory")?.clone());
            }
            other if other.starts_with("--") => return Err(format!("unknown flag '{other}'")),
            other => {
                if tool.is_none() {
                    tool = Some(other.to_string());
                } else if positional_json.is_none() {
                    positional_json = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument '{other}'\n{USAGE}"));
                }
            }
        }
        i += 1;
    }
    let tool = tool
        .ok_or(USAGE)?
        .parse::<ToolName>()
        .map_err(|error| format!("{error}\n{USAGE}"))?;
    if positional_json.is_some() as u8 + file.is_some() as u8 + u8::from(stdin) > 1 {
        return Err("pass the args one way: '<json>' | --file PATH | --stdin".into());
    }
    let raw = if let Some(path) = file {
        std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?
    } else if stdin {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("reading stdin: {e}"))?;
        s
    } else {
        positional_json.unwrap_or_else(|| "{}".into())
    };
    let args: Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("args must be a JSON object — {e}"))?;
    if !args.is_object() {
        return Err("args must be a JSON object".into());
    }
    Ok(Call { tool, args, home })
}

pub(crate) async fn run(args: &[String]) -> i32 {
    let call = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("atelier: {e}");
            return 2;
        }
    };
    let atelier = match &call.home {
        Some(dir) => Atelier::with_studio(Studio::with_docs_dir(dir.into())),
        None => Atelier::new(),
    };
    let had_out_path = call.args.get("out_path").is_some();
    match atelier.dispatch(call.tool, call.args, "cli").await {
        // Protocol-level failure: the call itself, not the tool, was wrong.
        Err(e) => {
            eprintln!("atelier: {e}");
            2
        }
        Ok(result) => {
            // The CLI has no inline-image channel: print the text report and
            // point at out_path when the pixels had nowhere to go.
            let has_image = result.content.iter().any(|c| c.as_image().is_some());
            for content in &result.content {
                if let Some(t) = content.as_text() {
                    println!("{}", t.text);
                }
            }
            if has_image && !had_out_path {
                eprintln!(
                    "atelier: the result carries an inline image — pass out_path to write it to a file"
                );
            }
            i32::from(server::is_error_result(&result))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_requires_a_tool() {
        assert!(parse(&argv(&[])).is_err());
        let error = parse(&argv(&["atelier_agent"])).err().unwrap();
        assert!(error.contains("unknown tool"));
    }

    #[test]
    fn parse_defaults_args_to_an_empty_object() {
        let c = parse(&argv(&["list_docs"])).unwrap();
        assert_eq!(c.tool, ToolName::ListDocs);
        assert_eq!(c.args, serde_json::json!({}));
        assert!(c.home.is_none());
    }

    #[test]
    fn parse_takes_positional_json_and_home() {
        let c = parse(&argv(&[
            "doc_new",
            "{\"name\":\"cat\"}",
            "--home",
            "/tmp/x",
        ]))
        .unwrap();
        assert_eq!(c.tool, ToolName::DocNew);
        assert_eq!(c.args["name"], "cat");
        assert_eq!(c.home.as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn parse_rejects_removed_context_flags() {
        for flag in ["--doc", "--layer", "--frame"] {
            assert!(parse(&argv(&["doc_draw", "{}", flag, "x"])).is_err());
        }
    }

    #[test]
    fn parse_rejects_competing_arg_sources_and_bad_json() {
        assert!(parse(&argv(&["doc_look", "{}", "--stdin"])).is_err());
        assert!(parse(&argv(&["doc_look", "{}", "--file", "x.json"])).is_err());
        assert!(parse(&argv(&["doc_look", "not json"])).is_err());
        // An array is valid JSON but not a tool's argument shape.
        assert!(parse(&argv(&["doc_look", "[1,2]"])).is_err());
        assert!(parse(&argv(&["doc_look", "{}", "--nope"])).is_err());
        assert!(parse(&argv(&["a", "{}", "extra"])).is_err());
    }

    #[tokio::test]
    async fn a_call_round_trips_through_dispatch() {
        // The wiring `run` glues together, minus stdout: parse, root at a
        // throwaway home, dispatch, read the text report.
        let dir = std::env::temp_dir().join(format!("atelier-call-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.to_string_lossy().to_string();
        let atelier = Atelier::with_studio(Studio::with_docs_dir((&home).into()));

        let create = parse(&argv(&[
            "doc_new",
            "{\"name\":\"cat\",\"width\":8,\"height\":8}",
        ]))
        .unwrap();
        let result = atelier
            .dispatch(create.tool, create.args, "test")
            .await
            .unwrap();
        assert!(!server::is_error_result(&result));
        let report = server::result_json(&result).unwrap();
        let doc_id = report["doc_id"].as_str().unwrap().to_string();

        let info = parse(&argv(&["doc_info", &format!(r#"{{"doc_id":"{doc_id}"}}"#)])).unwrap();
        let result = atelier
            .dispatch(info.tool, info.args, "test")
            .await
            .unwrap();
        let report = server::result_json(&result).unwrap();
        assert_eq!(report["w"], 8);
        // doc_new is journaled: the recipe exists beside the document.
        assert!(dir.join(doc_id).join("recipe.jsonl").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
