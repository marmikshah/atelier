//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON text content; studio
//! errors keep the {"error": ...} payload AND set `is_error` so MCP harnesses
//! flag the failure instead of treating it as a success.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Value};

use atelier_studio::Studio;

mod params;
mod recorder;
mod resources;
mod toolsdoc;

pub use toolsdoc::{tools_html, tools_text};

use params::*;
use recorder::Recorder;
use resources::{base64, parse_resource_uri, ResourceTarget, RESOURCE_RENDER_SCALE};

fn j(v: Value) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
}

/// Wraps a studio result as a tool result: Ok becomes a JSON text part; errors
/// keep the {"error": ...} payload (replay and older clients sniff it) and set
/// `is_error` so the harness surfaces the failure.
fn res(r: Result<Value, String>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::success(vec![Content::text(j(v))]),
        Err(e) => CallToolResult::error(vec![Content::text(j(json!({"error": e})))]),
    }
}

/// Wrap an image-producing studio result as MCP content: an inline PNG (so the
/// agent SEES the pixels in the same turn — no separate file read) plus a JSON
/// text part with the measured stats. Errors come back as a `{"error": ...}`
/// text part with `is_error` set, matching `res`.
fn img_result(r: Result<(Vec<u8>, Value), String>) -> CallToolResult {
    match r {
        Ok((png, report)) => CallToolResult::success(vec![
            Content::image(base64(&png), "image/png"),
            Content::text(j(report)),
        ]),
        Err(e) => CallToolResult::error(vec![Content::text(j(json!({"error": e})))]),
    }
}

/// Like `img_result`, but the PNG is optional: tools whose preview is
/// conditional (diff overlays, seam overlays) degrade to a plain text result.
fn opt_img_result(r: Result<(Option<Vec<u8>>, Value), String>) -> CallToolResult {
    match r {
        Ok((Some(png), report)) => img_result(Ok((png, report))),
        Ok((None, report)) => res(Ok(report)),
        Err(e) => res(Err(e)),
    }
}

/// Acknowledge an edit op with its TEXT report only — no inline preview image.
/// doc_look is the agent's only eye: returning a preview PNG from every edit
/// tool taxed every LLM client with image tokens (an upscaled frame is tens of
/// thousands of tokens) and undercut the deliberate see-and-fix loop. If the
/// agent needs to see the result, it calls doc_look.
fn edited(r: Result<Value, String>) -> CallToolResult {
    res(r)
}

/// A list of `[r,g,b(,a)]` arrays -> a palette of RGBA swatches.
fn palette_list(v: &[Vec<i64>]) -> Vec<[u8; 4]> {
    v.iter().map(|c| rgba(c)).collect()
}

/// [r,g,b] or [r,g,b,a] -> RGBA (alpha defaults to 255).
fn rgba(v: &[i64]) -> [u8; 4] {
    [
        v.first().copied().unwrap_or(0) as u8,
        v.get(1).copied().unwrap_or(0) as u8,
        v.get(2).copied().unwrap_or(0) as u8,
        v.get(3).copied().unwrap_or(255) as u8,
    ]
}

/// Parse the `doc_palette op=snap` / FX alpha policy string into an `AlphaSnap`.
/// The one place the mode strings are known — an unknown mode errors here
/// instead of silently mapping to Preserve.
fn alpha_snap(
    mode: Option<&str>,
    cutoff: Option<u8>,
    bg: Option<&[i64]>,
) -> Result<atelier_core::document::AlphaSnap, String> {
    use atelier_core::document::AlphaSnap;
    match mode.unwrap_or("preserve") {
        "preserve" => Ok(AlphaSnap::Preserve),
        "opaque" => Ok(AlphaSnap::Opaque(cutoff.unwrap_or(128))),
        "flatten" => Ok(AlphaSnap::Flatten(bg.map(rgba).unwrap_or([0, 0, 0, 255]))),
        m => Err(format!(
            "unknown alpha mode '{m}' — use preserve | opaque | flatten"
        )),
    }
}

/// Optional [x0,y0,x1,y1] -> (x0,y0,x1,y1). A region that is present but
/// shorter than 4 numbers is an agent mistake — error loudly instead of
/// silently acting on the whole canvas.
fn region(r: &Option<Vec<i32>>) -> Result<Option<(i32, i32, i32, i32)>, String> {
    match r {
        None => Ok(None),
        Some(v) if v.len() >= 4 => Ok(Some((v[0], v[1], v[2], v[3]))),
        Some(v) => Err(format!(
            "region needs 4 numbers [x0,y0,x1,y1], got {} — omit it to act on the whole canvas",
            v.len()
        )),
    }
}

/// Re-deserialize `doc_draw`'s flattened op params (plus the shared doc_id/
/// layer/frame) into a composite draw op's own typed struct (box_iso → DocBox,
/// panel → DocPanel), so those primitives ride the single `doc_draw` surface.
fn draw_op_params<T: serde::de::DeserializeOwned>(p: &DocDraw) -> Result<T, String> {
    let mut m = p.params.clone();
    m.insert("doc_id".to_string(), Value::String(p.doc_id.clone()));
    m.insert("layer".to_string(), Value::from(p.layer as u64));
    m.insert("frame".to_string(), Value::from(p.frame as u64));
    serde_json::from_value(Value::Object(m))
        .map_err(|e| format!("doc_draw op={}: bad params — {e}", p.op))
}

/// Unwrap a parse result inside a tool method, or return it as the tool error.
macro_rules! try_res {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return res(Err(e)),
        }
    };
}

/// The JSON report a tool answered with. Scans for the first TEXT part rather
/// than taking `content[0]`: the image-returning tools put the PNG first and the
/// stats after it.
fn result_json(result: &rmcp::model::CallToolResult) -> Option<Value> {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .find_map(|t| serde_json::from_str::<Value>(&t.text).ok())
}

fn is_error_result(result: &rmcp::model::CallToolResult) -> bool {
    if result.is_error == Some(true) {
        return true;
    }
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .and_then(|t| serde_json::from_str::<Value>(&t.text).ok())
        .is_some_and(|v| v.get("error").is_some())
}

// --- server ----------------------------------------------------------------

#[derive(Clone)]
pub struct Atelier {
    /// Shared so concurrent HTTP sessions serialise document file writes.
    studio: std::sync::Arc<std::sync::Mutex<Studio>>,
    tool_router: ToolRouter<Self>,
    /// Optional session recorder; when set, each tool call is logged to a recipe.
    recorder: Option<Recorder>,
}

