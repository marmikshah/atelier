//! Painting tools: op dispatchers, declarative grid painting, stateless region
//! edits, and graduated dither ramps.

use super::params::{self, *};
use super::{Atelier, edited, palette_list, region, res};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

#[tool_router(router = draw_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(
        description = "Draw exactly ONE mark on one cel. Operations: pencil{points,color,size?}; line/rect{x0,y0,x1,y1,color,...}; ellipse{cx,cy,rx,ry,color,fill?}; polyline/polygon/stroke/curve{points,color,...}; stamp{points,tip,colorize?}; fill/bucket{x,y,color,tolerance?}; gradient{stops,...}; scatter{colors,bounds,...}; noise{stops,bounds,...}; text{x,y,text,color,size?}; fill_cel{color}; clear_cel. Each call accepts one op plus optional opacity, blend_mode, or erase."
    )]
    pub(crate) async fn doc_draw(&self, Parameters(mut p): Parameters<DocDraw>) -> CallToolResult {
        params::revive_params(&mut p.params);
        res(self
            .studio()
            .doc_draw(&p.doc_id, p.layer, p.frame, p.op.as_str(), p.params))
    }

    #[tool(
        description = "Apply exactly ONE transform or effect to one cel. Effects: blur{radius}; outline{color}; drop_shadow{color,...}; bevel{light,dark}; shade/form{...}; dither{color_a,color_b,...}; pixel_perfect{...}. Transforms: flip; shift{dx,dy,wrap?}; rotate{turns?}; scale{w,h,method?}; symmetry. Colour: quantize{colors,max_colors?}; replace_color{from,to}; adjust{hue?,sat?,lum?}; gradient_map{stops}. Each call accepts one op plus optional opacity, blend_mode, or erase."
    )]
    pub(crate) async fn doc_fx(&self, Parameters(mut p): Parameters<DocFx>) -> CallToolResult {
        params::revive_params(&mut p.params);
        res(self
            .studio()
            .doc_fx(&p.doc_id, p.layer, p.frame, p.op.as_str(), p.params))
    }

    #[tool(
        description = "Apply a self-contained rectangular edit to one cel. `op`: clear erases `rect` [x0,y0,x1,y1]; move shifts `rect` by `offset` [dx,dy]."
    )]
    pub(crate) async fn doc_region(&self, Parameters(p): Parameters<DocRegion>) -> CallToolResult {
        edited(
            self.studio()
                .doc_region(&p.doc_id, p.op, p.layer, p.frame, Some(p.rect), p.offset),
        )
    }

    #[tool(
        description = "Paint a whole region declaratively from a character grid. `legend` maps single characters to [r,g,b(,a)] colours or palette indices; `rows` are pixel-row strings and '.'/' ' leave pixels untouched. Returns painted/clipped counts."
    )]
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

    #[tool(
        description = "Graduated multi-tone dithering across a ramp along axis h, v, or radial. pattern: bayer2/4/8, checker, or ign. only_existing repaints opaque pixels while preserving transparency."
    )]
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
