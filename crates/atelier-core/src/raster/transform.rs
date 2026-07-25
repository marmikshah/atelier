//! Geometric transforms and scalar fields: quarter-turn rotation, scaling,
//! area downscale, interior distance fields and background removal.

use image::{Rgba, RgbaImage};

use super::colour::oklab_delta;

/// How `scale` resamples: `Nearest` replicates whole source pixels (the crisp
/// pixel-art upscale), `AreaAverage` box-filters (alpha-weighted, so thin
/// outlines survive a shrink where nearest drops them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleMethod {
    Nearest,
    AreaAverage,
}

/// Resize to an exact `w`×`h`. A zero dimension clamps to 1: the raster
/// primitive stays total, while the Document wrapper rejects 0 where a
/// `Result` exists.
pub fn scale(img: &RgbaImage, w: u32, h: u32, method: ScaleMethod) -> RgbaImage {
    let (w, h) = (w.max(1), h.max(1));
    let (sw, sh) = (img.width(), img.height());
    // Nearest is also the area filter's own answer when nothing shrinks.
    if method == ScaleMethod::Nearest || (w >= sw && h >= sh) {
        return image::imageops::resize(img, w, h, image::imageops::FilterType::Nearest);
    }
    let mut out = RgbaImage::new(w, h);
    let fx = sw as f64 / w as f64;
    let fy = sh as f64 / h as f64;
    for ty in 0..h {
        let (y0, y1) = (ty as f64 * fy, (ty + 1) as f64 * fy);
        for tx in 0..w {
            let (x0, x1) = (tx as f64 * fx, (tx + 1) as f64 * fx);
            let (mut r, mut g, mut b, mut a, mut area) = (0f64, 0f64, 0f64, 0f64, 0f64);
            let mut sy = y0.floor() as u32;
            while (sy as f64) < y1 && sy < sh {
                let hy = y1.min(sy as f64 + 1.0) - y0.max(sy as f64);
                let mut sx = x0.floor() as u32;
                while (sx as f64) < x1 && sx < sw {
                    let wx = x1.min(sx as f64 + 1.0) - x0.max(sx as f64);
                    let wgt = wx * hy;
                    let p = img.get_pixel(sx, sy).0;
                    let pa = p[3] as f64 / 255.0;
                    r += p[0] as f64 * pa * wgt;
                    g += p[1] as f64 * pa * wgt;
                    b += p[2] as f64 * pa * wgt;
                    a += pa * wgt;
                    area += wgt;
                    sx += 1;
                }
                sy += 1;
            }
            let px = if a > 1e-9 {
                [
                    (r / a).round().clamp(0.0, 255.0) as u8,
                    (g / a).round().clamp(0.0, 255.0) as u8,
                    (b / a).round().clamp(0.0, 255.0) as u8,
                    (a / area.max(1e-9) * 255.0).round().clamp(0.0, 255.0) as u8,
                ]
            } else {
                [0, 0, 0, 0]
            };
            out.put_pixel(tx, ty, Rgba(px));
        }
    }
    out
}

/// Rotate about the centre by `turns_cw` quarter-turns CLOCKWISE (1 = 90°,
/// 2 = 180°, 3 = 270°; values wrap mod 4). The output keeps the INPUT's
/// dimensions — the canvas must never change size: content rotates about the
/// centre, what turns outside clips, and the vacated corners come back
/// transparent. Exact for square images; on odd turns `imageops` swaps
/// w×h ↔ h×w, so the rotated rectangle is re-anchored about the same centre
/// (half-pixel offsets round away from zero).
pub fn rotate_quarters(img: &RgbaImage, turns_cw: u8) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    match turns_cw % 4 {
        0 => img.clone(),
        2 => image::imageops::rotate180(img),
        turns => {
            // imageops quarter-turns are clockwise (rotate90: top edge →
            // right edge) and swap dimensions — re-anchor about the centre.
            let rotated = if turns == 1 {
                image::imageops::rotate90(img)
            } else {
                image::imageops::rotate270(img)
            };
            let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
            let ox = ((w as f64 - h as f64) / 2.0).round() as i32;
            let oy = ((h as f64 - w as f64) / 2.0).round() as i32;
            for y in 0..rotated.height() as i32 {
                for x in 0..rotated.width() as i32 {
                    let (tx, ty) = (x + ox, y + oy);
                    if tx >= 0 && ty >= 0 && (tx as u32) < w && (ty as u32) < h {
                        out.put_pixel(tx as u32, ty as u32, *rotated.get_pixel(x as u32, y as u32));
                    }
                }
            }
            out
        }
    }
}

