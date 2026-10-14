//! Turning a completed call into the statements its recipe records, and
//! applying a recorded effect back to a document.
//!
//! A document-level call is recorded as itself. A call on a cel is recorded
//! as what it left behind: the pixels inside the rectangle it changed, read
//! from the document after the call, under a scope that names the layer. The
//! three `doc_fx` operations that only move pixels are recorded as cel calls.

use image::RgbaImage;
use serde_json::{Map, Value, json};

use atelier_core::document::Document;

use crate::journal::{Change, LayerRef, Pixel, Scope, Statement, hex_colour, is_exact_fx};
use crate::{Studio, ToolName};

/// Keys a cel call leaves to its scope.
const SCOPE_KEYS: [&str; 5] = ["doc_id", "layer", "frame", "frame_to", "op"];

impl Studio {
    /// The statements that record a successful `tool` call made with `args`,
    /// given the handler's `result`. For a call on a cel the document is read
    /// to capture the pixels; it must be the document the call changed.
    pub fn statements_for_call(
        &self,
        id: &str,
        tool: ToolName,
        args: &Map<String, Value>,
        result: &Value,
    ) -> Result<Vec<Statement>, String> {
        if !crate::journal::is_pixel_tool(tool) {
            return Ok(vec![Statement::call(tool, args.clone())]);
        }
        let layer = index_arg(args, "layer")?.ok_or_else(|| format!("{tool} names no layer"))?;
        let frame = index_arg(args, "frame")?.ok_or_else(|| format!("{tool} names no frame"))?;
        let frame_to = index_arg(args, "frame_to")?;
        let (_dir, doc) = self.open(id)?;
        let name = layer_ref(&doc, layer)?;
        let op = args.get("op").and_then(Value::as_str);
        if tool == ToolName::DocFx && op.is_some_and(is_exact_fx) {
            let op = op.unwrap_or_default().to_string();
            let mut rest = args.clone();
            for key in SCOPE_KEYS {
                rest.remove(key);
            }
            return Ok((frame..=frame_to.unwrap_or(frame))
                .map(|frame| Statement::CelCall {
                    scope: Scope {
                        layer: name.clone(),
                        frame,
                    },
                    op: op.clone(),
                    args: rest.clone(),
                })
                .collect());
        }
        let annotation = annotation(tool, args);
        let boxes = changed_boxes(result, frame, frame_to)?;
        let mut statements = Vec::new();
        for frame in frame..=frame_to.unwrap_or(frame) {
            let change = match boxes.iter().find(|(at, _)| *at == frame) {
                Some((_, [x0, y0, x1, y1])) => {
                    let cel = doc.cel_full(layer, frame);
                    change_in(&cel, *x0, *y0, *x1, *y1)?
                }
                None => Change::None,
            };
            statements.push(Statement::Effect {
                scope: Scope {
                    layer: name.clone(),
                    frame,
                },
                annotation: annotation.clone(),
                change,
            });
        }
        Ok(statements)
    }

    /// The index of the layer a scope names, in `id` as it stands now.
    pub fn layer_index(&self, id: &str, layer: &LayerRef) -> Result<usize, String> {
        let (_dir, doc) = self.open(id)?;
        resolve_layer(&doc, layer)
    }

    /// Apply a recorded effect's pixels to `id`: paste the block, or fill the
    /// rectangle, exactly as recorded.
    pub fn apply_change(&self, id: &str, scope: &Scope, change: &Change) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let layer = resolve_layer(&doc, &scope.layer)?;
        let pixels = match change {
            Change::None => 0,
            Change::Solid {
                x0,
                y0,
                x1,
                y1,
                color,
            } => doc.fill_rect(layer, scope.frame, *x0, *y0, *x1, *y1, *color)?,
            Change::Rows { x, y, rows } => doc.paste(layer, scope.frame, *x, *y, rows)?,
        };
        doc.save(&dir)?;
        Ok(
            json!({"ok": true, "doc_id": id, "layer": layer, "frame": scope.frame, "pixels": pixels}),
        )
    }
}

fn index_arg(args: &Map<String, Value>, key: &str) -> Result<Option<usize>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be an index, got {value}")),
    }
}

/// How a layer is named in a scope: by name while that name is unique, by
/// index otherwise.
fn layer_ref(doc: &Document, layer: usize) -> Result<LayerRef, String> {
    let layers = &doc.meta().layers;
    let name = &layers
        .get(layer)
        .ok_or_else(|| format!("no layer {layer}"))?
        .name;
    if layers.iter().filter(|meta| meta.name == *name).count() == 1 {
        Ok(LayerRef::Name(name.clone()))
    } else {
        Ok(LayerRef::Index(layer))
    }
}

