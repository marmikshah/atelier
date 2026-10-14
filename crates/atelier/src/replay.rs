//! `atelier replay <file.recipe | doc-id>` — rebuild a document from its
//! recipe, in process.
//!
//! A recipe records what happened to a document: document-level calls as
//! themselves, and every change to a cel as the pixels it left behind. Calls
//! go through `Atelier::dispatch`, the one path MCP clients and `atelier call`
//! take. Effects go through `Atelier::replay_effect`, which pastes the
//! recorded pixels under the same ordering and transaction rules, so the
//! rebuilt picture never depends on how a drawing tool behaves today. The
//! whole run happens in an isolated document generation and becomes visible
//! only after every statement succeeds.
//!
//! Output convention: the per-step log goes to stdout (scriptable, the
//! recipe's visible result), while status/diagnostics go to stderr (header,
//! errors, the final "N step(s) ok" tally) so they don't pollute piped stdout.
//!
//! `--show` reads the same file and prints each statement with its recorded
//! pixels in terminal colour, for a person to read. It touches no store.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use atelier_mcp::server::{self, Atelier};
use atelier_studio::{
    Change, LayerRef, Legend, MAX_JOURNAL_BYTES, ParsedJournal, Pixel, Scope, Statement, Studio,
    ToolName, encode_statements, parse_journal,
};
use rmcp::model::CallToolResult;

/// One-line usage banner, shared by the `--help` path and the arg-error paths.
const USAGE: &str = "usage: atelier replay <file.recipe | doc-id> [--show] [--home DIR]";

/// Read the recipe to replay: either a file, or the journal of a document in
/// the store.
///
/// A path wins over an id, so a file named like a document still replays as the
/// file the user pointed at.
///
/// For a document ID, source and destination use the same selected store.
/// Pass a journal file to replay into a different store with `--home`.
fn read_source(path: &Path, label: &str) -> Result<String, String> {
    let file =
        std::fs::File::open(path).map_err(|error| format!("cannot read {label}: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {label}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("cannot read {label}: source is not a regular file"));
    }
    let length = metadata.len();
    if length > MAX_JOURNAL_BYTES {
        return Err(format!(
            "cannot read {label}: source is {length} bytes, over the {MAX_JOURNAL_BYTES}-byte replay limit"
        ));
    }
    let capacity = usize::try_from(length.min(MAX_JOURNAL_BYTES)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_JOURNAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(format!(
            "cannot read {label}: source grew beyond the {MAX_JOURNAL_BYTES}-byte replay limit while it was read"
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("cannot read {label}: not UTF-8: {error}"))
}

/// Resolve the recipe text: a readable file wins, otherwise the path is a
/// document id whose own journal is the source. `home` must be the same store
/// the replay writes into, or a bare id would be read from one store and
/// rebuilt in another.
fn resolve_source(path: &str, home: Option<&str>) -> Result<String, String> {
    let as_file = Path::new(path);
    if as_file.is_file() {
        return read_source(as_file, path);
    }
    let root = match home {
        Some(dir) => PathBuf::from(dir),
        None => crate::service::default_home(),
    };
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
    read_source(&journal, &format!("{path}'s journal"))
}

/// Entry point for the `replay` subcommand. Returns a process exit code.
/// `args` is everything after `replay` on the command line. Async because it
/// runs inside main's tokio runtime, driving the dispatch loop.
pub async fn run(args: &[String]) -> i32 {
    // Parse args: a single positional recipe path plus optional `--home <dir>`.
    let mut path: Option<&str> = None;
    let mut home: Option<&str> = None;
    let mut show = false;
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
            "--show" => {
                show = true;
                i += 1;
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
    let src = match resolve_source(path, home) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("replay: {e}");
            return 2;
        }
    };
    let recipe = match parse_journal(&src) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay: {e}");
            return 2;
        }
    };

    if recipe.statements.is_empty() {
        eprintln!("replay: recipe has no complete steps");
        return 2;
    }
    if recipe.torn_tail {
        eprintln!("replay: ignored an incomplete final journal line");
    }
    if show {
        return match show_recipe(&recipe) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("replay: {error}");
                1
            }
        };
    }
    match drive(recipe, home).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("replay: {e}");
            1
        }
    }
}

