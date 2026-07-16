//! Region clipboard, transforms and selection helpers.

use crate::raster;

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
}