fn resolve_layer(doc: &Document, layer: &LayerRef) -> Result<usize, String> {
    let layers = &doc.meta().layers;
    match layer {
        LayerRef::Index(index) if *index < layers.len() => Ok(*index),
        LayerRef::Index(index) => Err(format!("no layer {index}")),
        LayerRef::Name(name) => {
            let mut matches = layers
                .iter()
                .enumerate()
                .filter(|(_, meta)| meta.name == *name);
            match (matches.next(), matches.next()) {
                (Some((index, _)), None) => Ok(index),
                (None, _) => Err(format!("no layer named {name:?}")),
                (Some(_), Some(_)) => Err(format!("more than one layer is named {name:?}")),
            }
        }
    }
}

/// The inclusive boxes a call changed, per frame, from its result: a ranged
/// call reports `frame_changes`, a single-frame call `change_bbox`.
fn changed_boxes(
    result: &Value,
    frame: usize,
    frame_to: Option<usize>,
) -> Result<Vec<(usize, [u32; 4])>, String> {
    let corners = |value: &Value| -> Option<[u32; 4]> {
        let edge = |index: usize| {
            value
                .get(index)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        Some([edge(0)?, edge(1)?, edge(2)?, edge(3)?])
    };
    if frame_to.is_some() {
        let changes = result
            .get("frame_changes")
            .and_then(Value::as_array)
            .ok_or("a ranged cel call must report frame_changes")?;
        return changes
            .iter()
            .map(|entry| {
                let at = entry
                    .get(0)
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok());
                let corners = entry.get(1).and_then(corners);
                match (at, corners) {
                    (Some(at), Some(corners)) => Ok((at, corners)),
                    _ => Err(format!("malformed frame change {entry}")),
                }
            })
            .collect();
    }
    match result.get("change_bbox") {
        None => Err("a cel call must report change_bbox".into()),
        Some(Value::Null) => Ok(Vec::new()),
        Some(value) => corners(value)
            .map(|corners| vec![(frame, corners)])
            .ok_or_else(|| format!("malformed change_bbox {value}")),
    }
}

/// The pixels of `cel` inside an inclusive box, as the recorded change.
fn change_in(cel: &RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32) -> Result<Change, String> {
    if x1 < x0 || y1 < y0 || x1 >= cel.width() || y1 >= cel.height() {
        return Err(format!(
            "change box {x0},{y0} {x1},{y1} is outside the {}x{} canvas",
            cel.width(),
            cel.height()
        ));
    }
    let rows: Vec<Vec<Pixel>> = (y0..=y1)
        .map(|y| (x0..=x1).map(|x| cel.get_pixel(x, y).0).collect())
        .collect();
    let first = rows[0][0];
    if rows.iter().flatten().all(|pixel| *pixel == first) {
        return Ok(Change::Solid {
            x0,
            y0,
            x1,
            y1,
            color: first,
        });
    }
    Ok(Change::Rows { x: x0, y: y0, rows })
}

