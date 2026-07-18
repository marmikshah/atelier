//! The see-tools: `look` (the agent's primary eye — a flattened frame or an
//! analysis view of it, with measured stats) and `contact_sheet` (the
//! animator's flip-test). Image-returning methods hand back raw PNG bytes; the
//! server wraps them as inline MCP image content so the pixels arrive in the
//! same turn.

use std::fs;

use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use super::craft::{crop_region, MID_MAX, SHADOW_MAX};
use super::{encode_png, scale_nn, Studio};
use atelier_core::raster;

/// Alpha-composite one pixel onto `img` (source-over) — for overlays that must
/// not erase the art underneath.
fn blend_put(img: &mut RgbaImage, x: i32, y: i32, c: [u8; 4]) {
    if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
        return;
    }
    let d = img.get_pixel(x as u32, y as u32).0;
    let out = raster::composite_px(d, c, c[3] as f32 / 255.0, raster::Blend::Normal);
    img.put_pixel(x as u32, y as u32, Rgba(out));
}

/// Draw a string with the built-in 3×5 glyph font onto a standalone image
/// (mirror of `Document::text`, but for previews/overlays). Returns the x pen
/// after the last glyph.
fn draw_label(img: &mut RgbaImage, x: i32, y: i32, text: &str, color: [u8; 4]) -> i32 {
    let advance = raster::GLYPH_W + 1;
    let mut pen = x;
    for ch in text.chars() {
        let bits = raster::glyph(ch);
        for gy in 0..raster::GLYPH_H {
            for gx in 0..raster::GLYPH_W {
                let bit = gy * raster::GLYPH_W + (raster::GLYPH_W - 1 - gx);
                if (bits >> bit) & 1 == 1 {
                    blend_put(img, pen + gx, y + gy, color);
                }
            }
        }
        pen += advance;
    }
    pen
}

/// Overlay a pixel-cell grid and coordinate ruler on a scaled preview. `ox,oy`
/// is the native origin of the (possibly cropped) view; `scale` the upscale
/// factor; `step` the native-pixel grid spacing.
fn overlay_grid(img: &mut RgbaImage, ox: i32, oy: i32, scale: u32, step: i32, coords: bool) {
    let step = step.max(1);
    let s = scale as i32;
    let (w, h) = (img.width() as i32, img.height() as i32);
    let line = [255, 0, 255, 70]; // magenta, faint — survives over any art
                                  // Vertical lines at every `step`-th native column boundary.
    let mut nx = ox - ox.rem_euclid(step);
    while (nx - ox) * s <= w {
        let sx = (nx - ox) * s;
        if sx >= 0 && sx < w {
            for y in 0..h {
                blend_put(img, sx, y, line);
            }
            if coords && nx % (step * 2) == 0 {
                draw_label(img, sx + 1, 0, &nx.to_string(), [255, 255, 0, 220]);
            }
        }
        nx += step;
    }
    let mut ny = oy - oy.rem_euclid(step);
    while (ny - oy) * s <= h {
        let sy = (ny - oy) * s;
        if sy >= 0 && sy < h {
            for x in 0..w {
                blend_put(img, x, sy, line);
            }
            if coords && ny % (step * 2) == 0 {
                draw_label(img, 0, sy + 1, &ny.to_string(), [255, 255, 0, 220]);
            }
        }
        ny += step;
    }
}

