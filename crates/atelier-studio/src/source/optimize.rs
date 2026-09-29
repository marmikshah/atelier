//! Small, exact per-call reductions. Metadata is current state; pixel edits
//! replace changed cels, while economical existing draw operations are retained.
use super::{Source, format};
use crate::ToolName;
use atelier_core::document::Document;
use atelier_core::source::{Asset, Cel, Layer, compile, hex};
use image::RgbaImage;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

fn skeleton(doc: &Document) -> Asset {
    let m = doc.meta();
    Asset {
        format: 1,
        renderer: 1,
        name: m.name.clone(),
        canvas: [m.w, m.h],
        frames: m.frames.iter().map(|f| f.duration_ms).collect(),
        palette: m.palette.iter().map(hex).collect(),
        layers: m
            .layers
            .iter()
            .map(|l| Layer {
                name: l.name.clone(),
                opacity: l.opacity,
                visible: l.visible,
                blend: l.blend,
                cels: vec![],
            })
            .collect(),
        tags: m.tags.clone(),
        reference: m.reference.clone(),
    }
}
fn fingerprint(image: &RgbaImage) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(image.width().to_le_bytes());
    h.update(image.height().to_le_bytes());
    h.update(image.as_raw());
    h.finalize().to_vec()
}
fn pixel_cel(
    image: &RgbaImage,
    canvas: [u32; 2],
    files: &mut BTreeMap<String, Vec<u8>>,
    images: &mut BTreeMap<String, RgbaImage>,
) -> Result<Cel, String> {
    let dimensions = [image.width(), image.height()];
    let (origin, cropped) = crop(image);
    let mut cel = Cel {
        frames: vec![0],
        size: (dimensions != canvas).then_some(dimensions),
        origin,
        ..Cel::default()
    };
    if let Some((grid, legend)) = grid(&cropped) {
        cel.grid = Some(grid);
        cel.legend = legend;
    } else {
        let path = format!("pixels/{:x}.png", Sha256::digest(fingerprint(&cropped)));
        if !files.contains_key(&path) {
            files.insert(path.clone(), encode_png(&cropped)?);
        }
        images.insert(path.clone(), cropped);
        cel.image = Some(path);
    }
    Ok(cel)
}
fn cost(cel: &Cel, files: &BTreeMap<String, Vec<u8>>) -> usize {
    format::cel_bytes(cel) + cel.image.as_ref().map_or(0, |p| files[p].len())
}
fn matches(
    asset: &Asset,
    layer: usize,
    cel: &Cel,
    images: &BTreeMap<String, RgbaImage>,
    expected: &RgbaImage,
) -> bool {
    let mut candidate = asset.clone();
    for l in &mut candidate.layers {
        l.cels.clear();
    }
    candidate.layers[layer].cels.push(cel.clone());
    compile(&candidate, images).is_ok_and(|d| {
        d.cel(layer, cel.frames[0])
            .is_some_and(|(_, _, image)| image == expected)
    })
}
/// Rebuild only the representation; all candidates must reproduce exact RGBA.
pub(super) fn current(
    doc: &Document,
    dir: &Path,
    previous: Option<&Source>,
    call: Option<(ToolName, &Value)>,
) -> Result<Source, String> {
    let mut asset = skeleton(doc);
    let mut files = previous.map_or_else(BTreeMap::new, |p| p.files.clone());
    let mut images = previous.map_or_else(BTreeMap::new, |p| p.images.clone());
    let before = previous.map(Source::compile).transpose()?;
    let mut known = BTreeMap::new();
    if let (Some(source), Some(before)) = (previous, before.as_ref()) {
        for (layer, description) in source.asset.layers.iter().enumerate() {
            for c in &description.cels {
                if let Some((_, _, pixels)) = before.cel(layer, c.frames[0]) {
                    let mut c = c.clone();
                    if c.palette.is_none() && !c.draw.is_empty() {
                        c.palette = Some(source.asset.palette.clone());
                    }
                    c.size = Some([pixels.width(), pixels.height()]);
                    known.insert(fingerprint(pixels), c);
                }
            }
        }
    }
    for layer_index in 0..doc.meta().layers.len() {
        for frame_index in 0..doc.meta().frames.len() {
            let Some((x, y, pixels)) = doc.cel(layer_index, frame_index) else {
                continue;
            };
            // Unchanged cels already have an exact, compact description. Keep
            // it directly: metadata edits must not recompress every PNG.
            let mut selected = match known.get(&fingerprint(pixels)) {
                Some(old) => old.clone(),
                None => pixel_cel(pixels, asset.canvas, &mut files, &mut images)?,
            };
            selected.frames = vec![frame_index];
            selected.at = [x, y];
            selected.size = ([pixels.width(), pixels.height()] != asset.canvas)
                .then_some([pixels.width(), pixels.height()]);
            if selected.palette.as_ref() == Some(&asset.palette) {
                selected.palette = None;
            }
            if let Some((_, args)) =
                call.filter(|(t, _)| matches!(t, ToolName::DocDraw | ToolName::DocFx))
            {
                let layer = args
                    .get("layer")
                    .and_then(Value::as_u64)
                    .unwrap_or(usize::MAX as u64) as usize;
                let start = args
                    .get("frame")
                    .and_then(Value::as_u64)
                    .unwrap_or(usize::MAX as u64) as usize;
                let end = args
                    .get("frame_to")
                    .and_then(Value::as_u64)
                    .map_or(start, |v| v as usize);
                if layer == layer_index
                    && (start..=end).contains(&frame_index)
                    && [pixels.width(), pixels.height()] == asset.canvas
                    && [x, y] == [0, 0]
                {
                    let mut op = args
                        .as_object()
                        .cloned()
                        .ok_or("drawing arguments must be an object")?;
                    for key in ["doc_id", "layer", "frame", "frame_to", "expected_revision"] {
                        op.remove(key);
                    }
                    let op = Value::Object(op);
                    let mut candidates = vec![Cel {
                        frames: vec![frame_index],
                        draw: vec![op.clone()],
                        ..Cel::default()
                    }];
                    if let Some(before) = before.as_ref()
                        && let Some((bx, by, old_pixels)) = before.cel(layer, frame_index)
                        && [bx, by] == [0, 0]
                        && [old_pixels.width(), old_pixels.height()] == asset.canvas
                    {
                        let mut base = match known.get(&fingerprint(old_pixels)) {
                            Some(c) => c.clone(),
                            None => pixel_cel(old_pixels, asset.canvas, &mut files, &mut images)?,
                        };
                        if !base.draw.is_empty() && base.palette.as_ref() != Some(&asset.palette) {
                            base = pixel_cel(old_pixels, asset.canvas, &mut files, &mut images)?;
                        }
                        base.frames = vec![frame_index];
                        base.at = [0, 0];
                        base.palette = None;
                        base.draw.push(op);
                        candidates.push(base);
                    }
                    for candidate in candidates {
                        if cost(&candidate, &files) < cost(&selected, &files)
                            && matches(&asset, layer, &candidate, &images, pixels)
                        {
                            selected = candidate;
                        }
                    }
                }
            }
            let cels = &mut asset.layers[layer_index].cels;
            let equivalent = cels.iter_mut().find(|c| {
                let mut a = (*c).clone();
                a.frames = selected.frames.clone();
                a == selected
            });
            if let Some(existing) = equivalent {
                existing.frames.push(frame_index);
            } else {
                cels.push(selected);
            }
        }
    }
    if let Some(reference) = &asset.reference {
        files.insert(
            reference.clone(),
            super::load::read_file(&dir.join(reference), 256 * 1024 * 1024)?,
        );
    }
    if asset.validate().is_err() && (previous.is_some() || call.is_some()) {
        return current(doc, dir, None, None);
    }
    let mut named = BTreeMap::new();
    for (index, layer) in asset.layers.iter_mut().enumerate() {
        let stem = layer
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(40)
            .collect::<String>();
        for cel in &mut layer.cels {
            if let Some(old) = &cel.image {
                let path = format!(
                    "pixels/{}-L{index}-F{}.png",
                    stem.trim_matches('-'),
                    cel.frames[0]
                );
                named.insert(path.clone(), files[old].clone());
                cel.image = Some(path);
            }
        }
    }
    if let Some(reference) = &asset.reference {
        named.insert(reference.clone(), files[reference].clone());
    }
    let text = format::encode(&asset)?;
    // Verify the serialized representation that will actually be replayed.
    let asset: Asset =
        toml_edit::de::from_str(&text).map_err(|e| format!("invalid encoded recipe: {e}"))?;
    let source = Source::from_parts(text, asset, named)?;
    equivalent(doc, &source.compile()?)?;
    Ok(source)
}
fn crop(image: &RgbaImage) -> ([u32; 2], RgbaImage) {
    let (mut x0, mut y0, mut x1, mut y1) = (image.width(), image.height(), 0, 0);
    for (x, y, p) in image.enumerate_pixels() {
        // Alpha alone is not sufficient: invisible RGB is still authored data.
        if p.0 != [0, 0, 0, 0] {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == image.width() {
        ([0, 0], RgbaImage::new(1, 1))
    } else {
        (
            [x0, y0],
            image::imageops::crop_imm(image, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image(),
        )
    }
}

fn grid(image: &RgbaImage) -> Option<(String, BTreeMap<String, String>)> {
    if image.width() as u64 * image.height() as u64 > 1024 {
        return None;
    }
    let symbols = b"123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz@#$%&*+";
    let mut colors = BTreeMap::new();
    let mut text = String::new();
    for row in image.rows() {
        for p in row {
            if p.0 == [0, 0, 0, 0] {
                text.push('.');
                continue;
            }
            let next_index = colors.len();
            let c = match colors.entry(p.0) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    *entry.insert(*symbols.get(next_index)? as char)
                }
            };
            text.push(c);
        }
        text.push('\n');
    }
    Some((
        text,
        colors
            .into_iter()
            .map(|(c, s)| (s.to_string(), hex(&c)))
            .collect(),
    ))
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut rgba = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut rgba, image.width(), image.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        encoder
            .write_header()
            .map_err(|e| e.to_string())?
            .write_image_data(image.as_raw())
            .map_err(|e| e.to_string())?;
    }
    let mut table = BTreeMap::new();
    let mut colors = vec![];
    let mut indices = Vec::with_capacity(image.width() as usize * image.height() as usize);
    for p in image.pixels() {
        if let std::collections::btree_map::Entry::Vacant(entry) = table.entry(p.0) {
            if colors.len() == 256 {
                return Ok(rgba);
            }
            entry.insert(colors.len() as u8);
            colors.push(p.0);
        }
        indices.push(table[&p.0]);
    }
    for filter in [png::Filter::Adaptive, png::Filter::NoFilter] {
        let mut indexed = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut indexed, image.width(), image.height());
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(
                colors
                    .iter()
                    .flat_map(|p| p[..3].iter().copied())
                    .collect::<Vec<_>>(),
            );
            encoder.set_trns(colors.iter().map(|p| p[3]).collect::<Vec<_>>());
            encoder.set_compression(png::Compression::High);
            encoder.set_filter(filter);
            encoder
                .write_header()
                .map_err(|e| e.to_string())?
                .write_image_data(&indices)
                .map_err(|e| e.to_string())?;
        }
        if indexed.len() < rgba.len() {
            rgba = indexed;
        }
    }
    Ok(rgba)
}

pub(crate) fn equivalent(before: &Document, after: &Document) -> Result<u64, String> {
    let mut a = serde_json::to_value(before.meta()).map_err(|e| e.to_string())?;
    let mut b = serde_json::to_value(after.meta()).map_err(|e| e.to_string())?;
    a.as_object_mut().unwrap().remove("cels");
    b.as_object_mut().unwrap().remove("cels");
    if a != b || before.structure() != after.structure() {
        return Err("recipe changed document structure".into());
    }
    let mut checked = 0;
    for l in 0..before.meta().layers.len() {
        for f in 0..before.meta().frames.len() {
            match (before.cel(l, f), after.cel(l, f)) {
                (None, None) => {}
                (Some((ax, ay, a)), Some((bx, by, b))) if ax == bx && ay == by && a == b => {
                    checked += a.width() as u64 * a.height() as u64;
                }
                _ => {
                    return Err(format!(
                        "recipe changed cel ({l},{f}): pixels, bounds or offset differ"
                    ));
                }
            }
        }
    }
    Ok(checked)
}
