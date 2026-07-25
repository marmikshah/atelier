//! The store itself: where documents live on disk, the `ATELIER_HOME` policy,
//! and the per-document journal (`recipe.jsonl`) that makes every document a
//! replayable recipe.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use atelier_core::document::{DocMeta, Document};

use super::{DocumentId, JOURNAL_FILE, MAX_CANVAS, Studio, ToolName};

/// The one current journal-line shape, shared by the writer, store reader, and
/// replay parser.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub tool: ToolName,
    pub args: Map<String, Value>,
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

/// An advisory cross-process lock for one document store.
pub struct StoreLock {
    file: fs::File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

impl Studio {
    /// The store resolution policy, pure so it is testable without touching
    /// process env or cwd:
    ///
    /// 1. `ATELIER_HOME` wins — scripts, tests and sandboxes name their store.
    /// 2. A `./.atelier` in the working directory marks a local store. Recipes
    ///    and documents can then be committed beside the project. Opt in with
    ///    `atelier init` — an absent `.atelier` is never created implicitly.
    /// 3. Otherwise use the global home store.
    pub fn resolve_home(
        env: Option<&std::ffi::OsStr>,
        cwd: &std::path::Path,
        global_home: Option<PathBuf>,
    ) -> PathBuf {
        if let Some(dir) = env {
            return PathBuf::from(dir);
        }
        let local = cwd.join(".atelier");
        if local.is_dir() {
            return local;
        }
        global_home.unwrap_or_else(|| std::env::temp_dir().join("atelier"))
    }

    /// The default atelier home: the policy resolved at the process's env and
    /// cwd (see [`Self::resolve_home`]). The one implementation of the policy — the
    /// binary's service manager delegates here instead of keeping a parallel
    /// copy that could drift.
    pub fn default_home() -> PathBuf {
        Self::resolve_home(
            std::env::var_os("ATELIER_HOME").as_deref(),
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            dirs::home_dir().map(|home| home.join(".atelier")),
        )
    }

    /// The global tier only: `ATELIER_HOME`, else `~/.atelier`. The background
    /// daemon pins THIS at install time — a shared server has no stable current
    /// directory. A user who wants a different daemon store says so with
    /// `--home`.
    pub fn global_home() -> PathBuf {
        std::env::var("ATELIER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                // No resolvable home = a deliberate, visible choice of the temp
                // dir — not a silent relative "./.atelier" wherever the process
                // happens to run.
                dirs::home_dir()
                    .map(|h| h.join(".atelier"))
                    .unwrap_or_else(|| std::env::temp_dir().join("atelier"))
            })
    }

    #[allow(clippy::new_without_default)]
    pub fn new() -> Studio {
        let docs_dir = Self::default_home().join("documents");
        let _ = fs::create_dir_all(&docs_dir);
        Studio { docs_dir }
    }

    /// Build a studio rooted at an explicit documents directory, bypassing the
    /// process-global `ATELIER_HOME` env var. Lets an embedder (or a test) point
    /// a studio at an arbitrary location without mutating process state.
    pub fn with_docs_dir(docs_dir: PathBuf) -> Studio {
        let _ = fs::create_dir_all(&docs_dir);
        Studio { docs_dir }
    }

