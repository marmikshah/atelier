//! Brush stamping and rectangular transforms.

use image::RgbaImage;

use crate::raster;

use super::Document;

impl Document {
    /// Read a rectangular block for `move_region`. Out-of-cel pixels are
    /// transparent; inclusive corners are normalised and clamped to canvas.
    fn read_block(
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

    /// Write a flat RGBA block onto a cel for internal transforms. Transparent
    /// source pixels leave the destination unchanged.
    fn write_block(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        rw: u32,
        rh: u32,
        buf: &[u8],
    ) -> Result<(), String> {
        if buf.len() != (rw * rh * 4) as usize {
            return Err(format!("buffer length {} != {}x{}x4", buf.len(), rw, rh));
        }
        let img = self.cel_canvas(layer, frame)?;
        for ry in 0..rh as i32 {
            for rx in 0..rw as i32 {
                let i = ((ry as u32 * rw + rx as u32) * 4) as usize;
                let p = [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]];
                if p[3] == 0 {
                    continue;
                }
                raster::put(img, x + rx, y + ry, p);
            }
        }
        Ok(())
    }

    /// Stamp a brush-tip image at every point — the per-dab primitive behind a
    /// custom-brush `stamp` op. Each point is the tip's CENTRE: the top-left
    /// corner is `point - (w/2, h/2)` with integer division, the deterministic
    /// half-size rounding `raster::brush` already uses (an even-sized tip has
    /// no true centre pixel; its point is the lower-right of the middle ones).
    /// `colorize` is the classic "brush shape, current colour": the tip acts
    /// as an alpha mask — RGB from the colour, alpha = tip.a × colour.a (the
    /// `drop_shadow` tint) — while None stamps the tip's pixels verbatim.
    /// A fully transparent tip pixel keeps the destination; anything else
    /// overwrites. Returns the pixels painted, counting every in-canvas write
    /// — overlapping dabs count a shared pixel once per dab. Off-canvas points
    /// clip silently.
    pub fn stamp_tip(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        tip: &RgbaImage,
        colorize: Option<[u8; 4]>,
    ) -> Result<usize, String> {
        let (tw, th) = (tip.width(), tip.height());
        if tw == 0 || th == 0 {
            return Err(format!("stamp tip is empty ({tw}x{th})"));
        }
        // Validate the target even when there is nothing to paint — a wrong
        // index must fail loudly, not report Ok(0) (the clear_cel fix).
        self.check_cel(layer, frame)?;
        if points.is_empty() {
            return Ok(0);
        }
        // Tint once for the whole point list: every dab uses the same
        // effective buffer, so tinting per point would redo identical work.
        let tinted = colorize.map(|c| {
            tip.pixels()
                .flat_map(|p| {
                    let a = (p.0[3] as f32 * (c[3] as f32 / 255.0))
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    [c[0], c[1], c[2], a]
                })
                .collect::<Vec<u8>>()
        });
        let buf: &[u8] = match &tinted {
            Some(t) => t,
            None => tip.as_raw(),
        };
        let (cw, ch) = (self.meta.w as i32, self.meta.h as i32);
        let mut painted = 0usize;
        for &(px, py) in points {
            // Saturate the origin (i64 math, like move_region) so neither this
            // offset nor write_block's `x + rx` walk can overflow i32 on an
            // absurd point — a dab that far out clips to nothing anyway.
            let ox = (px as i64 - tw as i64 / 2)
                .clamp(i32::MIN as i64, i32::MAX as i64 - tw as i64 + 1)
                as i32;
            let oy = (py as i64 - th as i64 / 2)
                .clamp(i32::MIN as i64, i32::MAX as i64 - th as i64 + 1)
                as i32;
            // write_block reports nothing, so tally its writes up front: the
            // alpha>0 source pixels that land inside the canvas.
            for ry in 0..th as i32 {
                for rx in 0..tw as i32 {
                    let a = buf[((ry as u32 * tw + rx as u32) * 4 + 3) as usize];
                    let (tx, ty) = (ox + rx, oy + ry);
                    if a > 0 && tx >= 0 && ty >= 0 && tx < cw && ty < ch {
                        painted += 1;
                    }
                }
            }
            self.write_block(layer, frame, ox, oy, tw, th, buf)?;
        }
        Ok(painted)
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
        let (rw, rh, buf) = self.read_block(layer, frame, x0, y0, x1, y1)?;
        let (ax, ay) = (x0.min(x1).max(0), y0.min(y1).max(0));
        self.clear_region(layer, frame, x0, y0, x1, y1)?;
        // i64 math: `ax + dx` overflows i32 for an absurd (but accepted) delta;
        // saturate instead of panicking in debug / wrapping in release.
        let px = (ax as i64 + dx as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let py = (ay as i64 + dy as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        self.write_block(layer, frame, px, py, rw, rh, &buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn stamp_tip_paints_every_point_verbatim() {
        let mut d = Document::new("t", 10, 10);
        // Two-colour tip: verbatim stamping must carry both exactly.
        let mut tip = RgbaImage::from_pixel(2, 2, Rgba([4, 5, 6, 200]));
        tip.put_pixel(0, 0, Rgba([1, 2, 3, 255]));
        let painted = d.stamp_tip(0, 0, &[(3, 3), (6, 6)], &tip, None).unwrap();
        assert_eq!(painted, 8);
        // A 2x2 dab centred on (3,3) covers (2,2)..=(3,3); the tip's (0,0)
        // pixel lands at (2,2), its neighbours verbatim.
        assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [1, 2, 3, 255]);
        assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [4, 5, 6, 200]);
        assert_eq!(d.get_pixel(0, 0, 5, 5).unwrap(), [1, 2, 3, 255]);
        assert_eq!(d.get_pixel(0, 0, 6, 6).unwrap(), [4, 5, 6, 200]);
        // The gap between the dabs is untouched.
        assert_eq!(d.get_pixel(0, 0, 4, 4).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn stamp_tip_centres_odd_and_even_tips_exactly() {
        let mut d = Document::new("t", 12, 12);
        let odd = RgbaImage::from_pixel(3, 3, Rgba([9, 9, 9, 255]));
        d.stamp_tip(0, 0, &[(5, 5)], &odd, None).unwrap();
        // 3x3: the point is the exact middle pixel — the dab spans 4..=6.
        for (x, y) in [(4, 4), (6, 6), (5, 5), (6, 4), (4, 6)] {
            assert_eq!(
                d.get_pixel(0, 0, x, y).unwrap(),
                [9, 9, 9, 255],
                "({x},{y})"
            );
        }
        for (x, y) in [(3, 5), (7, 5), (5, 3), (5, 7)] {
            assert_eq!(d.get_pixel(0, 0, x, y).unwrap(), [0, 0, 0, 0], "({x},{y})");
        }
        // 2x2: no true centre pixel — w/2 integer division makes the point
        // the lower-right of the middle pixels, so the dab spans 7..=8.
        let even = RgbaImage::from_pixel(2, 2, Rgba([8, 8, 8, 255]));
        d.stamp_tip(0, 0, &[(8, 8)], &even, None).unwrap();
        for (x, y) in [(7, 7), (8, 8), (8, 7), (7, 8)] {
            assert_eq!(
                d.get_pixel(0, 0, x, y).unwrap(),
                [8, 8, 8, 255],
                "({x},{y})"
            );
        }
        for (x, y) in [(9, 8), (8, 9), (6, 8)] {
            assert_eq!(d.get_pixel(0, 0, x, y).unwrap(), [0, 0, 0, 0], "({x},{y})");
        }
    }

    #[test]
    fn stamp_tip_colorize_tints_rgb_and_keeps_tip_alpha() {
        let mut d = Document::new("t", 8, 8);
        // Alpha-gradient tip with a loud RGB of its own: colorize must
        // replace the RGB but keep the per-pixel alpha shape.
        let mut tip = RgbaImage::new(3, 1);
        tip.put_pixel(0, 0, Rgba([250, 100, 50, 255]));
        tip.put_pixel(1, 0, Rgba([250, 100, 50, 128]));
        tip.put_pixel(2, 0, Rgba([250, 100, 50, 0]));
        d.stamp_tip(0, 0, &[(4, 4)], &tip, Some([10, 20, 30, 255]))
            .unwrap();
        // A 3x1 dab centred on (4,4) spans (3,4)..=(5,4).
        assert_eq!(d.get_pixel(0, 0, 3, 4).unwrap(), [10, 20, 30, 255]);
        assert_eq!(d.get_pixel(0, 0, 4, 4).unwrap(), [10, 20, 30, 128]);
        // tip alpha 0 + source-over: the destination is kept, not painted.
        assert_eq!(d.get_pixel(0, 0, 5, 4).unwrap(), [0, 0, 0, 0]);
        // A translucent colour multiplies: round(128 × 200/255) = 100.
        d.stamp_tip(0, 0, &[(1, 1)], &tip, Some([10, 20, 30, 200]))
            .unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 1).unwrap(), [10, 20, 30, 200]);
        assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [10, 20, 30, 100]);
    }

    #[test]
    fn stamp_tip_clips_off_canvas_points_without_wrapping() {
        let mut d = Document::new("t", 8, 8);
        let tip = RgbaImage::from_pixel(3, 3, Rgba([7, 7, 7, 255]));
        let painted = d
            .stamp_tip(0, 0, &[(0, 0), (7, 7), (-50, -50)], &tip, None)
            .unwrap();
        // Corner dabs keep only their in-canvas quadrant; the far-off point
        // paints nothing — and nothing wraps around to the opposite edge.
        assert_eq!(painted, 8);
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [7, 7, 7, 255]);
        assert_eq!(d.get_pixel(0, 0, 7, 7).unwrap(), [7, 7, 7, 255]);
        assert_eq!(d.get_pixel(0, 0, 7, 0).unwrap(), [0, 0, 0, 0]);
        assert_eq!(d.get_pixel(0, 0, 0, 7).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn stamp_tip_rejects_a_zero_size_tip() {
        let mut d = Document::new("t", 4, 4);
        let e = d
            .stamp_tip(0, 0, &[(1, 1)], &RgbaImage::new(0, 0), None)
            .unwrap_err();
        assert!(e.contains("empty"), "{e}");
        assert!(d.cel_keys().is_empty(), "a rejected stamp must not paint");
    }

    #[test]
    fn stamp_tip_with_no_points_is_a_noop() {
        let mut d = Document::new("t", 4, 4);
        let tip = RgbaImage::from_pixel(2, 2, Rgba([1, 1, 1, 255]));
        assert_eq!(d.stamp_tip(0, 0, &[], &tip, None).unwrap(), 0);
        // Nothing painted: no cel materialised, nothing marked dirty.
        assert!(d.cel_keys().is_empty());
        // But a bad target still fails loudly, like every sibling region op.
        assert!(d.stamp_tip(9, 0, &[], &tip, None).is_err());
    }
}
