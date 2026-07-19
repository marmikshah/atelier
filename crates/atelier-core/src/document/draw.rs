//! Drawing primitives — the mark-making half of the `Document` surface.

use image::{Rgba, RgbaImage};

use crate::raster;

use super::{AlphaSnap, Document};

/// Flatten tolerance for `Document::curve`, in pixels. A quarter pixel sits
/// under the stroke core's 0.5-coverage edge, so the flattened polyline paints
/// identically to the true curve at any width.
const CURVE_TOLERANCE: f32 = 0.25;

impl Document {
    pub fn pencil(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        size: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        for (x, y) in points {
            raster::brush(img, *x, *y, color, size.max(1));
        }
        Ok(())
    }

    pub fn line(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        size: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        raster::draw_line(img, x0, y0, x1, y1, color, size.max(1));
        Ok(())
    }

    pub fn rect(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        fill: bool,
        size: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        // Clamp the fill span to the canvas: the corners are raw caller input,
        // and looping over an off-canvas span of billions of cells (one bad
        // coordinate) wedged the server on a no-op.
        let (w, h) = (img.width() as i32, img.height() as i32);
        let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
        let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
        if fill {
            for y in ay..=by {
                for x in ax..=bx {
                    raster::put(img, x, y, color);
                }
            }
        } else {
            let (ax, bx) = (x0.min(x1), x0.max(x1));
            let (ay, by) = (y0.min(y1), y0.max(y1));
            raster::draw_line(img, ax, ay, bx, ay, color, size.max(1));
            raster::draw_line(img, ax, by, bx, by, color, size.max(1));
            raster::draw_line(img, ax, ay, ax, by, color, size.max(1));
            raster::draw_line(img, bx, ay, bx, by, color, size.max(1));
        }
        Ok(())
    }

