//! Structured recipes are maintained by normal editing, then replayed directly.
mod edit;
mod load;
mod parse;
mod syntax;
mod write;
use crate::{Studio, ToolName};
pub use atelier_core::source::Asset;
pub(crate) use edit::equivalent;
pub use load::Source;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use uuid::Uuid;

/// The current replay bundle within a working document.
pub const RECIPE_PATH: &str = "recipe/recipe.atelier";
pub(crate) fn stored_source(document: &Path) -> Result<Option<Source>, String> {
    let root = document.join("recipe");
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(m) if !m.file_type().is_dir() => Err("recipe must be a real directory".into()),
        Ok(_) => Source::load(&root).map(Some),
    }
}
pub(crate) fn copy_stored_source(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(source) = stored_source(src)? {
        source.write_to(&dst.join("recipe"))?;
    }
    Ok(())
}
pub(crate) fn stored_source_bytes(root: &Path) -> Result<u64, String> {
    Ok(stored_source(root)?.map_or(0, |s| s.encoded_bytes()))
}
impl Studio {
    /// The current structured replay file for a working document.
    pub fn recipe_path(&self, id: &str) -> Result<std::path::PathBuf, String> {
        if !Self::valid_id(id) {
            return Err("invalid document id".into());
        }
        Ok(self.doc_dir(id).join(RECIPE_PATH))
    }

