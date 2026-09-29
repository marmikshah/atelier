use super::{Asset, Cel, color};
use crate::document::Document;
use image::{Rgba, RgbaImage};
use std::collections::BTreeMap;

/// Compile a layered recipe using bounded, already decoded PNG resources.
pub fn compile(asset: &Asset, images: &BTreeMap<String, RgbaImage>) -> Result<Document, String> {
    asset.validate()?;
    let mut document = Document::from_metadata(asset.metadata()?)?;
    for (layer, description) in asset.layers.iter().enumerate() {
        for cel in &description.cels {
            let img = render_cel(asset, cel, images)?;
            for frame in &cel.frames {
                document.set_cel(layer, *frame, cel.at[0], cel.at[1], img.clone())?;
            }
        }
    }
    Ok(document)
}

fn render_cel(
    asset: &Asset,
    cel: &Cel,
    images: &BTreeMap<String, RgbaImage>,
) -> Result<RgbaImage, String> {
    let size = cel.size(asset.canvas);
    let mut img = RgbaImage::new(size[0], size[1]);
    if let Some(grid) = &cel.grid {
        for (y, row) in grid.lines().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c != '.' {
                    img.put_pixel(
                        cel.origin[0] + x as u32,
                        cel.origin[1] + y as u32,
                        Rgba(color(
                            cel.legend
                                .get(&c.to_string())
                                .ok_or("grid symbol has no colour")?,
                        )?),
                    );
                }
            }
        }
    }
    if let Some(path) = &cel.image {
        let source = images
            .get(path)
            .ok_or_else(|| format!("missing image '{path}'"))?;
        super::validate::fits(cel, asset.canvas, [source.width(), source.height()])?;
        for (x, y, px) in source.enumerate_pixels() {
            img.put_pixel(cel.origin[0] + x, cel.origin[1] + y, *px);
        }
    }
    if !cel.draw.is_empty() {
        let mut doc = Document::new("cel", size[0], size[1]);
        doc.set_palette(
            cel.palette
                .as_ref()
                .unwrap_or(&asset.palette)
                .iter()
                .map(|s| color(s))
                .collect::<Result<_, _>>()?,
        )?;
        doc.set_cel(0, 0, 0, 0, img)?;
        for op in &cel.draw {
            doc.apply_op(0, 0, super::model::drawing_op(op)?.as_ref())?;
        }
        img = doc.cel_full(0, 0);
    }
    Ok(img)
}
