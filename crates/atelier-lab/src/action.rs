//! The learned action language (lab.md Phase 3): a compact DSL the policy
//! emits, plus the compiler that turns it into atelier tool calls.
//!
//! The DSL deliberately mirrors the editor's own concepts (patch, region,
//! palette, layer) and excludes select/clipboard ops — those are per-`Studio`
//! shared state, which per-episode isolation forbids. The compiler is a pure
//! function of the action and a document snapshot, so validation is unit-
//! testable without a studio on disk.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Hard cap on one action's footprint (lab.md item 16, "enforce patch-size
/// limits"): 1024 px is one full 32×32 canvas — the largest sprite the
/// research scope allows — so anything bigger is a decoding bug, not art.
pub const MAX_PATCH_PIXELS: u32 = 1024;

/// Legend charset for compiled `doc_paint_grid` calls — the same 62 glyphs
/// `doc_dump_region` uses, minus the reserved '.'/' ' (leave-untouched).
const GRID_GLYPHS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// The artistic pipeline stage (lab.md item 15). Stages are ordered; the env
/// records the current one on every transition and the compiler gates
/// actions on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Stage {
    Specification,
    Silhouette,
    ColorBlocking,
    Lighting,
    Detail,
    Cleanup,
    Finished,
}

impl Stage {
    /// The next stage forward, or None at Finished.
    pub fn next(self) -> Option<Stage> {
        use Stage::*;
        Some(match self {
            Specification => Silhouette,
            Silhouette => ColorBlocking,
            ColorBlocking => Lighting,
            Lighting => Detail,
            Detail => Cleanup,
            Cleanup => Finished,
            Finished => return None,
        })
    }
}

/// The edit itself — one of ~10 verbs (lab.md item 13).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ActionKind {
    /// The primary raster action (lab.md item 14): paint a `width`×`height`
    /// patch of palette indices onto `layer` at (`x`, `y`). `grid` is
    /// row-major, exactly `width * height` entries; erasing is ClearRegion's
    /// job, so every cell is a real swatch index.
    PaintPatch {
        layer: usize,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        grid: Vec<u32>,
    },
    ClearRegion {
        layer: usize,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    MoveRegion {
        layer: usize,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        dx: i32,
        dy: i32,
    },
    /// Mirror a region in place; `horizontal` flips left-right, otherwise
    /// top-bottom.
    MirrorRegion {
        layer: usize,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        horizontal: bool,
    },
    /// Recolour swatch `from` to swatch `to` everywhere (palette indices).
    ReplaceColor {
        from: u32,
        to: u32,
    },
    /// Lock the palette. Gated to the early stages — see `compile`.
    SetPalette {
        colors: Vec<[u8; 4]>,
    },
    AddLayer {
        name: Option<String>,
    },
    /// Merge `index` down onto the layer below.
    MergeLayer {
        index: usize,
    },
    /// Move one stage forward (never backward, never past Finished).
    AdvanceStage,
    /// Declare the artwork done; moves the stage to Finished.
    Finish,
}

impl ActionKind {
    fn name(&self) -> &'static str {
        match self {
            ActionKind::PaintPatch { .. } => "paint_patch",
            ActionKind::ClearRegion { .. } => "clear_region",
            ActionKind::MoveRegion { .. } => "move_region",
            ActionKind::MirrorRegion { .. } => "mirror_region",
            ActionKind::ReplaceColor { .. } => "replace_color",
            ActionKind::SetPalette { .. } => "set_palette",
            ActionKind::AddLayer { .. } => "add_layer",
            ActionKind::MergeLayer { .. } => "merge_layer",
            ActionKind::AdvanceStage => "advance_stage",
            ActionKind::Finish => "finish",
        }
    }
}

/// A model-proposed action: the edit plus WHY (lab.md item 17). The metadata
/// never affects compilation — it rides along into the recorded transition so
/// edit evaluation and training can pair intent with outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub action: ActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Inclusive corners `[x0, y0, x1, y1]`, matching atelier's region
    /// convention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_region: Option<[i32; 4]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserve: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_effect: Option<String>,
}

impl Action {
    /// An action with no effect metadata — the common case in tests and
    /// hand-written episodes.
    pub fn new(action: ActionKind) -> Action {
        Action {
            action,
            intent: None,
            target_region: None,
            preserve: Vec::new(),
            expected_effect: None,
        }
    }

