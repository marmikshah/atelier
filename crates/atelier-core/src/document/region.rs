//! Region clipboard, transforms and selection helpers.

use image::{Rgba, RgbaImage};

use crate::raster;
use crate::raster::resolve_region;

use super::Document;

impl Document {
    /// Copy a rectangular region of a cel as a flat RGBA buffer (w*h*4),
    /// returned with its width/height. Out-of-cel pixels come back transparent.
    /// The rect is given as inclusive corners and normalised/clamped to canvas.
    pub fn copy_region(
        &self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        self.check_cel(layer, frame)?;
        let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, self.meta.w, self.meta.h)
            .ok_or("region is empty after clamping to the canvas")?;
        let (rw, rh) = ((bx - ax + 1) as u32, (by - ay + 1) as u32);
        let mut buf = vec![0u8; (rw * rh * 4) as usize];
        // One cel lookup for the whole rect — get_pixel would re-probe the cel
        // map (and re-check the cel) for every pixel.
        if let Some((cx, cy, img)) = self.cels.get(&(layer, frame)) {
            for ry in 0..rh as i32 {
                for rx in 0..rw as i32 {
                    let (lx, ly) = (ax + rx - cx, ay + ry - cy);
                    if lx < 0 || ly < 0 || lx as u32 >= img.width() || ly as u32 >= img.height() {
                        continue;
                    }
                    let p = img.get_pixel(lx as u32, ly as u32).0;
                    let i = ((ry as u32 * rw + rx as u32) * 4) as usize;
                    buf[i..i + 4].copy_from_slice(&p);
                }
            }
        }
        Ok((rw, rh, buf))
    }

    /// Paste a flat RGBA buffer onto a cel at (x,y). `blend` true = source-over
    /// (transparent source pixels keep the destination); false = overwrite
    /// (copy every pixel including transparency, so it also erases).
    pub fn paste_region(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        rw: u32,
        rh: u32,
        buf: &[u8],
        blend: bool,
    ) -> Result<(), String> {
        if buf.len() != (rw * rh * 4) as usize {
            return Err(format!("buffer length {} != {}x{}x4", buf.len(), rw, rh));
        }
        let img = self.cel_canvas(layer, frame)?;
        for ry in 0..rh as i32 {
            for rx in 0..rw as i32 {
                let i = ((ry as u32 * rw + rx as u32) * 4) as usize;
                let p = [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]];
                if blend && p[3] == 0 {
                    continue;
                }
                raster::put(img, x + rx, y + ry, p);
            }
        }
        Ok(())
    }

    /// Erase a rectangular region of a cel (set to transparent).
    pub fn clear_region(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (ax, bx) = (x0.min(x1), x0.max(x1));
        let (ay, by) = (y0.min(y1), y0.max(y1));
        for y in ay..=by {
            for x in ax..=bx {
                raster::put(img, x, y, [0, 0, 0, 0]);
            }
        }
        Ok(())
    }

    /// Move a region within one cel by (dx,dy): copy it, clear the source, paste
    /// at the offset. Source-over paste so transparent pixels in the moved block
    /// do NOT punch a rectangular hole through the art already at the
    /// destination (the limb-nudge footgun) — only opaque source pixels write.
    pub fn move_region(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        dx: i32,
        dy: i32,
    ) -> Result<(), String> {
        let (rw, rh, buf) = self.copy_region(layer, frame, x0, y0, x1, y1)?;
        let (ax, ay) = (x0.min(x1).max(0), y0.min(y1).max(0));
        self.clear_region(layer, frame, x0, y0, x1, y1)?;
        self.paste_region(layer, frame, ax + dx, ay + dy, rw, rh, &buf, true)
    }

    /// Affine-transform a cel (or a `region` of it) in place about its centre:
    /// rotate `rot` degrees, scale (`sx`,`sy`), shear (`skew_x`,`skew_y` deg).
    /// `method` `"rotsprite"` super-samples to keep clusters from shattering;
    /// `"nearest"` is the raw grid transform. `clear_source` empties the source
    /// rect first (a true move rather than an overlay). Returns
    /// `(placed_bbox [x,y,w,h], placed_opaque_px)`.
    pub fn transform_cel(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        rot: f32,
        sx: f32,
        sy: f32,
        skew_x: f32,
        skew_y: f32,
        method: &str,
        clear_source: bool,
    ) -> Result<([i32; 4], u32), String> {
        self.check_cel(layer, frame)?;
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = resolve_region(region, w, h)?;
        let (rw, rh) = ((bx - ax + 1) as u32, (by - ay + 1) as u32);
        // Lift the source rect to a standalone image.
        let full = self.cel_image(layer, frame)?;
        let sub = image::imageops::crop_imm(&full, ax as u32, ay as u32, rw, rh).to_image();
        let ss = match method {
            "rotsprite" => 4,
            "nearest" => 1,
            other => return Err(format!("unknown method '{other}' — use rotsprite|nearest")),
        };
        let out = raster::affine_nn(&sub, rot, sx, sy, skew_x, skew_y, ss);
        let (tw, th) = (out.width(), out.height());
        if clear_source {
            self.clear_region(layer, frame, ax, ay, bx, by)?;
        }
        // Centre the result on the source rect's centre.
        let cx = ax as f32 + rw as f32 / 2.0;
        let cy = ay as f32 + rh as f32 / 2.0;
        let px = (cx - tw as f32 / 2.0).round() as i32;
        let py = (cy - th as f32 / 2.0).round() as i32;
        let placed_px = out.pixels().filter(|p| p.0[3] > 0).count() as u32;
        let buf = out.into_raw();
        self.paste_region(layer, frame, px, py, tw, th, &buf, true)?;
        Ok(([px, py, tw as i32, th as i32], placed_px))
    }

    /// Flood a contiguous same-colour region from `(x,y)` into a row-major
    /// boolean mask the size of the canvas — the magic-wand. `layer` None reads
    /// the flattened composite. `perceptual` uses OKLab ΔE (`tol` as a 0..255
    /// scale) instead of raw channel distance. `conn8` uses 8-connectivity.
    pub fn flood_mask(
        &self,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
        tol: i32,
        conn8: bool,
        perceptual: bool,
    ) -> Result<Vec<bool>, String> {
        let img = self.analysis_image(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut mask = vec![false; (w * h) as usize];
        if x < 0 || y < 0 || x >= w || y >= h {
            return Ok(mask);
        }
        let target = img.get_pixel(x as u32, y as u32).0;
        let de = tol as f32 / 255.0;
        let matches = |p: [u8; 4]| -> bool {
            // Never flood across the opaque/transparent boundary: the perceptual
            // ΔE ignores alpha, so without this a wand on a black fill would
            // bleed into a transparent black background (ΔE 0).
            if (p[3] == 0) != (target[3] == 0) {
                return false;
            }
            if perceptual {
                raster::oklab_delta(p, target) <= de
            } else {
                raster::close(p, target, tol)
            }
        };
        let mut stack = vec![(x, y)];
        while let Some((px, py)) = stack.pop() {
            if px < 0 || py < 0 || px >= w || py >= h {
                continue;
            }
            let i = (py * w + px) as usize;
            if mask[i] {
                continue;
            }
            if !matches(img.get_pixel(px as u32, py as u32).0) {
                continue;
            }
            mask[i] = true;
            stack.push((px + 1, py));
            stack.push((px - 1, py));
            stack.push((px, py + 1));
            stack.push((px, py - 1));
            if conn8 {
                stack.push((px + 1, py + 1));
                stack.push((px - 1, py - 1));
                stack.push((px + 1, py - 1));
                stack.push((px - 1, py + 1));
            }
        }
        Ok(mask)
    }

    /// Cut a region (and/or selection-masked pixels) of `layer` onto its OWN
    /// new layer directly above, same coordinates — converts a flat sprite
    /// into part layers (arm, head, tail) that keyframe_transform can move
    /// independently. `all_frames` cuts every frame's cel, keeping the part
    /// aligned across the timeline. Returns `(new_layer_index, pixels_moved)`.
    pub fn extract_to_layer(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        mask: Option<&[bool]>,
        name: Option<String>,
        all_frames: bool,
    ) -> Result<(usize, u64), String> {
        if layer >= self.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        if region.is_none() && mask.is_none() {
            return Err("extract needs a `region` or an active selection".into());
        }
        if !all_frames && frame >= self.meta.frames.len() {
            return Err(format!("no frame {}", frame));
        }
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let norm = region.map(|(x0, y0, x1, y1)| (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
        let in_scope = |x: i32, y: i32| -> bool {
            if let Some((ax, ay, bx, by)) = norm {
                if x < ax || x > bx || y < ay || y > by {
                    return false;
                }
            }
            match mask {
                Some(m) => m.get((y * w + x) as usize).copied() == Some(true),
                None => true,
            }
        };
        let new_layer = layer + 1;
        self.insert_layer(
            new_layer,
            name.or_else(|| Some("part".into())),
            255,
            "normal".into(),
        );
        let frames: Vec<usize> = if all_frames {
            (0..self.meta.frames.len()).collect()
        } else {
            vec![frame]
        };
        let mut moved_total = 0u64;
        for f in frames {
            if !self.cels.contains_key(&(layer, f)) {
                continue;
            }
            let full = self.cel_full(layer, f);
            let mut part = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
            let mut rest = full.clone();
            let mut moved = 0u64;
            for y in 0..h {
                for x in 0..w {
                    let p = *full.get_pixel(x as u32, y as u32);
                    if p.0[3] > 0 && in_scope(x, y) {
                        part.put_pixel(x as u32, y as u32, p);
                        rest.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
                        moved += 1;
                    }
                }
            }
            if moved > 0 {
                self.set_cel(layer, f, 0, 0, rest)?;
                self.set_cel(new_layer, f, 0, 0, part)?;
                moved_total += moved;
            }
        }
        Ok((new_layer, moved_total))
    }
}
