//! `atelier check` — objective, read-only project validation for local use and
//! CI.
//!
//! The command parses the project manifest, opens every stored document,
//! validates manifest document/tag references, replays each available journal
//! into an isolated temporary store and compares the rebuild with the live
//! document, then exercises every configured export against temporary output
//! paths. Subjective art critique remains an explicit editor tool.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use atelier_mcp::recipe::Recipe;
use atelier_mcp::server::{self, Atelier};
use atelier_studio::{JOURNAL_FILE, Studio};
use serde_json::{Value, json};

use crate::project::Project;

const USAGE: &str = "usage: atelier check [--json]";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug)]
struct Finding {
    kind: &'static str,
    name: String,
    status: Status,
    detail: String,
}

#[derive(Debug)]
struct Report {
    root: PathBuf,
    store: PathBuf,
    findings: Vec<Finding>,
}

impl Report {
    fn new(root: PathBuf, store: PathBuf) -> Self {
        Self {
            root,
            store,
            findings: Vec::new(),
        }
    }

    fn add(
        &mut self,
        status: Status,
        kind: &'static str,
        name: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.findings.push(Finding {
            kind,
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }

    fn counts(&self) -> (usize, usize, usize) {
        let mut passed = 0;
        let mut warnings = 0;
        let mut failed = 0;
        for finding in &self.findings {
            match finding.status {
                Status::Pass => passed += 1,
                Status::Warn => warnings += 1,
                Status::Fail => failed += 1,
            }
        }
        (passed, warnings, failed)
    }

    fn failed(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.status == Status::Fail)
    }

    fn json(&self) -> Value {
        let (passed, warnings, failed) = self.counts();
        json!({
            "version": 1,
            "ok": failed == 0,
            "root": self.root.to_string_lossy(),
            "store": self.store.to_string_lossy(),
            "summary": {
                "passed": passed,
                "warnings": warnings,
                "failed": failed,
            },
            "checks": self.findings.iter().map(|finding| json!({
                "kind": finding.kind,
                "name": finding.name,
                "status": finding.status.as_str(),
                "detail": finding.detail,
            })).collect::<Vec<_>>(),
        })
    }

    fn print_human(&self) {
        println!("atelier check: {}", self.root.display());
        println!("store: {}", self.store.display());
        for finding in &self.findings {
            println!(
                "{:<4} {:<10} {} — {}",
                finding.status.as_str(),
                finding.kind,
                finding.name,
                finding.detail
            );
        }
        let (passed, warnings, failed) = self.counts();
        println!("atelier check: {passed} passed, {warnings} warning(s), {failed} failed");
    }
}

enum Command {
    Run { json: bool },
    Help,
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" if !json => json = true,
            "--json" => return Err("--json may be passed only once".into()),
            "--help" | "-h" => return Ok(Command::Help),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Command::Run { json })
}

/// A unique temporary directory owned by one check run. Creating the directory
/// itself is atomic; Drop removes only the exact path this process created.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        for _ in 0..100 {
            let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("atelier-check-{}-{serial}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot create validation workspace {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err("cannot allocate a unique validation workspace".into())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Entry point for `atelier check`.
pub(crate) async fn run(args: &[String]) -> i32 {
    let json_output = match parse_args(args) {
        Ok(Command::Run { json }) => json,
        Ok(Command::Help) => {
            println!("{USAGE}");
            return 0;
        }
        Err(error) => {
            eprintln!("atelier check: {error}\n{USAGE}");
            return 2;
        }
    };

    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("atelier check: cannot read the current directory: {error}");
            return 1;
        }
    };
    let (store, _) = Studio::resolve_home(std::env::var_os("ATELIER_HOME").as_deref(), &root, None);
    let report = inspect(&root, &store).await;
    if json_output {
        match serde_json::to_string_pretty(&report.json()) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("atelier check: cannot encode JSON report: {error}");
                return 1;
            }
        }
    } else {
        report.print_human();
    }
    i32::from(report.failed())
}

