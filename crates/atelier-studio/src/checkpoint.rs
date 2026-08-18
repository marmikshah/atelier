//! Bounded checkpoint persistence, listing, restoration, and pruning.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use atelier_core::document::MAX_DOCUMENT_CELS;
use serde_json::{Value, json};

use super::{CheckpointAction, JOURNAL_FILE, Studio, store::read_bounded_utf8};

/// True only for the `cp<n>` ids `doc_checkpoint action=save` mints.
///
/// A checkpoint id is joined onto the store path and then handed to
/// `remove_dir_all`, so an unvalidated one is a directory traversal:
/// `../../../../x` escaped the store and deleted it. Every id the
/// tool hands out matches this shape, so rejecting anything else costs nothing.
fn valid_checkpoint_id(cpid: &str) -> bool {
    cpid.strip_prefix("cp").is_some_and(|number| {
        !number.is_empty()
            && number != "0"
            && !number.starts_with('0')
            && number.bytes().all(|byte| byte.is_ascii_digit())
            && number.parse::<u32>().is_ok()
    })
}

/// Checkpoints copy the complete live document state, so an unbounded history
/// can multiply storage unexpectedly. Callers must prune explicitly rather
/// than having older recovery points disappear behind their backs.
const MAX_CHECKPOINTS: usize = 32;

/// Labels are persisted as UTF-8 in `label.txt`; cap their encoded size before
/// creating the checkpoint directory or copying any document files.
const MAX_CHECKPOINT_LABEL_BYTES: usize = 4096;

/// Logical bytes retained by checkpoints for one document. Sparse files count
/// at their declared length so the quota describes restorable content, not the
/// current filesystem's compression or allocation strategy.
const MAX_CHECKPOINT_LOGICAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Bound hostile or legacy checkpoint roots before collecting their names.
const MAX_CHECKPOINT_DIRECTORY_ENTRIES: usize = 4096;

fn read_checkpoint_label(path: &Path, checkpoint_id: &str) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot inspect checkpoint '{checkpoint_id}' label: {error}"
        )),
        Ok(_) => read_bounded_utf8(
            path,
            &format!("checkpoint '{checkpoint_id}' label"),
            MAX_CHECKPOINT_LABEL_BYTES as u64,
        )
        .map(Some),
    }
}

fn write_checkpoint_label(checkpoint_dir: &Path, label: &str) -> Result<(), String> {
    let label_path = checkpoint_dir.join("label.txt");
    let write_result = (|| -> std::io::Result<()> {
        // `create_new` refuses a stale file or symlink rather than following it.
        let mut file = fs::File::options()
            .write(true)
            .create_new(true)
            .open(&label_path)?;
        file.write_all(label.as_bytes())
    })();
    if let Err(error) = write_result {
        return match fs::remove_dir_all(checkpoint_dir) {
            Ok(()) => Err(format!(
                "cannot write checkpoint label {}: {error}; the partial checkpoint was removed",
                label_path.display()
            )),
            Err(cleanup_error) => Err(format!(
                "cannot write checkpoint label {}: {error}; also cannot remove the partial checkpoint: {cleanup_error}",
                label_path.display()
            )),
        };
    }
    Ok(())
}

fn list_checkpoints(checkpoint_root: &Path) -> Result<Vec<String>, String> {
    let root_metadata = match fs::symlink_metadata(checkpoint_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "cannot inspect checkpoint directory {}: {error}",
                checkpoint_root.display()
            ));
        }
    };
    if !root_metadata.file_type().is_dir() {
        return Err(format!(
            "checkpoint directory {} must be a real directory (symlinks are refused)",
            checkpoint_root.display()
        ));
    }

    let entries = fs::read_dir(checkpoint_root).map_err(|error| {
        format!(
            "cannot read checkpoint directory {}: {error}",
            checkpoint_root.display()
        )
    })?;
    let mut checkpoints = Vec::new();
    let mut inspected = 0usize;
    for entry in entries {
        inspected = inspected
            .checked_add(1)
            .ok_or_else(|| "checkpoint directory traversal counter overflowed".to_string())?;
        if inspected > MAX_CHECKPOINT_DIRECTORY_ENTRIES {
            return Err(format!(
                "checkpoint directory {} has more than {MAX_CHECKPOINT_DIRECTORY_ENTRIES} entries; prune incomplete or retained checkpoints before continuing",
                checkpoint_root.display()
            ));
        }
        let entry = entry.map_err(|error| {
            format!(
                "cannot read an entry in checkpoint directory {}: {error}",
                checkpoint_root.display()
            )
        })?;
        let checkpoint_id = entry.file_name().to_string_lossy().to_string();
        if !valid_checkpoint_id(&checkpoint_id) {
            return Err(format!(
                "invalid checkpoint directory id '{checkpoint_id}' in {}; expected the cp<n> form returned by checkpoint save",
                checkpoint_root.display()
            ));
        }
        let checkpoint_path = entry.path();
        let checkpoint_metadata = fs::symlink_metadata(&checkpoint_path).map_err(|error| {
            format!(
                "cannot inspect checkpoint entry {}: {error}",
                checkpoint_path.display()
            )
        })?;
        if !checkpoint_metadata.file_type().is_dir() {
            return Err(format!(
                "checkpoint entry {} must be a real directory (symlinks and special files are refused)",
                checkpoint_path.display()
            ));
        }
        let metadata_path = checkpoint_path.join("doc.json");
        match fs::symlink_metadata(&metadata_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect checkpoint metadata {}: {error}",
                    metadata_path.display()
                ));
            }
            Ok(metadata) if metadata.file_type().is_file() => {
                checkpoints.push(checkpoint_id);
            }
            Ok(_) => {
                return Err(format!(
                    "checkpoint metadata {} must be a regular file (symlinks and special files are refused)",
                    metadata_path.display()
                ));
            }
        }
    }
    // Numeric order: lexicographic put cp10 before cp2 past nine checkpoints.
    checkpoints.sort_by_key(|id| {
        id.strip_prefix("cp")
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    });
    Ok(checkpoints)
}

