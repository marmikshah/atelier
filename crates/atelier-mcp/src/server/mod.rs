//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON text content; studio
//! errors keep the {"error": ...} payload AND set `is_error` so MCP harnesses
//! flag the failure instead of treating it as a success.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, CreateMessageRequestParams, GetPromptRequestParams,
    GetPromptResult, ListPromptsResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, PromptMessage, PromptMessageRole, RawImageContent, RawResource,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, Role,
    SamplingMessage, SamplingMessageContent, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Value};

use atelier_studio::Studio;

mod params;
mod prompts;
mod recorder;
mod resources;
mod toolsdoc;

pub use toolsdoc::{tools_html, tools_text};

use params::*;
use prompts::{build_prompt, prompt_specs};
use recorder::Recorder;
use resources::{
    base64, parse_resource_uri, ResourceTarget, RESOURCE_RENDER_SCALE, VISION_CRITIQUE_MAX_TOKENS,
};

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

/// [r,g,b] -> RGB (drops alpha) for light/tint colours.
fn rgb3(v: &[i64]) -> [u8; 3] {
    [
        v.first().copied().unwrap_or(0) as u8,
        v.get(1).copied().unwrap_or(0) as u8,
        v.get(2).copied().unwrap_or(0) as u8,
    ]
}

