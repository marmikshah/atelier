//! Per-document spritesheet and animation exports.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::*;
use super::{Atelier, res};

#[tool_router(router = export_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(description = "Export a spritesheet with metadata or a GIF/APNG animation.")]
    pub(crate) fn doc_export(&self, Parameters(p): Parameters<DocExport>) -> CallToolResult {
        res(self.studio().doc_export(
            &p.doc_id,
            p.op,
            &p.out_path,
            p.scale,
            p.meta,
            p.format,
            p.tag.as_deref(),
        ))
    }
}