impl Atelier {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with_studio(std::sync::Arc::new(std::sync::Mutex::new(Studio::new())))
    }

    pub fn with_studio(studio: std::sync::Arc<std::sync::Mutex<Studio>>) -> Self {
        Self {
            studio,
            tool_router: Self::tool_router(),
            recorder: None,
        }
    }

    /// Every tool the server has. There is no profile filter: the surface is
    /// small enough that hiding part of it behind a flag would cost more in
    /// confusion than it ever saved in context.
    fn advertised_tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// Enable session recording: every tool call is appended to a recipe at `path`.
    fn with_recording(mut self, path: std::path::PathBuf) -> Self {
        self.recorder = Some(Recorder::new(path));
        self
    }

    /// The shared studio.
    ///
    /// Recovers from a poisoned lock instead of propagating the panic: one bad
    /// tool call used to take the whole server down with it, since every later
    /// call re-panicked on the poison. The studio keeps almost no in-memory
    /// state — documents load and save per op — so the guarded data is not
    /// meaningfully corrupted by a panic mid-op, and a live server that answers
    /// the next call beats one that is bricked until restart.
    fn studio(&self) -> std::sync::MutexGuard<'_, Studio> {
        self.studio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Enumerate every browsable resource: per document, its structure JSON and a
    /// frame-0 PNG render, each with a human-readable name.
    fn list_resource_specs(&self) -> Vec<Resource> {
        let docs = self.studio().list_docs();
        let mut out = Vec::new();
        for d in docs["documents"].as_array().into_iter().flatten() {
            let Some(id) = d["id"].as_str() else { continue };
            let name = d["name"].as_str().unwrap_or(id);
            out.push(
                RawResource::new(format!("atelier://doc/{id}"), format!("{name} (structure)"))
                    .with_description("Document structure: layers, frames, cels, tags.")
                    .with_mime_type("application/json")
                    .no_annotation(),
            );
            out.push(
                RawResource::new(
                    format!("atelier://doc/{id}/render"),
                    format!("{name} (render)"),
                )
                .with_description(format!(
                    "Frame 0 flattened to a PNG at scale {RESOURCE_RENDER_SCALE}."
                ))
                .with_mime_type("image/png")
                .no_annotation(),
            );
        }
        out
    }

    /// Resolve a parsed [`ResourceTarget`] to its contents (structure JSON text or
    /// a PNG blob). Studio errors surface as `resource_not_found`.
    fn fetch_resource(
        &self,
        uri: &str,
        target: ResourceTarget,
    ) -> Result<ResourceContents, String> {
        match target {
            ResourceTarget::Structure(id) => {
                let v = self.studio().doc_info(&id)?;
                Ok(ResourceContents::text(j(v), uri).with_mime_type("application/json"))
            }
            ResourceTarget::Render(id) => {
                let png = self
                    .studio()
                    .render_png_bytes(&id, 0, RESOURCE_RENDER_SCALE)?;
                Ok(ResourceContents::blob(base64(&png), uri).with_mime_type("image/png"))
            }
        }
    }
}

