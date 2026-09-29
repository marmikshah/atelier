use std::collections::BTreeMap;

use image::{Rgba, RgbaImage};

use super::validate::{fits, grid_size};
use super::{Asset, Instance, Placement, color};
use crate::document::Document;
use crate::raster;

/// Compile using already bounded, decoded image resources. Missing resources
/// are errors; there is no network, fallback font lookup or filesystem access.
pub fn compile(asset: &Asset, images: &BTreeMap<String, RgbaImage>) -> Result<Document, String> {
    asset.validate()?;
    for (name, part) in &asset.parts {
        if let Some(path) = &part.image {
            let img = images
                .get(path)
                .ok_or_else(|| format!("part '{name}': missing image '{path}'"))?;
            fits(part, [img.width(), img.height()])?;
        }
    }
    let mut document = Document::from_metadata(asset.metadata()?)?;
    for cel in &asset.cels {
        let img = render(asset, &cel.part, images, &BTreeMap::new(), 0)?;
        let layer = asset
            .layers
            .iter()
            .position(|l| l.id == cel.layer)
            .ok_or("missing layer")?;
        for frame in &cel.frames {
            document.set_cel(layer, *frame, cel.at[0], cel.at[1], img.clone())?;
        }
    }
    Ok(document)
}

fn render(
    asset: &Asset,
    name: &str,
    images: &BTreeMap<String, RgbaImage>,
    bindings: &BTreeMap<String, String>,
    depth: usize,
) -> Result<RgbaImage, String> {
    if depth >= 64 {
        return Err("source dependency depth exceeds 64".into());
    }
    let p = &asset.parts[name];
    let mut img = RgbaImage::new(p.size[0], p.size[1]);
    if let Some(grid) = &p.grid {
        grid_size(grid)?;
        for (y, row) in grid.lines().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c != '.' {
                    img.put_pixel(
                        p.origin[0] + x as u32,
                        p.origin[1] + y as u32,
                        Rgba(asset.ink(&p.legend[&c.to_string()], bindings)?),
                    );
                }
            }
        }
    } else if let Some(path) = &p.image {
        let src = &images[path];
        for (x, y, px) in src.enumerate_pixels() {
            img.put_pixel(p.origin[0] + x, p.origin[1] + y, *px);
        }
    } else if let Some(items) = &p.items {
        for i in items {
            let mut bound = bindings.clone();
            // Resolve against the parent's bindings before adding local values.
            for (name, ink) in &i.bindings {
                bound.insert(name.clone(), super::hex(&asset.ink(ink, bindings)?));
            }
            let src = render(asset, &i.part, images, &bound, depth + 1)?;
            place(&mut img, &src, i);
        }
    } else if let Some(ops) = &p.draw {
        if let Some(base) = &p.base {
            img = render(asset, base, images, bindings, depth + 1)?;
        }
        let mut doc = Document::new(name, p.size[0], p.size[1]);
        doc.set_palette(
            p.palette
                .as_ref()
                .unwrap_or(&asset.palette)
                .iter()
                .map(|s| color(s))
                .collect::<Result<_, _>>()?,
        )?;
        doc.set_cel(0, 0, 0, 0, img)?;
        for (n, op) in ops.iter().enumerate() {
            doc.apply_op(0, 0, super::model::drawing_op(op)?.as_ref())
                .map_err(|e| format!("part '{name}', draw {}: {e}", n + 1))?;
        }
        img = doc.cel_full(0, 0);
    }
    Ok(img)
}

fn place(dst: &mut RgbaImage, src: &RgbaImage, i: &Instance) {
    let (w, h) = (src.width(), src.height());
    for (x, y, px) in src.enumerate_pixels() {
        let x = if i.flip_x { w - 1 - x } else { x };
        let y = if i.flip_y { h - 1 - y } else { y };
        let (x, y) = match i.turns {
            1 => (h - 1 - y, x),
            2 => (w - 1 - x, h - 1 - y),
            3 => (y, w - 1 - x),
            _ => (x, y),
        };
        for dy in 0..i.scale {
            let ty = i.at[1] as i64 + y as i64 * i.scale as i64 + dy as i64;
            if ty < 0 || ty >= dst.height() as i64 {
                continue;
            }
            for dx in 0..i.scale {
                let tx = i.at[0] as i64 + x as i64 * i.scale as i64 + dx as i64;
                if tx < 0 || tx >= dst.width() as i64 {
                    continue;
                }
                let value = if i.mode == Placement::Replace {
                    *px
                } else if px[3] == 0 {
                    continue;
                } else {
                    Rgba(raster::composite_px(
                        dst.get_pixel(tx as u32, ty as u32).0,
                        px.0,
                        i.opacity as f32 / 255.0,
                        i.blend,
                    ))
                };
                dst.put_pixel(tx as u32, ty as u32, value);
            }
        }
    }
}
