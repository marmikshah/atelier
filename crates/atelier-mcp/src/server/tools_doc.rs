//! Document lifecycle + structure tools: create/list/info/delete, layers,
//! frames, tags, cels, document history (`doc_checkpoint`) and the palette hub.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::*;
use super::{alpha_snap, edited, palette_list, region, res, rgba, Atelier};

#[tool_router(router = doc_router, vis = "pub(crate)")]
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
        description = "Frame lifecycle + timing in one tool. `op`: add (append a frame; `copy_from` duplicates that frame's cels, `count` appends several at once, `duration_ms` default 100) · duration (set frame `frame`'s `duration_ms`) · insert (new frame at `frame`) · duplicate (`frame`) · delete (`frame`; last frame protected) · move (`frame`→`to_index`). Cels reindex and tag ranges remap. (Animation tags have their own tool: doc_add_tag.)"
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
            p.count,
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
                    try_res!(rgba(base)),
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
                let colors = try_res!(palette_list(colors));
                res(studio.doc_set_palette(doc_id, colors))
            }
            "swap" => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=swap needs `doc_id`".to_string()));
                };
                let (Some(from), Some(to)) = (p.from.as_ref(), p.to.as_ref()) else {
                    return res(Err("doc_palette op=swap needs `from` and `to`".to_string()));
                };
                let from = try_res!(palette_list(from));
                let to = try_res!(palette_list(to));
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
                    match &p.palette {
                        Some(v) => Some(try_res!(palette_list(v))),
                        None => None,
                    },
                    alpha,
                );
                edited(r)
            }
            "sync" => {
                let palette = match &p.palette {
                    Some(v) => Some(try_res!(palette_list(v))),
                    None => None,
                };
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
}
