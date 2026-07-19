//! Engine-ready exports: spritesheet (+JSON sidecars), GIF and APNG.

use std::path::Path;

use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use crate::raster;

use super::Document;

impl Document {
    /// Render the horizontal spritesheet image (every frame side by side,
    /// nearest-neighbour scaled). Returns `(sheet, frame_w, frame_h)`, or an
    /// error if the strip's dimensions overflow `u32` — a many-frames × high
    /// scale sheet can otherwise wrap to a garbage-sized buffer.
    pub(super) fn sheet_image(&self, scale: u32) -> Result<(RgbaImage, u32, u32), String> {
        let n = self.meta.frames.len() as u32;
        let fw = self
            .meta
            .w
            .checked_mul(scale)
            .ok_or("sheet scale overflow")?;
        let fh = self
            .meta
            .h
            .checked_mul(scale)
            .ok_or("sheet scale overflow")?;
        let strip_w = fw.checked_mul(n).ok_or_else(|| {
            format!(
                "spritesheet is too large: {n} frames × {fw}px wide overflows — \
                 export fewer frames or a lower scale"
            )
        })?;
        let mut sheet = RgbaImage::from_pixel(strip_w, fh, Rgba([0, 0, 0, 0]));
        for f in 0..self.meta.frames.len() {
            let mut img = self.flatten(f);
            if scale > 1 {
                img = image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Nearest);
            }
            image::imageops::replace(&mut sheet, &img, (f as u32 * fw) as i64, 0);
        }
        Ok((sheet, fw, fh))
    }

    pub fn export_sheet(&self, out: &Path, scale: u32) -> Result<Value, String> {
        let n = self.meta.frames.len() as u32;
        let (sheet, fw, fh) = self.sheet_image(scale)?;
        sheet.save(out).map_err(|e| e.to_string())?;
        let frames: Vec<Value> = self
            .meta
            .frames
            .iter()
            .enumerate()
            .map(|(i, fr)| {
                json!({
                    "rect": [i as u32 * fw, 0, fw, fh],
                    "duration_ms": fr.duration_ms,
                })
            })
            .collect();
        let tags: Vec<Value> = self
            .meta
            .tags
            .iter()
            .map(|t| json!({"name": t.name, "from": t.from, "to": t.to, "direction": t.direction}))
            .collect();
        let slices = sheet_slices(&self.meta.slices, scale);
        let mut meta = json!({
            "path": out.to_string_lossy(), "frame_w": fw, "frame_h": fh,
            "count": n, "frames": frames, "tags": tags, "palette": self.meta.palette,
        });
        // Like the sidecar's early days: no slices, no `slices` key — readers
        // of the old shape see exactly what they always did.
        if !slices.is_empty() {
            meta["slices"] = json!(slices);
        }
        let mp = out.with_extension("json");
        std::fs::write(
            &mp,
            serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(meta)
    }

    /// Export the spritesheet with the industry-standard hash sprite-JSON
    /// sidecar instead of atelier's richer native shape — the layout that game
    /// engines' existing sheet importers already parse (`frames` keyed by name
    /// with `frame`/`sourceSize`/`duration`, `meta.frameTags`).
    pub fn export_sheet_std(&self, out: &Path, scale: u32) -> Result<Value, String> {
        let (sheet, fw, fh) = self.sheet_image(scale)?;
        sheet.save(out).map_err(|e| e.to_string())?;
        let stem = out
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.meta.name.clone());
        let image_name = out
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{stem}.png"));
        let mut frames = serde_json::Map::new();
        for (i, fr) in self.meta.frames.iter().enumerate() {
            frames.insert(
                format!("{stem} {i}"),
                json!({
                    "frame": {"x": i as u32 * fw, "y": 0, "w": fw, "h": fh},
                    "rotated": false,
                    "trimmed": false,
                    "spriteSourceSize": {"x": 0, "y": 0, "w": fw, "h": fh},
                    "sourceSize": {"w": fw, "h": fh},
                    "duration": fr.duration_ms,
                }),
            );
        }
        let frame_tags: Vec<Value> = self
            .meta
            .tags
            .iter()
            .map(|t| json!({"name": t.name, "from": t.from, "to": t.to, "direction": t.direction}))
            .collect();
        let mut meta = json!({
            "frames": frames,
            "meta": {
                "app": "atelier",
                "version": env!("CARGO_PKG_VERSION"),
                "image": image_name,
                "format": "RGBA8888",
                "size": {"w": sheet.width(), "h": sheet.height()},
                "scale": "1",
                "frameTags": frame_tags,
            },
        });
        // The engine-standard shape: meta.slices with per-frame keys holding
        // x/y/w/h bounds (+ optional center/pivot), in sheet pixels like
        // `frame` above.
        if !self.meta.slices.is_empty() {
            meta["meta"]["slices"] = json!(std_slices(&self.meta.slices, scale));
        }
        let mp = out.with_extension("json");
        std::fs::write(
            &mp,
            serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(json!({
            "path": out.to_string_lossy(),
            "meta_path": mp.to_string_lossy(),
            "meta_format": "standard",
            "count": self.meta.frames.len(),
            "frame_w": fw,
            "frame_h": fh,
        }))
    }

    /// Export an animated GIF. Plays `tag`'s sequence (honouring direction) or
    /// the whole timeline when `tag` is None. Returns the number of frames
    /// actually emitted (a pingpong tag emits more than its source range).
    pub fn export_gif(&self, out: &Path, scale: u32, tag: Option<&str>) -> Result<usize, String> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let seq = self.play_sequence(tag)?;
        // Build ONE palette across every frame and snap each frame to it before
        // encoding. The image crate quantizes each frame independently, so on
        // multi-colour art it picks a different 256-subset per frame — the
        // source of GIF inter-frame flicker. A shared palette (the locked one,
        // else a median-cut over all frames) makes the colours identical
        // frame-to-frame, so motion is the only thing that changes.
        let global: Vec<[u8; 4]> = if !self.meta.palette.is_empty() {
            self.meta.palette.clone()
        } else {
            // Count per distinct colour (BTreeMap = sorted, so the weighted
            // cut sees a deterministic order) instead of a per-pixel Vec —
            // same cut, far less memory on large frames.
            let mut counts: std::collections::BTreeMap<[u8; 3], u64> =
                std::collections::BTreeMap::new();
            for &f in &seq {
                for p in self.flatten(f).pixels() {
                    if p.0[3] > 0 {
                        *counts.entry([p.0[0], p.0[1], p.0[2]]).or_insert(0) += 1;
                    }
                }
            }
            if counts.is_empty() {
                Vec::new()
            } else {
                let pairs: Vec<([u8; 3], u64)> = counts.into_iter().collect();
                raster::median_cut_weighted(&pairs, 256, &[])
            }
        };
        let mut lab = raster::PaletteLab::new(&global);
        let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
        let mut enc = GifEncoder::new(std::io::BufWriter::new(file));
        enc.set_repeat(Repeat::Infinite)
            .map_err(|e| e.to_string())?;
        for &f in &seq {
            let mut img = self.flatten(f);
            if !global.is_empty() {
                for p in img.pixels_mut() {
                    if p.0[3] > 0 {
                        if let Some(i) = lab.nearest(p.0) {
                            let c = lab.color(i);
                            *p = Rgba([c[0], c[1], c[2], p.0[3]]);
                        }
                    }
                }
            }
            if scale > 1 {
                // Same checked math as sheet_image: studio clamps scale, but a
                // library caller can pass anything.
                let (fw, fh) = (
                    self.meta.w.checked_mul(scale).ok_or("gif scale overflow")?,
                    self.meta.h.checked_mul(scale).ok_or("gif scale overflow")?,
                );
                img = image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Nearest);
            }
            // GIF frame delay is u16 centiseconds — cap at the format's ceiling
            // (~655s) instead of truncating into a wrapped, faster frame.
            let ms = self.meta.frames[f].duration_ms.min(655_350);
            let delay = Delay::from_numer_denom_ms(ms, 1);
            enc.encode_frame(Frame::from_parts(img, 0, 0, delay))
                .map_err(|e| e.to_string())?;
        }
        Ok(seq.len())
    }

    /// Export an animated PNG (APNG) — the lossless, full-alpha sibling of
    /// `export_gif` (GIF is 256 colours with 1-bit alpha). Plays `tag`'s sequence
    /// (honouring direction) or the whole timeline when `tag` is None;
    /// nearest-neighbour `scale`; per-frame delay is `duration_ms` (in 1/1000s, or
    /// 1/100s for delays beyond 65s). Returns the number of frames emitted (a
    /// pingpong tag emits more than its range).
    pub fn export_apng(&self, out: &Path, scale: u32, tag: Option<&str>) -> Result<usize, String> {
        let seq = self.play_sequence(tag)?;
        if seq.is_empty() {
            return Err("nothing to export: document has no frames".into());
        }
        let sc = scale.max(1);
        // Same checked math as sheet_image: studio clamps scale, but a library
        // caller can pass anything.
        let (w, h) = (
            self.meta.w.checked_mul(sc).ok_or("apng scale overflow")?,
            self.meta.h.checked_mul(sc).ok_or("apng scale overflow")?,
        );
        let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(seq.len() as u32, 0)
            .map_err(|e| e.to_string())?;
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        for &f in &seq {
            let mut img = self.flatten(f);
            if sc > 1 {
                img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Nearest);
            }
            // PNG frame delay is a u16/u16 rational; pick a denominator that fits:
            // ms/1000 while it fits in u16, else centiseconds/100 (capped at u16).
            let ms = self.meta.frames[f].duration_ms;
            let (num, den) = if ms <= u16::MAX as u32 {
                (ms as u16, 1000)
            } else {
                ((ms / 10).min(u16::MAX as u32) as u16, 100)
            };
            writer
                .set_frame_delay(num, den)
                .map_err(|e| e.to_string())?;
            writer
                .write_image_data(img.as_raw())
                .map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
        Ok(seq.len())
    }
}

