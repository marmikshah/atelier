use super::{Asset, PixelData, Pixels, Step, color};
use crate::document::{
    MAX_DOCUMENT_CEL_PIXELS, MAX_DOCUMENT_CELS, MAX_DOCUMENT_DIMENSION, MAX_PALETTE_COLORS,
    validate_op,
};
use std::collections::HashSet;
pub const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORK: u64 = 512 * 1024 * 1024;
fn size([w, h]: [u32; 2]) -> Result<u64, String> {
    crate::raster::checked_rgba_dimensions("cel", w as u64, h as u64)?;
    Ok(w as u64 * h as u64)
}
fn fits(p: &Pixels, size: [u32; 2], x: u64, y: u64, w: u64, h: u64) -> Result<(), String> {
    if w == 0
        || h == 0
        || u64::from(p.origin[0]) + x + w > u64::from(size[0])
        || u64::from(p.origin[1]) + y + h > u64::from(size[1])
    {
        return Err("pixel data exceeds the cel's logical size".into());
    }
    Ok(())
}
impl Pixels {
    pub fn validate(&self, dimensions: [u32; 2]) -> Result<u64, String> {
        for (symbol, value) in &self.legend {
            if symbol.chars().count() != 1
                || symbol == "."
                || symbol.chars().any(char::is_whitespace)
            {
                return Err("invalid pixel symbol".into());
            }
            color(value)?;
        }
        let mut cursor = 0u64;
        let mut work = 0u64;
        for data in &self.data {
            let (w, h) = match data {
                PixelData::Grid(text) => {
                    let rows: Vec<_> = text.lines().collect();
                    let w = rows.iter().map(|s| s.chars().count()).max().unwrap_or(0) as u64;
                    let h = rows.len() as u64;
                    fits(self, dimensions, 0, cursor, w, h)?;
                    cursor += h;
                    (w, h)
                }
                PixelData::Row { runs, repeat } => {
                    let w = runs.iter().try_fold(0u64, |sum, (s, n)| {
                        if s.chars().count() != 1 || *n == 0 {
                            return Err(
                                "row requires single symbols and positive counts".to_string()
                            );
                        }
                        Ok(sum + u64::from(*n))
                    })?;
                    fits(self, dimensions, 0, cursor, w, u64::from(*repeat))?;
                    cursor += u64::from(*repeat);
                    (w, u64::from(*repeat))
                }
                PixelData::Span { x, y, text, rows } => {
                    if text.contains('.') || text.chars().any(char::is_whitespace) {
                        return Err("occupied spans cannot contain blank symbols".into());
                    }
                    let w = text.chars().count() as u64;
                    fits(
                        self,
                        dimensions,
                        u64::from(*x),
                        u64::from(*y),
                        w,
                        u64::from(*rows),
                    )?;
                    (w, u64::from(*rows))
                }
            };
            work = work.saturating_add(w.saturating_mul(h));
            if work > MAX_WORK {
                return Err("pixel writes exceed the work budget".into());
            }
            let symbols: Vec<&str> = match data {
                PixelData::Row { runs, .. } => runs.iter().map(|(s, _)| s.as_str()).collect(),
                _ => vec![],
            };
            for symbol in symbols {
                if symbol != "." && symbol != " " && !self.legend.contains_key(symbol) {
                    return Err(format!("pixel symbol '{symbol}' has no colour"));
                }
            }
            if let PixelData::Grid(text) | PixelData::Span { text, .. } = data {
                for c in text
                    .chars()
                    .filter(|c| !matches!(c, '.' | ' ' | '\n' | '\r'))
                {
                    if !self.legend.contains_key(&c.to_string()) {
                        return Err(format!("pixel symbol '{c}' has no colour"));
                    }
                }
            }
        }
        Ok(work)
    }
}
impl Asset {
    /// Check every count and coordinate before decoding any pixel payload.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != 1 || self.renderer != 1 {
            return Err("unsupported recipe format or renderer; expected 1/1".into());
        }
        self.metadata()?.validate()?;
        let mut assigned = HashSet::new();
        let (mut pixels, mut work) = (0u64, 0u64);
        for (layer, description) in self.layers.iter().enumerate() {
            for cel in &description.cels {
                let dimensions = cel.size(self.canvas);
                let area = size(dimensions)?;
                if cel.frames.is_empty() {
                    return Err("cel has no frames".into());
                }
                for (axis, dimension) in dimensions.iter().enumerate() {
                    let at = i64::from(cel.at[axis]);
                    if at + i64::from(*dimension) - 1 > i64::from(i32::MAX)
                        || i64::from(self.canvas[axis]) - 1 - at > i64::from(i32::MAX)
                    {
                        return Err("cel offset exceeds the signed coordinate range".into());
                    }
                }
                if cel.steps.len() > 100_000 {
                    return Err("too many construction steps".into());
                }
                let mut cel_work = area;
                for step in &cel.steps {
                    match step {
                        Step::Pixels(p) => {
                            cel_work = cel_work.saturating_add(p.validate(dimensions)?)
                        }
                        Step::Draw { op, palette } => {
                            if dimensions.iter().any(|d| *d > MAX_DOCUMENT_DIMENSION) {
                                return Err("drawing cel dimensions exceed the canvas limit".into());
                            }
                            if palette.len() > MAX_PALETTE_COLORS {
                                return Err("too many drawing palette colours".into());
                            }
                            for c in palette {
                                color(c)?;
                            }
                            validate_op(op)?;
                            cel_work = cel_work.saturating_add(area);
                        }
                    }
                }
                for frame in &cel.frames {
                    if *frame >= self.frames.len() || !assigned.insert((layer, *frame)) {
                        return Err("duplicate or out-of-range layer/frame assignment".into());
                    }
                    pixels = pixels.saturating_add(area);
                    work = work.saturating_add(cel_work);
                    if pixels > MAX_DOCUMENT_CEL_PIXELS
                        || assigned.len() > MAX_DOCUMENT_CELS
                        || work > MAX_WORK
                    {
                        return Err("recipe exceeds the cel, pixel or work budget".into());
                    }
                }
            }
        }
        Ok(())
    }
}