#[tool_router(router = tool_router)]
impl Atelier {
    // -- library --
    #[tool(
        description = "Create an editable document (layered canvas + timeline). Returns its id + structure."
    )]
    async fn doc_create(&self, Parameters(p): Parameters<DocCreate>) -> CallToolResult {
        res(self.studio().doc_create(&p.name, p.width, p.height))
    }

    #[tool(
        description = "List documents (id, name, size, frame/layer counts). Optional `prefix` selects a family by id start (`hero-` matches hero-idle, hero-run); `contains` filters by substring; both = AND. Omit both to list everything."
    )]
    async fn list_docs(&self, Parameters(p): Parameters<ListDocs>) -> CallToolResult {
        res(Ok(self.studio().list_docs_filtered(
            p.prefix.as_deref(),
            p.contains.as_deref(),
        )))
    }

    #[tool(description = "Get a document's structure: layers, frames, cels, tags.")]
    async fn doc_info(&self, Parameters(p): Parameters<DocRef>) -> CallToolResult {
        res(self.studio().doc_info(&p.doc_id))
    }

    #[tool(description = "Delete a document and all its files.")]
    async fn delete_doc(&self, Parameters(p): Parameters<DocRef>) -> CallToolResult {
        res(self.studio().delete_doc(&p.doc_id))
    }

    // -- documents: editable layered/timeline sprites --
    #[tool(
        description = "Layer structure in one tool. `op`: add (new layer on top — name/opacity/blend) · set (change layer `index`'s visible/opacity/blend; omit a field to leave it) · move (`index`→`to_index`) · insert (new layer at `index`) · delete · rename · duplicate · merge_down (`index` onto the layer below). Blend ∈ normal/multiply/screen/add/overlay/soft-light/hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/exclusion."
    )]
    async fn doc_layer(&self, Parameters(p): Parameters<DocLayer>) -> CallToolResult {
        res(self.studio().doc_layer(
            &p.doc_id, &p.op, p.index, p.to_index, p.name, p.visible, p.opacity, p.blend,
        ))
    }

    #[tool(
        description = "Frame lifecycle + timing in one tool. `op`: add (append a frame; `copy_from` duplicates that frame's cels, `duration_ms` default 100) · duration (set frame `frame`'s `duration_ms`) · insert (new frame at `frame`) · duplicate (`frame`) · delete (`frame`; last frame protected) · move (`frame`→`to_index`). Cels reindex and tag ranges remap. (Animation tags have their own tool: doc_add_tag.)"
    )]
    async fn doc_frame(&self, Parameters(p): Parameters<DocFrame>) -> CallToolResult {
        let studio = self.studio();
        // delete destroys cels and move can scramble tags — auto-checkpoint the
        if p.op == "delete" || p.op == "move" {
            studio.auto_checkpoint(&p.doc_id, "doc_frame");
        }
        res(studio.doc_frame(
            &p.doc_id,
            &p.op,
            p.frame,
            p.copy_from,
            p.to_index,
            p.duration_ms,
        ))
    }

    #[tool(
        description = "Add an animation tag (named frame range). direction: forward/reverse/pingpong."
    )]
    async fn doc_add_tag(&self, Parameters(p): Parameters<DocAddTag>) -> CallToolResult {
        res(self.studio().doc_add_tag(
            &p.doc_id,
            &p.name,
            p.from,
            p.to,
            p.direction.as_deref().unwrap_or("forward"),
        ))
    }

    #[tool(description = "Clear (empty) a layer×frame cel.")]
    async fn doc_clear_cel(&self, Parameters(p): Parameters<DocCel>) -> CallToolResult {
        res(self.studio().doc_clear_cel(&p.doc_id, p.layer, p.frame))
    }

    #[tool(
        description = "Region + clipboard ops on a cel. `op`: copy (rect [x0,y0,x1,y1] → clipboard) · cut (copy + clear) · clear (erase the rect) · move (shift the rect by dx,dy in place) · paste (clipboard at x,y; `blend` source-over by default, false overwrites). Clipboard is cross-document."
    )]
    async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        res(self.studio().doc_region(
            &p.doc_id, &p.op, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1, p.dx, p.dy, p.x, p.y,
            p.blend,
        ))
    }

    #[tool(
        description = "Export to a file. Per-document `op`: sheet (horizontal spritesheet PNG + JSON meta — rects/durations/tags/pivots/boxes/palette; `meta`=standard writes the industry-standard hash sprite-JSON engines' existing importers parse instead — no pivots/boxes in that shape) · anim (animated `format`=gif|apng, optional `tag` plays that animation in its direction) · tileset (slice a `tile_w`×`tile_h` grid → PNG + Tiled .tsx + JSON; canvas must divide evenly). Library-wide `op` (omit doc_id): all (one spritesheet PNG + JSON per document into `out_path` as a DIRECTORY) · atlas (pack EVERY frame of EVERY document into one atlas PNG + master JSON map — doc/frame/rect/duration/pivot — for slicing a whole game from one texture; `max_width` wraps the shelf packer, default 512). GIF/APNG alpha is 1-bit: a pixel is fully opaque or fully gone, so animation tuned with partial alpha (aa edges, per-op opacity) will jump at export — snap or flatten first. Shared: out_path, scale (sheet/anim/all/atlas 4, tileset 1)."
    )]
    async fn doc_export(&self, Parameters(p): Parameters<DocExport>) -> CallToolResult {
        let studio = self.studio();
        match p.op.as_str() {
            "all" => res(studio.export_all(&p.out_path, p.scale.unwrap_or(4))),
            "atlas" => {
                let max_width = p
                    .params
                    .get("max_width")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32)
                    .unwrap_or(512);
                res(studio.export_atlas(&p.out_path, p.scale.unwrap_or(4), max_width))
            }
            _ => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err(format!(
                        "doc_export op={} needs `doc_id` (only op=all|atlas span the library)",
                        p.op
                    )));
                };
                res(studio.doc_export(doc_id, &p.op, &p.out_path, p.scale, &p.params))
            }
        }
    }

    #[tool(
        description = "Set/modify the active pixel selection so subsequent painting ops (fill/gradient/scatter/rect/ellipse/polygon/pencil/line/batch) are confined to it. shape: rect (x0,y0,x1,y1) | ellipse (cx,cy,rx,ry) | color (layer,frame + `color` or sample x,y + tolerance) | all | none (clear). mode: replace (default) | add | subtract | intersect."
    )]
    async fn doc_select(&self, Parameters(p): Parameters<DocSelect>) -> CallToolResult {
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
                color: p.color.as_ref().map(|c| rgba(c)),
                sample: match (p.x, p.y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                },
                tol: p.tolerance.unwrap_or(0),
            })
        } else {
            None
        };
        res(self
            .studio()
            .doc_select(&p.doc_id, shape, mode, rect, ell, color_at))
    }

    // -- canvas readers (read-only analysis to SEE the canvas as data) --
    #[tool(
        description = "Dump a region of a frame as a text grid so you can read exact pixels blind. mode=\"symbol\" maps each distinct colour to a glyph (A..Z a..z 0..9) with a legend, `.`=transparent; mode=\"hex\" emits #rrggbb(aa)/`.` tokens. `layer` dumps one cel (omit = flattened). `region` [x0,y0,x1,y1] caps at 4096 px — crop large canvases."
    )]
    async fn doc_dump_region(&self, Parameters(p): Parameters<DocDumpRegion>) -> CallToolResult {
        res(self.studio().doc_dump_region(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            try_res!(region(&p.region)),
            p.mode.as_deref().unwrap_or("symbol"),
        ))
    }

    #[tool(
        description = "Opaque-vs-transparent shape report for a frame: tight bbox, fill_ratio (opaque/canvas), and a #/. grid of the whole canvas. `layer` reads one cel (omit = flattened). `alpha_threshold` is the min alpha counted opaque (default 1). Read a sprite's silhouette/readability at a glance."
    )]
    async fn doc_silhouette(&self, Parameters(p): Parameters<DocSilhouette>) -> CallToolResult {
        res(self.studio().doc_silhouette(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.alpha_threshold.unwrap_or(1),
        ))
    }

    #[tool(
        description = "Connected-component analysis of a frame: each blob's bbox, centroid, area and dominant colour (sorted by area desc, capped 64). `connectivity` 4|8 (default 8); `color` restricts to that exact colour (omit = any opaque); `min_area` filters the list. Stray 1–2px `specks` are always reported — catches orphan/leftover pixels."
    )]
    async fn doc_components(&self, Parameters(p): Parameters<DocComponents>) -> CallToolResult {
        let color = p.color.as_ref().filter(|c| c.len() >= 3).map(|c| rgba(c));
        res(self.studio().doc_components(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.connectivity.unwrap_or(8),
            color,
            p.min_area.unwrap_or(1),
        ))
    }

    // -- value & colour feedback (read-only analysis to judge values/colour) --

    // -- animation & tiling feedback (read-only) + keyframe write --
    #[tool(
        description = "Diff two frames pixel-by-pixel: returns changed/added/removed/recolored counts and the change_bbox. `layer` diffs one cel (omit = flattened). `region` [x0,y0,x1,y1] restricts the area. grid=true adds a text map (`.`unchanged `+`added `-`removed `~`recolored, area capped 4096 px). render=\"overlay\" returns an INLINE PNG of frame_b dimmed 40% with changed pixels flagged (green=added/red=removed/yellow=recoloured). Inspect what actually moved between animation frames."
    )]
    async fn doc_frame_diff(&self, Parameters(p): Parameters<DocFrameDiff>) -> CallToolResult {
        opt_img_result(self.studio().doc_frame_diff(
            &p.doc_id,
            p.frame_a,
            p.frame_b,
            p.layer,
            try_res!(region(&p.region)),
            p.grid.unwrap_or(false),
            p.render.as_deref().unwrap_or("none"),
            p.out_path.as_deref(),
            p.scale.unwrap_or(4),
        ))
    }

    #[tool(
        description = "Tiling seam check: wrap-test a frame's far edge against the near edge it abuts when repeated. axis=\"horizontal\" tests left↔right, \"vertical\" top↔bottom, \"both\" runs each. Per axis returns {mismatches, max_delta, worst:[[x,y,delta] ≤10]}; any mismatch also returns an INLINE overlay PNG (a directional one-shot effect that fades out will always mismatch its own wrap — that is the effect restarting, not a tiling bug) (frame dimmed, bad edge pixels red) so you see WHERE the seam pops. `threshold` is the max per-channel delta still counted a match (default 0). Verify seamless tiles."
    )]
    async fn doc_seam_report(&self, Parameters(p): Parameters<DocSeamReport>) -> CallToolResult {
        opt_img_result(self.studio().doc_seam_report(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            p.axis.as_deref().unwrap_or("both"),
            p.threshold.unwrap_or(0),
            p.out_path.as_deref(),
        ))
    }

    #[tool(
        description = "Audit an animation loop. mode=\"seam\" diffs the wrap the loop actually plays and returns seam_score = changed/opaque plus the change_bbox naming WHERE the loop pops (an EFFECT that fades to nothing scores ~1.0 by construction — for FX judge the absolute pixel count, not the ratio). mode=\"spacing\" tracks the opaque-mass CENTROID per played frame (per_frame_center/offset, total_drift, evenness; 0 = mechanically even); pass `region` to isolate one part (a swinging arm) over a static body. mode=\"arc\" returns the centroid trajectory, arc_residual (~0 = mechanical straight slide; higher = proper arc) and volume_cv (~0 = constant mass). mode=\"timing\" returns per-frame durations and flags uniform timing (reads mechanical — hold contacts ~1.5x). `tag` audits one tag (omit = whole timeline)."
    )]
    async fn doc_anim_audit(&self, Parameters(p): Parameters<DocAnimAudit>) -> CallToolResult {
        res(self.studio().doc_anim_audit(
            &p.doc_id,
            p.tag.as_deref(),
            p.layer,
            &p.mode,
            try_res!(region(&p.region)),
        ))
    }

    // -- pivots / palette (engine-ready sprites, cohesive colour) --

    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing). Each op is an object {\"op\":\"<name>\", ...} taking the same fields as the matching doc_draw/doc_fx op. Draw: pencil|line|rect|ellipse|polyline|polygon|stroke|fill|bucket|gradient|scatter|noise|text|fill_cel|clear_cel. FX: blur|outline|drop_shadow|bevel|shade|form|dither|pixel_perfect|flip|shift|symmetry|quantize|replace_color|adjust|gradient_map. Plus glow (batch only; params color?, radius?, intensity?, mode?). Add per-op \"opacity\" (0..255) and/or \"blend_mode\" to composite that op instead of overwriting. Honours an active doc_select."
    )]
    async fn doc_batch(&self, Parameters(p): Parameters<DocBatch>) -> CallToolResult {
        res(self.studio().doc_batch(&p.doc_id, p.layer, p.frame, p.ops))
    }

    #[tool(
        description = "Draw ONE shape/mark on a cel — the single-op form of doc_batch (use doc_batch for many ops at once). `op` plus its flattened params: pencil{points,color,size?} (each point is a SEPARATE dab — use polyline or line to CONNECT points into a stroke) · line{x0,y0,x1,y1,color,size?} · rect{x0,y0,x1,y1,color,fill?,size?} · ellipse{cx,cy,rx,ry,color,fill?} · polyline{points,color,size?,closed?} · polygon{points,color,fill?} · stroke{points,color,width?,aa?,snap?} (aa=true softens edges with PARTIAL-ALPHA pixels — set aa=false on a locked palette or before a GIF export, whose alpha is 1-bit) · fill{x,y,color,tolerance?} · gradient{stops,kind?,x0,y0,x1,y1,…} · scatter{colors,x0,y0,x1,y1,density?,seed?,size?} · noise{stops,x0,y0,x1,y1,kind?,scale?,…} · text{x,y,text,color,size?} · fill_cel{color} · box_iso{cx,cy,s,ht,color,light_right?} (shaded isometric cuboid — the hard-surface form primitive: crates, blocks, dice) · panel{x,y,w,h,fill,border?,bevel?} (HUD/UI box: filled body + border + inner bevel; pair with op=text for labels). All also accept opacity and blend_mode. Honours an active doc_select."
    )]
    async fn doc_draw(&self, Parameters(p): Parameters<DocDraw>) -> CallToolResult {
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
                    rgba(&b.color),
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
                    rgba(&pn.fill),
                    pn.border.as_deref().map(rgba).unwrap_or([20, 20, 28, 255]),
                    pn.bevel.unwrap_or(true),
                ))
            }
            _ => res(studio.doc_draw(&p.doc_id, p.layer, p.frame, &p.op, p.params)),
        }
    }

    #[tool(
        description = "Apply ONE transform/effect op that REWORKS existing pixels — the complement of doc_draw (which adds marks), single-op form of doc_batch. `op` plus its flattened params, grouped: **effects** blur{radius,region?} · outline{color,aa?} · drop_shadow{color,dx?,dy?,blur?,shadow_opacity?} · bevel{light,dark,depth?} · shade{light_dir?,steps?,mode?,ramp?,region?} · form{form,light_dir?,ramp?,strength?,region?} · dither{color_a,color_b,pattern?,density?,region?,only_existing?} · pixel_perfect{region?,color?} (thins 1px staircases on OUTLINES/lines — never run it over filled shapes, it shreds them); **transform** flip{horizontal?} · shift{dx?,dy?,wrap?} · symmetry{vertical?,horizontal?,keep_left?,keep_top?}; **colour** quantize{colors,max_colors?} · replace_color{from,to,tolerance?} · adjust{hue?,sat?,lum?,region?} · gradient_map{stops,region?} (remap luminance through colour stops, alpha kept). All also accept opacity/blend_mode and honour an active doc_select."
    )]
    async fn doc_fx(&self, Parameters(p): Parameters<DocFx>) -> CallToolResult {
        res(self
            .studio()
            .doc_fx(&p.doc_id, p.layer, p.frame, &p.op, p.params))
    }

    // -- world-class-art tools (the art-quality pass) --
    #[tool(
        description = "SEE a frame as an INLINE PNG (no separate file read) plus measured stats — the agent's primary and only eye for the canvas. mode: render | value/grayscale | bands | sat | hue | notan (3-value squint). grid + coords burn a pixel ruler into the upscale; onion ghosts neighbours; region crops; max_size makes a thumbnail; tile repeats the result N×N to check seamlessness; out_path also writes the PNG to a file. Stats report value min/max/mean/contrast and shadow/mid/light mass % (plus per-band coverage in bands/notan modes)."
    )]
    async fn doc_look(&self, Parameters(p): Parameters<DocLook>) -> CallToolResult {
        let opts = atelier_studio::LookOptions {
            scale: p.scale,
            region: try_res!(region(&p.region)),
            mode: p.mode.clone().unwrap_or_default(),
            bands: p.bands.unwrap_or(0),
            grid: p.grid.unwrap_or(false),
            coords: p.coords.unwrap_or(false),
            onion: p.onion.unwrap_or(false),
            max_size: p.max_size,
            tile: p.tile,
            out_path: p.out_path.clone(),
        };
        img_result(self.studio().look(&p.doc_id, p.frame.unwrap_or(0), &opts))
    }

    #[tool(
        description = "Document history for an all-destructive editor. action: save (snapshot the doc) | list | restore (roll back) | diff (regression deltas vs a snapshot: pixel/colour/contrast change, added/removed/recoloured) | prune. Snapshot before a risky op (form/quantize/fill/palette snap) and restore if it gets worse."
    )]
    async fn doc_checkpoint(&self, Parameters(p): Parameters<DocCheckpoint>) -> CallToolResult {
        res(self.studio().checkpoint(
            &p.doc_id,
            &p.action,
            p.label.as_deref(),
            p.checkpoint_id.as_deref(),
        ))
    }

    #[tool(
        description = "Art-director scorecard: the named pixel-art failure modes the agent can't see — orphan specks, un-AA'd jaggies (outer step corners), low contrast, per-form pillow-shading and mixed light direction, value-soup massing, and off-palette drift. Verdicts are conservative (ok|warn|info) with worst-offending cells so you can fix locally. Snapshot with doc_checkpoint first if acting on it."
    )]
    async fn doc_critique(&self, Parameters(p): Parameters<DocCritique>) -> CallToolResult {
        res(self.studio().critique(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            try_res!(region(&p.region)),
        ))
    }

    #[tool(
        description = "Graduated multi-tone dithering across a whole RAMP along an axis (h|v|radial) — master gradient shading, vs the two-colour `dither`. pattern bayer2/4/8 | checker | ign (blue-noise, no visible matrix grid). only_existing repaints just opaque pixels (shade existing art, keep alpha). Honours an active selection. Snap afterwards with doc_palette op=snap if it drifts."
    )]
    async fn doc_dither_ramp(&self, Parameters(p): Parameters<DocDitherRamp>) -> CallToolResult {
        res(self.studio().dither_ramp(
            &p.doc_id,
            p.layer,
            p.frame,
            try_res!(region(&p.region)),
            palette_list(&p.ramp),
            p.axis.as_deref().unwrap_or("v"),
            p.pattern.as_deref().unwrap_or("bayer4"),
            p.only_existing.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Every frame in ONE labelled inline grid (index + duration) — the animator's flip-test the agent can't otherwise do. onion=true ghosts each cell's previous frame under it (per-pair onion skin — judge spacing/overlap/popping from a single image). cols sets the grid width; scale upscales each frame."
    )]
    async fn doc_contact_sheet(
        &self,
        Parameters(p): Parameters<DocContactSheet>,
    ) -> CallToolResult {
        img_result(self.studio().contact_sheet(
            &p.doc_id,
            p.scale.unwrap_or(4),
            p.cols.unwrap_or(8),
            p.onion.unwrap_or(false),
        ))
    }

    #[tool(
        description = "The palette hub. `op`: generate (default) — synthesize a cohesive palette in OKLCh: a single shading ramp (scheme=\"mono\") or a multi-hue scheme (complementary|triadic|analogous|split|tetradic); `count` colours per ramp, `hue_shift` warms light/cools shadow, `sat_curve` (flat|arc|sat-in-shadow), `anchor_midtone` pins the base; returns ramps + flat palette + hex + evenness validation; `set_doc` locks it on a doc. set — lock explicit `colors` [[r,g,b(,a)]] on `doc_id`. snap — snap `doc_id`'s cel (or whole doc if layer/frame omitted) to its palette by perceptual nearest; `alpha` policy preserve|opaque|flatten (`cutoff`,`bg`), `palette` overrides. swap — recolour `from`→`to` across `doc_id` (optional layer/frame), updating the stored palette. report — colour-usage tally for `doc_id` (frame/layer/region, `dupe_threshold`). sync — broadcast one `palette` (or `from_doc`'s) across a document set (`ids` and/or `prefix`)."
    )]
    async fn doc_palette(&self, Parameters(p): Parameters<DocPalette>) -> CallToolResult {
        let studio = self.studio();
        match p.op.as_deref().unwrap_or("generate") {
            "generate" => {
                let Some(base) = p.base.as_deref() else {
                    return res(Err("doc_palette op=generate needs `base`".to_string()));
                };
                res(studio.palette(
                    rgba(base),
                    p.scheme.as_deref().unwrap_or("mono"),
                    p.count.unwrap_or(5),
                    p.value_lo,
                    p.value_hi,
                    p.hue_shift.unwrap_or(20.0),
                    p.sat_curve.as_deref().unwrap_or("arc"),
                    p.anchor_midtone.unwrap_or(false),
                    p.set_doc.as_deref(),
                ))
            }
            "set" => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=set needs `doc_id`".to_string()));
                };
                let Some(colors) = p.colors.as_ref() else {
                    return res(Err("doc_palette op=set needs `colors`".to_string()));
                };
                let colors: Vec<[u8; 4]> = colors.iter().map(|c| rgba(c)).collect();
                res(studio.doc_set_palette(doc_id, colors))
            }
            "swap" => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=swap needs `doc_id`".to_string()));
                };
                let (Some(from), Some(to)) = (p.from.as_ref(), p.to.as_ref()) else {
                    return res(Err("doc_palette op=swap needs `from` and `to`".to_string()));
                };
                let from: Vec<[u8; 4]> = from.iter().map(|c| rgba(c)).collect();
                let to: Vec<[u8; 4]> = to.iter().map(|c| rgba(c)).collect();
                res(studio.doc_palette_swap(doc_id, from, to, p.layer, p.frame))
            }
            "report" => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=report needs `doc_id`".to_string()));
                };
                res(studio.doc_palette_report(
                    doc_id,
                    p.frame,
                    p.layer,
                    try_res!(region(&p.region)),
                    p.dupe_threshold.unwrap_or(8),
                ))
            }
            "snap" => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=snap needs `doc_id`".to_string()));
                };
                let alpha = match alpha_snap(p.alpha.as_deref(), p.cutoff, p.bg.as_deref()) {
                    Ok(a) => a,
                    Err(e) => return res(Err(e)),
                };
                let r = studio.snap_palette(
                    doc_id,
                    p.layer,
                    p.frame,
                    p.palette.as_ref().map(|v| palette_list(v)),
                    alpha,
                );
                edited(r)
            }
            "sync" => {
                let palette = p
                    .palette
                    .as_ref()
                    .map(|v| v.iter().map(|c| rgba(c)).collect::<Vec<[u8; 4]>>());
                res(studio.doc_set_palette_sync(
                    p.ids.as_deref(),
                    p.prefix.as_deref(),
                    palette,
                    p.from_doc.as_deref(),
                ))
            }
            other => res(Err(format!(
                "doc_palette: unknown op '{other}' — use generate|set|snap|swap|report|sync"
            ))),
        }
    }

    #[tool(
        description = "Paint a whole region DECLARATIVELY from a character grid (the inverse of doc_dump_region): `legend` maps single characters to [r,g,b(,a)] colours or integer PALETTE INDICES, `rows` are pixel-row strings ('.'/' ' leave the pixel untouched). Emitting a sprite as a grid eliminates the absolute-coordinate failure class — prefer this over long pencil/rect sequences for detailed shapes. Verify by diffing against doc_dump_region. Returns painted/clipped counts — call doc_look to SEE the result. Honours an active selection."
    )]
    async fn doc_paint_grid(&self, Parameters(p): Parameters<DocPaintGrid>) -> CallToolResult {
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

    // -- part-layer rig: limb animation as transforms, not repaints --

    // -- reference subsystem: recreate-from-sample as a measurable loop --
    #[tool(
        description = "Reference workflow — recreate-from-sample as a measurable loop. `op`: set — attach the ORIGINAL reference (`path`, omit to clear) so compare/diff can score likeness; returns aspect-true fit suggestions. import — trace a source image cleaned onto a guide layer: `path`, `target_w` (required), optional `target_h`, `colors`, `dither`, `defringe`, `to_doc_palette`, `remove_bg`, `pin`; returns a text report — call doc_look to SEE it. analyze — decompose the reference (inline PNG): background coverage, a frequency-weighted SUBJECT palette to lock with doc_palette op=set, and the silhouette as a text grid; `path` analyzes an external file, `target_w` plans at a size. compare — SCORE a `frame` (run after every pass): inline side-by-side (mode=\"overlay\" ghosts the reference), silhouette IoU (≥0.80 reads), per-cell OKLab ΔE with worst cells as rects, and missing palette colours; `cells` sets the grid. diff — PER-PIXEL signed error map (heat PNG: red=too light, blue=too dark, green=wrong hue) plus the `top` worst pixels each with a fix direction."
    )]
    async fn doc_ref(&self, Parameters(p): Parameters<DocRefOp>) -> CallToolResult {
        let studio = self.studio();
        match p.op.as_str() {
            "set" => res(studio.set_reference(&p.doc_id, p.path.as_deref())),
            "import" => {
                let Some(path) = p.path.as_deref() else {
                    return res(Err("doc_ref op=import needs `path`".to_string()));
                };
                let Some(target_w) = p.target_w else {
                    return res(Err("doc_ref op=import needs `target_w`".to_string()));
                };
                let (layer, frame) = (p.layer.unwrap_or(0), p.frame.unwrap_or(0));
                let r = studio.import_clean(
                    &p.doc_id,
                    layer,
                    frame,
                    path,
                    target_w,
                    p.target_h,
                    p.colors.unwrap_or(16),
                    p.dither,
                    p.defringe.unwrap_or(false),
                    p.to_doc_palette.unwrap_or(false),
                    p.remove_bg.unwrap_or(false),
                    p.pin.as_ref().map(|v| palette_list(v)).unwrap_or_default(),
                );
                edited(r)
            }
            "analyze" => img_result(studio.ref_analyze(
                &p.doc_id,
                p.path.as_deref(),
                p.target_w,
                p.colors.unwrap_or(8),
            )),
            "compare" => img_result(studio.ref_compare(
                &p.doc_id,
                p.frame.unwrap_or(0),
                p.mode.as_deref().unwrap_or("side_by_side"),
                p.cells.unwrap_or(8),
            )),
            "diff" => {
                img_result(studio.diff_map(&p.doc_id, p.frame.unwrap_or(0), p.top.unwrap_or(20)))
            }
            other => res(Err(format!(
                "doc_ref: unknown op '{other}' — use set|import|analyze|compare|diff"
            ))),
        }
    }
}