/// Value-mass + colour stats over the opaque pixels of a native image — the
/// numbers that go beside the inline preview so every look is also measured.
fn look_stats(img: &RgbaImage, bands: Option<u32>) -> Value {
    let (mut min, mut max, mut sum, mut n) = (255u8, 0u8, 0u64, 0u64);
    let (mut shadow, mut mid, mut light) = (0u64, 0u64, 0u64);
    let mut distinct = std::collections::HashSet::new();
    // Optional per-band value coverage (the structure read for `bands`/`notan`,
    // carried over from the retired doc_render_value `band_pcts`).
    let nb = bands.map(|b| b.max(2) as usize);
    let mut band_counts = nb.map(|b| vec![0u64; b]);
    for p in img.pixels() {
        if p.0[3] == 0 {
            continue;
        }
        let v = raster::luma(p.0);
        min = min.min(v);
        max = max.max(v);
        sum += v as u64;
        n += 1;
        if v < SHADOW_MAX {
            shadow += 1;
        } else if v < MID_MAX {
            mid += 1;
        } else {
            light += 1;
        }
        if let (Some(b), Some(counts)) = (nb, band_counts.as_mut()) {
            counts[(v as usize * b / 256).min(b - 1)] += 1;
        }
        distinct.insert([p.0[0], p.0[1], p.0[2], p.0[3]]);
    }
    if n == 0 {
        return json!({"opaque_pixels": 0, "note": "empty — nothing opaque in view"});
    }
    let pct = |c: u64| (c as f64 / n as f64 * 1000.0).round() / 10.0;
    let mut out = json!({
        "opaque_pixels": n,
        "distinct_colors": distinct.len(),
        "value": {
            "min": min, "max": max,
            "mean": (sum as f64 / n as f64).round() as u32,
            "contrast": ((max - min) as f64 / 255.0 * 1000.0).round() / 1000.0,
        },
        // Value massing: a healthy read groups the canvas into a few clear
        // masses, not a soup. Even thirds is the soup-warning signal.
        "masses_pct": {"shadow": pct(shadow), "mid": pct(mid), "light": pct(light)},
    });
    if let Some(counts) = band_counts {
        out["band_pcts"] = json!(counts.iter().map(|c| pct(*c)).collect::<Vec<f64>>());
    }
    out
}

/// Repeat an image N×N — the seamlessness eyeball test for `doc_look` `tile`.
/// `look` clamps the caller's N to MAX_TILE before this: the output buffer is
/// (w·n)×(h·n), and an unclamped N was a memory-exhaustion abort.
pub(crate) const MAX_TILE: u32 = 16;

fn tile_image(img: &RgbaImage, n: u32) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = RgbaImage::new(w * n, h * n);
    for ty in 0..n {
        for tx in 0..n {
            image::imageops::replace(&mut out, img, (tx * w) as i64, (ty * h) as i64);
        }
    }
    out
}

/// Options for [`Studio::look`] — every knob has a sane default, so callers
/// name only what they change instead of threading twelve positionals.
#[derive(Clone, Default)]
pub struct LookOptions {
    /// Upscale factor; None = adaptive (~384px longest side).
    pub scale: Option<u32>,
    /// Crop region `[x0,y0,x1,y1]` in document pixels.
    pub region: Option<(i32, i32, i32, i32)>,
    /// render | value/grayscale | bands | sat | hue | notan. Empty = render.
    pub mode: String,
    /// Band count for bands mode (0 = default 4).
    pub bands: u32,
    /// Burn a pixel ruler into the upscale.
    pub grid: bool,
    /// Label the ruler with coordinates.
    pub coords: bool,
    /// Ghost neighbouring frames.
    pub onion: bool,
    /// Thumbnail bound for the longest side.
    pub max_size: Option<u32>,
    /// Repeat the result N×N (seam check).
    pub tile: Option<u32>,
    /// Also write the PNG here.
    pub out_path: Option<String>,
    /// Matte transparency for viewing: checker | dark | white. None keeps the
    /// alpha channel (most viewers show it on white, which hides light pixels).
    pub bg: Option<String>,
}