/// {"head":[x,y],...} joint params -> the (x,y) map the rig tools take
/// (entries shorter than 2 are dropped; the rig validates the contract).
fn joints_map(
    joints: &std::collections::HashMap<String, Vec<i64>>,
) -> std::collections::HashMap<String, (i32, i32)> {
    joints
        .iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|(k, v)| (k.clone(), (v[0] as i32, v[1] as i32)))
        .collect()
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
    /// Advertise the full 63-tool surface instead of the core profile. Read
    /// from ATELIER_PROFILE once at construction (env is process-stable), and
    /// injectable so both profiles are unit-testable.
    full_profile: bool,
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
            full_profile: profile_full(),
        }
    }

    /// Override the advertised profile (tests exercise both without env; the
    /// `tools` HTML generator forces the full surface).
    pub(crate) fn with_profile(mut self, full: bool) -> Self {
        self.full_profile = full;
        self
    }

    /// The tools the active profile advertises: `CORE_TOOLS` by default, the
    /// full router with `full_profile`. Discovery filter only — `call_tool`
    /// still dispatches every tool, so recipes/replay reach the tail.
    fn advertised_tools(&self) -> Vec<rmcp::model::Tool> {
        let mut tools = self.tool_router.list_all();
        if !self.full_profile {
            tools.retain(|t| CORE_TOOLS.contains(&t.name.as_ref()));
        }
        tools
    }

    /// Enable session recording: every tool call is appended to a recipe at `path`.
    fn with_recording(mut self, path: std::path::PathBuf) -> Self {
        self.recorder = Some(Recorder::new(path));
        self
    }

    fn studio(&self) -> std::sync::MutexGuard<'_, Studio> {
        self.studio.lock().expect("studio lock poisoned")
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

    #[tool(
        description = "Audit N documents as ONE game — the set-level doc_critique. Resolve members by `ids` and/or id `prefix` (union), then report per-doc palette/value/scale/pivot stats plus set cohesion: palette union size + unlocked docs + cross-doc near-duplicate colours (OKLab ΔE), silhouette-height scale outliers vs the set median, the set value range, and docs missing pivots. Verdict is 'cohesive' or a list of actionable warnings (e.g. run doc_palette op=sync). This is how a pile of sprites becomes one game."
    )]
    async fn doc_set_audit(&self, Parameters(p): Parameters<DocSetAudit>) -> CallToolResult {
        res(self
            .studio()
            .doc_set_audit(p.ids.as_deref(), p.prefix.as_deref()))
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
        description = "Frame lifecycle + timing in one tool. `op`: add (append a frame; `copy_from` duplicates that frame's cels, `duration_ms` default 100) · duration (set frame `frame`'s `duration_ms`) · insert (new frame at `frame`) · duplicate (`frame`) · delete (`frame`; last frame protected) · move (`frame`→`to_index`). Cels reindex and tag ranges remap. (Pivots, collision boxes, tags and keyframe motion have their own tools.)"
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
        description = "Region + clipboard ops on a cel. `op`: copy (rect [x0,y0,x1,y1] → clipboard) · cut (copy + clear) · clear (erase the rect) · move (shift the rect by dx,dy in place) · paste (clipboard at x,y; `blend` source-over by default, false overwrites). Clipboard is cross-document. (External-image import is doc_stamp_image; lifting a part onto its own layer is doc_extract_to_layer.)"
    )]
    async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        res(self.studio().doc_region(
            &p.doc_id, &p.op, p.layer, p.frame, p.x0, p.y0, p.x1, p.y1, p.dx, p.dy, p.x, p.y,
            p.blend,
        ))
    }

    #[tool(
        description = "Place an external PNG into a cel at (x,y) — import bridge for AI-gen/real/Figma art. Optional `scale` (area-average when shrinking so a hi-res reference keeps its features, nearest when growing so pixel art stays crisp), `target_w` to scale to an exact width instead (wins over `scale`), and `rotate` (degrees). By default draws OVER existing content with `opacity`+`blend` (sub-sprite reuse, no layer-per-element); `replace`=true overwrites the whole cel. Honours an active selection. Returns a text report — call doc_look to SEE the result."
    )]
    async fn doc_stamp_image(&self, Parameters(p): Parameters<DocStampImage>) -> CallToolResult {
        let studio = self.studio();
        let r = studio.doc_stamp_image(
            &p.doc_id,
            p.layer,
            p.frame,
            p.x.unwrap_or(0),
            p.y.unwrap_or(0),
            &p.png_path,
            p.scale.unwrap_or(1.0),
            p.target_w,
            p.rotate.unwrap_or(0.0),
            p.opacity.unwrap_or(255),
            p.blend.as_deref().unwrap_or("normal"),
            p.replace.unwrap_or(false),
        );
        edited(r)
    }

    #[tool(
        description = "Generate the deterministic 16-tile Wang/blob terrain set from a source doc: frame 0's layer 0 holds the INNER material, layer 1 the OUTER (top-left N×N of each is sampled). Creates a NEW document <id>-wang (canvas 4N×4N) holding all 16 corner combinations in a 4×4 grid (tile index = NE,SE,SW,NW corner bits); each set bit fills a quarter-disc (radius N/2) at that corner, adjacent set corners connect along their shared edge. Returns the new doc's structure + id."
    )]
    async fn doc_wang_tiles(&self, Parameters(p): Parameters<DocWangTiles>) -> CallToolResult {
        res(self.studio().wang_tiles(&p.doc_id, p.n))
    }

    #[tool(
        description = "TRUE 9-slice: author a panel ONCE (any style — bevels, rounded corners, ornate borders), then emit it at ANY size. The `src` rect is cut into a 3×3 grid by `inset`: corners copy verbatim, edges and centre tile (default) or stretch to fill `dst`. Transparent source pixels are skipped, so rounded panels keep their shape. The dialog-box / button / HUD-frame workhorse: draw one 12×12 panel, stamp every UI size from it."
    )]
    async fn doc_nine_slice(&self, Parameters(p): Parameters<DocNineSlice>) -> CallToolResult {
        let rect = |v: &Vec<i64>| -> Result<(i32, i32, i32, i32), String> {
            if v.len() != 4 {
                return Err("rect must be [x, y, w, h]".into());
            }
            Ok((v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32))
        };
        let (src, dst) = match (rect(&p.src), rect(&p.dst)) {
            (Ok(s), Ok(d)) => (s, d),
            (Err(e), _) | (_, Err(e)) => return res(Err(e)),
        };
        res(self.studio().nine_slice(
            &p.doc_id,
            p.layer,
            p.frame,
            p.src_layer.unwrap_or(p.layer),
            p.src_frame.unwrap_or(0),
            src,
            p.inset.unwrap_or(3) as i32,
            dst,
            p.mode.as_deref().unwrap_or("tile"),
        ))
    }

    #[tool(
        description = "Seeded PARTICLE EMITTER rendered to frames — sparks, embers, smoke, rain, magic motes. Particles spawn inside `region`, fly along `angle ± spread` at `speed` under `gravity`, fade + shrink over `life`, coloured birth→death along the ramp (auto-ramped from `color` if omitted). Fully deterministic in `seed` and phase-staggered so the animation LOOPS cleanly. Draws onto `layer` across `frames` (clearing each cel) and tags the range `emit` — put it on its own layer over the art. Export with doc_export op=anim tag=emit."
    )]
    async fn doc_emit(&self, Parameters(p): Parameters<DocEmit>) -> CallToolResult {
        if p.region.len() != 4 {
            return res(Err("region must be [x, y, w, h]".into()));
        }
        let ramp = p
            .ramp
            .as_ref()
            .map(|v| v.iter().map(|c| rgba(c)).collect::<Vec<[u8; 4]>>());
        res(self.studio().emit(
            &p.doc_id,
            p.layer,
            (
                p.region[0] as i32,
                p.region[1] as i32,
                p.region[2] as i32,
                p.region[3] as i32,
            ),
            p.frames.unwrap_or(8),
            p.count.unwrap_or(24),
            p.angle.unwrap_or(270.0),
            p.spread.unwrap_or(30.0),
            p.speed.unwrap_or(1.5),
            p.gravity.unwrap_or(0.0),
            p.life.unwrap_or(1.0),
            p.size.unwrap_or(2) as i32,
            p.seed.unwrap_or(1),
            rgba(&p.color),
            ramp,
        ))
    }

    #[tool(
        description = "Colour-vision-deficiency audit: simulate protanopia / deuteranopia / tritanopia over the flattened frame and report which of the art's distinct colour pairs — readable to typical vision — COLLAPSE under each simulation (OKLab ΔE falls below the readable floor). Returns an INLINE side-by-side strip (normal · protan · deutan · tritan) plus the collapsing pairs, so 'is my health bar readable to 8% of players?' is one call. Run before shipping UI/state colours."
    )]
    async fn doc_colorblind_check(
        &self,
        Parameters(p): Parameters<DocColorblindCheck>,
    ) -> CallToolResult {
        img_result(self.studio().doc_colorblind_check(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.scale.unwrap_or(2),
        ))
    }

    #[tool(
        description = "Generate the deterministic 47-tile BLOB autotile set — the full edge+corner bitmask family (the modern superset of the 16-corner Wang set). Same source contract as doc_wang_tiles: frame 0, layer 0 = INNER material, layer 1 = OUTER, top-left N×N sampled. Creates a NEW document <id>-blob (7N×7N, the 47 canonical neighbour masks in a 7×7 grid) and returns `masks` — the 8-bit neighbour mask per grid index (N=1 NE=2 E=4 SE=8 S=16 SW=32 W=64 NW=128) — so an engine autotiler maps straight onto the sheet. Export with doc_export op=tileset. See it in situ FIRST with doc_tilemap_assemble."
    )]
    async fn doc_autotile_set(&self, Parameters(p): Parameters<DocAutotileSet>) -> CallToolResult {
        res(self.studio().autotile_set(&p.doc_id, p.n))
    }

    #[tool(
        description = "Assemble a TILEMAP from a terrain mask — the in-situ test of an autotile set, and the only real one. `rows` = the map as strings (`#`/`1`/`x` = filled); every filled cell computes its 8-neighbour mask and renders straight from the source materials (layer 0 inner / layer 1 outer) with the same blob rules as doc_autotile_set, so what you see IS what the tile family produces in a real map. `outside` = filled (default: terrain continues past the map edge) | empty (borders get outlines). Creates a NEW document <id>-map — doc_look it to judge the terrain reads, then export."
    )]
    async fn doc_tilemap_assemble(
        &self,
        Parameters(p): Parameters<DocTilemapAssemble>,
    ) -> CallToolResult {
        let outside_filled = p.outside.as_deref().unwrap_or("filled") == "filled";
        res(self
            .studio()
            .tilemap_assemble(&p.doc_id, p.n, &p.rows, outside_filled))
    }

    #[tool(
        description = "Export to a file. Per-document `op`: sheet (horizontal spritesheet PNG + JSON meta — rects/durations/tags/pivots/boxes/palette; `meta`=standard writes the industry-standard hash sprite-JSON engines' existing importers parse instead — no pivots/boxes in that shape) · anim (animated `format`=gif|apng, optional `tag` plays that animation in its direction) · tileset (slice a `tile_w`×`tile_h` grid → PNG + Tiled .tsx + JSON; canvas must divide evenly). Library-wide `op` (omit doc_id): all (one spritesheet PNG + JSON per document into `out_path` as a DIRECTORY) · atlas (pack EVERY frame of EVERY document into one atlas PNG + master JSON map — doc/frame/rect/duration/pivot — for slicing a whole game from one texture; `max_width` wraps the shelf packer, default 512). Shared: out_path, scale (sheet/anim/all/atlas 4, tileset 1). For the Wang set use doc_wang_tiles."
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
        description = "Insert `steps` cross-faded DISSOLVE frames after frame `from`: every layer's pixels alpha-blend toward frame `to` (snapped to the locked palette), so in-betweens are semi-transparent double-exposures. ONLY for fades, FX dissolves, and impact flashes — NEVER pose/limb motion (limbs ghost instead of moving; use doc_keyframe_move or per-frame edits for that). Auto-checkpoints first; undo a bad tween with doc_checkpoint restore or doc_frame op=delete. Reindexes later cels and remaps tags."
    )]
    async fn doc_dissolve(&self, Parameters(p): Parameters<DocTween>) -> CallToolResult {
        let studio = self.studio();
        studio.auto_checkpoint(&p.doc_id, "dissolve");
        res(studio.doc_tween(
            &p.doc_id,
            p.from,
            p.to,
            p.steps.unwrap_or(1),
            p.duration_ms.unwrap_or(100),
        ))
    }

    #[tool(
        description = "Bloom/glow: blur a bright copy of the cel and composite it back through a light blend (`mode` screen/add) at `intensity`. `color` tints the glow (omit = the art's own colours). Honours an active selection."
    )]
    async fn doc_glow(&self, Parameters(p): Parameters<DocGlow>) -> CallToolResult {
        let color = p.color.as_ref().map(|c| rgba(c));
        // Default to snapping the bloom on-palette; `snap=false` keeps it soft.
        let snap = if p.snap != Some(false) {
            Some(atelier_core::document::AlphaSnap::Opaque(
                p.snap_cutoff.unwrap_or(64),
            ))
        } else {
            None
        };
        res(self.studio().doc_glow(
            &p.doc_id,
            p.layer,
            p.frame,
            color,
            p.radius.unwrap_or(2),
            p.intensity.unwrap_or(180),
            p.mode.as_deref().unwrap_or("screen"),
            snap,
        ))
    }

    #[tool(
        description = "Paint a RIM/edge light along the silhouette edges that FACE the light (`az`: 0=right, 90=down, 180=left, 270=up) — the edge-relative highlight that was otherwise hand-placed pixel by pixel. Estimates each edge pixel's outward normal and stamps `color` where it faces the light, `width` px thick, `falloff` tightens it. `dark=true` lights the away-facing edge instead (core/contact shadow). Topological — survives small canvases where a Fresnel term washes out. Honours an active selection."
    )]
    async fn doc_rim_light(&self, Parameters(p): Parameters<DocRimLight>) -> CallToolResult {
        res(self.studio().doc_rim_light(
            &p.doc_id,
            p.layer,
            p.frame,
            rgba(&p.color),
            p.az,
            p.width.unwrap_or(1),
            p.falloff.unwrap_or(1.5),
            p.dark.unwrap_or(false),
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "Cast a projected GROUND shadow from a caster silhouette — not a flat offset copy (that's doc_fx op=drop_shadow) but the caster flattened onto its contact row and sheared AWAY from the light, so a tall shape throws a long foreshortened shadow anchored at its feet. `az` = light azimuth (0=right, 90=down, 180=left, 270=up; pairs with doc_form_audit); `length` stretches it along the ground, `squash` 0..1 is how much height survives (0 = flat). With `receiver_layer` the shadow is painted onto that layer and clipped to its opaque pixels (lands on the ground only); omit to draw behind the caster on its own cel. `snap` keeps it on-palette."
    )]
    async fn doc_cast_shadow(&self, Parameters(p): Parameters<DocCastShadow>) -> CallToolResult {
        res(self.studio().doc_cast_shadow(
            &p.doc_id,
            p.layer,
            p.frame,
            p.az.unwrap_or(135.0),
            p.length.unwrap_or(1.0),
            p.squash.unwrap_or(0.2),
            rgba(&p.color),
            p.opacity.unwrap_or(140),
            p.receiver_layer,
            p.snap.unwrap_or(true),
        ))
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

    #[tool(
        description = "Per-form shading audit — sees the #1 beginner failure the scalar reports can't. For each connected opaque form it infers the light direction (least-squares fit of perceptual lightness → `light_azimuth_deg`, `plane_fit_r2`) and flags PILLOW-SHADING (`pillow_corr`: brightness hugging the silhouette centre instead of a light direction). The summary reports `pillow_forms`, the shared `dominant_light_azimuth_deg` / `light_spread_deg`, and a verdict (ok | pillow-shading detected | inconsistent light direction). `min_area` skips specks (default 12). Deterministic; run before relight/form or before export."
    )]
    async fn doc_form_audit(&self, Parameters(p): Parameters<DocFormAudit>) -> CallToolResult {
        res(self.studio().doc_form_audit(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.min_area.unwrap_or(12),
        ))
    }

    #[tool(
        description = "Coarse coverage/composition heatmap: split the flattened frame into rows×cols cells (default 8×8), each reporting opaque fill 0..1 and mean luma (null if empty), plus the content bbox and its centre offset from the canvas centre. Check balance/placement/negative space."
    )]
    async fn doc_coverage_map(&self, Parameters(p): Parameters<DocCoverageMap>) -> CallToolResult {
        res(self.studio().doc_coverage_map(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.cols.unwrap_or(8),
            p.rows.unwrap_or(8),
        ))
    }

    // -- value & colour feedback (read-only analysis to judge values/colour) --
    #[tool(
        description = "Contrast as a number (the WCAG luminance-ratio formula). NOTE: WCAG is a text-on-UI legibility standard, not a sprite-vs-scene readability metric — treat the ratio as a value-separation HINT, not a pass/fail. mode=\"region\" compares the mean colour inside `region` [x0,y0,x1,y1] against its 4px surround → {ratio}. mode=\"palette\" scores every pair of the frame's distinct opaque colours (capped 16 — quantize first if more) and lists the lowest-contrast pairs. mode=\"one-bit\" thresholds luma to a pure B/W PNG (returns path + black/white %) — the real silhouette-readability check. `min_ratio` (default 1.5) only flags pairs below it."
    )]
    async fn doc_contrast_check(
        &self,
        Parameters(p): Parameters<DocContrastCheck>,
    ) -> CallToolResult {
        opt_img_result(self.studio().doc_contrast_check(
            &p.doc_id,
            p.frame.unwrap_or(0),
            &p.mode,
            try_res!(region(&p.region)),
            p.min_ratio.unwrap_or(1.5),
            p.threshold.unwrap_or(128),
            p.out_path.as_deref(),
        ))
    }

    #[tool(
        description = "Validate a colour ramp's craft from explicit `colors` [[r,g,b],...] (≥2) OR a `doc_id`'s locked palette (optional [start,end) `slice`). Returns monotonic_value, value_deltas, even_spacing (max step deviation ≤25% of mean), per-step hue_shift_deg (signed shortest-arc), hue_direction (warm-to-cool|cool-to-warm|mixed|none), sat_arc, and warnings (e.g. value reversals). Doc-independent."
    )]
    async fn doc_ramp_validate(
        &self,
        Parameters(p): Parameters<DocRampValidate>,
    ) -> CallToolResult {
        let colors = p
            .colors
            .as_ref()
            .map(|cs| cs.iter().map(|c| rgba(c)).collect::<Vec<_>>());
        let slice = p
            .slice
            .as_ref()
            .filter(|s| s.len() >= 2)
            .map(|s| (s[0], s[1]));
        res(self
            .studio()
            .doc_ramp_validate(colors, p.doc_id.as_deref(), slice))
    }

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
        description = "Tiling seam check: wrap-test a frame's far edge against the near edge it abuts when repeated. axis=\"horizontal\" tests left↔right, \"vertical\" top↔bottom, \"both\" runs each. Per axis returns {mismatches, max_delta, worst:[[x,y,delta] ≤10]}; any mismatch also returns an INLINE overlay PNG (frame dimmed, bad edge pixels red) so you see WHERE the seam pops. `threshold` is the max per-channel delta still counted a match (default 0). Verify seamless tiles."
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
        description = "Audit an animation loop. mode=\"seam\" diffs the wrap the loop actually plays and returns seam_score = changed/opaque plus the change_bbox naming WHERE the loop pops. mode=\"spacing\" tracks the opaque-mass CENTROID per played frame (per_frame_center/offset, total_drift, evenness; 0 = mechanically even); pass `region` to isolate one part (a swinging arm) over a static body. mode=\"arc\" returns the centroid trajectory, arc_residual (~0 = mechanical straight slide; higher = proper arc) and volume_cv (~0 = constant mass). mode=\"timing\" returns per-frame durations and flags uniform timing (reads mechanical — hold contacts ~1.5x). `tag` audits one tag (omit = whole timeline)."
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

    #[tool(
        description = "Eased multi-frame region motion. Reads the `region` [x0,y0,x1,y1] content from `from_frame` and stamps it (source-over) into every frame in (from_frame, to_frame] at an eased fraction of the total (dx,dy); to_frame gets the full offset. easing: linear/ease-in/ease-out/ease-in-out (cubic), bounce, overshoot (shoots past then settles), elastic (decaying oscillation). clear_source=true (default) clears the original rect in each destination frame so a moving limb leaves no stale copy. Frames must already exist (else error — doc_frame op=add first). Returns frames_touched + per-frame offsets."
    )]
    async fn doc_keyframe_move(
        &self,
        Parameters(p): Parameters<DocKeyframeMove>,
    ) -> CallToolResult {
        if p.region.len() < 4 {
            return res(Err("region must be [x0,y0,x1,y1]".into()));
        }
        let region = (p.region[0], p.region[1], p.region[2], p.region[3]);
        res(self.studio().doc_keyframe_move(
            &p.doc_id,
            p.layer,
            region,
            p.from_frame,
            p.to_frame,
            p.dx,
            p.dy,
            p.easing.as_deref().unwrap_or("linear"),
            p.clear_source.unwrap_or(true),
        ))
    }

    // -- pivots / palette (engine-ready sprites, cohesive colour) --
    #[tool(
        description = "Set a frame's anchor/pivot point [x,y] in document pixels (feet, weapon mount, …) so engines position the sprite. Omit `pivot` to clear it. Exported (scaled) in sheet/atlas JSON."
    )]
    async fn doc_set_pivot(&self, Parameters(p): Parameters<DocSetPivot>) -> CallToolResult {
        let pivot = match &p.pivot {
            Some(v) if v.len() >= 2 => Some([v[0], v[1]]),
            Some(_) => return res(Err("pivot must be [x,y]".into())),
            None => None,
        };
        res(self.studio().doc_set_pivot(&p.doc_id, p.frame, pivot))
    }

    #[tool(
        description = "Set a frame's collision boxes — body/hit/hurt rects an engine reads straight off the sheet. Each box is {name, kind:body|hit|hurt, rect:[x,y,w,h]}. Replaces the frame's whole set; pass boxes=[] to clear. Emitted (scaled) in sheet/atlas JSON next to pivot."
    )]
    async fn doc_set_frame_boxes(
        &self,
        Parameters(p): Parameters<DocSetFrameBoxes>,
    ) -> CallToolResult {
        let mut boxes = Vec::with_capacity(p.boxes.len());
        for b in &p.boxes {
            if b.rect.len() != 4 {
                return res(Err(format!("box '{}' rect must be [x,y,w,h]", b.name)));
            }
            boxes.push(atelier_core::document::BoxMeta {
                name: b.name.clone(),
                kind: b.kind.clone(),
                rect: [b.rect[0], b.rect[1], b.rect[2], b.rect[3]],
            });
        }
        res(self.studio().doc_set_frame_boxes(&p.doc_id, p.frame, boxes))
    }

    #[tool(
        description = "Apply MANY ordered drawing ops to one cel in a single call (fast headless editing). Each op is an object {\"op\":\"<name>\", ...} taking the same fields as the matching doc_draw/doc_fx op. Draw: pencil|line|rect|ellipse|polyline|polygon|stroke|fill|bucket|gradient|scatter|noise|text|fill_cel|clear_cel. FX: blur|outline|drop_shadow|bevel|shade|form|dither|pixel_perfect|flip|shift|symmetry|quantize|replace_color|adjust|gradient_map. Plus glow (batch only) taking the same fields as the matching tool. Add per-op \"opacity\" (0..255) and/or \"blend_mode\" to composite that op instead of overwriting. Honours an active doc_select."
    )]
    async fn doc_batch(&self, Parameters(p): Parameters<DocBatch>) -> CallToolResult {
        res(self.studio().doc_batch(&p.doc_id, p.layer, p.frame, p.ops))
    }

    #[tool(
        description = "Draw ONE shape/mark on a cel — the single-op form of doc_batch (use doc_batch for many ops at once). `op` plus its flattened params: pencil{points,color,size?} · line{x0,y0,x1,y1,color,size?} · rect{x0,y0,x1,y1,color,fill?,size?} · ellipse{cx,cy,rx,ry,color,fill?} · polyline{points,color,size?,closed?} · polygon{points,color,fill?} · stroke{points,color,width?,aa?,snap?} · fill{x,y,color,tolerance?} · gradient{stops,kind?,x0,y0,x1,y1,…} · scatter{colors,x0,y0,x1,y1,density?,seed?,size?} · noise{stops,x0,y0,x1,y1,kind?,scale?,…} · text{x,y,text,color,size?} · fill_cel{color} · box_iso{cx,cy,s,ht,color,light_right?} (shaded isometric cuboid — the hard-surface form primitive: crates, blocks, dice) · panel{x,y,w,h,fill,border?,bevel?} (HUD/UI box: filled body + border + inner bevel; pair with op=text for labels). All also accept opacity and blend_mode. Honours an active doc_select."
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
        description = "Apply ONE transform/effect op that REWORKS existing pixels — the complement of doc_draw (which adds marks), single-op form of doc_batch. `op` plus its flattened params, grouped: **effects** blur{radius,region?} · outline{color,aa?} · drop_shadow{color,dx?,dy?,blur?,shadow_opacity?} · bevel{light,dark,depth?} · shade{light_dir?,steps?,mode?,ramp?,region?} · form{form,light_dir?,ramp?,strength?,region?} · dither{color_a,color_b,pattern?,density?,region?,only_existing?} · pixel_perfect{region?,color?}; **transform** flip{horizontal?} · shift{dx?,dy?,wrap?} · symmetry{vertical?,horizontal?,keep_left?,keep_top?}; **colour** quantize{colors,max_colors?} · replace_color{from,to,tolerance?} · adjust{hue?,sat?,lum?,region?} · gradient_map{stops,region?} (remap luminance through colour stops, alpha kept). All also accept opacity/blend_mode and honour an active doc_select. (Bloom-with-snap stays on doc_glow.)"
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
        description = "Render the ACTIVE selection as a quick-mask overlay (selected art shown, the rest dimmed + magenta-tinted) so you never paint through an unseen mask. Returns an inline PNG + selected-pixel count and bbox."
    )]
    async fn doc_select_render(
        &self,
        Parameters(p): Parameters<DocSelectRender>,
    ) -> CallToolResult {
        img_result(self.studio().select_render(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.scale.unwrap_or(6),
        ))
    }

    #[tool(
        description = "Document history for an all-destructive editor. action: save (snapshot the doc) | list | restore (roll back) | diff (regression deltas vs a snapshot: pixel/colour/contrast change, added/removed/recoloured) | prune. Snapshot before a risky op (form/quantize/relight/fill) and restore if it gets worse."
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
        description = "Contiguous magic-wand → the active selection mask. Floods from (x,y) over same-colour pixels (perceptual OKLab tolerance by default; `conn8` for 8-connectivity). `layer` omitted samples the flattened composite. `mode` combines with the current selection: replace|add|subtract|intersect. The precondition for local recolour/re-shade; pair with doc_select_render to SEE the mask."
    )]
    async fn doc_select_wand(&self, Parameters(p): Parameters<DocSelectWand>) -> CallToolResult {
        res(self.studio().select_wand(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            p.x,
            p.y,
            p.tol.unwrap_or(16),
            p.conn8.unwrap_or(false),
            p.perceptual.unwrap_or(true),
            p.mode.as_deref().unwrap_or("replace"),
        ))
    }

    #[tool(
        description = "Selective anti-aliasing (selout): drop one opaque, mid-value pixel into each outer staircase notch of the silhouette so diagonals read smooth instead of as Bresenham stairs. Pass a `ramp` to keep the AA on-palette; `max_run` keeps genuine sharp corners crisp; `only_color`/`region` scope it. Returns the AA pixel count."
    )]
    async fn doc_smooth_edges(&self, Parameters(p): Parameters<DocSmoothEdges>) -> CallToolResult {
        res(self.studio().smooth_edges(
            &p.doc_id,
            p.layer,
            p.frame,
            p.ramp.map(|v| palette_list(&v)),
            p.max_run.unwrap_or(2),
            p.keep_square.unwrap_or(true),
            p.only_color.as_deref().map(rgba),
            try_res!(region(&p.region)),
        ))
    }

    #[tool(
        description = "Affine-transform a cel (or `region`) IN PLACE about its centre — the #1 missing primitive: rotate degrees, scale_x/scale_y, skew_x/skew_y degrees. method rotsprite (super-sampled, keeps clusters from shattering) | nearest. preserve_volume derives scale_y=1/scale_x for squash-and-stretch. snap_palette re-snaps the transform fringe; clear_source makes it a move. Returns placed bbox + pixel counts."
    )]
    async fn doc_transform_cel(
        &self,
        Parameters(p): Parameters<DocTransformCel>,
    ) -> CallToolResult {
        let sx = p.scale_x.unwrap_or(1.0);
        let sy = match (p.preserve_volume.unwrap_or(false), p.scale_y) {
            (true, None) if sx.abs() > 1e-6 => 1.0 / sx,
            _ => p.scale_y.unwrap_or(1.0),
        };
        res(self.studio().transform_cel(
            &p.doc_id,
            p.layer,
            p.frame,
            try_res!(region(&p.region)),
            p.rotate.unwrap_or(0.0),
            sx,
            sy,
            p.skew_x.unwrap_or(0.0),
            p.skew_y.unwrap_or(0.0),
            p.method.as_deref().unwrap_or("rotsprite"),
            p.snap_palette.unwrap_or(true),
            p.clear_source.unwrap_or(false),
        ))
    }

    #[tool(
        description = "Art-director scorecard: the named pixel-art failure modes the agent can't see — orphan specks, un-AA'd jaggies (outer step corners), low contrast, per-form pillow-shading and mixed light direction (via the doc_form_audit engine), value-soup massing, and off-palette drift. Verdicts are conservative (ok|warn|info) with worst-offending cells so you can fix locally. Snapshot with doc_checkpoint first if acting on it."
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
        description = "Multi-light form shading — key/fill/rim, the leap from one-direction `form` to PAINTED form. Reads the silhouette as a height field, derives surface normals (bulge = how domed), and lights it: key by azimuth (0=right,90=down,180=left,270=up) + elevation (0=grazing,90=head-on), an auto fill opposite the key, a Fresnel rim, and ambient. Output multiplies the base colour (hue preserved, light colour tints) or snaps to `ramp`. Honours an active selection; pass a region on multi-material sprites."
    )]
    async fn doc_relight(&self, Parameters(p): Parameters<DocRelight>) -> CallToolResult {
        let studio = self.studio();
        studio.auto_checkpoint(&p.doc_id, "relight");
        res(studio.relight(
            &p.doc_id,
            p.layer,
            p.frame,
            try_res!(region(&p.region)),
            p.key_azimuth.unwrap_or(315.0),
            p.key_elevation.unwrap_or(50.0),
            p.key_intensity.unwrap_or(1.0),
            p.key_color.as_deref().map(rgb3).unwrap_or([255, 255, 255]),
            p.fill_intensity.unwrap_or(0.25),
            p.fill_color.as_deref().map(rgb3).unwrap_or([120, 140, 200]),
            p.rim_intensity.unwrap_or(0.0),
            p.rim_color.as_deref().map(rgb3).unwrap_or([255, 255, 255]),
            p.ambient.unwrap_or(0.35),
            p.ambient_color
                .as_deref()
                .map(rgb3)
                .unwrap_or([120, 130, 170]),
            p.bulge.unwrap_or(0.8),
            p.ramp.map(|v| palette_list(&v)),
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
        description = "Add a faint, non-destructive guide layer to construct against, then delete with doc_layer op=delete. kind: thirds (rule-of-thirds) | grid (square, `spacing`) | iso (2:1 lattice) | vp (rays from a vanishing point `vp`=[x,y]). Pure construction scaffolding — perspective, iso, and composition."
    )]
    async fn doc_perspective_guide(
        &self,
        Parameters(p): Parameters<DocPerspectiveGuide>,
    ) -> CallToolResult {
        let vp = p.vp.as_ref().filter(|v| v.len() >= 2).map(|v| (v[0], v[1]));
        res(self.studio().perspective_guide(
            &p.doc_id,
            p.kind.as_deref().unwrap_or("thirds"),
            p.color.as_deref().map(rgba).unwrap_or([255, 0, 255, 130]),
            p.spacing.unwrap_or(8),
            vp,
        ))
    }

    #[tool(
        description = "Form-following selective outline (vs a flat black keyline): mode from_fill colours each silhouette edge from the fill it borders, shaded `steps` darker/lighter; light/dark bias the whole contour. `ramp` keeps it on-palette. The 'painted' contour that turns with the form."
    )]
    async fn doc_outline_selective(
        &self,
        Parameters(p): Parameters<DocOutlineSelective>,
    ) -> CallToolResult {
        res(self.studio().outline_selective(
            &p.doc_id,
            p.layer,
            p.frame,
            p.mode.as_deref().unwrap_or("from_fill"),
            p.ramp.map(|v| palette_list(&v)),
            p.steps.unwrap_or(2),
            try_res!(region(&p.region)),
        ))
    }

    #[tool(
        description = "Paint a procedural MATERIAL onto the opaque pixels of a cel from one base colour: metal (specular band + reflection), wood (grain), stone (mottle + speckle), water (ripples), cloth (weave), skin (soft gradient), glass (sheen + streak). Deterministic in `seed`; pass `ramp` to control the palette, or `region`/an active selection to clip it. Turns 6–10 blind calls into 'reads as the material'."
    )]
    async fn doc_material(&self, Parameters(p): Parameters<DocMaterial>) -> CallToolResult {
        let studio = self.studio();
        studio.auto_checkpoint(&p.doc_id, "material");
        res(studio.material(
            &p.doc_id,
            p.layer,
            p.frame,
            try_res!(region(&p.region)),
            &p.material,
            rgba(&p.color),
            p.seed.unwrap_or(1),
            p.ramp.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Translucency report — makes glass/glow/soft-FX alpha MEASURABLE instead of eyeballed. Over the flattened frame (or one layer, region-clipped): counts opaque/partial/transparent pixels, mean alpha of non-transparent pixels, a partial-alpha band histogram, and the bbox of the partial pixels."
    )]
    async fn doc_translucency_report(
        &self,
        Parameters(p): Parameters<DocTranslucency>,
    ) -> CallToolResult {
        res(self.studio().doc_translucency_report(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            try_res!(region(&p.region)),
        ))
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
    #[tool(
        description = "RIG step: cut a part (arm, head, tail) of a flat sprite onto its OWN new layer directly above, same coordinates — by `region` rect or the active selection (doc_select_wand the limb, then use_selection=true). frames=\"all\" cuts every frame so the part stays separated across the timeline. After extraction, animate the part with doc_keyframe_transform / doc_keyframe_move on ITS layer while the body stays untouched. Returns the new layer index."
    )]
    async fn doc_extract_to_layer(
        &self,
        Parameters(p): Parameters<DocExtractToLayer>,
    ) -> CallToolResult {
        res(self.studio().doc_extract_to_layer(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            try_res!(region(&p.region)),
            p.use_selection.unwrap_or(false),
            p.name,
            p.frames.as_deref() == Some("all"),
        ))
    }

    #[tool(
        description = "JOINT-rotate a part across frames in ONE call: reads `region` from from_frame, then every frame in (from_frame, to_frame] gets the part rotated by the eased fraction of `rot_deg` about `pivot` (the joint, e.g. the shoulder — document pixels) plus the eased (dx,dy), original region cleared first. THE replacement for blind per-frame limb repainting — 'swing the arm 30° about the shoulder over frames 1-4' is one call. Works best on a part layer from doc_extract_to_layer. Resampled pixels snap to the locked palette by default."
    )]
    async fn doc_keyframe_transform(
        &self,
        Parameters(p): Parameters<DocKeyframeTransform>,
    ) -> CallToolResult {
        if p.region.len() < 4 {
            return res(Err("region must be [x0,y0,x1,y1]".into()));
        }
        if p.pivot.len() < 2 {
            return res(Err("pivot must be [x,y]".into()));
        }
        res(self.studio().doc_keyframe_transform(
            &p.doc_id,
            p.layer,
            (p.region[0], p.region[1], p.region[2], p.region[3]),
            (p.pivot[0], p.pivot[1]),
            p.from_frame,
            p.to_frame,
            p.rot_deg.unwrap_or(0.0),
            p.dx.unwrap_or(0),
            p.dy.unwrap_or(0),
            p.easing.as_deref().unwrap_or("linear"),
            p.snap_palette.unwrap_or(true),
        ))
    }

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

    #[tool(
        description = "AI eye for FREE-FORM art — what doc_ref op=diff can't do without a reference. Renders the frame and asks the MCP HOST to run its own vision model over it (atelier ships no weights, makes no network call, holds no keys — the host samples; nothing leaves your client beyond that). Returns a structured critique: does the silhouette read, are proportions/anatomy right, is the value/colour structure working, what 3 fixes raise it most. `focus` weights one axis. Requires a host that advertises the `sampling` capability; errors clearly if it doesn't."
    )]
    async fn doc_critique_vision(
        &self,
        Parameters(p): Parameters<DocCritiqueVision>,
        peer: rmcp::Peer<RoleServer>,
    ) -> CallToolResult {
        // Fail fast if the client never advertised sampling — otherwise
        // create_message would block waiting for a response a non-sampling host
        // never sends.
        let supports_sampling = peer
            .peer_info()
            .map(|i| i.capabilities.sampling.is_some())
            .unwrap_or(false);
        if !supports_sampling {
            return res(Err(
                "vision critique unavailable — this MCP host does not advertise \
                the `sampling` capability, so it cannot run a vision model on the render. \
                (atelier ships no model and makes no network call; the host must sample.)"
                    .to_string(),
            ));
        }

        let frame = p.frame.unwrap_or(0);
        let scale = p.scale.unwrap_or(8).clamp(1, 16);
        // Render to owned bytes; the studio guard drops before the await below so
        // the lock is never held across the sampling round-trip.
        let png = match self.studio().render_png_bytes(&p.doc_id, frame, scale) {
            Ok(bytes) => bytes,
            Err(e) => return res(Err(e)),
        };

        let focus = match p.focus.as_deref().map(str::trim) {
            Some(f) if !f.is_empty() => format!(" Weight the critique toward: {f}."),
            _ => String::new(),
        };
        let system = "You are a master pixel-art director reviewing a single sprite/frame. \
            Be specific and honest — name what is wrong and where (use rough pixel regions \
            like 'upper-left', 'the head'), not vague praise. Pixel art is low-resolution on \
            purpose; judge it as such (silhouette readability, value grouping, palette \
            discipline, proportion/anatomy, appeal), not as a photo."
            .to_string();
        let instruction = format!(
            "Critique this pixel-art frame.{focus}\n\nReturn:\n\
             1. Reads-as: what the subject clearly is, or 'unclear' + why.\n\
             2. Silhouette & proportion: what's off.\n\
             3. Value & colour: flat/muddy/banding/off-palette issues.\n\
             4. Top 3 fixes, highest-impact first, each naming where on the canvas.\n\
             Keep it tight."
        );

        let image = SamplingMessageContent::Image(RawImageContent {
            data: base64(&png),
            mime_type: "image/png".to_string(),
            meta: None,
        });
        let text = SamplingMessageContent::text(instruction);
        let mut params = CreateMessageRequestParams::default();
        params.messages = vec![SamplingMessage::new_multiple(Role::User, vec![image, text])];
        params.system_prompt = Some(system);
        // Enough for a tight structured critique (reads-as, silhouette, value,
        // top-3 fixes); the host model is asked to be terse.
        params.max_tokens = VISION_CRITIQUE_MAX_TOKENS;

        // Sampling is deprecated upstream (SEP-2577) but is still the only
        // no-network way to borrow the HOST's vision model; keep using it
        // until rmcp removes it, then revisit doc_critique_vision's transport.
        #[allow(deprecated)]
        match peer.create_message(params).await {
            Ok(result) => {
                let critique = result
                    .message
                    .content
                    .iter()
                    .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string();
                if critique.is_empty() {
                    return res(Err(format!(
                        "the host's vision model returned no text (model: {})",
                        result.model
                    )));
                }
                res(Ok(json!({
                    "doc_id": p.doc_id,
                    "frame": frame,
                    "model": result.model,
                    "critique": critique,
                })))
            }
            Err(e) => res(Err(format!(
                "vision critique unavailable — the MCP host must advertise the `sampling` \
                 capability (it runs its own vision model; atelier sends nothing elsewhere). \
                 host: {e}"
            ))),
        }
    }

    #[tool(
        description = "Generate a radial FX animation (ring | disc | rays) expanding from (cx,cy) across `frames`, fading along a ramp, tagged `burst` — impacts, shockwaves, explosions as frames. Clears the target layer's cels. Export with doc_export op=anim tag=burst."
    )]
    async fn doc_burst(&self, Parameters(p): Parameters<DocBurst>) -> CallToolResult {
        res(self.studio().burst(
            &p.doc_id,
            p.layer.unwrap_or(0),
            p.cx,
            p.cy,
            p.frames.unwrap_or(6),
            p.max_radius,
            p.kind.as_deref().unwrap_or("ring"),
            rgba(&p.color),
            p.ramp.map(|v| palette_list(&v)),
        ))
    }

    #[tool(
        description = "Build a CONNECTED humanoid figure from named JOINT coordinates — reason in joint space (which you do well) instead of placing every silhouette vertex (which you don't). Each bone is drawn as a tapered capsule (a doc_draw op=stroke ribbon) sharing its endpoints, so the whole body is ONE connected silhouette by construction: no detached limbs, no blocky rect stacks. joints = {\"head\":[x,y],\"shoulder_l\":[x,y],\"shoulder_r\":[x,y],\"elbow_l\":...,\"elbow_r\":...,\"hand_l\":...,\"hand_r\":...,\"hip_l\":...,\"hip_r\":...,\"knee_l\":...,\"knee_r\":...,\"foot_l\":...,\"foot_r\":...} (chest/pelvis derived from the midpoints). Re-pose across frames by calling again with new joints — the base for non-wobbly animation. Tune limb_w/torso_w/head_r to the sprite size."
    )]
    async fn doc_figure(&self, Parameters(p): Parameters<DocFigure>) -> CallToolResult {
        let joints = joints_map(&p.joints);
        res(self.studio().figure(
            &p.doc_id,
            p.layer,
            p.frame,
            &joints,
            rgba(&p.color),
            p.limb_w.unwrap_or(3) as i32,
            p.torso_w.unwrap_or(6) as i32,
            p.head_r.unwrap_or(4) as i32,
            p.aa.unwrap_or(true),
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "GENERATE a side-view walk cycle from a base standing pose (the same 13 joints as doc_figure). Feet stride along a gait path (one planted, one swinging, half a cycle apart), knees/elbows are solved by 2-bone IK, arms counter-swing the legs, and the body bobs — each frame is drawn as the connected-capsule figure and the range is tagged \"walk\". The walk is generated from joints, not hand-painted frame-by-frame, so limbs never wobble or detach. Tune frames/stride/lift/bob/arm_swing. Export with doc_export op=anim tag=walk."
    )]
    async fn doc_walk(&self, Parameters(p): Parameters<DocWalk>) -> CallToolResult {
        let joints = joints_map(&p.joints);
        res(self.studio().walk(
            &p.doc_id,
            p.layer,
            &joints,
            p.frames.unwrap_or(8),
            p.stride.unwrap_or(10) as i32,
            p.lift.unwrap_or(4) as i32,
            p.bob.unwrap_or(1) as i32,
            p.arm_swing.unwrap_or(6) as i32,
            rgba(&p.color),
            p.limb_w.unwrap_or(3) as i32,
            p.torso_w.unwrap_or(6) as i32,
            p.head_r.unwrap_or(4) as i32,
            p.aa.unwrap_or(true),
            p.snap.unwrap_or(true),
        ))
    }

    #[tool(
        description = "GENERATE a full animation cycle for a named GAIT from one standing pose (the same 13 joints as doc_figure) — the moveset generator. `gait`: idle (breathing bob) | run (airborne stride, pumping arms, forward lean) | jump (crouch → rise+tuck → fall → landing absorb) | attack (lead-arm sweep with a lunge) | hurt (recoil and recover). Knees/elbows are solved by 2-bone IK and every frame is the connected-capsule figure, so limbs never wobble or detach. Amplitudes scale from the figure's own leg length × `intensity`, so presets fit any sprite size. Frames are tagged with the gait — one call per gait builds a whole character moveset from the SAME pose (walk has its own tool). Export each with doc_export op=anim tag=<gait>."
    )]
    async fn doc_pose_cycle(&self, Parameters(p): Parameters<DocPoseCycle>) -> CallToolResult {
        let joints = joints_map(&p.joints);
        res(self.studio().pose_cycle(
            &p.doc_id,
            p.layer,
            &joints,
            &p.gait,
            p.frames.unwrap_or(0),
            p.intensity.unwrap_or(1.0),
            rgba(&p.color),
            p.limb_w.unwrap_or(3) as i32,
            p.torso_w.unwrap_or(6) as i32,
            p.head_r.unwrap_or(4) as i32,
            p.aa.unwrap_or(true),
            p.snap.unwrap_or(true),
        ))
    }
}

