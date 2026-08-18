//! Document lifecycle + structure tools: new/list/info/delete, layers,
//! frames, tags, document history (`doc_checkpoint`) and the palette hub.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::*;
use super::{Atelier, alpha_snap, edited, palette_list, region, res, rgba};

#[tool_router(router = doc_router, vis = "pub(crate)")]
impl Atelier {
    // -- library --
    #[tool(
        description = "Create a persisted layered animation document and return its opaque `doc_id`."
    )]
    pub(crate) fn doc_new(&self, Parameters(p): Parameters<DocNew>) -> CallToolResult {
        res(self.studio().doc_new(&p.name, p.width, p.height))
    }

    #[tool(description = "List up to 100 documents, with filters and cursor pagination.")]
    pub(crate) fn list_docs(&self, Parameters(p): Parameters<ListDocs>) -> CallToolResult {
        res(self.studio().list_docs_page(
            p.prefix.as_deref(),
            p.contains.as_deref(),
            p.cursor.as_ref().map(|cursor| cursor.as_str()),
            p.limit.unwrap_or(50),
        ))
    }

    #[tool(description = "Get document structure: layers, frames, cels, and tags.")]
    pub(crate) fn doc_info(&self, Parameters(p): Parameters<DocRef>) -> CallToolResult {
        res(self.studio().doc_info(&p.doc_id))
    }

    #[tool(description = "Delete a document and all its files.")]
    pub(crate) fn delete_doc(&self, Parameters(p): Parameters<DocRef>) -> CallToolResult {
        res(self.studio().delete_doc(&p.doc_id))
    }

    // -- documents: editable layered/timeline sprites --
    #[tool(description = "Add, edit, reorder, duplicate, merge, or delete a layer.")]
    pub(crate) fn doc_layer(&self, Parameters(p): Parameters<DocLayer>) -> CallToolResult {
        res(self.studio().doc_layer(
            &p.doc_id, p.op, p.index, p.to_index, p.name, p.visible, p.opacity, p.blend,
        ))
    }

    #[tool(description = "Add, edit, reorder, duplicate, or delete animation frames.")]
    pub(crate) fn doc_frame(&self, Parameters(p): Parameters<DocFrame>) -> CallToolResult {
        res(self.studio().doc_frame(
            &p.doc_id,
            p.op,
            p.frame,
            p.copy_from,
            p.to_index,
            p.duration_ms,
            p.count,
        ))
    }

    #[tool(description = "Add a named animation frame range.")]
    pub(crate) fn doc_add_tag(&self, Parameters(p): Parameters<DocAddTag>) -> CallToolResult {
        res(self.studio().doc_add_tag(
            &p.doc_id,
            &p.name,
            p.from,
            p.to,
            p.direction.unwrap_or_default(),
        ))
    }

    #[tool(description = "Save, list, restore, or prune document checkpoints.")]
    pub(crate) fn doc_checkpoint(
        &self,
        Parameters(p): Parameters<DocCheckpoint>,
    ) -> CallToolResult {
        res(self.studio().checkpoint(
            &p.doc_id,
            p.action,
            p.label.as_deref(),
            p.checkpoint_id.as_deref(),
        ))
    }

    #[tool(description = "Generate, set, inspect, snap, or swap document palettes.")]
    pub(crate) fn doc_palette(&self, Parameters(p): Parameters<DocPalette>) -> CallToolResult {
        let studio = self.studio();
        match p.op.unwrap_or_default() {
            atelier_studio::PaletteOp::Generate => {
                let Some(base) = p.base.as_deref() else {
                    return res(Err("doc_palette op=generate needs `base`".to_string()));
                };
                res(studio.palette(
                    try_res!(rgba(base)),
                    p.scheme.unwrap_or_default(),
                    p.count.unwrap_or(5),
                    p.value_lo,
                    p.value_hi,
                    p.hue_shift.unwrap_or(20.0),
                    p.sat_curve.unwrap_or_default(),
                    p.anchor_midtone.unwrap_or(false),
                    p.set_doc.as_deref(),
                ))
            }
            atelier_studio::PaletteOp::Set => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=set needs `doc_id`".to_string()));
                };
                let Some(colors) = p.colors.as_ref() else {
                    return res(Err("doc_palette op=set needs `colors`".to_string()));
                };
                let colors = try_res!(palette_list(colors));
                res(studio.doc_set_palette(doc_id, colors))
            }
            atelier_studio::PaletteOp::Swap => {
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
            atelier_studio::PaletteOp::Report => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=report needs `doc_id`".to_string()));
                };
                res(studio.doc_palette_report(
                    doc_id,
                    p.frame,
                    p.layer,
                    region(p.region),
                    p.dupe_threshold.unwrap_or(8),
                ))
            }
            atelier_studio::PaletteOp::Snap => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err("doc_palette op=snap needs `doc_id`".to_string()));
                };
                let alpha = match alpha_snap(p.alpha.unwrap_or_default(), p.cutoff, p.bg.as_deref())
                {
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
        }
    }
}
