use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::Path;

use atelier_core::document::Document;
use atelier_core::source::{Asset, Cel, Layer, Part, hex};
use image::RgbaImage;
use serde_json::{Value, json};
use uuid::Uuid;

use super::load::sync_tree;
use super::{Source, format, procedure};
use crate::{JournalEntry, atomic_rename::rename_no_replace};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MigrationMode {
    Auto,
    Pixels,
    Procedural,
}

pub(super) fn skeleton(doc: &Document) -> Asset {
    let m = doc.meta();
    Asset {
        format: 1,
        renderer: 1,
        name: m.name.clone(),
        canvas: [m.w, m.h],
        frames: m.frames.iter().map(|f| f.duration_ms).collect(),
        palette: m.palette.iter().map(hex).collect(),
        inks: BTreeMap::new(),
        layers: m
            .layers
            .iter()
            .enumerate()
            .map(|(n, l)| Layer {
                id: format!("{}-{n}", slug(&l.name)),
                name: l.name.clone(),
                opacity: l.opacity,
                visible: l.visible,
                blend: l.blend,
            })
            .collect(),
        tags: m.tags.clone(),
        reference: m.reference.clone(),
        parts: BTreeMap::new(),
        cels: vec![],
    }
}

pub(super) fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if s.len() >= 64 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-');
    if s.is_empty() {
        "layer".into()
    } else {
        s.into()
    }
}

fn snapshot(doc: &Document) -> Result<(Asset, BTreeMap<String, Vec<u8>>), String> {
    let mut asset = skeleton(doc);
    let mut files = BTreeMap::new();
    for cel in &doc.meta().cels {
        let (x, y, image) = doc
            .cel(cel.layer, cel.frame)
            .ok_or("missing original cel")?;
        let name = format!("{}-frame-{}", asset.layers[cel.layer].id, cel.frame);
        let (origin, crop) = crop(image);
        let mut part = Part {
            size: [image.width(), image.height()],
            origin,
            ..Part::default()
        };
        if let Some((grid, legend)) = grid(&crop) {
            part.grid = Some(grid);
            part.legend = legend;
        } else {
            let path = format!("pixels/{name}.png");
            files.insert(path.clone(), encode_png(&crop)?);
            part.image = Some(path);
        }
        asset.parts.insert(name.clone(), part);
        asset.cels.push(Cel {
            layer: asset.layers[cel.layer].id.clone(),
            frames: vec![cel.frame],
            part: name,
            at: [x, y],
        });
    }
    Ok((asset, files))
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

pub(super) fn equivalent(before: &Document, after: &Document) -> Result<u64, String> {
    let mut a = serde_json::to_value(before.meta()).map_err(|e| e.to_string())?;
    let mut b = serde_json::to_value(after.meta()).map_err(|e| e.to_string())?;
    a.as_object_mut().unwrap().remove("cels");
    b.as_object_mut().unwrap().remove("cels");
    if a != b || before.structure() != after.structure() {
        return Err("migration changed document structure".into());
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
                        "migration changed cel ({l},{f}): pixels, bounds or offset differ"
                    ));
                }
            }
        }
    }
    Ok(checked)
}