/// The default ("core") tool profile — the 20 tools an agent actually reaches
/// for on a typical sprite / animation / recreate-from-reference task, chosen by
/// usage probability (the canonical workflows + the create→draw→animate→audit→
/// export spine). Everything else — heavy effects, the rig/tile/particle
/// subsystems, the niche audits, multi-doc set tools — stays behind
/// `ATELIER_PROFILE=full`. The profile filters only what `tools/list`
/// ADVERTISES; every tool still EXECUTES via call_tool (so `atelier replay` and
/// a flag-flip both reach the long tail). Keep it small: every advertised tool
/// taxes the model's attention on every call.
const CORE_TOOLS: &[&str] = &[
    // lifecycle + the eye
    "doc_create",
    "doc_info",
    "list_docs",
    "delete_doc",
    "doc_look",
    "doc_checkpoint",
    // draw / transform
    "doc_draw",
    "doc_batch",
    "doc_fx",
    "doc_paint_grid",
    // structure
    "doc_layer",
    "doc_frame",
    "doc_region",
    "doc_add_tag",
    // palette (op-dispatched: generate|set|snap|swap|report|sync)
    "doc_palette",
    // export
    "doc_export",
    // the light audit set the see-and-fix loop leans on
    "doc_critique",
    "doc_silhouette",
    "doc_components",
    // reference loop (op-dispatched: set|import|analyze|compare|diff)
    "doc_ref",
];

