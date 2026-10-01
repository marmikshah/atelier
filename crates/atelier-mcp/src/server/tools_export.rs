//! Per-document spritesheet and animation exports.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::*;
use super::{Atelier, res};

#[tool_router(router = export_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(
        description = "Get/set/clear pixel-font metadata. Distances are pixels; rect=[x,y,w,h] or null for blank glyphs. Baseline is measured down from each rect's top; bearing_x positions its left edge from the pen. Space/missing_glyph are indices; space must be blank and map 32. Export with doc_export op=font."
    )]
    pub(crate) fn doc_font(&self, Parameters(p): Parameters<DocFont>) -> CallToolResult {
        res(self.studio().doc_font(&p.doc_id, p.op, p.font))
    }

    #[tool(
        description = "Export a PNG spritesheet, GIF/APNG animation, or static TrueType pixel font (op=font). Sheets default to RGBA; color_mode=rgb requires fully opaque pixels. Font export uses stored doc_font metadata and requires glyph pixels to be transparent or fully solid."
    )]
    pub(crate) fn doc_export(&self, Parameters(p): Parameters<DocExport>) -> CallToolResult {
        res(self.studio().doc_export(
            &p.doc_id,
            p.op,
            &p.out_path,
            p.scale,
            p.meta,
            p.format,
            p.tag.as_deref(),
            p.color_mode,
        ))
    }
}