async fn inspect(root: &Path, store_root: &Path) -> Report {
    let mut report = Report::new(root.to_path_buf(), store_root.to_path_buf());
    let project = match Project::load(root) {
        Ok(project) => project,
        Err(error) => {
            report.add(Status::Fail, "manifest", ".atelier/project.toml", error);
            return report;
        }
    };
    report.add(
        Status::Pass,
        "manifest",
        project.manifest().display().to_string(),
        format!(
            "version 1; {} configured export(s)",
            project.exports().len()
        ),
    );

    let docs_dir = store_root.join("documents");
    if !docs_dir.is_dir() {
        report.add(
            Status::Fail,
            "store",
            docs_dir.display().to_string(),
            "documents directory does not exist",
        );
        return report;
    }
    let studio = Studio::with_docs_dir(docs_dir.clone());
    let ids = match document_ids(&docs_dir) {
        Ok(ids) => ids,
        Err(error) => {
            report.add(Status::Fail, "store", docs_dir.display().to_string(), error);
            return report;
        }
    };
    report.add(
        Status::Pass,
        "store",
        docs_dir.display().to_string(),
        format!("{} document(s)", ids.len()),
    );

    let mut infos = BTreeMap::new();
    for id in &ids {
        match studio.doc_info(id) {
            Ok(info) => {
                report.add(Status::Pass, "document", id, document_summary(&info));
                infos.insert(id.clone(), info);
            }
            Err(error) => report.add(Status::Fail, "document", id, error),
        }
    }

    validate_references(&project, &infos, &mut report);

    let needs_scratch = !ids.is_empty() || !project.exports().is_empty();
    let scratch = if needs_scratch {
        match Scratch::new() {
            Ok(scratch) => Some(scratch),
            Err(error) => {
                report.add(Status::Fail, "runtime", "temporary store", error);
                return report;
            }
        }
    } else {
        None
    };

    if let Some(scratch) = &scratch {
        for id in &ids {
            validate_recipe(
                id,
                &docs_dir,
                &studio,
                infos.get(id),
                scratch.path(),
                &mut report,
            )
            .await;
        }
    }

    if project.exports().is_empty() {
        report.add(
            Status::Warn,
            "exports",
            "configured",
            "no exports configured; add [[exports]] when the project has deliverables",
        );
    } else if let Some(scratch) = &scratch {
        validate_exports(&project, &docs_dir, scratch.path(), &mut report).await;
    }

    report
}

fn document_ids(docs_dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(docs_dir)
        .map_err(|error| format!("cannot read {}: {error}", docs_dir.display()))?;
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("cannot read an entry in {}: {error}", docs_dir.display()))?;
        if entry.path().join("doc.json").is_file() {
            ids.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

fn document_summary(info: &Value) -> String {
    let number = |key: &str| {
        info.get(key)
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    };
    format!(
        "{}x{}; {} frame(s), {} layer(s), {} tag(s)",
        info.get("w").and_then(Value::as_u64).unwrap_or(0),
        info.get("h").and_then(Value::as_u64).unwrap_or(0),
        number("frames"),
        number("layers"),
        number("tags"),
    )
}

fn validate_references(project: &Project, infos: &BTreeMap<String, Value>, report: &mut Report) {
    for export in project.exports() {
        let Some(doc) = export.doc() else {
            continue;
        };
        let Some(info) = infos.get(doc) else {
            report.add(
                Status::Fail,
                "reference",
                export.name(),
                format!("document '{doc}' does not exist or could not be opened"),
            );
            continue;
        };
        if let Some(tag) = export.tag() {
            let exists = info
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| {
                    tags.iter()
                        .any(|entry| entry.get("name").and_then(Value::as_str) == Some(tag))
                });
            if !exists {
                report.add(
                    Status::Fail,
                    "reference",
                    export.name(),
                    format!("animation tag '{tag}' does not exist on document '{doc}'"),
                );
                continue;
            }
            report.add(
                Status::Pass,
                "reference",
                export.name(),
                format!("document '{doc}' and animation tag '{tag}' exist"),
            );
        } else {
            report.add(
                Status::Pass,
                "reference",
                export.name(),
                format!("document '{doc}' exists"),
            );
        }
    }
}

