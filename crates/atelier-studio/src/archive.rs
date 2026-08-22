//! Deterministic, bounded portable document archives.
//!
//! `.atelierpack` v1 is deliberately uncompressed: the document's PNGs are
//! already compressed, and a simple stream can be inspected and restored with
//! fixed memory. The archive contains one complete document generation,
//! including its recipe, revision, reference, and bounded checkpoint history.

use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use atelier_core::document::Document;
use crc32fast::Hasher;
use serde_json::{Value, json};
use uuid::Uuid;

use super::renameat2::{RENAME_NOREPLACE, renameat2};
use super::store::{parse_journal_file, read_bounded_utf8};
use super::{CommitOutcome, JOURNAL_FILE, REVISION_FILE, Studio};

const ARCHIVE_MAGIC: &[u8; 8] = b"ATLRPACK";
const ARCHIVE_VERSION: u32 = 1;
const DOCUMENT_ID_BYTES: usize = 36;
const ARCHIVE_HEADER_BYTES: u64 = 8 + 4 + DOCUMENT_ID_BYTES as u64 + 4 + 8;
const ARCHIVE_CRC_BYTES: u64 = 4;

const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ARCHIVE_PATH_BYTES: usize = 255;
const MAX_ARCHIVE_CHECKPOINTS: usize = 32;
const MAX_CHECKPOINT_LABEL_BYTES: u64 = 4096;
const MAX_REVISION_BYTES: u64 = 21;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateKind {
    Main,
    Checkpoint,
}

