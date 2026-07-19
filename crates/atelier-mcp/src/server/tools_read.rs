//! The eye: read-only canvas readers and audits (`doc_look`, text grids,
//! silhouette, components, frame diffs, seam/anim audits, critique, contact
//! sheet) plus the recreate-from-sample reference workflow. Nothing here
//! defines the art — it measures and reports it.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::*;
use super::{edited, img_result, opt_img_result, palette_list, region, res, rgba, Atelier};

#[tool_router(router = read_router, vis = "pub(crate)")]
impl Atelier {
    // -- world-class-art tools (the art-quality pass) --
    #[tool(
        description = "SEE a frame as an INLINE PNG (no separate file read) plus measured stats — the agent's primary and only eye for the canvas. mode: render | value/grayscale | bands | sat | hue | notan (3-value squint). grid + coords burn a pixel ruler into the upscale; onion ghosts neighbours; region crops; max_size makes a thumbnail; tile repeats the result N×N (N ≤ 16) to check seamlessness; bg mattes transparency (checker|dark|white — use it when judging white/light pixels, which vanish on a white viewer backdrop); out_path also writes the PNG to a file. Stats report value min/max/mean/contrast and shadow/mid/light mass % (plus per-band coverage in bands/notan modes)."
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
            bg: p.bg.clone(),
        };
        img_result(self.studio().look(&p.doc_id, p.frame.unwrap_or(0), &opts))
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
        let color = match &p.color {
            Some(c) => Some(try_res!(rgba(c))),
            None => None,
        };
        res(self.studio().doc_components(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.connectivity.unwrap_or(8),
            color,
            p.min_area.unwrap_or(1),
        ))
    }

    // -- animation & tiling feedback (read-only) --
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
                    match &p.pin {
                        Some(v) => try_res!(palette_list(v)),
                        None => Vec::new(),
                    },
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
