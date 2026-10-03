//! Engine-ready exports: spritesheet (+JSON sidecars), GIF and APNG.

use std::path::Path;

use image::{Rgba, RgbaImage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::raster;

use super::Document;

/// PNG channel layout. RGB exports require a fully opaque rendered sheet.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PngColorMode {
    #[default]
    Rgba,
    Rgb,
}

impl PngColorMode {
    const fn pixel_format(self) -> &'static str {
        match self {
            Self::Rgba => "RGBA8888",
            Self::Rgb => "RGB888",
        }
    }
}

fn save_sheet_png(sheet: &RgbaImage, out: &Path, color_mode: PngColorMode) -> Result<(), String> {
    // Validate before opening the output so a failed RGB export cannot replace
    // a previously exported asset or leave a partial PNG behind.
    let rgb = if color_mode == PngColorMode::Rgb {
        if let Some((x, y, pixel)) = sheet.enumerate_pixels().find(|(_, _, p)| p.0[3] != 255) {
            return Err(format!(
                "RGB PNG export requires fully opaque pixels; pixel ({x}, {y}) has alpha {}",
                pixel.0[3]
            ));
        }
        Some(
            sheet
                .as_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| p[..3].iter().copied())
                .collect::<Vec<u8>>(),
        )
    } else {
        None
    };
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut encoder =
        png::Encoder::new(std::io::BufWriter::new(file), sheet.width(), sheet.height());
    encoder.set_color(match color_mode {
        PngColorMode::Rgba => png::ColorType::Rgba,
        PngColorMode::Rgb => png::ColorType::Rgb,
    });
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(rgb.as_deref().unwrap_or(sheet.as_raw()))
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

impl Document {
    /// Render the horizontal spritesheet image (every frame side by side,
    /// nearest-neighbour scaled). Returns `(sheet, frame_w, frame_h)`, or an
    /// error if the strip's dimensions overflow `u32` — a many-frames × high
    /// scale sheet can otherwise wrap to a garbage-sized buffer.
    pub(super) fn sheet_image(&self, scale: u32) -> Result<(RgbaImage, u32, u32), String> {
        let n = self.meta.frames.len() as u64;
        let scale = scale.max(1) as u64;
        let frame_w = self.meta.w as u64 * scale;
        let frame_h = self.meta.h as u64 * scale;
        let strip_w = frame_w
            .checked_mul(n)
            .ok_or_else(|| format!("spritesheet width overflows: {n} frames × {frame_w}px"))?;
        let (strip_w, fh) = raster::checked_rgba_dimensions("spritesheet", strip_w, frame_h)?;
        let fw = u32::try_from(frame_w).map_err(|_| "spritesheet frame width overflows")?;
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

    /// Export a PNG sheet with native metadata, optionally requiring RGB.
    pub fn export_sheet_with_color_mode(
        &self,
        out: &Path,
        scale: u32,
        color_mode: PngColorMode,
    ) -> Result<Value, String> {
        let n = self.meta.frames.len();
        let (sheet, fw, fh) = self.sheet_image(scale)?;
        save_sheet_png(&sheet, out, color_mode)?;
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
        let meta = json!({
            "path": out.to_string_lossy(), "frame_w": fw, "frame_h": fh,
            "count": n, "frames": frames, "tags": tags, "palette": self.meta.palette,
            "color_mode": color_mode, "format": color_mode.pixel_format(),
        });
        let mp = out.with_extension("json");
        std::fs::write(
            &mp,
            serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(meta)
    }

    /// Export a PNG sheet with the standard hash sprite-JSON sidecar used by
    /// game engines, optionally requiring RGB channels.
    pub fn export_sheet_std_with_color_mode(
        &self,
        out: &Path,
        scale: u32,
        color_mode: PngColorMode,
    ) -> Result<Value, String> {
        let (sheet, fw, fh) = self.sheet_image(scale)?;
        save_sheet_png(&sheet, out, color_mode)?;
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
        let meta = json!({
            "frames": frames,
            "meta": {
                "app": "atelier",
                "version": env!("CARGO_PKG_VERSION"),
                "image": image_name,
                "format": color_mode.pixel_format(),
                "size": {"w": sheet.width(), "h": sheet.height()},
                "scale": "1",
                "frameTags": frame_tags,
            },
        });
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
            "color_mode": color_mode,
        }))
    }

    /// Export an animated GIF. Plays `tag`'s sequence (honouring direction) or
    /// the whole timeline when `tag` is None. Returns the number of frames
    /// actually emitted (a pingpong tag emits more than its source range).
    pub fn export_gif(&self, out: &Path, scale: u32, tag: Option<&str>) -> Result<usize, String> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let seq = self.play_sequence(tag)?;
        let scale = scale.max(1) as u64;
        let (frame_w, frame_h) = raster::checked_rgba_dimensions(
            "GIF frame",
            self.meta.w as u64 * scale,
            self.meta.h as u64 * scale,
        )?;
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
                    if p.0[3] > 0
                        && let Some(i) = lab.nearest(p.0)
                    {
                        let c = lab.color(i);
                        *p = Rgba([c[0], c[1], c[2], p.0[3]]);
                    }
                }
            }
            if scale > 1 {
                img = image::imageops::resize(
                    &img,
                    frame_w,
                    frame_h,
                    image::imageops::FilterType::Nearest,
                );
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
        let sc = scale.max(1) as u64;
        let (w, h) = raster::checked_rgba_dimensions(
            "APNG frame",
            self.meta.w as u64 * sc,
            self.meta.h as u64 * sc,
        )?;
        let frame_count = u32::try_from(seq.len())
            .map_err(|_| format!("APNG has too many frames: {}", seq.len()))?;
        let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(frame_count, 0)
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