struct SourceEntry {
    archive_path: String,
    source_path: PathBuf,
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl SourceEntry {
    fn new(archive_path: String, source_path: PathBuf) -> Result<Self, String> {
        if archive_path.is_empty() || archive_path.len() > MAX_ARCHIVE_PATH_BYTES {
            return Err(format!(
                "archive path '{}' is {} bytes; limit is {MAX_ARCHIVE_PATH_BYTES}",
                archive_path,
                archive_path.len()
            ));
        }
        validate_archive_path(&archive_path)?;
        let metadata = fs::symlink_metadata(&source_path).map_err(|error| {
            format!(
                "cannot inspect archive source '{}': {error}",
                source_path.display()
            )
        })?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "archive source '{}' must be a regular file (symlinks are refused)",
                source_path.display()
            ));
        }
        if metadata.len() > MAX_ENTRY_BYTES {
            return Err(format!(
                "archive entry '{archive_path}' is {} bytes; limit is {MAX_ENTRY_BYTES}",
                metadata.len()
            ));
        }
        #[cfg(unix)]
        let (device, inode) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        Ok(Self {
            archive_path,
            source_path,
            len: metadata.len(),
            #[cfg(unix)]
            device,
            #[cfg(unix)]
            inode,
        })
    }

    fn open(&self) -> Result<fs::File, String> {
        let path_metadata = fs::symlink_metadata(&self.source_path).map_err(|error| {
            format!(
                "cannot inspect archive source '{}': {error}",
                self.source_path.display()
            )
        })?;
        if !path_metadata.file_type().is_file() {
            return Err(format!(
                "archive source '{}' is no longer a regular file",
                self.source_path.display()
            ));
        }
        if path_metadata.len() != self.len {
            return Err(format!(
                "archive source '{}' changed size while the archive was prepared",
                self.source_path.display()
            ));
        }

        let mut options = fs::File::options();
        options.read(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(0o400000); // O_NOFOLLOW
        }
        let file = options.open(&self.source_path).map_err(|error| {
            format!(
                "cannot open archive source '{}': {error}",
                self.source_path.display()
            )
        })?;
        let opened = file.metadata().map_err(|error| {
            format!(
                "cannot inspect open archive source '{}': {error}",
                self.source_path.display()
            )
        })?;
        if !opened.file_type().is_file() || opened.len() != self.len {
            return Err(format!(
                "archive source '{}' changed while it was opened",
                self.source_path.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if opened.dev() != self.device
                || opened.ino() != self.inode
                || path_metadata.dev() != self.device
                || path_metadata.ino() != self.inode
            {
                return Err(format!(
                    "archive source '{}' was replaced while the archive was prepared",
                    self.source_path.display()
                ));
            }
        }
        Ok(file)
    }
}

impl Studio {
    /// Write one document as a deterministic `.atelierpack` v1 stream.
    ///
    /// The destination must not exist. A temporary file is fully written and
    /// synced before a no-replace rename publishes it in the same directory.
    pub fn pack_document(&self, id: &str, output: &Path) -> Result<Value, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{id}'"));
        }
        ensure_output_absent(output)?;
        let output_parent = output_parent(output)?;
        require_real_directory(output_parent, "archive output directory")?;

        let _lock = self.lock_store_shared()?;
        let document_dir = self.doc_dir(id);
        require_real_directory(&document_dir, "document directory")?;
        refuse_output_inside_document(output_parent, &document_dir)?;

        let entries = collect_document_entries(&document_dir, id)?;
        let (payload_bytes, archive_bytes) = archive_sizes(&entries)?;
        let entry_count = u32::try_from(entries.len())
            .map_err(|_| "archive entry count cannot fit the v1 header".to_string())?;

        let (temporary_path, mut temporary) = create_temporary_archive(output_parent)?;
        let write_result = (|| -> Result<(), String> {
            let mut hasher = Hasher::new();
            write_hashed(&mut temporary, &mut hasher, ARCHIVE_MAGIC)?;
            write_hashed(&mut temporary, &mut hasher, &ARCHIVE_VERSION.to_le_bytes())?;
            write_hashed(&mut temporary, &mut hasher, id.as_bytes())?;
            write_hashed(&mut temporary, &mut hasher, &entry_count.to_le_bytes())?;
            write_hashed(&mut temporary, &mut hasher, &payload_bytes.to_le_bytes())?;

            let mut buffer = [0u8; COPY_BUFFER_BYTES];
            for entry in &entries {
                let path_len = u16::try_from(entry.archive_path.len())
                    .map_err(|_| format!("archive path '{}' is too long", entry.archive_path))?;
                write_hashed(&mut temporary, &mut hasher, &path_len.to_le_bytes())?;
                write_hashed(&mut temporary, &mut hasher, entry.archive_path.as_bytes())?;
                write_hashed(&mut temporary, &mut hasher, &entry.len.to_le_bytes())?;

                let mut source = entry.open()?;
                let mut remaining = entry.len;
                while remaining != 0 {
                    let wanted =
                        usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
                    source.read_exact(&mut buffer[..wanted]).map_err(|error| {
                        format!(
                            "cannot read archive entry '{}' completely: {error}",
                            entry.archive_path
                        )
                    })?;
                    write_hashed(&mut temporary, &mut hasher, &buffer[..wanted])?;
                    remaining -= wanted as u64;
                }
                let mut extra = [0u8; 1];
                if source.read(&mut extra).map_err(|error| {
                    format!(
                        "cannot finish archive entry '{}': {error}",
                        entry.archive_path
                    )
                })? != 0
                {
                    return Err(format!(
                        "archive source '{}' grew while it was read",
                        entry.source_path.display()
                    ));
                }
            }

            temporary
                .write_all(&hasher.finalize().to_le_bytes())
                .map_err(|error| format!("cannot write archive checksum: {error}"))?;
            let written = temporary
                .metadata()
                .map_err(|error| format!("cannot inspect temporary archive: {error}"))?
                .len();
            if written != archive_bytes {
                return Err(format!(
                    "internal archive length mismatch: wrote {written} bytes, expected {archive_bytes}"
                ));
            }
            temporary
                .sync_all()
                .map_err(|error| format!("cannot sync temporary archive: {error}"))
        })();
        drop(temporary);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }

        if let Err(error) = rename_noreplace(&temporary_path, output) {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }
        let directory_sync = sync_directory(output_parent);

        let mut report = json!({
            "ok": true,
            "doc_id": id,
            "path": output.display().to_string(),
            "entries": entries.len(),
            "payload_bytes": payload_bytes,
        });
        if let Err(error) = directory_sync {
            report["warning"] = json!(format!(
                "archive was published, but its directory sync failed ({error}); verify the file before removing another backup"
            ));
        }
        Ok(report)
    }

    /// Restore one `.atelierpack` while preserving its document UUID.
    ///
    /// Existing ids are refused unless `replace` is explicit. Replacement is
    /// a same-filesystem atomic exchange after the complete staged tree has
    /// passed document, recipe, revision, reference, cel, and checkpoint
    /// validation.
    pub fn unpack_document(&self, input: &Path, replace: bool) -> Result<Value, String> {
        let (mut input_file, input_len) = open_bounded_archive(input)?;
        let mut hasher = Hasher::new();

        let magic = read_hashed_array::<8>(&mut input_file, &mut hasher, "archive magic")?;
        if &magic != ARCHIVE_MAGIC {
            return Err("not an Atelier archive: expected ATLRPACK magic".into());
        }
        let version = u32::from_le_bytes(read_hashed_array::<4>(
            &mut input_file,
            &mut hasher,
            "archive version",
        )?);
        if version != ARCHIVE_VERSION {
            return Err(format!(
                "unsupported Atelier archive version {version} (this build supports {ARCHIVE_VERSION})"
            ));
        }
        let id_bytes =
            read_hashed_array::<DOCUMENT_ID_BYTES>(&mut input_file, &mut hasher, "document id")?;
        let id = std::str::from_utf8(&id_bytes)
            .map_err(|error| format!("archive document id is not UTF-8: {error}"))?;
        if !Self::valid_id(id) {
            return Err(format!(
                "archive document id '{id}' is not a canonical lowercase UUIDv4"
            ));
        }
        let entry_count = u32::from_le_bytes(read_hashed_array::<4>(
            &mut input_file,
            &mut hasher,
            "entry count",
        )?) as usize;
        if entry_count > MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "archive has {entry_count} entries; limit is {MAX_ARCHIVE_ENTRIES}"
            ));
        }
        let declared_payload = u64::from_le_bytes(read_hashed_array::<8>(
            &mut input_file,
            &mut hasher,
            "payload byte count",
        )?);
        if declared_payload > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "archive declares {declared_payload} payload bytes; total archive limit is {MAX_ARCHIVE_BYTES} bytes"
            ));
        }

        let _lock = self.lock_store_exclusive()?;
        self.cleanup_stale_transactions()?;
        let (transaction, replaced) = self.begin_import_transaction(id, replace)?;
        let staged_dir = transaction.studio().doc_dir(id);
        fs::create_dir(&staged_dir).map_err(|error| {
            format!(
                "cannot create staged document '{}': {error}",
                staged_dir.display()
            )
        })?;

        let mut previous_path: Option<String> = None;
        let mut payload_bytes = 0u64;
        let mut checkpoint_ids = HashSet::new();
        let mut manifest = Vec::with_capacity(entry_count);
        let mut buffer = [0u8; COPY_BUFFER_BYTES];

        for index in 0..entry_count {
            let path_len = u16::from_le_bytes(read_hashed_array::<2>(
                &mut input_file,
                &mut hasher,
                "entry path length",
            )?) as usize;
            if path_len == 0 || path_len > MAX_ARCHIVE_PATH_BYTES {
                return Err(format!(
                    "archive entry {} path is {path_len} bytes; limit is 1..={MAX_ARCHIVE_PATH_BYTES}",
                    index + 1
                ));
            }
            let mut path_bytes = vec![0u8; path_len];
            read_hashed_exact(&mut input_file, &mut hasher, &mut path_bytes, "entry path")?;
            let archive_path = String::from_utf8(path_bytes).map_err(|error| {
                format!("archive entry {} path is not UTF-8: {error}", index + 1)
            })?;
            let checkpoint = validate_archive_path(&archive_path)?;
            if let Some(previous) = &previous_path
                && archive_path <= *previous
            {
                return Err(format!(
                    "archive entries are not in strict path order: '{archive_path}' follows '{previous}'"
                ));
            }
            previous_path = Some(archive_path.clone());
            if let Some(checkpoint) = checkpoint {
                checkpoint_ids.insert(checkpoint.to_owned());
                if checkpoint_ids.len() > MAX_ARCHIVE_CHECKPOINTS {
                    return Err(format!(
                        "archive has more than {MAX_ARCHIVE_CHECKPOINTS} checkpoints"
                    ));
                }
            }

            let content_len = u64::from_le_bytes(read_hashed_array::<8>(
                &mut input_file,
                &mut hasher,
                "entry content length",
            )?);
            if content_len > MAX_ENTRY_BYTES {
                return Err(format!(
                    "archive entry '{archive_path}' is {content_len} bytes; limit is {MAX_ENTRY_BYTES}"
                ));
            }
            payload_bytes = payload_bytes.checked_add(content_len).ok_or_else(|| {
                "archive payload length overflowed the supported range".to_string()
            })?;
            if payload_bytes > declared_payload || payload_bytes > MAX_ARCHIVE_BYTES {
                return Err(format!(
                    "archive payload exceeds its declared {declared_payload}-byte length"
                ));
            }

            let destination = staged_dir.join(&archive_path);
            let parent = destination
                .parent()
                .ok_or_else(|| format!("archive entry '{archive_path}' has no parent"))?;
            fs::create_dir_all(parent).map_err(|error| {
                format!("cannot create directory for archive entry '{archive_path}': {error}")
            })?;
            let mut options = fs::File::options();
            options.write(true).create_new(true);
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(0o400000); // O_NOFOLLOW
            }
            let mut destination_file = options.open(&destination).map_err(|error| {
                format!("cannot create archive entry '{archive_path}': {error}")
            })?;
            let mut remaining = content_len;
            while remaining != 0 {
                let wanted =
                    usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
                read_hashed_exact(
                    &mut input_file,
                    &mut hasher,
                    &mut buffer[..wanted],
                    &format!("content for '{archive_path}'"),
                )?;
                destination_file
                    .write_all(&buffer[..wanted])
                    .map_err(|error| {
                        format!("cannot write archive entry '{archive_path}': {error}")
                    })?;
                remaining -= wanted as u64;
            }
            manifest.push((archive_path, content_len));
        }

        if payload_bytes != declared_payload {
            return Err(format!(
                "archive payload is {payload_bytes} bytes, but the header declares {declared_payload}"
            ));
        }
        let expected_crc =
            u32::from_le_bytes(read_raw_array::<4>(&mut input_file, "archive checksum")?);
        let actual_crc = hasher.finalize();
        if actual_crc != expected_crc {
            return Err(format!(
                "archive checksum mismatch: expected {expected_crc:08x}, calculated {actual_crc:08x}"
            ));
        }
        let mut trailing = [0u8; 1];
        if input_file
            .read(&mut trailing)
            .map_err(|error| format!("cannot check archive end: {error}"))?
            != 0
        {
            return Err("archive contains trailing bytes after its checksum".into());
        }

        let actual_entries = collect_document_entries(&staged_dir, id)?;
        let actual_manifest: Vec<(String, u64)> = actual_entries
            .into_iter()
            .map(|entry| (entry.archive_path, entry.len))
            .collect();
        if actual_manifest != manifest {
            return Err("staged archive tree does not match its entry manifest".into());
        }

        let outcome = transaction.commit(id)?;
        let mut report = json!({
            "ok": true,
            "doc_id": id,
            "path": input.display().to_string(),
            "entries": entry_count,
            "payload_bytes": payload_bytes,
            "replaced": replaced,
        });
        if let CommitOutcome::DurabilityUncertain { warning } = outcome {
            report["warning"] = json!(warning);
        }

        debug_assert!(input_len <= MAX_ARCHIVE_BYTES);
        Ok(report)
    }
}

