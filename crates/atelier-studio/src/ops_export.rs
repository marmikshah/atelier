//! Per-document spritesheet and animation exports.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{DEFAULT_EXPORT_SCALE, Studio, export_scale};

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
        op: &str,
        out_path: &str,
        scale: Option<u32>,
        meta: Option<&str>,
        format: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let scale = export_scale(scale.unwrap_or(DEFAULT_EXPORT_SCALE));
        match op {
            "sheet" => {
                if format.is_some() || tag.is_some() {
                    return Err("doc_export op=sheet accepts `meta`, not `format` or `tag`".into());
                }
                let (_dir, document) = self.open(id)?;
                ensure_parent(out_path)?;
                match meta.unwrap_or("atelier") {
                    "atelier" => document.export_sheet(Path::new(out_path), scale),
                    "standard" => document.export_sheet_std(Path::new(out_path), scale),
                    other => Err(format!(
                        "doc_export op=sheet: unknown meta '{other}' — use atelier or standard"
                    )),
                }
            }
            "anim" => {
                if meta.is_some() {
                    return Err("doc_export op=anim accepts `format` and `tag`, not `meta`".into());
                }
                match format.unwrap_or("gif") {
                    "gif" => self.doc_export_gif(id, out_path, scale, tag),
                    "apng" => self.doc_export_apng(id, out_path, scale, tag),
                    other => Err(format!(
                        "doc_export op=anim: unknown format '{other}' — use gif or apng"
                    )),
                }
            }
            other => Err(format!(
                "unknown doc_export op '{other}' — use sheet or anim"
            )),
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