/// Tools that only LOOK: they read a document, never change it and never write
/// an artifact, so replaying them rebuilds nothing.
///
/// This is an allowlist, and the default is deliberately the other way: an
/// unlisted tool gets journaled. A stray read in a recipe replays as a harmless
/// no-op, but a mutation missing from a recipe silently produces different art —
/// so when in doubt, record. Anything added here must be genuinely inert.
const READ_ONLY_TOOLS: &[&str] = &[
    // the eye and the audits it reports through
    "doc_look",
    "doc_info",
    "doc_critique",
    "doc_silhouette",
    "doc_dump_region",
    "doc_components",
    "doc_anim_audit",
    "doc_frame_diff",
    "doc_seam_report",
    "doc_contact_sheet",
    // library-level: not part of any one document's provenance
    "list_docs",
    "delete_doc",
];

/// True when this call is part of how the document got made, and so belongs in
/// its journal.
///
/// The hub tools are mixed — `doc_ref op=compare` is the see-and-fix loop's eye
/// while `op=import` paints a guide layer — so the op decides, not the tool. The
/// read ops are also the ones an agent calls most, which is exactly the noise a
/// recipe should not carry.
fn is_journaled(tool: &str, args: &Value) -> bool {
    if READ_ONLY_TOOLS.contains(&tool) {
        return false;
    }
    let op = args.get("op").and_then(Value::as_str);
    !matches!(
        (tool, op),
        ("doc_ref", Some("analyze" | "compare" | "diff"))
            | ("doc_palette", Some("report"))
            | ("doc_checkpoint", Some("list"))
    )
}