fn collect_document_entries(document_dir: &Path, id: &str) -> Result<Vec<SourceEntry>, String> {
    let mut entries = Vec::new();
    collect_state_entries(document_dir, "", id, StateKind::Main, &mut entries)?;
    entries.sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
    if entries.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "document archive has {} entries; limit is {MAX_ARCHIVE_ENTRIES}",
            entries.len()
        ));
    }
    for pair in entries.windows(2) {
        if pair[0].archive_path == pair[1].archive_path {
            return Err(format!("duplicate archive path '{}'", pair[0].archive_path));
        }
    }
    Ok(entries)
}

fn collect_state_entries(
    root: &Path,
    archive_prefix: &str,
    id: &str,
    kind: StateKind,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), String> {
    require_real_directory(root, "document state directory")?;
    let document = Document::load(root)
        .map_err(|error| format!("invalid document state '{}': {error}", root.display()))?;
    let expected_cels: HashSet<String> = document
        .meta()
        .cels
        .iter()
        .map(|cel| cel.file.clone())
        .collect();
    let expects_reference = document.meta().reference.as_deref() == Some("reference.png");
    drop(document);

    let mut saw_doc = false;
    let mut saw_reference = false;
    let mut saw_cels = false;
    for entry in read_directory_sorted(root, "document state directory")? {
        let name = utf8_file_name(&entry)?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect '{}': {error}", entry.path().display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "document entry '{}' is a symbolic link; portable archives refuse links",
                entry.path().display()
            ));
        }

        match name.as_str() {
            "doc.json" if file_type.is_file() => {
                saw_doc = true;
                push_source(entries, archive_prefix, &name, entry.path())?;
            }
            JOURNAL_FILE if file_type.is_file() => {
                let parsed = parse_journal_file(id, &entry.path())
                    .map_err(|error| format!("invalid journal in '{}': {error}", root.display()))?;
                if parsed.torn_tail {
                    return Err(format!(
                        "journal in '{}' has an incomplete final line",
                        root.display()
                    ));
                }
                push_source(entries, archive_prefix, &name, entry.path())?;
            }
            REVISION_FILE if kind == StateKind::Main && file_type.is_file() => {
                validate_revision(&entry.path())?;
                push_source(entries, archive_prefix, &name, entry.path())?;
            }
            "reference.png" if file_type.is_file() => {
                if !expects_reference {
                    return Err(format!(
                        "reference.png exists in '{}', but doc.json does not name it",
                        root.display()
                    ));
                }
                validate_image(&entry.path(), "stored reference")?;
                saw_reference = true;
                push_source(entries, archive_prefix, &name, entry.path())?;
            }
            "label.txt" if kind == StateKind::Checkpoint && file_type.is_file() => {
                read_bounded_utf8(
                    &entry.path(),
                    "checkpoint label",
                    MAX_CHECKPOINT_LABEL_BYTES,
                )?;
                push_source(entries, archive_prefix, &name, entry.path())?;
            }
            "cels" if file_type.is_dir() => {
                saw_cels = true;
                collect_cels(&entry.path(), archive_prefix, &expected_cels, entries)?;
            }
            ".checkpoints" if kind == StateKind::Main && file_type.is_dir() => {
                collect_checkpoints(&entry.path(), id, entries)?;
            }
            _ => {
                let kind = if file_type.is_dir() {
                    "directory"
                } else if file_type.is_file() {
                    "file"
                } else {
                    "special entry"
                };
                return Err(format!(
                    "unknown {kind} '{}' is outside the portable archive grammar",
                    entry.path().display()
                ));
            }
        }
    }

    if !saw_doc {
        return Err(format!(
            "document state '{}' has no doc.json",
            root.display()
        ));
    }
    if expects_reference && !saw_reference {
        return Err(format!(
            "document state '{}' names a missing reference.png",
            root.display()
        ));
    }
    if !expected_cels.is_empty() && !saw_cels {
        return Err(format!(
            "document state '{}' names cels but has no cels directory",
            root.display()
        ));
    }
    Ok(())
}

