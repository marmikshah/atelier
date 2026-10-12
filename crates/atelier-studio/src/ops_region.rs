//! Stateless rectangular region operations.

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use atelier_core::document::Rescale;

use super::{RegionOp, RescaleMethod, RescaleOp, Studio};

impl Studio {
    /// Read one pixel — RGBA plus a `#rrggbbaa` value.
    ///
    /// This remains a test helper; callers should use `doc_dump_region`.
    #[cfg(test)]
    pub fn doc_get_pixel(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let p = match layer {
            Some(layer) => doc.get_pixel(layer, frame, x, y)?,
            None => {
                let image = doc.flatten(frame);
                if x < 0 || y < 0 || x as u32 >= image.width() || y as u32 >= image.height() {
                    [0, 0, 0, 0]
                } else {
                    image.get_pixel(x as u32, y as u32).0
                }
            }
        };
        Ok(json!({
            "x": x,
            "y": y,
            "rgba": p,
            "hex": crate::hex_rgba(&p),
            "layer": layer,
        }))
    }

    fn doc_move_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        rect: [i32; 4],
        offset: [i32; 2],
    ) -> Result<Value, String> {
        self.edit_with_ack(id, layer, frame, |document| {
            document.move_region(
                layer, frame, rect[0], rect[1], rect[2], rect[3], offset[0], offset[1],
            )
        })
    }

    fn doc_clear_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        rect: [i32; 4],
    ) -> Result<Value, String> {
        self.edit_with_ack(id, layer, frame, |document| {
            document.clear_region(layer, frame, rect[0], rect[1], rect[2], rect[3])
        })
    }

    /// Resize the whole document by one integer factor, keeping its
    /// proportions. `method` defaults to `nearest` going up and `majority`
    /// going down, the two that never introduce a colour.
    pub fn doc_rescale(
        &self,
        id: &str,
        op: RescaleOp,
        factor: u32,
        method: Option<RescaleMethod>,
    ) -> Result<Value, String> {
        let mode = match (op, method) {
            (RescaleOp::Up, None | Some(RescaleMethod::Nearest)) => Rescale::UpNearest,
            (RescaleOp::Up, Some(RescaleMethod::Epx)) => Rescale::UpEpx,
            (RescaleOp::Down, None | Some(RescaleMethod::Majority)) => Rescale::DownMajority,
            (RescaleOp::Down, Some(RescaleMethod::Nearest)) => Rescale::DownNearest,
            (RescaleOp::Down, Some(RescaleMethod::Average)) => Rescale::DownAverage,
            (RescaleOp::Up, Some(other)) => {
                return Err(format!(
                    "method '{other}' only scales down; op 'up' takes nearest or epx"
                ));
            }
            (RescaleOp::Down, Some(other)) => {
                return Err(format!(
                    "method '{other}' only scales up; op 'down' takes majority, nearest, or average"
                ));
            }
        };
        let (dir, mut doc) = self.open(id)?;
        let (from_w, from_h) = (doc.meta().w, doc.meta().h);
        doc.rescale(mode, factor)?;
        let (w, h) = (doc.meta().w, doc.meta().h);
        doc.save(&dir)?;
        Ok(serde_json::json!({
            "ok": true,
            "doc_id": id,
            "from": [from_w, from_h],
            "w": w,
            "h": h,
        }))
    }

    /// Apply a stateless rectangular operation.
    pub fn doc_region(
        &self,
        id: &str,
        op: RegionOp,
        layer: usize,
        frame: usize,
        rect: Option<[i32; 4]>,
        offset: Option<[i32; 2]>,
    ) -> Result<Value, String> {
        let rect = rect.ok_or_else(|| format!("doc_region op '{op}' needs `rect`"))?;
        match op {
            RegionOp::Clear => self.doc_clear_region(id, layer, frame, rect),
            RegionOp::Move => self.doc_move_region(
                id,
                layer,
                frame,
                rect,
                offset.ok_or("doc_region op 'move' needs `offset`")?,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-region-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn clear_and_move_are_self_contained() {
        let studio = studio("stateless");
        let created = studio.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        studio
            .doc_paint_grid(
                id,
                0,
                0,
                None,
                0,
                0,
                [("x".into(), json!([255, 0, 0, 255]))]
                    .into_iter()
                    .collect(),
                vec!["xx".into()],
                false,
            )
            .unwrap();

        studio
            .doc_region(id, RegionOp::Move, 0, 0, Some([0, 0, 1, 0]), Some([1, 1]))
            .unwrap();
        assert_eq!(
            studio.doc_get_pixel(id, Some(0), 0, 1, 1).unwrap()["rgba"],
            json!([255, 0, 0, 255])
        );

        studio
            .doc_region(id, RegionOp::Clear, 0, 0, Some([1, 1, 2, 1]), None)
            .unwrap();
        assert_eq!(
            studio.doc_get_pixel(id, Some(0), 0, 1, 1).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
    }

    fn dump(studio: &Studio, id: &str) -> image::RgbaImage {
        studio.open(id).unwrap().1.cel_full(0, 0)
    }

    #[test]
    fn rescale_resizes_canvas_and_cels_and_round_trips() {
        let studio = studio("rescale");
        let created = studio.doc_new("d", 4, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        studio
            .doc_paint_grid(
                id,
                0,
                0,
                None,
                1,
                0,
                [("x".into(), json!([255, 0, 0, 255]))]
                    .into_iter()
                    .collect(),
                vec!["x.".into(), ".x".into()],
                false,
            )
            .unwrap();
        let original = dump(&studio, id);

        let up = studio.doc_rescale(id, RescaleOp::Up, 3, None).unwrap();
        assert_eq!(
            (up["from"][0].as_u64(), up["w"].as_u64()),
            (Some(4), Some(12))
        );
        assert_eq!(up["h"], 6);
        let grown = dump(&studio, id);
        assert_eq!((grown.width(), grown.height()), (12, 6));
        assert_eq!(grown.get_pixel(3, 0).0, [255, 0, 0, 255]);
        assert_eq!(grown.get_pixel(5, 2).0, [255, 0, 0, 255]);
        assert_eq!(grown.get_pixel(6, 0).0[3], 0);

        // The default downscale undoes a nearest upscale exactly, after a reload.
        studio.doc_rescale(id, RescaleOp::Down, 3, None).unwrap();
        assert_eq!(dump(&studio, id), original);
        let info = studio.doc_info(id).unwrap();
        assert_eq!((info["w"].as_u64(), info["h"].as_u64()), (Some(4), Some(2)));
    }

    #[test]
    fn rescale_rejects_anything_that_would_change_proportions_or_mix_methods() {
        let studio = studio("rescale-reject");
        let created = studio.doc_new("d", 6, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let rescale = |op, factor, method| studio.doc_rescale(id, op, factor, method);

        let error = rescale(RescaleOp::Down, 4, None).unwrap_err();
        assert!(error.contains("does not divide evenly"), "got: {error}");
        assert!(rescale(RescaleOp::Up, 1, None).is_err());
        assert!(rescale(RescaleOp::Up, 17, None).is_err());
        let error = rescale(RescaleOp::Up, 3, Some(RescaleMethod::Epx)).unwrap_err();
        assert!(error.contains("2, 4, 8, or 16"), "got: {error}");
        assert!(rescale(RescaleOp::Up, 2, Some(RescaleMethod::Majority)).is_err());
        assert!(rescale(RescaleOp::Down, 2, Some(RescaleMethod::Epx)).is_err());
        let error = studio
            .doc_rescale(
                studio.doc_new("big", 4096, 8).unwrap()["doc_id"]
                    .as_str()
                    .unwrap(),
                RescaleOp::Up,
                2,
                None,
            )
            .unwrap_err();
        assert!(error.contains("pixel limit"), "got: {error}");

        // Every rejection left the canvas alone; the valid methods all work.
        let info = studio.doc_info(id).unwrap();
        assert_eq!((info["w"].as_u64(), info["h"].as_u64()), (Some(6), Some(4)));
        rescale(RescaleOp::Up, 4, Some(RescaleMethod::Epx)).unwrap();
        rescale(RescaleOp::Down, 2, Some(RescaleMethod::Nearest)).unwrap();
        rescale(RescaleOp::Down, 2, Some(RescaleMethod::Average)).unwrap();
        let info = studio.doc_info(id).unwrap();
        assert_eq!((info["w"].as_u64(), info["h"].as_u64()), (Some(6), Some(4)));
    }
}
