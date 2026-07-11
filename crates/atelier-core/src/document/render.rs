//! Flattening, previews and read-only analysis of rendered frames.

use image::{Rgba, RgbaImage};

use crate::raster;

use super::{Document, FrameDiff};

impl Document {
    // -- render / export ----------------------------------------------------

    /// Analysis image for a frame: the flattened composite, or one layer's cel
    /// when `layer` is given. The single entry point read-only tools share.
    pub fn analysis_image(&self, layer: Option<usize>, frame: usize) -> Result<RgbaImage, String> {
        if frame >= self.meta.frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                self.meta.frames.len()
            ));
        }
        match layer {
            Some(l) => self.cel_image(l, frame),
            None => Ok(self.flatten(frame)),
        }
    }

    /// Render a frame into analysis space: each opaque pixel becomes a grey level
    /// derived from `mode` (transparency preserved). "grayscale" = luma; "bands" =
    /// luma posterised into `bands` even steps; "saturation"/"hue" = that HSL
    /// channel scaled to 0..255 grey. The shared core behind doc_look's value modes.
    pub fn value_image(&self, frame: usize, mode: &str, bands: u32) -> Result<RgbaImage, String> {
        let src = self.analysis_image(None, frame)?;
        let bands = bands.max(1);
        let mut out = RgbaImage::from_pixel(src.width(), src.height(), Rgba([0, 0, 0, 0]));
        for (x, y, p) in src.enumerate_pixels() {
            let c = p.0;
            if c[3] == 0 {
                continue;
            }
            let g = match mode {
                "grayscale" => raster::luma(c),
                "bands" => {
                    // Posterise luma into `bands` even buckets, spread back to 0..255.
                    let l = raster::luma(c) as u32;
                    let b = (l * bands / 256).min(bands - 1);
                    if bands == 1 {
                        128
                    } else {
                        (b * 255 / (bands - 1)) as u8
                    }
                }
                "saturation" => (raster::saturation(c) * 255.0).round() as u8,
                "hue" => (raster::hue_deg(c) / 360.0 * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8,
                other => {
                    return Err(format!(
                        "unknown value mode '{}' — use grayscale|bands|saturation|hue",
                        other
                    ))
                }
            };
            out.put_pixel(x, y, Rgba([g, g, g, c[3]]));
        }
        Ok(out)
    }

    pub fn flatten(&self, frame: usize) -> RgbaImage {
        let mut out = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba([0, 0, 0, 0]));
        for (li, layer) in self.meta.layers.iter().enumerate() {
            if !layer.visible || layer.opacity == 0 {
                continue;
            }
            if let Some((cx, cy, img)) = self.cels.get(&(li, frame)) {
                raster::composite(
                    &mut out,
                    img,
                    *cx,
                    *cy,
                    layer.opacity,
                    raster::parse_blend(&layer.blend),
                );
            }
        }
        out
    }

    /// Flatten `frame` with onion-skin ghosts of the neighbours behind it: the
    /// previous frame tinted blue and the next tinted red, both faded, so motion
    /// is visible at a glance.
    fn flatten_onion(&self, frame: usize) -> RgbaImage {
        let n = self.meta.frames.len();
        let mut out = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba([0, 0, 0, 0]));
        let ghost = |src: RgbaImage, tint: [u8; 3]| -> RgbaImage {
            let mut g = src;
            for p in g.pixels_mut() {
                if p.0[3] == 0 {
                    continue;
                }
                let mix = |i: usize| ((p.0[i] as u32 + tint[i] as u32 * 2) / 3) as u8;
                p.0 = [mix(0), mix(1), mix(2), (p.0[3] as u32 * 90 / 255) as u8];
            }
            g
        };
        if frame > 0 {
            let g = ghost(self.flatten(frame - 1), [70, 110, 255]);
            raster::composite(&mut out, &g, 0, 0, 255, raster::Blend::Normal);
        }
        if frame + 1 < n {
            let g = ghost(self.flatten(frame + 1), [255, 90, 90]);
            raster::composite(&mut out, &g, 0, 0, 255, raster::Blend::Normal);
        }
        let cur = self.flatten(frame);
        raster::composite(&mut out, &cur, 0, 0, 255, raster::Blend::Normal);
        out
    }

    /// Render a frame to an image with preview options: `onion` ghosts the
    /// neighbours; `region` crops (document pixels); `tile` repeats the result
    /// in an N×N grid (seam check); `scale` nearest-upscales; `max_size`
    /// down-scales the longest side for a cheap thumbnail.
    pub fn render_preview(
        &self,
        frame: usize,
        scale: u32,
        region: Option<(i32, i32, i32, i32)>,
        onion: bool,
        tile: u32,
        max_size: Option<u32>,
    ) -> Result<RgbaImage, String> {
        let mut base = if onion {
            self.flatten_onion(frame)
        } else {
            self.flatten(frame)
        };
        if let Some((x0, y0, x1, y1)) = region {
            let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, self.meta.w, self.meta.h)
                .ok_or("render region is empty after clamping")?;
            base = image::imageops::crop_imm(
                &base,
                ax as u32,
                ay as u32,
                (bx - ax + 1) as u32,
                (by - ay + 1) as u32,
            )
            .to_image();
        }
        let tile = tile.max(1);
        if tile > 1 {
            let (tw, th) = (base.width(), base.height());
            let mut t = RgbaImage::from_pixel(tw * tile, th * tile, Rgba([0, 0, 0, 0]));
            for ty in 0..tile {
                for tx in 0..tile {
                    image::imageops::replace(&mut t, &base, (tx * tw) as i64, (ty * th) as i64);
                }
            }
            base = t;
        }
        let sc = scale.max(1);
        if sc > 1 {
            base = image::imageops::resize(
                &base,
                base.width() * sc,
                base.height() * sc,
                image::imageops::FilterType::Nearest,
            );
        }
        if let Some(ms) = max_size {
            let long = base.width().max(base.height());
            if long > ms && long > 0 {
                let f = ms as f32 / long as f32;
                let nw = (base.width() as f32 * f).round().max(1.0) as u32;
                let nh = (base.height() as f32 * f).round().max(1.0) as u32;
                base = image::imageops::resize(&base, nw, nh, image::imageops::FilterType::Nearest);
            }
        }
        Ok(base)
    }

    // -- animation & tiling feedback (read-only diff/seam primitives) --------

    /// Per-pixel diff of two frames over an (already clamped) region. `layer`
    /// None diffs the flattened composite, else that cel. Classifies each pixel
    /// as unchanged / added (transparent→opaque) / removed (opaque→transparent)
    /// or recoloured (opaque→different opaque), tallying counts and the bbox of
    /// every changed pixel. Returns `(added, removed, recolored, change_bbox,
    /// img_a, img_b)` so callers can also build a text grid or overlay render.
    pub fn frame_diff_region(
        &self,
        frame_a: usize,
        frame_b: usize,
        layer: Option<usize>,
        region: (i32, i32, i32, i32),
    ) -> Result<FrameDiff, String> {
        let a = self.analysis_image(layer, frame_a)?;
        let b = self.analysis_image(layer, frame_b)?;
        let (x0, y0, x1, y1) = region;
        let (mut added, mut removed, mut recolored) = (0u32, 0u32, 0u32);
        let mut bbox: Option<[i32; 4]> = None;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let pa = a.get_pixel(x as u32, y as u32).0;
                let pb = b.get_pixel(x as u32, y as u32).0;
                if pa == pb {
                    continue;
                }
                match (pa[3] > 0, pb[3] > 0) {
                    (false, true) => added += 1,
                    (true, false) => removed += 1,
                    _ => recolored += 1, // both opaque (different) — recolour
                }
                bbox = Some(match bbox {
                    Some([ax, ay, bx, by]) => [ax.min(x), ay.min(y), bx.max(x), by.max(y)],
                    None => [x, y, x, y],
                });
            }
        }
        Ok((added, removed, recolored, bbox, a, b))
    }

    /// Wrap-test one tiling axis: compare the far edge against the near edge that
    /// would abut it when the cel repeats. `horizontal` true tests column x=w-1
    /// vs x=0 (left/right tiling), false tests row y=h-1 vs y=0 (top/bottom).
    /// `threshold` is the max per-channel delta still counted as a match. Returns
    /// `(mismatches, max_delta, worst)` where `worst` is up to 10 `[x,y,delta]`
    /// edge cells sorted by descending delta (the position is the far-edge cell).
    pub fn seam_axis(
        &self,
        layer: Option<usize>,
        frame: usize,
        horizontal: bool,
        threshold: i32,
    ) -> Result<(u32, i32, Vec<[i32; 3]>), String> {
        let img = self.analysis_image(layer, frame)?;
        Ok(seam_axis_img(&img, horizontal, threshold))
    }
}