fn checked_logical_add(total: u64, bytes: u64, context: &str) -> Result<u64, String> {
    total.checked_add(bytes).ok_or_else(|| {
        format!("checkpoint logical-byte accounting overflow while inspecting {context}")
    })
}

fn managed_file_logical_bytes(path: &Path, context: &str, required: bool) -> Result<u64, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(format!(
                "cannot inspect {context} {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(format!(
            "{context} {} must be a regular file (symlinks and special files are refused)",
            path.display()
        ));
    }
    Ok(metadata.len())
}

fn managed_cels_logical_bytes(
    cels_dir: &Path,
    context: &str,
    max_entries: usize,
) -> Result<u64, String> {
    let metadata = fs::symlink_metadata(cels_dir)
        .map_err(|error| format!("cannot inspect {context} {}: {error}", cels_dir.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "{context} {} must be a real directory (symlinks and special files are refused)",
            cels_dir.display()
        ));
    }
    let entries = fs::read_dir(cels_dir)
        .map_err(|error| format!("cannot read {context} {}: {error}", cels_dir.display()))?;
    let mut total = 0u64;
    let mut inspected = 0usize;
    for entry in entries {
        inspected = inspected.checked_add(1).ok_or_else(|| {
            format!("checkpoint traversal counter overflow while inspecting {context}")
        })?;
        if inspected > max_entries {
            return Err(format!(
                "{context} contains more than {max_entries} cel entries; checkpoint size traversal is bounded"
            ));
        }
        let entry = entry.map_err(|error| {
            format!(
                "cannot read an entry in {context} {}: {error}",
                cels_dir.display()
            )
        })?;
        let path = entry.path();
        let bytes = managed_file_logical_bytes(&path, context, true)?;
        total = checked_logical_add(total, bytes, context)?;
    }
    Ok(total)
}

fn managed_snapshot_logical_bytes(
    root: &Path,
    context: &str,
    include_checkpoint_label: bool,
    max_cel_entries: usize,
) -> Result<u64, String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("cannot inspect {context} {}: {error}", root.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "{context} {} must be a real directory (symlinks are refused)",
            root.display()
        ));
    }

    let mut total = managed_file_logical_bytes(&root.join("doc.json"), context, true)?;
    total = checked_logical_add(
        total,
        managed_cels_logical_bytes(&root.join("cels"), context, max_cel_entries)?,
        context,
    )?;
    for name in [JOURNAL_FILE, "reference.png"] {
        total = checked_logical_add(
            total,
            managed_file_logical_bytes(&root.join(name), context, false)?,
            context,
        )?;
    }
    if include_checkpoint_label {
        total = checked_logical_add(
            total,
            managed_file_logical_bytes(&root.join("label.txt"), context, false)?,
            context,
        )?;
    }
    Ok(total)
}

fn retained_checkpoint_logical_bytes(
    checkpoint_root: &Path,
    checkpoint_ids: &[String],
) -> Result<u64, String> {
    let mut total = 0u64;
    for checkpoint_id in checkpoint_ids {
        let context = format!("checkpoint '{checkpoint_id}'");
        let bytes = managed_snapshot_logical_bytes(
            &checkpoint_root.join(checkpoint_id),
            &context,
            true,
            MAX_DOCUMENT_CELS,
        )?;
        total = checked_logical_add(total, bytes, &context)?;
    }
    Ok(total)
}

