use super::{Asset, Cel, PixelData, Pixels, Step, color};
use crate::document::Document;
use image::{Rgba, RgbaImage};
use std::collections::BTreeMap;

/// Compile the ordered source with the existing renderer. Pixels are decoded
/// directly into bounded cels; sparse spans never allocate padded grids.
pub fn compile(asset: &Asset) -> Result<Document, String> {
    asset.validate()?;
    let mut document = Document::from_metadata(asset.metadata()?)?;
    for (layer, description) in asset.layers.iter().enumerate() {
        for cel in &description.cels {
            let img = render_cel(asset, cel)?;
            for frame in &cel.frames {
                document.set_cel(layer, *frame, cel.at[0], cel.at[1], img.clone())?;
            }
        }
    }
    Ok(document)
}
fn render_cel(asset: &Asset, cel: &Cel) -> Result<RgbaImage, String> {
    let [w, h] = cel.size(asset.canvas);
    let mut img = RgbaImage::new(w, h);
    let mut doc = Document::new("cel", w.min(4096), h.min(4096));
    for step in &cel.steps {
        match step {
            Step::Pixels(pixels) => paint(pixels, &mut img)?,
            Step::Draw { op, palette } => {
                doc.set_cel(0, 0, 0, 0, img)?;
                doc.set_palette(palette.iter().map(|s| color(s)).collect::<Result<_, _>>()?)?;
                doc.apply_op(0, 0, op)?;
                img = doc
                    .cel(0, 0)
                    .map(|(_, _, p)| p.clone())
                    .unwrap_or_else(|| RgbaImage::new(w, h));
            }
        }
    }
    Ok(img)
}
/// Visit explicit writes only. Coordinates and counts are validated first.
pub(super) fn visit(
    pixels: &Pixels,
    mut put: impl FnMut(u32, u32, &str) -> Result<(), String>,
) -> Result<(), String> {
    let [ox, oy] = pixels.origin;
    let mut cursor = 0u32;
    for data in &pixels.data {
        match data {
            PixelData::Grid(text) => {
                for row in text.lines() {
                    for (x, symbol) in row.chars().enumerate() {
                        if symbol != '.' && symbol != ' ' {
                            put(ox + x as u32, oy + cursor, &symbol.to_string())?;
                        }
                    }
                    cursor += 1;
                }
            }
            PixelData::Row { runs, repeat } => {
                for dy in 0..*repeat {
                    let mut x = ox;
                    for (symbol, count) in runs {
                        if symbol != "." && symbol != " " {
                            for dx in 0..*count {
                                put(x + dx, oy + cursor + dy, symbol)?;
                            }
                        }
                        x += count;
                    }
                }
                cursor += repeat;
            }
            PixelData::Span { x, y, text, rows } => {
                for dy in 0..*rows {
                    for (dx, symbol) in text.chars().enumerate() {
                        put(ox + x + dx as u32, oy + y + dy, &symbol.to_string())?;
                    }
                }
            }
        }
    }
    Ok(())
}
fn paint(pixels: &Pixels, img: &mut RgbaImage) -> Result<(), String> {
    let colors = pixels
        .legend
        .iter()
        .map(|(s, c)| Ok((s.as_str(), Rgba(color(c)?))))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    visit(pixels, |x, y, s| {
        img.put_pixel(x, y, *colors.get(s).ok_or("pixel symbol has no colour")?);
        Ok(())
    })
}