async fn validate_recipe(
    id: &str,
    docs_dir: &Path,
    live: &Studio,
    live_info: Option<&Value>,
    scratch: &Path,
    report: &mut Report,
) {
    let path = docs_dir.join(id).join(JOURNAL_FILE);
    let source = match std::fs::read_to_string(&path) {
        Ok(source) if source.trim().is_empty() => {
            report.add(
                Status::Warn,
                "recipe",
                id,
                "journal is empty; this document cannot be reproduced",
            );
            return;
        }
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.add(
                Status::Warn,
                "recipe",
                id,
                "journal is missing; older documents remain usable but are not reproducible",
            );
            return;
        }
        Err(error) => {
            report.add(
                Status::Fail,
                "recipe",
                id,
                format!("cannot read {}: {error}", path.display()),
            );
            return;
        }
    };
    let format = Recipe::source_format(&source);
    if format != "authored-json"
        && let Some((number, line)) = source
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .last()
        && let Err(error) = serde_json::from_str::<Value>(line)
    {
        report.add(
            Status::Fail,
            "recipe",
            id,
            format!(
                "line {} is a partial final append: {error}; replay can recover earlier steps, but the journal is incomplete",
                number + 1
            ),
        );
        return;
    }
    let recipe = match Recipe::parse(&source) {
        Ok(recipe) => recipe,
        Err(error) => {
            report.add(Status::Fail, "recipe", id, error);
            return;
        }
    };
    let steps = recipe.steps.len();
    let replay_docs = scratch.join("replay").join(id).join("documents");
    let replay_studio = Arc::new(Mutex::new(Studio::with_docs_dir(replay_docs)));
    let atelier = Atelier::with_studio(Arc::clone(&replay_studio));
    let remapped =
        match crate::replay::validate_session(&recipe, Some(id.to_string()), &atelier).await {
            Ok(remapped) => remapped,
            Err(error) => {
                report.add(
                    Status::Fail,
                    "recipe",
                    id,
                    format!("{format}, {steps} step(s): replay failed: {error}"),
                );
                return;
            }
        };
    let Some(rebuilt_id) = rebuilt_id(id, &remapped) else {
        report.add(
            Status::Fail,
            "recipe",
            id,
            format!("{format}, {steps} step(s): replay created no matching document"),
        );
        return;
    };
    let replay_studio = replay_studio
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match live_info {
        Some(live_info) => {
            match compare_document(live, id, live_info, &replay_studio, &rebuilt_id) {
                Ok(frames) => report.add(
                    Status::Pass,
                    "recipe",
                    id,
                    format!(
                        "{format}, {steps} step(s); replay structure and {frames} frame(s) match"
                    ),
                ),
                Err(error) => report.add(
                    Status::Fail,
                    "recipe",
                    id,
                    format!("{format}, {steps} step(s): {error}"),
                ),
            }
        }
        None => report.add(
            Status::Pass,
            "recipe",
            id,
            format!("{format}, {steps} step(s) replayed; live comparison unavailable"),
        ),
    }
}

fn rebuilt_id(recorded: &str, remapped: &HashMap<String, String>) -> Option<String> {
    remapped.get(recorded).cloned().or_else(|| {
        (remapped.len() == 1)
            .then(|| remapped.values().next().cloned())
            .flatten()
    })
}

fn compare_document(
    live: &Studio,
    live_id: &str,
    live_info: &Value,
    rebuilt: &Studio,
    rebuilt_id: &str,
) -> Result<usize, String> {
    let rebuilt_info = rebuilt
        .doc_info(rebuilt_id)
        .map_err(|error| format!("cannot inspect replayed document: {error}"))?;
    if comparable_info(live_info.clone()) != comparable_info(rebuilt_info) {
        return Err("replay completed, but document structure differs from live state".into());
    }
    let frames = live_info
        .get("frames")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    for frame in 0..frames {
        let live_png = live
            .render_png_bytes(live_id, frame, 1)
            .map_err(|error| format!("cannot render live frame {frame}: {error}"))?;
        let rebuilt_png = rebuilt
            .render_png_bytes(rebuilt_id, frame, 1)
            .map_err(|error| format!("cannot render replayed frame {frame}: {error}"))?;
        if live_png != rebuilt_png {
            return Err(format!(
                "replay completed, but frame {frame} pixels differ from live state"
            ));
        }
    }
    Ok(frames)
}

/// A reference image is an external review aid, not part of the rebuilt pixel
/// state. Replay intentionally tolerates a missing `doc_ref op=set` path, so
/// exclude that one non-portable field from fidelity comparison.
fn comparable_info(mut info: Value) -> Value {
    if let Some(object) = info.as_object_mut() {
        object.remove("id");
        object.remove("reference");
    }
    info
}

