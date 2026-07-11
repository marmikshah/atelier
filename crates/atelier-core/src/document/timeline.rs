//! Animation: frame metadata, timeline lifecycle and eased keyframe motion.

use image::{Rgba, RgbaImage};
use serde_json::{json, Value};

use crate::raster;

use super::{BoxMeta, Document, FrameMeta, BOX_KINDS, DEFAULT_FRAME_MS};

impl Document {
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

    /// Replace a frame's collision boxes (empty vec clears them). Each box kind
    /// must be one of `BOX_KINDS`; an unknown kind fails fast with the list.
    pub fn set_frame_boxes(&mut self, frame: usize, boxes: Vec<BoxMeta>) -> Result<(), String> {
        if let Some(b) = boxes.iter().find(|b| !BOX_KINDS.contains(&b.kind.as_str())) {
            return Err(format!(
                "box kind must be one of {:?}, got '{}'",
                BOX_KINDS, b.kind
            ));
        }
        let f = self
            .meta
            .frames
            .get_mut(frame)
            .ok_or_else(|| format!("no frame {}", frame))?;
        f.boxes = boxes;
        Ok(())
    }

    /// Insert `steps` cross-faded (dissolve) in-between frames after frame
    /// `from`, interpolating every layer toward frame `to`. Reindexes later cels.
    pub fn tween(
        &mut self,
        from: usize,
        to: usize,
        steps: usize,
        duration_ms: u32,
    ) -> Result<usize, String> {
        let n = self.meta.frames.len();
        if from >= n || to >= n {
            return Err(format!(
                "tween frames {}->{} out of range (frames={})",
                from, to, n
            ));
        }
        if to <= from {
            return Err("tween requires to > from".into());
        }
        let steps = steps.max(1);
        let insert_at = from + 1;
        // Capture full-canvas source/target images per layer before reindexing.
        let nl = self.meta.layers.len();
        let pairs: Vec<(RgbaImage, RgbaImage)> = (0..nl)
            .map(|l| (self.cel_full(l, from), self.cel_full(l, to)))
            .collect();
        // Shift cels at/after the insertion point up by `steps`.
        self.shift_cel_frames(insert_at, steps as isize);
        // Insert frame metadata, inheriting the source frame's pivot/boxes so
        // in-betweens don't arrive with no anchor and no collision data.
        let src_meta = self.meta.frames[from].clone();
        for i in 0..steps {
            self.meta.frames.insert(
                insert_at + i,
                FrameMeta {
                    duration_ms,
                    pivot: src_meta.pivot,
                    boxes: src_meta.boxes.clone(),
                },
            );
        }
        // Keep tag ranges pointing at the frames they tagged (a tag spanning
        // the insertion stretches over the new in-betweens).
        for t in &mut self.meta.tags {
            if t.from >= insert_at {
                t.from += steps;
            }
            if t.to >= insert_at {
                t.to += steps;
            }
        }
        // Build cross-fade cels; with a locked palette, snap each blend so the
        // dissolve can't mint off-palette colours.
        let mut lab = raster::PaletteLab::new(&self.meta.palette);
        let (w, h) = (self.meta.w, self.meta.h);
        for s in 1..=steps {
            let t = s as f32 / (steps + 1) as f32;
            let fidx = insert_at + (s - 1);
            for (l, (a, b)) in pairs.iter().enumerate() {
                let mut img = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
                for y in 0..h {
                    for x in 0..w {
                        let pa = a.get_pixel(x, y).0;
                        let pb = b.get_pixel(x, y).0;
                        // Alpha-weighted (premultiplied) RGB blend: transparent
                        // pixels are stored [0,0,0,0], and a plain lerp lets
                        // that black bleed into fringes — which the palette
                        // snap then quantizes into wrong-hue colours.
                        let alpha = (pa[3] as f32 + (pb[3] as f32 - pa[3] as f32) * t)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        let (wa, wb) = (pa[3] as f32 * (1.0 - t), pb[3] as f32 * t);
                        let mixed = if wa + wb > 0.0 {
                            let ch = |i: usize| {
                                ((pa[i] as f32 * wa + pb[i] as f32 * wb) / (wa + wb))
                                    .round()
                                    .clamp(0.0, 255.0) as u8
                            };
                            [ch(0), ch(1), ch(2), alpha]
                        } else {
                            [0, 0, 0, 0]
                        };
                        let px = match (mixed[3] > 0, lab.nearest(mixed)) {
                            (true, Some(pi)) => {
                                let c = lab.color(pi);
                                [c[0], c[1], c[2], mixed[3]]
                            }
                            _ => mixed,
                        };
                        img.put_pixel(x, y, Rgba(px));
                    }
                }
                self.cels.insert((l, fidx), (0, 0, img));
            }
        }
        Ok(steps)
    }

