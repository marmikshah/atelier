//! `atelier replay <recipe.json>` — drive the tool surface through a scripted
//! list of tool calls, in-process.
//!
//! Every step goes through `Atelier::dispatch` — the same single path MCP
//! clients and `atelier call` take, journaling and write ordering included —
//! one call at a time, strictly in order: a recipe is a narrative, step N may
//! depend on step N-1's mutations.
//!
//! Output convention: the per-step log goes to stdout (scriptable, the recipe's
//! visible result), while status/diagnostics go to stderr (header, errors, the
//! final "N step(s) ok" tally) so they don't pollute piped stdout.

use std::collections::HashMap;

use serde_json::{Value, json};

use atelier_mcp::recipe::{Recipe, Step};
use atelier_mcp::server::{self, Atelier};
use atelier_studio::{Studio, ToolName};
use rmcp::model::CallToolResult;

/// One-line usage banner, shared by the `--help` path and the arg-error paths.
const USAGE: &str = "usage: atelier replay <recipe.json | doc-id> [--home DIR]";

/// Read the recipe to replay: either a file, or the journal of a document in
/// the store.
///
/// A path wins over an id, so a file named like a document still replays as the
/// file the user pointed at.
///
/// The lookup deliberately ignores `--home`: that flag names where the replay
/// *writes*, so `replay jt --home /tmp/sandbox` means "rebuild jt over there",
/// and reading the journal from the destination would only ever find an empty
/// store. Point `ATELIER_HOME` at a different store to read from one.
fn resolve_source(path: &str) -> Result<String, String> {
    let as_file = std::path::Path::new(path);
    if as_file.is_file() {
        return std::fs::read_to_string(as_file).map_err(|e| format!("cannot read {path}: {e}"));
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
            "document '{path}' has no journal — no replay source is available"
        ));
    }
    std::fs::read_to_string(&journal).map_err(|e| format!("cannot read {path}'s journal: {e}"))
}

/// Entry point for the `replay` subcommand. Returns a process exit code.
/// `args` is everything after `replay` on the command line. Async because it
/// runs inside main's tokio runtime, driving the dispatch loop.
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
    let src = match resolve_source(path) {
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

    match drive(recipe, home).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("replay: {e}");
            1
        }
    }
}

/// Build the in-process tool server and run every step in order.
async fn drive(recipe: Recipe, home: Option<&str>) -> Result<(), String> {
    // `--home` roots an isolated store for the run; otherwise the ambient
    // ATELIER_HOME (or the default) is where the rebuild lands.
    let atelier = match home {
        Some(dir) => Atelier::with_studio(Studio::with_docs_dir(dir.into())),
        None => Atelier::new(),
    };
    run_session(&recipe, &atelier).await
}