/// Build the in-process tool server and run every step in order.
async fn drive(recipe: ParsedJournal, home: Option<&str>) -> Result<(), String> {
    // `--home` roots an isolated store for the run; otherwise the ambient
    // ATELIER_HOME (or the default) is where the rebuild lands.
    let studio = match home {
        Some(dir) => Studio::with_home(dir.into()),
        None => Studio::new(),
    };
    run_atomic_session(&recipe, &studio).await.map(|_| ())
}

/// Replay into one private generation, then publish the completed document.
///
/// The outer store lock protects transaction cleanup and the final publication
/// from other processes. Per-step dispatch still uses its normal transaction
/// path, but those commits are visible only inside this outer generation.
async fn run_atomic_session(recipe: &ParsedJournal, studio: &Studio) -> Result<String, String> {
    let _store_lock = studio.lock_store_exclusive()?;
    studio.cleanup_stale_transactions()?;
    let transaction = studio.begin_transaction(None)?;
    let staged = Atelier::with_studio(transaction.studio().clone());
    let minted = run_session(recipe, &staged)
        .await
        .map_err(|error| format!("{error}; no replayed document was published"))?;

    let commit = transaction
        .commit(&minted)
        .map_err(|error| format!("{error}; no replayed document was published"))?;
    match commit {
        atelier_studio::CommitOutcome::Durable => {}
        atelier_studio::CommitOutcome::DurabilityUncertain { warning } => {
            eprintln!("replay: warning: {warning}");
        }
    }
    eprintln!(
        "replay: {} step(s) committed atomically",
        recipe.statements.len()
    );
    Ok(minted)
}

/// The replay loop: one statement at a time, in recipe order, against an
/// isolated store. Returns the newly minted document id for publication.
///
/// Recorded document ids never reach the server verbatim: `doc_new` always
/// mints a fresh opaque id, so every later target is rewritten to the id this
/// run actually received.
async fn run_session(recipe: &ParsedJournal, atelier: &Atelier) -> Result<String, String> {
    eprintln!("== replaying journal");

    let mut identity: Option<(String, String)> = None;
    for (idx, step) in recipe.statements.iter().enumerate() {
        let failed = |error: String| {
            print_step(idx, step, &format!("ERROR {error}"));
            format!("step {} ({}) failed: {error}", idx + 1, label(step))
        };
        let minted = identity.as_ref().map(|(_, minted)| minted.as_str());
        let result = match step {
            Statement::Call { tool, args } => {
                let mut args = args.clone();
                let recorded = if *tool == ToolName::DocNew {
                    Some(take_recorded_id(&mut args).map_err(failed)?)
                } else {
                    remap_ids(&mut args, identity.as_ref()).map_err(failed)?;
                    None
                };
                let result = atelier
                    .dispatch(*tool, Value::Object(args), "replay")
                    .await
                    .map_err(|error| failed(error.to_string()))?;
                // Map recorded id → minted id so every later step follows
                // this run's document, not whatever the recorded id happens
                // to name in the store.
                if let Some(recorded) = recorded
                    && !server::is_error_result(&result)
                {
                    let Some(minted) = minted_id(&result) else {
                        return Err(format!(
                            "step {} (doc_new) returned no document id — cannot remap later steps",
                            idx + 1
                        ));
                    };
                    if recorded != minted {
                        eprintln!("replay: '{recorded}' rebuilds as '{minted}'");
                    }
                    identity = Some((recorded, minted));
                }
                result
            }
            Statement::CelCall { scope, op, args } => {
                let doc_id = minted.ok_or_else(|| failed("no document yet".into()))?;
                let layer = atelier.layer_index(doc_id, &scope.layer).map_err(failed)?;
                let mut call = args.clone();
                call.insert("doc_id".into(), json!(doc_id));
                call.insert("layer".into(), json!(layer));
                call.insert("frame".into(), json!(scope.frame));
                call.insert("op".into(), json!(op));
                atelier
                    .dispatch(ToolName::DocFx, Value::Object(call), "replay")
                    .await
                    .map_err(|error| failed(error.to_string()))?
            }
            Statement::Effect { .. } => {
                let doc_id = minted.ok_or_else(|| failed("no document yet".into()))?;
                atelier
                    .replay_effect(doc_id, step.clone())
                    .await
                    .map_err(|error| failed(error.to_string()))?
            }
        };
        // atelier tools surface their own errors as a {"error": ...} text
        // payload with isError set; treat that as a failed step too. A recipe
        // is a narrative, so the first failed step ends the run.
        let summary = summarize(&result);
        print_step(idx, step, &summary);
        if server::is_error_result(&result) {
            return Err(format!(
                "step {} ({}) failed: {summary}",
                idx + 1,
                label(step)
            ));
        }
    }

    identity
        .map(|(_, minted)| minted)
        .ok_or_else(|| "recipe completed without creating a document".into())
}

