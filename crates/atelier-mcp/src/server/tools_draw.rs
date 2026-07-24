//! Painting tools: the op dispatchers (`doc_batch` / `doc_draw` / `doc_fx`),
//! declarative grid painting, region/clipboard ops, the active selection, and
//! graduated dither ramps — everything that puts pixels on a cel.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::{Value, json};

use super::params::{self, *};
use super::resources::base64_decode;
use super::{Atelier, batch_targets, draw_op_params, edited, palette_list, region, res, rgba};

#[tool_router(router = draw_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing) — and optionally to several frames at once: `frames` applies the same op list to each listed frame (repeated fixes on a static layer are ONE call). Each op is an object {\"op\":\"<name>\", ...} taking the same fields as the matching doc_draw/doc_fx op. Draw: pencil|line|rect|ellipse|polyline|polygon|stroke|curve|stamp|fill|bucket|gradient|scatter|noise|text|fill_cel|clear_cel. FX: blur|outline|drop_shadow|bevel|shade|form|dither|pixel_perfect|flip|shift|rotate|scale|symmetry|quantize|replace_color|adjust|gradient_map. Plus glow (batch only; params color?, radius?, intensity?, mode?). Add per-op \"opacity\" (0..255) and/or \"blend_mode\" to composite that op instead of overwriting, or \"erase\": true to make the op an ERASER (every pixel it touches goes transparent — any shape can punch a hole). Honours an active doc_select."
    )]
    pub(crate) async fn doc_batch(
        &self,
        Parameters(mut p): Parameters<DocBatch>,
    ) -> CallToolResult {
        for op in p.ops.iter_mut() {
            if let Some(obj) = op.as_object_mut() {
                params::revive_params(obj);
            }
        }
        let targets = batch_targets(p.frame, p.frames);
        if targets.len() == 1 {
            return res(self.studio().doc_batch(&p.doc_id, p.layer, p.frame, p.ops));
        }
        let studio = self.studio();
        // Preflight EVERY target frame before applying anything: the per-frame
        // loop saves each frame as it goes, so a bad frame late in the list
        // used to leave earlier frames mutated — and the errored call was never
        // journaled, silently diverging the document from its recipe.
        let n = try_res!(studio.frame_count(&p.doc_id));
        if let Some(bad) = targets.iter().find(|f| **f >= n) {
            return res(Err(format!(
                "frame {bad} out of range — document has {n} frame(s); no frames were applied"
            )));
        }
        let mut per_frame = Vec::new();
        let mut total: u64 = 0;
        for f in &targets {
            match studio.doc_batch(&p.doc_id, p.layer, *f, p.ops.clone()) {
                Ok(r) => {
                    let n = r.get("pixels_changed").and_then(Value::as_u64).unwrap_or(0);
                    total += n;
                    per_frame.push(json!({"frame": f, "pixels_changed": n}));
                }
                Err(e) => return res(Err(format!("frame {f}: {e}"))),
            }
        }
        res(Ok(json!({
            "ok": true,
            "doc_id": p.doc_id,
            "frames": targets,
            "ops": p.ops.len(),
            "pixels_changed": total,
            "per_frame": per_frame,
        })))
    }

    #[tool(
        description = "Draw ONE shape/mark on a cel — the single-op form of doc_batch (use doc_batch for many ops at once). `op` plus its flattened params: pencil{points,color,size?} (each point is a SEPARATE dab — use polyline or line to CONNECT points into a stroke) · line{x0,y0,x1,y1,color,size?} · rect{x0,y0,x1,y1,color,fill?,size?} · ellipse{cx,cy,rx,ry,color,fill?} · polyline{points,color,size?,closed?} · polygon{points,color,fill?} · stroke{points,color,width?,aa?,snap?} (aa=true softens edges with PARTIAL-ALPHA pixels — set aa=false on a locked palette or before a GIF export, whose alpha is 1-bit) · curve{points,color,width?,aa?,snap?} (a bezier through the control points — the stroke op, curved) · stamp{points,tip,colorize?} (a custom brush: tip is {w,h,pixels:[[r,g,b,a],…]} stamped centred on each point; colorize tints it) · fill{x,y,color,tolerance?} · gradient{stops,kind?,x0,y0,x1,y1,…} · scatter{colors,x0,y0,x1,y1,density?,seed?,size?} · noise{stops,x0,y0,x1,y1,kind?,scale?,…} · text{x,y,text,color,size?} · fill_cel{color} · box_iso{cx,cy,s,ht,color,light_right?} (shaded isometric cuboid — the hard-surface form primitive: crates, blocks, dice) · panel{x,y,w,h,fill,border?,bevel?} (HUD/UI box: filled body + border + inner bevel; pair with op=text for labels). All also accept opacity, blend_mode, and erase (true = the shape ERASES to transparent instead of drawing). Honours an active doc_select."
    )]
    pub(crate) async fn doc_draw(&self, Parameters(mut p): Parameters<DocDraw>) -> CallToolResult {
        params::revive_params(&mut p.params);
        let studio = self.studio();
        match p.op.as_str() {
            "box_iso" => {
                let b: DocBox = try_res!(draw_op_params(&p));
                res(studio.box_iso(
                    &b.doc_id,
                    b.layer,
                    b.frame,
                    b.cx,
                    b.cy,
                    b.s,
                    b.ht,
                    try_res!(rgba(&b.color)),
                    b.light_right.unwrap_or(true),
                ))
            }
            "panel" => {
                let pn: DocPanel = try_res!(draw_op_params(&p));
                res(studio.panel(
                    &pn.doc_id,
                    pn.layer,
                    pn.frame,
                    pn.x,
                    pn.y,
                    pn.w,
                    pn.h,
                    try_res!(rgba(&pn.fill)),
                    match &pn.border {
                        Some(b) => try_res!(rgba(b)),
                        None => [20, 20, 28, 255],
                    },
                    pn.bevel.unwrap_or(true),
                ))
            }
            _ => res(studio.doc_draw(&p.doc_id, p.layer, p.frame, &p.op, p.params)),
        }
    }

    #[tool(
        description = "Apply ONE transform/effect op that REWORKS existing pixels — the complement of doc_draw (which adds marks), single-op form of doc_batch. `op` plus its flattened params, grouped: **effects** blur{radius,region?} · outline{color,aa?} · drop_shadow{color,dx?,dy?,blur?,shadow_opacity?} · bevel{light,dark,depth?} · shade{light_dir?,steps?,mode?,ramp?,region?} · form{form,light_dir?,ramp?,strength?,region?} · dither{color_a,color_b,pattern?,density?,region?,only_existing?} · pixel_perfect{region?,color?} (thins 1px staircases on OUTLINES/lines — never run it over filled shapes or size>1 strokes, it shreds them); **transform** flip{horizontal?} · shift{dx?,dy?,wrap?} · rotate{turns?} (quarter-turns clockwise about the canvas centre — content clips, canvas never resizes) · scale{w,h,method?} (nearest or area-average; the cel keeps its anchor) · symmetry{vertical?,horizontal?,keep_left?,keep_top?}; **colour** quantize{colors,max_colors?} · replace_color{from,to,tolerance?} · adjust{hue?,sat?,lum?,region?} · gradient_map{stops,region?} (remap luminance through colour stops, alpha kept). All also accept opacity/blend_mode and honour an active doc_select."
    )]
    pub(crate) async fn doc_fx(&self, Parameters(mut p): Parameters<DocFx>) -> CallToolResult {
        params::revive_params(&mut p.params);
        res(self
            .studio()
            .doc_fx(&p.doc_id, p.layer, p.frame, &p.op, p.params))
    }

    #[tool(
        description = "Region + clipboard ops on a cel. `op`: copy (rect [x0,y0,x1,y1] → clipboard) · cut (copy + clear) · clear (erase the rect) · move (shift the rect by dx,dy in place) · paste (clipboard at x,y; `blend` source-over by default, false overwrites). Clipboard is cross-document."
    )]
    pub(crate) async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        // A replayed paste carries its pixels (journal-embedded) and must not
        // depend on — or clobber — the live clipboard.
        if p.op == "paste"
            && let Some(cb) = &p.clipboard
        {
            let buf = match base64_decode(&cb.data) {
                Ok(b) if b.len() == (cb.w as usize) * (cb.h as usize) * 4 => b,
                Ok(b) => {
                    return res(Err(format!(
                        "embedded clipboard is {} bytes, expected {}×{}×4",
                        b.len(),
                        cb.w,
                        cb.h
                    )));
                }
                Err(e) => return res(Err(format!("embedded clipboard: {e}"))),
            };
            return res(self.studio().doc_paste_pixels(
                &p.doc_id,
                p.layer,
                p.frame,
                p.x.unwrap_or(0),
                p.y.unwrap_or(0),
                cb.w,
                cb.h,
                &buf,
                p.blend.unwrap_or(true),
            ));
        }
        res(self.studio().doc_region(
            &p.doc_id, &p.op, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1, p.dx, p.dy, p.x, p.y,
            p.blend,
        ))
    }

    #[tool(
        description = "Set/modify the active pixel selection so subsequent painting ops (fill/gradient/scatter/rect/ellipse/polygon/pencil/line/batch) are confined to it. shape: rect (x0,y0,x1,y1) | ellipse (cx,cy,rx,ry) | polygon/lasso (points [[x,y],…] ≥3, auto-closed) | color (layer,frame + `color` or sample x,y + tolerance) | all | none (clear). mode: replace (default) | add | subtract | intersect."
    )]
    pub(crate) async fn doc_select(&self, Parameters(p): Parameters<DocSelect>) -> CallToolResult {
        let shape = p.shape.as_deref().unwrap_or("rect");
        let mode = p.mode.as_deref().unwrap_or("replace");
        let rect = match (p.x0, p.y0, p.x1, p.y1) {
            (Some(x0), Some(y0), Some(x1), Some(y1)) => Some((x0, y0, x1, y1)),
            _ => None,
        };
        let ell = match (p.cx, p.cy, p.rx, p.ry) {
            (Some(cx), Some(cy), Some(rx), Some(ry)) => Some((cx, cy, rx, ry)),
            _ => None,
        };
        let color_at = if shape == "color" {
            Some(atelier_studio::ColorSelect {
                layer: p.layer.unwrap_or(0),
                frame: p.frame.unwrap_or(0),
                color: match &p.color {
                    Some(c) => Some(try_res!(rgba(c))),
                    None => None,
                },
                sample: match (p.x, p.y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                },
                tol: p.tolerance.unwrap_or(0),
            })
        } else {
            None
        };
        res(self.studio().doc_select(
            &p.doc_id,
            shape,
            mode,
            rect,
            ell,
            color_at,
            p.points.as_deref(),
        ))
    }

    #[tool(
        description = "Paint a whole region DECLARATIVELY from a character grid (the inverse of doc_dump_region): `legend` maps single characters to [r,g,b(,a)] colours or integer PALETTE INDICES, `rows` are pixel-row strings ('.'/' ' leave the pixel untouched). Emitting a sprite as a grid eliminates the absolute-coordinate failure class — prefer this over long pencil/rect sequences for detailed shapes. Verify by diffing against doc_dump_region. Returns painted/clipped counts — call doc_look to SEE the result. Honours an active selection."
    )]
    pub(crate) async fn doc_paint_grid(
        &self,
        Parameters(p): Parameters<DocPaintGrid>,
    ) -> CallToolResult {
        let studio = self.studio();
        let r = studio.doc_paint_grid(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x.unwrap_or(0),
            p.y.unwrap_or(0),
            p.legend,
            p.rows,
        );
        edited(r)
    }

    #[tool(
        description = "Graduated multi-tone dithering across a whole RAMP along an axis (h|v|radial) — master gradient shading, vs the two-colour `dither`. pattern bayer2/4/8 | checker | ign (blue-noise, no visible matrix grid). only_existing repaints just opaque pixels (shade existing art, keep alpha). Honours an active selection. Snap afterwards with doc_palette op=snap if it drifts."
    )]
    pub(crate) async fn doc_dither_ramp(
        &self,
        Parameters(p): Parameters<DocDitherRamp>,
    ) -> CallToolResult {
        res(self.studio().dither_ramp(
            &p.doc_id,
            p.layer,
            p.frame,
            try_res!(region(&p.region)),
            try_res!(palette_list(&p.ramp)),
            p.axis.as_deref().unwrap_or("v"),
            p.pattern.as_deref().unwrap_or("bayer4"),
            p.only_existing.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Stamp tiles from a tileset document onto this cel as a tilemap. `op`: place — `tiles_doc`'s flattened frame 0 is sliced row-major into tile_w×tile_h tiles (index 0 = top-left; the tileset is read-only, never modified), and every [cell_x, cell_y, tile_index] of `cells` lands at pixel (cell_x*tile_w, cell_y*tile_h), source-over, clipped to the canvas (off-canvas cells are skipped, reported in cells_skipped). One call = one tilemap = one journal entry; all args are plain JSON, so replay is byte-identical. Export the tileset itself with doc_export op=tileset."
    )]
    pub(crate) async fn doc_tile(&self, Parameters(p): Parameters<DocTile>) -> CallToolResult {
        if p.op != "place" {
            return res(Err(format!("unknown doc_tile op '{}' — place", p.op)));
        }
        let mut cells: Vec<[i32; 3]> = Vec::with_capacity(p.cells.len());
        for (i, c) in p.cells.iter().enumerate() {
            match <[i32; 3]>::try_from(c.as_slice()) {
                Ok(cell) => cells.push(cell),
                Err(_) => {
                    return res(Err(format!(
                        "cells[{i}] must be [cell_x, cell_y, tile_index], got {c:?}"
                    )));
                }
            }
        }
        edited(self.studio().doc_place_tiles(
            &p.doc_id,
            p.layer,
            p.frame,
            &p.tiles_doc,
            p.tile_w,
            p.tile_h,
            &cells,
        ))
    }
}
