//! The editable document model — atelier's Aseprite-class core.
//!
//! A `Document` is a canvas of ordered **layers** (opacity / visibility / blend)
//! over a timeline of **frames** (each with a duration). A **cel** is one
//! layer×frame image placed at (x,y); cels are sparse. The document also holds a
//! **palette** and animation **tags** (named frame ranges).
//!
//! Persistence: a directory with `doc.json` (structure + cel file refs) and one
//! PNG per cel under `cels/`. Rendering flattens visible layers at a frame with
//! source-over compositing scaled by layer opacity; export covers flattened PNG,
//! a spritesheet (+ JSON meta) and an animated GIF that honours frame durations.

use std::collections::HashMap;
use std::path::Path;

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Serialize, Deserialize, Clone)]
pub struct LayerMeta {
    pub name: String,
    pub opacity: u8,
    pub visible: bool,
    /// Compositing mode: normal/multiply/screen/add/overlay/soft-light/
    /// hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/
    /// exclusion. Unknown values fall back to normal. See `Blend`.
    pub blend: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FrameMeta {
    pub duration_ms: u32,
    /// Optional anchor point in document pixels (e.g. feet / weapon mount).
    /// Engines read this to position the sprite; None = top-left origin.
    #[serde(default)]
    pub pivot: Option<[i32; 2]>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TagMeta {
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: String, // "forward" | "reverse" | "pingpong"
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CelMeta {
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub file: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DocMeta {
    pub name: String,
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub palette: Vec<[u8; 4]>,
    pub layers: Vec<LayerMeta>,
    pub frames: Vec<FrameMeta>,
    #[serde(default)]
    pub tags: Vec<TagMeta>,
    pub cels: Vec<CelMeta>,
}

/// A loaded document: structure + the cel images in memory.
pub struct Document {
    pub meta: DocMeta,
    /// (layer, frame) -> (x, y, image)
    cels: HashMap<(usize, usize), (i32, i32, RgbaImage)>,
}

fn cel_file(layer: usize, frame: usize) -> String {
    format!("cels/L{}_F{}.png", layer, frame)
}

impl Document {
    pub fn new(name: &str, w: u32, h: u32) -> Document {
        let meta = DocMeta {
            name: name.to_string(),
            w,
            h,
            palette: Vec::new(),
            layers: vec![LayerMeta {
                name: "Layer 1".into(),
                opacity: 255,
                visible: true,
                blend: "normal".into(),
            }],
            frames: vec![FrameMeta {
                duration_ms: 100,
                pivot: None,
            }],
            tags: Vec::new(),
            cels: Vec::new(),
        };
        Document {
            meta,
            cels: HashMap::new(),
        }
    }

    pub fn load(dir: &Path) -> Result<Document, String> {
        let s = std::fs::read_to_string(dir.join("doc.json")).map_err(|e| e.to_string())?;
        let meta: DocMeta = serde_json::from_str(&s).map_err(|e| e.to_string())?;
        let mut cels = HashMap::new();
        for c in &meta.cels {
            let img = image::open(dir.join(&c.file))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            cels.insert((c.layer, c.frame), (c.x, c.y, img));
        }
        Ok(Document { meta, cels })
    }

    pub fn save(&mut self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir.join("cels")).map_err(|e| e.to_string())?;
        let mut cel_metas = Vec::new();
        for ((layer, frame), (x, y, img)) in &self.cels {
            let file = cel_file(*layer, *frame);
            img.save(dir.join(&file)).map_err(|e| e.to_string())?;
            cel_metas.push(CelMeta {
                layer: *layer,
                frame: *frame,
                x: *x,
                y: *y,
                file,
            });
        }
        cel_metas.sort_by_key(|c| (c.layer, c.frame));
        self.meta.cels = cel_metas;
        std::fs::write(
            dir.join("doc.json"),
            serde_json::to_string_pretty(&self.meta).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // -- structure ----------------------------------------------------------

    /// Append a new layer on top; returns its index.
    pub fn add_layer(&mut self, name: Option<String>, opacity: u8, blend: String) -> usize {
        let idx = self.meta.layers.len();
        self.meta.layers.push(LayerMeta {
            name: name.unwrap_or_else(|| format!("Layer {}", idx + 1)),
            opacity,
            visible: true,
            blend,
        });
        idx
    }

    /// Patch a layer's visibility / opacity / blend (each optional).
    pub fn set_layer(
        &mut self,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<(), String> {
        let l = self
            .meta
            .layers
            .get_mut(layer)
            .ok_or_else(|| format!("no layer {}", layer))?;
        if let Some(v) = visible {
            l.visible = v;
        }
        if let Some(o) = opacity {
            l.opacity = o;
        }
        if let Some(b) = blend {
            l.blend = b;
        }
        Ok(())
    }

    /// Append a new frame; with `copy_from`, duplicate that frame's cels into it.
    pub fn add_frame(&mut self, duration_ms: u32, copy_from: Option<usize>) -> usize {
        let idx = self.meta.frames.len();
        self.meta.frames.push(FrameMeta {
            duration_ms,
            pivot: None,
        });
        if let Some(src) = copy_from {
            // duplicate every cel of frame `src` into the new frame
            let to_copy: Vec<(usize, (i32, i32, RgbaImage))> = self
                .cels
                .iter()
                .filter(|((_, f), _)| *f == src)
                .map(|((l, _), v)| (*l, (v.0, v.1, v.2.clone())))
                .collect();
            for (l, v) in to_copy {
                self.cels.insert((l, idx), v);
            }
        }
        idx
    }

    /// Set a frame's display duration in milliseconds.
    pub fn set_frame_duration(&mut self, frame: usize, ms: u32) -> Result<(), String> {
        let f = self
            .meta
            .frames
            .get_mut(frame)
            .ok_or_else(|| format!("no frame {}", frame))?;
        f.duration_ms = ms;
        Ok(())
    }

    /// Set (or clear, with None) a frame's anchor/pivot point.
    pub fn set_pivot(&mut self, frame: usize, pivot: Option<[i32; 2]>) -> Result<(), String> {
        let f = self
            .meta
            .frames
            .get_mut(frame)
            .ok_or_else(|| format!("no frame {}", frame))?;
        f.pivot = pivot;
        Ok(())
    }

    /// Add a named animation tag over an inclusive frame range.
    pub fn add_tag(
        &mut self,
        name: &str,
        from: usize,
        to: usize,
        direction: &str,
    ) -> Result<(), String> {
        if from > to || to >= self.meta.frames.len() {
            return Err(format!(
                "tag range {}..{} out of bounds (frames={})",
                from,
                to,
                self.meta.frames.len()
            ));
        }
        self.meta.tags.push(TagMeta {
            name: name.into(),
            from,
            to,
            direction: direction.into(),
        });
        Ok(())
    }

    fn check_cel(&self, layer: usize, frame: usize) -> Result<(), String> {
        if layer >= self.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        if frame >= self.meta.frames.len() {
            return Err(format!("no frame {}", frame));
        }
        Ok(())
    }

    /// Place (or replace) the cel image for (layer, frame) at offset (x, y).
    pub fn set_cel(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        img: RgbaImage,
    ) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.cels.insert((layer, frame), (x, y, img));
        Ok(())
    }

    /// Remove the cel at (layer, frame), if any.
    pub fn clear_cel(&mut self, layer: usize, frame: usize) {
        self.cels.remove(&(layer, frame));
    }

    // -- palette (indexed-friendly swatch list) -----------------------------

    /// Replace the document's palette swatch list.
    pub fn set_palette(&mut self, colors: Vec<[u8; 4]>) {
        self.meta.palette = colors;
    }

    /// JSON snapshot of the document structure (layers, frames, tags, cels,
    /// palette) for inspection — no pixel data.
    pub fn structure(&self) -> Value {
        let mut cels: Vec<Value> = self
            .cels
            .keys()
            .map(|(l, f)| json!({"layer": l, "frame": f}))
            .collect();
        cels.sort_by_key(|x| {
            (
                x["layer"].as_u64().unwrap_or(0),
                x["frame"].as_u64().unwrap_or(0),
            )
        });
        json!({
            "name": self.meta.name, "w": self.meta.w, "h": self.meta.h,
            "layers": self.meta.layers.iter().enumerate().map(|(i, l)| json!({
                "index": i, "name": l.name, "opacity": l.opacity, "visible": l.visible, "blend": l.blend
            })).collect::<Vec<_>>(),
            "frames": self.meta.frames.iter().enumerate().map(|(i, f)| json!({
                "index": i, "duration_ms": f.duration_ms, "pivot": f.pivot
            })).collect::<Vec<_>>(),
            "tags": self.meta.tags.iter().map(|t| json!({
                "name": t.name, "from": t.from, "to": t.to, "direction": t.direction
            })).collect::<Vec<_>>(),
            "cels": cels,
            "palette": self.meta.palette,
            "palette_len": self.meta.palette.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn palette_set_and_index() {
        let mut d = Document::new("t", 4, 4);
        d.set_palette(vec![[1, 1, 1, 255], [2, 2, 2, 255]]);
        assert_eq!(d.meta.palette.len(), 2);
        assert_eq!(d.meta.palette[1], [2, 2, 2, 255]);
    }

    #[test]
    fn pivot_set_and_clear() {
        let mut d = Document::new("t", 4, 4);
        d.set_pivot(0, Some([2, 3])).unwrap();
        assert_eq!(d.meta.frames[0].pivot, Some([2, 3]));
        d.set_pivot(0, None).unwrap();
        assert_eq!(d.meta.frames[0].pivot, None);
        assert!(d.set_pivot(9, Some([0, 0])).is_err());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("atelier_doc_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut d = Document::new("rt", 4, 4);
        let mut img = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 0]));
        img.put_pixel(0, 0, Rgba([10, 20, 30, 255]));
        d.set_cel(0, 0, 1, 1, img).unwrap();
        d.save(&dir).unwrap();

        let loaded = Document::load(&dir).unwrap();
        assert_eq!(loaded.meta.name, "rt");
        assert_eq!((loaded.meta.w, loaded.meta.h), (4, 4));
        // the cel is recorded in meta at the offset it was placed
        assert_eq!(loaded.meta.cels.len(), 1);
        let c = &loaded.meta.cels[0];
        assert_eq!((c.layer, c.frame, c.x, c.y), (0, 0, 1, 1));
        // the pixel painted into the cel survives the round-trip
        let cel_img = image::open(dir.join(&c.file)).unwrap().to_rgba8();
        assert_eq!(cel_img.get_pixel(0, 0).0, [10, 20, 30, 255]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn structure_reports_layers_frames_and_cels() {
        let mut d = Document::new("s", 4, 4);
        d.add_layer(None, 255, "normal".into());
        let img = RgbaImage::from_pixel(1, 1, Rgba([1, 1, 1, 255]));
        d.set_cel(1, 0, 0, 0, img).unwrap();
        let v = d.structure();
        assert_eq!(v["name"], "s");
        assert_eq!(v["layers"].as_array().unwrap().len(), 2);
        assert_eq!(v["frames"].as_array().unwrap().len(), 1);
        let cels = v["cels"].as_array().unwrap();
        assert_eq!(cels.len(), 1);
        assert_eq!(cels[0]["layer"], 1);
        assert_eq!(cels[0]["frame"], 0);
    }
}