    /// Timeline lifecycle: `delete` | `insert` | `duplicate` | `move`. Cels
    /// reindex and tag ranges remap with the frames; a tag covering only a
    /// deleted frame is dropped. The last remaining frame can't be deleted —
    /// this is the recovery path for a bad tween or duplicated pose.
    pub fn frame_ops(
        &mut self,
        action: &str,
        frame: usize,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
    ) -> Result<Value, String> {
        let n = self.meta.frames.len();
        match action {
            "delete" => {
                if frame >= n {
                    return Err(format!("no frame {} (frames={})", frame, n));
                }
                if n == 1 {
                    return Err("cannot delete the last remaining frame".into());
                }
                self.meta.frames.remove(frame);
                self.cels.retain(|k, _| k.1 != frame);
                self.shift_cel_frames(frame + 1, -1);
                self.meta
                    .tags
                    .retain(|t| !(t.from == frame && t.to == frame));
                for t in &mut self.meta.tags {
                    if t.from > frame {
                        t.from -= 1;
                    }
                    if t.to >= frame {
                        t.to -= 1;
                    }
                }
            }
            "insert" => {
                if frame > n {
                    return Err(format!(
                        "insert index {} out of range (frames={})",
                        frame, n
                    ));
                }
                self.meta.frames.insert(
                    frame,
                    FrameMeta {
                        duration_ms: duration_ms.unwrap_or(DEFAULT_FRAME_MS),
                        pivot: None,
                        boxes: Vec::new(),
                    },
                );
                self.shift_cel_frames(frame, 1);
                for t in &mut self.meta.tags {
                    if t.from >= frame {
                        t.from += 1;
                    }
                    if t.to >= frame {
                        t.to += 1;
                    }
                }
            }
            "duplicate" => {
                if frame >= n {
                    return Err(format!("no frame {} (frames={})", frame, n));
                }
                let meta = self.meta.frames[frame].clone();
                self.meta.frames.insert(frame + 1, meta);
                self.shift_cel_frames(frame + 1, 1);
                let to_copy: Vec<(usize, (i32, i32, RgbaImage))> = self
                    .cels
                    .iter()
                    .filter(|((_, f), _)| *f == frame)
                    .map(|((l, _), v)| (*l, (v.0, v.1, v.2.clone())))
                    .collect();
                for (l, v) in to_copy {
                    self.cels.insert((l, frame + 1), v);
                }
                // A tag ending on the duplicated frame grows to cover the copy.
                for t in &mut self.meta.tags {
                    if t.from > frame {
                        t.from += 1;
                    }
                    if t.to >= frame {
                        t.to += 1;
                    }
                }
            }
            "move" => {
                let to = to_index.ok_or("move needs `to_index`")?;
                if frame >= n || to >= n {
                    return Err(format!(
                        "move {}->{} out of range (frames={})",
                        frame, to, n
                    ));
                }
                if frame != to {
                    let mut order: Vec<usize> = (0..n).filter(|&i| i != frame).collect();
                    order.insert(to, frame);
                    let old_frames = std::mem::take(&mut self.meta.frames);
                    self.meta.frames = order.iter().map(|&o| old_frames[o].clone()).collect();
                    let mut map = vec![0usize; n];
                    for (newi, &old) in order.iter().enumerate() {
                        map[old] = newi;
                    }
                    let all: Vec<_> = self.cels.drain().collect();
                    self.cels = all
                        .into_iter()
                        .map(|((l, f), v)| ((l, map[f]), v))
                        .collect();
                    // Tag remap = remove-then-insert, NOT endpoint min/max
                    // through the permutation (that balloons a tag over
                    // untagged frames when a member moves far away, and drops
                    // a member on an in-tag reorder). A single-frame tag on
                    // the moved frame simply follows it.
                    for t in &mut self.meta.tags {
                        if t.from == frame && t.to == frame {
                            t.from = to;
                            t.to = to;
                            continue;
                        }
                        let was_inside = t.from <= frame && frame <= t.to;
                        // Phase 1: the moved frame leaves its old index.
                        let mut f0 = t.from - usize::from(t.from > frame);
                        let mut t0 = t.to - usize::from(t.to >= frame);
                        // Phase 2: it re-enters at final index `to`.
                        if f0 >= to {
                            f0 += 1;
                        }
                        if t0 >= to {
                            t0 += 1;
                        }
                        // A member landing back inside-or-adjacent rejoins its
                        // tag; one that moved far away leaves it (a tag must
                        // stay a contiguous range of TAGGED frames).
                        if was_inside && to <= t0 + 1 && to + 1 >= f0 {
                            f0 = f0.min(to);
                            t0 = t0.max(to);
                        }
                        t.from = f0;
                        t.to = t0;
                    }
                }
            }
            other => {
                return Err(format!(
                    "unknown frame action '{}' — use delete|insert|duplicate|move",
                    other
                ))
            }
        }
        Ok(json!({"ok": true, "action": action, "frames": self.meta.frames.len()}))
    }