/// Corner-seeded background removal: flood from each opaque corner over pixels
/// perceptually close to that corner's colour (OKLab ΔE ≤ `tol`), zeroing
/// their alpha. Run BEFORE palette extraction so a backdrop can't steal
/// palette slots from the subject. Corner seeds (not whole-border seeds) on
/// purpose: a subject that touches a canvas edge — feet at the bottom, a
/// full-bleed portrait — must not seed its own deletion. The cost is that
/// strongly-graded backdrops clear only the corner-toned bands; flat and
/// gently-graded backdrops clear fully. Leaves enclosed interior regions alone.
pub fn remove_background(img: &mut RgbaImage, tol: f32) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    if w == 0 || h == 0 {
        return;
    }
    let mut cleared = vec![false; (w * h) as usize];
    let mut stack: Vec<(i32, i32, [u8; 4])> = Vec::new();
    for &(cx, cy) in &[(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        let seed = img.get_pixel(cx as u32, cy as u32).0;
        if seed[3] > 0 {
            stack.push((cx, cy, seed));
        }
    }
    while let Some((x, y, seed)) = stack.pop() {
        if x < 0 || y < 0 || x >= w || y >= h {
            continue;
        }
        let i = (y * w + x) as usize;
        if cleared[i] {
            continue;
        }
        let p = img.get_pixel(x as u32, y as u32).0;
        if p[3] == 0 || oklab_delta(p, seed) > tol {
            continue;
        }
        cleared[i] = true;
        img.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
        stack.push((x + 1, y, seed));
        stack.push((x - 1, y, seed));
        stack.push((x, y + 1, seed));
        stack.push((x, y - 1, seed));
    }
}

