//! The store itself: where documents live on disk, the `ATELIER_HOME` policy,
//! and the per-document journal (`recipe.jsonl`) that makes every document a
//! replayable recipe.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use atelier_core::document::Document;

use super::{JOURNAL_FILE, MAX_CANVAS, Studio, slugify};

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
    /// 2. A `./.atelier` in the working directory marks a local store: ids mint
    ///    clean for that directory and recipes can be committed beside the
    ///    project. Opt in explicitly with
    ///    `atelier init` — an absent `.atelier` is never created implicitly.
    /// 3. Otherwise use the global home store.
    pub fn resolve_home(
        env: Option<&std::ffi::OsStr>,
        cwd: &std::path::Path,
        home: Option<PathBuf>,
    ) -> PathBuf {
        if let Some(dir) = env {
            return PathBuf::from(dir);
        }
        let local = cwd.join(".atelier");
        if local.is_dir() {
            return local;
        }
        home.unwrap_or_else(|| std::env::temp_dir().join("atelier"))
    }

    /// The default atelier home: the policy resolved at the process's env and
    /// cwd (see [`Self::resolve_home`]). The one implementation of the policy — the
    /// binary's service manager delegates here instead of keeping a parallel
    /// copy that could drift.
    pub fn default_home() -> PathBuf {
        Self::resolve_home(
            std::env::var_os("ATELIER_HOME").as_deref(),
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            dirs::home_dir(),
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
    /// JSON Lines, appended: one call per line, O(1) per write, and a killed
    /// process still leaves every completed line intact. Best-effort by design —
    /// a journal that cannot be written must never fail the drawing call that
    /// was otherwise fine.
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

    /// Read a document's journal back as its ordered calls.
    ///
    /// Same policy as the replay-side parser (`Recipe::parse_jsonl`): a torn
    /// FINAL line is a crash mid-append and is dropped, but a malformed line
    /// with content after it is real corruption and errors — silently skipping
    /// it would report "N steps / replayable" for a journal that `atelier
    /// replay` then refuses.
    pub fn journal(&self, id: &str) -> Result<Vec<Value>, String> {
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
            match serde_json::from_str(line) {
                Ok(v) => out.push(v),
                Err(error) if idx == last && error.is_eof() => break,
                Err(e) => return Err(format!("journal line {}: {e}", n + 1)),
            }
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

        fs::write(&path, format!("{clean}not json\n")).unwrap();
        let err = s.journal("d").unwrap_err();
        assert!(
            err.contains("line 3"),
            "complete final corruption errors: {err}"
        );
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
    fn env_wins_over_a_local_store_and_the_global() {
        let cwd = scratch_cwd("env", true);
        let path = Studio::resolve_home(
            Some(std::ffi::OsStr::new("/custom")),
            &cwd,
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(path, PathBuf::from("/custom"));
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_present_dot_atelier_marks_a_local_store() {
        let cwd = scratch_cwd("project", true);
        let path = Studio::resolve_home(None, &cwd, Some(PathBuf::from("/home/u")));
        assert_eq!(path, cwd.join(".atelier"));
        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn an_absent_dot_atelier_falls_back_to_global_and_creates_nothing() {
        let cwd = scratch_cwd("global", false);
        let path = Studio::resolve_home(None, &cwd, Some(PathBuf::from("/home/u")));
        assert_eq!(path, PathBuf::from("/home/u"));
        assert!(
            !cwd.join(".atelier").exists(),
            "resolution must never stamp a local store implicitly"
        );
        let _ = fs::remove_dir_all(&cwd);
    }
}
