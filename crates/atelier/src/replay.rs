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

use std::collections::HashMap;
use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

// The recipe format lives in the library crate (shared with the Recorder that
// writes it); the runner below only consumes it.
use atelier_mcp::recipe::{Recipe, Step};

/// One-line usage banner, shared by the `--help` path and the arg-error paths.
const USAGE: &str = "usage: atelier replay <recipe.json | doc-id> [--home DIR]";

/// Read the recipe to replay: either a file, or the journal of a document in
/// the store. The second value is the journal's own document id when the
/// source is a document — old journals predate the stamped `doc_id` on their
/// `doc_create` line, and the directory name is the ground truth for them.
///
/// A path wins over an id, so a file named like a document still replays as the
/// file the user pointed at.
///
/// The lookup deliberately ignores `--home`: that flag names where the replay
/// *writes*, so `replay jt --home /tmp/sandbox` means "rebuild jt over there",
/// and reading the journal from the destination would only ever find an empty
/// store. Point `ATELIER_HOME` at a different store to read from one.
fn resolve_source(path: &str) -> Result<(String, Option<String>), String> {
    let as_file = std::path::Path::new(path);
    if as_file.is_file() {
        return std::fs::read_to_string(as_file)
            .map(|s| (s, None))
            .map_err(|e| format!("cannot read {path}: {e}"));
    }
    let root = crate::service::default_home();
    let doc = root.join("documents").join(path);
    if !doc.is_dir() {
        return Err(format!(
            "no file or document '{path}' (looked in {})",
            root.join("documents").display()
        ));
    }
    let journal = doc.join(atelier_studio::JOURNAL_FILE);
    if !journal.is_file() {
        return Err(format!(
            "document '{path}' has no journal — it predates journaling, or was \
             built by a client that never wrote one"
        ));
    }
    std::fs::read_to_string(&journal)
        .map(|s| (s, Some(path.to_string())))
        .map_err(|e| format!("cannot read {path}'s journal: {e}"))
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

    // A bare document id replays that document's own journal — the whole point
    // of journaling by default is that you never had to keep a recipe file.
    let (src, journal_id) = match resolve_source(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("replay: {e}");
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

    match drive(recipe, journal_id, home).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("replay: {e}");
            1
        }
    }
}

/// Spawn the child server, handshake, and run every step in order.
async fn drive(
    recipe: Recipe,
    journal_id: Option<String>,
    home: Option<&str>,
) -> Result<(), String> {
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

    let result = run_session(&recipe, journal_id, &mut stdin, &mut reader).await;

    // Close the child's stdin so its stdio server loop terminates, then reap.
    drop(stdin);
    let _ = child.wait().await;
    result
}