fn collect_checkpoints(
    checkpoints_dir: &Path,
    id: &str,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), String> {
    require_real_directory(checkpoints_dir, "checkpoint directory")?;
    let checkpoints = read_directory_sorted(checkpoints_dir, "checkpoint directory")?;
    if checkpoints.len() > MAX_ARCHIVE_CHECKPOINTS {
        return Err(format!(
            "document has {} checkpoints; archive limit is {MAX_ARCHIVE_CHECKPOINTS}",
            checkpoints.len()
        ));
    }
    for checkpoint in checkpoints {
        let checkpoint_id = utf8_file_name(&checkpoint)?;
        if !is_checkpoint_id(&checkpoint_id) {
            return Err(format!(
                "unknown checkpoint entry '{}' is outside the cp<n> grammar",
                checkpoint.path().display()
            ));
        }
        let file_type = checkpoint.file_type().map_err(|error| {
            format!("cannot inspect '{}': {error}", checkpoint.path().display())
        })?;
        if !file_type.is_dir() {
            return Err(format!(
                "checkpoint '{}' must be a real directory (symlinks and special files are refused)",
                checkpoint.path().display()
            ));
        }
        let prefix = format!(".checkpoints/{checkpoint_id}");
        collect_state_entries(
            &checkpoint.path(),
            &prefix,
            id,
            StateKind::Checkpoint,
            entries,
        )?;
    }
    Ok(())
}