/// True when the full tool surface should be advertised (`ATELIER_PROFILE=full`).
fn profile_full() -> bool {
    std::env::var("ATELIER_PROFILE")
        .map(|v| v.eq_ignore_ascii_case("full"))
        .unwrap_or(false)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Atelier {
    /// Advertise the active tool profile: the 20 `CORE_TOOLS` by default, or the
    /// full surface when `ATELIER_PROFILE=full`. Discovery filter only — every
    /// tool still executes via `call_tool`, so recipes/replay reach the tail.
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
        let recording = self.recorder.as_ref().map(|r| {
            let args = request
                .arguments
                .clone()
                .map(Value::Object)
                .unwrap_or_else(|| json!({}));
            (r.clone(), request.name.to_string(), args)
        });
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await?;
        if let Some((recorder, tool, args)) = recording {
            if !is_error_result(&result) {
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

    /// List the packaged workflow prompts (the draw -> render -> audit loop).
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(prompt_specs()))
    }

    /// Fill one workflow prompt from its arguments. Unknown names become an
    /// `invalid_params` error.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        let (text, description) =
            build_prompt(&request.name, &request.arguments).ok_or_else(|| {
                ErrorData::invalid_params(format!("unknown prompt: {}", request.name), None)
            })?;
        let message = PromptMessage::new_text(PromptMessageRole::User, text);
        Ok(GetPromptResult::new(vec![message]).with_description(description))
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
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
             doc_silhouette. Animate by duplicating frames (doc_frame op=add copy_from) \
             and editing what moves — doc_keyframe_move for eased motion; doc_dissolve is \
             a dissolve, NOT pose interpolation. doc_checkpoint save before risky ops \
             (tween/form/quantize/relight) — restore rolls back. Export with \
             doc_export (op=sheet|anim|tileset) / op=all. list_docs browses the \
             library. This is the CORE tool profile (20 tools); the full 63-tool surface \
             (extra effects like relight/material/rim_light, rigging, audits, \
             perspective/wang/atlas) is available by restarting with \
             ATELIER_PROFILE=full."
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
    use super::prompts::PROMPTS;
    use super::recorder::iso_date;
    use super::*;

    #[test]
    fn recorder_writes_replayable_recipe() {
        let path = std::env::temp_dir().join("atelier-rec-roundtrip.json");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        rec.record("doc_create", json!({"name": "x", "width": 8, "height": 8}));
        rec.record("doc_info", json!({"doc_id": "x"}));

        // The file each rewrite leaves must parse through replay's own parser.
        let src = std::fs::read_to_string(&path).expect("recipe file written");
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");

        // Name is the file stem; description carries the recorded marker.
        assert_eq!(recipe.name, "atelier-rec-roundtrip");
        assert!(
            recipe.description.starts_with("recorded session "),
            "got: {}",
            recipe.description
        );
        // Steps round-trip in order with their args intact.
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        assert_eq!(recipe.steps[0].args["width"], 8);
        assert_eq!(recipe.steps[1].tool, "doc_info");
        assert_eq!(recipe.steps[1].args["doc_id"], "x");

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
    fn no_tool_description_promises_an_inline_preview_it_cannot_deliver() {
        let previewless = [
            "doc_paint_grid",
            "doc_stamp_image",
            "doc_ref",
            "doc_palette",
        ];
        for t in Atelier::new().with_profile(true).advertised_tools() {
            if !previewless.contains(&t.name.as_ref()) {
                continue;
            }
            let d = t.description.as_deref().unwrap_or("").to_lowercase();
            assert!(
                !d.contains("inline preview"),
                "{} advertises an inline preview but edits return text only",
                t.name
            );
        }
    }

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

    #[test]
    fn iso_date_is_well_formed() {
        let d = iso_date();
        let parts: Vec<&str> = d.split('-').collect();
        assert_eq!(parts.len(), 3, "got: {d}");
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        let (y, m, day): (i64, u32, u32) = (
            parts[0].parse().unwrap(),
            parts[1].parse().unwrap(),
            parts[2].parse().unwrap(),
        );
        assert!(y >= 2025, "year too small: {y}");
        assert!((1..=12).contains(&m), "month: {m}");
        assert!((1..=31).contains(&day), "day: {day}");
    }

    /// Build a `{name: value}` argument object the way a get_prompt request carries it.
    fn args(pairs: &[(&str, &str)]) -> Option<serde_json::Map<String, Value>> {
        Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                .collect(),
        )
    }

    #[test]
    fn prompt_substitutes_args_and_falls_back() {
        // Provided args (incl. an optional one) are substituted verbatim.
        let (text, _) = build_prompt(
            "pixel-sprite",
            &args(&[
                ("subject", "a knight"),
                ("size", "48"),
                ("palette_hint", "cool steel"),
            ]),
        )
        .expect("known prompt");
        assert!(text.contains("a knight"), "{text}");
        assert!(text.contains("48x48"), "{text}");
        assert!(text.contains("cool steel"), "{text}");

        // Omitted optional args fall back to the template default (no `{size}` leaks).
        let (text, _) = build_prompt("pixel-sprite", &args(&[("subject", "a slime")])).expect("ok");
        assert!(text.contains("a slime"), "{text}");
        assert!(text.contains("32x32"), "{text}");
        assert!(
            !text.contains('{') && !text.contains('}'),
            "unfilled slot: {text}"
        );

        // Unknown prompt name is rejected.
        assert!(build_prompt("nope", &None).is_none());
    }

    #[test]
    fn every_prompt_references_real_tools() {
        // The live tool list (the drift target).
        let names: std::collections::HashSet<String> = Atelier::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();

        for spec in PROMPTS {
            // Render with no args so we exercise every fallback branch too.
            let text = (spec.build)(&|_| None);
            for tool in spec.tools {
                // Each declared tool name must actually exist...
                assert!(
                    names.contains(*tool),
                    "prompt {} names missing tool {tool}",
                    spec.name
                );
                // ...and must appear verbatim in the rendered guidance.
                assert!(
                    text.contains(tool),
                    "prompt {} omits its declared tool {tool}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn core_profile_names_are_all_real_tools() {
        // A typo in CORE_TOOLS would silently drop a core tool from the default
        // profile — guard it against the live tool list.
        let names: std::collections::HashSet<String> = Atelier::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for t in CORE_TOOLS {
            assert!(names.contains(*t), "CORE_TOOLS names missing tool {t}");
        }
        // Core is a strict, smaller subset of the full surface.
        assert!(CORE_TOOLS.len() < names.len());
        // The advertised counts are exact and documented (README / tools.html (regen: make docs) /
        // ARCHITECTURE / .env.example / install.sh). If either changes, update
        // those surfaces in the same commit — this assertion is the reminder.
        assert_eq!(
            CORE_TOOLS.len(),
            20,
            "core profile size changed — update the docs"
        );
        assert_eq!(
            names.len(),
            63,
            "total tool count changed — update the docs"
        );
        // Both profiles, exercised without touching env.
        assert_eq!(
            Atelier::new().with_profile(false).advertised_tools().len(),
            20
        );
        assert_eq!(
            Atelier::new().with_profile(true).advertised_tools().len(),
            63
        );
        // The nearest doc surface is the in-file get_info instructions string —
        // it has drifted before; pin its counts too.
        let instructions = Atelier::new().get_info().instructions.unwrap_or_default();
        assert!(
            instructions.contains("20 tools"),
            "get_info instructions drifted from the core count"
        );
        assert!(
            instructions.contains("63-tool"),
            "get_info instructions drifted from the full count"
        );
    }

    #[test]
    fn prompt_specs_advertise_every_prompt() {
        let specs = prompt_specs();
        let names: Vec<&str> = specs.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"pixel-sprite"));
        assert!(names.contains(&"walk-cycle"));
        assert!(names.contains(&"seamless-tile"));
        // Each carries a description and its required `subject`/`character`/`material` arg.
        for p in &specs {
            assert!(p.description.is_some(), "{} has no description", p.name);
            let req = p
                .arguments
                .as_ref()
                .expect("args advertised")
                .iter()
                .filter(|a| a.required == Some(true))
                .count();
            assert!(req >= 1, "{} advertises no required arg", p.name);
        }
    }
}
