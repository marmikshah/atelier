//! Per-document spritesheet and animation exports.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::{self, *};
use super::{Atelier, res};

#[tool_router(router = export_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(
        description = "Export one document to a file. `op`: sheet (horizontal PNG plus JSON metadata; `meta` atelier|standard) or anim (`format` gif|apng and optional animation `tag`). `scale` defaults to 4. GIF/APNG alpha is 1-bit, so snap or flatten partial alpha first."
    )]
    pub(crate) async fn doc_export(
        &self,
        Parameters(mut p): Parameters<DocExport>,
    ) -> CallToolResult {
        params::revive_params(&mut p.params);
        res(self
            .studio()
            .doc_export(&p.doc_id, &p.op, &p.out_path, p.scale, &p.params))
    }
}
