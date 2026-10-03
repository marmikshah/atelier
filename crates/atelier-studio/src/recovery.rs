//! Atomic replacement of unreadable saved document generations.

use std::fs;

use atelier_core::document::Document;
use serde_json::{Value, json};

use crate::{CommitOutcome, Studio, ToolName};

impl Studio {
    /// After a failed tool call, replace an unreadable document with a fresh
    /// generation. The caller must hold the store's exclusive lock. Healthy
    /// documents and missing IDs are untouched; directory links are refused.
    /// Verification and archive inspection never call this method.
    pub fn recover_document(&self, id: &str) -> Result<Option<Value>, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{id}'"));
        }
        let dir = self.doc_dir(id);
        match fs::symlink_metadata(&dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot inspect document '{id}': {error}")),
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(format!("document '{id}' must be a real directory"));
            }
            Ok(_) => {}
        }
        let validation = (|| -> Result<(), String> {
            let document = Document::load(&dir)?;
            if let Some(name) = &document.meta().reference {
                let path = super::reference::ref_path(&dir, name)?;
                if !fs::symlink_metadata(&path)
                    .map_err(|error| error.to_string())?
                    .file_type()
                    .is_file()
                {
                    return Err("stored reference must be a regular file".into());
                }
                super::open_bounded(&path)?;
            }
            self.document_revision(id)?;
            let journal = super::store::parse_journal_file(id, &dir.join(crate::JOURNAL_FILE))?;
            if journal.entries.is_empty() {
                return Err("document has no complete replay journal".into());
            }
            // A torn append cannot be extended without changing its meaning.
            if journal.torn_tail {
                return Err("document journal has an incomplete final line".into());
            }
            Ok(())
        })();
        let Err(reason) = validation else {
            return Ok(None);
        };

        // Valid current metadata remains authoritative for the named canvas.
        // Unreadable or unsupported metadata has no shape to infer or migrate.
        let (name, width, height) = Document::load_metadata(&dir)
            .map(|meta| (meta.name, meta.w, meta.h))
            .unwrap_or_else(|_| ("Recovered document".into(), 32, 32));
        self.cleanup_stale_transactions()?;
        let (transaction, _) = self.begin_import_transaction(id, true)?;
        let staged = transaction.studio();
        let staged_dir = staged.doc_dir(id);
        fs::create_dir(&staged_dir).map_err(|error| error.to_string())?;
        Document::new(&name, width, height).save(&staged_dir)?;
        staged.set_document_revision(id, 0)?;
        staged.journal_append(
            id,
            ToolName::DocNew,
            &json!({
                "doc_id": id, "name": name, "width": width, "height": height,
            }),
        )?;
        let outcome = transaction.commit(id)?;
        let mut report = json!({
            "doc_id": id,
            "reason": reason,
            "message": "unreadable saved data was replaced with a fresh document",
        });
        if let CommitOutcome::DurabilityUncertain { warning } = outcome {
            report["commit_warning"] = json!(warning);
        }
        Ok(Some(report))
    }
}
