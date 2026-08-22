//! Same-filesystem store transactions for shipped CLI and MCP mutations.
//!
//! A call runs against a staged document tree. Regular files are hard-linked
//! where safe; writers replace changed files by rename, while the append-only
//! journal is copied. The completed staged tree is then atomically exchanged
//! with the live tree using the platform's atomic directory exchange.

use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::atomic_rename::exchange;
use super::{JOURNAL_FILE, Studio};

const TRANSACTIONS_DIR: &str = ".transactions";

/// Result after a transaction has crossed its atomic visibility point.
///
/// `DurabilityUncertain` is still a successful commit: reporting it as a
/// failed mutation would invite a retry that could duplicate a non-idempotent
/// edit. The warning tells the caller that the new generation was published,
/// but the final parent-directory fsync could not be confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Durable,
    DurabilityUncertain { warning: String },
}

impl CommitOutcome {
    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Durable => None,
            Self::DurabilityUncertain { warning } => Some(warning),
        }
    }
}

/// A private store on which one mutating call executes before publication.
pub struct StoreTransaction {
    live_docs_dir: PathBuf,
    stage_root: PathBuf,
    studio: Studio,
    intent: TransactionIntent,
    committed: bool,
}

enum TransactionIntent {
    Create,
    Existing(String),
}

impl StoreTransaction {
    /// The isolated Studio passed to the normal shared handler path.
    pub fn studio(&self) -> &Studio {
        &self.studio
    }

    /// Atomically publish `id` after a successful handler and journal write.
    ///
    /// Existing→existing exchanges a complete document state. Missing→present
    /// creates a document. Present→missing commits deletion. Every other shape
    /// is an invalid transaction result.
    pub fn commit(mut self, id: &str) -> Result<CommitOutcome, String> {
        if !Studio::valid_id(id) {
            return Err(format!("invalid transaction document id '{id}'"));
        }
        if let TransactionIntent::Existing(expected) = &self.intent
            && expected != id
        {
            return Err(format!(
                "transaction for existing document '{expected}' cannot commit as '{id}'"
            ));
        }
        let live = self.live_docs_dir.join(id);
        let staged = self.stage_root.join(id);
        let live_exists = directory_state(&live, "live document")?;
        let staged_exists = directory_state(&staged, "staged document")?;

        match (&self.intent, live_exists) {
            (TransactionIntent::Create, true) => {
                return Err(format!(
                    "refusing to replace existing document '{id}' from a creation transaction"
                ));
            }
            (TransactionIntent::Existing(expected), false) => {
                return Err(format!(
                    "document '{expected}' disappeared before its transaction could commit"
                ));
            }
            _ => {}
        }

        if staged_exists {
            sync_tree(&staged)?;
        }

        match (live_exists, staged_exists) {
            (true, true) => rename_exchange(&live, &staged)?,
            (false, true) => fs::rename(&staged, &live)
                .map_err(|e| format!("cannot publish new document '{id}': {e}"))?,
            (true, false) => fs::rename(&live, &staged)
                .map_err(|e| format!("cannot publish deletion of '{id}': {e}"))?,
            (false, false) => {
                return Err(format!(
                    "transaction for '{id}' produced neither a document nor a deletion"
                ));
            }
        }
        self.committed = true;
        // Rename/exchange crosses two parent directories. Both directory-entry
        // updates must reach stable storage before the commit is Durable.
        let live_sync = sync_dir(&self.live_docs_dir);
        let stage_sync = sync_dir(&self.stage_root);
        let outcome = commit_outcome(id, live_sync.and(stage_sync));
        // After an exchange this removes the old generation; after deletion it
        // removes the tombstone. Cleanup is not part of the commit point.
        let _ = fs::remove_dir_all(&self.stage_root);
        Ok(outcome)
    }
}

impl Drop for StoreTransaction {
    fn drop(&mut self) {
        if !self.committed || self.stage_root.exists() {
            let _ = fs::remove_dir_all(&self.stage_root);
        }
    }
}