/// Scale an inclusive-corner rect from document to sheet pixels — the far
/// corner covers a whole scaled pixel cell, hence `(c + 1) * scale - 1`.
fn scale_rect(r: [i32; 4], scale: u32) -> [i32; 4] {
    let s = scale as i32;
    [r[0] * s, r[1] * s, (r[2] + 1) * s - 1, (r[3] + 1) * s - 1]
}

/// Inclusive corners → the x/y/w/h object the engine-standard sheet format
/// uses.
fn rect_xywh(r: [i32; 4]) -> Value {
    json!({"x": r[0], "y": r[1], "w": r[2] - r[0] + 1, "h": r[3] - r[1] + 1})
}

/// Slices in the native sidecar's shape (name/rect/center/pivot), corners
/// scaled into sheet pixels like the frame rects.
fn sheet_slices(slices: &[super::SliceMeta], scale: u32) -> Vec<Value> {
    slices
        .iter()
        .map(|s| {
            let mut o = serde_json::Map::new();
            o.insert("name".into(), json!(s.name));
            o.insert("rect".into(), json!(scale_rect(s.rect, scale)));
            if let Some(c) = s.center {
                o.insert("center".into(), json!(scale_rect(c, scale)));
            }
            if let Some(p) = s.pivot {
                o.insert(
                    "pivot".into(),
                    json!([p[0] * scale as i32, p[1] * scale as i32]),
                );
            }
            Value::Object(o)
        })
        .collect()
}