/// Remove the concrete id stamped into the recorded `doc_new` arguments. The
/// replayed call mints a new id; later targets are remapped to it.
fn take_recorded_id(args: &mut Map<String, Value>) -> Result<String, String> {
    match args.remove("doc_id") {
        Some(Value::String(id)) if !id.is_empty() => Ok(id),
        Some(_) => Err("doc_new doc_id must be a non-empty string".into()),
        None => Err(
            "journal doc_new is missing required args.doc_id — rewrite the journal in the current format"
                .into(),
        ),
    }
}

/// Rewrite targets through the single recorded-to-minted document identity.
/// Covers each id-bearing field on the tool surface: `doc_id` everywhere,
/// plus doc_palette's `set_doc`.
fn remap_ids(
    args: &mut Map<String, Value>,
    identity: Option<&(String, String)>,
) -> Result<(), String> {
    for key in ["doc_id", "set_doc"] {
        let Some(recorded) = args.get(key).and_then(Value::as_str) else {
            continue;
        };
        if let Some((_, minted)) = identity.filter(|(source, _)| source == recorded) {
            args.insert(key.into(), json!(minted));
        } else {
            return Err(format!(
                "recorded document id '{recorded}' in `{key}` has no doc_new mapping"
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

/// A short name for a statement in the step log: the tool and its op, or the
/// cel and what was done to it.
fn label(step: &Statement) -> String {
    match step {
        Statement::Call { tool, args } => match args.get("op").and_then(Value::as_str) {
            Some(op) => format!("{tool} {op}"),
            None => tool.to_string(),
        },
        Statement::CelCall { scope, op, .. } => format!("{} {op}", scope_text(scope)),
        Statement::Effect {
            scope, annotation, ..
        } => format!("{} {annotation}", scope_text(scope)),
    }
}

fn scope_text(scope: &Scope) -> String {
    match &scope.layer {
        LayerRef::Name(name) => format!("{name:?} @{}", scope.frame),
        LayerRef::Index(index) => format!("#{index} @{}", scope.frame),
    }
}

/// `--show`: print every statement as the recipe spells it, with its pixels
/// drawn in terminal colour. Nothing is replayed and nothing is saved.
fn show_recipe(recipe: &ParsedJournal) -> Result<(), String> {
    let id = recorded_id(recipe);
    let mut legend = Legend::default();
    for (idx, step) in recipe.statements.iter().enumerate() {
        let text = encode_statements(std::slice::from_ref(step), &mut legend, &id)?;
        // The effect line is the last line before any rows; legend lines the
        // encoder added above it are bookkeeping the reader does not need.
        let line = text
            .lines()
            .find(|line| !line.starts_with("legend ") && !line.starts_with("    "))
            .unwrap_or("");
        let mut out = format!("[{}] {line}\n", idx + 1);
        if let Statement::Effect { change, .. } = step {
            out.push_str(&paint_change(change));
        }
        if !emit(&out) {
            return Ok(());
        }
    }
    eprintln!("replay: showed {} step(s)", recipe.statements.len());
    Ok(())
}

/// The document id a recipe was recorded for, stamped into its `doc_new`.
fn recorded_id(recipe: &ParsedJournal) -> String {
    match recipe.statements.first() {
        Some(Statement::Call { args, .. }) => args
            .get("doc_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Grids above this many pixels are summarised rather than drawn.
const SHOW_AREA_CAP: u64 = 4096;

fn paint_change(change: &Change) -> String {
    match change {
        Change::None => String::new(),
        Change::Solid {
            x0,
            y0,
            x1,
            y1,
            color,
        } => {
            let (width, height) = (u64::from(x1 - x0) + 1, u64::from(y1 - y0) + 1);
            if width * height > SHOW_AREA_CAP {
                return format!("  ({width}×{height} of one colour)\n");
            }
            let row = vec![*color; width as usize];
            (0..height)
                .map(|_| format!("  {}\n", paint_row(&row)))
                .collect()
        }
        Change::Rows { rows, .. } => {
            let area = rows.len() as u64 * rows.first().map(Vec::len).unwrap_or(0) as u64;
            if area > SHOW_AREA_CAP {
                return "  (area too large to draw)\n".into();
            }
            rows.iter()
                .map(|row| format!("  {}\n", paint_row(row)))
                .collect()
        }
    }
}

/// One row of pixels as terminal true-colour cells, two columns per pixel so
/// pixels come out roughly square.
fn paint_row(row: &[Pixel]) -> String {
    let mut out = String::new();
    for pixel in row {
        if pixel[3] == 0 {
            out.push_str("\x1b[0;2m··");
        } else {
            out.push_str(&format!(
                "\x1b[48;2;{};{};{}m  ",
                pixel[0], pixel[1], pixel[2]
            ));
        }
    }
    out.push_str("\x1b[0m");
    out
}

/// Write to stdout, reporting a closed pipe instead of panicking on it.
fn emit(text: &str) -> bool {
    use std::io::Write;
    std::io::stdout().write_all(text.as_bytes()).is_ok()
}

/// One-line per-step report: `[N] step — summary`. Goes to stdout. A closed
/// pipe does not stop the replay; the document is still rebuilt.
fn print_step(idx: usize, step: &Statement, summary: &str) {
    emit(&format!("[{}] {} — {summary}\n", idx + 1, label(step)));
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

    const ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const RECORDED_ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    const MISSING_ID: &str = "f47ac10b-58cc-4372-a567-0e02b2c3d479";

    fn text_result(text: &str) -> CallToolResult {
        CallToolResult::success(vec![Content::text(text)])
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn recorded_identity_is_required_and_removed_before_dispatch() {
        let mut args = object(json!({
            "name": "Hero",
            "doc_id": ID
        }));
        assert_eq!(take_recorded_id(&mut args).unwrap(), ID);
        assert_eq!(args, object(json!({"name": "Hero"})));

        let mut unstamped = object(json!({"name": "Hero"}));
        assert!(take_recorded_id(&mut unstamped).is_err());
    }

    #[test]
    fn remap_rewrites_every_id_bearing_field() {
        let identity = (RECORDED_ID.to_string(), ID.to_string());
        let mut args = object(json!({
            "doc_id": RECORDED_ID,
            "set_doc": RECORDED_ID,
            "name": "hero"
        }));
        remap_ids(&mut args, Some(&identity)).unwrap();
        assert_eq!(
            args,
            object(json!({
                "doc_id": ID,
                "set_doc": ID,
                "name": "hero"
            }))
        );
        let mut unresolved = object(json!({"doc_id": MISSING_ID}));
        assert!(remap_ids(&mut unresolved, Some(&identity)).is_err());
    }

    #[test]
    fn minted_id_reads_the_new_payload() {
        let result = text_result(&format!("{{\"doc_id\":\"{ID}\",\"width\":8}}"));
        assert_eq!(minted_id(&result).as_deref(), Some(ID));
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

    #[test]
    fn source_size_is_rejected_from_metadata_before_reading() {
        let path = std::env::temp_dir().join(format!(
            "atelier-replay-oversize-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_JOURNAL_BYTES + 1).unwrap();

        let error = read_source(&path, "oversize recipe").unwrap_err();
        assert!(
            error.contains(&format!("{}-byte replay limit", MAX_JOURNAL_BYTES)),
            "{error}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn show_paints_the_recorded_pixels_without_a_store() {
        let recipe = parse_journal(&format!(
            "atelier 5 {RECORDED_ID} 0.1.0\n\
             new name=shown width=4 height=4\n\
             layer add name=ink\n\
             legend A=#102030\n\
             ink @0 grid = 1,2 3x1\n    A.A\n\
             ink @0 erase pencil = 1,2 1,2 .\n\
             ink @0 erase pencil ~\n"
        ))
        .unwrap();
        let ink = "\x1b[48;2;16;32;48m  ";
        let mut shown = String::new();
        let mut legend = Legend::default();
        assert_eq!(recorded_id(&recipe), RECORDED_ID);
        for (idx, step) in recipe.statements.iter().enumerate() {
            let text =
                encode_statements(std::slice::from_ref(step), &mut legend, RECORDED_ID).unwrap();
            let line = text
                .lines()
                .find(|line| !line.starts_with("legend ") && !line.starts_with("    "))
                .unwrap();
            shown.push_str(&format!("[{}] {line}\n", idx + 1));
            if let Statement::Effect { change, .. } = step {
                shown.push_str(&paint_change(change));
            }
        }
        assert_eq!(
            shown,
            format!(
                "[1] new name=shown width=4 height=4\n\
                 [2] layer add name=ink\n\
                 [3] ink @0 grid = 1,2 3x1\n  {ink}\x1b[0;2m··{ink}\x1b[0m\n\
                 [4] ink @0 erase pencil = 1,2 1,2 .\n  \x1b[0;2m··\x1b[0m\n\
                 [5] ink @0 erase pencil ~\n"
            )
        );
        assert_eq!(
            paint_row(&[[16, 32, 48, 0x80], [0, 0, 0, 0]]),
            format!("{ink}\x1b[0;2m··\x1b[0m"),
            "a translucent pixel shows its colour"
        );
        show_recipe(&recipe).unwrap();
    }

    #[tokio::test]
    async fn explicit_home_uses_the_same_documents_layout_as_atelier_home() {
        let home =
            std::env::temp_dir().join(format!("atelier-replay-home-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let recipe = parse_journal(&format!(
            "atelier 5 {RECORDED_ID} 0.1.0\nnew name=home-layout width=4 height=4\n"
        ))
        .unwrap();

        drive(recipe, Some(home.to_string_lossy().as_ref()))
            .await
            .unwrap();

        let studio = Studio::with_home(home.clone());
        assert_eq!(studio.list_docs()["count"], 1);
        assert!(home.join("documents").is_dir());
        assert!(
            std::fs::read_dir(&home)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .all(|entry| entry.file_name() == "documents"),
            "--home must not place document ids directly below the home"
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn failed_recipe_publishes_none_of_its_steps() {
        let dir = std::env::temp_dir().join(format!(
            "atelier-replay-atomic-failure-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let studio = Studio::with_docs_dir(dir.clone());
        let recipe = parse_journal(&format!(
            "atelier 5 {RECORDED_ID} 0.1.0\n\
             new name=rolled-back width=4 height=4\n\
             layer delete\n"
        ))
        .unwrap();

        let error = run_atomic_session(&recipe, &studio).await.unwrap_err();

        assert!(
            error.contains("step 2 (doc_layer delete) failed"),
            "{error}"
        );
        assert!(
            error.contains("no replayed document was published"),
            "{error}"
        );
        assert_eq!(studio.list_docs()["count"], 0);
        let transaction_root = dir.join(".transactions");
        assert!(
            !transaction_root.exists()
                || std::fs::read_dir(&transaction_root)
                    .unwrap()
                    .next()
                    .is_none(),
            "failed replay must not leave a staged generation"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_effect_on_a_missing_layer_fails_the_replay() {
        let dir = std::env::temp_dir().join(format!(
            "atelier-replay-missing-layer-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let studio = Studio::with_docs_dir(dir.clone());
        let recipe = parse_journal(&format!(
            "atelier 5 {RECORDED_ID} 0.1.0\n\
             new name=x width=4 height=4\n\
             ink @0 pencil = 0,0 0,0 #ffffff\n"
        ))
        .unwrap();
        let error = run_atomic_session(&recipe, &studio).await.unwrap_err();
        assert!(error.contains("no layer named"), "{error}");
        assert_eq!(studio.list_docs()["count"], 0);
        let _ = std::fs::remove_dir_all(dir);
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
        // journal — palette set, index-legend grid paint, ranged edits, a
        // translucent stroke, an erase and a no-op.
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
            "doc_frame",
            json!({"doc_id": doc_id, "op": "add", "count": 2}),
        )
        .await;
        dispatch_ok(
            &atelier_a,
            "doc_palette",
            json!({"op": "set", "doc_id": doc_id, "colors": [[10, 20, 30], [200, 30, 30], [30, 30, 200]]}),
        )
        .await;
        let revision_before_range = studio_a.document_revision(doc_id).unwrap();
        let journal_before_range = studio_a.journal(doc_id).unwrap().len();
        let ranged = dispatch_ok(
            &atelier_a,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "frame_to": 2,
                   "op": "pencil", "points": [[0, 0]], "color": [200, 30, 30]}),
        )
        .await;
        assert_eq!(ranged["frames_targeted"], 3);
        assert_eq!(ranged["revision"], revision_before_range + 1);
        let journal_after_range = studio_a.journal(doc_id).unwrap();
        assert_eq!(
            journal_after_range.len(),
            journal_before_range + 3,
            "a ranged call records one effect per cel"
        );
        assert!(matches!(
            journal_after_range.last().unwrap(),
            Statement::Effect { scope, change: Change::Solid { color: [200, 30, 30, 255], .. }, .. }
                if scope.frame == 2
        ));
        assert_eq!(
            studio_a.document_revision(doc_id).unwrap(),
            revision_before_range + 1,
            "one ranged call increments the document revision once"
        );
        dispatch_ok(
            &atelier_a,
            "doc_fx",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "frame_to": 2,
                   "op": "shift", "dx": 1, "dy": 0}),
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
        dispatch_ok(
            &atelier_a,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 1, "op": "line",
                   "x0": 0, "y0": 7, "x1": 7, "y1": 0, "color": [30, 30, 200, 90]}),
        )
        .await;
        dispatch_ok(
            &atelier_a,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "op": "pencil",
                   "points": [[1, 1], [7, 7]], "color": [1, 1, 1], "erase": true}),
        )
        .await;
        let no_op = dispatch_ok(
            &atelier_a,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 2, "op": "pencil",
                   "points": [[7, 7]], "color": [1, 1, 1], "erase": true}),
        )
        .await;
        assert_eq!(no_op["pixels_changed"], 0);
        let render = |studio: &Studio, id: &str, frame: usize| {
            studio
                .look(
                    id,
                    frame,
                    &atelier_studio::LookOptions {
                        scale: Some(4),
                        ..Default::default()
                    },
                )
                .unwrap()
                .0
        };
        let original: Vec<Vec<u8>> = (0..=2)
            .map(|frame| render(&studio_a, doc_id, frame))
            .collect();

        // Replay the journal into a fresh store and export the rebuild.
        let journal =
            std::fs::read_to_string(dir_a.join(doc_id).join(atelier_studio::JOURNAL_FILE)).unwrap();
        let recipe = parse_journal(&journal).unwrap();
        assert!(
            recipe.statements.len() >= 12,
            "the journal drives the rebuild"
        );
        assert!(
            matches!(
                recipe.statements.last().unwrap(),
                Statement::Effect {
                    change: Change::None,
                    ..
                }
            ),
            "a call that changed nothing is recorded as such"
        );
        let studio_b = Studio::with_docs_dir(dir_b.clone());
        let committed_id = run_atomic_session(&recipe, &studio_b).await.unwrap();
        let replayed_docs = studio_b.list_docs();
        let replay_id = replayed_docs["documents"][0]["doc_id"].as_str().unwrap();
        assert_eq!(committed_id, replay_id);
        assert_eq!(
            studio_b.document_revision(replay_id).unwrap(),
            recipe.statements.len() as u64,
            "the outer atomic publication must preserve inner step revisions without adding one"
        );
        let replayed: Vec<Vec<u8>> = (0..=2)
            .map(|frame| render(&studio_b, replay_id, frame))
            .collect();

        assert_eq!(
            original, replayed,
            "a journaled rebuild must export pixel-identical pixels"
        );
        // The rebuilt document's own recipe is the one it was built from.
        let rebuilt =
            std::fs::read_to_string(dir_b.join(replay_id).join(atelier_studio::JOURNAL_FILE))
                .unwrap();
        assert_eq!(
            rebuilt.split_once('\n').unwrap().1,
            journal.split_once('\n').unwrap().1,
            "replay records the same statements it read"
        );
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}
