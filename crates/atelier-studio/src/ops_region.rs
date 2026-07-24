//! The active pixel selection (`doc_select`) and the cross-document clipboard
//! (`doc_region`): process-lived state on the `Studio` that painting ops
//! consult before touching a cel.

use serde_json::{Value, json};

use super::{ColorSelect, Studio};

/// A copied rectangular region: width, height, flat RGBA buffer.
pub(crate) type Clip = (u32, u32, Vec<u8>);

/// An active pixel selection: which document it belongs to, its dimensions, and
/// one `bool` per pixel (row-major). Painting ops confine to the `true` pixels.
#[derive(Clone)]
pub(crate) struct Selection {
    pub doc_id: String,
    pub w: u32,
    pub h: u32,
    pub mask: Vec<bool>,
}

/// How a new shape combines with the selection already on the document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SelectMode {
    Replace,
    Add,
    Subtract,
    Intersect,
}

impl SelectMode {
    /// Parse a caller-supplied mode.
    ///
    /// Rejects anything unknown rather than falling back to `Replace`: a typo
    /// (`"substract"`) used to silently wipe the existing selection and replace
    /// it, which reads as a successful subtract until the export is wrong.
    fn parse(mode: &str) -> Result<Self, String> {
        match mode {
            "replace" => Ok(Self::Replace),
            "add" => Ok(Self::Add),
            "subtract" => Ok(Self::Subtract),
            "intersect" => Ok(Self::Intersect),
            other => Err(format!(
                "unknown selection mode '{other}' — expected replace, add, subtract or intersect"
            )),
        }
    }

    /// Combine one pixel: `base` is the current selection, `shape` the new one.
    fn combine(self, base: bool, shape: bool) -> bool {
        match self {
            Self::Replace => shape,
            Self::Add => base || shape,
            Self::Subtract => base && !shape,
            Self::Intersect => base && shape,
        }
    }
}

