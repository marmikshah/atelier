//! The store itself: where documents live on disk, the `ATELIER_HOME` policy,
//! and the per-document journal (`recipe.jsonl`) that makes every document a
//! replayable recipe.

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use atelier_core::document::Document;

use super::{DocumentId, JOURNAL_FILE, MAX_CANVAS, Studio, ToolName};

/// Current JSONL journal entry format.
pub const JOURNAL_FORMAT_VERSION: u32 = 1;

const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JOURNAL_ENTRIES: usize = 100_000;

const fn journal_format_v1() -> u32 {
    JOURNAL_FORMAT_VERSION
}

/// The one current journal-line shape, shared by the writer, store reader, and
/// replay parser.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    #[serde(default = "journal_format_v1")]
    pub format_version: u32,
    pub tool: ToolName,
    pub args: Map<String, Value>,
}

impl JournalEntry {
    pub fn new(tool: ToolName, args: Map<String, Value>) -> Self {
        Self {
            format_version: JOURNAL_FORMAT_VERSION,
            tool,
            args,
        }
    }
}

/// Validate the current per-document journal contract. An absent/empty journal
/// means "no recipe"; a non-empty one is a complete, self-identifying rebuild.
pub fn validate_journal(entries: &[JournalEntry]) -> Result<(), String> {
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.format_version != JOURNAL_FORMAT_VERSION)
    {
        return Err(format!(
            "unsupported journal format {} (this build supports {})",
            entry.format_version, JOURNAL_FORMAT_VERSION
        ));
    }
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

pub(crate) fn read_bounded_utf8(
    path: &std::path::Path,
    label: &str,
    max_bytes: u64,
) -> Result<String, String> {
    let path_metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !path_metadata.file_type().is_file() {
        return Err(format!(
            "{label} must be a regular file (symlinks are refused)"
        ));
    }
    let mut options = fs::File::options();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(0o400000);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let file_metadata = file.metadata().map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(format!("{label} changed while it was being opened"));
        }
    }
    let length = file_metadata.len();
    if length > max_bytes {
        return Err(format!(
            "{label} is {length} bytes, over the {max_bytes}-byte verification limit"
        ));
    }
    let capacity = usize::try_from(length.min(max_bytes)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{label} grew beyond the {max_bytes}-byte verification limit while it was read"
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("{label} is not UTF-8: {error}"))
}

pub(crate) struct ParsedJournal {
    pub(crate) entries: Vec<JournalEntry>,
    pub(crate) torn_tail: bool,
}

pub(crate) fn parse_journal_file(
    id: &str,
    path: &std::path::Path,
) -> Result<ParsedJournal, String> {
    let body = read_bounded_utf8(path, JOURNAL_FILE, MAX_JOURNAL_BYTES)?;
    let final_line_was_terminated = body.ends_with('\n');
    let mut lines = body
        .lines()
        .enumerate()
        .map(|(line, value)| (line, value.trim()))
        .filter(|(_, value)| !value.is_empty())
        .peekable();
    let mut entries = Vec::new();
    let mut torn_tail = false;
    while let Some((line, value)) = lines.next() {
        let is_last = lines.peek().is_none();
        match serde_json::from_str::<JournalEntry>(value) {
            Ok(entry) if entries.len() < MAX_JOURNAL_ENTRIES => entries.push(entry),
            Ok(_) => {
                return Err(format!(
                    "journal has more than {MAX_JOURNAL_ENTRIES} entries; split or archive its recipe before verification"
                ));
            }
            Err(error) if is_last && error.is_eof() && !final_line_was_terminated => {
                torn_tail = true;
                break;
            }
            Err(error) => return Err(format!("journal line {}: {error}", line + 1)),
        }
    }
    validate_journal(&entries).map_err(|error| format!("journal: {error}"))?;
    if let Some(recorded_id) = entries
        .first()
        .and_then(|entry| entry.args.get("doc_id"))
        .and_then(Value::as_str)
        && recorded_id != id
    {
        return Err(format!(
            "journal: doc_new stamp '{recorded_id}' does not match document '{id}'"
        ));
    }
    Ok(ParsedJournal { entries, torn_tail })
}