    /// One-line summary for the light observation's recent-actions list.
    pub fn summarize(&self) -> String {
        match &self.action {
            ActionKind::PaintPatch {
                layer,
                x,
                y,
                width,
                height,
                ..
            } => format!("paint_patch l{layer} {width}x{height}@({x},{y})"),
            ActionKind::ClearRegion {
                layer,
                x,
                y,
                width,
                height,
            } => format!("clear_region l{layer} {width}x{height}@({x},{y})"),
            ActionKind::MoveRegion {
                layer,
                x,
                y,
                width,
                height,
                dx,
                dy,
            } => format!("move_region l{layer} {width}x{height}@({x},{y}) by ({dx},{dy})"),
            ActionKind::MirrorRegion {
                layer,
                x,
                y,
                width,
                height,
                horizontal,
            } => {
                let axis = if *horizontal { "h" } else { "v" };
                format!("mirror_region l{layer} {width}x{height}@({x},{y}) {axis}")
            }
            ActionKind::ReplaceColor { from, to } => format!("replace_color {from}→{to}"),
            ActionKind::SetPalette { colors } => format!("set_palette {} colours", colors.len()),
            ActionKind::AddLayer { name } => match name {
                Some(n) => format!("add_layer '{n}'"),
                None => "add_layer".into(),
            },
            ActionKind::MergeLayer { index } => format!("merge_layer {index}"),
            ActionKind::AdvanceStage => "advance_stage".into(),
            ActionKind::Finish => "finish".into(),
        }
    }
}

/// What the compiler validates against: document identity and geometry, the
/// locked palette, the current stage, and each layer's frame-0 raster (some
/// actions derive pixels — MirrorRegion reads what it mirrors, and the no-op
/// guards compare against what's already there).
#[derive(Clone, Debug)]
pub struct DocSnapshot {
    pub doc_id: String,
    pub width: u32,
    pub height: u32,
    pub palette: Vec<[u8; 4]>,
    /// Task-level palette budget. Kept in the snapshot so validation stays a
    /// pure compiler concern instead of accepting an invalid action and only
    /// noticing it in the next observation.
    pub max_colors: usize,
    pub stage: Stage,
    /// Per-layer indexed rasters of frame 0 (row-major, None = transparent).
    pub layers: Vec<Vec<Option<u32>>>,
}

impl DocSnapshot {
    fn cell(&self, layer: usize, x: i32, y: i32) -> Option<u32> {
        self.layers[layer][(y as u32 * self.width + x as u32) as usize]
    }
}

/// One atelier tool call the compiler emitted: tool name + args JSON, ready
/// for `Atelier::dispatch`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledCall {
    pub tool: String,
    pub args: Value,
}

