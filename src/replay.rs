//! `atelier replay <recipe.json>` — drive the atelier MCP server through a
//! scripted list of tool calls.
//!
//! The runner is itself an MCP *client*: it spawns this very binary
//! (`current_exe`) as a child over stdio, performs the handshake
//! (`initialize` → `notifications/initialized`), then issues one `tools/call`
//! per recipe step, waiting for each response before sending the next. The
//! server happily pipelines concurrent requests, so strict sequencing is on us
//! — a recipe is a narrative, step N may depend on step N-1's mutations.
//!
//! No rmcp client dependency: that would pull in `process-wrap` (child-process
//! transport) which we don't otherwise need. Hand-rolled line-delimited
//! JSON-RPC over the child's stdin/stdout keeps the dep tree unchanged.
//!
//! Output convention: the per-step log goes to stdout (scriptable, the recipe's
//! visible result), while status/diagnostics go to stderr (header, errors, the
//! final "N step(s) ok" tally) so they don't pollute piped stdout.

use std::process::Stdio;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// One-line usage banner, shared by the `--help` path and the arg-error paths.
const USAGE: &str = "usage: atelier replay <recipe.json> [--home DIR]";

/// A replay recipe: a named, described sequence of tool calls.
#[derive(Debug, Deserialize)]
pub(crate) struct Recipe {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) steps: Vec<Step>,
}

/// One scripted tool call.
#[derive(Debug, Deserialize)]
pub(crate) struct Step {
    pub(crate) tool: String,
    #[serde(default = "empty_obj")]
    pub(crate) args: Value,
    /// Optional human note, echoed alongside the step for context.
    #[serde(default)]
    pub(crate) note: Option<String>,
}

fn empty_obj() -> Value {
    json!({})
}

impl Recipe {
    /// Parse a recipe from JSON text, with actionable errors.
    pub(crate) fn parse(src: &str) -> Result<Recipe, String> {
        let recipe: Recipe = serde_json::from_str(src).map_err(|e| {
            format!(
                "invalid recipe JSON: {e} — expected {{name, description, steps:[{{tool, args}}]}}"
            )
        })?;
        if recipe.steps.is_empty() {
            return Err("recipe has no steps — add at least one {tool, args} to `steps`".into());
        }
        Ok(recipe)
    }
}

/// Entry point for the `replay` subcommand. Returns a process exit code.
/// `args` is everything after `replay` on the command line. Async because it
/// runs inside main's tokio runtime, driving the child server over stdio.
pub async fn run(args: &[String]) -> i32 {
    // Parse args: a single positional recipe path plus optional `--home <dir>`.
    let mut path: Option<&str> = None;
    let mut home: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--home" => {
                let Some(dir) = args.get(i + 1) else {
                    eprintln!("replay: --home needs a directory argument");
                    return 2;
                };
                home = Some(dir);
                i += 2;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("replay: unknown flag {other}");
                return 2;
            }
            other => {
                if path.is_some() {
                    eprintln!("replay: unexpected extra argument {other}");
                    return 2;
                }
                path = Some(other);
                i += 1;
            }
        }
    }

    let Some(path) = path else {
        eprintln!("{USAGE}");
        return 2;
    };

    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("replay: cannot read {path}: {e}");
            return 2;
        }
    };
    let recipe = match Recipe::parse(&src) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay: {e}");
            return 2;
        }
    };

    match drive(recipe, home).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("replay: {e}");
            1
        }
    }
}

/// Spawn the child server, handshake, and run every step in order.
async fn drive(recipe: Recipe, home: Option<&str>) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate atelier binary (current_exe): {e}"))?;

    let mut cmd = Command::new(&exe);
    // No subcommand args → the child runs the stdio MCP server. Pipe stdio for
    // the JSON-RPC dialogue; let the child's stderr flow straight through.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // `--home` overrides ATELIER_HOME for an isolated run; otherwise the child
    // inherits our environment (including any ambient ATELIER_HOME).
    if let Some(dir) = home {
        cmd.env("ATELIER_HOME", dir);
    }

    let mut child: Child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn server child: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("child stdin unavailable after spawn")?;
    let mut reader = BufReader::new(
        child
            .stdout
            .take()
            .ok_or("child stdout unavailable after spawn")?,
    );

    let result = run_session(&recipe, &mut stdin, &mut reader).await;

    // Close the child's stdin so its stdio server loop terminates, then reap.
    drop(stdin);
    let _ = child.wait().await;
    result
}