fn enforce_checkpoint_logical_quota(
    document_id: &str,
    current_bytes: u64,
    new_bytes: u64,
) -> Result<u64, String> {
    let projected_bytes =
        checked_logical_add(current_bytes, new_bytes, "projected checkpoint storage")?;
    if projected_bytes > MAX_CHECKPOINT_LOGICAL_BYTES {
        return Err(format!(
            "checkpoint logical-byte quota for document '{document_id}' would be exceeded: current={current_bytes} bytes, projected={projected_bytes} bytes, limit={MAX_CHECKPOINT_LOGICAL_BYTES} bytes; prune checkpoints before saving another"
        ));
    }
    Ok(projected_bytes)
}

fn require_new_checkpoint_destination(path: &Path, checkpoint_id: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect destination for checkpoint '{checkpoint_id}' {}: {error}",
            path.display()
        )),
        Ok(_) => Err(format!(
            "destination for checkpoint '{checkpoint_id}' already exists at {}; prune that incomplete checkpoint before saving",
            path.display()
        )),
    }
}

fn copy_managed_regular_file(src: &Path, dst: &Path, context: &str) -> Result<(), String> {
    let path_metadata = fs::symlink_metadata(src)
        .map_err(|error| format!("cannot inspect {context} {}: {error}", src.display()))?;
    if !path_metadata.file_type().is_file() {
        return Err(format!(
            "{context} {} must be a regular file (symlinks and special files are refused)",
            src.display()
        ));
    }

    let mut options = fs::File::options();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Linux O_NOFOLLOW closes the final-component swap between lstat/open.
        options.custom_flags(0o400000);
    }
    let source = options
        .open(src)
        .map_err(|error| format!("cannot open {context} {}: {error}", src.display()))?;
    let source_metadata = source
        .metadata()
        .map_err(|error| format!("cannot inspect opened {context} {}: {error}", src.display()))?;
    if !source_metadata.file_type().is_file() {
        return Err(format!(
            "opened {context} {} is not a regular file",
            src.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if path_metadata.dev() != source_metadata.dev()
            || path_metadata.ino() != source_metadata.ino()
        {
            return Err(format!("{context} {} changed while opening", src.display()));
        }
    }

    let mut destination = fs::File::options()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|error| format!("cannot create snapshot file {}: {error}", dst.display()))?;
    let expected = source_metadata.len();
    let copied = std::io::copy(
        &mut source.take(expected.saturating_add(1)),
        &mut destination,
    )
    .map_err(|error| {
        format!(
            "cannot copy {context} {} to {}: {error}",
            src.display(),
            dst.display()
        )
    })?;
    if copied != expected {
        return Err(format!(
            "{context} {} changed size while it was copied (expected {expected} bytes, copied {copied})",
            src.display()
        ));
    }
    fs::set_permissions(dst, source_metadata.permissions()).map_err(|error| {
        format!(
            "cannot preserve permissions on snapshot file {}: {error}",
            dst.display()
        )
    })
}

fn copy_optional_managed_file(src: &Path, dst: &Path, context: &str) -> Result<(), String> {
    match fs::symlink_metadata(src) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect {context} {}: {error}",
            src.display()
        )),
        Ok(_) => copy_managed_regular_file(src, dst, context),
    }
}

/// Snapshot every file that defines live document state. Checkpoint metadata
/// itself is intentionally excluded.
fn snapshot_files(src: &Path, dst: &Path) -> Result<(), String> {
    let dst_parent = dst
        .parent()
        .ok_or_else(|| format!("snapshot destination {} has no parent", dst.display()))?;
    fs::create_dir_all(dst_parent).map_err(|error| {
        format!(
            "cannot create snapshot parent {}: {error}",
            dst_parent.display()
        )
    })?;
    fs::create_dir(dst).map_err(|error| {
        format!(
            "cannot create snapshot directory {}: {error}",
            dst.display()
        )
    })?;
    copy_managed_regular_file(
        &src.join("doc.json"),
        &dst.join("doc.json"),
        "snapshot metadata",
    )?;

    let source_cels = src.join("cels");
    let cels_metadata = fs::symlink_metadata(&source_cels).map_err(|error| {
        format!(
            "cannot inspect snapshot cels {}: {error}",
            source_cels.display()
        )
    })?;
    if !cels_metadata.file_type().is_dir() {
        return Err(format!(
            "snapshot cels {} must be a real directory (symlinks and special files are refused)",
            source_cels.display()
        ));
    }
    let destination_cels = dst.join("cels");
    fs::create_dir(&destination_cels).map_err(|error| {
        format!(
            "cannot create snapshot cels {}: {error}",
            destination_cels.display()
        )
    })?;
    let entries = fs::read_dir(&source_cels).map_err(|error| {
        format!(
            "cannot read snapshot cels {}: {error}",
            source_cels.display()
        )
    })?;
    let mut inspected = 0usize;
    for entry in entries {
        inspected = inspected
            .checked_add(1)
            .ok_or("snapshot cel traversal counter overflowed")?;
        if inspected > MAX_DOCUMENT_CELS {
            return Err(format!(
                "snapshot contains more than {MAX_DOCUMENT_CELS} cel entries"
            ));
        }
        let entry = entry.map_err(|error| {
            format!(
                "cannot read an entry in snapshot cels {}: {error}",
                source_cels.display()
            )
        })?;
        copy_managed_regular_file(
            &entry.path(),
            &destination_cels.join(entry.file_name()),
            "snapshot cel",
        )?;
    }

    // A restored document must restore both its external comparison context
    // and the recipe that describes its pixels. Otherwise the canvas rolls
    // back while replay keeps the discarded edits.
    for name in [JOURNAL_FILE, "reference.png"] {
        copy_optional_managed_file(&src.join(name), &dst.join(name), name)?;
    }
    Ok(())
}