/// Structured compile-time rejection (lab.md item 16). Recorded verbatim in
/// the transition when an action is rejected — the invalid-action rate is a
/// tracked metric, so these must be machine-readable, not just strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompileError {
    OutOfBounds {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        canvas_width: u32,
        canvas_height: u32,
    },
    BadPaletteIndex {
        index: u32,
        palette_len: usize,
    },
    UnknownLayer {
        layer: usize,
        layers: usize,
    },
    PatchTooLarge {
        pixels: u64,
        max: u32,
    },
    GridSizeMismatch {
        expected: usize,
        got: usize,
    },
    TooManyColors {
        count: usize,
        max: usize,
    },
    /// The action would change nothing (empty region, no-op paint, …).
    EmptyModification {
        reason: String,
    },
    /// The action is not allowed in the current stage.
    StageViolation {
        action: String,
        stage: Stage,
        reason: String,
    },
    /// The target is structurally invalid (e.g. merging layer 0 down).
    InvalidTarget {
        reason: String,
    },
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::OutOfBounds {
                x,
                y,
                width,
                height,
                canvas_width,
                canvas_height,
            } => write!(
                f,
                "region {width}x{height}@({x},{y}) is outside the {canvas_width}x{canvas_height} canvas"
            ),
            CompileError::BadPaletteIndex { index, palette_len } => {
                write!(f, "palette index {index} out of range (palette has {palette_len})")
            }
            CompileError::UnknownLayer { layer, layers } => {
                write!(f, "no layer {layer} (document has {layers})")
            }
            CompileError::PatchTooLarge { pixels, max } => {
                write!(f, "patch is {pixels} px, over the {max}-px limit")
            }
            CompileError::GridSizeMismatch { expected, got } => {
                write!(f, "grid has {got} cells, expected {expected} (width*height)")
            }
            CompileError::TooManyColors { count, max } => {
                write!(f, "patch uses {count} distinct colours, over the {max}-glyph grid charset")
            }
            CompileError::EmptyModification { reason } => write!(f, "empty modification: {reason}"),
            CompileError::StageViolation {
                action,
                stage,
                reason,
            } => write!(f, "{action} is not allowed in stage {stage:?}: {reason}"),
            CompileError::InvalidTarget { reason } => write!(f, "invalid target: {reason}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Raster edits change pixels, so they belong to the making stages: not in
/// Specification (palette and layers first), not after Finish.
fn require_editable(action: &str, stage: Stage) -> Result<(), CompileError> {
    let reason = match stage {
        Stage::Specification => {
            "raster edits start at Silhouette — set the palette and AdvanceStage first"
        }
        Stage::Finished => "the episode is finished",
        _ => return Ok(()),
    };
    Err(CompileError::StageViolation {
        action: action.into(),
        stage,
        reason: reason.into(),
    })
}

/// Bounds + non-empty + patch-size gate shared by every region action.
/// Coordinates are computed in i64: `x + width` must reject, never wrap.
fn check_region(
    doc: &DocSnapshot,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), CompileError> {
    if width == 0 || height == 0 {
        return Err(CompileError::EmptyModification {
            reason: "region has zero width or height".into(),
        });
    }
    let pixels = width as u64 * height as u64;
    if pixels > MAX_PATCH_PIXELS as u64 {
        return Err(CompileError::PatchTooLarge {
            pixels,
            max: MAX_PATCH_PIXELS,
        });
    }
    let out = x < 0
        || y < 0
        || x as i64 + width as i64 > doc.width as i64
        || y as i64 + height as i64 > doc.height as i64;
    if out {
        return Err(CompileError::OutOfBounds {
            x,
            y,
            width,
            height,
            canvas_width: doc.width,
            canvas_height: doc.height,
        });
    }
    Ok(())
}

fn check_layer(doc: &DocSnapshot, layer: usize) -> Result<(), CompileError> {
    if layer >= doc.layers.len() {
        return Err(CompileError::UnknownLayer {
            layer,
            layers: doc.layers.len(),
        });
    }
    Ok(())
}

fn check_index(doc: &DocSnapshot, index: u32) -> Result<(), CompileError> {
    if index as usize >= doc.palette.len() {
        return Err(CompileError::BadPaletteIndex {
            index,
            palette_len: doc.palette.len(),
        });
    }
    Ok(())
}

/// True when the region holds at least one opaque pixel in the snapshot.
fn region_has_pixels(doc: &DocSnapshot, layer: usize, x: i32, y: i32, w: u32, h: u32) -> bool {
    (y..y + h as i32).any(|cy| (x..x + w as i32).any(|cx| doc.cell(layer, cx, cy).is_some()))
}

/// Build the `doc_paint_grid` legend + rows for a cell grid of optional
/// palette indices (None → the reserved '.', untouched). Indices are
/// validated BEFORE this runs, so the glyph map just dedupes in sorted order
/// for a stable, diff-friendly legend.
fn grid_to_paint_args(
    cells: &[Option<u32>],
    width: u32,
) -> Result<(Value, Vec<String>), CompileError> {
    let mut distinct: Vec<u32> = cells.iter().flatten().copied().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() > GRID_GLYPHS.len() {
        return Err(CompileError::TooManyColors {
            count: distinct.len(),
            max: GRID_GLYPHS.len(),
        });
    }
    let glyph_of =
        |index: u32| GRID_GLYPHS[distinct.iter().position(|i| *i == index).unwrap()] as char;
    let mut legend = serde_json::Map::new();
    for index in &distinct {
        legend.insert(glyph_of(*index).to_string(), json!(index));
    }
    let rows: Vec<String> = cells
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(|c| match c {
                    Some(i) => glyph_of(*i),
                    None => '.',
                })
                .collect()
        })
        .collect();
    Ok((Value::Object(legend), rows))
}