impl Studio {
    /// Remove transaction debris left by a killed process. The caller must hold
    /// the live store's exclusive lock, so no other writer can be staging.
    pub fn cleanup_stale_transactions(&self) -> Result<(), String> {
        let root = self.docs_dir.join(TRANSACTIONS_DIR);
        match fs::symlink_metadata(&root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "cannot inspect stale transactions at {}: {error}",
                    root.display()
                ));
            }
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(format!(
                    "refusing non-directory transaction path {}",
                    root.display()
                ));
            }
            Ok(_) => {}
        }
        fs::remove_dir_all(&root).map_err(|error| {
            format!(
                "cannot clean stale transactions at {}: {error}",
                root.display()
            )
        })
    }

    /// Start an isolated store transaction. `id=None` creates an empty staging
    /// store for `doc_new`; `Some(id)` snapshots that one existing document.
    pub fn begin_transaction(&self, id: Option<&str>) -> Result<StoreTransaction, String> {
        if let Some(id) = id
            && !Self::valid_id(id)
        {
            return Err(format!("invalid document id '{id}'"));
        }
        let transactions = self.docs_dir.join(TRANSACTIONS_DIR);
        ensure_directory(&transactions, "transaction directory")?;
        let stage_root = transactions.join(Uuid::new_v4().to_string());
        fs::create_dir(&stage_root)
            .map_err(|e| format!("cannot create transaction {}: {e}", stage_root.display()))?;

        let staged = (|| -> Result<(), String> {
            if let Some(id) = id {
                let source = self.docs_dir.join(id);
                if !self.exists(id) {
                    return Err(format!("no document '{id}'"));
                }
                stage_tree(&source, &stage_root.join(id))?;
            }
            Ok(())
        })();
        if let Err(error) = staged {
            let _ = fs::remove_dir_all(&stage_root);
            return Err(error);
        }

        Ok(StoreTransaction {
            live_docs_dir: self.docs_dir.clone(),
            studio: Studio::with_docs_dir(stage_root.clone()),
            stage_root,
            intent: id.map_or(TransactionIntent::Create, |id| {
                TransactionIntent::Existing(id.to_owned())
            }),
            committed: false,
        })
    }

    /// Start an empty transaction for a portable document import.
    ///
    /// Unlike [`Self::begin_transaction`], replacement does not snapshot the
    /// generation that is about to be superseded: the archive is a complete
    /// document tree and is validated in isolation before commit. The caller
    /// must hold the store's exclusive lock across this call and commit.
    pub(crate) fn begin_import_transaction(
        &self,
        id: &str,
        replace: bool,
    ) -> Result<(StoreTransaction, bool), String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid imported document id '{id}'"));
        }

        let live = self.docs_dir.join(id);
        let live_exists = directory_state(&live, "archive destination")?;
        if live_exists && !replace {
            return Err(format!(
                "document '{id}' already exists; pass replace=true to replace it atomically"
            ));
        }
        let transactions = self.docs_dir.join(TRANSACTIONS_DIR);
        ensure_directory(&transactions, "transaction directory")?;
        let stage_root = transactions.join(Uuid::new_v4().to_string());
        fs::create_dir(&stage_root).map_err(|e| {
            format!(
                "cannot create import transaction {}: {e}",
                stage_root.display()
            )
        })?;

        Ok((
            StoreTransaction {
                live_docs_dir: self.docs_dir.clone(),
                studio: Studio::with_docs_dir(stage_root.clone()),
                stage_root,
                intent: if live_exists {
                    TransactionIntent::Existing(id.to_owned())
                } else {
                    TransactionIntent::Create
                },
                committed: false,
            },
            live_exists,
        ))
    }
}

fn directory_state(path: &Path, label: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "{label} {} must be a real directory (symlinks are refused)",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "cannot inspect {label} {}: {error}",
            path.display()
        )),
    }
}

