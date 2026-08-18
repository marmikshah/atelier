//! Painting tools: op dispatchers, declarative grid painting, stateless region
//! edits, and graduated dither ramps.

use super::params::*;
use super::{Atelier, edited, palette_list, region, res};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

#[tool_router(router = draw_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(description = "Apply one typed drawing operation across frame cels.")]
    pub(crate) async fn doc_draw(&self, Parameters(mut p): Parameters<DocDraw>) -> CallToolResult {
        revive_legacy_params(&mut p.params);
        res(self.studio().doc_draw(
            &p.doc_id,
            p.layer,
            p.frame,
            p.frame_to,
            p.op.as_str(),
            p.params,
        ))
    }

    #[tool(
        description = "Apply one typed effect, transform, or colour operation across frame cels."
    )]
    pub(crate) async fn doc_fx(&self, Parameters(mut p): Parameters<DocFx>) -> CallToolResult {
        revive_legacy_params(&mut p.params);
        res(self.studio().doc_fx(
            &p.doc_id,
            p.layer,
            p.frame,
            p.frame_to,
            p.op.as_str(),
            p.params,
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use atelier_studio::{DumpMode, FrameOp, Studio};
    use serde_json::json;

    #[tokio::test]
    async fn draw_and_fx_handlers_forward_inclusive_frame_ranges() {
        let root = std::env::temp_dir().join("atelier-mcp-draw-frame-range");
        let _ = std::fs::remove_dir_all(&root);
        let studio = Studio::with_docs_dir(root);
        let created = studio.doc_new("range", 2, 2).unwrap();
        let doc_id = created["doc_id"].as_str().unwrap();
        studio
            .doc_frame(doc_id, FrameOp::Add, None, None, None, None, Some(2))
            .unwrap();
        let atelier = Atelier::with_studio(studio.clone());

        let draw = atelier
            .doc_draw(Parameters(
                serde_json::from_value(json!({
                    "doc_id": doc_id,
                    "layer": 0,
                    "frame": 0,
                    "frame_to": 2,
                    "op": "fill_cel",
                    "color": [9, 8, 7, 255]
                }))
                .unwrap(),
            ))
            .await;
        assert_ne!(draw.is_error, Some(true));
        let draw = crate::server::result_json(&draw).unwrap();
        assert_eq!(draw["frames_targeted"], 3);
        assert_eq!(draw["pixels_changed"], 12);

        let fx = atelier
            .doc_fx(Parameters(
                serde_json::from_value(json!({
                    "doc_id": doc_id,
                    "layer": 0,
                    "frame": 0,
                    "frame_to": 2,
                    "op": "replace_color",
                    "from": [9, 8, 7, 255],
                    "to": [1, 2, 3, 255]
                }))
                .unwrap(),
            ))
            .await;
        assert_ne!(fx.is_error, Some(true));
        let fx = crate::server::result_json(&fx).unwrap();
        assert_eq!(fx["frames_targeted"], 3);
        assert_eq!(fx["pixels_changed"], 12);
        for frame in 0..=2 {
            assert_eq!(
                studio
                    .doc_dump_region(doc_id, frame, Some(0), Some((0, 0, 0, 0)), DumpMode::Hex)
                    .unwrap()["rows"][0],
                "#010203"
            );
        }
    }
}
