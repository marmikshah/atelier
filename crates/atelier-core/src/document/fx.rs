//! Effects that rework existing pixels — shading, lighting, texture, cleanup.

use image::{Rgba, RgbaImage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::raster;
use crate::raster::resolve_region;

use super::Document;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DitherAxis {
    #[serde(rename = "h")]
    Horizontal,
    #[default]
    #[serde(rename = "v")]
    Vertical,
    #[serde(rename = "radial")]
    Radial,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DitherPattern {
    #[serde(rename = "bayer2")]
    Bayer2,
    #[default]
    #[serde(rename = "bayer4")]
    Bayer4,
    #[serde(rename = "bayer8")]
    Bayer8,
    #[serde(rename = "checker")]
    Checker,
    #[serde(rename = "ign")]
    Ign,
}

impl DitherPattern {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bayer2 => "bayer2",
            Self::Bayer4 => "bayer4",
            Self::Bayer8 => "bayer8",
            Self::Checker => "checker",
            Self::Ign => "ign",
        }
    }
}

impl Document {
    /// Graduated multi-tone dithering across a whole ramp along an axis — master
    /// gradient shading, vs the two-colour Bayer of `dither`. For each pixel the
    /// position `t` along `axis` (`h`|`v`|`radial`) picks a ramp pair; an ordered
    /// or blue-noise (`ign`) threshold dithers between them. `only_existing`
    /// repaints just the opaque pixels (shade existing art) and keeps their
    /// alpha. Returns the pixel count changed.
    pub fn dither_ramp(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        ramp: &[[u8; 4]],
        axis: DitherAxis,
        pattern: DitherPattern,
        only_existing: bool,
    ) -> Result<u32, String> {
        if ramp.len() < 2 {
            return Err("dither_ramp needs a ramp of >= 2 colours".into());
        }
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = resolve_region(region, w, h)?;
        let span_x = (bx - ax).max(1) as f32;
        let span_y = (by - ay).max(1) as f32;
        let (cx, cy) = ((ax + bx) as f32 / 2.0, (ay + by) as f32 / 2.0);
        let rmax = ((span_x / 2.0).powi(2) + (span_y / 2.0).powi(2))
            .sqrt()
            .max(1.0);
        let last = ramp.len() - 1;
        let img = self.cel_canvas(layer, frame)?;
        let mut changed = 0;
        for y in ay..=by {
            for x in ax..=bx {
                let cur = img.get_pixel(x as u32, y as u32).0;
                if only_existing && cur[3] == 0 {
                    continue;
                }
                let t = match axis {
                    DitherAxis::Vertical => (y - ay) as f32 / span_y,
                    DitherAxis::Radial => {
                        ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / rmax
                    }
                    DitherAxis::Horizontal => (x - ax) as f32 / span_x,
                }
                .clamp(0.0, 1.0);
                let pos = t * last as f32;
                let k = pos.floor() as usize;
                let frac = pos - k as f32;
                let thr = raster::ramp_dither_threshold(pattern.as_str(), x, y);
                let idx = if frac > thr {
                    (k + 1).min(last)
                } else {
                    k.min(last)
                };
                let c = ramp[idx];
                let out = [c[0], c[1], c[2], if only_existing { cur[3] } else { 255 }];
                if out != cur {
                    img.put_pixel(x as u32, y as u32, Rgba(out));
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }

    /// Draw a 1px outline around the opaque pixels of a cel. `aa` also softens
    /// the diagonal-only corner pixels (reduced alpha) so the outline reads as
    /// anti-aliased instead of stair-stepped.
    pub fn outline_cel(
        &mut self,
        layer: usize,
        frame: usize,
        color: [u8; 4],
        aa: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let opaque = |img: &RgbaImage, x: i32, y: i32| {
            x >= 0 && y >= 0 && x < w && y < h && img.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let n4 = [(-1, 0), (1, 0), (0, -1), (0, 1)];
        let nd = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
        let mut writes: Vec<(i32, i32, [u8; 4])> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if opaque(img, x, y) {
                    continue;
                }
                if n4.iter().any(|(ox, oy)| opaque(img, x + ox, y + oy)) {
                    writes.push((x, y, color));
                } else if aa && nd.iter().any(|(ox, oy)| opaque(img, x + ox, y + oy)) {
                    let a = (color[3] as u32 * 110 / 255) as u8; // soft corner
                    writes.push((x, y, [color[0], color[1], color[2], a]));
                }
            }
        }
        for (x, y, c) in writes {
            img.put_pixel(x as u32, y as u32, Rgba(c));
        }
        Ok(())
    }

    /// Project a coloured drop shadow of the cel's opaque silhouette offset by
    /// (dx,dy), at `opacity`, optionally `blur`red, and composite the original
    /// art back on top. Self-contained on one cel.
    pub fn drop_shadow(
        &mut self,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        color: [u8; 4],
        opacity: u8,
        blur: i32,
    ) -> Result<(), String> {
        let orig = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (orig.width(), orig.height());
        let mut shadow = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        let scale = (opacity as f32 / 255.0) * (color[3] as f32 / 255.0);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let a = orig.get_pixel(x as u32, y as u32).0[3];
                let (sx, sy) = (x + dx, y + dy);
                if a > 0 && sx >= 0 && sy >= 0 && (sx as u32) < w && (sy as u32) < h {
                    let alpha = (a as f32 * scale).round().clamp(0.0, 255.0) as u8;
                    shadow.put_pixel(
                        sx as u32,
                        sy as u32,
                        Rgba([color[0], color[1], color[2], alpha]),
                    );
                }
            }
        }
        let mut out = raster::box_blur(&shadow, blur);
        raster::composite(&mut out, &orig, 0, 0, 255, raster::Blend::Normal);
        *self.cel_canvas(layer, frame)? = out;
        Ok(())
    }