pub(super) fn publish(
    doc: &Document,
    doc_dir: &Path,
    entries: &[JournalEntry],
    destination: &Path,
    mode: MigrationMode,
) -> Result<Value, String> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(format!(
            "destination '{}' already exists",
            destination.display()
        ));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stage = parent.join(format!(".atelier-migrate-{}", Uuid::new_v4()));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let result = (|| {
        let (pixels, mut files) = snapshot(doc)?;
        if let Some(path) = &pixels.reference {
            files.insert(
                path.clone(),
                super::load::read_file(&doc_dir.join(path), 256 * 1024 * 1024)?,
            );
        }
        for (name, bytes) in &files {
            let path = stage.join(name);
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::write(path, bytes).map_err(|e| e.to_string())?;
        }
        let mut selected = pixels;
        let mut representation = "pixels";
        let mut note = None;
        if mode != MigrationMode::Pixels {
            match procedure::recover(doc, entries) {
                Ok(program) => {
                    let program_text = format::encode(&program)?;
                    let procedural = Source::from_text(program_text.clone(), &stage)?;
                    match procedural
                        .compile()
                        .and_then(|after| equivalent(doc, &after))
                    {
                        Ok(_) => {
                            if mode == MigrationMode::Procedural {
                                selected = program;
                                representation = "procedural";
                            } else {
                                (selected, representation) = mix(program, &selected, &files)?;
                            }
                        }
                        Err(error) if mode == MigrationMode::Procedural => return Err(error),
                        Err(error) => {
                            note = Some(format!("procedural candidate rejected: {error}"))
                        }
                    }
                }
                Err(error) if mode == MigrationMode::Procedural => return Err(error),
                Err(_) => {}
            }
        }
        let text = format::encode(&selected)?;
        fs::write(stage.join("source.toml"), &text).map_err(|e| e.to_string())?;
        let rebuilt = Source::load(&stage)?;
        let checked = equivalent(doc, &rebuilt.compile()?)?;
        // Remove unused raster candidates; only authored dependencies are shipped.
        let used: BTreeSet<_> = rebuilt.files.keys().collect();
        for name in files.keys() {
            if !used.contains(name) {
                fs::remove_file(stage.join(name)).map_err(|e| e.to_string())?;
            }
        }
        if stage.join("pixels").is_dir()
            && fs::read_dir(stage.join("pixels"))
                .map_err(|e| e.to_string())?
                .next()
                .is_none()
        {
            fs::remove_dir(stage.join("pixels")).map_err(|e| e.to_string())?;
        }
        sync_tree(&stage)?;
        rename_no_replace(&stage, destination)
            .map_err(|e| format!("cannot publish source: {e}"))?;
        let mut report = json!({"ok": true, "source": destination.join("source.toml"), "revision": rebuilt.revision,
            "representation": representation, "manifest_bytes": text.len(), "resource_bytes": rebuilt.files.values().map(Vec::len).sum::<usize>(),
            "parts": selected.parts.len(), "cels": doc.meta().cels.len(), "rgba_pixels_verified": checked,
            "metadata_equal": true, "pixels_equal": true});
        if let Some(note) = note {
            report["note"] = json!(note);
        }
        if let Err(e) = File::open(parent).and_then(|f| f.sync_all()) {
            report["warning"] = json!(format!("source published; directory sync failed: {e}"));
        }
        Ok(report)
    })();
    let _ = fs::remove_dir_all(stage);
    result
}

/// Choose independently at each final cel boundary. A gradient can retain its
/// procedure while a neighbouring hand-painted layer uses pixels. Recovery
/// does not link independent cels, so their construction subgraphs are disjoint.
fn mix(
    mut program: Asset,
    pixels: &Asset,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(Asset, &'static str), String> {
    let mut baked = 0;
    for pixel in &pixels.cels {
        let n = program
            .cels
            .iter()
            .position(|c| c.layer == pixel.layer && c.frames == pixel.frames)
            .ok_or("missing procedural cel")?;
        let mut construction = program.clone();
        construction.cels = vec![program.cels[n].clone()];
        procedure::prune(&mut construction);
        let mut raster = pixels.clone();
        raster.cels = vec![pixel.clone()];
        procedure::prune(&mut raster);
        let resource_bytes = raster
            .parts
            .values()
            .filter_map(|p| p.image.as_ref())
            .map(|p| files[p].len())
            .sum::<usize>();
        if format::encode(&construction)?.len() >= format::encode(&raster)?.len() + resource_bytes {
            let name = format!("pixels/{}", pixel.part);
            program
                .parts
                .insert(name.clone(), pixels.parts[&pixel.part].clone());
            program.cels[n] = Cel {
                part: name,
                ..pixel.clone()
            };
            baked += 1;
        }
    }
    procedure::prune(&mut program);
    let representation = if baked == pixels.cels.len() {
        "pixels"
    } else if baked == 0 {
        "procedural"
    } else {
        "mixed"
    };
    Ok((program, representation))
}
