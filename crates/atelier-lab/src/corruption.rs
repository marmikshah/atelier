//! Deterministic synthetic corruptions for critic pretraining (lab.md Phase 5).
//!
//! The first five families operate on the exact palette-index raster rather
//! than encoded PNGs. Every result carries exact forward and inverse edits, so
//! the same record supports ranking, localization, and one-action repair data.

use serde::{Deserialize, Serialize};

/// One static indexed sprite: transparent cells are `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSprite {
    pub width: u32,
    pub height: u32,
    pub palette: Vec<[u8; 4]>,
    pub indices: Vec<Option<u32>>,
}

impl IndexedSprite {
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err("sprite dimensions must be non-zero".into());
        }
        let expected = self.width as usize * self.height as usize;
        if self.indices.len() != expected {
            return Err(format!(
                "indexed raster has {} cells, expected {expected}",
                self.indices.len()
            ));
        }
        if let Some(index) = self
            .indices
            .iter()
            .flatten()
            .find(|index| **index as usize >= self.palette.len())
        {
            return Err(format!(
                "palette index {index} is out of range for {} colors",
                self.palette.len()
            ));
        }
        Ok(())
    }

    fn offset(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorruptionKind {
    IsolatedPixelInsertion,
    PaletteBloat,
    BrokenOutline,
    SilhouetteCollision,
    ReducedValueContrast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Subtle,
    Moderate,
    Severe,
}

impl Severity {
    fn count(self) -> usize {
        match self {
            Severity::Subtle => 1,
            Severity::Moderate => 2,
            Severity::Severe => 4,
        }
    }

    fn blend_num(self) -> u16 {
        match self {
            Severity::Subtle => 1,
            Severity::Moderate => 2,
            Severity::Severe => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelEdit {
    pub x: u32,
    pub y: u32,
    pub before: Option<u32>,
    pub after: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteEdit {
    pub index: usize,
    pub before: Option<[u8; 4]>,
    pub after: Option<[u8; 4]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorruptionOperation {
    pub pixel_edits: Vec<PixelEdit>,
    pub palette_edits: Vec<PaletteEdit>,
}

impl CorruptionOperation {
    fn inverse(&self) -> Self {
        CorruptionOperation {
            pixel_edits: self
                .pixel_edits
                .iter()
                .map(|e| PixelEdit {
                    x: e.x,
                    y: e.y,
                    before: e.after,
                    after: e.before,
                })
                .collect(),
            palette_edits: self
                .palette_edits
                .iter()
                .rev()
                .map(|e| PaletteEdit {
                    index: e.index,
                    before: e.after,
                    after: e.before,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorruptionRecord {
    pub clean: IndexedSprite,
    pub corrupted: IndexedSprite,
    pub kind: CorruptionKind,
    /// Inclusive `[x0, y0, x1, y1]` containing the visible effect.
    pub affected_region: [u32; 4],
    pub severity: Severity,
    pub forward_operation: CorruptionOperation,
    pub possible_inverse_operation: CorruptionOperation,
}

/// Apply an exact recorded edit. This is also the repair-data execution path.
pub fn apply_operation(
    sprite: &IndexedSprite,
    operation: &CorruptionOperation,
) -> Result<IndexedSprite, String> {
    sprite.validate()?;
    let mut out = sprite.clone();
    for edit in &operation.palette_edits {
        match (edit.before, edit.after) {
            (None, Some(color)) if edit.index == out.palette.len() => out.palette.push(color),
            (Some(expected), None)
                if edit.index + 1 == out.palette.len()
                    && out.palette.get(edit.index) == Some(&expected) =>
            {
                out.palette.pop();
            }
            (Some(expected), Some(color)) if out.palette.get(edit.index) == Some(&expected) => {
                out.palette[edit.index] = color;
            }
            _ => {
                return Err(format!(
                    "palette edit {} does not match the sprite",
                    edit.index
                ))
            }
        }
    }
    for edit in &operation.pixel_edits {
        if edit.x >= out.width || edit.y >= out.height {
            return Err(format!(
                "pixel edit ({},{}) is out of bounds",
                edit.x, edit.y
            ));
        }
        let offset = out.offset(edit.x, edit.y);
        if out.indices[offset] != edit.before {
            return Err(format!(
                "pixel edit ({},{}) expected {:?}, found {:?}",
                edit.x, edit.y, edit.before, out.indices[offset]
            ));
        }
        out.indices[offset] = edit.after;
    }
    out.validate()?;
    Ok(out)
}

#[derive(Clone, Copy)]
struct DeterministicRng(u64);

impl DeterministicRng {
    fn next(&mut self, len: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 32) as usize) % len
    }
}

fn luma([r, g, b, _]: [u8; 4]) -> u16 {
    (r as u16 * 54 + g as u16 * 183 + b as u16 * 19) / 256
}

fn foreground(sprite: &IndexedSprite) -> Vec<(u32, u32)> {
    sprite
        .indices
        .iter()
        .enumerate()
        .filter_map(|(i, value)| value.map(|_| (i as u32 % sprite.width, i as u32 / sprite.width)))
        .collect()
}

fn bbox(points: &[(u32, u32)]) -> Result<[u32; 4], String> {
    let &(x, y) = points.first().ok_or("corruption affected no pixels")?;
    Ok(points.iter().skip(1).fold([x, y, x, y], |b, &(px, py)| {
        [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)]
    }))
}

fn neighbors(sprite: &IndexedSprite, x: u32, y: u32) -> impl Iterator<Item = (u32, u32)> {
    let width = sprite.width;
    let height = sprite.height;
    (-1..=1).flat_map(move |dy| {
        (-1..=1).filter_map(move |dx| {
            if dx == 0 && dy == 0 {
                return None;
            }
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            (nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32)
                .then_some((nx as u32, ny as u32))
        })
    })
}

fn edit_pixels(
    sprite: &IndexedSprite,
    points: impl IntoIterator<Item = (u32, u32, Option<u32>)>,
) -> CorruptionOperation {
    let pixel_edits = points
        .into_iter()
        .map(|(x, y, after)| PixelEdit {
            x,
            y,
            before: sprite.indices[sprite.offset(x, y)],
            after,
        })
        .collect();
    CorruptionOperation {
        pixel_edits,
        palette_edits: vec![],
    }
}

fn isolated_pixels(
    sprite: &IndexedSprite,
    severity: Severity,
    rng: &mut DeterministicRng,
) -> Result<CorruptionOperation, String> {
    let used = sprite
        .indices
        .iter()
        .flatten()
        .copied()
        .next()
        .ok_or("isolated-pixel corruption needs foreground")?;
    let mut candidates: Vec<(u32, u32)> = (0..sprite.height)
        .flat_map(|y| (0..sprite.width).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            sprite.indices[sprite.offset(x, y)].is_none()
                && neighbors(sprite, x, y)
                    .all(|(nx, ny)| sprite.indices[sprite.offset(nx, ny)].is_none())
        })
        .collect();
    if candidates.is_empty() {
        return Err("no isolated transparent cells are available".into());
    }
    let mut edits = Vec::new();
    for _ in 0..severity.count().min(candidates.len()) {
        let picked = candidates.swap_remove(rng.next(candidates.len()));
        edits.push((picked.0, picked.1, Some(used)));
    }
    Ok(edit_pixels(sprite, edits))
}

fn palette_bloat(
    sprite: &IndexedSprite,
    severity: Severity,
    rng: &mut DeterministicRng,
) -> Result<CorruptionOperation, String> {
    let foreground = foreground(sprite);
    let &(x, y) = foreground
        .get(rng.next(foreground.len().max(1)))
        .ok_or("palette-bloat corruption needs foreground")?;
    let original_index = sprite.indices[sprite.offset(x, y)].expect("foreground is opaque");
    let mut near = sprite.palette[original_index as usize];
    let delta = severity.count() as u8;
    near[0] = near[0]
        .saturating_add(delta)
        .max(near[0].saturating_sub(delta));
    if near == sprite.palette[original_index as usize] {
        near[2] = near[2].saturating_sub(delta.max(1));
    }
    let new_index = sprite.palette.len() as u32;
    let mut candidates: Vec<_> = foreground
        .into_iter()
        .filter(|&(px, py)| sprite.indices[sprite.offset(px, py)] == Some(original_index))
        .collect();
    let mut pixel_edits = Vec::new();
    for _ in 0..severity.count().min(candidates.len()) {
        let (px, py) = candidates.swap_remove(rng.next(candidates.len()));
        pixel_edits.push(PixelEdit {
            x: px,
            y: py,
            before: Some(original_index),
            after: Some(new_index),
        });
    }
    Ok(CorruptionOperation {
        pixel_edits,
        palette_edits: vec![PaletteEdit {
            index: sprite.palette.len(),
            before: None,
            after: Some(near),
        }],
    })
}

fn broken_outline(
    sprite: &IndexedSprite,
    severity: Severity,
    rng: &mut DeterministicRng,
) -> Result<CorruptionOperation, String> {
    let darkest = sprite
        .indices
        .iter()
        .flatten()
        .copied()
        .min_by_key(|index| luma(sprite.palette[*index as usize]))
        .ok_or("broken-outline corruption needs foreground")?;
    let mut candidates: Vec<_> = foreground(sprite)
        .into_iter()
        .filter(|&(x, y)| {
            sprite.indices[sprite.offset(x, y)] == Some(darkest)
                && neighbors(sprite, x, y)
                    .any(|(nx, ny)| sprite.indices[sprite.offset(nx, ny)].is_none())
        })
        .collect();
    if candidates.is_empty() {
        return Err("no dark boundary pixels are available".into());
    }
    let mut edits = Vec::new();
    for _ in 0..severity.count().min(candidates.len()) {
        let (x, y) = candidates.swap_remove(rng.next(candidates.len()));
        edits.push((x, y, None));
    }
    Ok(edit_pixels(sprite, edits))
}

fn silhouette_collision(
    sprite: &IndexedSprite,
    severity: Severity,
    rng: &mut DeterministicRng,
) -> Result<CorruptionOperation, String> {
    let foreground = foreground(sprite);
    if foreground.is_empty() {
        return Err("silhouette-collision corruption needs foreground".into());
    }
    let mut gaps = Vec::new();
    for &(x, y) in &foreground {
        let index = sprite.indices[sprite.offset(x, y)].expect("foreground is opaque");
        for (dx, dy) in [(1i32, 0i32), (0, 1)] {
            for distance in 2..=5 {
                let tx = x as i32 + dx * distance;
                let ty = y as i32 + dy * distance;
                if tx < 0 || ty < 0 || tx >= sprite.width as i32 || ty >= sprite.height as i32 {
                    break;
                }
                if sprite.indices[sprite.offset(tx as u32, ty as u32)].is_some() {
                    let between: Vec<_> = (1..distance)
                        .map(|d| ((x as i32 + dx * d) as u32, (y as i32 + dy * d) as u32))
                        .collect();
                    if between
                        .iter()
                        .all(|&(gx, gy)| sprite.indices[sprite.offset(gx, gy)].is_none())
                    {
                        gaps.push((index, between));
                    }
                    break;
                }
            }
        }
    }
    let (index, mut points) = gaps
        .get(rng.next(gaps.len().max(1)))
        .cloned()
        .ok_or("no silhouette gap is available to collide")?;
    points.truncate(severity.count().min(points.len()).max(1));
    Ok(edit_pixels(
        sprite,
        points.into_iter().map(|(x, y)| (x, y, Some(index))),
    ))
}

fn reduced_contrast(
    sprite: &IndexedSprite,
    severity: Severity,
) -> Result<CorruptionOperation, String> {
    let mut used: Vec<u32> = sprite.indices.iter().flatten().copied().collect();
    used.sort_unstable();
    used.dedup();
    if used.len() < 2 {
        return Err("reduced-contrast corruption needs at least two used colors".into());
    }
    let darkest = *used
        .iter()
        .min_by_key(|index| luma(sprite.palette[**index as usize]))
        .expect("used is nonempty");
    let lightest = *used
        .iter()
        .max_by_key(|index| luma(sprite.palette[**index as usize]))
        .expect("used is nonempty");
    let dark = sprite.palette[darkest as usize];
    let light = sprite.palette[lightest as usize];
    let n = severity.blend_num();
    let blend = |a: u8, b: u8| ((a as u16 * (4 - n) + b as u16 * n) / 4) as u8;
    let after = [
        blend(light[0], dark[0]),
        blend(light[1], dark[1]),
        blend(light[2], dark[2]),
        light[3],
    ];
    Ok(CorruptionOperation {
        pixel_edits: vec![],
        palette_edits: vec![PaletteEdit {
            index: lightest as usize,
            before: Some(light),
            after: Some(after),
        }],
    })
}

/// Produce one reproducible clean/corrupted training pair.
pub fn corrupt(
    clean: &IndexedSprite,
    kind: CorruptionKind,
    severity: Severity,
    seed: u64,
) -> Result<CorruptionRecord, String> {
    clean.validate()?;
    let mut rng = DeterministicRng(seed ^ 0xA7E1_1E5E_DA7A_5EED);
    let forward_operation = match kind {
        CorruptionKind::IsolatedPixelInsertion => isolated_pixels(clean, severity, &mut rng)?,
        CorruptionKind::PaletteBloat => palette_bloat(clean, severity, &mut rng)?,
        CorruptionKind::BrokenOutline => broken_outline(clean, severity, &mut rng)?,
        CorruptionKind::SilhouetteCollision => silhouette_collision(clean, severity, &mut rng)?,
        CorruptionKind::ReducedValueContrast => reduced_contrast(clean, severity)?,
    };
    let corrupted = apply_operation(clean, &forward_operation)?;
    if corrupted == *clean {
        return Err("corruption produced no change".into());
    }
    let changed_points: Vec<_> = if forward_operation.pixel_edits.is_empty() {
        foreground(clean)
    } else {
        forward_operation
            .pixel_edits
            .iter()
            .map(|e| (e.x, e.y))
            .collect()
    };
    let affected_region = bbox(&changed_points)?;
    let possible_inverse_operation = forward_operation.inverse();
    Ok(CorruptionRecord {
        clean: clean.clone(),
        corrupted,
        kind,
        affected_region,
        severity,
        forward_operation,
        possible_inverse_operation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite() -> IndexedSprite {
        let mut indices = vec![None; 64];
        // Two outlined blocks with a two-pixel gap: enough structure for all
        // five deterministic corruption families.
        for y in 2..=5 {
            for x in 1..=2 {
                indices[y * 8 + x] = Some(if x == 1 || y == 2 || y == 5 { 0 } else { 1 });
            }
            for x in 5..=6 {
                indices[y * 8 + x] = Some(if x == 6 || y == 2 || y == 5 { 0 } else { 2 });
            }
        }
        IndexedSprite {
            width: 8,
            height: 8,
            palette: vec![[12, 14, 18, 255], [170, 45, 55, 255], [245, 210, 90, 255]],
            indices,
        }
    }

    #[test]
    fn all_initial_corruptions_are_deterministic_and_repairable() {
        let clean = sprite();
        for kind in [
            CorruptionKind::IsolatedPixelInsertion,
            CorruptionKind::PaletteBloat,
            CorruptionKind::BrokenOutline,
            CorruptionKind::SilhouetteCollision,
            CorruptionKind::ReducedValueContrast,
        ] {
            for severity in [Severity::Subtle, Severity::Moderate, Severity::Severe] {
                let a = corrupt(&clean, kind, severity, 42).unwrap();
                let b = corrupt(&clean, kind, severity, 42).unwrap();
                assert_eq!(a, b, "{kind:?} {severity:?} must be deterministic");
                assert_ne!(a.clean, a.corrupted);
                assert_eq!(
                    apply_operation(&a.corrupted, &a.possible_inverse_operation).unwrap(),
                    clean,
                    "{kind:?} {severity:?} inverse must be exact"
                );
            }
        }
    }

    #[test]
    fn malformed_indexed_sprites_are_rejected() {
        let mut bad = sprite();
        bad.indices.pop();
        assert!(corrupt(&bad, CorruptionKind::BrokenOutline, Severity::Subtle, 1).is_err());
    }
}