/// The MCP conversation: handshake then one `tools/call` per step.
///
/// Recorded document ids never reach the server verbatim: `doc_create` re-mints
/// its id in the destination store (a collision mints `name-2`), so every step
/// is rewritten from the id the recipe recorded to the id this run actually
/// got. Without that, replaying into a store where the id already exists sends
/// every draw to the LIVE original instead of the fresh copy.
async fn run_session(
    recipe: &Recipe,
    journal_id: Option<String>,
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
    let mut ids: HashMap<String, String> = HashMap::new();
    // Old journals predate the stamped doc_id on their doc_create line; the
    // journal's directory name is the recorded id, and it binds to the first
    // (a per-document journal's only) create.
    let mut journal_id = journal_id;
    for (idx, step) in recipe.steps.iter().enumerate() {
        let mut args = step.args.clone();
        let recorded = if step.tool == "doc_create" {
            create_recorded_id(&mut args, journal_id.take().as_deref())
        } else {
            remap_ids(&mut args, &ids);
            None
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": idx + 1,
            "method": "tools/call",
            "params": {"name": step.tool, "arguments": args}
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

        // Bind recorded id → minted id so every later step follows this run's
        // document, not whatever the recorded id happens to name in the store.
        if let Some(recorded) = recorded {
            let Some(minted) = minted_id(&result) else {
                return Err(format!(
                    "step {} (doc_create) returned no document id — cannot remap later steps",
                    idx + 1
                ));
            };
            if recorded != minted {
                eprintln!("replay: '{recorded}' rebuilds as '{minted}'");
            }
            ids.insert(recorded, minted);
        }
    }

    eprintln!("replay: {} step(s) ok", recipe.steps.len());
    Ok(())
}

/// The id this `doc_create` stood for when recorded: the journal-stamped
/// `doc_id` if present (stripped before sending — the tool mints its own),
/// else the journal's own id (old journals predate stamping), else the slug
/// the name would mint into an empty store (authored recipes).
fn create_recorded_id(args: &mut Value, journal_id: Option<&str>) -> Option<String> {
    let stamped = args
        .as_object_mut()
        .and_then(|o| o.remove("doc_id"))
        .and_then(|v| v.as_str().map(str::to_string));
    stamped
        .or_else(|| journal_id.map(str::to_string))
        .or_else(|| {
            args.get("name")
                .and_then(Value::as_str)
                .map(atelier_studio::slugify)
        })
}

/// Rewrite every recorded document id in `args` through the remap table.
/// Covers each id-bearing field on the tool surface: `doc_id` everywhere,
/// plus doc_palette's `set_doc`, `from_doc` and `ids`.
fn remap_ids(args: &mut Value, ids: &HashMap<String, String>) {
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    for key in ["doc_id", "set_doc", "from_doc"] {
        if let Some(mapped) = obj
            .get(key)
            .and_then(Value::as_str)
            .and_then(|v| ids.get(v))
        {
            obj.insert(key.into(), json!(mapped));
        }
    }
    if let Some(list) = obj.get_mut("ids").and_then(Value::as_array_mut) {
        for item in list {
            if let Some(mapped) = item.as_str().and_then(|v| ids.get(v)) {
                *item = json!(mapped);
            }
        }
    }
}

/// Pull the minted id out of a `doc_create` result: the first text content
/// block carries the tool's JSON payload, whose `id` field is the new id.
fn minted_id(result: &Value) -> Option<String> {
    let text = result
        .get("content")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|b| b.get("text").and_then(Value::as_str))?;
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
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
/// its JSON payload in a text content block — but image-first results
/// (doc_look, diff overlays) put the PNG block before it, so find
/// the first TEXT block rather than dumping base64 from the first block; fall back
/// to a compact dump of whatever shape we get.
fn summarize(result: &Value) -> String {
    let text = result.get("content").and_then(Value::as_array).and_then(
        |blocks: &Vec<Value>| -> Option<&str> {
            blocks
                .iter()
                .find_map(|b| b.get("text").and_then(Value::as_str))
        },
    );
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
    fn create_hint_prefers_stamp_then_journal_then_slug() {
        // Journal-stamped id wins and is stripped from the sent args.
        let mut args = json!({"name": "Hero", "doc_id": "hero-2"});
        assert_eq!(
            create_recorded_id(&mut args, Some("ignored")).as_deref(),
            Some("hero-2")
        );
        assert_eq!(args, json!({"name": "Hero"}));

        // Old journal: the directory name is the recorded id.
        let mut args = json!({"name": "Hero"});
        assert_eq!(
            create_recorded_id(&mut args, Some("hero-2")).as_deref(),
            Some("hero-2")
        );

        // Authored recipe: predict the slug an empty store would mint.
        let mut args = json!({"name": "Invader March"});
        assert_eq!(
            create_recorded_id(&mut args, None).as_deref(),
            Some("invader-march")
        );
    }

    #[test]
    fn remap_rewrites_every_id_bearing_field() {
        let ids: HashMap<String, String> = [("hero".to_string(), "hero-2".to_string())].into();
        let mut args = json!({
            "doc_id": "hero",
            "set_doc": "hero",
            "from_doc": "hero",
            "ids": ["hero", "villain"],
            "name": "hero"
        });
        remap_ids(&mut args, &ids);
        assert_eq!(
            args,
            json!({
                "doc_id": "hero-2",
                "set_doc": "hero-2",
                "from_doc": "hero-2",
                "ids": ["hero-2", "villain"],
                "name": "hero"
            })
        );
    }

    #[test]
    fn minted_id_reads_the_create_payload() {
        let result = json!({
            "content": [{"type": "text", "text": "{\"id\":\"hero-2\",\"width\":8}"}]
        });
        assert_eq!(minted_id(&result).as_deref(), Some("hero-2"));
        assert_eq!(minted_id(&json!({"content": []})), None);
    }

    #[test]
    fn summarize_pulls_text_content() {
        let result = json!({
            "content": [{"type": "text", "text": "{\"doc_id\":\"x\",\"w\":8}"}]
        });
        assert_eq!(summarize(&result), "{\"doc_id\":\"x\",\"w\":8}");
    }

    #[test]
    fn summarize_skips_leading_image_blocks() {
        // img_result puts the PNG first and the JSON report second — the log
        // line must be the report, never truncated base64.
        let result = json!({
            "content": [
                {"type": "image", "data": "iVBORw0KGgo…", "mimeType": "image/png"},
                {"type": "text", "text": "{\"path\":\"p.png\"}"}
            ]
        });
        assert_eq!(summarize(&result), "{\"path\":\"p.png\"}");
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