impl Studio {
    // -- doc_checkpoint: snapshot / restore -------------------------------

    /// History for an all-destructive editor: snapshot the document directory,
    /// list/restore snapshots, or prune them. `action`: `save` | `list` |
    /// `restore` | `prune`.
    pub fn checkpoint(
        &self,
        id: &str,
        action: CheckpointAction,
        label: Option<&str>,
        checkpoint_id: Option<&str>,
    ) -> Result<Value, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{}'", id));
        }
        if !self.exists(id) {
            return Err(format!("no document '{}'", id));
        }
        // `checkpoint_id` is joined onto the store path and handed to
        // remove_dir_all, so it is as dangerous as `doc_id` and
        // gets the same treatment. Ids are always minted as `cp{n}` (below), so
        // anything else — traversal, absolute paths, a stray name — is a
        // caller error, not a lookup miss.
        if let Some(cpid) = checkpoint_id
            && !valid_checkpoint_id(cpid)
        {
            return Err(format!(
                "invalid checkpoint id '{}' — expected the cp<n> form doc_checkpoint action=save returns",
                cpid
            ));
        }
        let dir = self.doc_dir(id);
        let cps = dir.join(".checkpoints");
        match action {
            CheckpointAction::Save => {
                if let Some(label) = label
                    && label.len() > MAX_CHECKPOINT_LABEL_BYTES
                {
                    return Err(format!(
                        "checkpoint label is {} UTF-8 bytes, over the {MAX_CHECKPOINT_LABEL_BYTES}-byte limit",
                        label.len()
                    ));
                }
                let existing = list_checkpoints(&cps)?;
                if existing.len() >= MAX_CHECKPOINTS {
                    return Err(format!(
                        "document '{id}' already has the maximum {MAX_CHECKPOINTS} checkpoints; prune one before saving another"
                    ));
                }
                let n = existing
                    .iter()
                    .filter_map(|s| s.strip_prefix("cp").and_then(|t| t.parse::<u32>().ok()))
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("checkpoint id space is exhausted; prune checkpoints before saving")?;
                let cpid = format!("cp{}", n);
                let dst = cps.join(&cpid);
                require_new_checkpoint_destination(&dst, &cpid)?;
                let current_bytes = retained_checkpoint_logical_bytes(&cps, &existing)?;
                let mut new_bytes = managed_snapshot_logical_bytes(
                    &dir,
                    "live document",
                    false,
                    MAX_DOCUMENT_CELS,
                )?;
                if let Some(label) = label {
                    new_bytes =
                        checked_logical_add(new_bytes, label.len() as u64, "new checkpoint label")?;
                }
                enforce_checkpoint_logical_quota(id, current_bytes, new_bytes)?;
                // A failed snapshot must not leave a partial checkpoint dir —
                // restore would treat it as valid and the prune rotation
                // (which lists by doc.json presence) might never collect it.
                if let Err(e) = snapshot_files(&dir, &dst) {
                    let _ = fs::remove_dir_all(&dst);
                    return Err(e);
                }
                if let Some(lbl) = label {
                    write_checkpoint_label(&dst, lbl)?;
                }
                Ok(json!({"saved": cpid, "label": label, "doc_id": id}))
            }
            CheckpointAction::List => {
                let items: Vec<Value> = list_checkpoints(&cps)?
                    .into_iter()
                    .map(|cpid| -> Result<Value, String> {
                        let label =
                            read_checkpoint_label(&cps.join(&cpid).join("label.txt"), &cpid)?;
                        Ok(json!({"id": cpid, "label": label}))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(json!({"doc_id": id, "checkpoints": items, "count": items.len()}))
            }
            CheckpointAction::Restore => {
                let cpid = checkpoint_id.ok_or("restore needs checkpoint_id")?;
                let listed = list_checkpoints(&cps)?;
                if !listed.iter().any(|candidate| candidate == cpid) {
                    return Err(format!("no checkpoint '{}'", cpid));
                }
                let cp = cps.join(cpid);
                // Stage the snapshot's files beside the live doc FIRST, then
                // swap: the old code deleted the live cels/doc.json and copied
                // into the void, so a mid-copy failure (disk full, perms)
                // destroyed the working document it was meant to rescue.
                let staging = dir.join(".restore-staging");
                let _ = fs::remove_dir_all(&staging);
                if let Err(e) = snapshot_files(&cp, &staging) {
                    let _ = fs::remove_dir_all(&staging);
                    return Err(e);
                }
                // Swap: drop the live pixels, then same-dir renames (atomic on
                // one filesystem) move the staged files into place. Not fully
                // atomic across the TWO renames: a crash in between leaves the
                // doc without a doc.json (headless) — re-run restore to finish
                // the swap; the checkpoint itself is untouched.
                let _ = fs::remove_dir_all(dir.join("cels"));
                for name in ["doc.json", JOURNAL_FILE, "reference.png"] {
                    let _ = fs::remove_file(dir.join(name));
                }
                let swapped = (|| -> std::io::Result<()> {
                    fs::rename(staging.join("cels"), dir.join("cels"))?;
                    fs::rename(staging.join("doc.json"), dir.join("doc.json"))?;
                    for name in [JOURNAL_FILE, "reference.png"] {
                        let staged = staging.join(name);
                        if staged.is_file() {
                            fs::rename(staged, dir.join(name))?;
                        }
                    }
                    Ok(())
                })();
                let _ = fs::remove_dir_all(&staging);
                swapped.map_err(|e| {
                    format!("restore staged but the swap failed ({e}) — re-run restore to retry")
                })?;
                Ok(json!({"restored": cpid, "doc_id": id}))
            }
            CheckpointAction::Prune => match checkpoint_id {
                Some(cpid) => {
                    let cp = cps.join(cpid);
                    let _ = fs::remove_dir_all(&cp);
                    Ok(json!({"pruned": cpid, "doc_id": id}))
                }
                None => {
                    let _ = fs::remove_dir_all(&cps);
                    Ok(json!({"pruned": "all", "doc_id": id}))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-checkpoint-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn checkpoint_id_cannot_escape_the_store() {
        let s = studio("cp-escape");
        let created = s.doc_new("c", 8, 8).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap();

        // A directory outside the store that must survive every attempt.
        let outside = std::env::temp_dir().join("atelier-test-cp-escape-victim");
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();

        for evil in [
            "../../../../atelier-test-cp-escape-victim",
            "../..",
            "..",
            "/tmp",
            "cp1/../../..",
            "not-a-checkpoint",
            "cp",
            "cp0",
            "cp00",
            "cp01",
            "cp1x",
            "cp4294967296",
        ] {
            for action in [CheckpointAction::Prune, CheckpointAction::Restore] {
                let r = s.checkpoint(id, action, None, Some(evil));
                assert!(
                    r.is_err(),
                    "{action:?} accepted the traversal id {evil:?}: {r:?}"
                );
            }
        }
        assert!(outside.exists(), "a checkpoint id escaped the store");
        let _ = std::fs::remove_dir_all(&outside);

        // The real id still works.
        assert!(
            s.checkpoint(id, CheckpointAction::Restore, None, Some("cp1"))
                .is_ok()
        );
    }

    #[test]
    fn checkpoint_directory_ids_must_be_valid_and_prunable() {
        for invalid_id in ["not-prunable", "cp0", "cp00", "cp01", "cp4294967296"] {
            let s = studio(&format!("cp-directory-id-{invalid_id}"));
            let created = s.doc_new("c", 2, 2).unwrap();
            let id = created["doc_id"].as_str().unwrap();
            let invalid = s.doc_dir(id).join(".checkpoints").join(invalid_id);
            fs::create_dir_all(&invalid).unwrap();
            fs::write(invalid.join("doc.json"), b"{}").unwrap();

            for action in [CheckpointAction::List, CheckpointAction::Save] {
                let error = s.checkpoint(id, action, None, None).unwrap_err();
                assert!(
                    error.contains(&format!("invalid checkpoint directory id '{invalid_id}'"))
                        && error.contains("expected the cp<n> form"),
                    "got: {error}"
                );
            }

            s.checkpoint(id, CheckpointAction::Prune, None, None)
                .unwrap();
            assert!(s.checkpoint(id, CheckpointAction::Save, None, None).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_copy_refuses_a_link_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let s = studio("cp-copy-link");
        let outside = s.docs_dir.join("outside-copy-source");
        let linked = s.docs_dir.join("linked-copy-source");
        let destination = s.docs_dir.join("copied-target");
        fs::write(&outside, b"must remain untouched").unwrap();
        symlink(&outside, &linked).unwrap();

        let error =
            copy_managed_regular_file(&linked, &destination, "snapshot fixture").unwrap_err();
        assert!(
            error.contains("symlinks and special files are refused"),
            "got: {error}"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"must remain untouched");
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_restore_refuses_a_linked_checkpoint_directory() {
        use std::os::unix::fs::symlink;

        let s = studio("cp-restore-link");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let outside = s.docs_dir.join("outside-checkpoint");
        snapshot_files(&s.doc_dir(id), &outside).unwrap();
        let checkpoints = s.doc_dir(id).join(".checkpoints");
        fs::create_dir(&checkpoints).unwrap();
        symlink(&outside, checkpoints.join("cp1")).unwrap();

        let error = s
            .checkpoint(id, CheckpointAction::Restore, None, Some("cp1"))
            .unwrap_err();
        assert!(
            error.contains("must be a real directory") && error.contains("symlinks"),
            "got: {error}"
        );
        assert!(outside.join("doc.json").is_file());
    }

    #[test]
    fn checkpoint_save_requires_explicit_pruning_at_the_retention_limit() {
        let s = studio("cp-retention");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();

        for n in 1..=MAX_CHECKPOINTS {
            let saved = s
                .checkpoint(id, CheckpointAction::Save, None, None)
                .unwrap();
            assert_eq!(saved["saved"], format!("cp{n}"));
        }

        let checkpoint_dir = s.doc_dir(id).join(".checkpoints");
        let error = s
            .checkpoint(id, CheckpointAction::Save, Some("must not evict"), None)
            .unwrap_err();
        assert!(error.contains("maximum 32 checkpoints"), "got: {error}");
        assert!(checkpoint_dir.join("cp1").join("doc.json").is_file());
        assert!(!checkpoint_dir.join("cp33").exists());
        assert_eq!(
            s.checkpoint(id, CheckpointAction::List, None, None)
                .unwrap()["count"],
            MAX_CHECKPOINTS
        );

        s.checkpoint(id, CheckpointAction::Prune, None, Some("cp1"))
            .unwrap();
        let saved = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap();
        assert_eq!(saved["saved"], "cp33");
        assert_eq!(
            s.checkpoint(id, CheckpointAction::List, None, None)
                .unwrap()["count"],
            MAX_CHECKPOINTS
        );
    }

    #[test]
    fn checkpoint_space_quota_rejects_a_sparse_new_snapshot_before_writes() {
        let s = studio("cp-space-new");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let doc_dir = s.doc_dir(id);
        let sparse = fs::File::create(doc_dir.join("reference.png")).unwrap();
        sparse.set_len(MAX_CHECKPOINT_LOGICAL_BYTES).unwrap();

        let projected =
            managed_snapshot_logical_bytes(&doc_dir, "live document", false, MAX_DOCUMENT_CELS)
                .unwrap();
        let error = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap_err();
        assert!(error.contains("current=0 bytes"), "got: {error}");
        assert!(
            error.contains(&format!("projected={projected} bytes")),
            "got: {error}"
        );
        assert!(
            error.contains("limit=2147483648 bytes")
                && error.contains("prune checkpoints before saving another"),
            "got: {error}"
        );
        assert!(
            !doc_dir.join(".checkpoints").exists(),
            "quota rejection wrote checkpoint state"
        );

        fs::remove_file(doc_dir.join("reference.png")).unwrap();
        assert!(s.checkpoint(id, CheckpointAction::Save, None, None).is_ok());
    }

    #[test]
    fn checkpoint_space_quota_counts_retained_sparse_files_without_eviction() {
        let s = studio("cp-space-retained");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap();

        let checkpoint_root = s.doc_dir(id).join(".checkpoints");
        let cp1 = checkpoint_root.join("cp1");
        let sparse = fs::File::create(cp1.join("reference.png")).unwrap();
        sparse.set_len(MAX_CHECKPOINT_LOGICAL_BYTES).unwrap();
        let ids = vec!["cp1".to_string()];
        let current = retained_checkpoint_logical_bytes(&checkpoint_root, &ids).unwrap();
        let new = managed_snapshot_logical_bytes(
            &s.doc_dir(id),
            "live document",
            false,
            MAX_DOCUMENT_CELS,
        )
        .unwrap();
        let projected = current.checked_add(new).unwrap();

        let error = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap_err();
        assert!(
            error.contains(&format!("current={current} bytes"))
                && error.contains(&format!("projected={projected} bytes"))
                && error.contains("limit=2147483648 bytes"),
            "got: {error}"
        );
        assert!(cp1.join("doc.json").is_file(), "quota failure evicted cp1");
        assert!(!checkpoint_root.join("cp2").exists());

        s.checkpoint(id, CheckpointAction::Prune, None, Some("cp1"))
            .unwrap();
        assert!(s.checkpoint(id, CheckpointAction::Save, None, None).is_ok());
    }

    #[test]
    fn checkpoint_space_accounting_and_cel_traversal_are_checked_and_bounded() {
        let overflow = checked_logical_add(u64::MAX, 1, "overflow fixture").unwrap_err();
        assert!(overflow.contains("accounting overflow"), "got: {overflow}");
        assert_eq!(
            enforce_checkpoint_logical_quota("fixture", MAX_CHECKPOINT_LOGICAL_BYTES, 0).unwrap(),
            MAX_CHECKPOINT_LOGICAL_BYTES
        );
        let over_limit =
            enforce_checkpoint_logical_quota("fixture", MAX_CHECKPOINT_LOGICAL_BYTES, 1)
                .unwrap_err();
        assert!(
            over_limit.contains("current=2147483648 bytes")
                && over_limit.contains("projected=2147483649 bytes"),
            "got: {over_limit}"
        );

        let s = studio("cp-space-traversal");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        fs::write(s.doc_dir(id).join("cels/quota-fixture.png"), []).unwrap();
        let error = managed_snapshot_logical_bytes(&s.doc_dir(id), "bounded fixture", false, 0)
            .unwrap_err();
        assert!(
            error.contains("more than 0 cel entries") && error.contains("traversal is bounded"),
            "got: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_space_scan_refuses_linked_and_special_managed_files() {
        use std::os::unix::{fs::symlink, net::UnixListener};

        let s = studio("cp-space-managed-files");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let doc_dir = s.doc_dir(id);
        let cels = doc_dir.join("cels");
        let outside = s.docs_dir.join("outside-cel");
        fs::write(&outside, b"must remain untouched").unwrap();
        let linked = cels.join("linked.png");
        symlink(&outside, &linked).unwrap();

        let link_error = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap_err();
        assert!(
            link_error.contains("symlinks and special files are refused"),
            "got: {link_error}"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"must remain untouched");
        assert!(!doc_dir.join(".checkpoints").exists());

        fs::remove_file(&linked).unwrap();
        s.checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap();
        let checkpoint_link = doc_dir.join(".checkpoints/cp1/cels").join("linked.png");
        symlink(&outside, &checkpoint_link).unwrap();
        let retained_link_error = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap_err();
        assert!(
            retained_link_error.contains("checkpoint 'cp1'")
                && retained_link_error.contains("symlinks and special files are refused"),
            "got: {retained_link_error}"
        );
        assert_eq!(fs::read(&outside).unwrap(), b"must remain untouched");
        assert!(!doc_dir.join(".checkpoints/cp2").exists());
        fs::remove_file(checkpoint_link).unwrap();

        let socket_path = cels.join("special.socket");
        let socket = UnixListener::bind(&socket_path).unwrap();
        let special_error = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap_err();
        assert!(
            special_error.contains("symlinks and special files are refused"),
            "got: {special_error}"
        );
        assert!(!doc_dir.join(".checkpoints/cp2").exists());
        drop(socket);
    }

    #[test]
    fn checkpoint_labels_are_bounded_by_utf8_bytes_before_snapshot_writes() {
        let s = studio("cp-label-limit");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let checkpoint_dir = s.doc_dir(id).join(".checkpoints");
        let over_limit = "é".repeat(MAX_CHECKPOINT_LABEL_BYTES / 2 + 1);

        let error = s
            .checkpoint(id, CheckpointAction::Save, Some(&over_limit), None)
            .unwrap_err();
        assert!(
            error.contains("4098 UTF-8 bytes") && error.contains("4096-byte limit"),
            "got: {error}"
        );
        assert!(
            !checkpoint_dir.exists(),
            "an oversized label created checkpoint state"
        );

        let at_limit = "é".repeat(MAX_CHECKPOINT_LABEL_BYTES / 2);
        let saved = s
            .checkpoint(id, CheckpointAction::Save, Some(&at_limit), None)
            .unwrap();
        assert_eq!(saved["saved"], "cp1");
        assert_eq!(
            fs::read(checkpoint_dir.join("cp1").join("label.txt"))
                .unwrap()
                .len(),
            MAX_CHECKPOINT_LABEL_BYTES
        );
        assert_eq!(
            s.checkpoint(id, CheckpointAction::List, None, None)
                .unwrap()["checkpoints"][0]["label"],
            at_limit
        );
    }

    #[test]
    fn checkpoint_label_write_failure_removes_the_partial_snapshot() {
        let s = studio("cp-label-write-failure");
        let checkpoint = s.docs_dir.join("label-write-fixture");
        fs::create_dir_all(checkpoint.join("label.txt")).unwrap();

        let error = write_checkpoint_label(&checkpoint, "important").unwrap_err();
        assert!(
            error.contains("cannot write checkpoint label")
                && error.contains("partial checkpoint was removed"),
            "got: {error}"
        );
        assert!(
            !checkpoint.exists(),
            "failed label persistence left a restorable snapshot"
        );
    }

    #[test]
    fn checkpoint_save_refuses_an_incomplete_destination_until_pruned() {
        let s = studio("cp-incomplete-destination");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let checkpoint = s.doc_dir(id).join(".checkpoints").join("cp1");
        fs::create_dir_all(&checkpoint).unwrap();
        fs::write(checkpoint.join("unmanaged"), b"preserve until prune").unwrap();

        let error = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap_err();
        assert!(
            error.contains("destination for checkpoint 'cp1' already exists")
                && error.contains("prune that incomplete checkpoint"),
            "got: {error}"
        );
        assert_eq!(
            fs::read(checkpoint.join("unmanaged")).unwrap(),
            b"preserve until prune"
        );

        s.checkpoint(id, CheckpointAction::Prune, None, Some("cp1"))
            .unwrap();
        assert!(s.checkpoint(id, CheckpointAction::Save, None, None).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_list_bounds_and_refuses_untrusted_label_files() {
        use std::os::unix::fs::symlink;

        let s = studio("cp-label-read-hardening");
        let created = s.doc_new("c", 2, 2).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.checkpoint(id, CheckpointAction::Save, Some("safe"), None)
            .unwrap();
        let label_path = s
            .doc_dir(id)
            .join(".checkpoints")
            .join("cp1")
            .join("label.txt");

        fs::write(&label_path, vec![b'x'; MAX_CHECKPOINT_LABEL_BYTES + 1]).unwrap();
        let oversized = s
            .checkpoint(id, CheckpointAction::List, None, None)
            .unwrap_err();
        assert!(
            oversized.contains("4097 bytes") && oversized.contains("4096-byte"),
            "got: {oversized}"
        );

        fs::write(&label_path, [0xff]).unwrap();
        let invalid_utf8 = s
            .checkpoint(id, CheckpointAction::List, None, None)
            .unwrap_err();
        assert!(invalid_utf8.contains("not UTF-8"), "got: {invalid_utf8}");

        fs::remove_file(&label_path).unwrap();
        symlink(s.doc_dir(id).join("doc.json"), &label_path).unwrap();
        let linked = s
            .checkpoint(id, CheckpointAction::List, None, None)
            .unwrap_err();
        assert!(linked.contains("symlinks are refused"), "got: {linked}");
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    use image::RgbaImage;

    #[test]
    fn restore_brings_back_complete_checkpointed_state() {
        let root = std::env::temp_dir().join("atelier-craft-restore");
        let _ = fs::remove_dir_all(&root);
        let s = Studio::with_docs_dir(root.clone());
        let created = s.doc_new("c", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_draw(
            id,
            0,
            0,
            None,
            "rect",
            json!({"x0": 0, "y0": 0, "x1": 1, "y1": 1, "color": [200, 0, 0, 255], "fill": true})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();

        let red_ref = root.join("red.png");
        let blue_ref = root.join("blue.png");
        RgbaImage::from_pixel(2, 2, image::Rgba([200, 0, 0, 255]))
            .save(&red_ref)
            .unwrap();
        RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 200, 255]))
            .save(&blue_ref)
            .unwrap();
        s.set_reference(id, red_ref.to_str()).unwrap();
        s.journal_append(
            id,
            crate::ToolName::DocNew,
            &json!({"name": "c", "doc_id": id}),
        )
        .unwrap();

        let cp = s
            .checkpoint(id, CheckpointAction::Save, None, None)
            .unwrap();
        let cpid = cp["saved"].as_str().unwrap().to_string();
        // Wreck every checkpointed state surface, then restore.
        s.doc_draw(
            id,
            0,
            0,
            None,
            "fill_cel",
            json!({"color": [0, 0, 0, 255]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        s.set_reference(id, blue_ref.to_str()).unwrap();
        s.journal_append(
            id,
            crate::ToolName::DocDraw,
            &json!({"doc_id": id, "op": "fill_cel"}),
        )
        .unwrap();

        s.checkpoint(id, CheckpointAction::Restore, None, Some(&cpid))
            .unwrap();
        let px = s.doc_get_pixel(id, Some(0), 0, 0, 0).unwrap();
        assert_eq!(px["rgba"], json!([200, 0, 0, 255]));
        assert_eq!(s.journal(id).unwrap().len(), 1);
        assert_eq!(
            image::open(root.join(id).join("reference.png"))
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [200, 0, 0, 255]
        );
        // The staging dir must not linger.
        assert!(!root.join(id).join(".restore-staging").exists());
    }
}