/// Slices in the engine-standard sheet-JSON shape (`meta.slices` with
/// per-frame keys) for `export_sheet_std`.
fn std_slices(slices: &[super::SliceMeta], scale: u32) -> Vec<Value> {
    slices
        .iter()
        .map(|s| {
            let mut key = serde_json::Map::new();
            key.insert("frame".into(), json!(0));
            key.insert("bounds".into(), rect_xywh(scale_rect(s.rect, scale)));
            if let Some(c) = s.center {
                key.insert("center".into(), rect_xywh(scale_rect(c, scale)));
            }
            if let Some(p) = s.pivot {
                key.insert(
                    "pivot".into(),
                    json!({"x": p[0] * scale as i32, "y": p[1] * scale as i32}),
                );
            }
            json!({"name": s.name, "keys": [Value::Object(key)]})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atelier_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn export_sheet_emits_slices_only_when_present() {
        let dir = tmp_dir("slice_export");
        let mut d = Document::new("t", 8, 8);
        d.fill_cel(0, 0, [1, 2, 3, 255]).unwrap();
        // No slices → no key: readers of the old shape see no difference.
        let plain = d.export_sheet(&dir.join("plain.png"), 1).unwrap();
        assert!(plain.get("slices").is_none());
        // With a slice: name/rect/center/pivot, corners scaled to sheet pixels.
        d.add_slice("hud", [1, 1, 3, 2], Some([2, 1, 2, 2]), Some([1, 1]))
            .unwrap();
        let out = dir.join("sliced.png");
        let meta = d.export_sheet(&out, 2).unwrap();
        let slices = meta["slices"].as_array().unwrap();
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0]["name"], json!("hud"));
        assert_eq!(slices[0]["rect"], json!([2, 2, 7, 5]));
        assert_eq!(slices[0]["center"], json!([4, 2, 5, 5]));
        assert_eq!(slices[0]["pivot"], json!([2, 2]));
        // The sidecar on disk carries them too.
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap())
                .unwrap();
        assert_eq!(on_disk["slices"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_sheet_std_emits_engine_shaped_slices() {
        let dir = tmp_dir("slice_export_std");
        let mut d = Document::new("t", 8, 8);
        d.fill_cel(0, 0, [1, 2, 3, 255]).unwrap();
        d.add_slice("nine", [1, 1, 5, 5], Some([2, 2, 4, 4]), Some([3, 3]))
            .unwrap();
        let out = dir.join("std.png");
        d.export_sheet_std(&out, 1).unwrap();
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap())
                .unwrap();
        let slices = meta["meta"]["slices"].as_array().unwrap();
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0]["name"], json!("nine"));
        let key = &slices[0]["keys"][0];
        assert_eq!(key["bounds"], json!({"x": 1, "y": 1, "w": 5, "h": 5}));
        assert_eq!(key["center"], json!({"x": 2, "y": 2, "w": 3, "h": 3}));
        assert_eq!(key["pivot"], json!({"x": 3, "y": 3}));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
