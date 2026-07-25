//! Per-document spritesheet and animation exports.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{AnimationFormat, DEFAULT_EXPORT_SCALE, ExportOp, SheetMeta, Studio, export_scale};

fn ensure_parent(out_path: &str) -> Result<(), String> {
    if let Some(parent) = Path::new(out_path).parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    Ok(())
}

impl Studio {
    pub fn doc_export(
        &self,
        id: &str,
        op: ExportOp,
        out_path: &str,
        scale: Option<u32>,
        meta: Option<SheetMeta>,
        format: Option<AnimationFormat>,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let scale = export_scale(scale.unwrap_or(DEFAULT_EXPORT_SCALE));
        match op {
            ExportOp::Sheet => {
                if format.is_some() || tag.is_some() {
                    return Err("doc_export op=sheet accepts `meta`, not `format` or `tag`".into());
                }
                let (_dir, document) = self.open(id)?;
                ensure_parent(out_path)?;
                match meta.unwrap_or_default() {
                    SheetMeta::Atelier => document.export_sheet(Path::new(out_path), scale),
                    SheetMeta::Standard => document.export_sheet_std(Path::new(out_path), scale),
                }
            }
            ExportOp::Anim => {
                if meta.is_some() {
                    return Err("doc_export op=anim accepts `format` and `tag`, not `meta`".into());
                }
                match format.unwrap_or_default() {
                    AnimationFormat::Gif => self.doc_export_gif(id, out_path, scale, tag),
                    AnimationFormat::Apng => self.doc_export_apng(id, out_path, scale, tag),
                }
            }
        }
    }

    fn doc_export_gif(
        &self,
        id: &str,
        out_path: &str,
        scale: u32,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let (_dir, document) = self.open(id)?;
        ensure_parent(out_path)?;
        let frames = document.export_gif(Path::new(out_path), scale, tag)?;
        Ok(json!({"path": out_path, "frames": frames, "tag": tag}))
    }

    fn doc_export_apng(
        &self,
        id: &str,
        out_path: &str,
        scale: u32,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let (_dir, document) = self.open(id)?;
        ensure_parent(out_path)?;
        let frames = document.export_apng(Path::new(out_path), scale, tag)?;
        Ok(json!({"path": out_path, "frames": frames, "tag": tag}))
    }
}
