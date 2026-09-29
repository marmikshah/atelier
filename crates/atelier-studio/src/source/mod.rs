//! Source bundles: bounded local files, migration, focused edits, and one-shot
//! publication into the working store. The compiler itself lives in core.

mod edit;
mod format;
mod load;
mod migrate;
mod procedure;

pub use atelier_core::source::{Asset, Part};
pub use edit::Edits;
pub use load::Source;
pub use migrate::MigrationMode;

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Studio, ToolName};

/// A source-backed journal starts from this bundled snapshot. The path is
/// fixed, relative to the journal, and never an arbitrary external pathname.
pub const JOURNAL_SOURCE: &str = "source/source.toml";

/// Read the managed starting state used by journals, checkpoints and archives.
pub(crate) fn stored_source(document: &Path) -> Result<Option<Source>, String> {
    let root = document.join("source");
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
        Ok(m) if !m.file_type().is_dir() => Err("stored source must be a real directory".into()),
        Ok(_) => Source::load(&root).map(Some),
    }
}

pub(crate) fn copy_stored_source(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(source) = stored_source(src)? {
        source.write_to(&dst.join("source"))?;
    }
    Ok(())
}

pub(crate) fn stored_source_bytes(root: &Path) -> Result<u64, String> {
    Ok(stored_source(root)?.map_or(0, |s| s.encoded_bytes()))
}

impl Studio {
    /// Compile a self-contained source snapshot and publish a fresh document.
    /// Later pixel tools append local overrides to its ordinary journal.
    pub fn build_source(&self, source: &Source) -> Result<Value, String> {
        let mut document = source.compile()?;
        let _lock = self.lock_store_exclusive()?;
        self.cleanup_stale_transactions()?;
        let tx = self.begin_transaction(None)?;
        let id = Uuid::new_v4().to_string();
        let dir = tx.studio().doc_dir(&id);
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        source.write_to(&dir.join("source"))?;
        if let Some(path) = &source.asset.reference {
            fs::write(dir.join("reference.png"), &source.files[path]).map_err(|e| e.to_string())?;
        }
        document.save(&dir)?;
        tx.studio().journal_append(
            &id,
            ToolName::DocNew,
            &json!({"doc_id": id, "source": JOURNAL_SOURCE}),
        )?;
        tx.studio().set_document_revision(&id, 1)?;
        let outcome = tx.commit(&id)?;
        let mut result = json!({"ok": true, "doc_id": id, "source_revision": source.revision,
            "layers": source.asset.layers.len(), "frames": source.asset.frames.len()});
        if let Some(warning) = outcome.warning() {
            result["warning"] = json!(warning);
        }
        Ok(result)
    }

    /// Migrate the current state and, when useful, its construction operations.
    /// The destination must not exist. Originals and the store remain intact.
    pub fn migrate_source(
        &self,
        id: &str,
        destination: &Path,
        mode: MigrationMode,
    ) -> Result<Value, String> {
        let _lock = self.lock_store_shared()?;
        let (dir, document) = self.open(id)?;
        let entries = self.journal(id)?;
        migrate::publish(&document, &dir, &entries, destination, mode)
    }
}

#[cfg(test)]
mod tests;
