use super::{Asset, Cel, color};
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
fn grid_size(grid: &str) -> Result<[u32; 2], String> {
    let mut lines = grid.lines();
    let first = lines.next().ok_or("grid is empty")?;
    let w = first.chars().count();
    let mut h = 1;
    for row in lines {
        if row.chars().count() != w {
            return Err("grid rows must have equal width".into());
        }
        h += 1;
    }
    size([w as u32, h])?;
    Ok([w as u32, h])
}
pub(super) fn fits(cel: &Cel, canvas: [u32; 2], [w, h]: [u32; 2]) -> Result<(), String> {
    let size = cel.size(canvas);
    if cel.origin[0] as u64 + w as u64 > size[0] as u64
        || cel.origin[1] as u64 + h as u64 > size[1] as u64
    {
        return Err("pixel resource and origin exceed the cel's logical size".into());
    }
    Ok(())
}
impl Asset {
    /// Check structure, dimensions and total rendering work before allocation.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != 1 || self.renderer != 1 {
            return Err("unsupported recipe format or renderer; expected 1/1".into());
        }
        self.metadata()?.validate()?;
        let mut assigned = HashSet::new();
        let (mut pixels, mut work) = (0_u64, 0_u64);
        for (layer, description) in self.layers.iter().enumerate() {
            for cel in &description.cels {
                let dimensions = cel.size(self.canvas);
                let area = size(dimensions)?;
                if cel.frames.is_empty() {
                    return Err("cel has no frames".into());
                }
                if cel.grid.is_some() && cel.image.is_some() {
                    return Err("a cel cannot contain both a grid and an image".into());
                }
                if cel.grid.is_none() && !cel.legend.is_empty() {
                    return Err("legend requires a grid".into());
                }
                for (axis, dimension) in dimensions.iter().enumerate() {
                    let at = i64::from(cel.at[axis]);
                    if at + i64::from(*dimension) - 1 > i64::from(i32::MAX)
                        || i64::from(self.canvas[axis]) - 1 - at > i64::from(i32::MAX)
                    {
                        return Err("cel offset exceeds the signed coordinate range".into());
                    }
                }
                if let Some(grid) = &cel.grid {
                    fits(cel, self.canvas, grid_size(grid)?)?;
                    for (symbol, value) in &cel.legend {
                        if symbol.chars().count() != 1
                            || symbol == "."
                            || symbol.chars().any(char::is_whitespace)
                        {
                            return Err("invalid grid symbol".into());
                        }
                        color(value)?;
                    }
                    for c in grid.lines().flat_map(str::chars) {
                        if c != '.' && !cel.legend.contains_key(&c.to_string()) {
                            return Err(format!("grid symbol '{c}' has no colour"));
                        }
                    }
                }
                if let Some(palette) = &cel.palette {
                    if palette.len() > MAX_PALETTE_COLORS {
                        return Err("too many drawing palette colours".into());
                    }
                    for value in palette {
                        color(value)?;
                    }
                }
                if !cel.draw.is_empty() && dimensions.iter().any(|d| *d > MAX_DOCUMENT_DIMENSION) {
                    return Err("drawing cel dimensions exceed the canvas limit".into());
                }
                if cel.draw.len() > 100_000 {
                    return Err("too many drawing operations".into());
                }
                for op in &cel.draw {
                    validate_op(super::model::drawing_op(op)?.as_ref())?;
                }
                for frame in &cel.frames {
                    if *frame >= self.frames.len() || !assigned.insert((layer, *frame)) {
                        return Err("duplicate or out-of-range layer/frame assignment".into());
                    }
                    pixels = pixels.saturating_add(area);
                    work = work.saturating_add(area.saturating_mul(1 + cel.draw.len() as u64));
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
