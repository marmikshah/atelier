//! Per-document spritesheet and animation exports.

use std::fs;
use std::path::Path;

use atelier_core::document::{FontMeta, PngColorMode};
use serde_json::{Value, json};

use super::{
    AnimationFormat, DEFAULT_EXPORT_SCALE, ExportOp, FontOp, SheetMeta, Studio, export_scale,
};

fn ensure_parent(out_path: &str) -> Result<(), String> {
    if let Some(parent) = Path::new(out_path).parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    Ok(())
}

impl Studio {
    /// Read, replace, or clear the document's editable pixel-font metadata.
    pub fn doc_font(&self, id: &str, op: FontOp, font: Option<FontMeta>) -> Result<Value, String> {
        if op != FontOp::Set && font.is_some() {
            return Err("doc_font accepts `font` only with op=set".into());
        }
        if op == FontOp::Get {
            return Ok(json!({"font":self.doc_info(id)?["font"]}));
        }
        let font = match op {
            FontOp::Set => Some(font.ok_or("doc_font op=set needs `font`")?),
            FontOp::Clear => None,
            FontOp::Get => unreachable!(),
        };
        let (dir, mut document) = self.open(id)?;
        document.set_font(font)?;
        document.save(&dir)?;
        Ok(json!({"font":document.meta().font}))
    }

    pub fn doc_export(
        &self,
        id: &str,
        op: ExportOp,
        out_path: &str,
        scale: Option<u32>,
        meta: Option<SheetMeta>,
        format: Option<AnimationFormat>,
        tag: Option<&str>,
        color_mode: Option<PngColorMode>,
    ) -> Result<Value, String> {
        match op {
            ExportOp::Sheet => {
                if format.is_some() || tag.is_some() {
                    return Err("doc_export op=sheet accepts `meta`, not `format` or `tag`".into());
                }
                let (_dir, document) = self.open(id)?;
                ensure_parent(out_path)?;
                let scale = export_scale(scale.unwrap_or(DEFAULT_EXPORT_SCALE));
                let color_mode = color_mode.unwrap_or_default();
                match meta.unwrap_or_default() {
                    SheetMeta::Atelier => document.export_sheet_with_color_mode(
                        Path::new(out_path),
                        scale,
                        color_mode,
                    ),
                    SheetMeta::Standard => document.export_sheet_std_with_color_mode(
                        Path::new(out_path),
                        scale,
                        color_mode,
                    ),
                }
            }
            ExportOp::Anim => {
                if meta.is_some() || color_mode.is_some() {
                    return Err(
                        "doc_export op=anim accepts `format` and `tag`, not `meta` or `color_mode`"
                            .into(),
                    );
                }
                match format.unwrap_or_default() {
                    AnimationFormat::Gif => self.doc_export_gif(
                        id,
                        out_path,
                        export_scale(scale.unwrap_or(DEFAULT_EXPORT_SCALE)),
                        tag,
                    ),
                    AnimationFormat::Apng => self.doc_export_apng(
                        id,
                        out_path,
                        export_scale(scale.unwrap_or(DEFAULT_EXPORT_SCALE)),
                        tag,
                    ),
                }
            }
            ExportOp::Font => {
                if scale.is_some()
                    || meta.is_some()
                    || format.is_some()
                    || tag.is_some()
                    || color_mode.is_some()
                {
                    return Err("doc_export op=font uses stored font metadata; scale, meta, format, tag, and color_mode apply to image exports".into());
                }
                let (_dir, document) = self.open(id)?;
                ensure_parent(out_path)?;
                document.export_font(Path::new(out_path))
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