fn collect_cels(
    cels_dir: &Path,
    archive_prefix: &str,
    expected_cels: &HashSet<String>,
    entries: &mut Vec<SourceEntry>,
) -> Result<(), String> {
    require_real_directory(cels_dir, "cels directory")?;
    let mut actual = HashSet::new();
    for cel in read_directory_sorted(cels_dir, "cels directory")? {
        let name = utf8_file_name(&cel)?;
        let relative = format!("cels/{name}");
        let file_type = cel
            .file_type()
            .map_err(|error| format!("cannot inspect '{}': {error}", cel.path().display()))?;
        if !file_type.is_file() || !is_cel_name(&name) {
            return Err(format!(
                "cel entry '{}' must be a regular L<n>_F<n>.png file (symlinks are refused)",
                cel.path().display()
            ));
        }
        if !expected_cels.contains(&relative) {
            return Err(format!(
                "cel '{}' is not referenced by doc.json",
                cel.path().display()
            ));
        }
        actual.insert(relative.clone());
        push_source(entries, archive_prefix, &relative, cel.path())?;
    }
    if actual != *expected_cels {
        let mut missing: Vec<_> = expected_cels.difference(&actual).cloned().collect();
        missing.sort();
        return Err(format!(
            "cels directory is missing doc.json entries: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn push_source(
    entries: &mut Vec<SourceEntry>,
    prefix: &str,
    relative: &str,
    source: PathBuf,
) -> Result<(), String> {
    if entries.len() >= MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "document archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry limit"
        ));
    }
    let archive_path = if prefix.is_empty() {
        relative.to_owned()
    } else {
        format!("{prefix}/{relative}")
    };
    entries.push(SourceEntry::new(archive_path, source)?);
    Ok(())
}

/// Validate one entry name and return its checkpoint id, when applicable.
fn validate_archive_path(path: &str) -> Result<Option<&str>, String> {
    if !path.is_ascii() || path.len() > MAX_ARCHIVE_PATH_BYTES {
        return Err(format!(
            "archive path '{path}' must be ASCII and at most {MAX_ARCHIVE_PATH_BYTES} bytes"
        ));
    }
    if matches!(
        path,
        "doc.json" | JOURNAL_FILE | REVISION_FILE | "reference.png"
    ) {
        return Ok(None);
    }
    if let Some(name) = path.strip_prefix("cels/")
        && !name.contains('/')
        && is_cel_name(name)
    {
        return Ok(None);
    }

    let parts: Vec<_> = path.split('/').collect();
    if parts.len() >= 3
        && parts[0] == ".checkpoints"
        && is_checkpoint_id(parts[1])
        && (matches!(
            parts.as_slice(),
            [
                _,
                _,
                "doc.json" | "recipe.jsonl" | "reference.png" | "label.txt"
            ]
        ) || (parts.len() == 4 && parts[2] == "cels" && is_cel_name(parts[3])))
    {
        return Ok(Some(parts[1]));
    }
    Err(format!(
        "archive path '{path}' is outside the closed `.atelierpack` grammar"
    ))
}

fn is_checkpoint_id(value: &str) -> bool {
    value
        .strip_prefix("cp")
        .is_some_and(|number| is_canonical_decimal(number) && number != "0")
}

fn is_cel_name(value: &str) -> bool {
    let Some(body) = value
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(".png"))
    else {
        return false;
    };
    let Some((layer, frame)) = body.split_once("_F") else {
        return false;
    };
    is_canonical_decimal(layer) && is_canonical_decimal(frame)
}

fn is_canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && value.parse::<usize>().is_ok()
}

fn validate_revision(path: &Path) -> Result<(), String> {
    let source = read_bounded_utf8(path, REVISION_FILE, MAX_REVISION_BYTES)?;
    let digits = source.strip_suffix('\n').unwrap_or(&source);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "revision '{}' must contain one unsigned decimal integer",
            path.display()
        ));
    }
    digits.parse::<u64>().map(|_| ()).map_err(|error| {
        format!(
            "revision '{}' is outside the u64 range: {error}",
            path.display()
        )
    })
}

