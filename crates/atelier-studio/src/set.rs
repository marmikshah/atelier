//! Cross-document set tools — the game-level eye. A game is not one sprite but
//! a SET of documents that must read as one work: shared palette, consistent
//! value range, consistent scale, aligned pivots. These tools resolve a family
//! (explicit ids or an id prefix) and audit or synchronize it as a whole.

use serde_json::{json, Value};

use atelier_core::document::AlphaSnap;

use super::Studio;

impl Studio {
    /// Resolve a document set from explicit `ids` and/or an id `prefix`.
    /// Errors on an unknown id or an empty resolution — a set tool silently
    /// running on nothing is how "audited the game" lies happen.
    fn resolve_set(
        &self,
        ids: Option<&[String]>,
        prefix: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(ids) = ids {
            for id in ids {
                if !self.exists(id) {
                    return Err(format!("no document '{}'", id));
                }
                if !out.contains(id) {
                    out.push(id.clone());
                }
            }
        }
        if let Some(p) = prefix {
            for id in self.doc_ids() {
                if id.starts_with(p) && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        if out.is_empty() {
            return Err("set resolves to no documents — pass ids and/or a matching prefix".into());
        }
        Ok(out)
    }

    /// Broadcast ONE palette across a document set: lock it on every member and
    /// perceptually snap every cel onto it. `palette` explicit colours, or
    /// `from_doc` copies the source doc's locked palette. Returns per-doc
    /// moved-pixel counts.
    pub fn doc_set_palette_sync(
        &self,
        ids: Option<&[String]>,
        prefix: Option<&str>,
        palette: Option<Vec<[u8; 4]>>,
        from_doc: Option<&str>,
    ) -> Result<Value, String> {
        let members = self.resolve_set(ids, prefix)?;
        let pal: Vec<[u8; 4]> = match (palette, from_doc) {
            (Some(p), _) if !p.is_empty() => p,
            (_, Some(src)) => {
                let (_d, doc) = self.open(src)?;
                if doc.meta().palette.is_empty() {
                    return Err(format!("source doc '{}' has no locked palette", src));
                }
                doc.meta().palette.clone()
            }
            _ => return Err("pass `palette` colours or `from_doc` to copy from".into()),
        };
        let mut results: Vec<Value> = Vec::new();
        for id in &members {
            let (dir, mut doc) = self.open(id)?;
            doc.set_palette(pal.clone());
            let moved = doc.snap_to_palette(&pal, None, None, AlphaSnap::Preserve);
            doc.save(&dir)?;
            results.push(json!({"id": id, "pixels_moved": moved}));
        }
        Ok(json!({
            "ok": true,
            "palette_colors": pal.len(),
            "documents": results,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::Studio;
    use serde_json::{json, Value};

    fn studio(name: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-set-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    /// Single draw-op shorthand: `params` is the op's JSON object (as `json!`).
    fn draw(s: &Studio, id: &str, op: &str, params: Value) -> Result<Value, String> {
        s.doc_draw(id, 0, 0, op, params.as_object().unwrap().clone())
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn palette_sync_broadcasts_and_snaps() {
        let s = studio("sync");
        s.doc_create("src", 8, 8).unwrap();
        s.doc_set_palette("src", vec![[255, 0, 0, 255], [0, 0, 255, 255]])
            .unwrap();
        s.doc_create("tgt", 8, 8).unwrap();
        // Off-palette reddish pixel that must snap to pure red.
        draw(
            &s,
            "tgt",
            "pencil",
            json!({"points": [[1, 1]], "color": [220, 30, 30, 255]}),
        )
        .unwrap();
        let r = s
            .doc_set_palette_sync(Some(&ids(&["tgt"])), None, None, Some("src"))
            .unwrap();
        assert_eq!(r["palette_colors"], 2);
        assert_eq!(r["documents"][0]["pixels_moved"], 1);
        let px = s.doc_get_pixel("tgt", Some(0), 0, 1, 1).unwrap();
        assert_eq!(px["rgba"], serde_json::json!([255, 0, 0, 255]));
    }

    #[test]
    fn list_docs_filters_by_prefix_and_substring() {
        let s = studio("filters");
        for n in ["hero-idle", "hero-run", "tile-grass"] {
            s.doc_create(n, 4, 4).unwrap();
        }
        let all = s.list_docs_filtered(None, None);
        assert_eq!(all["count"], 3);
        let fam = s.list_docs_filtered(Some("hero-"), None);
        assert_eq!(fam["count"], 2);
        let sub = s.list_docs_filtered(None, Some("grass"));
        assert_eq!(sub["count"], 1);
        let both = s.list_docs_filtered(Some("hero-"), Some("run"));
        assert_eq!(both["count"], 1);
    }
}