    /// Resolve the ordered frame indices to *play*. With `tag`, honour that
    /// tag's `[from,to]` range and direction; without one, play the whole
    /// timeline forward. `reverse` plays high→low. `pingpong` plays forward
    /// then back over the inner frames only (endpoints not duplicated) so a
    /// looping playback doesn't stutter on the turn-around frames.
    pub fn play_sequence(&self, tag: Option<&str>) -> Result<Vec<usize>, String> {
        if self.meta.frames.is_empty() {
            return Ok(vec![]);
        }
        let (from, to, dir) = match tag {
            Some(name) => {
                let t = self
                    .meta
                    .tags
                    .iter()
                    .find(|t| t.name == name)
                    .ok_or_else(|| format!("no tag '{}'", name))?;
                (t.from, t.to, t.direction.as_str())
            }
            None => (0, self.meta.frames.len() - 1, "forward"),
        };
        // Clamp defensively in case a tag references frames since removed.
        let last = self.meta.frames.len() - 1;
        let (from, to) = (from.min(last), to.min(last));
        let fwd: Vec<usize> = (from..=to).collect();
        let seq = match dir {
            "reverse" => fwd.into_iter().rev().collect(),
            "pingpong" => {
                let mut s = fwd;
                if to > from + 1 {
                    s.extend((from + 1..to).rev()); // inner frames, no dup of endpoints
                }
                s
            }
            _ => fwd, // "forward" and any unknown direction
        };
        Ok(seq)
    }

    /// Eased multi-frame region motion. `from_frame`'s region content is the
    /// source; for each frame f in (from, to] it is stamped (source-over) at the
    /// eased offset `(round(dx*t), round(dy*t))`, where t advances from 0→1 over
    /// the span shaped by `easing` (forwarded verbatim to `raster::ease` —
    /// linear/ease-in/ease-out/ease-in-out/bounce/overshoot/elastic; the
    /// non-monotone curves can push past the full offset before settling). With
    /// `clear_source` the ORIGINAL region rect
    /// is cleared in each destination frame first (so a moved limb leaves no
    /// stale copy behind). `from_frame` itself is never touched. Reuses the
    /// region copy/clear/paste clipboard internals. Returns the per-frame applied
    /// offsets `[[dx,dy], ...]`.
    pub fn keyframe_move(
        &mut self,
        layer: usize,
        region: (i32, i32, i32, i32),
        from_frame: usize,
        to_frame: usize,
        dx: i32,
        dy: i32,
        easing: &str,
        clear_source: bool,
    ) -> Result<Vec<[i32; 2]>, String> {
        if to_frame <= from_frame {
            return Err("keyframe_move needs to_frame > from_frame".into());
        }
        raster::validate_ease(easing)?;
        let n = self.meta.frames.len();
        if to_frame >= n {
            return Err(format!(
                "frame {} does not exist (frames={}) — add it with doc_add_frame first",
                to_frame, n
            ));
        }
        // Snapshot the source region content once, from the start keyframe. The
        // anchored top-left is where it gets re-stamped (clamped like copy_region).
        let (x0, y0, x1, y1) = region;
        let (rw, rh, buf) = self.copy_region(layer, from_frame, x0, y0, x1, y1)?;
        let (ax, ay) = (x0.min(x1).max(0), y0.min(y1).max(0));
        let span = (to_frame - from_frame) as f32;
        let mut offsets: Vec<[i32; 2]> = Vec::new();
        for f in (from_frame + 1)..=to_frame {
            let t = raster::ease((f - from_frame) as f32 / span, easing);
            let (ox, oy) = (
                (dx as f32 * t).round() as i32,
                (dy as f32 * t).round() as i32,
            );
            if clear_source {
                self.clear_region(layer, f, x0, y0, x1, y1)?;
            }
            self.paste_region(layer, f, ax + ox, ay + oy, rw, rh, &buf, true)?;
            offsets.push([ox, oy]);
        }
        Ok(offsets)
    }