    /// Draw an ellipse (rx==ry ⇒ circle). Filled or 1px outline. The radii are
    /// inflated by half a pixel in the boundary test so the four cardinal tips
    /// come out rounded instead of single-pixel nubs; the outline is the
    /// morphological inner edge of that same fill, so it is always a clean,
    /// closed, gap-free 1px ring that matches the filled shape exactly.
    pub fn ellipse(
        &mut self,
        layer: usize,
        frame: usize,
        cx: i32,
        cy: i32,
        rx: i32,
        ry: i32,
        color: [u8; 4],
        fill: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (rx, ry) = (rx.max(1), ry.max(1));
        let (a, b) = (rx as f32 + 0.5, ry as f32 + 0.5);
        let inside = |x: i32, y: i32| (x as f32 / a).powi(2) + (y as f32 / b).powi(2) <= 1.0;
        // Scan only the offsets that can land on the canvas: the radii are raw
        // caller input, and an i32::MAX radius turned one bad call into a
        // 4-billion-row loop on a server that then answered nothing else.
        let (w, h) = (img.width() as i32, img.height() as i32);
        // Saturating ops: an i32::MIN centre would overflow a plain `-cx`.
        let (x0, x1) = (
            (-rx).max(cx.saturating_neg()),
            rx.min((w - 1).saturating_sub(cx)),
        );
        let (y0, y1) = (
            (-ry).max(cy.saturating_neg()),
            ry.min((h - 1).saturating_sub(cy)),
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                if !inside(x, y) {
                    continue;
                }
                let draw = fill
                    || !(inside(x - 1, y)
                        && inside(x + 1, y)
                        && inside(x, y - 1)
                        && inside(x, y + 1));
                if draw {
                    raster::put(img, cx + x, cy + y, color);
                }
            }
        }
        Ok(())
    }

    /// Connected line segments through `points` (open path). `closed` also joins
    /// the last point back to the first (polygon outline). Square brush `size`.
    pub fn polyline(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        size: i32,
        closed: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let s = size.max(1);
        if points.len() == 1 {
            raster::brush(img, points[0].0, points[0].1, color, s);
        }
        for w in points.windows(2) {
            raster::draw_line(img, w[0].0, w[0].1, w[1].0, w[1].1, color, s);
        }
        if closed && points.len() >= 3 {
            let (a, b) = (*points.last().unwrap(), points[0]);
            raster::draw_line(img, a.0, a.1, b.0, b.1, color, s);
        }
        Ok(())
    }

    /// Polygon through `points`. `fill` scanline-fills the interior (even-odd)
    /// and strokes the edge so steep sides have no 1px gaps; otherwise draws the
    /// closed outline only. Clean organic curves — canopies, ponds, bodies.
    pub fn polygon(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        fill: bool,
    ) -> Result<(), String> {
        if points.len() < 3 || !fill {
            return self.polyline(layer, frame, points, color, 1, true);
        }
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let img = self.cel_canvas(layer, frame)?;
        let ymin = points.iter().map(|p| p.1).min().unwrap().max(0);
        let ymax = points.iter().map(|p| p.1).max().unwrap().min(h - 1);
        let n = points.len();
        for y in ymin..=ymax {
            let yf = y as f32 + 0.5;
            // X where each edge crosses this scanline's centre.
            let mut xs: Vec<f32> = Vec::new();
            for i in 0..n {
                let (x1, y1) = points[i];
                let (x2, y2) = points[(i + 1) % n];
                let (y1f, y2f) = (y1 as f32, y2 as f32);
                if (y1f <= yf && y2f > yf) || (y2f <= yf && y1f > yf) {
                    let t = (yf - y1f) / (y2f - y1f);
                    xs.push(x1 as f32 + t * (x2 as f32 - x1 as f32));
                }
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut i = 0;
            while i + 1 < xs.len() {
                let xa = (xs[i].ceil() as i32).max(0);
                let xb = (xs[i + 1].floor() as i32).min(w - 1);
                for x in xa..=xb {
                    raster::put(img, x, y, color);
                }
                i += 2;
            }
        }
        self.polyline(layer, frame, points, color, 1, true)
    }

    pub fn bucket_fill(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        color: [u8; 4],
        tol: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        if x < 0 || y < 0 || x >= w || y >= h {
            return Ok(());
        }
        let target = img.get_pixel(x as u32, y as u32).0;
        if raster::close(target, color, 0) {
            return Ok(());
        }
        // Visited mask, not a colour check: when the fill colour is itself
        // within `tol` of the target, painted pixels still match and the
        // scan loops forever without it.
        let mut visited = vec![false; (w * h) as usize];
        let mut stack = vec![(x, y)];
        while let Some((px, py)) = stack.pop() {
            if px < 0 || py < 0 || px >= w || py >= h {
                continue;
            }
            let i = (py * w + px) as usize;
            if visited[i] {
                continue;
            }
            visited[i] = true;
            let p = img.get_pixel(px as u32, py as u32).0;
            // close_rgb ignores alpha, and transparent is stored [0,0,0,0] — so
            // without this a fill outside a BLACK outline matches the outline
            // (same RGB) and eats it, and a fill inside a black shape escapes to
            // the whole canvas. Same guard, same reason, as flood_mask.
            if (p[3] == 0) != (target[3] == 0) {
                continue;
            }
            if !raster::close_rgb(p, target, tol) {
                continue;
            }
            img.put_pixel(px as u32, py as u32, Rgba(color));
            stack.push((px + 1, py));
            stack.push((px - 1, py));
            stack.push((px, py + 1));
            stack.push((px, py - 1));
        }
        Ok(())
    }

    pub fn replace_color(
        &mut self,
        layer: usize,
        frame: usize,
        from: [u8; 4],
        to: [u8; 4],
        tol: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        for p in img.pixels_mut() {
            // RGB max-channel match (alpha ignored) so AA/semi-transparent edges
            // of the target colour are recoloured too, not left as a halo. But
            // transparent is stored [0,0,0,0], so matching on RGB alone made
            // `from` = black repaint the whole empty canvas. Only cross the
            // opaque/transparent line when that is what was asked for.
            if (p.0[3] == 0) != (from[3] == 0) {
                continue;
            }
            if raster::close_rgb(p.0, from, tol) {
                *p = Rgba(to);
            }
        }
        Ok(())
    }

    pub fn flip(&mut self, layer: usize, frame: usize, horizontal: bool) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let flipped = if horizontal {
            image::imageops::flip_horizontal(img)
        } else {
            image::imageops::flip_vertical(img)
        };
        *img = flipped;
        Ok(())
    }

    /// Shift a cel's contents by (dx,dy). `wrap` true rolls pixels around the
    /// edges (toroidal — for making/checking seamless tiles); false leaves the
    /// exposed edges transparent.
    pub fn shift(
        &mut self,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        wrap: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut out = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
        for y in 0..h {
            for x in 0..w {
                // i64 math: `x + dx` overflows i32 for an absurd (but accepted)
                // dx — a debug panic, a wrapped pixel in release.
                let (tx, ty) = if wrap {
                    (
                        (x as i64 + dx as i64).rem_euclid(w as i64) as i32,
                        (y as i64 + dy as i64).rem_euclid(h as i64) as i32,
                    )
                } else {
                    match (
                        (x as i64 + dx as i64).try_into(),
                        (y as i64 + dy as i64).try_into(),
                    ) {
                        (Ok(tx), Ok(ty)) => (tx, ty),
                        _ => continue, // shifted fully out of i32 range = off-canvas
                    }
                };
                if tx >= 0 && ty >= 0 && tx < w && ty < h {
                    out.put_pixel(tx as u32, ty as u32, *img.get_pixel(x as u32, y as u32));
                }
            }
        }
        *img = out;
        Ok(())
    }

    /// Rotate a cel's content by `turns_cw` quarter-turns CLOCKWISE (1 = 90°,
    /// 2 = 180°, 3 = 270°; values wrap mod 4). Like `flip`, the cel is
    /// normalised to the full canvas first, so the content turns about the
    /// canvas centre and the canvas never changes size: what rotates outside
    /// clips, the vacated corners come back transparent. Exact on a square
    /// canvas.
    pub fn rotate_cel(&mut self, layer: usize, frame: usize, turns_cw: u8) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        *img = raster::rotate_quarters(img, turns_cw);
        Ok(())
    }

    /// Resize a cel's image to exactly `w`×`h`, keeping its (x,y) anchor —
    /// the document canvas is untouched, so a cel that outgrows the canvas
    /// clips at composite as usual. `method` is "nearest" (crisp pixel
    /// replication) or "area-average" (the alpha-weighted box filter, kinder
    /// to thin outlines on shrinks). A missing cel or a zero dimension is a
    /// caller mistake and errors loudly, not a silent no-op.
    pub fn scale_cel(
        &mut self,
        layer: usize,
        frame: usize,
        w: u32,
        h: u32,
        method: &str,
    ) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        if w == 0 || h == 0 {
            return Err(format!("scale_cel needs w/h >= 1 (got {w}x{h})"));
        }
        let Some((_, _, img)) = self.cels.get_mut(&(layer, frame)) else {
            return Err(format!("no cel at layer {layer} frame {frame} to scale"));
        };
        *img = match method {
            "nearest" => image::imageops::resize(img, w, h, image::imageops::FilterType::Nearest),
            "area-average" => raster::downscale_area(img, w, h),
            other => {
                return Err(format!(
                    "unknown scale method '{other}' — use nearest|area-average"
                ))
            }
        };
        self.mark_dirty(layer, frame);
        Ok(())
    }

    /// Draw a variable-width, anti-aliased stroke through `pts` (each
    /// `(x, y, full_width)`) as the union of round-capped capsules — the
    /// clean-by-construction stroke core. Connected (union, no gaps between
    /// samples like stacked beziers leave), tapered (per-vertex width — `0` ends
    /// give a 1px point), and smooth (analytic coverage, not a Bresenham
    /// staircase). A two-point call is a tapered capsule limb. `aa=false` keeps a
    /// crisp edge; with a palette locked and `snap`, the stroke's RGB is pulled
    /// on-palette (alpha preserved, so soft AA stays soft but on-palette).
    pub fn stroke(
        &mut self,
        layer: usize,
        frame: usize,
        pts: &[(i32, i32, i32)],
        color: [u8; 4],
        aa: bool,
        snap: bool,
    ) -> Result<(), String> {
        let f: Vec<(f32, f32, f32)> = pts
            .iter()
            .map(|&(x, y, w)| (x as f32, y as f32, w.max(0) as f32))
            .collect();
        self.stroke_f(layer, frame, &f, color, aa, snap)
    }

    /// Sub-pixel variant of [`Self::stroke`]: each point is `(x, y, full_width)`
    /// in continuous coordinates, fed straight to the coverage core with no
    /// integer round trip. Curves and poses sampled in `f32` (arcs, IK-solved
    /// limbs, eased motion) keep their sub-pixel precision instead of collapsing
    /// to whole pixels — the fix for choppy staircased curves. `stroke` is the
    /// integer-point wrapper over this.
    pub fn stroke_f(
        &mut self,
        layer: usize,
        frame: usize,
        pts: &[(f32, f32, f32)],
        color: [u8; 4],
        aa: bool,
        snap: bool,
    ) -> Result<(), String> {
        if pts.is_empty() {
            return Err("stroke needs at least one point".into());
        }
        let pal = self.meta.palette.clone();
        let f: Vec<(f32, f32, f32)> = pts
            .iter()
            .map(|&(x, y, w)| (x, y, w.max(0.0) / 2.0))
            .collect();
        let img = self.cel_canvas(layer, frame)?;
        raster::stroke_ribbon(img, &f, color, aa);
        if snap && !pal.is_empty() {
            self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
        }
        Ok(())
    }

    /// Stroke a Bezier curve through the control polygon `points` (2 = line,
    /// 3 = quadratic, 4 = cubic, more = higher degree) with a constant
    /// `width`. The polygon is flattened to a sub-pixel polyline and painted
    /// by the same coverage core as [`Self::stroke_f`] — same `color`, `aa`
    /// and `snap` semantics as a straight stroke, bent. Fewer than 2 control
    /// points is not a curve and errors.
    pub fn curve(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(f32, f32)],
        color: [u8; 4],
        width: f32,
        aa: bool,
        snap: bool,
    ) -> Result<(), String> {
        if points.len() < 2 {
            return Err("curve needs at least 2 control points".into());
        }
        let flat = raster::flatten_bezier(points, CURVE_TOLERANCE);
        let pts: Vec<(f32, f32, f32)> = flat.iter().map(|&(x, y)| (x, y, width)).collect();
        self.stroke_f(layer, frame, &pts, color, aa, snap)
    }

    /// Stamp `text` left-to-right with the built-in 3×5 pixel font, top-left at
    /// (x,y). `size` is the integer pixel scale of each cell (so a glyph is
    /// 3·size wide, 5·size tall), clamped to 1..=64 to bound the inner loops.
    /// Glyphs are separated by one scaled pixel of spacing. Lowercase maps to
    /// uppercase; unknown chars render as a hollow box. Returns the rendered width
    /// in document pixels so callers can lay out the next line/element. HUD text,
    /// damage numbers, lettering.
    pub fn text(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        text: &str,
        color: [u8; 4],
        size: i32,
    ) -> Result<i32, String> {
        let s = size.clamp(1, 64); // bound the s×s inner loops against huge tool input
        let advance = (raster::GLYPH_W + 1) * s; // glyph cell + 1px scaled spacing
        let img = self.cel_canvas(layer, frame)?;
        let mut pen = x;
        for ch in text.chars() {
            let bits = raster::glyph(ch);
            for gy in 0..raster::GLYPH_H {
                for gx in 0..raster::GLYPH_W {
                    // Left column is the high bit of each 3-bit row.
                    let bit = gy * raster::GLYPH_W + (raster::GLYPH_W - 1 - gx);
                    if (bits >> bit) & 1 == 0 {
                        continue;
                    }
                    // Scale the lit cell into an s×s block.
                    for dy in 0..s {
                        for dx in 0..s {
                            raster::put(img, pen + gx * s + dx, y + gy * s + dy, color);
                        }
                    }
                }
            }
            pen += advance;
        }
        // Trim the trailing inter-glyph spacing from the reported width.
        Ok(if text.is_empty() { 0 } else { pen - x - s })
    }

    /// Mirror a cel across a vertical axis (column `vertical`) and/or a
    /// horizontal axis (row `horizontal`). `keep_left`/`keep_top` choose which
    /// side is the source that gets reflected onto the other. Draw half a sprite,
    /// mirror it for instant symmetry.
    pub fn symmetry(
        &mut self,
        layer: usize,
        frame: usize,
        vertical: Option<i32>,
        horizontal: Option<i32>,
        keep_left: bool,
        keep_top: bool,
    ) -> Result<(), String> {
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let img = self.cel_canvas(layer, frame)?;
        if let Some(ax) = vertical {
            for y in 0..h {
                for x in 0..w {
                    let on_src = if keep_left { x < ax } else { x > ax };
                    if on_src {
                        // i64: `2 * ax` overflows i32 for an axis far off-canvas.
                        let mx = 2 * ax as i64 - x as i64;
                        if mx >= 0 && mx < w as i64 {
                            let p = *img.get_pixel(x as u32, y as u32);
                            img.put_pixel(mx as u32, y as u32, p);
                        }
                    }
                }
            }
        }
        if let Some(ay) = horizontal {
            for y in 0..h {
                for x in 0..w {
                    let on_src = if keep_top { y < ay } else { y > ay };
                    if on_src {
                        let my = 2 * ay as i64 - y as i64;
                        if my >= 0 && my < h as i64 {
                            let p = *img.get_pixel(x as u32, y as u32);
                            img.put_pixel(x as u32, my as u32, p);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Paint a whole region declaratively from a character grid — the inverse
    /// of dump_region. Each row string is one pixel row starting at (ox,oy);
    /// every character must be in `legend` ('.'/' ' skip, leaving the pixel
    /// untouched). LLMs emit a sprite as a grid far more reliably than as a
    /// sequence of absolute-coordinate draw calls — this removes the
    /// coordinate-math failure class. Returns `(painted, clipped)`.
    pub fn paint_grid(
        &mut self,
        layer: usize,
        frame: usize,
        ox: i32,
        oy: i32,
        legend: &std::collections::HashMap<char, [u8; 4]>,
        rows: &[String],
    ) -> Result<(u64, u64), String> {
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let img = self.cel_canvas(layer, frame)?;
        let (mut painted, mut clipped) = (0u64, 0u64);
        for (ry, row) in rows.iter().enumerate() {
            for (rx, ch) in row.chars().enumerate() {
                if ch == '.' || ch == ' ' {
                    continue;
                }
                let color = legend.get(&ch).ok_or_else(|| {
                    format!(
                        "row {} col {}: character '{}' is not in the legend",
                        ry, rx, ch
                    )
                })?;
                let (tx, ty) = (ox + rx as i32, oy + ry as i32);
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    clipped += 1;
                    continue;
                }
                img.put_pixel(tx as u32, ty as u32, Rgba(*color));
                painted += 1;
            }
        }
        Ok((painted, clipped))
    }
}

#[cfg(test)]
mod xform_tests {
    use super::*;

    fn px(v: u8) -> [u8; 4] {
        [v, 0, 0, 255]
    }

    /// 2×2 cel with one identifiable shade per pixel.
    fn tagged_cel() -> RgbaImage {
        let mut cel = RgbaImage::new(2, 2);
        cel.put_pixel(0, 0, Rgba(px(10)));
        cel.put_pixel(1, 0, Rgba(px(20)));
        cel.put_pixel(0, 1, Rgba(px(30)));
        cel.put_pixel(1, 1, Rgba(px(40)));
        cel
    }

    #[test]
    fn rotate_cel_turns_content_clockwise_in_place() {
        // 4 wide × 2 tall canvas: a 90° CW turn sends the top edge to the
        // right — the middle two columns survive as a centred 2×2 block, the
        // outer columns rotate off the top/bottom and clip.
        let mut d = Document::new("t", 4, 2);
        d.pencil(0, 0, &[(1, 0)], px(10), 1).unwrap();
        d.pencil(0, 0, &[(2, 0)], px(20), 1).unwrap();
        d.pencil(0, 0, &[(1, 1)], px(30), 1).unwrap();
        d.pencil(0, 0, &[(2, 1)], px(40), 1).unwrap();
        d.pencil(0, 0, &[(0, 0)], px(50), 1).unwrap(); // clips
        d.pencil(0, 0, &[(3, 1)], px(60), 1).unwrap(); // clips
        d.dirty.clear(); // isolate the op's own dirty-marking
        d.rotate_cel(0, 0, 1).unwrap();
        assert_eq!(d.get_pixel(0, 0, 2, 0).unwrap(), px(10)); // top -> right
        assert_eq!(d.get_pixel(0, 0, 2, 1).unwrap(), px(20));
        assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), px(30));
        assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), px(40));
        for &(x, y) in &[(0, 0), (3, 0), (0, 1), (3, 1)] {
            assert_eq!(d.get_pixel(0, 0, x, y).unwrap(), [0, 0, 0, 0], "({x},{y})");
        }
        // Canvas size is untouched and the op marks the cel for save.
        assert_eq!((d.meta().w, d.meta().h), (4, 2));
        assert!(d.dirty.contains(&(0, 0)));
    }

    #[test]
    fn rotate_cel_full_turns_180s_and_bad_indices() {
        let mut d = Document::new("t", 3, 2);
        d.pencil(0, 0, &[(0, 0)], px(10), 1).unwrap();
        d.rotate_cel(0, 0, 0).unwrap();
        d.rotate_cel(0, 0, 4).unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), px(10));
        // 180 twice returns the art exactly, even on a non-square canvas.
        d.rotate_cel(0, 0, 2).unwrap();
        d.rotate_cel(0, 0, 2).unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), px(10));
        assert!(d.rotate_cel(9, 0, 1).is_err());
        assert!(d.rotate_cel(0, 9, 1).is_err());
    }

    #[test]
    fn scale_cel_resizes_the_cel_and_keeps_its_anchor() {
        let mut d = Document::new("t", 8, 8);
        d.set_cel(0, 0, 3, 2, tagged_cel()).unwrap();
        d.dirty.clear();
        d.scale_cel(0, 0, 4, 4, "nearest").unwrap();
        // Each source pixel is now a 2×2 block hanging off the same anchor.
        assert_eq!(d.get_pixel(0, 0, 3, 2).unwrap(), px(10));
        assert_eq!(d.get_pixel(0, 0, 4, 3).unwrap(), px(10));
        assert_eq!(d.get_pixel(0, 0, 6, 5).unwrap(), px(40));
        // Outside the cel still reads transparent; the canvas didn't move.
        assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [0, 0, 0, 0]);
        assert_eq!(d.get_pixel(0, 0, 7, 6).unwrap(), [0, 0, 0, 0]);
        assert_eq!((d.meta().w, d.meta().h), (8, 8));
        let (x, y, img) = d.cels.get(&(0, 0)).unwrap();
        assert_eq!((*x, *y), (3, 2), "anchor must survive the resize");
        assert_eq!(img.dimensions(), (4, 4));
        assert!(d.dirty.contains(&(0, 0)));
    }

    #[test]
    fn scale_cel_area_average_shrinks_to_the_block_mean() {
        let mut d = Document::new("t", 8, 8);
        d.set_cel(0, 0, 0, 0, tagged_cel()).unwrap();
        d.scale_cel(0, 0, 1, 1, "area-average").unwrap();
        // Mean of 10/20/30/40 with opaque alpha throughout.
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), px(25));
    }

    #[test]
    fn scale_cel_rejects_bad_input_loudly() {
        let mut d = Document::new("t", 8, 8);
        // Nothing to scale is a caller mistake, not a no-op.
        assert!(d
            .scale_cel(0, 0, 4, 4, "nearest")
            .unwrap_err()
            .contains("no cel"));
        d.set_cel(0, 0, 0, 0, tagged_cel()).unwrap();
        assert!(d.scale_cel(0, 0, 0, 4, "nearest").is_err());
        assert!(d.scale_cel(0, 0, 4, 0, "nearest").is_err());
        assert!(d
            .scale_cel(0, 0, 4, 4, "cubic")
            .unwrap_err()
            .contains("unknown scale method"));
        assert!(d
            .scale_cel(9, 0, 4, 4, "nearest")
            .unwrap_err()
            .contains("no layer"));
        // Failed calls leave the cel alone.
        assert_eq!(d.cels.get(&(0, 0)).unwrap().2.dimensions(), (2, 2));
    }

    #[test]
    fn rotate_and_scale_persist_through_save() {
        // The dirty marking is only real if a save writes the changed cel:
        // save clears the dirty set, so a skipped mark would leave the OLD
        // pixels on disk and the reload below would catch it.
        let dir = std::env::temp_dir().join(format!("atelier-xform-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 0)], px(10), 1).unwrap();
        d.save(&dir).unwrap();
        d.rotate_cel(0, 0, 1).unwrap();
        d.save(&dir).unwrap();
        let back = Document::load(&dir).unwrap();
        // 90° CW on a square sends the top-left corner to the top-right.
        assert_eq!(back.get_pixel(0, 0, 3, 0).unwrap(), px(10));
        assert_eq!(back.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);

        let dir2 = std::env::temp_dir().join(format!("atelier-xform2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir2);
        let mut d2 = Document::new("t", 8, 8);
        d2.set_cel(0, 0, 1, 1, tagged_cel()).unwrap();
        d2.save(&dir2).unwrap();
        d2.scale_cel(0, 0, 4, 4, "nearest").unwrap();
        d2.save(&dir2).unwrap();
        let back2 = Document::load(&dir2).unwrap();
        // Anchor preserved, content doubled: the 2×2 cel now spans (1,1)..(4,4).
        assert_eq!(back2.get_pixel(0, 0, 1, 1).unwrap(), px(10));
        assert_eq!(back2.get_pixel(0, 0, 4, 4).unwrap(), px(40));
        assert_eq!(back2.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_paints_both_endpoints_with_hard_pixels() {
        let mut d = Document::new("t", 16, 16);
        d.curve(
            0,
            0,
            &[(1.0, 8.0), (8.0, 1.0), (15.0, 8.0)],
            [255, 0, 0, 255],
            1.0,
            false,
            false,
        )
        .unwrap();
        // The curve lands exactly on the control polygon's endpoints.
        assert_eq!(d.get_pixel(0, 0, 1, 8).unwrap(), [255, 0, 0, 255]);
        assert_eq!(d.get_pixel(0, 0, 15, 8).unwrap(), [255, 0, 0, 255]);
        // ... and its middle is pulled toward the (8,1) control point, off
        // the y=8 chord a plain line would paint.
        let lifted = (0..8).any(|y| d.get_pixel(0, 0, 8, y).unwrap()[3] == 255);
        assert!(lifted, "column x=8 stayed on the chord — no curve");
        // aa=false: hard pixels only, no fractional coverage anywhere.
        for y in 0..16 {
            for x in 0..16 {
                let a = d.get_pixel(0, 0, x, y).unwrap()[3];
                assert!(a == 0 || a == 255, "({x},{y}) has fractional alpha {a}");
            }
        }
    }

    #[test]
    fn curve_with_fewer_than_two_points_errors() {
        let mut d = Document::new("t", 4, 4);
        assert!(d
            .curve(0, 0, &[], [0, 0, 0, 255], 1.0, true, false)
            .is_err());
        assert!(d
            .curve(0, 0, &[(1.0, 1.0)], [0, 0, 0, 255], 1.0, true, false)
            .is_err());
    }
}
