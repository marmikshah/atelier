//! Read-only canvas rendering, analysis, animation audits, and reference-image
//! comparison.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::*;
use super::{Atelier, img_result, opt_img_result, region, res, rgba};

#[tool_router(router = read_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(description = "Render and measure a frame using configurable analysis views.")]
    pub(crate) async fn doc_look(&self, Parameters(p): Parameters<DocLook>) -> CallToolResult {
        let opts = atelier_studio::LookOptions {
            scale: p.scale,
            region: region(p.region),
            mode: p.mode.unwrap_or_default(),
            bands: p.bands.unwrap_or(0),
            grid: p.grid.unwrap_or(false),
            coords: p.coords.unwrap_or(false),
            onion: p.onion.unwrap_or(false),
            max_size: p.max_size,
            tile: p.tile,
            out_path: p.out_path.clone(),
            bg: p.bg,
        };
        img_result(self.studio().look(&p.doc_id, p.frame.unwrap_or(0), &opts))
    }

    #[tool(description = "Return up to 4096 exact pixels as a symbol or hexadecimal grid.")]
    pub(crate) async fn doc_dump_region(
        &self,
        Parameters(p): Parameters<DocDumpRegion>,
    ) -> CallToolResult {
        res(self.studio().doc_dump_region(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            region(p.region),
            p.mode.unwrap_or_default(),
        ))
    }

    #[tool(description = "Report opaque bounds, fill ratio, and a silhouette grid.")]
    pub(crate) async fn doc_silhouette(
        &self,
        Parameters(p): Parameters<DocSilhouette>,
    ) -> CallToolResult {
        res(self.studio().doc_silhouette(
            &p.doc_id,
            p.frame.unwrap_or(0),
            p.layer,
            p.alpha_threshold.unwrap_or(1),
        ))
    }

    #[tool(description = "Measure up to 64 connected components and small specks.")]
    pub(crate) async fn doc_components(
        &self,
        Parameters(p): Parameters<DocComponents>,
    ) -> CallToolResult {
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

    #[tool(description = "Compare two frames or cels, with optional grid or overlay.")]
    pub(crate) async fn doc_frame_diff(
        &self,
        Parameters(p): Parameters<DocFrameDiff>,
    ) -> CallToolResult {
        opt_img_result(self.studio().doc_frame_diff(
            &p.doc_id,
            p.frame_a,
            p.frame_b,
            p.layer,
            region(p.region),
            p.grid.unwrap_or(false),
            p.render.unwrap_or_default(),
            p.out_path.as_deref(),
            p.scale.unwrap_or(4),
        ))
    }

    #[tool(description = "Measure horizontal or vertical tiling seams and mismatches.")]
    pub(crate) async fn doc_seam_report(
        &self,
        Parameters(p): Parameters<DocSeamReport>,
    ) -> CallToolResult {
        opt_img_result(self.studio().doc_seam_report(
            &p.doc_id,
            p.layer,
            p.frame.unwrap_or(0),
            p.axis.unwrap_or_default(),
            p.threshold.unwrap_or(0),
            p.out_path.as_deref(),
        ))
    }

    #[tool(description = "Audit animation loops, motion spacing, arcs, or timing.")]
    pub(crate) async fn doc_anim_audit(
        &self,
        Parameters(p): Parameters<DocAnimAudit>,
    ) -> CallToolResult {
        res(self.studio().doc_anim_audit(
            &p.doc_id,
            p.tag.as_deref(),
            p.layer,
            p.mode,
            region(p.region),
        ))
    }

    #[tool(description = "Report common contour, lighting, value, and palette problems.")]
    pub(crate) async fn doc_critique(
        &self,
        Parameters(p): Parameters<DocCritique>,
    ) -> CallToolResult {
        res(self
            .studio()
            .critique(&p.doc_id, p.frame.unwrap_or(0), p.layer, region(p.region)))
    }

    #[tool(description = "Render all frames as a labelled inline contact sheet.")]
    pub(crate) async fn doc_contact_sheet(
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

    #[tool(description = "Set, analyze, compare, or diff a document reference image.")]
    pub(crate) async fn doc_ref(&self, Parameters(p): Parameters<DocRefOp>) -> CallToolResult {
        let studio = self.studio();
        match p.op {
            atelier_studio::ReferenceOp::Set => {
                res(studio.set_reference(&p.doc_id, p.path.as_deref()))
            }
            atelier_studio::ReferenceOp::Analyze => img_result(studio.ref_analyze(
                &p.doc_id,
                p.path.as_deref(),
                p.target_w,
                p.colors.unwrap_or(8),
            )),
            atelier_studio::ReferenceOp::Compare => img_result(studio.ref_compare(
                &p.doc_id,
                p.frame.unwrap_or(0),
                p.mode.unwrap_or_default(),
                p.cells.unwrap_or(8),
            )),
            atelier_studio::ReferenceOp::Diff => {
                img_result(studio.diff_map(&p.doc_id, p.frame.unwrap_or(0), p.top.unwrap_or(20)))
            }
        }
    }
}
