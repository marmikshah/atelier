//! The store itself: where documents live on disk, the `ATELIER_HOME` policy,
//! and the per-document journal (`recipe.jsonl`) that makes every document a
//! replayable recipe.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use atelier_core::document::Document;

use super::{slugify, Studio, JOURNAL_FILE, MAX_CANVAS};

impl Studio {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Studio {
        let home = std::env::var("ATELIER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                // No resolvable home = a deliberate, visible choice of the temp
                // dir — not a silent relative "./.atelier" wherever the process
                // happens to run (matches the binary's service::default_home).
                dirs::home_dir()
                    .map(|h| h.join(".atelier"))
                    .unwrap_or_else(|| std::env::temp_dir().join("atelier"))
            });
        let docs_dir = home.join("documents");
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
            if let Some(p) = prefix {
                if !id.starts_with(p) {
                    continue;
                }
            }
            if let Some(c) = contains {
                if !id.contains(c) {
                    continue;
                }
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
                Err(_) if idx == last => break, // torn final line — crash mid-append
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
}