/// Even-odd scanline fill of the closed polygon `points` (last vertex joins
/// back to the first) into a row-major `w`×`h` mask. The same crossing test as
/// `Document::polygon`'s fill, minus its edge stroke — a selection is a region
/// test, not paint. Vertices may sit off-canvas: crossings clamp to the mask
/// bounds instead of panicking.
fn polygon_mask(points: &[[i32; 2]], w: u32, h: u32, mask: &mut [bool]) {
    let (w, h) = (w as i32, h as i32);
    let ymin = points.iter().map(|p| p[1]).min().unwrap_or(0).max(0);
    let ymax = points.iter().map(|p| p[1]).max().unwrap_or(-1).min(h - 1);
    let n = points.len();
    for y in ymin..=ymax {
        let yf = y as f32 + 0.5;
        // X where each edge crosses this scanline's centre.
        let mut xs: Vec<f32> = Vec::new();
        for i in 0..n {
            let [x1, y1] = points[i];
            let [x2, y2] = points[(i + 1) % n];
            let (y1f, y2f) = (y1 as f32, y2 as f32);
            if (y1f <= yf && y2f > yf) || (y2f <= yf && y1f > yf) {
                let t = (yf - y1f) / (y2f - y1f);
                xs.push(x1 as f32 + t * (x2 as f32 - x1 as f32));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut i = 0;
        while i + 1 < xs.len() {
            // float→int casts saturate, so an off-canvas crossing clamps to the
            // canvas edge; a span that ends before it starts fills nothing.
            let xa = (xs[i].ceil() as i32).max(0);
            let xb = (xs[i + 1].floor() as i32).min(w - 1);
            for x in xa..=xb {
                mask[(y * w + x) as usize] = true;
            }
            i += 2;
        }
    }
}

impl Studio {
    /// The active selection's mask for document `id`, validated against the
    /// canvas: Ok(None) = no selection targets this doc; Err = the selection
    /// targets this doc but no longer matches its dimensions. Erroring beats
    /// the old behaviour (silently applying the op UNMASKED), which let a
    /// paint the agent believed was confined repaint the whole cel.
    pub(crate) fn selection_mask_for(
        &self,
        id: &str,
        w: u32,
        h: u32,
    ) -> Result<Option<&[bool]>, String> {
        match &self.selection {
            Some(s) if s.doc_id == id => {
                if s.w != w || s.h != h {
                    Err(format!(
                        "active selection is stale: it covers a {}x{} canvas but '{}' is {}x{} — \
                         doc_select shape=none to clear it, then reselect",
                        s.w, s.h, id, w, h
                    ))
                } else {
                    Ok(Some(&s.mask))
                }
            }
            _ => Ok(None),
        }
    }

    /// Set/modify the active selection mask. `shape`: rect / ellipse / color /
    /// polygon (alias `lasso`, `points` vertices, auto-closed) / all / none.
    /// `mode` combines with the current selection: replace / add / subtract /
    /// intersect. Painting ops then confine to the `true` pixels until the
    /// selection is replaced or cleared (shape "none").
    pub fn doc_select(
        &mut self,
        id: &str,
        shape: &str,
        mode: &str,
        rect: Option<(i32, i32, i32, i32)>,
        ell: Option<(i32, i32, i32, i32)>,
        color_at: Option<ColorSelect>,
        points: Option<&[[i32; 2]]>,
    ) -> Result<Value, String> {
        let mode = SelectMode::parse(mode)?;
        let (_dir, doc) = self.open(id)?;
        let (w, h) = (doc.meta().w, doc.meta().h);
        let n = (w * h) as usize;
        if shape == "none" {
            self.selection = None;
            return Ok(json!({"doc_id": id, "selected_pixels": 0, "cleared": true}));
        }
        let idx = |x: i32, y: i32| (y as u32 * w + x as u32) as usize;
        let mut shape_mask = vec![false; n];
        match shape {
            "all" => shape_mask.iter_mut().for_each(|b| *b = true),
            "rect" => {
                let (x0, y0, x1, y1) = rect.ok_or("rect selection needs x0,y0,x1,y1")?;
                let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w as i32 - 1));
                let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h as i32 - 1));
                for y in ay..=by {
                    for x in ax..=bx {
                        shape_mask[idx(x, y)] = true;
                    }
                }
            }
            "ellipse" => {
                let (cx, cy, rx, ry) = ell.ok_or("ellipse selection needs cx,cy,rx,ry")?;
                let (a, b) = (rx.max(1) as f32 + 0.5, ry.max(1) as f32 + 0.5);
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        // i64: `x - cx` overflows i32 for an extreme centre.
                        let (dx, dy) =
                            ((x as i64 - cx as i64) as f32, (y as i64 - cy as i64) as f32);
                        if (dx / a).powi(2) + (dy / b).powi(2) <= 1.0 {
                            shape_mask[idx(x, y)] = true;
                        }
                    }
                }
            }
            "color" => {
                let c = color_at
                    .ok_or("color selection needs layer/frame and a color or sample point")?;
                let target = match (c.color, c.sample) {
                    (Some(col), _) => col,
                    (None, Some((px, py))) => doc.get_pixel(c.layer, c.frame, px, py)?,
                    (None, None) => {
                        return Err("color selection needs `color` or `x,y` to sample".into());
                    }
                };
                let near = |a: [u8; 4], b: [u8; 4]| -> bool {
                    (0..4)
                        .map(|i| (a[i] as i32 - b[i] as i32).abs())
                        .sum::<i32>()
                        <= c.tol
                };
                // One cel read for the whole scan — get_pixel would re-probe
                // the cel map per pixel.
                let img = doc.analysis_image(Some(c.layer), c.frame)?;
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        if near(img.get_pixel(x as u32, y as u32).0, target) {
                            shape_mask[idx(x, y)] = true;
                        }
                    }
                }
            }
            "polygon" | "lasso" => {
                let pts = points.ok_or("polygon selection needs `points`")?;
                // Two vertices trace a line, not a region — refuse loudly
                // rather than quietly selecting nothing.
                if pts.len() < 3 {
                    return Err(format!(
                        "polygon selection needs at least 3 points, got {}",
                        pts.len()
                    ));
                }
                polygon_mask(pts, w, h, &mut shape_mask);
            }
            other => return Err(format!("unknown selection shape '{}'", other)),
        }
        let base = match &self.selection {
            Some(s) if s.doc_id == id && s.w == w && s.h == h => s.mask.clone(),
            _ => vec![false; n],
        };
        let mask: Vec<bool> = (0..n)
            .map(|i| mode.combine(base[i], shape_mask[i]))
            .collect();
        let count = mask.iter().filter(|b| **b).count();
        self.selection = Some(Selection {
            doc_id: id.to_string(),
            w,
            h,
            mask,
        });
        Ok(json!({"doc_id": id, "selected_pixels": count, "w": w, "h": h}))
    }

    // -- region / clipboard --------------------------------------------------

    /// Read one pixel — RGBA + a `#rrggbbaa` hex string. Test-only helper; the
    /// `doc_get_pixel` tool was removed (use `doc_dump_region` in production).
    #[cfg(test)]
    pub fn doc_get_pixel(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let p = match layer {
            Some(l) => doc.get_pixel(l, frame, x, y)?,
            None => {
                let img = doc.flatten(frame);
                if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() {
                    [0, 0, 0, 0]
                } else {
                    img.get_pixel(x as u32, y as u32).0
                }
            }
        };
        let hex = crate::hex_rgba(&p);
        Ok(json!({"x": x, "y": y, "rgba": p, "hex": hex, "layer": layer}))
    }

    /// Copy a region into the shared clipboard (does not modify the document).
    pub fn doc_copy_region(
        &mut self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let (w, h, buf) = doc.copy_region(layer, frame, x0, y0, x1, y1)?;
        self.clipboard = Some((w, h, buf));
        Ok(json!({"copied": {"w": w, "h": h}}))
    }

    /// Cut = copy to clipboard, then clear the source region.
    pub fn doc_cut_region(
        &mut self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let (w, h, buf) = doc.copy_region(layer, frame, x0, y0, x1, y1)?;
        doc.clear_region(layer, frame, x0, y0, x1, y1)?;
        doc.save(&dir)?;
        self.clipboard = Some((w, h, buf));
        Ok(json!({"cut": {"w": w, "h": h}, "doc_id": id}))
    }

    /// Paste the clipboard onto a cel at (x,y). `blend` true = source-over,
    /// false = overwrite (also stamps transparency). Works across documents.
    pub fn doc_paste(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        blend: bool,
    ) -> Result<Value, String> {
        let (w, h, buf) = self
            .clipboard
            .as_ref()
            .ok_or("clipboard is empty — copy or cut a region first")?;
        self.doc_paste_pixels(id, layer, frame, x, y, *w, *h, buf, blend)
    }

    /// Paste explicit pixels without touching the shared clipboard. This is
    /// the replay path: a journaled paste step embeds the pixels it used, so a
    /// document's journal stays self-contained even when the copy came from
    /// another document (which the per-document journal cannot express).
    #[allow(clippy::too_many_arguments)]
    pub fn doc_paste_pixels(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        buf: &[u8],
        blend: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.paste_region(layer, frame, x, y, w, h, buf, blend)?;
        doc.save(&dir)?;
        Ok(json!({"pasted": {"w": w, "h": h, "at": [x, y]}, "doc_id": id}))
    }

    /// The current clipboard, if any — the journal embeds it into each paste
    /// step it records (see `doc_paste_pixels`).
    pub fn clipboard_pixels(&self) -> Option<(u32, u32, &[u8])> {
        self.clipboard.as_ref().map(|(w, h, b)| (*w, *h, &b[..]))
    }

    pub fn doc_move_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        dx: i32,
        dy: i32,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.move_region(layer, frame, x0, y0, x1, y1, dx, dy))
    }

    pub fn doc_clear_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.clear_region(layer, frame, x0, y0, x1, y1))
    }

    /// One-tool dispatch over region + clipboard ops — `op`: `copy` | `cut` |
    /// `clear` (the `[x0,y0,x1,y1]` rect) | `move` (rect by `dx,dy`) | `paste`
    /// (the clipboard at `x,y`, `blend` source-over by default). Routes to the
    /// kept region methods.
    pub fn doc_region(
        &mut self,
        id: &str,
        op: &str,
        layer: usize,
        frame: usize,
        x0: Option<i32>,
        y0: Option<i32>,
        x1: Option<i32>,
        y1: Option<i32>,
        dx: Option<i32>,
        dy: Option<i32>,
        x: Option<i32>,
        y: Option<i32>,
        blend: Option<bool>,
    ) -> Result<Value, String> {
        // The rect ops act on whatever region they are given — a defaulted 0
        // would silently target the top-left corner, so the corners are required.
        let rect = |name: &str| -> Result<(i32, i32, i32, i32), String> {
            match (x0, y0, x1, y1) {
                (Some(x0), Some(y0), Some(x1), Some(y1)) => Ok((x0, y0, x1, y1)),
                _ => Err(format!("doc_region op '{name}' needs x0/y0/x1/y1")),
            }
        };
        match op {
            "copy" => {
                let (x0, y0, x1, y1) = rect(op)?;
                self.doc_copy_region(id, layer, frame, x0, y0, x1, y1)
            }
            "cut" => {
                let (x0, y0, x1, y1) = rect(op)?;
                self.doc_cut_region(id, layer, frame, x0, y0, x1, y1)
            }
            "clear" => {
                let (x0, y0, x1, y1) = rect(op)?;
                self.doc_clear_region(id, layer, frame, x0, y0, x1, y1)
            }
            "move" => {
                let (x0, y0, x1, y1) = rect(op)?;
                let (dx, dy) = dx.zip(dy).ok_or("doc_region op 'move' needs dx/dy")?;
                self.doc_move_region(id, layer, frame, x0, y0, x1, y1, dx, dy)
            }
            "paste" => self.doc_paste(
                id,
                layer,
                frame,
                x.unwrap_or(0),
                y.unwrap_or(0),
                blend.unwrap_or(true),
            ),
            other => Err(format!(
                "doc_region: unknown op '{other}' — use copy|cut|paste|move|clear"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-region-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    fn mask_of(s: &Studio) -> Vec<bool> {
        s.selection.as_ref().unwrap().mask.clone()
    }

    #[test]
    fn polygon_triangle_masks_exactly_the_filled_pixels() {
        let mut s = studio("tri");
        s.doc_create("d", 8, 8).unwrap();
        // Right triangle; the even-odd fill covers rows y=1..=5, one pixel
        // narrower per row (hand-computed from the scanline crossings).
        let out = s
            .doc_select(
                "d",
                "polygon",
                "replace",
                None,
                None,
                None,
                Some(&[[1, 1], [6, 1], [1, 6]]),
            )
            .unwrap();
        assert_eq!(out["selected_pixels"], json!(15));
        let mut expected = vec![false; 64];
        for (y, xmax) in [(1usize, 5usize), (2, 4), (3, 3), (4, 2), (5, 1)] {
            for x in 1..=xmax {
                expected[y * 8 + x] = true;
            }
        }
        assert_eq!(mask_of(&s), expected);
    }

    #[test]
    fn polygon_composes_with_rect_through_add_and_subtract() {
        let mut s = studio("modes");
        s.doc_create("d", 8, 8).unwrap();
        let tri = [[1, 1], [6, 1], [1, 6]];
        // Top half of the canvas.
        let out = s
            .doc_select("d", "rect", "replace", Some((0, 0, 7, 3)), None, None, None)
            .unwrap();
        assert_eq!(out["selected_pixels"], json!(32));
        // Adding the triangle only brings in its rows below y=3.
        let out = s
            .doc_select("d", "polygon", "add", None, None, None, Some(&tri))
            .unwrap();
        assert_eq!(out["selected_pixels"], json!(35));
        // Subtracting it again removes the 12 pixels they shared.
        let out = s
            .doc_select("d", "polygon", "subtract", None, None, None, Some(&tri))
            .unwrap();
        assert_eq!(out["selected_pixels"], json!(20));
        let mask = mask_of(&s);
        let at = |x: usize, y: usize| mask[y * 8 + x];
        assert!(
            at(0, 0) && at(7, 3),
            "rect pixels outside the triangle stay"
        );
        assert!(!at(1, 1) && !at(5, 1), "shared pixels were subtracted");
        assert!(!at(1, 5), "below the rect was never selected");
    }

    #[test]
    fn polygon_needs_three_points_and_its_vertices() {
        let mut s = studio("few");
        s.doc_create("d", 8, 8).unwrap();
        let e = s
            .doc_select(
                "d",
                "lasso",
                "replace",
                None,
                None,
                None,
                Some(&[[0, 0], [3, 3]]),
            )
            .unwrap_err();
        assert!(e.contains("at least 3"), "got: {e}");
        let e = s
            .doc_select("d", "polygon", "replace", None, None, None, None)
            .unwrap_err();
        assert!(e.contains("points"), "got: {e}");
        // A rejected lasso must not disturb the selection already in place.
        assert!(s.selection.is_none());
    }

    #[test]
    fn polygon_fully_off_canvas_selects_nothing() {
        let mut s = studio("off");
        s.doc_create("d", 8, 8).unwrap();
        // Above/left: the scanline y-range is empty.
        let out = s
            .doc_select(
                "d",
                "polygon",
                "replace",
                None,
                None,
                None,
                Some(&[[-20, -20], [-10, -20], [-10, -5]]),
            )
            .unwrap();
        assert_eq!(out["selected_pixels"], json!(0));
        // Right of the canvas: crossings exist on paper but clamp outside.
        let out = s
            .doc_select(
                "d",
                "polygon",
                "replace",
                None,
                None,
                None,
                Some(&[[100, 0], [200, 0], [100, 7]]),
            )
            .unwrap();
        assert_eq!(out["selected_pixels"], json!(0));
    }
}
