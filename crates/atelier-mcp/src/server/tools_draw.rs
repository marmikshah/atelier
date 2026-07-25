//! Painting tools: op dispatchers, declarative grid painting, stateless region
//! edits, and graduated dither ramps.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::{Value, json};

use super::params::{self, *};
use super::{Atelier, batch_targets, edited, palette_list, region, res};

#[tool_router(router = draw_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing) — and optionally to several frames at once: `frames` applies the same op list to each listed frame. Each op is an object {\"op\":\"<name>\", ...} taking the same fields as the matching doc_draw/doc_fx op. Draw: pencil|line|rect|ellipse|polyline|polygon|stroke|curve|stamp|fill|bucket|gradient|scatter|noise|text|fill_cel|clear_cel. FX: blur|outline|drop_shadow|bevel|shade|form|dither|pixel_perfect|flip|shift|rotate|scale|symmetry|quantize|replace_color|adjust|gradient_map. Plus glow (batch only). Add per-op \"opacity\" (0..255), \"blend_mode\", or \"erase\": true."
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
        description = "Draw ONE mark on a cel — the single-op form of doc_batch: pencil{points,color,size?}; line/rect{x0,y0,x1,y1,color,...}; ellipse{cx,cy,rx,ry,color,fill?}; polyline/polygon/stroke/curve{points,color,...}; stamp{points,tip,colorize?}; fill/bucket{x,y,color,tolerance?}; gradient{stops,...}; scatter{colors,bounds,...}; noise{stops,bounds,...}; text{x,y,text,color,size?}; fill_cel{color}; clear_cel. These accept opacity, blend_mode, and erase."
    )]
    pub(crate) async fn doc_draw(&self, Parameters(mut p): Parameters<DocDraw>) -> CallToolResult {
        params::revive_params(&mut p.params);
        res(self
            .studio()
            .doc_draw(&p.doc_id, p.layer, p.frame, p.op.as_str(), p.params))
    }

    #[tool(
        description = "Rework one cel — the single-op form of doc_batch. Effects: blur{radius}; outline{color}; drop_shadow{color,...}; bevel{light,dark}; shade/form{...}; dither{color_a,color_b,...}; pixel_perfect{...}. Transforms: flip; shift{dx,dy,wrap?}; rotate{turns?}; scale{w,h,method?}; symmetry. Colour: quantize{colors,max_colors?}; replace_color{from,to}; adjust{hue?,sat?,lum?}; gradient_map{stops}. All accept opacity, blend_mode, and erase."
    )]
    pub(crate) async fn doc_fx(&self, Parameters(mut p): Parameters<DocFx>) -> CallToolResult {
        params::revive_params(&mut p.params);
        res(self
            .studio()
            .doc_fx(&p.doc_id, p.layer, p.frame, p.op.as_str(), p.params))
    }

    #[tool(
        description = "Apply a self-contained rectangular edit to one cel. `op`: clear erases `rect` [x0,y0,x1,y1]; move shifts `rect` by `offset` [dx,dy]."
    )]
    pub(crate) async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        edited(
            self.studio()
                .doc_region(&p.doc_id, p.op, p.layer, p.frame, Some(p.rect), p.offset),
        )
    }

    #[tool(
        description = "Paint a whole region declaratively from a character grid. `legend` maps single characters to [r,g,b(,a)] colours or palette indices; `rows` are pixel-row strings and '.'/' ' leave pixels untouched. Returns painted/clipped counts."
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
        description = "Graduated multi-tone dithering across a ramp along axis h, v, or radial. pattern: bayer2/4/8, checker, or ign. only_existing repaints opaque pixels while preserving transparency."
    )]
    pub(crate) async fn doc_dither_ramp(
        &self,
        Parameters(p): Parameters<DocDitherRamp>,
    ) -> CallToolResult {
        res(self.studio().dither_ramp(
            &p.doc_id,
            p.layer,
            p.frame,
            region(p.region),
            try_res!(palette_list(&p.ramp)),
            p.axis.unwrap_or_default(),
            p.pattern.unwrap_or_default(),
            p.only_existing.unwrap_or(true),
        ))
    }
}