fn ensure_directory(path: &Path, label: &str) -> Result<(), String> {
    match directory_state(path, label)? {
        true => Ok(()),
        false => match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                directory_state(path, label).and_then(|is_dir| {
                    is_dir
                        .then_some(())
                        .ok_or_else(|| format!("{label} {} is not a directory", path.display()))
                })
            }
            Err(error) => Err(format!("cannot create {label} {}: {error}", path.display())),
        },
    }
}

#[derive(Clone, Copy)]
enum TreeIoEvent {
    VisitDirectory,
    VisitFile,
    LinkFile,
    CopyFile,
    SyncDirectory,
    SyncFile,
}

fn stage_tree(source: &Path, destination: &Path) -> Result<(), String> {
    stage_tree_inner(source, destination, true, &mut |_| {})
}

fn stage_tree_inner<F>(
    source: &Path,
    destination: &Path,
    copy_root_journal: bool,
    observe: &mut F,
) -> Result<(), String>
where
    F: FnMut(TreeIoEvent),
{
    if !directory_state(source, "document source")? {
        return Err(format!("document source {} is missing", source.display()));
    }
    fs::create_dir(destination)
        .map_err(|e| format!("cannot stage {}: {e}", destination.display()))?;
    observe(TreeIoEvent::VisitDirectory);
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            // Only the live document's root recipe is appendable. Recipes in
            // immutable checkpoint snapshots are never edited in place:
            // restore copies one out, prune unlinks the staged directory, and
            // save creates a new snapshot. Hard-linking those nested journals
            // avoids copying up to the entire retained checkpoint quota on
            // every otherwise-small document mutation.
            stage_tree_inner(&source_path, &destination_path, false, observe)?;
        } else if file_type.is_file() {
            observe(TreeIoEvent::VisitFile);
            // Appending to a hard-linked journal would mutate the live recipe.
            // Other writers replace files by rename before changing content;
            // nested checkpoint recipes are immutable snapshots (above).
            if copy_root_journal && entry.file_name() == JOURNAL_FILE {
                fs::copy(&source_path, &destination_path).map_err(|e| e.to_string())?;
                observe(TreeIoEvent::CopyFile);
            } else if fs::hard_link(&source_path, &destination_path).is_ok() {
                observe(TreeIoEvent::LinkFile);
            } else {
                fs::copy(&source_path, &destination_path).map_err(|e| e.to_string())?;
                observe(TreeIoEvent::CopyFile);
            }
        } else {
            return Err(format!(
                "refusing non-regular document entry {}",
                source_path.display()
            ));
        }
    }
    Ok(())
}

fn sync_tree(path: &Path) -> Result<(), String> {
    sync_tree_inner(path, &mut |_| {})
}

fn sync_tree_inner<F>(path: &Path, observe: &mut F) -> Result<(), String>
where
    F: FnMut(TreeIoEvent),
{
    observe(TreeIoEvent::VisitDirectory);
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let child = entry.path();
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            sync_tree_inner(&child, observe)?;
        } else if file_type.is_file() {
            observe(TreeIoEvent::VisitFile);
            fs::File::open(&child)
                .and_then(|file| file.sync_all())
                .map_err(|e| format!("cannot sync {}: {e}", child.display()))?;
            observe(TreeIoEvent::SyncFile);
        } else {
            return Err(format!(
                "refusing non-regular staged entry {}",
                child.display()
            ));
        }
    }
    sync_dir(path).map_err(|e| format!("cannot sync {}: {e}", path.display()))?;
    observe(TreeIoEvent::SyncDirectory);
    Ok(())
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn commit_outcome(id: &str, store_sync: std::io::Result<()>) -> CommitOutcome {
    match store_sync {
        Ok(()) => CommitOutcome::Durable,
        Err(error) => CommitOutcome::DurabilityUncertain {
            warning: format!(
                "document '{id}' was committed, but the store directory sync failed ({error}); do not retry this mutation automatically"
            ),
        },
    }
}