    fn lock_store(&self, exclusive: bool) -> Result<StoreLock, String> {
        let path = self.docs_dir.join(".store.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| format!("cannot open store lock {}: {error}", path.display()))?;
        if exclusive {
            fs4::FileExt::lock(&file)
        } else {
            fs4::FileExt::lock_shared(&file)
        }
        .map_err(|error| format!("cannot lock document store: {error}"))?;
        Ok(StoreLock { file })
    }

    /// Hold a shared store lock until the returned guard is dropped.
    pub fn lock_store_shared(&self) -> Result<StoreLock, String> {
        self.lock_store(false)
    }

    /// Hold an exclusive store lock until the returned guard is dropped.
    pub fn lock_store_exclusive(&self) -> Result<StoreLock, String> {
        self.lock_store(true)
    }

    pub(crate) fn doc_dir(&self, id: &str) -> PathBuf {
        self.docs_dir.join(id)
    }

    /// Only canonical UUIDv4 document ids reach the filesystem. IDs arrive
    /// untrusted over MCP, and a value like `../x` would otherwise escape the
    /// store.
    pub(crate) fn valid_id(id: &str) -> bool {
        DocumentId::is_valid(id)
    }

    pub(crate) fn exists(&self, id: &str) -> bool {
        Self::valid_id(id) && self.doc_dir(id).join("doc.json").exists()
    }

    /// All document ids on disk (directories with a doc.json), sorted.
    pub(crate) fn doc_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.docs_dir) {
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().to_string();
                if Self::valid_id(&id) && e.path().join("doc.json").exists() {
                    out.push(id);
                }
            }
        }
        out.sort();
        out
    }

    pub(crate) fn open(&self, id: &str) -> Result<(PathBuf, Document), String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{}'", id));
        }
        let dir = self.doc_dir(id);
        if !dir.join("doc.json").exists() {
            let existing = self.doc_ids().join(", ");
            return Err(format!(
                "no document '{}'. existing: {}",
                id,
                if existing.is_empty() {
                    "(none)".into()
                } else {
                    existing
                }
            ));
        }
        let doc = Document::load(&dir)?;
        Ok((dir, doc))
    }

    // -- library ------------------------------------------------------------

    pub fn doc_new(&self, name: &str, w: u32, h: u32) -> Result<Value, String> {
        if w == 0 || h == 0 || w > MAX_CANVAS || h > MAX_CANVAS {
            return Err(format!(
                "canvas {w}x{h} out of range — width/height must be 1..={MAX_CANVAS}"
            ));
        }
        // Directory creation is the atomic uniqueness claim. The dispatch
        // layer already serializes normal callers, but Studio is also a public
        // embedding API and two independent processes must not share an id
        // even if they race between generation and creation.
        let (id, dir) = {
            let mut created = None;
            for _ in 0..32 {
                let id = DocumentId::new_v4();
                let dir = self.doc_dir(id.as_str());
                match fs::create_dir(&dir) {
                    Ok(()) => {
                        created = Some((id, dir));
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error.to_string()),
                }
            }
            created.ok_or("could not generate a unique document id after 32 attempts")?
        };
        let mut doc = Document::new(name, w, h);
        if let Err(error) = doc.save(&dir) {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
        let mut out = doc.structure();
        out["doc_id"] = json!(id);
        Ok(out)
    }

    pub fn doc_info(&self, id: &str) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let mut out = doc.structure();
        out["doc_id"] = json!(id);
        Ok(out)
    }

    pub fn list_docs(&self) -> Value {
        self.list_docs_filtered(None, None)
    }

    /// `prefix` keeps opaque ids starting with it; `contains` searches either
    /// the id or the display name. Both are case-sensitive; combined = AND.
    pub fn list_docs_filtered(&self, prefix: Option<&str>, contains: Option<&str>) -> Value {
        let mut items = Vec::new();
        for id in self.doc_ids() {
            if let Some(p) = prefix
                && !id.starts_with(p)
            {
                continue;
            }
            // Read doc.json directly (don't load cel images just to list).
            let meta = fs::read_to_string(self.doc_dir(&id).join("doc.json"))
                .map_err(|error| error.to_string())
                .and_then(|source| {
                    serde_json::from_str::<DocMeta>(&source).map_err(|error| error.to_string())
                })
                .and_then(|meta| {
                    meta.validate()?;
                    Ok(meta)
                });
            if let Some(c) = contains
                && !id.contains(c)
                && !meta
                    .as_ref()
                    .is_ok_and(|document| document.name.contains(c))
            {
                continue;
            }
            let item = match meta {
                Ok(meta) => json!({
                    "doc_id": id,
                    "name": meta.name,
                    "w": meta.w,
                    "h": meta.h,
                    "frames": meta.frames.len(),
                    "layers": meta.layers.len(),
                }),
                Err(error) => json!({
                    "doc_id": id,
                    "error": format!("invalid doc.json: {error}"),
                }),
            };
            items.push(item);
        }
        json!({"count": items.len(), "documents": items})
    }

    pub fn delete_doc(&self, id: &str) -> Result<Value, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{}'", id));
        }
        if !self.exists(id) {
            return Err(format!("no document '{}'", id));
        }
        fs::remove_dir_all(self.doc_dir(id)).map_err(|e| e.to_string())?;
        Ok(json!({"deleted": id}))
    }

    // -- the document journal ----------------------------------------------

    /// Path of a document's journal: the ordered calls that built it.
    fn journal_path(&self, id: &str) -> PathBuf {
        self.docs_dir.join(id).join(JOURNAL_FILE)
    }

    /// Append one call to `id`'s journal.
    ///
    /// The journal is what makes "every document is a replayable recipe" true
    /// rather than aspirational: it lives beside the art it produced, so a
    /// document carries its own provenance and nothing has to be turned on
    /// beforehand to get it.
    ///
    /// JSON Lines, appended: one call per line, O(1) per write, and a killed
    /// process still leaves every completed line intact. Best-effort by design —
    /// a journal that cannot be written must never fail the drawing call that
    /// was otherwise fine.
    pub fn journal_append(&self, id: &str, tool: ToolName, args: &Value) {
        // Defence in depth: `id` is joined onto the store path, so validate it
        // here too rather than trust every caller forever — a bad id must never
        // write recipe.jsonl outside the store (the repo has had a traversal bug
        // before). `.is_dir()` alone would follow `../` through a real dir.
        if !Self::valid_id(id) {
            return;
        }
        let dir = self.docs_dir.join(id);
        if !dir.is_dir() {
            return; // no document, nothing to journal (e.g. a failed create)
        }
        let Some(args) = args.as_object() else {
            eprintln!("atelier: refusing to journal non-object args for {tool}");
            return;
        };
        let entry = JournalEntry {
            tool,
            args: args.clone(),
        };
        let Ok(mut line) = serde_json::to_string(&entry) else {
            return;
        };
        line.push('\n');
        let appended = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path(id))
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(e) = appended {
            eprintln!("atelier: could not journal {tool} for '{id}': {e}");
        }
    }

    /// Read a document's journal back as its ordered calls.
    ///
    /// Same policy as the replay-side parser (`Recipe::parse_jsonl`): a torn
    /// FINAL line is a crash mid-append and is dropped, but a malformed line
    /// with content after it is real corruption and errors — silently skipping
    /// it would report "N steps / replayable" for a journal that `atelier
    /// replay` then refuses.
    pub fn journal(&self, id: &str) -> Result<Vec<JournalEntry>, String> {
        if !self.exists(id) {
            return Err(format!("no document '{id}'"));
        }
        let path = self.journal_path(id);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let body = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let nonempty: Vec<(usize, &str)> = body
            .lines()
            .enumerate()
            .map(|(n, l)| (n, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        let last = nonempty.len().saturating_sub(1);
        let mut out = Vec::new();
        for (idx, (n, line)) in nonempty.iter().enumerate() {
            match serde_json::from_str::<JournalEntry>(line) {
                Ok(entry) => out.push(entry),
                Err(error) if idx == last && error.is_eof() => break,
                Err(e) => return Err(format!("journal line {}: {e}", n + 1)),
            }
        }
        validate_journal(&out).map_err(|error| format!("journal: {error}"))?;
        if let Some(recorded_id) = out
            .first()
            .and_then(|entry| entry.args.get("doc_id"))
            .and_then(Value::as_str)
            && recorded_id != id
        {
            return Err(format!(
                "journal: doc_new stamp '{recorded_id}' does not match document '{id}'"
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-test-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn a_document_journals_the_calls_that_built_it() {
        let s = studio("journal");
        let created = s.doc_new("d", 8, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        assert!(s.journal(id).unwrap().is_empty(), "nothing recorded yet");

        s.journal_append(id, ToolName::DocNew, &json!({"name": "d", "doc_id": id}));
        s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}));
        let steps = s.journal(id).unwrap();
        assert_eq!(steps.len(), 2, "appends accumulate in order");
        assert_eq!(steps[0].tool, ToolName::DocNew);
        assert_eq!(steps[1].args["op"], "rect");

        // Journaling an unknown document is a no-op, never a panic or a stray
        // directory: a failed create must not leave a journal behind.
        s.journal_append("nope", ToolName::DocDraw, &json!({}));
        assert!(s.journal("nope").is_err(), "no document, no journal");
    }

    #[test]
    fn journal_read_policy_matches_the_replay_parser() {
        // Torn FINAL line = crash mid-append, tolerated. Mid-file corruption =
        // error — silently skipping it would list steps that replay refuses.
        let s = studio("journal-policy");
        let created = s.doc_new("d", 8, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.journal_append(id, ToolName::DocNew, &json!({"name": "d", "doc_id": id}));
        s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}));
        let path = s.journal_path(id);

        let clean = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{clean}{{\"tool\":\"doc_")).unwrap();
        assert_eq!(s.journal(id).unwrap().len(), 2, "torn final line dropped");

        fs::write(&path, format!("not json\n{clean}")).unwrap();
        let err = s.journal(id).unwrap_err();
        assert!(err.contains("line 1"), "mid-file corruption errors: {err}");

        fs::write(&path, format!("{clean}not json\n")).unwrap();
        let err = s.journal(id).unwrap_err();
        assert!(
            err.contains("line 3"),
            "complete final corruption errors: {err}"
        );

        fs::write(
            &path,
            "{\"tool\":\"doc_new\",\"args\":[],\"note\":\"old\"}\n",
        )
        .unwrap();
        let err = s.journal(id).unwrap_err();
        assert!(
            err.contains("line 1"),
            "non-current entry shape errors: {err}"
        );

        fs::write(
            &path,
            "{\"tool\":\"doc_new\",\"args\":{\"name\":\"d\",\"doc_id\":\"123e4567-e89b-42d3-a456-426614174000\"}}\n",
        )
        .unwrap();
        let err = s.journal(id).unwrap_err();
        assert!(err.contains("does not match document"), "got: {err}");
    }

    #[test]
    fn journal_contract_rejects_context_and_cross_document_steps() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let other = "123e4567-e89b-42d3-a456-426614174000";
        let create = JournalEntry {
            tool: ToolName::DocNew,
            args: serde_json::from_value(json!({"name": "d", "doc_id": id})).unwrap(),
        };
        for entry in [
            JournalEntry {
                tool: ToolName::DocCheckpoint,
                args: serde_json::from_value(json!({"doc_id": id, "action": "save"})).unwrap(),
            },
            JournalEntry {
                tool: ToolName::DocDraw,
                args: serde_json::from_value(json!({"doc_id": other, "op": "clear_cel"})).unwrap(),
            },
            JournalEntry {
                tool: ToolName::DocDraw,
                args: serde_json::from_value(json!({"op": "clear_cel"})).unwrap(),
            },
        ] {
            assert!(validate_journal(&[create.clone(), entry]).is_err());
        }
        let palette = JournalEntry {
            tool: ToolName::DocPalette,
            args: serde_json::from_value(json!({
                "op": "generate",
                "base": [1, 2, 3],
                "set_doc": id
            }))
            .unwrap(),
        };
        validate_journal(&[create, palette]).unwrap();
    }

    #[test]
    fn store_locks_coordinate_independent_studios() {
        let first = studio("store-lock");
        let second = Studio::with_docs_dir(first.docs_dir.clone());
        let guard = first.lock_store_exclusive().unwrap();
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(second.docs_dir.join(".store.lock"))
            .unwrap();
        assert!(matches!(
            fs4::FileExt::try_lock_shared(&contender),
            Err(fs4::TryLockError::WouldBlock)
        ));
        drop(guard);
        fs4::FileExt::try_lock_shared(&contender).unwrap();
        fs4::FileExt::unlock(&contender).unwrap();
    }

    #[test]
    fn list_docs_filters_by_prefix_and_substring() {
        let s = studio("filters");
        let mut ids = Vec::new();
        for name in ["hero-idle", "hero-run", "tile-grass"] {
            ids.push(
                s.doc_new(name, 4, 4).unwrap()["doc_id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        assert_eq!(s.list_docs_filtered(None, None)["count"], 3);
        assert_eq!(s.list_docs_filtered(Some(&ids[0]), None)["count"], 1);
        assert_eq!(
            s.list_docs_filtered(None, Some("hero"))["count"],
            2,
            "contains also searches display names"
        );
        assert_eq!(
            s.list_docs_filtered(None, Some(&ids[1][ids[1].len() - 6..]))["count"],
            1
        );
        assert_eq!(
            s.list_docs_filtered(Some(&ids[2]), Some(&ids[2][3..]))["count"],
            1
        );
    }

    #[test]
    fn document_ids_are_opaque_and_names_may_repeat() {
        let s = studio("opaque-ids");
        let first = s.doc_new("same name", 4, 4).unwrap();
        let second = s.doc_new("same name", 4, 4).unwrap();
        let first_id = first["doc_id"].as_str().unwrap();
        let second_id = second["doc_id"].as_str().unwrap();

        assert!(Studio::valid_id(first_id), "unexpected id: {first_id}");
        assert!(Studio::valid_id(second_id), "unexpected id: {second_id}");
        assert_ne!(first_id, second_id);
        assert_eq!(s.list_docs_filtered(None, Some("same name"))["count"], 2);
        assert!(s.doc_info("same-name").is_err(), "names are never ids");
    }

    /// A scratch cwd with (or without) a `.atelier` for the resolver tests.
    fn scratch_cwd(tag: &str, with_local: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atelier-test-resolve-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        if with_local {
            fs::create_dir_all(dir.join(".atelier")).unwrap();
        } else {
            fs::create_dir_all(&dir).unwrap();
        }
        dir
    }

    #[test]
    fn env_wins_over_a_local_store_and_the_global() {
        let cwd = scratch_cwd("env", true);
        let path = Studio::resolve_home(
            Some(std::ffi::OsStr::new("/custom")),
            &cwd,
            Some(PathBuf::from("/home/u/.atelier")),
        );
        assert_eq!(path, PathBuf::from("/custom"));
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_present_dot_atelier_marks_a_local_store() {
        let cwd = scratch_cwd("project", true);
        let path = Studio::resolve_home(None, &cwd, Some(PathBuf::from("/home/u/.atelier")));
        assert_eq!(path, cwd.join(".atelier"));
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn an_absent_dot_atelier_falls_back_to_global_and_creates_nothing() {
        let cwd = scratch_cwd("global", false);
        let path = Studio::resolve_home(None, &cwd, Some(PathBuf::from("/home/u/.atelier")));
        assert_eq!(path, PathBuf::from("/home/u/.atelier"));
        assert!(
            !cwd.join(".atelier").exists(),
            "resolution must never stamp a local store implicitly"
        );
        let _ = fs::remove_dir_all(&cwd);
    }
}