/// Normalised interior distance for each cell of a `w`×`h` boolean foreground
/// mask `fg`: 0 on background (and the outermost foreground rim), rising toward
/// 1 at the most-interior foreground pixel. A two-pass 1/√2-weight chamfer
/// distance transform, divided by its max so the field is resolution- and
/// shape-independent. Used by `Document::form` "auto" to give an arbitrary blob
/// volume (bright core, dark edges) without assuming an elliptical outline.
pub fn interior_distance(fg: &[bool], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    const BIG: f32 = 1.0e9;
    const D1: f32 = 1.0;
    const D2: f32 = std::f32::consts::SQRT_2;
    let idx = |x: usize, y: usize| y * w + x;
    let mut d = vec![0.0f32; n];
    for i in 0..n {
        d[i] = if fg[i] { BIG } else { 0.0 };
    }
    // Forward pass (top-left → bottom-right).
    for y in 0..h {
        for x in 0..w {
            if !fg[idx(x, y)] {
                continue;
            }
            let mut m = d[idx(x, y)];
            if x > 0 {
                m = m.min(d[idx(x - 1, y)] + D1);
            }
            if y > 0 {
                m = m.min(d[idx(x, y - 1)] + D1);
            }
            if x > 0 && y > 0 {
                m = m.min(d[idx(x - 1, y - 1)] + D2);
            }
            if x + 1 < w && y > 0 {
                m = m.min(d[idx(x + 1, y - 1)] + D2);
            }
            d[idx(x, y)] = m;
        }
    }
    // Backward pass (bottom-right → top-left).
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            if !fg[idx(x, y)] {
                continue;
            }
            let mut m = d[idx(x, y)];
            if x + 1 < w {
                m = m.min(d[idx(x + 1, y)] + D1);
            }
            if y + 1 < h {
                m = m.min(d[idx(x, y + 1)] + D1);
            }
            if x + 1 < w && y + 1 < h {
                m = m.min(d[idx(x + 1, y + 1)] + D2);
            }
            if x > 0 && y + 1 < h {
                m = m.min(d[idx(x - 1, y + 1)] + D2);
            }
            d[idx(x, y)] = m;
        }
    }
    let mx = d.iter().copied().fold(0.0f32, f32::max).max(1.0);
    for v in d.iter_mut() {
        *v /= mx;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pixel identifiable: red channel = 10 + 20·(y·w + x).
    fn tagged(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = (10 + 20 * (y * w + x)) as u8;
                img.put_pixel(x, y, Rgba([v, 0, 0, 255]));
            }
        }
        img
    }

    /// ImageBuffer has no PartialEq — compare dims + raw bytes.
    fn same(a: &RgbaImage, b: &RgbaImage) -> bool {
        a.dimensions() == b.dimensions() && a.as_raw() == b.as_raw()
    }

    #[test]
    fn rotate_quarters_cw90_keeps_dims_and_clips_the_far_corners() {
        // 2 wide × 4 tall: a 90° CW turn makes the content 4 wide × 2 tall
        // about the same centre, so the top and bottom rows rotate off the
        // sides and only the middle band survives, re-centred.
        let src = tagged(2, 4);
        let out = rotate_quarters(&src, 1);
        assert_eq!(out.dimensions(), (2, 4), "canvas must not change size");
        // The surviving band lands centred: source (x,y) -> output (2-y, x+1).
        assert_eq!(out.get_pixel(1, 1).0, [50, 0, 0, 255]); // was (0,1)
        assert_eq!(out.get_pixel(1, 2).0, [70, 0, 0, 255]); // was (1,1)
        assert_eq!(out.get_pixel(0, 1).0, [90, 0, 0, 255]); // was (0,2)
        assert_eq!(out.get_pixel(0, 2).0, [110, 0, 0, 255]); // was (1,2)
        // Rows 0 and 3 are the corners the turn vacated.
        for x in 0..2 {
            assert_eq!(out.get_pixel(x, 0).0, [0, 0, 0, 0], "({x},0)");
            assert_eq!(out.get_pixel(x, 3).0, [0, 0, 0, 0], "({x},3)");
        }
        // The top/bottom source rows (10, 30, 130, 150) rotated off-canvas.
        for p in out.pixels() {
            assert!(!matches!(p.0[0], 10 | 30 | 130 | 150));
        }
    }

    #[test]
    fn rotate_quarters_square_turn_is_exact() {
        // No clipping on a square: [a b; c d] -> CW -> [c a; d b].
        let src = tagged(2, 2);
        let out = rotate_quarters(&src, 1);
        assert_eq!(out.get_pixel(0, 0).0, [50, 0, 0, 255]);
        assert_eq!(out.get_pixel(1, 0).0, [10, 0, 0, 255]);
        assert_eq!(out.get_pixel(0, 1).0, [70, 0, 0, 255]);
        assert_eq!(out.get_pixel(1, 1).0, [30, 0, 0, 255]);
    }

    #[test]
    fn rotate_quarters_180_twice_is_identity_on_a_non_square_image() {
        let src = tagged(2, 4);
        assert!(same(&rotate_quarters(&rotate_quarters(&src, 2), 2), &src));
    }

    #[test]
    fn rotate_quarters_inverse_turns_cancel_and_full_turns_are_no_ops() {
        let src = tagged(3, 3);
        assert!(same(&rotate_quarters(&rotate_quarters(&src, 1), 3), &src));
        assert!(same(&rotate_quarters(&rotate_quarters(&src, 3), 1), &src));
        assert!(same(&rotate_quarters(&src, 0), &src));
        assert!(same(&rotate_quarters(&src, 4), &src));
    }

    #[test]
    fn scale_nearest_replicates_each_pixel_as_a_block() {
        let src = tagged(2, 2);
        let out = scale(&src, 4, 4, ScaleMethod::Nearest);
        assert_eq!(out.dimensions(), (4, 4));
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    out.get_pixel(x, y).0,
                    src.get_pixel(x / 2, y / 2).0,
                    "({x},{y})"
                );
            }
        }
    }

    #[test]
    fn scale_area_average_averages_each_source_block() {
        // 4×4 -> 2×2: each output pixel is the mean of its exact 2×2 source
        // footprint. Values x·40 + y·15 give half-integer means on purpose,
        // pinning the rounding too.
        let mut src = RgbaImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                src.put_pixel(x, y, Rgba([(x * 40 + y * 15) as u8, 0, 0, 255]));
            }
        }
        let out = scale(&src, 2, 2, ScaleMethod::AreaAverage);
        assert_eq!(out.dimensions(), (2, 2));
        assert_eq!(out.get_pixel(0, 0).0, [28, 0, 0, 255]); // 27.5
        assert_eq!(out.get_pixel(1, 0).0, [108, 0, 0, 255]); // 107.5
        assert_eq!(out.get_pixel(0, 1).0, [58, 0, 0, 255]); // 57.5
        assert_eq!(out.get_pixel(1, 1).0, [138, 0, 0, 255]); // 137.5
    }

    #[test]
    fn scale_clamps_zero_dimensions_to_one() {
        // The primitive stays total; the Document wrapper rejects 0 loudly.
        let src = tagged(2, 2);
        assert_eq!(scale(&src, 0, 0, ScaleMethod::Nearest).dimensions(), (1, 1));
        assert_eq!(
            scale(&src, 0, 3, ScaleMethod::AreaAverage).dimensions(),
            (1, 3)
        );
    }

    #[test]
    fn area_scale_uses_nearest_when_growing() {
        let src = tagged(2, 2);
        let out = scale(&src, 4, 4, ScaleMethod::AreaAverage);
        assert_eq!(out.dimensions(), (4, 4));
        assert_eq!(out.get_pixel(0, 0).0, src.get_pixel(0, 0).0);
        assert_eq!(out.get_pixel(3, 3).0, src.get_pixel(1, 1).0);
    }
}
