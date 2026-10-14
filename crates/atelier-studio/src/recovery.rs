//! Damage detection and explicit replacement of unreadable saved documents.

use std::fs;

use atelier_core::document::Document;
use serde_json::{Value, json};

use crate::{CommitOutcome, Studio, ToolName};

/// True when a [`Studio::document_damage`] reason means the document is
/// intact but saved in a format this build does not read. Such a document
/// opens again with the Atelier version that wrote it.
pub fn is_format_mismatch(reason: &str) -> bool {
    reason.contains("unsupported document format") || reason.contains("unsupported journal format")
}

impl Studio {
    /// Why a saved document cannot be used, or `None` when it is healthy or
    /// missing. Read-only: tool calls use this to explain a failure without
    /// touching the saved data. Directory links are refused.
    pub fn document_damage(&self, id: &str) -> Result<Option<String>, String> {
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
            // A document from before the recipe format keeps its journal
            // under the earlier name; that is a version mismatch, not damage.
            if !dir.join(crate::JOURNAL_FILE).exists() && dir.join("recipe.jsonl").exists() {
                return Err(format!(
                    "unsupported journal format: recipe.jsonl predates {} (this build supports {})",
                    crate::JOURNAL_FILE,
                    crate::JOURNAL_SCHEMA
                ));
            }
            let journal = super::store::parse_journal_file(id, &dir.join(crate::JOURNAL_FILE))?;
            if journal.entries.is_empty() {
                return Err("document has no complete recipe".into());
            }
            // A torn append cannot be extended without changing its meaning.
            if journal.torn_tail {
                return Err("document journal has an incomplete final line".into());
            }
            Ok(())
        })();
        Ok(validation.err())
    }

    /// Replace an unreadable document with a fresh generation, discarding its
    /// saved pixels and journal. Only the explicit `library reset` command
    /// calls this; tool calls never do. Healthy documents and missing IDs are
    /// untouched and report `None`.
    pub fn reset_document(&self, id: &str) -> Result<Option<Value>, String> {
        let _lock = self.lock_store_exclusive()?;
        let Some(reason) = self.document_damage(id)? else {
            return Ok(None);
        };
        let dir = self.doc_dir(id);

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
