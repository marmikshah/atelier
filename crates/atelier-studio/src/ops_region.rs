//! Stateless rectangular region operations.

use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::{RegionOp, Studio};

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
                0,
                0,
                [("x".into(), json!([255, 0, 0, 255]))]
                    .into_iter()
                    .collect(),
                vec!["xx".into()],
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
}