/// [`Document::seam_axis`] over an already-flattened frame — callers testing
/// several axes (or also rendering an overlay) flatten once and reuse it.
pub fn seam_axis_img(
    img: &RgbaImage,
    horizontal: bool,
    threshold: i32,
) -> (u32, i32, Vec<[i32; 3]>) {
    {
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut mismatches = 0u32;
        let mut max_delta = 0i32;
        let mut all: Vec<[i32; 3]> = Vec::new();
        // The pairs of (far-edge, near-edge) cells along the seam.
        let pairs: Vec<((i32, i32), (i32, i32))> = if horizontal {
            (0..h).map(|y| ((w - 1, y), (0, y))).collect()
        } else {
            (0..w).map(|x| ((x, h - 1), (x, 0))).collect()
        };
        for ((fx, fy), (nx, ny)) in pairs {
            let pf = img.get_pixel(fx as u32, fy as u32).0;
            let pn = img.get_pixel(nx as u32, ny as u32).0;
            let delta = (0..4)
                .map(|c| (pf[c] as i32 - pn[c] as i32).abs())
                .max()
                .unwrap();
            if delta > threshold {
                mismatches += 1;
                max_delta = max_delta.max(delta);
                all.push([fx, fy, delta]);
            }
        }
        all.sort_by(|a, b| b[2].cmp(&a[2]));
        all.truncate(10);
        (mismatches, max_delta, all)
    }
}