    /// Replace the current recipe after a successful call, within the private
    /// store transaction. It describes the resulting artwork, never its history.
    pub fn save_recipe(&self, id: &str, call: Option<(ToolName, &Value)>) -> Result<(), String> {
        let dir = self.doc_dir(id);
        let document = atelier_core::document::Document::load(&dir)?;
        let legacy = dir.join(crate::JOURNAL_FILE);
        let remove_legacy = match fs::symlink_metadata(&legacy) {
            Ok(meta) if meta.file_type().is_file() => true,
            Ok(_) => return Err("legacy recipe.jsonl must be a regular file".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.to_string()),
        };
        let previous = stored_source(&dir)?;
        let source = edit::current(&document, &dir, previous.as_ref(), call)?;
        let staged = dir.join(format!(".recipe-{}", Uuid::new_v4()));
        let result = (|| {
            source.write_to(&staged)?;
            load::sync_tree(&staged)?;
            let destination = dir.join("recipe");
            if previous.is_some() {
                crate::atomic_rename::exchange(&staged, &destination).map_err(|e| e.to_string())?;
            } else {
                crate::atomic_rename::rename_no_replace(&staged, &destination)
                    .map_err(|e| e.to_string())?;
            }
            if remove_legacy {
                fs::remove_file(&legacy).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(staged);
        result
    }
    /// Compile a recipe and publish one complete working document.
    pub fn build_source(&self, source: &Source) -> Result<Value, String> {
        let mut document = source.compile()?;
        let _lock = self.lock_store_exclusive()?;
        self.cleanup_stale_transactions()?;
        let tx = self.begin_transaction(None)?;
        let id = Uuid::new_v4().to_string();
        let dir = tx.studio().doc_dir(&id);
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        source.write_to(&dir.join("recipe"))?;
        if let Some(path) = &source.asset.reference {
            fs::write(dir.join("reference.png"), &source.files[path]).map_err(|e| e.to_string())?;
        }
        document.save(&dir)?;
        tx.studio().set_document_revision(&id, 1)?;
        let outcome = tx.commit(&id)?;
        let mut result = json!({"ok":true,"doc_id":id,"recipe":self.doc_dir(&id).join(RECIPE_PATH),"layers":source.asset.layers.len(),"frames":source.asset.frames.len()});
        if let Some(warning) = outcome.warning() {
            result["warning"] = json!(warning);
        }
        Ok(result)
    }
    /// Remove legacy call history from a just-imported document. Normal tool
    /// editing deliberately does not invoke this one-time representation choice.
    pub fn compact_import(&self, id: &str) -> Result<(), String> {
        let (dir, doc) = self.open(id)?;
        let previous = stored_source(&dir)?.ok_or("missing imported source")?;
        let source = edit::imported(&doc, &previous)?;
        let stage = dir.join(format!(".recipe-{}", Uuid::new_v4()));
        let result = (|| {
            source.write_to(&stage)?;
            load::sync_tree(&stage)?;
            crate::atomic_rename::exchange(&stage, &dir.join("recipe")).map_err(|e| e.to_string())
        })();
        let _ = fs::remove_dir_all(stage);
        result
    }
    /// Install a migrated construction without changing the document id,
    /// pixels, metadata, revision or retained checkpoints. Verification precedes
    /// the atomic store transaction; old journals disappear only on success.
    pub fn install_recipe(&self, id: &str, source: &Source) -> Result<Value, String> {
        self.install_recipe_at(id, None, source)
    }
    /// Migrate a retained checkpoint without changing its label or pixels.
    pub fn install_checkpoint_recipe(
        &self,
        id: &str,
        checkpoint: &str,
        source: &Source,
    ) -> Result<Value, String> {
        if !crate::checkpoint::valid_checkpoint_id(checkpoint) {
            return Err("invalid checkpoint id".into());
        }
        self.install_recipe_at(id, Some(checkpoint), source)
    }
    fn install_recipe_at(
        &self,
        id: &str,
        checkpoint: Option<&str>,
        source: &Source,
    ) -> Result<Value, String> {
        let _lock = self.lock_store_exclusive()?;
        let (mut native_dir, mut current) = self.open(id)?;
        if let Some(cp) = checkpoint {
            native_dir = native_dir.join(".checkpoints").join(cp);
            current = atelier_core::document::Document::load(&native_dir)?;
        }
        if let Some(reference) = &source.asset.reference {
            let before = load::read_file(&native_dir.join("reference.png"), 256 * 1024 * 1024)?;
            if source.files.get(reference) != Some(&before) {
                return Err("migrated source changed the stored reference image".into());
            }
        }
        let checked = edit::equivalent(&current, &source.compile()?)?;
        let tx = self.begin_transaction(Some(id))?;
        let mut dir = tx.studio().doc_dir(id);
        if let Some(cp) = checkpoint {
            dir = dir.join(".checkpoints").join(cp);
        }
        let root = dir.join("recipe");
        if fs::symlink_metadata(&root).is_ok() {
            stored_source(&dir)?;
            fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
        }
        source.write_to(&root)?;
        let legacy = dir.join(crate::JOURNAL_FILE);
        match fs::symlink_metadata(&legacy) {
            Ok(meta) if meta.is_file() => fs::remove_file(legacy).map_err(|e| e.to_string())?,
            Ok(_) => return Err("legacy journal must be a regular file".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let outcome = tx.commit(id)?;
        let mut result = json!({"ok":true,"doc_id":id,"source":native_dir.join(RECIPE_PATH),"checkpoint":checkpoint,"rgba_pixels_verified":checked,"metadata_equal":true,"pixels_equal":true});
        if let Some(warning) = outcome.warning() {
            result["warning"] = json!(warning);
        }
        Ok(result)
    }
    /// Export a document's current recipe. Legacy native documents without a
    /// recipe are captured from their exact current pixels and metadata.
    pub fn export_recipe(&self, id: &str, destination: &Path) -> Result<Value, String> {
        let _lock = self.lock_store_shared()?;
        let (dir, document) = self.open(id)?;
        let saved = stored_source(&dir)?;
        let source = match saved {
            Some(source) => source,
            None => edit::current(&document, &dir, None, None)?,
        };
        let checked = edit::equivalent(&document, &source.compile()?)?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let stage = parent.join(format!(".atelier-migrate-{}", Uuid::new_v4()));
        let result = (|| {
            source.write_to(&stage)?;
            load::sync_tree(&stage)?;
            let output = if destination.extension().is_some_and(|e| e == "atelier") {
                if !source.files.is_empty() {
                    return Err("recipes with reference images need a directory destination".into());
                }
                crate::atomic_rename::rename_no_replace(&stage.join("recipe.atelier"), destination)
                    .map_err(|e| e.to_string())?;
                destination.to_path_buf()
            } else {
                crate::atomic_rename::rename_no_replace(&stage, destination)
                    .map_err(|e| e.to_string())?;
                destination.join("recipe.atelier")
            };
            fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| {
                    format!(
                        "source published at {}, but parent sync failed: {e}",
                        output.display()
                    )
                })?;
            Ok(
                json!({"ok":true,"source":output,"manifest_bytes":source.text.len(),"resource_bytes":source.files.values().map(Vec::len).sum::<usize>(),"cels":document.meta().cels.len(),"rgba_pixels_verified":checked,"metadata_equal":true,"pixels_equal":true}),
            )
        })();
        let _ = fs::remove_dir_all(stage);
        result
    }
}
#[cfg(test)]
mod tests;