/// Composite `img` over an opaque backdrop: `checker` (two-tone transparency
/// grid, cells sized to read at the applied scale), `dark`, or `white`.
fn matte(img: &RgbaImage, bg: &str, scale: u32) -> Result<RgbaImage, String> {
    let cell = (scale.max(1) * 4).max(4);
    let color_at = |x: u32, y: u32| -> [u8; 4] {
        match bg {
            "checker" => {
                if ((x / cell) + (y / cell)).is_multiple_of(2) {
                    [58, 62, 76, 255]
                } else {
                    [40, 43, 54, 255]
                }
            }
            "dark" => [24, 26, 32, 255],
            "white" => [255, 255, 255, 255],
            _ => [0, 0, 0, 0], // unreachable; validated below
        }
    };
    if !matches!(bg, "checker" | "dark" | "white") {
        return Err(format!("unknown bg '{bg}' — use checker|dark|white"));
    }
    let mut out = RgbaImage::new(img.width(), img.height());
    for (x, y, p) in img.enumerate_pixels() {
        let b = color_at(x, y);
        let a = p.0[3] as u32;
        let blend = |s: u8, d: u8| ((s as u32 * a + d as u32 * (255 - a)) / 255) as u8;
        out.put_pixel(
            x,
            y,
            image::Rgba([
                blend(p.0[0], b[0]),
                blend(p.0[1], b[1]),
                blend(p.0[2], b[2]),
                255,
            ]),
        );
    }
    Ok(out)
}

impl Studio {
    // -- doc_look: the single SEE call -------------------------------------