fn validate_image(path: &Path, label: &str) -> Result<(), String> {
    let source = SourceEntry::new("reference.png".into(), path.to_path_buf())?;
    let reader = image::ImageReader::new(BufReader::new(source.open()?))
        .with_guessed_format()
        .map_err(|error| format!("cannot identify {label} '{}': {error}", path.display()))?;
    let dimensions = reader
        .into_dimensions()
        .map_err(|error| format!("cannot read {label} '{}': {error}", path.display()))?;
    atelier_core::raster::checked_rgba_dimensions(
        label,
        u64::from(dimensions.0),
        u64::from(dimensions.1),
    )?;
    let mut reader = image::ImageReader::new(BufReader::new(source.open()?))
        .with_guessed_format()
        .map_err(|error| format!("cannot identify {label} '{}': {error}", path.display()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(dimensions.0);
    limits.max_image_height = Some(dimensions.1);
    reader.limits(limits);
    reader
        .decode()
        .map(|_| ())
        .map_err(|error| format!("cannot decode {label} '{}': {error}", path.display()))
}

fn archive_sizes(entries: &[SourceEntry]) -> Result<(u64, u64), String> {
    let mut payload = 0u64;
    let mut total = ARCHIVE_HEADER_BYTES + ARCHIVE_CRC_BYTES;
    for entry in entries {
        payload = payload
            .checked_add(entry.len)
            .ok_or_else(|| "archive payload length overflowed".to_string())?;
        let encoded = 2u64
            .checked_add(entry.archive_path.len() as u64)
            .and_then(|value| value.checked_add(8))
            .and_then(|value| value.checked_add(entry.len))
            .ok_or_else(|| "archive encoded length overflowed".to_string())?;
        total = total
            .checked_add(encoded)
            .ok_or_else(|| "archive encoded length overflowed".to_string())?;
    }
    if total > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "archive would be {total} bytes; limit is {MAX_ARCHIVE_BYTES}"
        ));
    }
    Ok((payload, total))
}

fn read_directory_sorted(path: &Path, label: &str) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|error| format!("cannot read {label} '{}': {error}", path.display()))?
    {
        if entries.len() == MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "{label} '{}' has more than {} entries",
                path.display(),
                MAX_ARCHIVE_ENTRIES
            ));
        }
        entries
            .push(entry.map_err(|error| {
                format!("cannot inspect {label} '{}': {error}", path.display())
            })?);
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn utf8_file_name(entry: &fs::DirEntry) -> Result<String, String> {
    entry.file_name().into_string().map_err(|_| {
        format!(
            "document entry '{}' has a non-UTF-8 name",
            entry.path().display()
        )
    })
}

fn require_real_directory(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {label} '{}': {error}", path.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "{label} '{}' must be a real directory (symlinks are refused)",
            path.display()
        ));
    }
    Ok(())
}

fn ensure_output_absent(output: &Path) -> Result<(), String> {
    if output.file_name().is_none() {
        return Err("archive output must name a file".into());
    }
    match fs::symlink_metadata(output) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect archive output '{}': {error}",
            output.display()
        )),
        Ok(_) => Err(format!(
            "refusing to overwrite existing archive output '{}'",
            output.display()
        )),
    }
}

fn output_parent(output: &Path) -> Result<&Path, String> {
    output
        .parent()
        .map(|parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        })
        .ok_or_else(|| format!("archive output '{}' has no parent", output.display()))
}

fn refuse_output_inside_document(output_parent: &Path, document_dir: &Path) -> Result<(), String> {
    let output_parent = fs::canonicalize(output_parent).map_err(|error| {
        format!(
            "cannot resolve archive output directory '{}': {error}",
            output_parent.display()
        )
    })?;
    let document_dir = fs::canonicalize(document_dir).map_err(|error| {
        format!(
            "cannot resolve document directory '{}': {error}",
            document_dir.display()
        )
    })?;
    if output_parent.starts_with(&document_dir) {
        return Err("archive output must be outside the document being packed".into());
    }
    Ok(())
}

fn create_temporary_archive(parent: &Path) -> Result<(PathBuf, fs::File), String> {
    for _ in 0..32 {
        let path = parent.join(format!(".atelierpack-{}.tmp", Uuid::new_v4()));
        let mut options = fs::File::options();
        options.write(true).create_new(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(0o400000); // O_NOFOLLOW
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot create temporary archive in '{}': {error}",
                    parent.display()
                ));
            }
        }
    }
    Err("cannot allocate a unique temporary archive after 32 attempts".into())
}

fn open_bounded_archive(path: &Path) -> Result<(fs::File, u64), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect archive '{}': {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "archive '{}' must be a regular file (symlinks are refused)",
            path.display()
        ));
    }
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "archive '{}' is {} bytes; limit is {MAX_ARCHIVE_BYTES}",
            path.display(),
            metadata.len()
        ));
    }

    let mut options = fs::File::options();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0o400000); // O_NOFOLLOW
    }
    let file = options
        .open(path)
        .map_err(|error| format!("cannot open archive '{}': {error}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("cannot inspect open archive '{}': {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
            return Err(format!(
                "archive '{}' changed while it was opened",
                path.display()
            ));
        }
    }
    Ok((file, opened.len()))
}

fn write_hashed(file: &mut fs::File, hasher: &mut Hasher, bytes: &[u8]) -> Result<(), String> {
    file.write_all(bytes)
        .map_err(|error| format!("cannot write temporary archive: {error}"))?;
    hasher.update(bytes);
    Ok(())
}

fn read_hashed_exact(
    file: &mut fs::File,
    hasher: &mut Hasher,
    bytes: &mut [u8],
    label: &str,
) -> Result<(), String> {
    file.read_exact(bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    hasher.update(bytes);
    Ok(())
}

fn read_hashed_array<const N: usize>(
    file: &mut fs::File,
    hasher: &mut Hasher,
    label: &str,
) -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    read_hashed_exact(file, hasher, &mut bytes, label)?;
    Ok(bytes)
}