fn read_journal_file(id: &str, path: &std::path::Path) -> Result<Vec<JournalEntry>, String> {
    parse_journal_file(id, path).map(|journal| journal.entries)
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
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".atelier")),
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
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".atelier"))
                    .unwrap_or_else(|| std::env::temp_dir().join("atelier"))
            })
    }

    #[allow(clippy::new_without_default)]
    pub fn new() -> Studio {
        let docs_dir = Self::default_home().join("documents");
        let _ = fs::create_dir_all(&docs_dir);
        Studio { docs_dir }
    }

    /// Build a studio rooted at an explicit Atelier home, using the same
    /// `<home>/documents` layout as [`Self::new`] and `ATELIER_HOME`.
    ///
    /// CLI `--home` flags should use this constructor. Embedders and tests
    /// that already have the documents directory itself can use
    /// [`Self::with_docs_dir`].
    pub fn with_home(home: PathBuf) -> Studio {
        Self::with_docs_dir(home.join("documents"))
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
        if !Self::valid_id(id) {
            return false;
        }
        let dir = self.doc_dir(id);
        fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.file_type().is_dir())
            && fs::symlink_metadata(dir.join("doc.json"))
                .is_ok_and(|metadata| metadata.file_type().is_file())
    }

    /// All document ids on disk (directories with a doc.json), sorted.
    pub(crate) fn doc_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.docs_dir) {
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().to_string();
                if Self::valid_id(&id)
                    && e.file_type().is_ok_and(|kind| kind.is_dir())
                    && fs::symlink_metadata(e.path().join("doc.json"))
                        .is_ok_and(|metadata| metadata.file_type().is_file())
                {
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
        if !self.exists(id) {
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
        self.list_docs_inner(prefix, contains, None, usize::MAX)
    }

    /// Return a bounded page of the filtered library. `cursor` is the last
    /// opaque id returned by the previous page and is exclusive.
    pub fn list_docs_page(
        &self,
        prefix: Option<&str>,
        contains: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Value, String> {
        const MAX_PAGE: usize = 100;
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(format!("list limit must be 1..={MAX_PAGE}, got {limit}"));
        }
        if let Some(cursor) = cursor
            && !Self::valid_id(cursor)
        {
            return Err(format!("invalid list cursor '{cursor}'"));
        }
        Ok(self.list_docs_inner(prefix, contains, cursor, limit))
    }

    fn list_docs_inner(
        &self,
        prefix: Option<&str>,
        contains: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Value {
        let mut items = Vec::new();
        let mut total = 0usize;
        let mut has_more = false;
        for id in self.doc_ids() {
            if let Some(p) = prefix
                && !id.starts_with(p)
            {
                continue;
            }
            // Share the normal open path's bounded, symlink-refusing metadata
            // loader without decoding cel images just to list documents.
            let meta = Document::load_metadata(&self.doc_dir(&id));
            if let Some(c) = contains
                && !id.contains(c)
                && !meta
                    .as_ref()
                    .is_ok_and(|document| document.name.contains(c))
            {
                continue;
            }
            total += 1;
            if cursor.is_some_and(|cursor| id.as_str() <= cursor) {
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
            if items.len() < limit {
                items.push(item);
            } else {
                has_more = true;
            }
        }
        let next_cursor = has_more
            .then(|| {
                items
                    .last()
                    .and_then(|item| item.get("doc_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        json!({
            "count": items.len(),
            "total": total,
            "truncated": has_more,
            "next_cursor": next_cursor,
            "documents": items,
        })
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
    /// JSON Lines, appended: one versioned call per line. Failure is explicit;
    /// the dispatch transaction must not publish pixels whose recipe could not
    /// be persisted.
    pub fn journal_append(&self, id: &str, tool: ToolName, args: &Value) -> Result<(), String> {
        // Defence in depth: `id` is joined onto the store path, so validate it
        // here too rather than trust every caller forever — a bad id must never
        // write recipe.jsonl outside the store (the repo has had a traversal bug
        // before). `.is_dir()` alone would follow `../` through a real dir.
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{id}'"));
        }
        if !self.exists(id) {
            return Err(format!("no document '{id}' to journal"));
        }
        let args = args
            .as_object()
            .ok_or_else(|| format!("refusing to journal non-object args for {tool}"))?;
        let entry = JournalEntry::new(tool, args.clone());
        let path = self.journal_path(id);
        let (mut existing_entries, existing_bytes) = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), 0),
            Err(error) => return Err(format!("cannot inspect journal for '{id}': {error}")),
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(format!(
                    "journal for '{id}' must be a regular file (symlinks are refused)"
                ));
            }
            Ok(metadata) => {
                let parsed = parse_journal_file(id, &path)?;
                if parsed.torn_tail {
                    return Err(format!(
                        "journal for '{id}' has an incomplete final line; run `atelier library verify` and repair it before editing"
                    ));
                }
                (parsed.entries, metadata.len())
            }
        };
        if existing_entries.len() >= MAX_JOURNAL_ENTRIES {
            return Err(format!(
                "journal for '{id}' reached the {MAX_JOURNAL_ENTRIES}-entry safety limit"
            ));
        }
        existing_entries.push(entry.clone());
        validate_journal(&existing_entries)
            .map_err(|error| format!("refusing journal append: {error}"))?;
        if existing_entries
            .first()
            .and_then(|first| first.args.get("doc_id"))
            .and_then(Value::as_str)
            != Some(id)
        {
            return Err(format!(
                "refusing journal append: doc_new stamp does not match document '{id}'"
            ));
        }

        let mut line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        line.push('\n');
        let line_bytes = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if existing_bytes > MAX_JOURNAL_BYTES.saturating_sub(line_bytes) {
            return Err(format!(
                "journal for '{id}' would exceed the {MAX_JOURNAL_BYTES}-byte safety limit"
            ));
        }

        let mut options = fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;

            // Linux O_NOFOLLOW: refuse a final-component symlink atomically.
            options.custom_flags(0o400000);
        }
        let mut file = options
            .open(&path)
            .map_err(|e| format!("could not open journal for '{id}': {e}"))?;
        std::io::Write::write_all(&mut file, line.as_bytes())
            .map_err(|e| format!("could not journal {tool} for '{id}': {e}"))?;
        file.sync_data()
            .map_err(|e| format!("could not sync journal for '{id}': {e}"))
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
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("cannot inspect journal for '{id}': {error}")),
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(format!(
                    "journal for '{id}' must be a regular file (symlinks are refused)"
                ));
            }
            Ok(_) => {}
        }
        read_journal_file(id, &path)
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
        assert!(
            s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}))
                .is_err(),
            "a recipe cannot begin with an edit"
        );

        s.journal_append(id, ToolName::DocNew, &json!({"name": "d", "doc_id": id}))
            .unwrap();
        s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}))
            .unwrap();
        let steps = s.journal(id).unwrap();
        assert_eq!(steps.len(), 2, "appends accumulate in order");
        assert_eq!(steps[0].tool, ToolName::DocNew);
        assert_eq!(steps[1].args["op"], "rect");

        // Journaling an unknown document is a no-op, never a panic or a stray
        // directory: a failed create must not leave a journal behind.
        assert!(
            s.journal_append("nope", ToolName::DocDraw, &json!({}))
                .is_err()
        );
        assert!(s.journal("nope").is_err(), "no document, no journal");
    }

    #[test]
    fn journal_read_policy_matches_the_replay_parser() {
        // Torn FINAL line = crash mid-append, tolerated. Mid-file corruption =
        // error — silently skipping it would list steps that replay refuses.
        let s = studio("journal-policy");
        let created = s.doc_new("d", 8, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.journal_append(id, ToolName::DocNew, &json!({"name": "d", "doc_id": id}))
            .unwrap();
        s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}))
            .unwrap();
        let path = s.journal_path(id);

        let clean = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{clean}{{\"tool\":\"doc_")).unwrap();
        assert_eq!(s.journal(id).unwrap().len(), 2, "torn final line dropped");
        assert!(
            s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}))
                .is_err(),
            "new writes must not cement a torn tail into the journal"
        );

        fs::write(&path, format!("{clean}{{\"tool\":\n")).unwrap();
        let err = s.journal(id).unwrap_err();
        assert!(
            err.contains("line 3"),
            "a newline-terminated incomplete object is corruption: {err}"
        );

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
        let create = JournalEntry::new(
            ToolName::DocNew,
            serde_json::from_value(json!({"name": "d", "doc_id": id})).unwrap(),
        );
        for entry in [
            JournalEntry::new(
                ToolName::DocCheckpoint,
                serde_json::from_value(json!({"doc_id": id, "action": "save"})).unwrap(),
            ),
            JournalEntry::new(
                ToolName::DocDraw,
                serde_json::from_value(json!({"doc_id": other, "op": "clear_cel"})).unwrap(),
            ),
            JournalEntry::new(
                ToolName::DocDraw,
                serde_json::from_value(json!({"op": "clear_cel"})).unwrap(),
            ),
        ] {
            assert!(validate_journal(&[create.clone(), entry]).is_err());
        }
        let palette = JournalEntry::new(
            ToolName::DocPalette,
            serde_json::from_value(json!({
                "op": "generate",
                "base": [1, 2, 3],
                "set_doc": id
            }))
            .unwrap(),
        );
        validate_journal(&[create, palette]).unwrap();
    }

    #[test]
    fn journals_are_versioned_and_legacy_v1_remains_readable() {
        let s = studio("journal-version");
        let created = s.doc_new("d", 8, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.journal_append(id, ToolName::DocNew, &json!({"name": "d", "doc_id": id}))
            .unwrap();
        let path = s.journal_path(id);
        let current: Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(current["format_version"], JOURNAL_FORMAT_VERSION);

        let legacy =
            format!("{{\"tool\":\"doc_new\",\"args\":{{\"name\":\"d\",\"doc_id\":\"{id}\"}}}}\n");
        fs::write(&path, legacy).unwrap();
        assert_eq!(
            s.journal(id).unwrap()[0].format_version,
            JOURNAL_FORMAT_VERSION
        );

        let mut future = current;
        future["format_version"] = json!(JOURNAL_FORMAT_VERSION + 1);
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&future).unwrap()),
        )
        .unwrap();
        let error = s.journal(id).unwrap_err();
        assert!(error.contains("unsupported journal format"), "got: {error}");
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
    fn document_pages_are_bounded_and_cursor_stable() {
        let s = studio("pages");
        for name in ["one", "two", "three"] {
            s.doc_new(name, 4, 4).unwrap();
        }

        let first = s.list_docs_page(None, None, None, 1).unwrap();
        assert_eq!(first["count"], 1);
        assert_eq!(first["total"], 3);
        assert_eq!(first["truncated"], true);
        let first_id = first["documents"][0]["doc_id"].as_str().unwrap();
        assert_eq!(first["next_cursor"], first_id);

        let second = s
            .list_docs_page(None, None, first["next_cursor"].as_str(), 1)
            .unwrap();
        let second_id = second["documents"][0]["doc_id"].as_str().unwrap();
        assert!(second_id > first_id);
        assert_eq!(second["total"], 3);
        assert_eq!(second["truncated"], true);

        let final_page = s.list_docs_page(None, None, Some(second_id), 1).unwrap();
        assert_eq!(final_page["count"], 1);
        assert_eq!(final_page["truncated"], false);
        assert!(final_page["next_cursor"].is_null());

        assert!(s.list_docs_page(None, None, None, 0).is_err());
        assert!(s.list_docs_page(None, None, Some("not-a-uuid"), 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn normal_store_access_refuses_symlinked_documents_and_files() {
        use std::os::unix::fs::symlink;

        let s = studio("store-symlink-safety");
        let created = s.doc_new("safe", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let document = s.doc_dir(id);
        let held = s.docs_dir.join("held-document");

        fs::rename(&document, &held).unwrap();
        symlink(&held, &document).unwrap();
        assert!(!s.exists(id));
        assert!(!s.doc_ids().iter().any(|candidate| candidate == id));
        assert!(s.open(id).is_err());
        assert!(s.delete_doc(id).is_err());

        fs::remove_file(&document).unwrap();
        fs::rename(&held, &document).unwrap();
        let metadata = document.join("doc.json");
        let held_metadata = document.join("held-doc.json");
        fs::rename(&metadata, &held_metadata).unwrap();
        symlink(&held_metadata, &metadata).unwrap();
        assert!(!s.exists(id));
        assert!(s.open(id).is_err());

        fs::remove_file(&metadata).unwrap();
        fs::rename(&held_metadata, &metadata).unwrap();
        s.journal_append(id, ToolName::DocNew, &json!({"name": "safe", "doc_id": id}))
            .unwrap();
        let journal = s.journal_path(id);
        let held_journal = document.join("held-recipe.jsonl");
        fs::rename(&journal, &held_journal).unwrap();
        symlink(&held_journal, &journal).unwrap();
        assert!(
            s.journal_append(id, ToolName::DocDraw, &json!({"doc_id": id, "op": "rect"}))
                .is_err()
        );
        let error = s.journal(id).unwrap_err();
        assert!(error.contains("regular file"), "got: {error}");

        let _ = fs::remove_dir_all(&s.docs_dir);
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
