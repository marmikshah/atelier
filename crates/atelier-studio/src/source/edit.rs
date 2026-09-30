//! Update the editable construction after a tool call. Representations are
//! selected only when importing raster pixels; later edits preserve them.
use super::{Source, parse, write};
use crate::ToolName;
use atelier_core::document::Document;
use atelier_core::source::{Asset, Cel, Layer, PixelData, Pixels, Step, color, compile, hex};
use image::RgbaImage;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
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
fn symbol(index: usize) -> String {
    let ascii = b"123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz@#$%&*+";
    if let Some(s) = ascii.get(index) {
        return (*s as char).to_string();
    }
    let mut code = 0x100 + (index - ascii.len()) as u32;
    loop {
        if let Some(c) = char::from_u32(code)
            && !c.is_whitespace()
            && !c.is_control()
        {
            return c.to_string();
        }
        code += 1;
    }
}
fn mark(c: [u8; 4], legend: &mut BTreeMap<String, String>) -> String {
    let value = hex(&c);
    if let Some((s, _)) = legend.iter().find(|(_, v)| **v == value) {
        return s.clone();
    }
    let mut index = 0;
    while legend.contains_key(&symbol(index)) {
        index += 1;
    }
    let s = symbol(index);
    legend.insert(s.clone(), value);
    s
}
fn rows(
    image: &RgbaImage,
    origin: [u32; 2],
    size: [u32; 2],
    legend: &mut BTreeMap<String, String>,
) -> Vec<String> {
    let mut symbols = legend
        .iter()
        .map(|(s, c)| Ok((color(c)?, s.clone())))
        .collect::<Result<BTreeMap<_, _>, String>>()
        .expect("validated legend");
    let mut result = vec![];
    for y in origin[1]..origin[1] + size[1] {
        let mut row = String::new();
        for x in origin[0]..origin[0] + size[0] {
            let c = image.get_pixel(x, y).0;
            if c == [0; 4] {
                row.push('.');
            } else {
                let s = symbols.entry(c).or_insert_with(|| mark(c, legend));
                row.push_str(s);
            }
        }
        result.push(row);
    }
    result
}
fn bounds(image: &RgbaImage) -> Option<([u32; 2], [u32; 2])> {
    let (mut x0, mut y0, mut x1, mut y1) = (image.width(), image.height(), 0, 0);
    for (x, y, p) in image.enumerate_pixels() {
        if p.0 != [0; 4] {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 != image.width()).then(|| ([x0, y0], [x1 - x0 + 1, y1 - y0 + 1]))
}
fn runs(row: &str) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = vec![];
    for c in row.chars() {
        let s = c.to_string();
        if let Some((last, n)) = out.last_mut()
            && *last == s
        {
            *n += 1;
        } else {
            out.push((s, 1));
        }
    }
    out
}
fn row_data(rows: &[String]) -> Vec<PixelData> {
    let mut out: Vec<PixelData> = vec![];
    for row in rows {
        let r = runs(row);
        if let Some(PixelData::Row { runs, repeat }) = out.last_mut()
            && *runs == r
        {
            *repeat += 1;
        } else {
            out.push(PixelData::Row { runs: r, repeat: 1 });
        }
    }
    out
}
fn spans(rows: &[String]) -> Vec<PixelData> {
    let mut active: BTreeMap<(u32, String), (u32, u32)> = BTreeMap::new();
    let mut result = vec![];
    for (y, row) in rows.iter().enumerate() {
        let chars: Vec<_> = row.chars().collect();
        let mut current = BTreeMap::new();
        let mut x = 0usize;
        while x < chars.len() {
            if chars[x] == '.' || chars[x] == ' ' {
                x += 1;
                continue;
            }
            let start = x;
            while x < chars.len() && chars[x] != '.' && chars[x] != ' ' {
                x += 1;
            }
            current.insert(
                (start as u32, chars[start..x].iter().collect::<String>()),
                (),
            );
        }
        let completed: Vec<_> = active
            .keys()
            .filter(|key| !current.contains_key(*key))
            .cloned()
            .collect();
        for key in completed {
            let (start, height) = active.remove(&key).expect("active span");
            result.push(PixelData::Span {
                x: key.0,
                y: start,
                text: key.1,
                rows: height,
            });
        }
        for key in current.into_keys() {
            active
                .entry(key)
                .and_modify(|(_, height)| *height += 1)
                .or_insert((y as u32, 1));
        }
    }
    for ((x, text), (y, rows)) in active {
        result.push(PixelData::Span { x, y, text, rows });
    }
    result.sort_by_key(|d| match d {
        PixelData::Span { x, y, .. } => (*y, *x),
        _ => (0, 0),
    });
    result
}
fn capture(image: &RgbaImage) -> Option<Pixels> {
    let (origin, size) = bounds(image)?;
    let mut legend = BTreeMap::new();
    let rows = rows(image, origin, size, &mut legend);
    let total = size[0] as u64 * size[1] as u64;
    let blanks = rows
        .iter()
        .flat_map(|r| r.chars())
        .filter(|c| *c == '.')
        .count() as u64;
    // A documented import rule. It never changes an existing source's form.
    let data = if total <= 1024 {
        vec![PixelData::Grid(rows.join("\n") + "\n")]
    } else if blanks * 4 > total {
        spans(&rows)
    } else {
        row_data(&rows)
    };
    Some(Pixels {
        origin,
        legend,
        data,
    })
}
fn capture_as(image: &RgbaImage, previous: &Pixels) -> Option<Pixels> {
    let (origin, size) = bounds(image)?;
    let used: HashSet<_> = image.pixels().map(|p| p.0).collect();
    let mut legend = previous.legend.clone();
    legend.retain(|_, c| color(c).is_ok_and(|v| used.contains(&v)));
    let rows = rows(image, origin, size, &mut legend);
    let data = match previous.data.first() {
        Some(PixelData::Grid(_)) => vec![PixelData::Grid(rows.join("\n") + "\n")],
        Some(PixelData::Row { .. }) => row_data(&rows),
        _ => spans(&rows),
    };
    Some(Pixels {
        origin,
        legend,
        data,
    })
}
fn snapshot(image: &RgbaImage) -> Cel {
    Cel {
        frames: vec![0],
        steps: capture(image)
            .map(|p| vec![Step::Pixels(p)])
            .unwrap_or_default(),
        ..Cel::default()
    }
}
fn patch(before: &RgbaImage, after: &RgbaImage) -> Option<Pixels> {
    let mut p = Pixels::default();
    for y in 0..after.height() {
        let mut x = 0;
        while x < after.width() {
            if before.get_pixel(x, y) == after.get_pixel(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            let mut text = String::new();
            while x < after.width() && before.get_pixel(x, y) != after.get_pixel(x, y) {
                text.push_str(&mark(after.get_pixel(x, y).0, &mut p.legend));
                x += 1;
            }
            p.data.push(PixelData::Span {
                x: start,
                y,
                text,
                rows: 1,
            });
        }
    }
    (!p.data.is_empty()).then_some(p)
}
/// Combine adjacent sparse patches while retaining explicit transparent writes.
/// Large patches remain separate to keep the temporary coordinate map bounded.
fn combine(old: &Pixels, new: &Pixels) -> Option<Pixels> {
    if old.origin != [0, 0] || new.origin != [0, 0] {
        return None;
    }
    let mut written = BTreeMap::new();
    for p in [old, new] {
        for data in &p.data {
            let PixelData::Span { x, y, text, rows } = data else {
                return None;
            };
            if u64::from(*rows) * text.chars().count() as u64 > 4096 || written.len() > 4096 {
                return None;
            }
            for dy in 0..*rows {
                for (dx, s) in text.chars().enumerate() {
                    let c = color(p.legend.get(&s.to_string())?).ok()?;
                    written.insert((y + dy, x + dx as u32), c);
                }
            }
        }
    }
    if written.len() > 4096 {
        return None;
    }
    let used: HashSet<_> = written.values().copied().collect();
    let mut legend = old.legend.clone();
    legend.retain(|_, c| color(c).is_ok_and(|v| used.contains(&v)));
    let mut data: Vec<PixelData> = vec![];
    for ((y, x), c) in written {
        let symbol = mark(c, &mut legend);
        if let Some(PixelData::Span {
            x: begin,
            y: row,
            text,
            rows: 1,
        }) = data.last_mut()
            && *row == y
            && *begin + text.chars().count() as u32 == x
        {
            text.push_str(&symbol);
        } else {
            data.push(PixelData::Span {
                x,
                y,
                text: symbol,
                rows: 1,
            });
        }
    }
    Some(Pixels {
        origin: [0, 0],
        legend,
        data,
    })
}
fn draw_call(call: Option<(ToolName, &Value)>, l: usize, f: usize) -> Option<Value> {
    let (_, args) = call.filter(|(t, _)| matches!(t, ToolName::DocDraw | ToolName::DocFx))?;
    let layer = args.get("layer")?.as_u64()? as usize;
    let first = args.get("frame")?.as_u64()? as usize;
    let last = args
        .get("frame_to")
        .and_then(Value::as_u64)
        .map_or(first, |v| v as usize);
    if layer != l || !(first..=last).contains(&f) {
        return None;
    }
    let mut op = args.as_object()?.clone();
    for key in ["doc_id", "layer", "frame", "frame_to", "expected_revision"] {
        op.remove(key);
    }
    Some(Value::Object(op))
}
fn matches(asset: &Asset, layer: usize, cel: &Cel, expected: &RgbaImage) -> bool {
    let mut a = asset.clone();
    for l in &mut a.layers {
        l.cels.clear();
    }
    a.layers[layer].cels.push(cel.clone());
    compile(&a).is_ok_and(|d| {
        d.cel(layer, cel.frames[0])
            .is_some_and(|(_, _, p)| p == expected)
    })
}
pub(super) fn current(
    doc: &Document,
    dir: &Path,
    previous: Option<&Source>,
    call: Option<(ToolName, &Value)>,
) -> Result<Source, String> {
    let mut asset = skeleton(doc);
    let before = previous.map(Source::compile).transpose()?;
    for l in 0..doc.meta().layers.len() {
        for f in 0..doc.meta().frames.len() {
            let Some((x, y, pixels)) = doc.cel(l, f) else {
                continue;
            };
            let mut old = None;
            if let (Some(source), Some(before)) = (previous, before.as_ref()) {
                // Preserve a target's construction, then consider copied/moved cels.
                if let Some(layer) = source.asset.layers.get(l) {
                    for c in &layer.cels {
                        if c.frames.contains(&f) {
                            if let Some((_, _, p)) = before.cel(l, f) {
                                old = Some((c.clone(), p));
                            }
                            break;
                        }
                    }
                }
                if old.as_ref().is_none_or(|(_, p)| *p != pixels) {
                    'find: for (bl, layer) in source.asset.layers.iter().enumerate() {
                        for c in &layer.cels {
                            if let Some((_, _, p)) = before.cel(bl, c.frames[0])
                                && p == pixels
                            {
                                old = Some((c.clone(), p));
                                break 'find;
                            }
                        }
                    }
                }
            }
            let mut cel = old.as_ref().map_or_else(
                || Cel {
                    frames: vec![f],
                    ..Cel::default()
                },
                |(c, _)| c.clone(),
            );
            cel.frames = vec![f];
            cel.at = [x, y];
            cel.size = ([pixels.width(), pixels.height()] != asset.canvas)
                .then_some([pixels.width(), pixels.height()]);
            let unchanged = old.as_ref().is_some_and(|(_, p)| *p == pixels);
            if !unchanged {
                let mut represented = false;
                let pixel_only = old
                    .as_ref()
                    .is_some_and(|(c, _)| c.steps.iter().all(|s| matches!(s, Step::Pixels(_))));
                if let Some(op) = draw_call(call, l, f)
                    && !(pixel_only && op["op"] == "pencil")
                    && [x, y] == [0, 0]
                    && [pixels.width(), pixels.height()] == asset.canvas
                {
                    let step = Step::Draw {
                        op,
                        palette: asset.palette.clone(),
                    };
                    let mut whole = cel.clone();
                    whole.steps = vec![step.clone()];
                    if matches(&asset, l, &whole, pixels) {
                        cel = whole;
                        represented = true;
                    } else if old.is_some() {
                        let mut corrected = cel.clone();
                        corrected.steps.pop();
                        corrected.steps.push(step.clone());
                        if matches(&asset, l, &corrected, pixels) {
                            cel = corrected;
                            represented = true;
                        } else {
                            cel.steps.push(step);
                            represented = matches(&asset, l, &cel, pixels);
                            if !represented {
                                cel.steps.pop();
                            }
                        }
                    }
                }
                if !represented {
                    if let Some((base, old_pixels)) = &old
                        && old_pixels.dimensions() == pixels.dimensions()
                    {
                        cel.steps = base.steps.clone();
                        if cel.steps.iter().all(|s| matches!(s, Step::Pixels(_))) {
                            if let Some(Step::Pixels(first)) = cel.steps.first() {
                                cel.steps = capture_as(pixels, first)
                                    .map(|p| vec![Step::Pixels(p)])
                                    .unwrap_or_default();
                            } else {
                                cel.steps = snapshot(pixels).steps;
                            }
                        } else if let Some(p) = patch(old_pixels, pixels) {
                            if let Some(Step::Pixels(old)) = cel.steps.last_mut()
                                && let Some(merged) = combine(old, &p)
                            {
                                *old = merged;
                            } else {
                                cel.steps.push(Step::Pixels(p));
                            }
                        }
                    } else if let Some((base, _)) = &old
                        && base.steps.iter().all(|s| matches!(s, Step::Pixels(_)))
                        && let Some(Step::Pixels(first)) = base.steps.first()
                    {
                        cel.steps = capture_as(pixels, first)
                            .map(|p| vec![Step::Pixels(p)])
                            .unwrap_or_default();
                    } else {
                        cel.steps = snapshot(pixels).steps;
                    }
                }
            }
            let cels = &mut asset.layers[l].cels;
            if let Some(shared) = cels.iter_mut().find(|c| {
                let mut copy = (*c).clone();
                copy.frames = cel.frames.clone();
                copy == cel
            }) {
                shared.frames.push(f);
            } else {
                cels.push(cel);
            }
        }
    }
    let mut files = BTreeMap::new();
    if let Some(reference) = &asset.reference {
        files.insert(
            reference.clone(),
            super::load::read_file(&dir.join(reference), 256 * 1024 * 1024)?,
        );
    }
    let canonical = write::encode(&asset)?;
    let text = previous.map_or_else(
        || Ok(canonical.clone()),
        |s| write::preserve(&s.text, &canonical),
    )?;
    let parsed = parse::decode(&text)?;
    let source = Source::from_parts(text, parsed, files)?;
    equivalent(doc, &source.compile()?)?;
    Ok(source)
}
pub(crate) fn equivalent(before: &Document, after: &Document) -> Result<u64, String> {
    let mut a = serde_json::to_value(before.meta()).map_err(|e| e.to_string())?;
    let mut b = serde_json::to_value(after.meta()).map_err(|e| e.to_string())?;
    a.as_object_mut().ok_or("invalid metadata")?.remove("cels");
    b.as_object_mut().ok_or("invalid metadata")?.remove("cels");
    if a != b || before.structure() != after.structure() {
        return Err("recipe changed document structure".into());
    }
    let mut checked = 0;
    for l in 0..before.meta().layers.len() {
        for f in 0..before.meta().frames.len() {
            match (before.cel(l, f), after.cel(l, f)) {
                (None, None) => {}
                (Some((ax, ay, a)), Some((bx, by, b))) if ax == bx && ay == by && a == b => {
                    checked += a.width() as u64 * a.height() as u64
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

/// One-time import cleanup. Legacy tool-call history is not an editable
/// construction worth preserving when a single pixel block is smaller. This
/// decision happens only at migration; normal edits never switch encodings.
pub(super) fn imported(doc: &Document, previous: &Source) -> Result<Source, String> {
    let mut asset = previous.asset.clone();
    let mut bytes = write::encode(&asset)?.len();
    for l in 0..asset.layers.len() {
        for c in 0..asset.layers[l].cels.len() {
            let old = asset.layers[l].cels[c].clone();
            let pixels = doc.cel(l, old.frames[0]).ok_or("missing imported cel")?.2;
            let mut candidate = old.clone();
            candidate.steps = snapshot(pixels).steps;
            asset.layers[l].cels[c] = candidate;
            let candidate_bytes = write::encode(&asset)?.len();
            if candidate_bytes < bytes {
                bytes = candidate_bytes;
            } else {
                asset.layers[l].cels[c] = old;
            }
        }
    }
    let text = write::encode(&asset)?;
    let source = Source::from_parts(text.clone(), parse::decode(&text)?, previous.files.clone())?;
    equivalent(doc, &source.compile()?)?;
    Ok(source)
}