impl Document {
    /// Opaque-mass CENTROID of a frame's silhouette (mean of opaque pixel
    /// coordinates), optionally clipped to `region`. A mass centroid, unlike
    /// the old bbox-corner midpoint, actually moves when a limb swings over a
    /// static torso — and the region clip makes one part's motion measurable
    /// on its own.
    pub fn silhouette_center(
        &self,
        layer: Option<usize>,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Option<[f64; 2]>, String> {
        Ok(self.silhouette_stats(layer, frame, region)?.map(|(c, _)| c))
    }

    /// One-flatten combination of `silhouette_center` and the full-frame
    /// opaque count — the per-frame pair the animation audits read, without
    /// flattening the same frame twice. The count is whole-frame (matching
    /// `opaque_count`); only the centroid is clipped to `region`.
    pub fn silhouette_stats(
        &self,
        layer: Option<usize>,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Option<([f64; 2], u64)>, String> {
        let img = self.analysis_image(layer, frame)?;
        let (mut sx, mut sy, mut n, mut opaque) = (0f64, 0f64, 0u64, 0u64);
        for (x, y, p) in img.enumerate_pixels() {
            if p.0[3] == 0 {
                continue;
            }
            opaque += 1;
            if let Some((x0, y0, x1, y1)) = region {
                let (xi, yi) = (x as i32, y as i32);
                if xi < x0.min(x1) || xi > x0.max(x1) || yi < y0.min(y1) || yi > y0.max(y1) {
                    continue;
                }
            }
            sx += x as f64;
            sy += y as f64;
            n += 1;
        }
        Ok((n > 0).then(|| ([sx / n as f64, sy / n as f64], opaque)))
    }

    /// Count opaque pixels in a frame (denominator for the seam loop score).
    pub fn opaque_count(&self, layer: Option<usize>, frame: usize) -> Result<u64, String> {
        let img = self.analysis_image(layer, frame)?;
        Ok(img.pixels().filter(|p| p.0[3] > 0).count() as u64)
    }
}