    /// Flatten a frame (or an analysis-space view of it) to inline PNG bytes
    /// plus measured stats — the agent's primary eye. `mode`: `render` |
    /// `value`/`grayscale` | `bands` | `sat` | `hue` | `notan`. `grid`/`coords`
    /// burn a pixel ruler into the upscale. Returns `(png_bytes, stats)`.
    pub fn look(
        &self,
        id: &str,
        frame: usize,
        opts: &LookOptions,
    ) -> Result<(Vec<u8>, Value), String> {
        let LookOptions {
            scale,
            region,
            mode,
            bands,
            grid,
            coords,
            onion,
            max_size,
            tile,
            out_path,
            bg,
        } = opts;
        let (scale, region, max_size, tile) = (*scale, *region, *max_size, *tile);
        let (grid, coords, onion) = (*grid, *coords, *onion);
        let mode = if mode.is_empty() {
            "render"
        } else {
            mode.as_str()
        };
        let bands = if *bands == 0 { 4 } else { (*bands).min(256) };
        let out_path = out_path.as_deref();
        let (_dir, doc) = self.open(id)?;
        if frame >= doc.meta().frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                doc.meta().frames.len()
            ));
        }
        // Adaptive default: big enough to judge a small sprite, clamped so a
        // large canvas doesn't waste vision tokens.
        let scale = scale.unwrap_or_else(|| crate::preview_scale(doc.meta().w, doc.meta().h));
        // Native, full-canvas image for the requested mode.
        let native = match mode {
            "render" => {
                if onion {
                    doc.render_preview(frame, 1, None, true, 1, None)?
                } else {
                    doc.flatten(frame)
                }
            }
            "value" | "grayscale" => doc.value_image(frame, "grayscale", 1)?,
            "bands" => doc.value_image(frame, "bands", bands.max(2))?,
            "sat" | "saturation" => doc.value_image(frame, "saturation", 1)?,
            "hue" => doc.value_image(frame, "hue", 1)?,
            // Notan = 3-value massing (dark / mid / light) — the squint test.
            "notan" => doc.value_image(frame, "bands", 3)?,
            other => {
                return Err(format!(
                    "unknown look mode '{}' — use render|value|bands|sat|hue|notan",
                    other
                ))
            }
        };
        let (view, ox, oy) = crop_region(&native, region)?;
        // Per-band coverage is the value-structure read; meaningful only for the
        // posterised modes (carried over from the retired doc_render_value).
        let band_arg = match mode {
            "bands" => Some(bands.max(2)),
            "notan" => Some(3),
            _ => None,
        };
        let stats = look_stats(&view, band_arg);
        // Scale: explicit nearest upscale, or a max_size thumbnail (no grid).
        // applied_scale goes through the same clamp scale_nn applies internally,
        // so the report, the grid ruler and the matte all agree with the
        // actual resize (a caller's scale=1000 used to report 1000 while the
        // image came out at 16×).
        let mut out;
        let mut applied_scale = crate::export_scale(scale);
        if let Some(ms) = max_size {
            let long = view.width().max(view.height());
            if long > ms && ms > 0 {
                let f = ms as f32 / long as f32;
                out = image::imageops::resize(
                    &view,
                    (view.width() as f32 * f).round().max(1.0) as u32,
                    (view.height() as f32 * f).round().max(1.0) as u32,
                    image::imageops::FilterType::Nearest,
                );
                applied_scale = 1; // thumbnail; grid would be meaningless
            } else {
                out = scale_nn(&view, applied_scale);
            }
        } else {
            out = scale_nn(&view, applied_scale);
        }
        // Matte transparency on request — a white viewer backdrop (the common
        // default) makes white-hot FX pixels invisible; checker/dark keep the
        // silhouette readable. Applied under the art, before the grid overlay.
        if let Some(bg) = bg.as_deref() {
            out = matte(&out, bg, applied_scale)?;
        }
        if grid && applied_scale >= 2 {
            // Aim for ~8px native grid cells; at least every pixel boundary.
            let step = (8).min((view.width().max(view.height()) as i32 / 2).max(1));
            overlay_grid(&mut out, ox, oy, applied_scale, step, coords);
        }
        // Tile the result N×N to eyeball seamlessness (the retired doc_render's
        // `tile`); applied after scale/grid so each cell shows the upscaled art.
        // Clamped: the buffer grows with N² — an unbounded N was a memory-abort.
        if let Some(t) = tile {
            let t = t.min(MAX_TILE);
            if t > 1 {
                out = tile_image(&out, t);
            }
        }
        // Optional file write for export/file workflows (the retired doc_render's
        // `out_path`). look stays inline-primary, so we only touch disk on request.
        let saved_path = match out_path {
            Some(p) => {
                let pb = std::path::PathBuf::from(p);
                if let Some(parent) = pb.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
                out.save(&pb).map_err(|e| e.to_string())?;
                Some(pb.to_string_lossy().into_owned())
            }
            None => None,
        };
        let png = encode_png(&out)?;
        let mut report = json!({
            "doc_id": id, "frame": frame, "mode": mode,
            "native_size": [view.width(), view.height()],
            "render_size": [out.width(), out.height()],
            "scale": applied_scale,
            "region_origin": [ox, oy],
            "stats": stats,
        });
        if let Some(p) = saved_path {
            report["path"] = json!(p);
        }
        Ok((png, report))
    }

    // -- doc_contact_sheet: the animator's flip-test -----------------------

    /// Every frame in one labelled inline grid — the flip-test the agent can't
    /// otherwise do. `onion` draws each cell over a 35%-alpha ghost of the
    /// PREVIOUS frame: the closest a single still image gets to letting a
    /// vision model perceive motion (spacing, overlap, popping) per pair.
    /// Returns `(png_bytes, report)`.
    pub fn contact_sheet(
        &self,
        id: &str,
        scale: u32,
        cols: usize,
        onion: bool,
    ) -> Result<(Vec<u8>, Value), String> {
        let (_dir, doc) = self.open(id)?;
        let n = doc.meta().frames.len();
        let cols = cols.max(1).min(n.max(1));
        let rows = n.div_ceil(cols);
        let (w, h) = (doc.meta().w, doc.meta().h);
        // Clamp ONCE and use it for both the sheet math and the cell renders:
        // cellw/cellh used to take the raw caller value while scale_nn clamped
        // internally — scale > 16 sized the cells for one image and drew another
        // (and an extreme value overflowed the sheet dimensions entirely).
        let s = crate::export_scale(scale);
        let (pad, label_h) = (3u32, raster::GLYPH_H as u32 + 1);
        let cellw = w * s;
        let cellh = h * s + label_h;
        let sheetw = cols as u32 * (cellw + pad) + pad;
        let sheeth = rows as u32 * (cellh + pad) + pad;
        let mut sheet = RgbaImage::from_pixel(sheetw, sheeth, Rgba([20, 20, 26, 255]));
        // Carry each frame's scaled render into the next iteration — the onion
        // ghost is the previous cell, not a second flatten+scale.
        let mut prev_scaled: Option<RgbaImage> = None;
        for f in 0..n {
            let scaled = scale_nn(&doc.flatten(f), s);
            let (col, row) = ((f % cols) as u32, (f / cols) as u32);
            let ox = pad + col * (cellw + pad);
            let oy = pad + row * (cellh + pad) + label_h;
            if onion && f > 0 {
                let ghost = prev_scaled
                    .take()
                    .unwrap_or_else(|| scale_nn(&doc.flatten(f - 1), s));
                for (x, y, p) in ghost.enumerate_pixels() {
                    if p.0[3] > 0 {
                        let a = (p.0[3] as u32 * 35 / 100) as u8;
                        blend_put(
                            &mut sheet,
                            (ox + x) as i32,
                            (oy + y) as i32,
                            [p.0[0], p.0[1], p.0[2], a],
                        );
                    }
                }
            }
            for (x, y, p) in scaled.enumerate_pixels() {
                if p.0[3] > 0 {
                    blend_put(&mut sheet, (ox + x) as i32, (oy + y) as i32, p.0);
                }
            }
            if onion {
                prev_scaled = Some(scaled);
            }
            let dur = doc.meta().frames[f].duration_ms;
            draw_label(
                &mut sheet,
                ox as i32,
                (oy - label_h) as i32,
                &format!("F{} {}", f, dur),
                [200, 200, 210, 255],
            );
        }
        let png = encode_png(&sheet)?;
        Ok((
            png,
            json!({"doc_id": id, "frames": n, "cols": cols, "rows": rows, "size": [sheetw, sheeth]}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-view-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn contact_sheet_returns_a_grid() {
        let s = studio("contact");
        s.doc_create("c", 4, 4).unwrap();
        let (png, report) = s.contact_sheet("c", 4, 8, false).unwrap();
        assert_eq!(&png[0..4], b"\x89PNG");
        assert_eq!(report["frames"], 1);
    }

    #[test]
    fn matte_makes_transparency_opaque_and_rejects_unknown_bg() {
        // A white viewer backdrop hides white-hot FX pixels — the matte is how
        // the agent actually sees them.
        let img = RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 0]));
        for bg in ["checker", "dark", "white"] {
            let out = matte(&img, bg, 4).unwrap();
            assert!(out.pixels().all(|p| p.0[3] == 255), "{bg}: fully opaque");
        }
        // Checker must alternate (two distinct backdrop tones visible).
        let out = matte(&img, "checker", 1).unwrap();
        let tones: std::collections::HashSet<[u8; 4]> = out.pixels().map(|p| p.0).collect();
        assert_eq!(tones.len(), 2, "checker shows two cells");
        assert!(matte(&img, "plaid", 1).is_err());
    }

    #[test]
    fn look_bg_flows_through_the_options() {
        let s = studio("lookbg");
        s.doc_create("c", 4, 4).unwrap();
        let opts = LookOptions {
            scale: Some(1),
            bg: Some("dark".into()),
            ..Default::default()
        };
        let (png, _) = s.look("c", 0, &opts).unwrap();
        assert_eq!(&png[0..4], b"\x89PNG");
        let bad = LookOptions {
            bg: Some("plaid".into()),
            ..Default::default()
        };
        assert!(s.look("c", 0, &bad).is_err());
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn look_tile_and_scale_are_capped_not_fatal() {
        let dir = std::env::temp_dir().join("atelier-view-hard");
        let _ = fs::remove_dir_all(&dir);
        let s = Studio::with_docs_dir(dir);
        s.doc_create("c", 8, 8).unwrap();
        // tile=u32::MAX used to attempt a (8·4·4G)² buffer; now clamps to 16.
        let (_png, r) = s
            .look(
                "c",
                0,
                &LookOptions {
                    scale: Some(2),
                    tile: Some(u32::MAX),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            r["render_size"],
            json!([8 * 2 * super::MAX_TILE, 8 * 2 * super::MAX_TILE])
        );
        // A huge explicit scale is clamped AND reported as clamped (the grid
        // ruler and the report used to disagree with the actual resize).
        let (_png, r) = s
            .look(
                "c",
                0,
                &LookOptions {
                    scale: Some(1_000_000),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(r["scale"], json!(16));
        assert_eq!(r["render_size"], json!([128, 128]));
    }
}
