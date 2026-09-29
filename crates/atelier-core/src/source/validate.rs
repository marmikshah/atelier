use std::collections::{BTreeMap, HashSet};

use super::{Asset, Part, Placement, color};
use crate::document::{
    MAX_DOCUMENT_CEL_PIXELS, MAX_DOCUMENT_CELS, MAX_DOCUMENT_DIMENSION, MAX_PALETTE_COLORS,
    validate_op,
};
use crate::raster::Blend;

pub const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PARTS: usize = 16_384;
const MAX_DEPTH: usize = 64;
const MAX_WORK: u64 = 512 * 1024 * 1024;
const MAX_INSTANCES: u64 = 100_000;

pub(super) fn size([w, h]: [u32; 2]) -> Result<u64, String> {
    crate::raster::checked_rgba_dimensions("part", w as u64, h as u64)?;
    Ok(w as u64 * h as u64)
}

fn id(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
        || name.starts_with('#')
    {
        return Err(format!(
            "invalid source name '{name}': use 1..128 ASCII letters, digits, _, -, ., /"
        ));
    }
    Ok(())
}

pub(super) fn grid_size(grid: &str) -> Result<[u32; 2], String> {
    if grid.len() as u64 > MAX_SOURCE_BYTES {
        return Err("grid exceeds the source size limit".into());
    }
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

pub(super) fn fits(part: &Part, [w, h]: [u32; 2]) -> Result<(), String> {
    if part.origin[0] as u64 + w as u64 > part.size[0] as u64
        || part.origin[1] as u64 + h as u64 > part.size[1] as u64
    {
        return Err("pixel resource and origin exceed the part's logical size".into());
    }
    Ok(())
}

impl Asset {
    /// Validate references, cycles, expanded work and decoded pixel budgets
    /// before allocating any rendered part or cel.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != 1 || self.renderer != 1 {
            return Err(format!(
                "unsupported source format/renderer {}/{}; expected 1/1",
                self.format, self.renderer
            ));
        }
        self.metadata()?.validate()?;
        if self.parts.len() > MAX_PARTS || self.inks.len() > MAX_PARTS {
            return Err("too many source parts or inks".into());
        }
        for (name, value) in &self.inks {
            id(name)?;
            color(value)?;
        }
        let mut layers = HashSet::new();
        for layer in &self.layers {
            id(&layer.id)?;
            if !layers.insert(layer.id.as_str()) {
                return Err(format!("duplicate layer id '{}'", layer.id));
            }
        }
        let mut part_pixels = 0;
        for (name, part) in &self.parts {
            id(name)?;
            self.validate_part(part)
                .map_err(|e| format!("part '{name}': {e}"))?;
            part_pixels += size(part.size)?;
            if part_pixels > MAX_DOCUMENT_CEL_PIXELS {
                return Err("source parts exceed the decoded pixel budget".into());
            }
        }
        let mut costs = BTreeMap::new();
        for name in self.parts.keys() {
            self.cost(name, &mut vec![], &mut costs)?;
        }
        let mut cels = HashSet::new();
        let (mut pixels, mut work, mut instances) = (0, 0, 0);
        for cel in &self.cels {
            if !layers.contains(cel.layer.as_str()) || cel.frames.is_empty() {
                return Err(format!(
                    "cel uses unknown layer '{}' or no frames",
                    cel.layer
                ));
            }
            let part = self
                .parts
                .get(&cel.part)
                .ok_or_else(|| format!("unknown part '{}'", cel.part))?;
            // Persisted cel readers use signed pixel coordinates. Reject an
            // anchor whose visible-coordinate subtraction or cel extent would
            // overflow those readers, before publishing an unusable document.
            for axis in 0..2 {
                let at = i64::from(cel.at[axis]);
                if at + i64::from(part.size[axis]) - 1 > i64::from(i32::MAX)
                    || i64::from(self.canvas[axis]) - 1 - at > i64::from(i32::MAX)
                {
                    return Err(format!(
                        "cel '{}' offset exceeds the signed coordinate range",
                        cel.layer
                    ));
                }
            }
            for frame in &cel.frames {
                if *frame >= self.frames.len() || !cels.insert((&cel.layer, *frame)) {
                    return Err(format!(
                        "duplicate or out-of-range cel '{}', frame {frame}",
                        cel.layer
                    ));
                }
                pixels += size(part.size)?;
                let (w, n, _) = costs[&cel.part];
                work += w;
                instances += n;
                if pixels > MAX_DOCUMENT_CEL_PIXELS
                    || cels.len() > MAX_DOCUMENT_CELS
                    || work > MAX_WORK
                    || instances > MAX_INSTANCES
                {
                    return Err("expanded source exceeds the cel, pixel or work budget".into());
                }
            }
        }
        Ok(())
    }

    fn validate_part(&self, p: &Part) -> Result<(), String> {
        size(p.size)?;
        if [
            p.grid.is_some(),
            p.image.is_some(),
            p.items.is_some(),
            p.draw.is_some(),
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            != 1
        {
            return Err("a part needs exactly one of grid, image, items, draw".into());
        }
        if p.grid.is_none() && !p.legend.is_empty() {
            return Err("legend requires a grid".into());
        }
        if p.draw.is_none() && (p.base.is_some() || p.palette.is_some()) {
            return Err("base and palette require draw".into());
        }
        if p.grid.is_none() && p.image.is_none() && p.origin != [0, 0] {
            return Err("origin requires grid or image".into());
        }
        for dep in p.dependencies() {
            if !self.parts.contains_key(dep) {
                return Err(format!("unknown part '{dep}'"));
            }
        }
        if let Some(grid) = &p.grid {
            fits(p, grid_size(grid)?)?;
            for (symbol, ink) in &p.legend {
                if symbol.chars().count() != 1
                    || symbol == "."
                    || symbol.chars().any(char::is_whitespace)
                {
                    return Err(format!("invalid grid symbol '{symbol}'"));
                }
                self.ink(ink, &BTreeMap::new())?;
            }
            for c in grid.lines().flat_map(str::chars) {
                if c != '.' && !p.legend.contains_key(&c.to_string()) {
                    return Err(format!("grid symbol '{c}' has no legend entry"));
                }
            }
        }
        if let Some(palette) = &p.palette {
            if palette.len() > MAX_PALETTE_COLORS {
                return Err("too many procedure palette colours".into());
            }
            for c in palette {
                color(c)?;
            }
        }
        if let Some(ops) = &p.draw {
            if p.size.iter().any(|d| *d > MAX_DOCUMENT_DIMENSION) {
                return Err("drawing part dimensions exceed the document canvas limit".into());
            }
            if ops.len() > 100_000 {
                return Err("too many drawing operations".into());
            }
            for op in ops {
                validate_op(super::model::drawing_op(op)?.as_ref())?;
            }
            if let Some(base) = &p.base
                && self.parts[base].size != p.size
            {
                return Err("draw base must have the same logical size".into());
            }
        }
        for i in p.items.iter().flatten() {
            if !(1..=16).contains(&i.scale) || i.turns > 3 {
                return Err("instance scale must be 1..16 and turns 0..3".into());
            }
            if i.mode == Placement::Replace && (i.opacity != 255 || i.blend != Blend::Normal) {
                return Err("opacity/blend require mode = 'over'".into());
            }
            for (name, ink) in &i.bindings {
                if !self.inks.contains_key(name) {
                    return Err(format!("unknown bound ink '{name}'"));
                }
                self.ink(ink, &BTreeMap::new())?;
            }
        }
        Ok(())
    }

    fn cost(
        &self,
        name: &str,
        path: &mut Vec<String>,
        costs: &mut BTreeMap<String, (u64, u64, usize)>,
    ) -> Result<(u64, u64, usize), String> {
        if path.iter().any(|s| s == name) {
            return Err(format!(
                "cyclic part reference: {} -> {name}",
                path.join(" -> ")
            ));
        }
        if path.len() >= MAX_DEPTH {
            return Err("source dependency depth exceeds 64".into());
        }
        if let Some(cost) = costs.get(name) {
            if path.len() + cost.2 > MAX_DEPTH {
                return Err("source dependency depth exceeds 64".into());
            }
            return Ok(*cost);
        }
        let p = &self.parts[name];
        path.push(name.into());
        let mut work = size(p.size)? * (1 + p.draw.as_ref().map_or(0, Vec::len) as u64);
        let mut nodes: u64 = 1;
        let mut depth = 1;
        for dep in p.dependencies() {
            let (w, n, d) = self.cost(dep, path, costs)?;
            work = work.saturating_add(w);
            nodes = nodes.saturating_add(n);
            depth = depth.max(d + 1);
        }
        for i in p.items.iter().flatten() {
            work = work.saturating_add(size(self.parts[&i.part].size)? * (i.scale as u64).pow(2));
        }
        path.pop();
        if work > MAX_WORK || nodes > MAX_INSTANCES {
            return Err(format!("part '{name}' expands beyond the work budget"));
        }
        costs.insert(name.into(), (work, nodes, depth));
        Ok((work, nodes, depth))
    }
}
