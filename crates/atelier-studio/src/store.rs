//! The store itself: where documents live on disk, the `ATELIER_HOME` policy,
//! and the per-document journal (`recipe.jsonl`) that makes every document a
//! replayable recipe.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use atelier_core::document::Document;

use super::{JOURNAL_FILE, MAX_CANVAS, Studio, slugify};

/// Why a store root was picked: an explicit override, a project-local
/// `./.atelier`, or the global home store. doctor surfaces it so "which
/// store am I on?" is always answerable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HomeOrigin {
    /// `ATELIER_HOME` (or an explicit `--home`) named the store.
    Env,
    /// A `./.atelier` directory exists in the working directory.
    Project,
    /// The fallback: `~/.atelier` (or the temp dir when no home resolves).
    Global,
}

/// An advisory cross-process lock for one Atelier document store.
///
/// The lock file is infrastructure only; dropping this guard releases the OS
/// lock. Writers take an exclusive guard across dispatch plus journaling, while
/// snapshot-style readers such as `atelier check` hold a shared guard.
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
    /// 2. A `./.atelier` in the working directory marks a PROJECT store: the
    ///    art belongs to whatever lives there (a game repo), so ids mint clean
    ///    (`hero`, never `hero-2` from some other project's hero) and recipes
    ///    can be committed next to the game. Opt in once per project with
    ///    `atelier init` — an absent `.atelier` is never created implicitly.
    /// 3. Otherwise the global home store. Standing in `$HOME` that IS
    ///    `~/.atelier`, so the global store is just "the project store of the
    ///    home directory" — one mental model, not two.
    pub fn resolve_home(
        env: Option<&std::ffi::OsStr>,
        cwd: &std::path::Path,
        home: Option<PathBuf>,
    ) -> (PathBuf, HomeOrigin) {
        if let Some(dir) = env {
            return (PathBuf::from(dir), HomeOrigin::Env);
        }
        let local = cwd.join(".atelier");
        if local.is_dir() {
            return (local, HomeOrigin::Project);
        }
        (
            home.unwrap_or_else(|| std::env::temp_dir().join("atelier")),
            HomeOrigin::Global,
        )
    }

    /// The default atelier home: the policy resolved at the process's env and
    /// cwd (see [`Self::resolve_home`]). The one implementation of the policy — the
    /// binary's service manager delegates here instead of keeping a parallel
    /// copy that could drift.
    pub fn default_home() -> PathBuf {
        Self::default_home_with_origin().0
    }

    /// [`Self::default_home`] plus why — for doctor, which displays the choice.
    pub fn default_home_with_origin() -> (PathBuf, HomeOrigin) {
        Self::resolve_home(
            std::env::var_os("ATELIER_HOME").as_deref(),
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            dirs::home_dir(),
        )
    }

    /// The global tier only: `ATELIER_HOME`, else `~/.atelier`. The background
    /// daemon pins THIS at install time — a shared server has no "current
    /// directory", so a project store must never become its default; a user
    /// who wants the daemon on a project store says so with `--home`.
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
        Studio {
            docs_dir,
            clipboard: None,
            selection: None,
        }
    }

    /// Build a studio rooted at an explicit documents directory, bypassing the
    /// process-global `ATELIER_HOME` env var. Lets an embedder (or a test) point
    /// a studio at an arbitrary location without mutating process state.
    pub fn with_docs_dir(docs_dir: PathBuf) -> Studio {
        let _ = fs::create_dir_all(&docs_dir);
        Studio {
            docs_dir,
            clipboard: None,
            selection: None,
        }
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
        let locked = if exclusive {
            fs4::FileExt::lock(&file)
        } else {
            fs4::FileExt::lock_shared(&file)
        };
        locked.map_err(|error| format!("cannot lock document store: {error}"))?;
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

    /// Stored ids are always slugs (`doc_create` slugifies the name). Reject
    /// anything else before it reaches the filesystem — ids arrive untrusted
    /// over MCP, and an id like `../x` would otherwise escape the store.
    pub(crate) fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    pub(crate) fn exists(&self, id: &str) -> bool {
        self.doc_dir(id).join("doc.json").exists()
    }

    /// All document ids on disk (directories with a doc.json), sorted.
    pub(crate) fn doc_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.docs_dir) {
            for e in rd.flatten() {
                if e.path().join("doc.json").exists() {
                    out.push(e.file_name().to_string_lossy().to_string());
                }
            }
        }
        out.sort();
        out
    }

    fn unique_id(&self, base: &str) -> String {
        let base = slugify(base);
        if !self.exists(&base) {
            return base;
        }
        let mut i = 2;
        loop {
            let cand = format!("{}-{}", base, i);
            if !self.exists(&cand) {
                return cand;
            }
            i += 1;
        }
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

    pub fn doc_create(&self, name: &str, w: u32, h: u32) -> Result<Value, String> {
        if w == 0 || h == 0 || w > MAX_CANVAS || h > MAX_CANVAS {
            return Err(format!(
                "canvas {w}x{h} out of range — width/height must be 1..={MAX_CANVAS}"
            ));
        }
        let id = self.unique_id(name);
        let dir = self.doc_dir(&id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut doc = Document::new(name, w, h);
        doc.save(&dir)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn doc_info(&self, id: &str) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    /// Just the frame count, read off doc.json without decoding any cels — the
    /// MCP multi-frame `doc_batch` preflights its target frames against it
    /// before applying anything.
    pub fn frame_count(&self, id: &str) -> Result<usize, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{id}'"));
        }
        if !self.exists(id) {
            return Err(format!("no document '{id}'"));
        }
        let s = fs::read_to_string(self.doc_dir(id).join("doc.json")).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
        v["frames"]
            .as_array()
            .map(|a| a.len())
            .ok_or_else(|| format!("document '{id}': doc.json has no frames array"))
    }

    pub fn list_docs(&self) -> Value {
        self.list_docs_filtered(None, None)
    }

    /// `prefix` keeps ids starting with it (family selector: `hero-` matches
    /// `hero-idle`, `hero-run`); `contains` keeps ids with the substring. Both
    /// case-sensitive on the slug; combined = AND.
    pub fn list_docs_filtered(&self, prefix: Option<&str>, contains: Option<&str>) -> Value {
        let mut items = Vec::new();
        for id in self.doc_ids() {
            if let Some(p) = prefix
                && !id.starts_with(p)
            {
                continue;
            }
            if let Some(c) = contains
                && !id.contains(c)
            {
                continue;
            }
            // Read doc.json directly (don't load cel images just to list).
            let meta = fs::read_to_string(self.doc_dir(&id).join("doc.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let (name, w, h, frames, layers) = match &meta {
                Some(m) => (
                    m["name"].clone(),
                    m["w"].clone(),
                    m["h"].clone(),
                    m["frames"].as_array().map(|a| a.len()).unwrap_or(0),
                    m["layers"].as_array().map(|a| a.len()).unwrap_or(0),
                ),
                None => (json!(id), json!(null), json!(null), 0, 0),
            };
            items.push(
                json!({"id": id, "name": name, "w": w, "h": h, "frames": frames, "layers": layers}),
            );
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
    /// Legacy `{tool,args}` JSON Lines, appended one call per line. Kept as the
    /// compatibility writer for documents whose journal began before compact
    /// JSONL v2; new dispatch-created journals use
    /// [`Self::journal_append_compact`]. Best-effort by design — a journal that
    /// cannot be written must never fail the drawing call that was otherwise
    /// fine.
    pub fn journal_append(&self, id: &str, tool: &str, args: &Value) {
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
        let Ok(mut line) = serde_json::to_string(&json!({"tool": tool, "args": args})) else {
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

    /// Append a compact JSONL v2 call, while preserving an existing legacy
    /// journal's format. New journals receive a header and call in one append;
    /// existing v2 journals receive only the call; existing `{tool,args}`
    /// journals continue through [`Self::journal_append`] so a document is
    /// never left with mixed, unreplayable line formats.
    pub fn journal_append_compact(&self, id: &str, tool: &str, args: &Value) {
        if !Self::valid_id(id) {
            return;
        }
        let dir = self.docs_dir.join(id);
        if !dir.is_dir() {
            return;
        }
        let (header, line) = match super::recipe::compact_journal_record(id, tool, args.clone()) {
            Ok(encoded) => encoded,
            Err(_) => {
                self.journal_append(id, tool, args);
                return;
            }
        };
        let path = self.journal_path(id);
        enum Format {
            New,
            Legacy,
            Compact,
        }
        let format = match fs::metadata(&path) {
            Ok(metadata) if metadata.len() == 0 => Format::New,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Format::New,
            Ok(_) => {
                use std::io::BufRead;
                let first =
                    fs::File::open(&path)
                        .map(std::io::BufReader::new)
                        .and_then(|mut reader| {
                            let mut line = String::new();
                            loop {
                                line.clear();
                                if reader.read_line(&mut line)? == 0 || !line.trim().is_empty() {
                                    return Ok(line);
                                }
                            }
                        });
                match first.ok() {
                    Some(first) if super::recipe::valid_compact_header(first.trim()) => {
                        Format::Compact
                    }
                    Some(first)
                        if serde_json::from_str::<Value>(first.trim())
                            .ok()
                            .is_some_and(|value| {
                                value.get("tool").and_then(Value::as_str).is_some()
                            }) =>
                    {
                        Format::Legacy
                    }
                    _ => {
                        eprintln!(
                            "atelier: could not journal {tool} for '{id}': \
                             existing recipe has an unknown or corrupt header"
                        );
                        return;
                    }
                }
            }
            Err(error) => {
                eprintln!("atelier: could not inspect journal for '{id}': {error}");
                return;
            }
        };
        if matches!(format, Format::Legacy) {
            self.journal_append(id, tool, args);
            return;
        }
        if header.contains('\n') || line.contains('\n') {
            eprintln!(
                "atelier: could not journal {tool} for '{id}': encoded line contains newline"
            );
            return;
        }
        let payload = match format {
            Format::New => format!("{header}\n{line}\n"),
            Format::Compact => format!("{line}\n"),
            Format::Legacy => unreachable!("legacy returned above"),
        };
        let appended = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, payload.as_bytes()));
        if let Err(error) = appended {
            eprintln!("atelier: could not journal {tool} for '{id}': {error}");
        }
    }

    /// Read a document's journal back as its ordered calls.
    ///
    /// Uses the shared recipe parser, so legacy and compact files both return
    /// normalized `{tool,args,note?}` calls and corruption has exactly the same
    /// verdict here as it does under `atelier replay`.
    pub fn journal(&self, id: &str) -> Result<Vec<Value>, String> {
        if !self.exists(id) {
            return Err(format!("no document '{id}'"));
        }
        let path = self.journal_path(id);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let body = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        let recipe = super::recipe::Recipe::parse(&body)?;
        recipe
            .steps
            .into_iter()
            .map(|step| serde_json::to_value(step).map_err(|error| error.to_string()))
            .collect()
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
        s.doc_create("d", 8, 8).unwrap();
        assert!(s.journal("d").unwrap().is_empty(), "nothing recorded yet");

        s.journal_append("d", "doc_create", &json!({"name": "d"}));
        s.journal_append("d", "doc_draw", &json!({"op": "rect"}));
        let steps = s.journal("d").unwrap();
        assert_eq!(steps.len(), 2, "appends accumulate in order");
        assert_eq!(steps[0]["tool"], "doc_create");
        assert_eq!(steps[1]["args"]["op"], "rect");

        // Journaling an unknown document is a no-op, never a panic or a stray
        // directory: a failed create must not leave a journal behind.
        s.journal_append("nope", "doc_draw", &json!({}));
        assert!(s.journal("nope").is_err(), "no document, no journal");
    }

    #[test]
    fn journal_read_policy_matches_the_replay_parser() {
        // Torn FINAL line = crash mid-append, tolerated. Mid-file corruption =
        // error — silently skipping it would list steps that replay refuses.
        let s = studio("journal-policy");
        s.doc_create("d", 8, 8).unwrap();
        s.journal_append("d", "doc_create", &json!({"name": "d"}));
        s.journal_append("d", "doc_draw", &json!({"op": "rect"}));
        let path = s.journal_path("d");

        let clean = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{clean}{{\"tool\":\"doc_")).unwrap();
        assert_eq!(s.journal("d").unwrap().len(), 2, "torn final line dropped");

        fs::write(&path, format!("not json\n{clean}")).unwrap();
        let err = s.journal("d").unwrap_err();
        assert!(err.contains("line 1"), "mid-file corruption errors: {err}");
    }

    #[test]
    fn compact_journal_headers_are_not_counted_as_steps() {
        let s = studio("journal-compact");
        s.doc_create("d", 8, 8).unwrap();
        s.journal_append_compact("d", "doc_create", &json!({"name":"d","doc_id":"d"}));
        s.journal_append_compact("d", "doc_info", &json!({"doc_id":"d"}));
        let steps = s.journal("d").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["tool"], "doc_create");
        assert_eq!(steps[0]["args"]["doc_id"], "d");
        assert_eq!(steps[1]["tool"], "doc_info");
        assert_eq!(steps[1]["args"]["doc_id"], "d");
        let source = fs::read_to_string(s.journal_path("d")).unwrap();
        assert_eq!(source.lines().count(), 3, "one header plus two calls");
    }

    #[test]
    fn compact_appends_preserve_an_existing_legacy_journal() {
        let s = studio("journal-legacy-append");
        s.doc_create("d", 8, 8).unwrap();
        s.journal_append("d", "doc_create", &json!({"name":"d"}));
        s.journal_append_compact(
            "d",
            "doc_draw",
            &json!({"doc_id":"d","layer":0,"frame":0,"op":"clear_cel"}),
        );
        let source = fs::read_to_string(s.journal_path("d")).unwrap();
        assert!(
            source.lines().all(|line| line.contains("\"tool\"")),
            "legacy and compact lines must never mix: {source}"
        );
        assert_eq!(s.journal("d").unwrap().len(), 2);
    }

    #[test]
    fn compact_append_rejects_an_incomplete_v2_header() {
        let s = studio("journal-invalid-v2-header");
        s.doc_create("d", 8, 8).unwrap();
        let path = s.journal_path("d");
        fs::write(&path, "{\"v\":2}\n").unwrap();
        s.journal_append_compact(
            "d",
            "doc_draw",
            &json!({"doc_id":"d","layer":0,"frame":0,"op":"clear_cel"}),
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "{\"v\":2}\n",
            "a corrupt header must never gain dependent compact calls"
        );
    }

    #[test]
    fn store_locks_coordinate_independent_studio_instances() {
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
    fn frame_count_reads_doc_json_directly() {
        let s = studio("fc");
        s.doc_create("d", 4, 4).unwrap();
        assert_eq!(s.frame_count("d").unwrap(), 1);
        s.doc_add_frame("d", 100, None, 4).unwrap();
        assert_eq!(s.frame_count("d").unwrap(), 5);
        assert!(s.frame_count("ghost").is_err());
        assert!(s.frame_count("../x").is_err());
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
    fn env_wins_over_a_project_store_and_the_global() {
        let cwd = scratch_cwd("env", true);
        let (p, origin) = Studio::resolve_home(
            Some(std::ffi::OsStr::new("/custom")),
            &cwd,
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(p, PathBuf::from("/custom"));
        assert_eq!(origin, HomeOrigin::Env);
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_present_dot_atelier_marks_a_project_store() {
        let cwd = scratch_cwd("project", true);
        let (p, origin) = Studio::resolve_home(None, &cwd, Some(PathBuf::from("/home/u")));
        assert_eq!(p, cwd.join(".atelier"));
        assert_eq!(origin, HomeOrigin::Project);
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn an_absent_dot_atelier_falls_back_to_global_and_creates_nothing() {
        let cwd = scratch_cwd("global", false);
        let (p, origin) = Studio::resolve_home(None, &cwd, Some(PathBuf::from("/home/u")));
        assert_eq!(p, PathBuf::from("/home/u"));
        assert_eq!(origin, HomeOrigin::Global);
        assert!(
            !cwd.join(".atelier").exists(),
            "resolution must never stamp a project store implicitly"
        );
        let _ = fs::remove_dir_all(&cwd);
    }
}