/// The call in words, for the person reading the recipe: the verb, its
/// colour, up to two points of geometry, then its modifiers.
fn annotation(tool: ToolName, args: &Map<String, Value>) -> String {
    let mut words: Vec<String> = Vec::new();
    let erase = args.get("erase").and_then(Value::as_bool).unwrap_or(false);
    if erase {
        words.push("erase".into());
    }
    let op = args.get("op").and_then(Value::as_str);
    match tool {
        ToolName::DocPaintGrid => words.push("grid".into()),
        ToolName::DocDitherRamp => words.push("dither_ramp".into()),
        ToolName::DocRegion => words.push(op.unwrap_or("region").into()),
        _ => words.push(op.unwrap_or("?").into()),
    }
    if !erase
        && let Some(colour) = args
            .get("color")
            .and_then(atelier_core::document::color_array)
    {
        words.push(hex_colour(colour));
    }
    let number = |key: &str| args.get(key).and_then(Value::as_i64);
    let point = |x: &str, y: &str| Some(format!("{},{}", number(x)?, number(y)?));
    for word in [
        point("x0", "y0"),
        point("x1", "y1"),
        point("cx", "cy"),
        point("rx", "ry"),
        point("x", "y"),
    ]
    .into_iter()
    .flatten()
    {
        words.push(word);
    }
    if tool == ToolName::DocRegion {
        if let Some(rect) = args.get("rect").and_then(Value::as_array)
            && let [x0, y0, x1, y1] = rect.as_slice()
        {
            words.push(format!("{x0},{y0}"));
            words.push(format!("{x1},{y1}"));
        }
        if let Some(offset) = args.get("offset").and_then(Value::as_array)
            && let [dx, dy] = offset.as_slice()
        {
            words.push(format!("{dx},{dy}"));
        }
    }
    for flag in ["fill", "closed", "aa", "replace", "wrap"] {
        if args.get(flag).and_then(Value::as_bool) == Some(true) {
            words.push(flag.into());
        }
    }
    for (key, word) in [
        ("size", "size"),
        ("width", "width"),
        ("opacity", "opacity"),
        ("blend_mode", "blend"),
    ] {
        if let Some(value) = args.get(key).filter(|v| v.is_number() || v.is_string()) {
            words.push(word.into());
            words.push(
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
    }
    words
        .into_iter()
        .filter(|word| !word.contains(['=', '~']) && !word.chars().any(char::is_whitespace))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::TRANSPARENT;
    use std::fs;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-record-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn calls_on_cels_are_recorded_as_what_they_left_behind() {
        let studio = studio("effects");
        let id = studio.doc_new("d", 4, 3).unwrap()["doc_id"]
            .as_str()
            .unwrap()
            .to_string();
        studio
            .doc_layer(
                &id,
                crate::LayerOp::Rename,
                Some(0),
                None,
                Some("ink".into()),
                None,
                None,
                None,
            )
            .unwrap();
        let args = object(json!({"doc_id": id, "layer": 0, "frame": 0, "op": "line",
            "x0": 0, "y0": 0, "x1": 2, "y1": 0, "color": "#102030", "size": 1}));
        let params =
            object(json!({"x0": 0, "y0": 0, "x1": 2, "y1": 0, "color": "#102030", "size": 1}));
        let result = studio.doc_draw(&id, 0, 0, None, "line", params).unwrap();
        let recorded = studio
            .statements_for_call(&id, ToolName::DocDraw, &args, &result)
            .unwrap();
        assert_eq!(
            recorded,
            vec![Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("ink".into()),
                    frame: 0
                },
                annotation: "line #102030 0,0 2,0 size 1".into(),
                change: Change::Solid {
                    x0: 0,
                    y0: 0,
                    x1: 2,
                    y1: 0,
                    color: [0x10, 0x20, 0x30, 255]
                }
            }]
        );

        // A call that changes nothing is recorded as a no-op, and an erase
        // drops its colour from the annotation.
        let args = object(json!({"doc_id": id, "layer": 0, "frame": 0, "op": "pencil",
            "points": [[3, 2]], "color": "#102030", "erase": true}));
        let params = object(json!({"points": [[3, 2]], "color": "#102030", "erase": true}));
        let result = studio.doc_draw(&id, 0, 0, None, "pencil", params).unwrap();
        let recorded = studio
            .statements_for_call(&id, ToolName::DocDraw, &args, &result)
            .unwrap();
        assert_eq!(
            recorded,
            vec![Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("ink".into()),
                    frame: 0
                },
                annotation: "erase pencil".into(),
                change: Change::None
            }]
        );

        // A grid over the line: rows with the pixels as they stand after.
        let args = object(json!({"doc_id": id, "layer": 0, "frame": 0, "x": 1, "y": 0,
            "legend": {"K": "#000000"}, "rows": ["K.", ".K"], "replace": true}));
        let result = studio
            .doc_paint_grid(
                &id,
                0,
                0,
                None,
                1,
                0,
                object(json!({"K": "#000000"})),
                vec!["K.".into(), ".K".into()],
                true,
            )
            .unwrap();
        let recorded = studio
            .statements_for_call(&id, ToolName::DocPaintGrid, &args, &result)
            .unwrap();
        let black = [0, 0, 0, 255];
        assert_eq!(
            recorded,
            vec![Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("ink".into()),
                    frame: 0
                },
                annotation: "grid 1,0 replace".into(),
                change: Change::Rows {
                    x: 1,
                    y: 0,
                    rows: vec![vec![black, TRANSPARENT], vec![TRANSPARENT, black]]
                }
            }]
        );

        // Exact fx are cel calls, one per frame of a range.
        studio
            .doc_frame(&id, crate::FrameOp::Add, None, Some(0), None, None, Some(1))
            .unwrap();
        let args = object(json!({"doc_id": id, "layer": 0, "frame": 0, "frame_to": 1,
            "op": "shift", "dx": 1, "wrap": true}));
        let result = studio
            .doc_fx(
                &id,
                0,
                0,
                Some(1),
                "shift",
                object(json!({"dx": 1, "wrap": true})),
            )
            .unwrap();
        let recorded = studio
            .statements_for_call(&id, ToolName::DocFx, &args, &result)
            .unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(
            recorded[1],
            Statement::CelCall {
                scope: Scope {
                    layer: LayerRef::Name("ink".into()),
                    frame: 1
                },
                op: "shift".into(),
                args: object(json!({"dx": 1, "wrap": true}))
            }
        );

        // A ranged pixel call records one effect per frame it changed.
        let args = object(json!({"doc_id": id, "layer": 0, "frame": 0, "frame_to": 1,
            "op": "rect", "x0": 0, "y0": 2, "x1": 0, "y1": 2, "color": [1, 2, 3], "fill": true}));
        let params =
            object(json!({"x0": 0, "y0": 2, "x1": 0, "y1": 2, "color": [1, 2, 3], "fill": true}));
        let result = studio.doc_draw(&id, 0, 0, Some(1), "rect", params).unwrap();
        let recorded = studio
            .statements_for_call(&id, ToolName::DocDraw, &args, &result)
            .unwrap();
        assert_eq!(recorded.len(), 2);
        assert!(matches!(
            &recorded[0],
            Statement::Effect { annotation, change: Change::Solid { x0: 0, y0: 2, x1: 0, y1: 2, color: [1, 2, 3, 255] }, .. }
                if annotation == "rect #010203 0,2 0,2 fill"
        ));

        // A document-level call is recorded as itself.
        let args = object(json!({"doc_id": id, "op": "add", "name": "ink"}));
        let recorded = studio
            .statements_for_call(&id, ToolName::DocLayer, &args, &json!({}))
            .unwrap();
        assert_eq!(recorded, vec![Statement::call(ToolName::DocLayer, args)]);
    }

    #[test]
    fn shared_layer_names_fall_back_to_the_index_and_effects_apply_back() {
        let studio = studio("apply");
        let id = studio.doc_new("d", 3, 2).unwrap()["doc_id"]
            .as_str()
            .unwrap()
            .to_string();
        studio
            .doc_layer(
                &id,
                crate::LayerOp::Add,
                None,
                None,
                Some("Layer 1".into()),
                None,
                None,
                None,
            )
            .unwrap();
        let (_, doc) = studio.open(&id).unwrap();
        assert_eq!(layer_ref(&doc, 1).unwrap(), LayerRef::Index(1));
        assert!(
            studio
                .layer_index(&id, &LayerRef::Name("Layer 1".into()))
                .is_err()
        );
        assert_eq!(studio.layer_index(&id, &LayerRef::Index(1)).unwrap(), 1);
        studio
            .doc_layer(
                &id,
                crate::LayerOp::Rename,
                Some(1),
                None,
                Some("ink".into()),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            studio
                .layer_index(&id, &LayerRef::Name("ink".into()))
                .unwrap(),
            1
        );

        let scope = Scope {
            layer: LayerRef::Name("ink".into()),
            frame: 0,
        };
        let red = [255, 0, 0, 255];
        studio
            .apply_change(
                &id,
                &scope,
                &Change::Rows {
                    x: 1,
                    y: 0,
                    rows: vec![vec![red, TRANSPARENT], vec![TRANSPARENT, red]],
                },
            )
            .unwrap();
        studio
            .apply_change(
                &id,
                &scope,
                &Change::Solid {
                    x0: 0,
                    y0: 1,
                    x1: 0,
                    y1: 1,
                    color: [0, 0, 255, 128],
                },
            )
            .unwrap();
        let (_, doc) = studio.open(&id).unwrap();
        let cel = doc.cel_full(1, 0);
        assert_eq!(cel.get_pixel(1, 0).0, red);
        assert_eq!(cel.get_pixel(2, 0).0, TRANSPARENT);
        assert_eq!(cel.get_pixel(2, 1).0, red);
        assert_eq!(
            cel.get_pixel(0, 1).0,
            [0, 0, 255, 128],
            "pasted, not composited"
        );
        assert!(
            studio
                .apply_change(
                    &id,
                    &scope,
                    &Change::Solid {
                        x0: 0,
                        y0: 0,
                        x1: 3,
                        y1: 0,
                        color: red
                    }
                )
                .is_err(),
            "a block outside the canvas is refused"
        );
    }
}