    /// Eased rotation + translation of a region about an arbitrary PIVOT
    /// across a frame range on one layer — the joint primitive: "swing the arm
    /// 30° about the shoulder over frames 1..4" in one call instead of four
    /// blind repaints. Reads the region's content from `from_frame`; each
    /// later frame gets the part rotated by the eased angle and offset by the
    /// eased (dx,dy) about `pivot` (document coords), with the original region
    /// cleared first so no stale copy remains. `snap` re-snaps the resampled
    /// pixels to the locked palette.
    pub fn keyframe_transform(
        &mut self,
        layer: usize,
        region: (i32, i32, i32, i32),
        pivot: (f32, f32),
        from_frame: usize,
        to_frame: usize,
        rot_deg: f32,
        dx: i32,
        dy: i32,
        easing: &str,
        snap: bool,
    ) -> Result<Vec<Value>, String> {
        if to_frame <= from_frame {
            return Err("keyframe_transform needs to_frame > from_frame".into());
        }
        raster::validate_ease(easing)?;
        let n = self.meta.frames.len();
        if to_frame >= n {
            return Err(format!(
                "frame {} does not exist (frames={}) — add it with doc_add_frame first",
                to_frame, n
            ));
        }
        let (x0, y0, x1, y1) = (
            region.0.min(region.2),
            region.1.min(region.3),
            region.0.max(region.2),
            region.1.max(region.3),
        );
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (rx0, ry0) = (x0.max(0), y0.max(0));
        let (rx1, ry1) = (x1.min(w - 1), y1.min(h - 1));
        if rx0 > rx1 || ry0 > ry1 {
            return Err("region is empty after clamping to the canvas".into());
        }
        let (rw, rh) = ((rx1 - rx0 + 1) as u32, (ry1 - ry0 + 1) as u32);
        let full = self.cel_full(layer, from_frame);
        let mut part = RgbaImage::from_pixel(rw, rh, Rgba([0, 0, 0, 0]));
        for y in 0..rh {
            for x in 0..rw {
                part.put_pixel(x, y, *full.get_pixel(rx0 as u32 + x, ry0 as u32 + y));
            }
        }
        let mut lab = raster::PaletteLab::new(&self.meta.palette);
        let span = (to_frame - from_frame) as f32;
        let mut placed = Vec::new();
        for f in (from_frame + 1)..=to_frame {
            let t = raster::ease((f - from_frame) as f32 / span, easing);
            let theta = rot_deg * t;
            let (ox, oy) = (
                (dx as f32 * t).round() as i32,
                (dy as f32 * t).round() as i32,
            );
            let rotated = raster::affine_nn(&part, theta, 1.0, 1.0, 0.0, 0.0, 2);
            // affine_nn rotates about the part's centre and returns a
            // bbox-sized image; place it so the JOINT pivot stays fixed:
            // c' = pivot + R(c - pivot), then the eased translation.
            let (cx, cy) = (rx0 as f32 + rw as f32 / 2.0, ry0 as f32 + rh as f32 / 2.0);
            let r = theta.to_radians();
            let (cos, sin) = (r.cos(), r.sin());
            let (vx, vy) = (cx - pivot.0, cy - pivot.1);
            let ncx = pivot.0 + cos * vx - sin * vy;
            let ncy = pivot.1 + sin * vx + cos * vy;
            let px = (ncx + ox as f32 - rotated.width() as f32 / 2.0).round() as i32;
            let py = (ncy + oy as f32 - rotated.height() as f32 / 2.0).round() as i32;
            self.clear_region(layer, f, rx0, ry0, rx1, ry1)?;
            let img = self.cel_canvas(layer, f)?;
            for (sx, sy, p) in rotated.enumerate_pixels() {
                if p.0[3] == 0 {
                    continue;
                }
                let (tx, ty) = (px + sx as i32, py + sy as i32);
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    continue;
                }
                let c = match (snap, lab.nearest(p.0)) {
                    (true, Some(pi)) => {
                        let pc = lab.color(pi);
                        Rgba([pc[0], pc[1], pc[2], p.0[3]])
                    }
                    _ => *p,
                };
                img.put_pixel(tx as u32, ty as u32, c);
            }
            placed.push(json!({
                "frame": f,
                "rot_deg": (theta * 10.0).round() / 10.0,
                "offset": [ox, oy],
                "placed_at": [px, py],
            }));
        }
        Ok(placed)
    }
}