fn read_raw_array<const N: usize>(file: &mut fs::File, label: &str) -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    Ok(bytes)
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn rename_noreplace(source: &Path, destination: &Path) -> Result<(), String> {
    renameat2(source, destination, RENAME_NOREPLACE).map_err(|error| {
        format!(
            "cannot publish archive '{}' without replacement: {}",
            destination.display(),
            error
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use image::{Rgba, RgbaImage};
    use serde_json::{Map, json};

    use super::*;
    use crate::{CheckpointAction, ToolName};

    struct TestArea {
        root: PathBuf,
    }

    impl TestArea {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("atelier-archive-{tag}-{}", Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            Self { root }
        }

        fn studio(&self, name: &str) -> Studio {
            Studio::with_docs_dir(self.root.join(name))
        }
    }

    impl Drop for TestArea {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn make_document(area: &TestArea, studio: &Studio) -> String {
        let created = studio.doc_new("portable", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap().to_owned();

        let mut legend = Map::new();
        legend.insert("x".into(), json!([255, 32, 16, 255]));
        studio
            .doc_paint_grid(&id, 0, 0, 0, 0, legend, vec!["xx".into(), "xx".into()])
            .unwrap();
        studio.set_document_revision(&id, 7).unwrap();
        studio
            .journal_append(
                &id,
                ToolName::DocNew,
                &json!({"doc_id": id, "name": "portable", "w": 4, "h": 4}),
            )
            .unwrap();

        let reference_path = area.root.join("source-reference.png");
        RgbaImage::from_pixel(3, 2, Rgba([3, 7, 11, 255]))
            .save(&reference_path)
            .unwrap();
        studio.set_reference(&id, reference_path.to_str()).unwrap();
        studio
            .checkpoint(&id, CheckpointAction::Save, Some("before polish"), None)
            .unwrap();
        id
    }

    fn test_archive(id: &str, mut entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let payload = entries
            .iter()
            .map(|(_, content)| content.len() as u64)
            .sum::<u64>();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ARCHIVE_MAGIC);
        bytes.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        bytes.extend_from_slice(id.as_bytes());
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload.to_le_bytes());
        for (path, content) in entries {
            bytes.extend_from_slice(&(path.len() as u16).to_le_bytes());
            bytes.extend_from_slice(path.as_bytes());
            bytes.extend_from_slice(&(content.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&content);
        }
        let crc = crc32fast::hash(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes
    }

    fn header_with_counts(id: &str, entries: u32, payload: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(ARCHIVE_MAGIC);
        bytes.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        bytes.extend_from_slice(id.as_bytes());
        bytes.extend_from_slice(&entries.to_le_bytes());
        bytes.extend_from_slice(&payload.to_le_bytes());
        bytes
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn pack_is_deterministic_and_round_trips_complete_state() {
        let area = TestArea::new("roundtrip");
        let source = area.studio("source");
        let id = make_document(&area, &source);
        let first = area.root.join("first.atelierpack");
        let second = area.root.join("second.atelierpack");

        let report = source.pack_document(&id, &first).unwrap();
        source.pack_document(&id, &second).unwrap();
        assert_eq!(report["doc_id"], id);
        assert!(report["entries"].as_u64().unwrap() >= 9);
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());

        let restored = area.studio("restored");
        let unpacked = restored.unpack_document(&first, false).unwrap();
        assert_eq!(unpacked["doc_id"], id);
        assert_eq!(unpacked["replaced"], false);
        assert_eq!(
            restored.doc_info(&id).unwrap(),
            source.doc_info(&id).unwrap()
        );
        assert_eq!(restored.document_revision(&id).unwrap(), 7);
        assert_eq!(restored.journal(&id).unwrap().len(), 1);
        assert_eq!(
            restored
                .checkpoint(&id, CheckpointAction::List, None, None)
                .unwrap()["checkpoints"][0]["label"],
            "before polish"
        );

        let repacked = area.root.join("repacked.atelierpack");
        restored.pack_document(&id, &repacked).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(repacked).unwrap());
    }

    #[test]
    fn collision_is_safe_and_replace_is_explicit_and_atomic() {
        let area = TestArea::new("replace");
        let source = area.studio("source");
        let id = make_document(&area, &source);
        let archive = area.root.join("source.atelierpack");
        source.pack_document(&id, &archive).unwrap();

        let restored = area.studio("restored");
        restored.unpack_document(&archive, false).unwrap();
        restored.set_document_revision(&id, 99).unwrap();
        let collision = restored.unpack_document(&archive, false).unwrap_err();
        assert!(collision.contains("already exists"), "{collision}");
        assert_eq!(restored.document_revision(&id).unwrap(), 99);

        let mut corrupt = fs::read(&archive).unwrap();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0x80;
        let corrupt_path = area.root.join("corrupt.atelierpack");
        write_bytes(&corrupt_path, &corrupt);
        let error = restored.unpack_document(&corrupt_path, true).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert_eq!(restored.document_revision(&id).unwrap(), 99);

        let replaced = restored.unpack_document(&archive, true).unwrap();
        assert_eq!(replaced["replaced"], true);
        assert_eq!(restored.document_revision(&id).unwrap(), 7);
    }

    #[test]
    fn explicit_replace_recovers_a_corrupt_real_document_directory() {
        let area = TestArea::new("replace-corrupt");
        let source = area.studio("source");
        let id = make_document(&area, &source);
        let archive = area.root.join("source.atelierpack");
        source.pack_document(&id, &archive).unwrap();

        let restored = area.studio("restored");
        let corrupt_dir = restored.doc_dir(&id);
        fs::create_dir(&corrupt_dir).unwrap();
        fs::write(corrupt_dir.join("unrecoverable.txt"), b"broken").unwrap();
        assert!(!restored.exists(&id));

        let report = restored.unpack_document(&archive, true).unwrap();
        assert_eq!(report["replaced"], true);
        assert_eq!(restored.document_revision(&id).unwrap(), 7);
        assert!(!corrupt_dir.join("unrecoverable.txt").exists());
    }

    #[test]
    fn unpack_rejects_traversal_trailing_data_and_all_wire_limits() {
        let area = TestArea::new("malformed");
        let restored = area.studio("restored");
        let id = "123e4567-e89b-42d3-a456-426614174000";

        let traversal = area.root.join("traversal.atelierpack");
        write_bytes(
            &traversal,
            &test_archive(id, vec![("../doc.json".into(), b"{}".to_vec())]),
        );
        let error = restored.unpack_document(&traversal, false).unwrap_err();
        assert!(error.contains("closed `.atelierpack` grammar"), "{error}");
        assert!(!restored.exists(id));

        let valid_shape = test_archive(id, vec![("doc.json".into(), b"{}".to_vec())]);
        let trailing = area.root.join("trailing.atelierpack");
        let mut with_trailing = valid_shape;
        with_trailing.push(0);
        write_bytes(&trailing, &with_trailing);
        let error = restored.unpack_document(&trailing, false).unwrap_err();
        assert!(error.contains("trailing bytes"), "{error}");

        let too_many = area.root.join("too-many.atelierpack");
        write_bytes(
            &too_many,
            &header_with_counts(id, MAX_ARCHIVE_ENTRIES as u32 + 1, 0),
        );
        let error = restored.unpack_document(&too_many, false).unwrap_err();
        assert!(error.contains("entries"), "{error}");
        assert!(error.contains("limit"), "{error}");

        let too_large = area.root.join("too-large.atelierpack");
        write_bytes(
            &too_large,
            &header_with_counts(id, 0, MAX_ARCHIVE_BYTES + 1),
        );
        let error = restored.unpack_document(&too_large, false).unwrap_err();
        assert!(error.contains("total archive limit"), "{error}");

        let long_path = area.root.join("long-path.atelierpack");
        let mut bytes = header_with_counts(id, 1, 0);
        bytes.extend_from_slice(&((MAX_ARCHIVE_PATH_BYTES + 1) as u16).to_le_bytes());
        write_bytes(&long_path, &bytes);
        let error = restored.unpack_document(&long_path, false).unwrap_err();
        assert!(error.contains("path is 256 bytes"), "{error}");

        let huge_entry = area.root.join("huge-entry.atelierpack");
        let mut bytes = header_with_counts(id, 1, MAX_ENTRY_BYTES + 1);
        bytes.extend_from_slice(&("doc.json".len() as u16).to_le_bytes());
        bytes.extend_from_slice(b"doc.json");
        bytes.extend_from_slice(&(MAX_ENTRY_BYTES + 1).to_le_bytes());
        write_bytes(&huge_entry, &bytes);
        let error = restored.unpack_document(&huge_entry, false).unwrap_err();
        assert!(error.contains("entry 'doc.json'"), "{error}");

        let checkpoints = area.root.join("checkpoints.atelierpack");
        let checkpoint_entries = (1..=MAX_ARCHIVE_CHECKPOINTS + 1)
            .map(|index| (format!(".checkpoints/cp{index}/label.txt"), Vec::new()))
            .collect();
        write_bytes(&checkpoints, &test_archive(id, checkpoint_entries));
        let error = restored.unpack_document(&checkpoints, false).unwrap_err();
        assert!(error.contains("more than 32 checkpoints"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn pack_refuses_links_unknown_entries_and_overwrite() {
        use std::os::unix::fs::symlink;

        let area = TestArea::new("links");
        let source = area.studio("source");
        let id = make_document(&area, &source);
        let document_dir = source.doc_dir(&id);
        let outside = area.root.join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, document_dir.join("linked-note")).unwrap();
        let archive = area.root.join("source.atelierpack");
        let error = source.pack_document(&id, &archive).unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        fs::remove_file(document_dir.join("linked-note")).unwrap();

        fs::write(document_dir.join("notes.txt"), b"not managed").unwrap();
        let error = source.pack_document(&id, &archive).unwrap_err();
        assert!(error.contains("unknown file"), "{error}");
        fs::remove_file(document_dir.join("notes.txt")).unwrap();

        source.pack_document(&id, &archive).unwrap();
        let before = fs::read(&archive).unwrap();
        let error = source.pack_document(&id, &archive).unwrap_err();
        assert!(error.contains("refusing to overwrite"), "{error}");
        assert_eq!(fs::read(archive).unwrap(), before);
    }
}