/// The MCP conversation: handshake then one `tools/call` per step.
async fn run_session(
    recipe: &Recipe,
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
) -> Result<(), String> {
    // Header line (to stderr, status channel) so the run is self-identifying.
    eprintln!("== {} — {}", recipe.name, recipe.description);

    // --- handshake: initialize -> initialized notification -------------------
    let init = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            // Must match a protocol version the server supports.
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "atelier-replay", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    send(stdin, &init).await?;
    let init_resp = recv(reader).await?;
    if let Some(err) = init_resp.get("error") {
        return Err(format!("server rejected initialize: {err}"));
    }
    let notify = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    send(stdin, &notify).await?;

    // --- steps ---------------------------------------------------------------
    for (idx, step) in recipe.steps.iter().enumerate() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": idx + 1,
            "method": "tools/call",
            "params": {"name": step.tool, "arguments": step.args}
        });
        send(stdin, &req).await?;
        let resp = recv(reader).await?;

        // Transport-level JSON-RPC error (unknown tool, malformed args, …).
        if let Some(err) = resp.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            print_step(idx, step, &format!("ERROR {msg}"));
            return Err(format!("step {} ({}) failed: {msg}", idx + 1, step.tool));
        }

        let result = resp.get("result").cloned().unwrap_or(Value::Null);
        // atelier tools surface their own errors as a {"error": ...} text
        // payload with isError set; treat that as a failed step too.
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let summary = summarize(&result);
        print_step(idx, step, &summary);
        if is_error {
            return Err(format!(
                "step {} ({}) failed: {summary}",
                idx + 1,
                step.tool
            ));
        }
    }

    eprintln!("replay: {} step(s) ok", recipe.steps.len());
    Ok(())
}

/// One-line per-step report: `[N] tool — summary  (note)`. Goes to stdout.
fn print_step(idx: usize, step: &Step, summary: &str) {
    let note = step
        .note
        .as_deref()
        .map(|n| format!("  ({n})"))
        .unwrap_or_default();
    println!("[{}] {} — {summary}{note}", idx + 1, step.tool);
}

/// Condense a `tools/call` result into a single readable line. atelier returns
/// its payload as the first text content block (a JSON string); fall back to a
/// compact dump of whatever shape we get.
fn summarize(result: &Value) -> String {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str);
    let line = match text {
        Some(t) => t.to_string(),
        None => result.to_string(),
    };
    // Keep the log scannable.
    const MAX: usize = 200;
    if line.chars().count() > MAX {
        let truncated: String = line.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        line
    }
}

/// Write one line-delimited JSON-RPC message to the child.
async fn send(stdin: &mut ChildStdin, msg: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(msg).map_err(|e| format!("encode request: {e}"))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write to server: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("flush to server: {e}"))?;
    Ok(())
}

/// Read one JSON-RPC message (one line) from the child, skipping any
/// notifications/log lines the server may emit unsolicited. Assumes strict
/// request/response sequencing: it returns the next response without matching
/// against the request's id.
async fn recv(reader: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("read from server: {e}"))?;
        if n == 0 {
            return Err("server closed the connection unexpectedly".into());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(trimmed)
            .map_err(|e| format!("server sent invalid JSON ({e}): {trimmed}"))?;
        // A response carries an `id`; bare notifications (no id) are skipped.
        if msg.get("id").is_some() {
            return Ok(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_recipe() {
        let src = r#"{
            "name": "demo",
            "description": "tiny",
            "steps": [
                {"tool": "doc_create", "args": {"name": "x", "width": 8, "height": 8}},
                {"tool": "doc_info", "args": {"doc_id": "x"}, "note": "inspect"}
            ]
        }"#;
        let r = Recipe::parse(src).expect("should parse");
        assert_eq!(r.name, "demo");
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[0].tool, "doc_create");
        assert_eq!(r.steps[1].note.as_deref(), Some("inspect"));
    }

    #[test]
    fn args_default_to_empty_object() {
        let src = r#"{
            "name": "n",
            "description": "d",
            "steps": [{"tool": "list_docs"}]
        }"#;
        let r = Recipe::parse(src).expect("should parse");
        assert_eq!(r.steps[0].args, json!({}));
        assert_eq!(r.steps[0].note, None);
    }

    #[test]
    fn empty_steps_rejected() {
        let src = r#"{"name": "n", "description": "d", "steps": []}"#;
        let err = Recipe::parse(src).expect_err("empty steps must error");
        assert!(err.contains("no steps"), "got: {err}");
    }

    #[test]
    fn malformed_json_actionable_error() {
        let err = Recipe::parse("{ not json").expect_err("must error");
        assert!(err.contains("invalid recipe JSON"), "got: {err}");
    }

    #[test]
    fn summarize_pulls_text_content() {
        let result = json!({
            "content": [{"type": "text", "text": "{\"doc_id\":\"x\",\"w\":8}"}]
        });
        assert_eq!(summarize(&result), "{\"doc_id\":\"x\",\"w\":8}");
    }

    #[test]
    fn summarize_truncates_long_lines() {
        let long = "a".repeat(500);
        let result = json!({"content": [{"type": "text", "text": long}]});
        let s = summarize(&result);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 201);
    }
}