/// The document a call belongs to. `doc_create` names it in the result (the id
/// is minted there, not passed in); everything else carries `doc_id`.
fn journal_target(tool: &str, args: &Value, result: &CallToolResult) -> Option<String> {
    match tool {
        // The id is minted in the result, not passed in.
        "doc_create" => result_json(result)?
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string),
        // doc_export writes an external artifact; it does not define the
        // document's pixels, so it must NOT enter the per-document recipe —
        // replaying a rebuild would re-run the export against the author's
        // out_path (or fail-abort if that path is gone). The session recorder
        // still captures it (it keys off `is_journaled`, not this).
        "doc_export" => None,
        // op=generate locks its palette onto `set_doc`, carrying no `doc_id`;
        // without this the lock is silently dropped from the recipe and replay
        // rebuilds the document off-palette.
        "doc_palette" => args
            .get("doc_id")
            .or_else(|| args.get("set_doc"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => args
            .get("doc_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// The args a call is recorded with. `doc_create`'s minted id exists only in
/// its result; stamping it into the recorded args lets `atelier replay` remap
/// every later step's ids when a re-run mints a different one (the tool itself
/// ignores the extra field, and replay strips it before sending).
fn recorded_args(tool: &str, mut args: Value, target: Option<&str>) -> Value {
    if tool == "doc_create" {
        if let (Some(id), Some(obj)) = (target, args.as_object_mut()) {
            obj.insert("doc_id".into(), json!(id));
        }
    }
    args
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Atelier {
    /// Advertise the tool surface — all of it. It is small enough that hiding
    /// part of it behind a profile flag would cost more in confusion than it
    /// ever saved in context.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.advertised_tools()))
    }

    /// Hand-written so we can record each call before delegating to the
    /// `#[tool_router]`-generated dispatcher. This replicates the body the
    /// `#[tool_handler]` macro would otherwise emit (build a `ToolCallContext`,
    /// then `self.tool_router.call(...)`); because we define it, the macro skips
    /// its own. Application errors come back as Ok `{"error": ...}` payloads (see
    /// `res`), so we record only steps that actually succeeded — a failed step
    /// would break `atelier replay`, which fails fast on an error payload.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        // Snapshot tool name + raw args up front (the request is moved into the
        // dispatcher). args default to `{}` to match a recipe step's shape.
        // Snapshot name/args before the router consumes the request: both the
        // journal and the session recorder need them after the call returns.
        let tool = request.name.to_string();
        let args = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
        let recorder = self.recorder.clone();

        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await?;

        // A read rebuilds nothing, so it belongs in neither recipe. Both
        // recorders answer to the same question — what did it take to make
        // this? — so they share the one classifier.
        if !is_error_result(&result) && is_journaled(&tool, &args) {
            let target = journal_target(&tool, &args, &result);
            let args = recorded_args(&tool, args, target.as_deref());
            // The document's own journal: on by default, so every document is a
            // replayable recipe without anyone having to know to ask first.
            if let Some(id) = &target {
                self.studio().journal_append(id, &tool, &args);
            }
            // The session recorder (--record) stays opt-in and cross-document:
            // it captures a whole sitting, which per-document journals cannot
            // express.
            if let Some(recorder) = recorder {
                recorder.record(&tool, args);
            }
        }
        Ok(result)
    }

    /// List the browsable resources: structure JSON + frame-0 render per document.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            self.list_resource_specs(),
        ))
    }

    /// Read one resource by URI. Unknown URIs and missing documents become a
    /// `resource_not_found` error rather than a panic.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let target = parse_resource_uri(&request.uri).ok_or_else(|| {
            ErrorData::resource_not_found(format!("unknown resource uri: {}", request.uri), None)
        })?;
        let contents = self
            .fetch_resource(&request.uri, target)
            .map_err(|e| ErrorData::resource_not_found(e, None))?;
        Ok(ReadResourceResult::new(vec![contents]))
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.instructions = Some(
            "atelier: the pixel-art studio you can see — a headless editor where every \
             mark is a tool call and doc_look hands the frame back as an image. doc_create a \
             layered/animated document, then paint cels with doc_draw (one op: \
             line/rect/ellipse/fill/stroke/text/…) or doc_batch (many ops in one call). LOOK with doc_look \
             after every burst of edits — it returns the frame as an INLINE image plus \
             value stats (pass doc_look out_path when you also need the PNG written to a file). \
             Recreating from a reference image? doc_ref op=set FIRST, doc_ref op=analyze \
             to plan (subject palette + silhouette), optionally doc_ref op=import onto a \
             guide layer, then doc_ref op=compare after EVERY pass — it scores silhouette \
             IoU and per-cell colour ΔE against the reference so likeness is measured, \
             not remembered. \
             Audit before exporting: doc_critique (failure modes), doc_palette op=report, \
             doc_silhouette. Animate by duplicating frames (doc_frame op=add copy_from) and \
             repainting only what moves — there is no pose interpolation; doc_frame_diff and \
             doc_anim_audit check what changed and whether the timing reads. doc_checkpoint \
             save before risky ops (quantize, palette snap) — restore rolls back. Export with \
             doc_export (op=sheet|anim|tileset) / op=all. list_docs browses the library. \
             28 tools, all of them advertised — there is no profile to switch."
                .into(),
        );
        info
    }
}

