//! The document store: a flat library of editable pixel-art documents.
//!
//! State lives under ~/.atelier (override with ATELIER_HOME). Each document
//! is a directory `documents/<id>/` with a `doc.json` (structure + cel refs) and
//! one PNG per cel under `cels/`. There is no project/grouping layer — a document
//! is the unit, addressed by its `id` (a slug derived from its name).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::document::Document;

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

#[derive(Clone)]
pub struct Studio {
    docs_dir: PathBuf,
}

impl Studio {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Studio {
        let home = std::env::var("ATELIER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".atelier"));
        let docs_dir = home.join("documents");
        let _ = fs::create_dir_all(&docs_dir);
        Studio { docs_dir }
    }

    /// Test-only: build a studio rooted at an explicit directory (avoids the
    /// process-global ATELIER_HOME env var, so tests stay parallel-safe).
    #[cfg(test)]
    fn with_docs_dir(docs_dir: PathBuf) -> Studio {
        let _ = fs::create_dir_all(&docs_dir);
        Studio { docs_dir }
    }

    fn doc_dir(&self, id: &str) -> PathBuf {
        self.docs_dir.join(id)
    }

    fn exists(&self, id: &str) -> bool {
        self.doc_dir(id).join("doc.json").exists()
    }

    /// All document ids on disk (directories with a doc.json), sorted.
    fn doc_ids(&self) -> Vec<String> {
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

    fn open(&self, id: &str) -> Result<(PathBuf, Document), String> {
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

    pub fn list_docs(&self) -> Value {
        let mut items = Vec::new();
        for id in self.doc_ids() {
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
        if !self.exists(id) {
            return Err(format!("no document '{}'", id));
        }
        fs::remove_dir_all(self.doc_dir(id)).map_err(|e| e.to_string())?;
        Ok(json!({"deleted": id}))
    }

    // -- structure / timeline (open -> mutate -> save) ----------------------

    // First caller (the open -> mutate -> commit ops) lands in a later step.
    #[allow(dead_code)]
    fn commit(&self, dir: &Path, id: &str, mut doc: Document) -> Result<Value, String> {
        doc.save(dir)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
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
    fn create_persists_and_lists() {
        let s = studio("create");
        s.doc_create("Hero Sprite", 16, 16).unwrap();
        let listed = s.list_docs();
        assert_eq!(listed["count"], 1);
        // slug derived from the name
        assert_eq!(listed["documents"][0]["id"], "hero-sprite");
        // reloads from disk (open path), not just in-memory
        assert_eq!(s.doc_info("hero-sprite").unwrap()["w"], 16);
    }

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("Hero Sprite"), "hero-sprite");
        assert_eq!(slugify("  Multi   Space  "), "multi-space");
        assert_eq!(slugify("Weird!!Chars??"), "weird-chars");
        // empty / punctuation-only falls back
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("---"), "untitled");
    }

    #[test]
    fn unique_id_disambiguates_collisions() {
        let s = studio("unique");
        // three docs with the same name → suffixed slugs
        s.doc_create("dup", 4, 4).unwrap();
        s.doc_create("dup", 4, 4).unwrap();
        s.doc_create("dup", 4, 4).unwrap();
        let listed = s.list_docs();
        assert_eq!(listed["count"], 3);
        let ids: Vec<String> = listed["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["dup", "dup-2", "dup-3"]);
    }
}