fn rename_exchange(left: &Path, right: &Path) -> Result<(), String> {
    exchange(left, right).map_err(|error| {
        format!(
            "cannot atomically exchange {} and {}: {}",
            left.display(),
            right.display(),
            error
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{CheckpointAction, ToolName};

    fn studio(tag: &str) -> Studio {
        let root = std::env::temp_dir().join(format!("atelier-transaction-{tag}"));
        let _ = fs::remove_dir_all(&root);
        Studio::with_docs_dir(root)
    }

    #[derive(Default)]
    struct TreeIoStats {
        visited_directories: usize,
        visited_files: usize,
        linked_files: usize,
        copied_files: usize,
        synced_directories: usize,
        synced_files: usize,
    }

    impl TreeIoStats {
        fn observe(&mut self, event: TreeIoEvent) {
            match event {
                TreeIoEvent::VisitDirectory => self.visited_directories += 1,
                TreeIoEvent::VisitFile => self.visited_files += 1,
                TreeIoEvent::LinkFile => self.linked_files += 1,
                TreeIoEvent::CopyFile => self.copied_files += 1,
                TreeIoEvent::SyncDirectory => self.synced_directories += 1,
                TreeIoEvent::SyncFile => self.synced_files += 1,
            }
        }
    }

    fn measure_stage_tree(source: &Path, destination: &Path) -> Result<TreeIoStats, String> {
        let mut stats = TreeIoStats::default();
        stage_tree_inner(source, destination, true, &mut |event| {
            stats.observe(event);
        })?;
        Ok(stats)
    }

    fn measure_sync_tree(path: &Path) -> Result<TreeIoStats, String> {
        let mut stats = TreeIoStats::default();
        sync_tree_inner(path, &mut |event| stats.observe(event))?;
        Ok(stats)
    }

    #[test]
    fn dropped_transaction_leaves_pixels_and_journal_unchanged() {
        let live = studio("rollback");
        let created = live.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        live.journal_append(id, ToolName::DocNew, &json!({"name":"d","doc_id":id}))
            .unwrap();

        let transaction = live.begin_transaction(Some(id)).unwrap();
        transaction
            .studio()
            .doc_draw(
                id,
                0,
                0,
                None,
                "fill_cel",
                json!({"color":[9,8,7,255]}).as_object().unwrap().clone(),
            )
            .unwrap();
        transaction
            .studio()
            .journal_append(id, ToolName::DocDraw, &json!({"doc_id":id,"op":"fill_cel"}))
            .unwrap();

        assert_eq!(
            live.doc_get_pixel(id, Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
        assert_eq!(live.journal(id).unwrap().len(), 1);
        drop(transaction);
        assert_eq!(live.journal(id).unwrap().len(), 1);
    }

    #[test]
    fn commit_publishes_pixels_and_journal_together() {
        let live = studio("commit");
        let created = live.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        live.journal_append(id, ToolName::DocNew, &json!({"name":"d","doc_id":id}))
            .unwrap();
        let transaction = live.begin_transaction(Some(id)).unwrap();
        transaction
            .studio()
            .doc_draw(
                id,
                0,
                0,
                None,
                "fill_cel",
                json!({"color":[9,8,7,255]}).as_object().unwrap().clone(),
            )
            .unwrap();
        transaction
            .studio()
            .journal_append(id, ToolName::DocDraw, &json!({"doc_id":id,"op":"fill_cel"}))
            .unwrap();
        transaction.commit(id).unwrap();

        assert_eq!(
            live.doc_get_pixel(id, Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([9, 8, 7, 255])
        );
        assert_eq!(live.journal(id).unwrap().len(), 2);
    }

    #[test]
    fn transaction_commits_creation_and_deletion() {
        let live = studio("lifecycle");
        let create = live.begin_transaction(None).unwrap();
        let report = create.studio().doc_new("d", 4, 4).unwrap();
        let id = report["doc_id"].as_str().unwrap();
        create.commit(id).unwrap();
        assert!(live.exists(id));

        let delete = live.begin_transaction(Some(id)).unwrap();
        delete.studio().delete_doc(id).unwrap();
        delete.commit(id).unwrap();
        assert!(!live.exists(id));
    }

    #[test]
    fn staging_links_checkpoint_recipes_but_syncs_the_complete_generation() {
        let live = studio("checkpoint-io-profile");
        let created = live.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        live.journal_append(id, ToolName::DocNew, &json!({"name":"d","doc_id":id}))
            .unwrap();
        for label in ["first", "second"] {
            live.checkpoint(id, CheckpointAction::Save, Some(label), None)
                .unwrap();
        }

        let source = live.doc_dir(id);
        let stage_root = live.docs_dir.join("measured-stage");
        fs::create_dir(&stage_root).unwrap();
        let staged = stage_root.join(id);
        let stage_stats = measure_stage_tree(&source, &staged).unwrap();

        // Empty canvas: root+root-cels+checkpoint-root+2*(cp+cp-cels),
        // with doc/recipe at the root and doc/recipe/label per checkpoint.
        assert_eq!(stage_stats.visited_directories, 7);
        assert_eq!(stage_stats.visited_files, 8);
        assert_eq!(stage_stats.copied_files, 1);
        assert_eq!(stage_stats.linked_files, 7);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let source_root_recipe = fs::metadata(source.join(JOURNAL_FILE)).unwrap();
            let staged_root_recipe = fs::metadata(staged.join(JOURNAL_FILE)).unwrap();
            assert_ne!(source_root_recipe.ino(), staged_root_recipe.ino());

            let checkpoint_recipe = Path::new(".checkpoints/cp1").join(JOURNAL_FILE);
            let source_checkpoint = fs::metadata(source.join(&checkpoint_recipe)).unwrap();
            let staged_checkpoint = fs::metadata(staged.join(&checkpoint_recipe)).unwrap();
            assert_eq!(source_checkpoint.ino(), staged_checkpoint.ino());
            assert_eq!(source_checkpoint.dev(), staged_checkpoint.dev());
        }

        let sync_stats = measure_sync_tree(&staged).unwrap();
        assert_eq!(sync_stats.visited_directories, 7);
        assert_eq!(sync_stats.synced_directories, 7);
        assert_eq!(sync_stats.visited_files, 8);
        assert_eq!(sync_stats.synced_files, 8);
    }

    #[test]
    fn creation_transaction_refuses_to_replace_a_live_document() {
        let live = studio("creation-collision");
        let create = live.begin_transaction(None).unwrap();
        let report = create.studio().doc_new("staged", 4, 4).unwrap();
        let id = report["doc_id"].as_str().unwrap();
        stage_tree(&create.studio().doc_dir(id), &live.doc_dir(id)).unwrap();

        let error = create.commit(id).unwrap_err();

        assert!(error.contains("refusing to replace"), "{error}");
        assert_eq!(live.doc_info(id).unwrap()["name"], "staged");
    }

    #[test]
    fn post_commit_sync_failure_is_success_with_a_no_retry_warning() {
        let outcome = commit_outcome("doc", Err(std::io::Error::other("sync unavailable")));
        let warning = outcome.warning().expect("uncertain durability warning");
        assert!(warning.contains("was committed"));
        assert!(warning.contains("do not retry"));
    }

    #[cfg(unix)]
    #[test]
    fn transaction_roots_and_documents_refuse_symlinks() {
        use std::os::unix::fs::symlink;

        let live = studio("symlink-safety");
        let external = live.docs_dir.join("external");
        fs::create_dir(&external).unwrap();
        let transactions = live.docs_dir.join(TRANSACTIONS_DIR);
        symlink(&external, &transactions).unwrap();
        assert!(live.cleanup_stale_transactions().is_err());
        assert!(live.begin_transaction(None).is_err());
        fs::remove_file(&transactions).unwrap();

        let created = live.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let document = live.doc_dir(id);
        let held = live.docs_dir.join("held-document");
        fs::rename(&document, &held).unwrap();
        symlink(&held, &document).unwrap();
        assert!(live.begin_transaction(Some(id)).is_err());
    }
}
