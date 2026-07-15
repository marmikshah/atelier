//! Cross-document set tools — the game-level eye. A game is not one sprite but
//! a SET of documents that must read as one work: shared palette, consistent
//! value range, consistent scale, aligned pivots. These tools resolve a family
//! (explicit ids or an id prefix) and audit or synchronize it as a whole.

use serde_json::{json, Value};

use atelier_core::document::AlphaSnap;
use atelier_core::raster;

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

    /// Audit N documents as ONE game: per-doc palette/value/scale/pivot stats
    /// plus set-level cohesion verdicts. Frame 0 of the flattened composite is
    /// the sample (the representative pose).
    pub fn doc_set_audit(
        &self,
        ids: Option<&[String]>,
        prefix: Option<&str>,
    ) -> Result<Value, String> {
        let members = self.resolve_set(ids, prefix)?;
        if members.len() < 2 {
            return Err("set audit needs at least 2 documents".into());
        }
        struct DocStat {
            id: String,
            palette: Vec<[u8; 4]>,
            off_palette: u64,
            opaque: u64,
            /// Darkest/brightest luma over the opaque pixels — `None` when the
            /// document has none, since an empty document has no value range
            /// rather than a range of nothing.
            value: Option<(u8, u8)>,
            height: u32,
            has_pivot: bool,
        }
        let mut stats: Vec<DocStat> = Vec::new();
        for id in &members {
            let (_dir, doc) = self.open(id)?;
            let img = doc.analysis_image(None, 0)?;
            let pal = doc.meta().palette.clone();
            let inset: std::collections::HashSet<[u8; 4]> = pal.iter().copied().collect();
            let (mut vmin, mut vmax, mut opaque, mut off) = (255u8, 0u8, 0u64, 0u64);
            let (mut y0, mut y1) = (i64::MAX, i64::MIN);
            for (_, y, p) in img.enumerate_pixels() {
                if p.0[3] == 0 {
                    continue;
                }
                opaque += 1;
                let v = raster::luma(p.0);
                vmin = vmin.min(v);
                vmax = vmax.max(v);
                y0 = y0.min(y as i64);
                y1 = y1.max(y as i64);
                if !pal.is_empty() && !inset.contains(&p.0) {
                    off += 1;
                }
            }
            let height = if y1 >= y0 { (y1 - y0 + 1) as u32 } else { 0 };
            let has_pivot = doc.meta().frames.iter().any(|f| f.pivot.is_some());
            stats.push(DocStat {
                id: id.clone(),
                palette: pal,
                off_palette: off,
                opaque,
                value: (opaque > 0).then_some((vmin, vmax)),
                height,
                has_pivot,
            });
        }
        // -- palette cohesion: union of locked palettes; a set reads as one
        // game when the union stays small and every doc draws from it.
        let mut union: Vec<[u8; 4]> = Vec::new();
        for s in &stats {
            for c in &s.palette {
                if !union.contains(c) {
                    union.push(*c);
                }
            }
        }
        let unlocked: Vec<&str> = stats
            .iter()
            .filter(|s| s.palette.is_empty())
            .map(|s| s.id.as_str())
            .collect();
        // Near-duplicate colours across docs (OKLab ΔE < 0.04): two docs each
        // on-palette can still drift apart by shipping almost-identical swatches.
        let mut near_dupes: Vec<Value> = Vec::new();
        for i in 0..union.len() {
            for j in (i + 1)..union.len() {
                let (a, b) = (union[i], union[j]);
                let (l1, a1, b1) = raster::srgb_to_oklab(a);
                let (l2, a2, b2) = raster::srgb_to_oklab(b);
                let de = ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt();
                if de < 0.04 && near_dupes.len() < 16 {
                    near_dupes.push(json!({
                        "a": crate::hex_rgb(&a),
                        "b": crate::hex_rgb(&b),
                        "delta_e": (de * 1000.0).round() / 1000.0,
                    }));
                }
            }
        }
        // -- scale cohesion: silhouette heights vs the set median. Characters
        // drawn at 0.5x/2x the set scale are the classic mixed-source tell.
        let mut heights: Vec<u32> = stats.iter().map(|s| s.height).filter(|h| *h > 0).collect();
        heights.sort_unstable();
        let median = heights.get(heights.len() / 2).copied().unwrap_or(0);
        let scale_outliers: Vec<Value> = stats
            .iter()
            .filter(|s| {
                s.height > 0
                    && median > 0
                    && (s.height as f64 / median as f64 > 1.6
                        || (s.height as f64 / median as f64) < 0.6)
            })
            .map(|s| json!({"id": s.id, "height": s.height, "median": median}))
            .collect();
        // -- value cohesion: per-doc contrast ranges should overlap; a doc that
        // lives in a different value band reads as pasted from another game.
        // Empty documents carry no value, so they neither widen the set range
        // nor drag it to a bogus [255, 0].
        let set_range = {
            let mut vals = stats.iter().filter_map(|s| s.value).peekable();
            vals.peek().is_some().then(|| {
                vals.fold((255u8, 0u8), |(lo, hi), (vmin, vmax)| {
                    (lo.min(vmin), hi.max(vmax))
                })
            })
        };
        let docs_json: Vec<Value> = stats
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "palette_colors": s.palette.len(),
                    "off_palette_px": s.off_palette,
                    "opaque_px": s.opaque,
                    "value_range": s.value.map(|(lo, hi)| json!([lo, hi])),
                    "silhouette_height": s.height,
                    "has_pivot": s.has_pivot,
                })
            })
            .collect();
        let no_pivot: Vec<&str> = stats
            .iter()
            .filter(|s| !s.has_pivot)
            .map(|s| s.id.as_str())
            .collect();
        let mut warnings: Vec<String> = Vec::new();
        if !unlocked.is_empty() {
            warnings.push(format!(
                "{} doc(s) have no locked palette: {}",
                unlocked.len(),
                unlocked.join(", ")
            ));
        }
        if union.len() > 32 {
            warnings.push(format!(
                "palette union is {} colours — a cohesive set usually shares far fewer; run doc_palette op=sync",
                union.len()
            ));
        }
        if !near_dupes.is_empty() {
            warnings.push(format!(
                "{} near-duplicate colour pair(s) across docs (ΔE<0.04) — merge them",
                near_dupes.len()
            ));
        }
        if !scale_outliers.is_empty() {
            warnings.push(format!(
                "{} doc(s) are scale outliers vs the set median silhouette height",
                scale_outliers.len()
            ));
        }
        if stats.iter().any(|s| s.off_palette > 0) {
            warnings.push("off-palette pixels present — doc_palette op=snap the offenders".into());
        }
        Ok(json!({
            "members": members,
            "documents": docs_json,
            "palette": {
                "union_colors": union.len(),
                "unlocked_docs": unlocked,
                "near_duplicates": near_dupes,
            },
            "scale": {
                "median_silhouette_height": median,
                "outliers": scale_outliers,
            },
            "value": {"set_range": set_range.map(|(lo, hi)| json!([lo, hi]))},
            "pivots": {"missing": no_pivot},
            "verdict": if warnings.is_empty() { json!("cohesive") } else { json!(warnings) },
        }))
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
    fn set_audit_flags_palette_drift_and_scale_outlier() {
        let s = studio("audit");
        // Two small sprites on one palette, one giant sprite on its own colours.
        for (name, h, color) in [
            ("hero-idle", 10i32, [200, 40, 40, 255]),
            ("hero-run", 11, [200, 40, 40, 255]),
            ("hero-boss", 30, [40, 40, 210, 255]),
        ] {
            s.doc_create(name, 32, 32).unwrap();
            draw(
                &s,
                name,
                "rect",
                json!({"x0": 4, "y0": 1, "x1": 8, "y1": h, "color": color, "fill": true}),
            )
            .unwrap();
        }
        s.doc_set_palette("hero-idle", vec![[200, 40, 40, 255]])
            .unwrap();
        s.doc_set_palette("hero-run", vec![[200, 40, 40, 255]])
            .unwrap();
        let r = s.doc_set_audit(None, Some("hero-")).unwrap();
        assert_eq!(r["members"].as_array().unwrap().len(), 3);
        // hero-boss: unlocked palette + scale outlier vs median height.
        let verdict = r["verdict"].as_array().expect("warnings expected");
        assert!(!verdict.is_empty());
        assert_eq!(r["scale"]["outliers"].as_array().unwrap().len(), 1);
        assert_eq!(
            r["palette"]["unlocked_docs"].as_array().unwrap()[0],
            Value::from("hero-boss")
        );
    }

    #[test]
    fn set_audit_cohesive_when_shared() {
        let s = studio("cohesive");
        for name in ["orc-a", "orc-b"] {
            s.doc_create(name, 16, 16).unwrap();
            draw(
                &s,
                name,
                "rect",
                json!({"x0": 4, "y0": 4, "x1": 10, "y1": 12, "color": [90, 140, 80, 255], "fill": true}),
            )
            .unwrap();
            s.doc_set_palette(name, vec![[90, 140, 80, 255]]).unwrap();
            s.doc_set_pivot(name, 0, Some([8, 12])).unwrap();
        }
        let r = s
            .doc_set_audit(Some(&ids(&["orc-a", "orc-b"])), None)
            .unwrap();
        assert_eq!(r["verdict"], Value::from("cohesive"));
        assert_eq!(r["palette"]["union_colors"], 1);
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

    #[test]
    fn resolve_set_errors_on_unknown_and_empty() {
        let s = studio("resolve");
        s.doc_create("a", 4, 4).unwrap();
        assert!(s.doc_set_audit(Some(&ids(&["a", "ghost"])), None).is_err());
        assert!(s.doc_set_audit(None, Some("zz-")).is_err());
    }
}