/// The dispatch loop: one `dispatch` per step, in recipe order.
///
/// Recorded document ids never reach the server verbatim: `doc_new` always
/// mints a fresh opaque id, so every later target is rewritten to the id this
/// run actually received. Authored recipes name the returned value with an
/// explicit `bind` and refer to it as `$name`; journals carry the concrete id
/// captured from the original live call.
async fn run_session(recipe: &Recipe, atelier: &Atelier) -> Result<(), String> {
    // Header line (to stderr, status channel) so the run is self-identifying.
    eprintln!("== {} — {}", recipe.name, recipe.description);

    let mut ids: HashMap<String, String> = HashMap::new();
    for (idx, step) in recipe.steps.iter().enumerate() {
        let mut args = step.args.clone();
        let recorded = if step.tool == ToolName::DocNew {
            match new_recorded_id(step, &mut args, recipe.is_journal()) {
                Ok(recorded) => recorded,
                Err(error) => {
                    print_step(idx, step, &format!("ERROR {error}"));
                    return Err(format!("step {} ({}) failed: {error}", idx + 1, step.tool));
                }
            }
        } else {
            if let Err(error) = remap_ids(&mut args, &ids) {
                print_step(idx, step, &format!("ERROR {error}"));
                return Err(format!("step {} ({}) failed: {error}", idx + 1, step.tool));
            }
            None
        };
        let result = match atelier.dispatch(step.tool, args, "replay").await {
            Ok(result) => result,
            // Protocol error (malformed args) — recipe parsing already rejects
            // unknown tools. A recipe is a narrative, so the first failed step
            // ends the run.
            Err(e) => {
                print_step(idx, step, &format!("ERROR {e}"));
                return Err(format!("step {} ({}) failed: {e}", idx + 1, step.tool));
            }
        };
        // atelier tools surface their own errors as a {"error": ...} text
        // payload with isError set; treat that as a failed step too.
        let is_error = server::is_error_result(&result);
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
                    "step {} (doc_new) returned no document id — cannot remap later steps",
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

/// Resolve the identity represented by `doc_new` and remove journal-only
/// output metadata before dispatch. A journal carries the concrete id returned
/// by the original call. An authored recipe names that result with `bind`.
fn new_recorded_id(step: &Step, args: &mut Value, journal: bool) -> Result<Option<String>, String> {
    let obj = args
        .as_object_mut()
        .ok_or("doc_new args must be a JSON object")?;
    let stamped = match obj.remove("doc_id") {
        None => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id),
        Some(_) => return Err("doc_new doc_id must be a non-empty string".into()),
    };
    if journal {
        return stamped.map(Some).ok_or_else(|| {
            "journal doc_new is missing required args.doc_id — rewrite the journal in the current format"
                .into()
        });
    }
    if stamped.is_some() {
        return Err("authored doc_new must not include journal-only args.doc_id".into());
    }
    step.bind
        .as_ref()
        .map(|binding| Some(format!("${binding}")))
        .ok_or_else(|| "authored doc_new needs an explicit bind".into())
}

/// Rewrite every recorded document id in `args` through the remap table.
/// Covers each id-bearing field on the tool surface: `doc_id` everywhere,
/// plus doc_palette's `set_doc`.
fn remap_ids(args: &mut Value, ids: &HashMap<String, String>) -> Result<(), String> {
    let Some(obj) = args.as_object_mut() else {
        return Ok(());
    };
    for key in ["doc_id", "set_doc"] {
        let Some(recorded) = obj.get(key).and_then(Value::as_str) else {
            continue;
        };
        if let Some(mapped) = ids.get(recorded) {
            obj.insert(key.into(), json!(mapped));
        } else if recorded.starts_with('$') {
            return Err(format!(
                "unresolved document binding '{recorded}' in `{key}`"
            ));
        }
    }
    Ok(())
}

/// Pull the minted id out of a `doc_new` result.
fn minted_id(result: &CallToolResult) -> Option<String> {
    server::result_json(result)?
        .get("doc_id")
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

/// Condense a tool result into a single readable line. atelier returns its
/// JSON payload in a text content block — but image-first results (doc_look,
/// diff overlays) put the PNG block before it, so take the first TEXT block
/// rather than dumping base64; fall back to a debug dump of the shape.
fn summarize(result: &CallToolResult) -> String {
    let line = match result.content.iter().find_map(|c| c.as_text()) {
        Some(t) => t.text.clone(),
        None => format!("{result:?}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ContentBlock as Content;

    fn text_result(text: &str) -> CallToolResult {
        CallToolResult::success(vec![Content::text(text)])
    }

    #[test]
    fn parses_full_recipe() {
        let src = r#"{
            "name": "demo",
            "description": "tiny",
            "steps": [
                {"tool": "doc_new", "bind": "doc", "args": {"name": "x", "width": 8, "height": 8}},
                {"tool": "doc_info", "args": {"doc_id": "$doc"}, "note": "inspect"}
            ]
        }"#;
        let r = Recipe::parse(src).expect("should parse");
        assert_eq!(r.name, "demo");
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[0].tool, ToolName::DocNew);
        assert_eq!(r.steps[1].note.as_deref(), Some("inspect"));
    }

    #[test]
    fn args_are_required() {
        let src = r#"{
            "name": "n",
            "description": "d",
            "steps": [{"tool": "list_docs"}]
        }"#;
        assert!(Recipe::parse(src).is_err());
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
    fn new_identity_separates_journals_from_authored_bindings() {
        let journal_step = Step {
            tool: ToolName::DocNew,
            args: json!({}),
            bind: None,
            note: None,
        };
        // A current journal requires its stamp and strips it from sent args.
        let mut args = json!({"name": "Hero", "doc_id": "d_0000000000000000"});
        assert_eq!(
            new_recorded_id(&journal_step, &mut args, true)
                .unwrap()
                .as_deref(),
            Some("d_0000000000000000")
        );
        assert_eq!(args, json!({"name": "Hero"}));

        // An unstamped journal is obsolete rather than inferred from location.
        let mut args = json!({"name": "Hero"});
        assert!(new_recorded_id(&journal_step, &mut args, true).is_err());

        // An authored recipe names the returned value rather than predicting it.
        let authored_step = Step {
            tool: ToolName::DocNew,
            args: json!({}),
            bind: Some("hero".into()),
            note: None,
        };
        let mut args = json!({"name": "Invader March"});
        assert_eq!(
            new_recorded_id(&authored_step, &mut args, false)
                .unwrap()
                .as_deref(),
            Some("$hero")
        );

        // The two source forms cannot be silently mixed.
        let mut args = json!({"name": "Hero", "doc_id": "d_0000000000000000"});
        assert!(new_recorded_id(&authored_step, &mut args, false).is_err());
    }

    #[test]
    fn remap_rewrites_every_id_bearing_field() {
        let ids: HashMap<String, String> =
            [("$hero".to_string(), "d_0000000000000000".to_string())].into();
        let mut args = json!({
            "doc_id": "$hero",
            "set_doc": "$hero",
            "name": "hero"
        });
        remap_ids(&mut args, &ids).unwrap();
        assert_eq!(
            args,
            json!({
                "doc_id": "d_0000000000000000",
                "set_doc": "d_0000000000000000",
                "name": "hero"
            })
        );
        let mut unresolved = json!({"doc_id": "$missing"});
        assert!(remap_ids(&mut unresolved, &ids).is_err());
    }

    #[test]
    fn minted_id_reads_the_new_payload() {
        let result = text_result("{\"doc_id\":\"d_0000000000000000\",\"width\":8}");
        assert_eq!(minted_id(&result).as_deref(), Some("d_0000000000000000"));
        let empty = CallToolResult::success(vec![]);
        assert_eq!(minted_id(&empty), None);
    }

    #[test]
    fn summarize_pulls_text_content() {
        let result = text_result("{\"doc_id\":\"x\",\"w\":8}");
        assert_eq!(summarize(&result), "{\"doc_id\":\"x\",\"w\":8}");
    }

    #[test]
    fn summarize_skips_leading_image_blocks() {
        // img_result puts the PNG first and the JSON report second — the log
        // line must be the report, never truncated base64.
        let result = CallToolResult::success(vec![
            Content::image("iVBORw0KGgo".to_string(), "image/png".to_string()),
            Content::text("{\"path\":\"p.png\"}"),
        ]);
        assert_eq!(summarize(&result), "{\"path\":\"p.png\"}");
    }

    #[test]
    fn summarize_truncates_long_lines() {
        let long = "a".repeat(500);
        let result = text_result(&long);
        let s = summarize(&result);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 201);
    }

    /// The exact-pixel gate replay has always claimed but never enforced:
    /// a document rebuilt from its own journal must render byte-identical
    /// to the original. Bytes, not just structure — a deterministic encoder
    /// makes byte equality exactly pixel equality.
    #[tokio::test]
    async fn replayed_document_renders_pixel_identical_to_the_original() {
        async fn dispatch_ok(atelier: &Atelier, tool: &str, args: Value) -> Value {
            let name = tool.parse::<ToolName>().unwrap();
            let r = atelier
                .dispatch(name, args, "test")
                .await
                .expect("dispatch");
            assert!(
                !server::is_error_result(&r),
                "{tool} failed: {:?}",
                server::result_json(&r)
            );
            server::result_json(&r).unwrap()
        }

        let tag = format!("atelier-test-replay-px-{}", std::process::id());
        let (dir_a, dir_b) = (
            std::env::temp_dir().join(format!("{tag}-a")),
            std::env::temp_dir().join(format!("{tag}-b")),
        );
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);

        // Build a document through dispatch, so every mutation lands in its
        // journal — palette set, index-legend grid paint, a raw draw.
        let studio_a = Studio::with_docs_dir(dir_a.clone());
        let atelier_a = Atelier::with_studio(studio_a.clone());
        let created = dispatch_ok(
            &atelier_a,
            "doc_new",
            json!({"name": "px", "width": 8, "height": 8}),
        )
        .await;
        let doc_id = created["doc_id"].as_str().unwrap();
        dispatch_ok(
            &atelier_a,
            "doc_palette",
            json!({"op": "set", "doc_id": doc_id, "colors": [[10, 20, 30], [200, 30, 30], [30, 30, 200]]}),
        )
        .await;
        dispatch_ok(
            &atelier_a,
            "doc_paint_grid",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "x": 1, "y": 1,
                   "legend": {"a": 0, "b": 1}, "rows": ["aba", "bab", "aba"]}),
        )
        .await;
        dispatch_ok(
            &atelier_a,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "op": "rect",
                   "x0": 5, "y0": 5, "x1": 7, "y1": 7, "color": [30, 30, 200], "fill": true}),
        )
        .await;
        let render = |studio: &Studio, id: &str| {
            studio
                .look(
                    id,
                    0,
                    &atelier_studio::LookOptions {
                        scale: Some(4),
                        ..Default::default()
                    },
                )
                .unwrap()
                .0
        };
        let original = render(&studio_a, doc_id);

        // Replay the journal into a fresh store and export the rebuild.
        let journal =
            std::fs::read_to_string(dir_a.join(doc_id).join(atelier_studio::JOURNAL_FILE)).unwrap();
        let recipe = Recipe::parse(&journal).unwrap();
        assert!(recipe.steps.len() >= 4, "the journal drives the rebuild");
        let studio_b = Studio::with_docs_dir(dir_b.clone());
        let atelier_b = Atelier::with_studio(studio_b.clone());
        run_session(&recipe, &atelier_b).await.unwrap();
        let replayed_docs = studio_b.list_docs();
        let replay_id = replayed_docs["documents"][0]["doc_id"].as_str().unwrap();
        let replayed = render(&studio_b, replay_id);

        assert_eq!(
            original, replayed,
            "a journaled rebuild must export pixel-identical pixels"
        );
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}