/// Compile one DSL action into ordered atelier tool calls (lab.md item 16).
/// Validation is total: an action that survives `compile` is in-bounds,
/// on-palette, on an existing layer, stage-legal, and not a no-op — so a
/// dispatch failure afterwards means the SNAPSHOT went stale, not that the
/// action was malformed. `AdvanceStage`/`Finish` are env-side state and
/// compile to zero calls.
pub fn compile(action: &Action, doc: &DocSnapshot) -> Result<Vec<CompiledCall>, CompileError> {
    let kind = &action.action;
    let call = |tool: &str, args: Value| CompiledCall {
        tool: tool.into(),
        args,
    };
    match kind {
        ActionKind::PaintPatch {
            layer,
            x,
            y,
            width,
            height,
            grid,
        } => {
            require_editable(kind.name(), doc.stage)?;
            check_layer(doc, *layer)?;
            check_region(doc, *x, *y, *width, *height)?;
            let expected = (*width * *height) as usize;
            if grid.len() != expected {
                return Err(CompileError::GridSizeMismatch {
                    expected,
                    got: grid.len(),
                });
            }
            for index in grid {
                check_index(doc, *index)?;
            }
            let cells: Vec<Option<u32>> = grid.iter().map(|i| Some(*i)).collect();
            // A patch that repaints every cell its current colour is a no-op
            // — the classic reward-hacking action, rejected at compile time.
            let noop = cells.iter().enumerate().all(|(i, c)| {
                let (cx, cy) = (x + (i as u32 % width) as i32, y + (i as u32 / width) as i32);
                doc.cell(*layer, cx, cy) == *c
            });
            if noop {
                return Err(CompileError::EmptyModification {
                    reason: "every cell already holds that colour".into(),
                });
            }
            let (legend, rows) = grid_to_paint_args(&cells, *width)?;
            Ok(vec![call(
                "doc_paint_grid",
                json!({
                    "doc_id": doc.doc_id, "layer": layer, "frame": 0,
                    "x": x, "y": y, "legend": legend, "rows": rows,
                }),
            )])
        }
        ActionKind::ClearRegion {
            layer,
            x,
            y,
            width,
            height,
        } => {
            require_editable(kind.name(), doc.stage)?;
            check_layer(doc, *layer)?;
            check_region(doc, *x, *y, *width, *height)?;
            if !region_has_pixels(doc, *layer, *x, *y, *width, *height) {
                return Err(CompileError::EmptyModification {
                    reason: "the region is already transparent".into(),
                });
            }
            Ok(vec![call(
                "doc_region",
                json!({
                    "doc_id": doc.doc_id, "op": "clear", "layer": layer, "frame": 0,
                    "x0": x, "y0": y, "x1": x + *width as i32 - 1, "y1": y + *height as i32 - 1,
                }),
            )])
        }
        ActionKind::MoveRegion {
            layer,
            x,
            y,
            width,
            height,
            dx,
            dy,
        } => {
            require_editable(kind.name(), doc.stage)?;
            check_layer(doc, *layer)?;
            check_region(doc, *x, *y, *width, *height)?;
            if *dx == 0 && *dy == 0 {
                return Err(CompileError::EmptyModification {
                    reason: "a (0,0) offset moves nothing".into(),
                });
            }
            if !region_has_pixels(doc, *layer, *x, *y, *width, *height) {
                return Err(CompileError::EmptyModification {
                    reason: "the source region is fully transparent".into(),
                });
            }
            // The destination must land fully on-canvas: doc_region move
            // clips, and silently losing half a sprite is worse than a
            // rejected action.
            check_region(doc, x + dx, y + dy, *width, *height)?;
            Ok(vec![call(
                "doc_region",
                json!({
                    "doc_id": doc.doc_id, "op": "move", "layer": layer, "frame": 0,
                    "x0": x, "y0": y, "x1": x + *width as i32 - 1, "y1": y + *height as i32 - 1,
                    "dx": dx, "dy": dy,
                }),
            )])
        }
        ActionKind::MirrorRegion {
            layer,
            x,
            y,
            width,
            height,
            horizontal,
        } => {
            require_editable(kind.name(), doc.stage)?;
            check_layer(doc, *layer)?;
            check_region(doc, *x, *y, *width, *height)?;
            if !region_has_pixels(doc, *layer, *x, *y, *width, *height) {
                return Err(CompileError::EmptyModification {
                    reason: "the region is fully transparent".into(),
                });
            }
            // atelier's flip covers a whole cel and the clipboard path is
            // per-studio shared state, so the compiler derives the mirrored
            // pixels itself: clear the region, then paint the mirrored cells.
            let mut cells = vec![None; (*width * *height) as usize];
            for j in 0..*height as i32 {
                for i in 0..*width as i32 {
                    let (sx, sy) = if *horizontal {
                        (x + *width as i32 - 1 - i, y + j)
                    } else {
                        (x + i, y + *height as i32 - 1 - j)
                    };
                    cells[(j as u32 * width + i as u32) as usize] = doc.cell(*layer, sx, sy);
                }
            }
            let noop = cells.iter().enumerate().all(|(i, c)| {
                let (cx, cy) = (x + (i as u32 % width) as i32, y + (i as u32 / width) as i32);
                doc.cell(*layer, cx, cy) == *c
            });
            if noop {
                return Err(CompileError::EmptyModification {
                    reason: "the region is already symmetric on that axis".into(),
                });
            }
            let (legend, rows) = grid_to_paint_args(&cells, *width)?;
            Ok(vec![
                call(
                    "doc_region",
                    json!({
                        "doc_id": doc.doc_id, "op": "clear", "layer": layer, "frame": 0,
                        "x0": x, "y0": y, "x1": x + *width as i32 - 1, "y1": y + *height as i32 - 1,
                    }),
                ),
                call(
                    "doc_paint_grid",
                    json!({
                        "doc_id": doc.doc_id, "layer": layer, "frame": 0,
                        "x": x, "y": y, "legend": legend, "rows": rows,
                    }),
                ),
            ])
        }
        ActionKind::ReplaceColor { from, to } => {
            require_editable(kind.name(), doc.stage)?;
            check_index(doc, *from)?;
            check_index(doc, *to)?;
            if from == to {
                return Err(CompileError::EmptyModification {
                    reason: "from and to are the same swatch".into(),
                });
            }
            let used = doc.layers.iter().any(|r| r.contains(&Some(*from)));
            if !used {
                return Err(CompileError::EmptyModification {
                    reason: format!("swatch {from} is not on the canvas"),
                });
            }
            Ok(vec![call(
                "doc_palette",
                json!({
                    "op": "swap", "doc_id": doc.doc_id,
                    "from": [doc.palette[*from as usize]],
                    "to": [doc.palette[*to as usize]],
                }),
            )])
        }
        ActionKind::SetPalette { colors } => {
            // The palette defines what index means, so it is decided before
            // lighting begins; afterwards ReplaceColor is the colour edit.
            match doc.stage {
                Stage::Specification | Stage::Silhouette | Stage::ColorBlocking => {}
                stage => {
                    return Err(CompileError::StageViolation {
                        action: kind.name().into(),
                        stage,
                        reason: "the palette locks once colour blocking is done — use ReplaceColor"
                            .into(),
                    })
                }
            }
            if colors.is_empty() {
                return Err(CompileError::EmptyModification {
                    reason: "an empty palette can hold no colour".into(),
                });
            }
            if colors.len() > doc.max_colors {
                return Err(CompileError::TooManyColors {
                    count: colors.len(),
                    max: doc.max_colors,
                });
            }
            if colors == &doc.palette {
                return Err(CompileError::EmptyModification {
                    reason: "the document already has that palette".into(),
                });
            }
            Ok(vec![call(
                "doc_palette",
                json!({"op": "set", "doc_id": doc.doc_id, "colors": colors}),
            )])
        }
        ActionKind::AddLayer { name } => {
            if doc.stage == Stage::Finished {
                return Err(CompileError::StageViolation {
                    action: kind.name().into(),
                    stage: doc.stage,
                    reason: "the episode is finished".into(),
                });
            }
            let mut args = json!({"doc_id": doc.doc_id, "op": "add"});
            if let Some(n) = name {
                args["name"] = json!(n);
            }
            Ok(vec![call("doc_layer", args)])
        }
        ActionKind::MergeLayer { index } => {
            require_editable(kind.name(), doc.stage)?;
            check_layer(doc, *index)?;
            if *index == 0 {
                return Err(CompileError::InvalidTarget {
                    reason: "layer 0 has nothing below it to merge into".into(),
                });
            }
            Ok(vec![call(
                "doc_layer",
                json!({"doc_id": doc.doc_id, "op": "merge_down", "index": index}),
            )])
        }
        ActionKind::AdvanceStage => match doc.stage.next() {
            Some(_) => Ok(vec![]),
            None => Err(CompileError::StageViolation {
                action: kind.name().into(),
                stage: doc.stage,
                reason: "already at Finished — there is no stage forward".into(),
            }),
        },
        ActionKind::Finish => {
            if doc.stage == Stage::Finished {
                return Err(CompileError::EmptyModification {
                    reason: "the episode is already finished".into(),
                });
            }
            Ok(vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(stage: Stage) -> DocSnapshot {
        DocSnapshot {
            doc_id: "d".into(),
            width: 4,
            height: 4,
            palette: vec![[10, 0, 0, 255], [20, 0, 0, 255]],
            max_colors: 16,
            stage,
            layers: vec![vec![None; 16]],
        }
    }

    /// A snapshot with swatch 1 painted in the 2×2 block at (1,1).
    fn painted_snap(stage: Stage) -> DocSnapshot {
        let mut s = snap(stage);
        for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            s.layers[0][y * 4 + x] = Some(1);
        }
        s
    }

    fn paint(layer: usize, x: i32, y: i32, w: u32, h: u32, grid: Vec<u32>) -> Action {
        Action::new(ActionKind::PaintPatch {
            layer,
            x,
            y,
            width: w,
            height: h,
            grid,
        })
    }

    #[test]
    fn valid_paint_patch_compiles_to_doc_paint_grid() {
        let s = snap(Stage::Silhouette);
        let calls = compile(&paint(0, 1, 2, 2, 2, vec![0, 1, 1, 0]), &s).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "doc_paint_grid");
        let args = &calls[0].args;
        assert_eq!(args["doc_id"], json!("d"));
        assert_eq!(args["layer"], json!(0));
        assert_eq!(args["frame"], json!(0));
        assert_eq!(args["x"], json!(1));
        assert_eq!(args["y"], json!(2));
        // Legend dedupes in sorted order: swatch 0 → 'A', swatch 1 → 'B'.
        assert_eq!(args["legend"], json!({"A": 0, "B": 1}));
        assert_eq!(args["rows"], json!(["AB", "BA"]));
    }

    #[test]
    fn out_of_bounds_patch_is_rejected() {
        let s = snap(Stage::Silhouette);
        let e = compile(&paint(0, 3, 0, 2, 2, vec![0; 4]), &s).unwrap_err();
        assert!(
            matches!(
                e,
                CompileError::OutOfBounds {
                    x: 3,
                    width: 2,
                    canvas_width: 4,
                    ..
                }
            ),
            "{e:?}"
        );
        let e = compile(&paint(0, 0, -1, 2, 2, vec![0; 4]), &s).unwrap_err();
        assert!(
            matches!(e, CompileError::OutOfBounds { y: -1, .. }),
            "{e:?}"
        );
    }

    #[test]
    fn bad_palette_index_is_rejected() {
        let s = snap(Stage::Silhouette);
        let e = compile(&paint(0, 0, 0, 1, 1, vec![2]), &s).unwrap_err();
        assert_eq!(
            e,
            CompileError::BadPaletteIndex {
                index: 2,
                palette_len: 2
            }
        );
    }

    #[test]
    fn empty_and_malformed_patches_are_rejected() {
        let s = snap(Stage::Silhouette);
        let e = compile(&paint(0, 0, 0, 0, 2, vec![]), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        let e = compile(&paint(0, 0, 0, 2, 2, vec![0; 3]), &s).unwrap_err();
        assert_eq!(
            e,
            CompileError::GridSizeMismatch {
                expected: 4,
                got: 3
            }
        );
        // Repainting what's already there is a no-op.
        let s = painted_snap(Stage::Silhouette);
        let e = compile(&paint(0, 1, 1, 2, 2, vec![1; 4]), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
    }

    #[test]
    fn oversize_patch_is_rejected() {
        let mut s = snap(Stage::Silhouette);
        s.width = 64;
        s.height = 64;
        s.layers = vec![vec![None; 64 * 64]];
        let e = compile(&paint(0, 0, 0, 33, 33, vec![0; 33 * 33]), &s).unwrap_err();
        assert_eq!(
            e,
            CompileError::PatchTooLarge {
                pixels: 33 * 33,
                max: MAX_PATCH_PIXELS
            }
        );
    }

    #[test]
    fn unknown_layer_is_rejected() {
        let s = snap(Stage::Silhouette);
        let e = compile(&paint(3, 0, 0, 1, 1, vec![0]), &s).unwrap_err();
        assert_eq!(
            e,
            CompileError::UnknownLayer {
                layer: 3,
                layers: 1
            }
        );
    }

    #[test]
    fn stage_rules_gate_palette_and_edits() {
        // Raster edits are illegal in Specification…
        let s = snap(Stage::Specification);
        let e = compile(&paint(0, 0, 0, 1, 1, vec![0]), &s).unwrap_err();
        assert!(
            matches!(
                e,
                CompileError::StageViolation {
                    stage: Stage::Specification,
                    ..
                }
            ),
            "{e:?}"
        );
        // …but SetPalette is exactly what Specification is for.
        let set = Action::new(ActionKind::SetPalette {
            colors: vec![[1, 2, 3, 255]],
        });
        assert!(compile(&set, &s).is_ok());
        // SetPalette stays legal through ColorBlocking, then locks.
        assert!(compile(&set, &snap(Stage::ColorBlocking)).is_ok());
        let e = compile(&set, &snap(Stage::Lighting)).unwrap_err();
        assert!(
            matches!(
                e,
                CompileError::StageViolation {
                    stage: Stage::Lighting,
                    ..
                }
            ),
            "{e:?}"
        );
        // Nothing edits a Finished document.
        let e = compile(&paint(0, 0, 0, 1, 1, vec![0]), &snap(Stage::Finished)).unwrap_err();
        assert!(
            matches!(
                e,
                CompileError::StageViolation {
                    stage: Stage::Finished,
                    ..
                }
            ),
            "{e:?}"
        );
    }

    #[test]
    fn advance_stage_only_moves_forward() {
        let s = snap(Stage::Specification);
        let calls = compile(&Action::new(ActionKind::AdvanceStage), &s).unwrap();
        assert!(calls.is_empty(), "stage moves are env-side, not tool calls");
        let e = compile(
            &Action::new(ActionKind::AdvanceStage),
            &snap(Stage::Finished),
        )
        .unwrap_err();
        assert!(
            matches!(
                e,
                CompileError::StageViolation {
                    stage: Stage::Finished,
                    ..
                }
            ),
            "{e:?}"
        );
        // Finish is idempotent-checked too.
        let e = compile(&Action::new(ActionKind::Finish), &snap(Stage::Finished)).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        assert!(compile(&Action::new(ActionKind::Finish), &s)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn clear_region_rejects_clearing_nothing() {
        let s = snap(Stage::Silhouette);
        let clear = |x, y| {
            Action::new(ActionKind::ClearRegion {
                layer: 0,
                x,
                y,
                width: 2,
                height: 2,
            })
        };
        let e = compile(&clear(0, 0), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        let s = painted_snap(Stage::Silhouette);
        let calls = compile(&clear(1, 1), &s).unwrap();
        assert_eq!(calls[0].tool, "doc_region");
        assert_eq!(calls[0].args["op"], json!("clear"));
        assert_eq!(calls[0].args["x1"], json!(2), "inclusive corner");
    }

    #[test]
    fn mirror_region_compiles_to_clear_plus_mirrored_paint() {
        let mut s = snap(Stage::Silhouette);
        s.layers[0][4 + 1] = Some(1); // one pixel at (1,1)
        let mirror = Action::new(ActionKind::MirrorRegion {
            layer: 0,
            x: 1,
            y: 1,
            width: 2,
            height: 2,
            horizontal: true,
        });
        let calls = compile(&mirror, &s).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args["op"], json!("clear"));
        assert_eq!(calls[1].tool, "doc_paint_grid");
        // Horizontal mirror: the pixel lands at (2,1) — row "A." flips to ".A".
        assert_eq!(calls[1].args["rows"], json!([".A", ".."]));
        // Mirroring a symmetric region changes nothing.
        let s = painted_snap(Stage::Silhouette);
        let e = compile(&mirror, &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
    }

    #[test]
    fn move_region_requires_content_and_on_canvas_destination() {
        let s = snap(Stage::Silhouette);
        let mv = |x, y, dx, dy| {
            Action::new(ActionKind::MoveRegion {
                layer: 0,
                x,
                y,
                width: 2,
                height: 2,
                dx,
                dy,
            })
        };
        let e = compile(&mv(0, 0, 1, 0), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        let s = painted_snap(Stage::Silhouette);
        let e = compile(&mv(1, 1, 0, 0), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        // Moving the 2×2 block at (1,1) by (2,0) lands half off-canvas.
        let e = compile(&mv(1, 1, 2, 0), &s).unwrap_err();
        assert!(matches!(e, CompileError::OutOfBounds { x: 3, .. }), "{e:?}");
        let calls = compile(&mv(1, 1, -1, 0), &s).unwrap();
        assert_eq!(calls[0].args["op"], json!("move"));
        assert_eq!(calls[0].args["dx"], json!(-1));
    }

    #[test]
    fn replace_color_swaps_palette_colours() {
        let s = painted_snap(Stage::Silhouette);
        let swap = |from, to| Action::new(ActionKind::ReplaceColor { from, to });
        let calls = compile(&swap(1, 0), &s).unwrap();
        assert_eq!(calls[0].tool, "doc_palette");
        assert_eq!(calls[0].args["op"], json!("swap"));
        assert_eq!(calls[0].args["from"], json!([[20, 0, 0, 255]]));
        assert_eq!(calls[0].args["to"], json!([[10, 0, 0, 255]]));
        let e = compile(&swap(1, 1), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        // Swatch 0 is in the palette but not on the canvas.
        let e = compile(&swap(0, 1), &s).unwrap_err();
        assert!(matches!(e, CompileError::EmptyModification { .. }), "{e:?}");
        let e = compile(&swap(1, 9), &s).unwrap_err();
        assert!(
            matches!(e, CompileError::BadPaletteIndex { index: 9, .. }),
            "{e:?}"
        );
    }

    #[test]
    fn merge_layer_rejects_layer_zero() {
        let mut s = painted_snap(Stage::Cleanup);
        s.layers.push(vec![None; 16]);
        let e = compile(&Action::new(ActionKind::MergeLayer { index: 0 }), &s).unwrap_err();
        assert!(matches!(e, CompileError::InvalidTarget { .. }), "{e:?}");
        let calls = compile(&Action::new(ActionKind::MergeLayer { index: 1 }), &s).unwrap();
        assert_eq!(calls[0].args["op"], json!("merge_down"));
        assert_eq!(calls[0].args["index"], json!(1));
    }

    #[test]
    fn action_effect_metadata_matches_lab_md_item_17() {
        let v = json!({
            "action": {"ReplaceColor": {"from": 1, "to": 2}},
            "intent": "Separate the shield from the torso",
            "target_region": [4, 10, 12, 18],
            "preserve": ["helmet silhouette", "shield damage"],
            "expected_effect": "Improve silhouette readability"
        });
        let a: Action = serde_json::from_value(v).unwrap();
        assert_eq!(a.action, ActionKind::ReplaceColor { from: 1, to: 2 });
        assert_eq!(
            a.intent.as_deref(),
            Some("Separate the shield from the torso")
        );
        assert_eq!(a.target_region, Some([4, 10, 12, 18]));
        assert_eq!(a.preserve.len(), 2);
        // Metadata is optional everywhere: a bare action parses too.
        let bare: Action = serde_json::from_value(json!({"action": "Finish"})).unwrap();
        assert_eq!(bare.action, ActionKind::Finish);
        assert_eq!(bare.summarize(), "finish");
    }

    #[test]
    fn set_palette_enforces_task_budget_and_rejects_noop() {
        let mut s = snap(Stage::Specification);
        s.max_colors = 1;
        let too_many = Action::new(ActionKind::SetPalette {
            colors: vec![[1, 2, 3, 255], [4, 5, 6, 255]],
        });
        assert!(matches!(
            compile(&too_many, &s),
            Err(CompileError::TooManyColors { count: 2, max: 1 })
        ));

        s.max_colors = 16;
        let noop = Action::new(ActionKind::SetPalette {
            colors: s.palette.clone(),
        });
        assert!(matches!(
            compile(&noop, &s),
            Err(CompileError::EmptyModification { .. })
        ));
    }
}
