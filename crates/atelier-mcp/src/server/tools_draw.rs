//! Painting tools: op dispatchers, declarative grid painting, stateless region
//! edits, and graduated dither ramps.

use super::params::*;
use super::{Atelier, edited, palette_list, region, res};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

#[tool_router(router = draw_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(description = "Apply one typed drawing operation to a cel.")]
    pub(crate) async fn doc_draw(&self, Parameters(mut p): Parameters<DocDraw>) -> CallToolResult {
        revive_legacy_params(&mut p.params);
        res(self
            .studio()
            .doc_draw(&p.doc_id, p.layer, p.frame, p.op.as_str(), p.params))
    }

    #[tool(description = "Apply one typed effect, transform, or colour operation to a cel.")]
    pub(crate) async fn doc_fx(&self, Parameters(mut p): Parameters<DocFx>) -> CallToolResult {
        revive_legacy_params(&mut p.params);
        res(self
            .studio()
            .doc_fx(&p.doc_id, p.layer, p.frame, p.op.as_str(), p.params))
    }

    #[tool(description = "Clear or move a rectangular cel region.")]
    pub(crate) async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        edited(
            self.studio()
                .doc_region(&p.doc_id, p.op, p.layer, p.frame, Some(p.rect), p.offset),
        )
    }

    #[tool(description = "Paint a region from character rows and a colour legend.")]
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

    #[tool(description = "Dither a colour ramp horizontally, vertically, or radially.")]
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
