//! Animation: frame metadata, timeline lifecycle and eased keyframe motion.

use image::RgbaImage;
use serde_json::{json, Value};

use super::{Document, FrameMeta, DEFAULT_FRAME_MS};

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
                    // Re-keyed cels need writing under their new frame names.
                    self.mark_all_dirty();
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
}
