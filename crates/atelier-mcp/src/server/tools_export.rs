//! File exports: one hub tool, op-dispatched — `sheet` / `anim` / `tileset`
//! per document, `all` / `atlas` across the whole library.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

use super::params::{self, *};
use super::{res, Atelier};

#[tool_router(router = export_router, vis = "pub(crate)")]
impl Atelier {
    #[tool(
        description = "Export to a file. Per-document `op`: sheet (horizontal spritesheet PNG + JSON meta — rects/durations/tags/palette; `meta`=standard writes the industry-standard hash sprite-JSON engines' existing importers parse instead) · anim (animated `format`=gif|apng, optional `tag` plays that animation in its direction) · tileset (slice a `tile_w`×`tile_h` grid → PNG + Tiled .tsx + JSON; canvas must divide evenly). Library-wide `op` (omit doc_id): all (one spritesheet PNG + JSON per document into `out_path` as a DIRECTORY) · atlas (pack EVERY frame of EVERY document into one atlas PNG + master JSON map — doc/frame/rect/duration — for slicing a whole game from one texture; `max_width` wraps the shelf packer, default 512). GIF/APNG alpha is 1-bit: a pixel is fully opaque or fully gone, so animation tuned with partial alpha (aa edges, per-op opacity) will jump at export — snap or flatten first. Shared: out_path, scale (sheet/anim/all/atlas 4, tileset 1)."
    )]
    pub(crate) async fn doc_export(
        &self,
        Parameters(mut p): Parameters<DocExport>,
    ) -> CallToolResult {
        params::revive_params(&mut p.params);
        let studio = self.studio();
        match p.op.as_str() {
            "all" => res(studio.export_all(&p.out_path, p.scale.unwrap_or(4))),
            "atlas" => {
                let max_width = p
                    .params
                    .get("max_width")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32)
                    .unwrap_or(512);
                res(studio.export_atlas(&p.out_path, p.scale.unwrap_or(4), max_width))
            }
            _ => {
                let Some(doc_id) = p.doc_id.as_deref() else {
                    return res(Err(format!(
                        "doc_export op={} needs `doc_id` (only op=all|atlas span the library)",
                        p.op
                    )));
                };
                res(studio.doc_export(doc_id, &p.op, &p.out_path, p.scale, &p.params))
            }
        }
    }
}