async fn validate_exports(project: &Project, docs_dir: &Path, scratch: &Path, report: &mut Report) {
    let atelier = Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(
        docs_dir.to_path_buf(),
    ))));
    for export in project.exports() {
        let out_path = scratch.join("exports").join(export.out());
        let mut args = export.dispatch_args();
        args["out_path"] = json!(out_path.to_string_lossy().as_ref());
        let result = match atelier.dispatch_quiet("doc_export", args, "check").await {
            Ok(result) => result,
            Err(error) => {
                report.add(Status::Fail, "export", export.name(), error.to_string());
                continue;
            }
        };
        if server::is_error_result(&result) {
            let detail = server::result_json(&result)
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "export failed".into());
            report.add(Status::Fail, "export", export.name(), detail);
        } else {
            report.add(
                Status::Pass,
                "export",
                export.name(),
                format!(
                    "op={} validated without writing project output '{}'",
                    export.op(),
                    export.out()
                ),
            );
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
    fn arguments_are_strict() {
        assert!(matches!(
            parse_args(&argv(&["--json"])),
            Ok(Command::Run { json: true })
        ));
        assert!(matches!(parse_args(&argv(&["--help"])), Ok(Command::Help)));
        assert!(parse_args(&argv(&["--json", "--json"])).is_err());
        assert!(parse_args(&argv(&["project"])).is_err());
    }

    async fn dispatch_ok(atelier: &Atelier, tool: &str, args: Value) {
        let result = atelier.dispatch(tool, args, "test").await.unwrap();
        assert!(
            !server::is_error_result(&result),
            "{tool}: {:?}",
            server::result_json(&result)
        );
    }

    #[tokio::test]
    async fn healthy_project_replays_and_validates_its_export() {
        let root = Scratch::new().unwrap();
        let store = root.path().join(".atelier");
        let docs = store.join("documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            store.join("project.toml"),
            "version=1\n\
             [[exports]]\n\
             name='hero-walk'\n\
             doc='hero'\n\
             op='anim'\n\
             out='dist/hero.gif'\n\
             tag='walk'\n",
        )
        .unwrap();
        let atelier = Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(docs))));
        dispatch_ok(
            &atelier,
            "doc_create",
            json!({"name":"hero","width":4,"height":4}),
        )
        .await;
        dispatch_ok(
            &atelier,
            "doc_draw",
            json!({
                "doc_id":"hero","layer":0,"frame":0,"op":"fill_cel",
                "color":[10,20,30,255]
            }),
        )
        .await;
        dispatch_ok(
            &atelier,
            "doc_add_tag",
            json!({"doc_id":"hero","name":"walk","from":0,"to":0}),
        )
        .await;

        let report = inspect(root.path(), &store).await;
        assert!(!report.failed(), "{:#?}", report.findings);
        assert!(report.findings.iter().any(|finding| {
            finding.kind == "recipe" && finding.name == "hero" && finding.status == Status::Pass
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.kind == "export"
                && finding.name == "hero-walk"
                && finding.status == Status::Pass
        }));
        assert_eq!(report.json()["version"], 1);
        assert_eq!(report.json()["ok"], true);
    }

    #[tokio::test]
    async fn detects_a_missing_reference_and_recipe_drift() {
        let root = Scratch::new().unwrap();
        let store = root.path().join(".atelier");
        let docs = store.join("documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            store.join("project.toml"),
            "version=1\n\
             [[exports]]\n\
             name='missing'\n\
             doc='ghost'\n\
             op='sheet'\n\
             out='dist/ghost.png'\n",
        )
        .unwrap();
        let studio = Arc::new(Mutex::new(Studio::with_docs_dir(docs)));
        let atelier = Atelier::with_studio(Arc::clone(&studio));
        dispatch_ok(
            &atelier,
            "doc_create",
            json!({"name":"hero","width":4,"height":4}),
        )
        .await;
        // A direct Studio mutation deliberately bypasses dispatch/journaling.
        studio
            .lock()
            .unwrap()
            .doc_draw(
                "hero",
                0,
                0,
                "fill_cel",
                json!({"color":[255,0,0,255]}).as_object().unwrap().clone(),
            )
            .unwrap();

        let report = inspect(root.path(), &store).await;
        assert!(report.failed());
        assert!(report.findings.iter().any(|finding| {
            finding.kind == "reference"
                && finding.name == "missing"
                && finding.status == Status::Fail
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.kind == "recipe"
                && finding.name == "hero"
                && finding.status == Status::Fail
                && finding.detail.contains("differs from live state")
        }));
    }

    #[tokio::test]
    async fn check_rejects_a_recoverable_but_incomplete_final_append() {
        use std::io::Write as _;

        let root = Scratch::new().unwrap();
        let store = root.path().join(".atelier");
        let docs = store.join("documents");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(store.join("project.toml"), "version=1\n").unwrap();
        let atelier =
            Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(docs.clone()))));
        dispatch_ok(
            &atelier,
            "doc_create",
            json!({"name":"hero","width":4,"height":4}),
        )
        .await;
        std::fs::OpenOptions::new()
            .append(true)
            .open(docs.join("hero").join(JOURNAL_FILE))
            .unwrap()
            .write_all(b"{\"call\":")
            .unwrap();

        let report = inspect(root.path(), &store).await;
        assert!(report.findings.iter().any(|finding| {
            finding.kind == "recipe"
                && finding.name == "hero"
                && finding.status == Status::Fail
                && finding.detail.contains("partial final append")
        }));
    }
}