    /// Bloom: blur a bright copy of the cel and composite it back through a
    /// light blend (`mode`, e.g. screen/add) at `intensity`. `color` (None =
    /// the art's own colours) tints the glow. Self-contained on one cel.
    pub fn glow(
        &mut self,
        layer: usize,
        frame: usize,
        color: Option<[u8; 4]>,
        radius: i32,
        intensity: u8,
        blend: raster::Blend,
    ) -> Result<(), String> {
        let orig = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (orig.width(), orig.height());
        let mut src = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        for y in 0..h {
            for x in 0..w {
                let p = orig.get_pixel(x, y).0;
                if p[3] > 0 {
                    let c = color.unwrap_or(p);
                    src.put_pixel(x, y, Rgba([c[0], c[1], c[2], p[3]]));
                }
            }
        }
        let g = raster::box_blur(&src, radius.max(1));
        let mut out = orig.clone();
        raster::composite(&mut out, &g, 0, 0, intensity, blend);
        *self.cel_canvas(layer, frame)? = out;
        Ok(())
    }

    /// Fake-3D bevel: lighten the top/left edge band and darken the bottom/right
    /// band of the opaque shape (each within `depth` pixels of a silhouette
    /// edge), giving raised volume. `light`/`dark` carry their own alpha as the
    /// effect strength.
    pub fn bevel(
        &mut self,
        layer: usize,
        frame: usize,
        light: [u8; 4],
        dark: [u8; 4],
        depth: i32,
    ) -> Result<(), String> {
        let orig = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (orig.width() as i32, orig.height() as i32);
        // Bands deeper than the shape itself change nothing — clamp the raw
        // caller value instead of looping to i32::MAX per pixel.
        let depth = depth.max(1).min(w.max(h));
        let opaque = |x: i32, y: i32| {
            x >= 0 && y >= 0 && x < w && y < h && orig.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let img = self.cel_canvas(layer, frame)?;
        for y in 0..h {
            for x in 0..w {
                if !opaque(x, y) {
                    continue;
                }
                let mut lit = false;
                let mut shd = false;
                for d in 1..=depth {
                    if !opaque(x, y - d) || !opaque(x - d, y) {
                        lit = true;
                    }
                    if !opaque(x, y + d) || !opaque(x + d, y) {
                        shd = true;
                    }
                }
                let base = orig.get_pixel(x as u32, y as u32).0;
                let np = match (lit, shd) {
                    (true, false) => raster::composite_px(
                        base,
                        light,
                        light[3] as f32 / 255.0,
                        raster::Blend::Normal,
                    ),
                    (false, true) => raster::composite_px(
                        base,
                        dark,
                        dark[3] as f32 / 255.0,
                        raster::Blend::Normal,
                    ),
                    _ => base,
                };
                img.put_pixel(x as u32, y as u32, Rgba(np));
            }
        }
        Ok(())
    }

    /// Box-blur a cel by `radius` (premultiplied, so no dark haloes), optionally
    /// limited to `region`. Soft shadows, depth-of-field, smoke.
    pub fn blur(
        &mut self,
        layer: usize,
        frame: usize,
        radius: i32,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let blurred = raster::box_blur(img, radius);
        match region {
            None => *img = blurred,
            Some((x0, y0, x1, y1)) => {
                let (w, h) = (img.width() as i32, img.height() as i32);
                let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
                let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
                for y in ay..=by {
                    for x in ax..=bx {
                        img.put_pixel(x as u32, y as u32, *blurred.get_pixel(x as u32, y as u32));
                    }
                }
            }
        }
        Ok(())
    }

    /// Paint a colour gradient over a cel. `kind` "linear" runs the axis
    /// (x0,y0)->(x1,y1); "radial" centres at (x0,y0) with (x1,y1) on the rim.
    /// `stops` are (pos 0..1, RGBA); `dither` "bayer"/"noise" gives a band-free
    /// pixel-art look (else smooth lerp). `region` (inclusive corners) clips the
    /// paint; `blend` true composites over existing pixels (so stop alpha is a
    /// real falloff — vignettes, light), false overwrites.
    pub fn gradient(
        &mut self,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        mut stops: Vec<(f32, [u8; 4])>,
        dither: &str,
        seed: u64,
        region: Option<(i32, i32, i32, i32)>,
        blend: bool,
    ) -> Result<(), String> {
        if stops.is_empty() {
            return Err("gradient needs at least one stop".into());
        }
        stops.iter_mut().for_each(|s| s.0 = s.0.clamp(0.0, 1.0));
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (rx0, ry0, rx1, ry1) = match region {
            // A region fully off-canvas is a silent no-op, not an error.
            Some((a, b, c, d)) => {
                match raster::clamp_region(a, b, c, d, self.meta.w, self.meta.h) {
                    Some(r) => r,
                    None => return Ok(()),
                }
            }
            None => (0, 0, self.meta.w as i32 - 1, self.meta.h as i32 - 1),
        };
        let radial = kind == "radial";
        let (ax, ay) = (x0 as f32, y0 as f32);
        let (dx, dy) = (x1 as f32 - ax, y1 as f32 - ay);
        let len2 = dx * dx + dy * dy;
        let radius = len2.sqrt();
        let img = self.cel_canvas(layer, frame)?;
        for py in ry0..=ry1 {
            for px in rx0..=rx1 {
                let (fx, fy) = (px as f32, py as f32);
                let t = if radial {
                    if radius <= 0.0 {
                        0.0
                    } else {
                        (((fx - ax).powi(2) + (fy - ay).powi(2)).sqrt() / radius).clamp(0.0, 1.0)
                    }
                } else if len2 <= 0.0 {
                    0.0
                } else {
                    (((fx - ax) * dx + (fy - ay) * dy) / len2).clamp(0.0, 1.0)
                };
                let c = raster::sample_gradient(&stops, t, dither, px, py, seed);
                let (ux, uy) = (px as u32, py as u32);
                let out = if blend {
                    raster::over(img.get_pixel(ux, uy).0, c)
                } else {
                    c
                };
                img.put_pixel(ux, uy, Rgba(out));
            }
        }
        Ok(())
    }

    /// Scatter pixels of random `colors` across a region at `density` (0..1
    /// probability per pixel), deterministic for a given `seed`. `size` is a
    /// square dot. Organic grass/foliage/dust/stars without hand-listing each
    /// speckle. Source-over so alpha colours layer onto existing art.
    pub fn scatter(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        colors: &[[u8; 4]],
        density: f32,
        seed: u64,
        size: i32,
    ) -> Result<(), String> {
        if colors.is_empty() {
            return Err("scatter needs at least one colour".into());
        }
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
        let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
        let d = density.clamp(0.0, 1.0);
        // A dot larger than the canvas paints it solid — clamp the raw caller
        // value so a size×size inner loop can't explode.
        let s = size.max(1).min(w.max(h));
        let o = s / 2;
        let img = self.cel_canvas(layer, frame)?;
        for py in ay..=by {
            for px in ax..=bx {
                if (raster::hash2(px, py, seed) as f32 / u32::MAX as f32) >= d {
                    continue;
                }
                let c = colors[raster::hash2(px, py, seed ^ 0xA5A5_5A5A) as usize % colors.len()];
                for ddy in 0..s {
                    for ddx in 0..s {
                        let (tx, ty) = (px - o + ddx, py - o + ddy);
                        if tx >= 0 && ty >= 0 && tx < w && ty < h {
                            let (ux, uy) = (tx as u32, ty as u32);
                            img.put_pixel(ux, uy, Rgba(raster::over(img.get_pixel(ux, uy).0, c)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Fill a region with procedural noise mapped through colour `stops`. `kind`
    /// "cloud" (fBm value noise, `octaves`), "perlin" (gradient) or "voronoi"
    /// (cellular). `scale` is the feature size in pixels; `blend` composites over
    /// existing pixels. Textures, terrain, organic mottling.
    pub fn noise(
        &mut self,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        scale: f32,
        octaves: u32,
        seed: u64,
        mut stops: Vec<(f32, [u8; 4])>,
        blend: bool,
    ) -> Result<(), String> {
        if stops.is_empty() {
            return Err("noise needs at least one colour stop".into());
        }
        stops.iter_mut().for_each(|s| s.0 = s.0.clamp(0.0, 1.0));
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
        let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
        let freq = 1.0 / scale.max(0.0001);
        // fBm cost is linear in octaves and each octave beyond ~16 adds
        // sub-pixel detail nobody reads at canvas scale — cap the raw value.
        let octaves = octaves.clamp(1, 16);
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let (fx, fy) = (x as f32 * freq, y as f32 * freq);
                let t = match kind {
                    "perlin" => raster::perlin(fx, fy, seed),
                    "voronoi" => raster::voronoi(fx, fy, seed),
                    _ => raster::fbm(fx, fy, seed, octaves),
                }
                .clamp(0.0, 1.0);
                let c = raster::sample_gradient(&stops, t, "none", x, y, seed);
                let (ux, uy) = (x as u32, y as u32);
                let out = if blend {
                    raster::over(img.get_pixel(ux, uy).0, c)
                } else {
                    c
                };
                img.put_pixel(ux, uy, Rgba(out));
            }
        }
        Ok(())
    }

    /// Shift hue (`dh` degrees) and add to saturation (`ds`) / lightness (`dl`,
    /// each -1..1) of every opaque pixel, optionally limited to `region`.
    /// Recolour / tint / brighten part of a cel.
    pub fn adjust(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        dh: f32,
        ds: f32,
        dl: f32,
    ) -> Result<(), String> {
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = resolve_region(region, w, h)?;
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = img.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                let (hh, ss, ll) = raster::rgb_to_hsl(p[0], p[1], p[2]);
                let rgb = raster::hsl_to_rgb(
                    hh + dh,
                    (ss + ds).clamp(0.0, 1.0),
                    (ll + dl).clamp(0.0, 1.0),
                );
                img.put_pixel(x as u32, y as u32, Rgba([rgb[0], rgb[1], rgb[2], p[3]]));
            }
        }
        Ok(())
    }

    /// Edge-lit on-ramp shading. For each opaque pixel: if the neighbour 1px
    /// *toward* the light is transparent/outside it is a lit rim (push `steps`
    /// toward the light end); if the neighbour *away* from the light is
    /// transparent/outside it is core shadow (push `steps` toward the dark end).
    /// `mode` limits to just highlights or just shadows.
    ///
    /// With a `ramp` (ordered dark→light) each touched pixel snaps to its
    /// nearest ramp entry by luma, then moves ±`steps` along the ramp. Without a
    /// ramp we HSL-shift: lit pixels gain +12% lightness/`steps` and warm their
    /// hue toward 50°; shadow pixels lose 12%/`steps` and cool toward 250°.
    /// Reads the pre-op cel for neighbour tests so all writes are simultaneous
    /// (a rim pixel never re-reads an already-shaded neighbour mid-pass).
    pub fn shade(
        &mut self,
        layer: usize,
        frame: usize,
        light_dir: &str,
        steps: i32,
        region: Option<(i32, i32, i32, i32)>,
        mode: &str,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<(), String> {
        let (ldx, ldy) = match light_dir {
            "top-left" => (-1, -1),
            "top" => (0, -1),
            "top-right" => (1, -1),
            "left" => (-1, 0),
            "right" => (1, 0),
            "bottom-left" => (-1, 1),
            "bottom" => (0, 1),
            "bottom-right" => (1, 1),
            other => {
                return Err(format!(
                    "unknown light_dir '{}' — use top-left/top/top-right/left/right/bottom-left/bottom/bottom-right",
                    other
                ));
            }
        };
        let (do_hi, do_sh) = match mode {
            "both" => (true, true),
            "highlight" => (true, false),
            "shadow" => (false, true),
            other => {
                return Err(format!(
                    "unknown mode '{}' — use both/highlight/shadow",
                    other
                ));
            }
        };
        let steps = steps.max(1);
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = resolve_region(region, w, h)?;
        let (w, h) = (w as i32, h as i32);
        // Snapshot so neighbour opacity/colour reads are all pre-op.
        let before = self.cel_canvas(layer, frame)?.clone();
        let opaque = |x: i32, y: i32| -> bool {
            x >= 0 && y >= 0 && x < w && y < h && before.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = before.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                // Lit rim wins over core shadow when a pixel is both (thin art).
                let lit = !opaque(x + ldx, y + ldy);
                let shadow = !opaque(x - ldx, y - ldy);
                let dir = if lit && do_hi {
                    1
                } else if shadow && do_sh {
                    -1
                } else {
                    continue;
                };
                let out = match &ramp {
                    Some(r) if !r.is_empty() => raster::shade_ramp(p, r, dir * steps),
                    _ => raster::shade_hsl(p, dir, steps),
                };
                img.put_pixel(x as u32, y as u32, Rgba(out));
            }
        }
        Ok(())
    }

    /// Volume shading: lay a rounded-form light gradient across the *interior*
    /// of a shape and snap it to a dark→light `ramp`. Where `shade` only lights
    /// silhouette rims, `form` fills the body — a flat-filled blob gains real
    /// volume in one call (the move that turns stacked ellipses into one lit
    /// sphere).
    ///
    /// `form`:
    ///   "sphere"      ball: radial surface-normal falloff in the region ellipse
    ///   "cylinder-h"  horizontal tube: rounds top↔bottom, flat left↔right
    ///   "cylinder-v"  vertical tube: rounds left↔right, flat top↔bottom
    ///   "auto"        any shape: interior-distance of the opaque silhouette
    ///                 (bright core → dark edge), nudged toward `light_dir`
    /// `light_dir` places the highlight (the 8 compass dirs `shade` uses).
    /// `region` sets the form's bounds/centre; omitted, it's the opaque bbox.
    /// `ramp` ordered dark→light; omitted, one is derived from the mean colour.
    /// `strength` 0..1 compresses (low) or spans the full ramp (1, default).
    pub fn form(
        &mut self,
        layer: usize,
        frame: usize,
        light_dir: &str,
        form: &str,
        region: Option<(i32, i32, i32, i32)>,
        ramp: Option<Vec<[u8; 4]>>,
        strength: f32,
    ) -> Result<(), String> {
        let (ldx, ldy) = match light_dir {
            "top-left" => (-1.0f32, -1.0f32),
            "top" => (0.0, -1.0),
            "top-right" => (1.0, -1.0),
            "left" => (-1.0, 0.0),
            "right" => (1.0, 0.0),
            "bottom-left" => (-1.0, 1.0),
            "bottom" => (0.0, 1.0),
            "bottom-right" => (1.0, 1.0),
            other => {
                return Err(format!(
                    "unknown light_dir '{}' — use top-left/top/top-right/left/right/bottom-left/bottom/bottom-right",
                    other
                ));
            }
        };
        if !matches!(form, "sphere" | "cylinder-h" | "cylinder-v" | "auto") {
            return Err(format!(
                "unknown form '{}' — use sphere/cylinder-h/cylinder-v/auto",
                form
            ));
        }
        let strength = strength.clamp(0.05, 1.0);
        let (w, h) = (self.meta.w, self.meta.h);

        // Snapshot so every read is pre-op; resolve the working region.
        let before = self.cel_canvas(layer, frame)?.clone();
        let (ax, ay, bx, by) = match region {
            Some(r) => resolve_region(Some(r), w, h)?,
            None => {
                let (mut x0, mut y0, mut x1, mut y1) = (w as i32, h as i32, -1i32, -1i32);
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        if before.get_pixel(x as u32, y as u32).0[3] > 0 {
                            x0 = x0.min(x);
                            y0 = y0.min(y);
                            x1 = x1.max(x);
                            y1 = y1.max(y);
                        }
                    }
                }
                if x1 < 0 {
                    return Ok(()); // nothing opaque to shade
                }
                (x0, y0, x1, y1)
            }
        };

        // Ramp: explicit, else derived from the mean opaque colour in-region.
        let ramp = match ramp {
            Some(r) if !r.is_empty() => r,
            _ => {
                let (mut sr, mut sg, mut sb, mut cnt) = (0u64, 0u64, 0u64, 0u64);
                for y in ay..=by {
                    for x in ax..=bx {
                        let p = before.get_pixel(x as u32, y as u32).0;
                        if p[3] > 0 {
                            sr += p[0] as u64;
                            sg += p[1] as u64;
                            sb += p[2] as u64;
                            cnt += 1;
                        }
                    }
                }
                if cnt == 0 {
                    return Ok(());
                }
                let base = [(sr / cnt) as u8, (sg / cnt) as u8, (sb / cnt) as u8, 255];
                raster::make_ramp(base, 5, 12.0, 0.22, 0.18)
            }
        };
        let n = ramp.len() as f32;

        // Region ellipse centre + radii (sphere/cylinder geometry).
        let cx = (ax + bx) as f32 * 0.5;
        let cy = (ay + by) as f32 * 0.5;
        let rx = ((bx - ax) as f32 * 0.5).max(1.0);
        let ry = ((by - ay) as f32 * 0.5).max(1.0);

        // Light vector toward the key; z biased to the viewer for a soft front-key.
        let llen = (ldx * ldx + ldy * ldy + 0.36).sqrt();
        let (lx, ly, lz) = (ldx / llen, ldy / llen, 0.6 / llen);

        // "auto" needs the silhouette's interior distance over the region rect.
        let rw = (bx - ax + 1) as usize;
        let dt = if form == "auto" {
            let rh = (by - ay + 1) as usize;
            let mut fg = vec![false; rw * rh];
            for y in ay..=by {
                for x in ax..=bx {
                    if before.get_pixel(x as u32, y as u32).0[3] > 0 {
                        fg[(y - ay) as usize * rw + (x - ax) as usize] = true;
                    }
                }
            }
            Some(raster::interior_distance(&fg, rw, rh))
        } else {
            None
        };

        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = before.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                let nx = (x as f32 - cx) / rx; // -1..1 across the region
                let ny = (y as f32 - cy) / ry;
                // intensity: 0 (shadow) .. 1 (lit)
                let intensity = match form {
                    "sphere" => {
                        let z = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
                        (nx * lx + ny * ly + z * lz) * 0.5 + 0.5
                    }
                    "cylinder-h" => {
                        let z = (1.0 - ny * ny).max(0.0).sqrt();
                        (ny * ly + z * lz) * 0.5 + 0.5
                    }
                    "cylinder-v" => {
                        let z = (1.0 - nx * nx).max(0.0).sqrt();
                        (nx * lx + z * lz) * 0.5 + 0.5
                    }
                    _ => {
                        // auto: bright core, dark edge, nudged toward the light.
                        let d = dt.as_ref().unwrap()[(y - ay) as usize * rw + (x - ax) as usize];
                        let bias = (-(nx * lx) - (ny * ly)) * 0.5 + 0.5;
                        (d * (0.55 + 0.45 * bias)).clamp(0.0, 1.0)
                    }
                };
                let t = ((intensity - 0.5) * strength + 0.5).clamp(0.0, 1.0);
                let i = (t * (n - 1.0)).round().clamp(0.0, n - 1.0) as usize;
                let c = ramp[i];
                img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], p[3]]));
            }
        }
        Ok(())
    }

    /// Two-colour ordered dither over a region. `pattern` "checker"/"bayer2"/
    /// "bayer4"/"bayer8"; `density` 0..1 biases the mix toward `color_b` (the
    /// fraction of pixels that take color_b via the threshold matrix). When
    /// `only_existing` only pixels already equal to color_a or color_b are
    /// repainted — recolour an existing flat region into a dither without
    /// spilling onto neighbouring art. Reuses the shared Bayer thresholds
    /// (bayer8).
    pub fn dither(
        &mut self,
        layer: usize,
        frame: usize,
        region: (i32, i32, i32, i32),
        color_a: [u8; 4],
        color_b: [u8; 4],
        pattern: &str,
        density: f32,
        only_existing: bool,
    ) -> Result<(), String> {
        let valid = ["checker", "bayer2", "bayer4", "bayer8"];
        if !valid.contains(&pattern) {
            return Err(format!(
                "unknown pattern '{}' — use checker/bayer2/bayer4/bayer8",
                pattern
            ));
        }
        let density = density.clamp(0.0, 1.0);
        let (x0, y0, x1, y1) = region;
        let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, self.meta.w, self.meta.h)
            .ok_or("dither region is empty after clamping to the canvas")?;
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                if only_existing {
                    let p = img.get_pixel(x as u32, y as u32).0;
                    if p != color_a && p != color_b {
                        continue;
                    }
                }
                // threshold in [0,1): paint color_b where density exceeds it.
                let c = if density > raster::dither_threshold(pattern, x, y) {
                    color_b
                } else {
                    color_a
                };
                img.put_pixel(x as u32, y as u32, Rgba(c));
            }
        }
        Ok(())
    }

    /// Remove L-corner doubles from 1px strokes (the "pixel-perfect" cleanup
    /// technique). A pixel P is erased when it matches the target colour(s), two
    /// orthogonally-adjacent neighbours forming an L (left+top, top+right,
    /// right+bottom or bottom+left) also match, AND the diagonal cell between
    /// that pair does NOT match — i.e. P only exists to thicken an elbow.
    /// Iterates to a fixpoint (max 8 passes). `color` (optional) restricts the
    /// target to strokes of that exact colour. Returns the count erased.
    pub fn pixel_perfect(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        color: Option<[u8; 4]>,
    ) -> Result<u32, String> {
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = resolve_region(region, w, h)?;
        let (w, h) = (w as i32, h as i32);
        let img = self.cel_canvas(layer, frame)?;
        // A cell "matches" the stroke when opaque (and == color, if restricted).
        let stroke = |img: &RgbaImage, x: i32, y: i32| -> bool {
            if x < 0 || y < 0 || x >= w || y >= h {
                return false;
            }
            let p = img.get_pixel(x as u32, y as u32).0;
            match color {
                Some(c) => p == c,
                None => p[3] > 0,
            }
        };
        // The four L-corners: (a-offset, b-offset, diagonal-offset).
        let corners = [
            ((-1, 0), (0, -1), (-1, -1)), // left + top
            ((0, -1), (1, 0), (1, -1)),   // top + right
            ((1, 0), (0, 1), (1, 1)),     // right + bottom
            ((0, 1), (-1, 0), (-1, 1)),   // bottom + left
        ];
        let mut removed = 0u32;
        for _ in 0..8 {
            let mut drop: Vec<(i32, i32)> = Vec::new();
            for y in ay..=by {
                for x in ax..=bx {
                    if !stroke(img, x, y) {
                        continue;
                    }
                    let elbow = corners.iter().any(|&((ax2, ay2), (bx2, by2), (dx, dy))| {
                        stroke(img, x + ax2, y + ay2)
                            && stroke(img, x + bx2, y + by2)
                            && !stroke(img, x + dx, y + dy)
                    });
                    if elbow {
                        drop.push((x, y));
                    }
                }
            }
            if drop.is_empty() {
                break;
            }
            for (x, y) in drop {
                img.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Snap every opaque pixel to the nearest colour in `palette`. With an empty
    /// palette, derive one of `max_colors` from the cel by median cut. Returns
    /// the palette used. Posterise / down-palette imported art.
    pub fn quantize(
        &mut self,
        layer: usize,
        frame: usize,
        palette: Vec<[u8; 4]>,
        max_colors: usize,
    ) -> Result<Vec<[u8; 4]>, String> {
        let img = self.cel_canvas(layer, frame)?;
        let pal = if !palette.is_empty() {
            palette
        } else {
            let opaque: Vec<[u8; 3]> = img
                .pixels()
                .filter(|p| p.0[3] > 0)
                .map(|p| [p.0[0], p.0[1], p.0[2]])
                .collect();
            raster::median_cut(&opaque, max_colors)
        };
        if pal.is_empty() {
            return Err("quantize needs a palette or max_colors >= 1".into());
        }
        // OKLab nearest, matching snap_to_palette — the old RGB squared
        // distance disagreed with the snap metric, so iterative quantize/snap
        // cycles flipped colours back and forth (and skin tones snapped grey).
        let mut lab = raster::PaletteLab::new(&pal);
        for p in img.pixels_mut() {
            if p.0[3] == 0 {
                continue;
            }
            if let Some(i) = lab.nearest(p.0) {
                let c = lab.color(i);
                *p = Rgba([c[0], c[1], c[2], p.0[3]]);
            }
        }
        Ok(pal)
    }

    /// GRADIENT MAP: remap every opaque pixel's LUMINANCE through colour
    /// `stops` (0 = darkest, 1 = lightest), preserving alpha — the one-call
    /// mood/recolour move (sunset-ify, poison-ify, night-palette a sprite)
    /// that keeps all the drawn shading structure and swaps only its colour
    /// story. Region-scopable.
    pub fn gradient_map(
        &mut self,
        layer: usize,
        frame: usize,
        mut stops: Vec<(f32, [u8; 4])>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), String> {
        if stops.is_empty() {
            return Err("gradient_map needs at least one colour stop".into());
        }
        stops.iter_mut().for_each(|s| s.0 = s.0.clamp(0.0, 1.0));
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (ax, ay, bx, by) = resolve_region(region, self.meta.w, self.meta.h)?;
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = img.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                let t = raster::luma(p) as f32 / 255.0;
                let c = raster::sample_gradient(&stops, t, "none", x, y, 0);
                img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], p[3]]));
            }
        }
        Ok(())
    }
}
