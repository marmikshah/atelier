//! Recover named per-layer construction graphs for the exact, local subset of
//! history whose dependencies can be proved. Unsupported edits use pixels;
//! every candidate is also compared against an actual legacy replay.

use super::migrate::{skeleton, slug};
use crate::{JournalEntry, ToolName};
use atelier_core::document::{Document, color_array};
use atelier_core::source::{Asset, Cel, Instance, Part, Placement, hex};

pub(super) fn recover(doc: &Document, entries: &[JournalEntry]) -> Result<Asset, String> {
    if doc.meta().frames.len() != 1
        || entries
            .first()
            .is_none_or(|e| e.args.contains_key("source"))
    {
        return Err("procedure recovery requires a single-frame legacy journal".into());
    }
    let mut asset = skeleton(doc);
    let mut structure = Document::new(&doc.meta().name, doc.meta().w, doc.meta().h);
    let mut roots: Vec<Option<String>> = vec![None];
    let mut serial = 0;
    for entry in entries.iter().skip(1) {
        let a = &entry.args;
        let op = a.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let number = |key: &str| -> Result<usize, String> {
            a.get(key)
                .and_then(|v| v.as_u64())
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("procedure needs {key}"))
        };
        match entry.tool {
            ToolName::DocPalette if op == "set" => {
                let colors = a
                    .get("colors")
                    .and_then(|v| v.as_array())
                    .ok_or("procedure palette missing colors")?;
                structure.set_palette(
                    colors
                        .iter()
                        .map(color_array)
                        .collect::<Option<Vec<_>>>()
                        .ok_or("invalid procedure palette")?,
                )?;
            }
            ToolName::DocDraw | ToolName::DocFx => {
                let layer = number("layer")?;
                if number("frame")? != 0 || a.contains_key("frame_to") || layer >= roots.len() {
                    return Err("unsupported ranged procedure".into());
                }
                let mut params = a.clone();
                for key in ["doc_id", "layer", "frame", "expected_revision"] {
                    params.remove(key);
                }
                if op == "clear_cel" && params.len() == 1 {
                    roots[layer] = None;
                    continue;
                }
                let palette: Vec<_> = structure.meta().palette.iter().map(hex).collect();
                let reusable = roots[layer].as_ref().is_some_and(|name| {
                    asset.parts[name].draw.is_some()
                        && asset.parts[name].palette.as_ref() == Some(&palette)
                });
                if !reusable {
                    serial += 1;
                    let name = format!("{}-{serial}", slug(&structure.meta().layers[layer].name));
                    let part = Part {
                        size: asset.canvas,
                        draw: Some(vec![]),
                        base: roots[layer].take(),
                        palette: Some(palette),
                        ..Part::default()
                    };
                    asset.parts.insert(name.clone(), part);
                    roots[layer] = Some(name);
                }
                asset
                    .parts
                    .get_mut(roots[layer].as_ref().unwrap())
                    .unwrap()
                    .draw
                    .as_mut()
                    .unwrap()
                    .push(params.into());
            }
            ToolName::DocLayer => {
                let name = a.get("name").and_then(|v| v.as_str()).map(str::to_owned);
                let opacity = a.get("opacity").and_then(|v| v.as_u64()).map(|v| v as u8);
                let blend = a
                    .get("blend")
                    .and_then(|v| v.as_str())
                    .map(str::parse)
                    .transpose()?;
                let visible = a.get("visible").and_then(|v| v.as_bool());
                if op == "add" {
                    structure.add_layer(name, opacity.unwrap_or(255), blend.unwrap_or_default());
                    roots.push(None);
                    continue;
                }
                let index = number("index")?;
                if index >= roots.len() {
                    return Err("procedure layer out of range".into());
                }
                match op {
                    "rename" => structure.rename_layer(index, name.ok_or("missing layer name")?)?,
                    "set" => structure.set_layer(index, visible, opacity, blend)?,
                    "move" => {
                        let to = number("to_index")?;
                        structure.move_layer(index, to)?;
                        let part = roots.remove(index);
                        roots.insert(to, part);
                    }
                    "delete" => {
                        structure.delete_layer(index)?;
                        roots.remove(index);
                    }
                    "merge_down" if index > 0 => {
                        if let Some(upper) = roots[index].clone() {
                            let mut items = vec![];
                            if let Some(lower) = &roots[index - 1] {
                                items.push(Instance::new(lower));
                            }
                            let mut top = Instance::new(upper);
                            top.mode = Placement::Over;
                            top.opacity = structure.meta().layers[index].opacity;
                            top.blend = structure.meta().layers[index].blend;
                            items.push(top);
                            serial += 1;
                            let name = format!(
                                "{}-merge-{serial}",
                                slug(&structure.meta().layers[index - 1].name)
                            );
                            asset.parts.insert(
                                name.clone(),
                                Part {
                                    size: asset.canvas,
                                    items: Some(items),
                                    ..Part::default()
                                },
                            );
                            roots[index - 1] = Some(name);
                        }
                        structure.merge_down(index)?;
                        roots.remove(index);
                    }
                    _ => return Err(format!("procedure recovery does not handle layer {op}")),
                }
            }
            _ => return Err(format!("procedure recovery does not handle {}", entry.tool)),
        }
    }
    if roots.len() != asset.layers.len() {
        return Err("procedure layer count changed".into());
    }
    for (layer, part) in roots.into_iter().enumerate() {
        if let Some(part) = part {
            asset.cels.push(Cel {
                layer: asset.layers[layer].id.clone(),
                frames: vec![0],
                part,
                at: [0, 0],
            });
        }
    }
    prune(&mut asset);
    asset.validate()?;
    Ok(asset)
}

pub(super) fn prune(asset: &mut Asset) {
    let mut used = std::collections::BTreeSet::new();
    let mut pending = asset
        .cels
        .iter()
        .map(|c| c.part.clone())
        .collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        if used.insert(name.clone()) {
            pending.extend(
                asset.parts[&name]
                    .dependencies()
                    .into_iter()
                    .map(str::to_owned),
            );
        }
    }
    asset.parts.retain(|name, _| used.contains(name));
}