/// Run over stdio (default transport). `record` enables session recording to a
/// recipe at that path.
pub async fn run(record: Option<std::path::PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let mut atelier = Atelier::new();
    if let Some(path) = record {
        atelier = atelier.with_recording(path);
    }
    let service = atelier.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Run as a networked MCP server over Streamable HTTP at `addr`, mounted at
/// `/mcp`. One shared studio backs all sessions (writes serialised by its Mutex).
/// `allowed_hosts` extends the loopback default for LAN/remote `Host` validation
/// (DNS-rebinding guard); pass the host(s) clients will use.
pub async fn run_http(
    addr: &str,
    allowed_hosts: Vec<String>,
    record: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };

    // Shared studio across all HTTP sessions.
    let studio = std::sync::Arc::new(std::sync::Mutex::new(Studio::new()));
    // One recorder shared across sessions so every call lands in one recipe.
    let recorder = record.map(Recorder::new);
    let mut config = StreamableHttpServerConfig::default();
    for h in allowed_hosts {
        if !config.allowed_hosts.contains(&h) {
            config.allowed_hosts.push(h);
        }
    }
    let factory = {
        let studio = studio.clone();
        let recorder = recorder.clone();
        move || {
            let mut atelier = Atelier::with_studio(studio.clone());
            atelier.recorder = recorder.clone();
            Ok(atelier)
        }
    };
    let service: StreamableHttpService<Atelier, LocalSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    eprintln!("atelier MCP listening on http://{local}/mcp");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_surface_is_the_size_the_docs_claim() {
        let n = Atelier::new().tool_router.list_all().len();
        // Written into README / tools.html (regen: make docs) / architecture.html.
        // Change the surface, update them in the same commit — this is the reminder.
        assert_eq!(n, 28, "tool count changed — update the docs");
        assert_eq!(
            Atelier::new().advertised_tools().len(),
            28,
            "every tool is advertised; there is no profile filter"
        );
        let instructions = Atelier::new().get_info().instructions.unwrap_or_default();
        assert!(
            instructions.contains("28 tools"),
            "get_info instructions drifted from the tool count"
        );
    }

    #[test]
    fn no_tool_description_points_at_a_tool_that_does_not_exist() {
        // A description is the model's only guide. Naming a tool that was
        // removed sends it to call something that will never answer — and the
        // count pin cannot see this, because the count is still right.
        let tools = Atelier::new().tool_router.list_all();
        let names: std::collections::HashSet<String> =
            tools.iter().map(|t| t.name.to_string()).collect();
        let mentioned = regex_lite_doc_tools(&tools);
        for (tool, referenced) in mentioned {
            assert!(
                names.contains(&referenced),
                "{tool}'s description names '{referenced}', which is not a tool"
            );
        }
    }

    /// Every `doc_*` token appearing in each tool's description AND its input
    /// schema (schemars copies doc comments into schema descriptions — a stray
    /// `///` above a param struct ships to the model just as surely as the
    /// `#[tool]` description does), paired with the tool it came from.
    /// Hand-rolled: a regex crate for one scan in one test is a dependency the
    /// tower does not need.
    fn regex_lite_doc_tools(tools: &[rmcp::model::Tool]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for t in tools {
            let schema = serde_json::to_string(&t.input_schema).unwrap_or_default();
            let d = format!("{} {schema}", t.description.as_deref().unwrap_or(""));
            for (i, _) in d.match_indices("doc_") {
                let tail: String = d[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                    .collect();
                // `doc_id` is a parameter name, not a tool.
                if tail == "doc_id" || tail == "doc_" {
                    continue;
                }
                out.push((t.name.to_string(), tail));
            }
        }
        out
    }

    #[test]
    fn no_param_struct_ships_a_top_level_schema_description() {
        // The `#[tool]` description is a tool's one prose surface; a doc
        // comment on the param struct rides into the schema's top-level
        // `description` behind its back (a blank line does NOT detach a `///`
        // — deleting a neighbouring struct can silently re-home its comment).
        for t in Atelier::new().tool_router.list_all() {
            assert!(
                !t.input_schema.contains_key("description"),
                "{}'s param struct carries a doc comment — the model would see \
                 it as a second, competing description: {:?}",
                t.name,
                t.input_schema.get("description")
            );
        }
    }

    #[test]
    fn the_shipped_skills_name_only_real_tools() {
        // The shipped skills (crates/atelier/skills) are the workflow guidance we
        // install for users and embed in `atelier agent`; they name tools
        // verbatim. Delete a tool and they rot silently — the same drift as a
        // stale description, one crate over.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../atelier/skills");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return; // skills are not present in a packaged crate; nothing to check
        };
        let names: std::collections::HashSet<String> = Atelier::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        let mut checked = 0;
        for e in entries.filter_map(Result::ok) {
            let skill = e.path();
            if skill.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&skill) else {
                continue;
            };
            checked += 1;
            for (i, _) in body.match_indices("doc_") {
                let tool: String = body[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                    .collect();
                if tool == "doc_id" || tool == "doc_" {
                    continue;
                }
                assert!(
                    names.contains(&tool),
                    "{} names '{tool}', which is not a tool",
                    skill.display()
                );
            }
        }
        assert!(
            checked >= 3,
            "expected the three shipped skills, saw {checked}"
        );
    }

    #[test]
    fn every_read_only_tool_is_a_real_tool() {
        let names: Vec<String> = Atelier::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for t in READ_ONLY_TOOLS {
            assert!(
                names.contains(&t.to_string()),
                "READ_ONLY_TOOLS names '{t}', which is not a tool — renamed or removed? \
                 A stale entry here silently drops a real call from every journal."
            );
        }
    }

    #[test]
    fn the_eye_is_never_journaled_but_the_hand_always_is() {
        // Reads rebuild nothing; replaying them is noise.
        for t in ["doc_look", "doc_info", "doc_critique", "doc_silhouette"] {
            assert!(!is_journaled(t, &json!({"doc_id": "d"})), "{t} is a read");
        }
        // Anything that marks the canvas has to be in the recipe.
        for t in [
            "doc_draw",
            "doc_batch",
            "doc_fx",
            "doc_create",
            "doc_export",
        ] {
            assert!(
                is_journaled(t, &json!({"doc_id": "d"})),
                "{t} builds the art"
            );
        }
    }

    #[test]
    fn hub_tools_are_classified_by_op_not_by_name() {
        // doc_ref is both the eye (compare) and the hand (import).
        assert!(!is_journaled("doc_ref", &json!({"op": "compare"})));
        assert!(!is_journaled("doc_ref", &json!({"op": "diff"})));
        assert!(is_journaled("doc_ref", &json!({"op": "import"})));
        assert!(is_journaled("doc_ref", &json!({"op": "set"})));
        // Same split inside doc_palette and doc_checkpoint.
        assert!(!is_journaled("doc_palette", &json!({"op": "report"})));
        assert!(is_journaled("doc_palette", &json!({"op": "set"})));
        assert!(!is_journaled("doc_checkpoint", &json!({"op": "list"})));
        assert!(is_journaled("doc_checkpoint", &json!({"op": "restore"})));
        // An unknown tool defaults to journaled: a missing mutation corrupts
        // the replay, a spurious step only wastes a line.
        assert!(is_journaled("doc_newly_added_tool", &json!({})));
    }

    #[test]
    fn doc_create_is_journaled_to_the_id_it_minted() {
        // The id is in the result, not the args — journaling by args alone
        // would file every doc_create under nothing.
        let created = CallToolResult::success(vec![Content::text(
            json!({"id": "sprite", "w": 8}).to_string(),
        )]);
        assert_eq!(
            journal_target("doc_create", &json!({"name": "sprite"}), &created).as_deref(),
            Some("sprite")
        );
        let drew = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        assert_eq!(
            journal_target("doc_draw", &json!({"doc_id": "sprite"}), &drew).as_deref(),
            Some("sprite")
        );
    }

    #[test]
    fn doc_create_records_the_minted_id_for_replay_remapping() {
        // A collision mints `sprite-2`; without the stamp, replay could not
        // tell which recorded id the later steps' `doc_id: "sprite"` meant.
        assert_eq!(
            recorded_args("doc_create", json!({"name": "sprite"}), Some("sprite-2")),
            json!({"name": "sprite", "doc_id": "sprite-2"})
        );
        // Every other tool records its args untouched.
        assert_eq!(
            recorded_args("doc_draw", json!({"doc_id": "sprite"}), Some("sprite")),
            json!({"doc_id": "sprite"})
        );
    }

    #[test]
    fn replay_fidelity_edges_are_journaled_correctly() {
        let ok = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        // doc_export writes an artifact, not document state — replaying a rebuild
        // must not re-run it, so it belongs to no document's recipe.
        assert!(is_journaled(
            "doc_export",
            &json!({"doc_id": "hero", "op": "sheet"})
        ));
        assert_eq!(
            journal_target("doc_export", &json!({"doc_id": "hero", "op": "sheet"}), &ok),
            None,
            "export must not enter the per-document journal"
        );
        // doc_palette op=generate locks a palette onto `set_doc`, carrying no
        // `doc_id`; the recipe must still capture it or replay rebuilds off-palette.
        assert_eq!(
            journal_target(
                "doc_palette",
                &json!({"op": "generate", "set_doc": "hero"}),
                &ok
            )
            .as_deref(),
            Some("hero")
        );
    }

    #[test]
    fn result_json_reads_past_a_leading_image() {
        // img_result puts the PNG first and the stats after it; taking
        // content[0] would miss the report entirely.
        let looked = CallToolResult::success(vec![
            Content::image("ZmFrZQ==".to_string(), "image/png".to_string()),
            Content::text(json!({"doc_id": "x"}).to_string()),
        ]);
        assert_eq!(
            result_json(&looked).unwrap().get("doc_id").unwrap(),
            &json!("x")
        );
    }

    #[test]
    fn recorder_appends_a_replayable_jsonl_recipe() {
        let path = std::env::temp_dir().join("atelier-rec-roundtrip.jsonl");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        rec.record("doc_create", json!({"name": "x", "width": 8, "height": 8}));
        rec.record("doc_draw", json!({"doc_id": "x", "op": "rect"}));

        // Whatever the recorder leaves on disk must parse through replay's own
        // parser — the two halves share this format or neither works.
        let src = std::fs::read_to_string(&path).expect("recipe file written");
        assert_eq!(src.lines().count(), 2, "one appended line per call");
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        assert_eq!(recipe.steps[0].args["width"], 8);
        assert_eq!(recipe.steps[1].args["op"], "rect");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_skips_failed_steps() {
        let path = std::env::temp_dir().join("atelier-rec-skip.json");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        // Mirror call_tool: record only when the result is not an error payload.
        let ok = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(j(
            json!({"id": "x"}),
        ))]);
        let err = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(j(
            json!({"error": "no such doc"}),
        ))]);
        assert!(!is_error_result(&ok));
        assert!(is_error_result(&err));
        if !is_error_result(&ok) {
            rec.record("doc_create", json!({"name": "x"}));
        }
        if !is_error_result(&err) {
            rec.record("doc_info", json!({"doc_id": "nope"}));
        }

        let src = std::fs::read_to_string(&path).expect("recipe file written");
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");
        assert_eq!(recipe.steps.len(), 1);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_creates_missing_parent_dir() {
        let base = std::env::temp_dir().join(format!("atelier-rec-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("a").join("b").join("recipe.json");
        let rec = Recorder::new(path.clone()); // must create a/b/
        rec.record("doc_create", json!({"name": "x"}));
        assert!(path.exists(), "recipe written into freshly created dirs");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resource_uri_parses_and_rejects() {
        assert_eq!(
            parse_resource_uri("atelier://doc/hero"),
            Some(ResourceTarget::Structure("hero".into()))
        );
        assert_eq!(
            parse_resource_uri("atelier://doc/hero/render"),
            Some(ResourceTarget::Render("hero".into()))
        );
        // Unknown scheme, empty id, extra segments, and bare prefixes -> None.
        assert_eq!(parse_resource_uri("file:///hero"), None);
        assert_eq!(parse_resource_uri("atelier://doc/"), None);
        assert_eq!(parse_resource_uri("atelier://doc//render"), None);
        assert_eq!(parse_resource_uri("atelier://doc/hero/extra"), None);
        assert_eq!(parse_resource_uri("atelier://doc/hero/render/x"), None);
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors exercise the 0/1/2 trailing-byte padding cases.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    /// Build an Atelier whose studio is rooted at a throwaway temp dir.
    fn temp_atelier(tag: &str) -> Atelier {
        let dir = std::env::temp_dir().join(format!("atelier-srv-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let studio = Studio::with_docs_dir(dir);
        Atelier::with_studio(std::sync::Arc::new(std::sync::Mutex::new(studio)))
    }

    #[test]
    fn fetch_returns_structure_json_and_png_blob() {
        let a = temp_atelier("fetch");
        a.studio().doc_create("Hero", 8, 8).unwrap();

        // Structure resource: JSON text carrying the document's dimensions.
        let s = a
            .fetch_resource(
                "atelier://doc/hero",
                ResourceTarget::Structure("hero".into()),
            )
            .expect("structure fetched");
        match s {
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                let v: Value = serde_json::from_str(&text).expect("valid json");
                assert_eq!(v["w"], 8);
                assert_eq!(v["id"], "hero");
            }
            other => panic!("expected text contents, got {other:?}"),
        }

        // Render resource: a base64 PNG blob (verify the magic bytes decode out).
        let r = a
            .fetch_resource(
                "atelier://doc/hero/render",
                ResourceTarget::Render("hero".into()),
            )
            .expect("render fetched");
        match r {
            ResourceContents::BlobResourceContents {
                blob, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("image/png"));
                // base64 of a PNG always starts with the "iVBOR" signature.
                assert!(blob.starts_with("iVBOR"), "not a PNG blob: {}", &blob[..8]);
            }
            other => panic!("expected blob contents, got {other:?}"),
        }
    }

    /// doc_look is the agent's only eye: an edit op acknowledges with text only.
    /// Returning a preview PNG per edit costs LLM clients tens of thousands of
    /// image tokens a call — and telling the model it got a preview when it
    /// didn't suppresses the doc_look it should make. Pin both halves of the
    /// contract: edits carry no image, visual tools still do.
    #[test]
    fn edit_results_carry_no_image_but_visual_results_do() {
        let has_image = |r: &CallToolResult| {
            r.content
                .iter()
                .any(|c| matches!(c.raw, rmcp::model::RawContent::Image(_)))
        };

        let edit = edited(Ok(json!({"ok": true})));
        assert!(
            !has_image(&edit),
            "edit ops must return text only — doc_look is the agent's eye"
        );

        // A 1x1 PNG stands in for any rendered frame.
        let png = vec![0x89, 0x50, 0x4E, 0x47];
        let visual = img_result(Ok((png, json!({"ok": true}))));
        assert!(
            has_image(&visual),
            "visual tools must still return the frame"
        );
    }

    /// A tool that promises an inline preview it no longer returns lies to the
    /// model: it may skip doc_look believing it already saw the art. No edit
    /// tool's description may advertise one.

    #[test]
    fn list_resource_specs_pairs_each_doc() {
        let a = temp_atelier("list");
        a.studio().doc_create("Hero", 8, 8).unwrap();
        let specs = a.list_resource_specs();
        let uris: Vec<&str> = specs.iter().map(|r| r.uri.as_str()).collect();
        assert!(uris.contains(&"atelier://doc/hero"));
        assert!(uris.contains(&"atelier://doc/hero/render"));
        // Both carry their advertised mime types.
        for r in &specs {
            assert!(r.mime_type.is_some());
        }
    }

    #[test]
    fn fetch_unknown_doc_errors() {
        let a = temp_atelier("missing");
        let err = a
            .fetch_resource(
                "atelier://doc/ghost",
                ResourceTarget::Structure("ghost".into()),
            )
            .unwrap_err();
        assert!(err.contains("no document"), "got: {err}");
    }
}
