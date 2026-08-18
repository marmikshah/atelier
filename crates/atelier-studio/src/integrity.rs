//! Read-only document-store verification and bounded diagnostic reporting.

use std::collections::{BinaryHeap, HashSet};
use std::fs;
use std::path::Path;

use atelier_core::document::{DocMeta, MAX_DOCUMENT_METADATA_BYTES};
use serde::Serialize;

use super::store::{parse_journal_file, read_bounded_utf8};
use super::{JOURNAL_FILE, REVISION_FILE, Studio};

/// Verification is routinely pointed at old or damaged data. These limits
/// bound verifier allocations and machine-readable output.
const MAX_RETAINED_ISSUES: usize = 256;
const MAX_STORE_ENTRIES: usize = 100_000;
const MAX_TREE_DEPTH: usize = 32;

/// Severity of one finding from [`Studio::verify_store`]. Errors mean a
/// document cannot be loaded or replayed as recorded. Warnings identify store
/// debris that Atelier ignores but an operator may want to inspect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegritySeverity {
    Error,
    Warning,
}

/// One actionable document-store integrity finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IntegrityIssue {
    pub severity: IntegritySeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    pub component: String,
    pub message: String,
    pub action: String,
}

/// Complete read-only verification report for one document store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoreIntegrityReport {
    pub ok: bool,
    pub documents_dir: String,
    pub documents: usize,
    pub cels: usize,
    pub journal_entries: usize,
    pub errors: usize,
    pub warnings: usize,
    pub issues_truncated: bool,
    pub omitted_issues: usize,
    pub issues: Vec<IntegrityIssue>,
}

impl StoreIntegrityReport {
    fn new(documents_dir: &Path) -> Self {
        Self {
            ok: true,
            documents_dir: documents_dir.display().to_string(),
            documents: 0,
            cels: 0,
            journal_entries: 0,
            errors: 0,
            warnings: 0,
            issues_truncated: false,
            omitted_issues: 0,
            issues: Vec::new(),
        }
    }

    fn issue(
        &mut self,
        severity: IntegritySeverity,
        document_id: Option<&str>,
        component: impl Into<String>,
        message: impl Into<String>,
        action: impl Into<String>,
    ) {
        match severity {
            IntegritySeverity::Error => {
                self.errors += 1;
                self.ok = false;
            }
            IntegritySeverity::Warning => self.warnings += 1,
        }
        if self.issues.len() < MAX_RETAINED_ISSUES {
            self.issues.push(IntegrityIssue {
                severity,
                document_id: document_id.map(str::to_string),
                component: component.into(),
                message: message.into(),
                action: action.into(),
            });
        } else {
            self.issues_truncated = true;
            self.omitted_issues += 1;
        }
    }

    /// Account for equivalent findings that were deliberately not materialised
    /// while preserving complete report totals.
    fn omit(&mut self, severity: IntegritySeverity, count: usize) {
        if count == 0 {
            return;
        }
        match severity {
            IntegritySeverity::Error => {
                self.errors += count;
                self.ok = false;
            }
            IntegritySeverity::Warning => self.warnings += count,
        }
        self.issues_truncated = true;
        self.omitted_issues += count;
    }
}

fn is_managed_cel_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix("cels/") else {
        return false;
    };
    let Some(rest) = name
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(".png"))
    else {
        return false;
    };
    let Some((layer, frame)) = rest.split_once("_F") else {
        return false;
    };
    !layer.is_empty()
        && !frame.is_empty()
        && layer.chars().all(|value| value.is_ascii_digit())
        && frame.chars().all(|value| value.is_ascii_digit())
}

/// Validate one persisted image with the same decode budget as document load,
/// while also rejecting links that could make a store depend on outside data.
fn verify_stored_image(path: &Path, require_png: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("stored image is a symbolic link".into());
    }
    if !metadata.is_file() {
        return Err("stored image is not a regular file".into());
    }
    let probe = image::ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    if require_png && probe.format() != Some(image::ImageFormat::Png) {
        return Err("cel is not encoded as PNG".into());
    }
    let dimensions = probe.into_dimensions().map_err(|error| error.to_string())?;
    atelier_core::raster::checked_rgba_dimensions(
        "stored image",
        dimensions.0 as u64,
        dimensions.1 as u64,
    )?;
    let mut reader = image::ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(dimensions.0);
    limits.max_image_height = Some(dimensions.1);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())?;
    Ok(())
}

impl Studio {
    /// Verify every document that the store can address without changing any
    /// files. The shared store lock prevents an atomic transaction swap from
    /// mixing generations while metadata, cels, and journals are inspected.
    pub fn verify_store(&self) -> Result<StoreIntegrityReport, String> {
        let _lock = self.lock_store_shared()?;
        let mut report = StoreIntegrityReport::new(&self.docs_dir);
        let read_dir = fs::read_dir(&self.docs_dir).map_err(|error| {
            format!(
                "cannot read document store {}: {error}",
                self.docs_dir.display()
            )
        })?;
        let mut entries = Vec::new();
        for entry in read_dir {
            match entry {
                Ok(entry) if entries.len() < MAX_STORE_ENTRIES => entries.push(entry),
                Ok(_) => {
                    report.issue(
                        IntegritySeverity::Error,
                        None,
                        "store",
                        format!("store contains more than {MAX_STORE_ENTRIES} entries"),
                        "archive or split the store before running verification again",
                    );
                    break;
                }
                Err(error) => report.issue(
                    IntegritySeverity::Error,
                    None,
                    "store",
                    format!("cannot inspect a store entry: {error}"),
                    "check ownership and read permissions on the documents directory",
                ),
            }
        }
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".store.lock" {
                match entry.file_type() {
                    Ok(file_type) if file_type.is_file() => {}
                    Ok(_) => report.issue(
                        IntegritySeverity::Error,
                        None,
                        ".store.lock",
                        "store lock is not a regular file",
                        "stop Atelier and replace it with a regular lock file",
                    ),
                    Err(error) => report.issue(
                        IntegritySeverity::Error,
                        None,
                        ".store.lock",
                        format!("cannot inspect store lock: {error}"),
                        "check ownership and permissions for the store lock",
                    ),
                }
                continue;
            }
            if name == ".transactions" {
                self.verify_transaction_area(&entry.path(), &mut report);
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    report.issue(
                        IntegritySeverity::Error,
                        Some(&name),
                        "directory",
                        format!("cannot inspect entry type: {error}"),
                        "check ownership and permissions for this entry",
                    );
                    continue;
                }
            };
            if !Self::valid_id(&name) {
                let looks_like_document =
                    file_type.is_dir() && entry.path().join("doc.json").exists();
                report.issue(
                    if looks_like_document {
                        IntegritySeverity::Error
                    } else {
                        IntegritySeverity::Warning
                    },
                    Some(&name),
                    "directory",
                    if looks_like_document {
                        "document directory is not named with a canonical UUIDv4"
                    } else {
                        "unrecognized entry is ignored by Atelier"
                    },
                    if looks_like_document {
                        "restore or import this document through Atelier; renaming it alone will not repair its journal stamp"
                    } else {
                        "move the entry outside the documents directory after confirming it is not needed"
                    },
                );
                continue;
            }

            report.documents += 1;
            if file_type.is_symlink() {
                report.issue(
                    IntegritySeverity::Error,
                    Some(&name),
                    "directory",
                    "document directory is a symbolic link",
                    "replace the link with a real document directory inside this store",
                );
                continue;
            }
            if !file_type.is_dir() {
                report.issue(
                    IntegritySeverity::Error,
                    Some(&name),
                    "directory",
                    "document id does not refer to a directory",
                    "restore the document directory from backup or remove the stray entry",
                );
                continue;
            }
            self.verify_document(&name, &entry.path(), &mut report);
        }
        Ok(report)
    }

    fn verify_document(&self, id: &str, dir: &Path, report: &mut StoreIntegrityReport) {
        let metadata_path = dir.join("doc.json");
        let metadata_source = match fs::symlink_metadata(&metadata_path) {
            Err(error) => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "doc.json",
                    format!("cannot inspect metadata: {error}"),
                    "restore a readable doc.json from backup",
                );
                None
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "doc.json",
                    "metadata is a symbolic link",
                    "replace the link with a regular doc.json inside the document directory",
                );
                None
            }
            Ok(metadata) if !metadata.is_file() => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "doc.json",
                    "metadata is not a regular file",
                    "restore a regular doc.json from backup",
                );
                None
            }
            Ok(_) => {
                match read_bounded_utf8(&metadata_path, "doc.json", MAX_DOCUMENT_METADATA_BYTES) {
                    Ok(source) => Some(source),
                    Err(error) => {
                        report.issue(
                            IntegritySeverity::Error,
                            Some(id),
                            "doc.json",
                            format!("cannot read metadata: {error}"),
                            "restore a valid, readable doc.json from backup",
                        );
                        None
                    }
                }
            }
        };
        let metadata = metadata_source.as_deref().and_then(|source| {
            match serde_json::from_str::<DocMeta>(source) {
                Ok(metadata) => Some(metadata),
                Err(error) => {
                    report.issue(
                        IntegritySeverity::Error,
                        Some(id),
                        "doc.json",
                        format!("invalid metadata JSON: {error}"),
                        "restore doc.json from a backup made by a compatible Atelier version",
                    );
                    None
                }
            }
        });

        if let Some(metadata) = &metadata {
            if let Err(error) = metadata.validate() {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "doc.json",
                    error,
                    "restore doc.json from a backup made by a compatible Atelier version",
                );
            }
            self.verify_cels(id, dir, metadata, report);
            if metadata.reference.as_deref() == Some("reference.png") {
                let path = dir.join("reference.png");
                if let Err(error) = verify_stored_image(&path, false) {
                    report.issue(
                        IntegritySeverity::Error,
                        Some(id),
                        "reference.png",
                        error,
                        "restore the reference image from backup or clear it with `doc_ref op=set` and no path",
                    );
                }
            }
        }

        self.verify_document_tree(
            id,
            dir,
            metadata
                .as_ref()
                .is_some_and(|metadata| metadata.reference.as_deref() == Some("reference.png")),
            report,
        );

        if self.exists(id)
            && let Err(error) = self.document_revision(id)
        {
            report.issue(
                IntegritySeverity::Error,
                Some(id),
                REVISION_FILE,
                error,
                "restore a regular revision file containing one unsigned decimal integer",
            );
        }

        let journal_path = dir.join(JOURNAL_FILE);
        match fs::symlink_metadata(&journal_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => report.issue(
                IntegritySeverity::Warning,
                Some(id),
                JOURNAL_FILE,
                "journal is missing; the current document can be edited but not replayed",
                "restore recipe.jsonl from backup, or keep the document as an intentionally non-replayable asset",
            ),
            Err(error) => report.issue(
                IntegritySeverity::Error,
                Some(id),
                JOURNAL_FILE,
                format!("cannot inspect journal: {error}"),
                "check ownership and permissions for recipe.jsonl",
            ),
            Ok(metadata) if metadata.file_type().is_symlink() => report.issue(
                IntegritySeverity::Error,
                Some(id),
                JOURNAL_FILE,
                "journal is a symbolic link",
                "replace the link with a regular recipe.jsonl inside the document directory",
            ),
            Ok(metadata) if !metadata.is_file() => report.issue(
                IntegritySeverity::Error,
                Some(id),
                JOURNAL_FILE,
                "journal is not a regular file",
                "restore a regular recipe.jsonl or remove it if replay history is intentionally discarded",
            ),
            Ok(_) => match parse_journal_file(id, &journal_path) {
                Ok(journal) => {
                    report.journal_entries += journal.entries.len();
                    if journal.torn_tail {
                        report.issue(
                            IntegritySeverity::Warning,
                            Some(id),
                            JOURNAL_FILE,
                            "incomplete final journal line was ignored",
                            "truncate the incomplete final line after confirming the preceding recipe is complete",
                        );
                    }
                }
                Err(error) => report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    JOURNAL_FILE,
                    error,
                    "restore recipe.jsonl from backup, or remove it if replay history is intentionally discarded",
                ),
            },
        }
    }

    fn verify_cels(
        &self,
        id: &str,
        dir: &Path,
        metadata: &DocMeta,
        report: &mut StoreIntegrityReport,
    ) {
        report.cels += metadata.cels.len();
        let expected: HashSet<&str> = metadata.cels.iter().map(|cel| cel.file.as_str()).collect();
        let cels_dir = dir.join("cels");
        match fs::symlink_metadata(&cels_dir) {
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && metadata.cels.is_empty() =>
            {
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "cels",
                    "cels directory is missing",
                    "restore the document's cels directory from backup",
                );
                return;
            }
            Err(error) => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "cels",
                    format!("cannot inspect cels directory: {error}"),
                    "check ownership and permissions for the cels directory",
                );
                return;
            }
            Ok(directory_metadata) if directory_metadata.file_type().is_symlink() => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "cels",
                    "cels directory is a symbolic link",
                    "replace the link with a real cels directory inside this document",
                );
                return;
            }
            Ok(directory_metadata) if !directory_metadata.is_dir() => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "cels",
                    "cels path is not a directory",
                    "restore the document's cels directory from backup",
                );
                return;
            }
            Ok(_) => {}
        }

        for cel in &metadata.cels {
            let canonical = format!("cels/L{}_F{}.png", cel.layer, cel.frame);
            if cel.file != canonical {
                // Metadata validation reports the unsafe path. Never join it.
                continue;
            }
            if let Err(error) = verify_stored_image(&dir.join(&cel.file), true) {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    &cel.file,
                    error,
                    "restore this cel PNG from backup",
                );
            }
        }

        match fs::read_dir(&cels_dir) {
            Err(error) => report.issue(
                IntegritySeverity::Error,
                Some(id),
                "cels",
                format!("cannot list cels: {error}"),
                "check ownership and permissions for the cels directory",
            ),
            Ok(entries) => {
                // Keep only the lexicographically first bounded set, while
                // counting every orphan in the summary.
                let mut orphaned = BinaryHeap::new();
                let mut orphan_count = 0usize;
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            let relative = format!("cels/{}", entry.file_name().to_string_lossy());
                            let file_type = match entry.file_type() {
                                Ok(file_type) => file_type,
                                Err(error) => {
                                    report.issue(
                                        IntegritySeverity::Error,
                                        Some(id),
                                        &relative,
                                        format!("cannot inspect cel entry type: {error}"),
                                        "check ownership and permissions for this entry",
                                    );
                                    continue;
                                }
                            };
                            if !file_type.is_file() {
                                report.issue(
                                    IntegritySeverity::Error,
                                    Some(id),
                                    &relative,
                                    "cels entry is not a regular file",
                                    "replace links and special files with regular cel PNGs, or remove them",
                                );
                            } else if is_managed_cel_path(&relative)
                                && !expected.contains(relative.as_str())
                            {
                                orphan_count += 1;
                                if orphaned.len() < MAX_RETAINED_ISSUES {
                                    orphaned.push(relative);
                                } else if orphaned.peek().is_some_and(|largest| relative < *largest)
                                {
                                    orphaned.pop();
                                    orphaned.push(relative);
                                }
                            } else if !is_managed_cel_path(&relative) {
                                report.issue(
                                    IntegritySeverity::Warning,
                                    Some(id),
                                    &relative,
                                    "unrecognized file in the cels directory",
                                    "move the file outside the document after confirming it is not needed",
                                );
                            }
                        }
                        Err(error) => report.issue(
                            IntegritySeverity::Error,
                            Some(id),
                            "cels",
                            format!("cannot inspect a cel entry: {error}"),
                            "check ownership and permissions for the cels directory",
                        ),
                    }
                }
                let retained = orphaned.len();
                let mut orphaned = orphaned.into_vec();
                orphaned.sort();
                for orphan in orphaned {
                    report.issue(
                        IntegritySeverity::Warning,
                        Some(id),
                        &orphan,
                        "cel PNG is not referenced by doc.json",
                        "remove the orphan after confirming it is not needed; Atelier will not load it",
                    );
                }
                report.omit(
                    IntegritySeverity::Warning,
                    orphan_count.saturating_sub(retained),
                );
            }
        }
    }

    fn verify_transaction_area(&self, path: &Path, report: &mut StoreIntegrityReport) {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.issue(
                    IntegritySeverity::Error,
                    None,
                    ".transactions",
                    format!("cannot inspect transaction directory: {error}"),
                    "check ownership and permissions for the transaction directory",
                );
                return;
            }
        };
        if !metadata.is_dir() {
            report.issue(
                IntegritySeverity::Error,
                None,
                ".transactions",
                "transaction path is not a real directory",
                "stop Atelier and replace it with an empty real directory",
            );
            return;
        }
        let mut entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                report.issue(
                    IntegritySeverity::Error,
                    None,
                    ".transactions",
                    format!("cannot read transaction directory: {error}"),
                    "check ownership and permissions for the transaction directory",
                );
                return;
            }
        };
        if entries.next().is_some() {
            report.issue(
                IntegritySeverity::Warning,
                None,
                ".transactions",
                "stale transaction data remains while no writer holds the store lock",
                "restart Atelier to run stale-transaction cleanup, then verify again",
            );
            verify_tree_types(None, path, path, report);
        }
    }

    fn verify_document_tree(
        &self,
        id: &str,
        dir: &Path,
        has_reference: bool,
        report: &mut StoreIntegrityReport,
    ) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    "directory",
                    format!("cannot list document entries: {error}"),
                    "check ownership and permissions for the document directory",
                );
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report.issue(
                        IntegritySeverity::Error,
                        Some(id),
                        "directory",
                        format!("cannot inspect a document entry: {error}"),
                        "check ownership and permissions for the document directory",
                    );
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), "doc.json" | "recipe.jsonl" | "cels")
                || name == REVISION_FILE
                || (name == "reference.png" && has_reference)
            {
                continue;
            }
            if name == ".checkpoints" {
                match fs::symlink_metadata(entry.path()) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => {
                        report.issue(
                            IntegritySeverity::Error,
                            Some(id),
                            ".checkpoints",
                            "checkpoint path is not a real directory",
                            "replace it with a real checkpoint directory or remove it",
                        );
                        continue;
                    }
                    Err(error) => {
                        report.issue(
                            IntegritySeverity::Error,
                            Some(id),
                            ".checkpoints",
                            format!("cannot inspect checkpoint directory: {error}"),
                            "check ownership and permissions for the checkpoint directory",
                        );
                        continue;
                    }
                }
                verify_tree_types(Some(id), dir, &entry.path(), report);
                continue;
            }

            let component = name.clone();
            match entry.file_type() {
                Ok(file_type) if file_type.is_file() => report.issue(
                    IntegritySeverity::Warning,
                    Some(id),
                    component,
                    "unrecognized document file is ignored by Atelier",
                    "move the file outside the document after confirming it is not needed",
                ),
                Ok(file_type) if file_type.is_dir() => {
                    report.issue(
                        IntegritySeverity::Warning,
                        Some(id),
                        &component,
                        "unrecognized document directory is ignored by Atelier",
                        "move the directory outside the document after confirming it is not needed",
                    );
                    verify_tree_types(Some(id), dir, &entry.path(), report);
                }
                Ok(_) => report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    component,
                    "unrecognized document entry is a link or special file",
                    "remove it or replace it with a regular file inside the document",
                ),
                Err(error) => report.issue(
                    IntegritySeverity::Error,
                    Some(id),
                    component,
                    format!("cannot inspect document entry: {error}"),
                    "check ownership and permissions for this entry",
                ),
            }
        }
    }
}

fn verify_tree_types(
    document_id: Option<&str>,
    display_root: &Path,
    root: &Path,
    report: &mut StoreIntegrityReport,
) {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut inspected = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                report.issue(
                    IntegritySeverity::Error,
                    document_id,
                    directory.display().to_string(),
                    format!("cannot inspect directory tree: {error}"),
                    "check ownership and permissions for this directory",
                );
                continue;
            }
        };
        for entry in entries {
            inspected += 1;
            if inspected > MAX_STORE_ENTRIES {
                report.issue(
                    IntegritySeverity::Error,
                    document_id,
                    root.display().to_string(),
                    format!("directory tree contains more than {MAX_STORE_ENTRIES} entries"),
                    "archive or prune this directory tree before verifying again",
                );
                return;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report.issue(
                        IntegritySeverity::Error,
                        document_id,
                        directory.display().to_string(),
                        format!("cannot inspect directory entry: {error}"),
                        "check ownership and permissions for this entry",
                    );
                    continue;
                }
            };
            let entry_path = entry.path();
            let component = entry_path
                .strip_prefix(display_root)
                .unwrap_or(&entry_path)
                .display()
                .to_string();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    report.issue(
                        IntegritySeverity::Error,
                        document_id,
                        component,
                        format!("cannot inspect entry type: {error}"),
                        "check ownership and permissions for this entry",
                    );
                    continue;
                }
            };
            if file_type.is_symlink() || (!file_type.is_file() && !file_type.is_dir()) {
                report.issue(
                    IntegritySeverity::Error,
                    document_id,
                    component,
                    "stored tree contains a link or special file",
                    "remove it or replace it with data stored directly inside the document",
                );
            } else if file_type.is_dir() {
                if depth >= MAX_TREE_DEPTH {
                    report.issue(
                        IntegritySeverity::Error,
                        document_id,
                        component,
                        format!("stored tree exceeds the maximum depth of {MAX_TREE_DEPTH}"),
                        "flatten or remove the deeply nested directory tree",
                    );
                } else {
                    stack.push((entry.path(), depth + 1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolName;
    use serde_json::json;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-integrity-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    fn journal_path(studio: &Studio, id: &str) -> std::path::PathBuf {
        studio.doc_dir(id).join(JOURNAL_FILE)
    }

    #[test]
    fn checks_metadata_cels_journals_and_orphans() {
        let studio = studio("store");
        let created = studio.doc_new("verified", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap().to_string();
        studio
            .doc_draw(
                &id,
                0,
                0,
                "fill_cel",
                json!({"color":[9,8,7,255]}).as_object().unwrap().clone(),
            )
            .unwrap();
        studio
            .journal_append(
                &id,
                ToolName::DocNew,
                &json!({"name":"verified","width":4,"height":4,"doc_id":id}),
            )
            .unwrap();
        studio
            .journal_append(
                &id,
                ToolName::DocDraw,
                &json!({"doc_id":id,"op":"fill_cel","color":[9,8,7,255]}),
            )
            .unwrap();

        let clean = studio.verify_store().unwrap();
        assert!(clean.ok, "unexpected findings: {:?}", clean.issues);
        assert_eq!(
            (clean.documents, clean.cels, clean.journal_entries),
            (1, 1, 2)
        );

        fs::write(studio.doc_dir(&id).join("cels/L9_F9.png"), b"orphan").unwrap();
        let warned = studio.verify_store().unwrap();
        assert!(warned.ok);
        assert!(warned.issues.iter().any(|issue| {
            issue.severity == IntegritySeverity::Warning && issue.component == "cels/L9_F9.png"
        }));

        fs::write(studio.doc_dir(&id).join("cels/L0_F0.png"), b"not a png").unwrap();
        fs::write(journal_path(&studio, &id), "not json\n").unwrap();
        let broken = studio.verify_store().unwrap();
        assert!(!broken.ok);
        assert!(broken.errors >= 2, "findings: {:?}", broken.issues);
        assert!(
            broken
                .issues
                .iter()
                .any(|issue| issue.component == "cels/L0_F0.png")
        );
        assert!(
            broken
                .issues
                .iter()
                .any(|issue| issue.component == JOURNAL_FILE)
        );
    }

    #[test]
    fn verifies_revision_sidecars_without_rejecting_legacy_absence() {
        let studio = studio("revision");
        let created = studio.doc_new("verified", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap().to_string();
        studio
            .journal_append(
                &id,
                ToolName::DocNew,
                &json!({"name":"verified","width":4,"height":4,"doc_id":id}),
            )
            .unwrap();

        let legacy = studio.verify_store().unwrap();
        assert!(
            legacy.ok,
            "legacy revision zero is valid: {:?}",
            legacy.issues
        );

        studio.set_document_revision(&id, 7).unwrap();
        let current = studio.verify_store().unwrap();
        assert!(
            current.ok,
            "revision sidecar is managed: {:?}",
            current.issues
        );
        assert!(
            current
                .issues
                .iter()
                .all(|issue| issue.component != REVISION_FILE)
        );

        fs::write(studio.doc_dir(&id).join(REVISION_FILE), "not-a-number\n").unwrap();
        let corrupt = studio.verify_store().unwrap();
        assert!(!corrupt.ok);
        assert!(corrupt.issues.iter().any(|issue| {
            issue.severity == IntegritySeverity::Error && issue.component == REVISION_FILE
        }));
    }

    #[test]
    fn reports_a_recovered_torn_journal_tail() {
        let studio = studio("torn-journal");
        let created = studio.doc_new("verified", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap().to_string();
        studio
            .journal_append(
                &id,
                ToolName::DocNew,
                &json!({"name":"verified","width":4,"height":4,"doc_id":id}),
            )
            .unwrap();
        let path = journal_path(&studio, &id);
        let mut journal = fs::read_to_string(&path).unwrap();
        journal.push_str("{\"format_version\":1,\"tool\":\"doc_draw\"");
        fs::write(path, journal).unwrap();

        let report = studio.verify_store().unwrap();
        assert!(report.ok);
        assert_eq!(report.journal_entries, 1);
        assert!(report.issues.iter().any(|issue| {
            issue.severity == IntegritySeverity::Warning
                && issue.message.contains("incomplete final journal line")
        }));
    }

    #[test]
    fn bounds_text_before_parsing_it() {
        let studio = studio("bounded-text");
        let path = studio.docs_dir.join("bounded.txt");
        fs::write(&path, b"12345").unwrap();
        assert_eq!(read_bounded_utf8(&path, "fixture", 5).unwrap(), "12345");
        let error = read_bounded_utf8(&path, "fixture", 4).unwrap_err();
        assert!(error.contains("4-byte verification limit"), "got: {error}");

        fs::write(&path, [0xff]).unwrap();
        let error = read_bounded_utf8(&path, "fixture", 5).unwrap_err();
        assert!(error.contains("not UTF-8"), "got: {error}");
    }

    #[test]
    fn caps_retained_orphans_but_preserves_totals() {
        let studio = studio("issue-cap");
        let created = studio.doc_new("bounded", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap().to_string();
        studio
            .journal_append(
                &id,
                ToolName::DocNew,
                &json!({"name":"bounded","width":4,"height":4,"doc_id":id}),
            )
            .unwrap();
        let total = MAX_RETAINED_ISSUES + 20;
        for index in 0..total {
            fs::write(
                studio
                    .doc_dir(&id)
                    .join(format!("cels/L{index}_F{index}.png")),
                b"orphan",
            )
            .unwrap();
        }

        let report = studio.verify_store().unwrap();
        assert!(report.ok);
        assert_eq!(report.warnings, total);
        assert_eq!(report.issues.len(), MAX_RETAINED_ISSUES);
        assert!(report.issues_truncated);
        assert_eq!(report.omitted_issues, 20);
        assert!(
            report
                .issues
                .windows(2)
                .all(|pair| pair[0].component <= pair[1].component)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_links_hidden_in_managed_store_trees() {
        use std::os::unix::fs::symlink;

        let studio = studio("hidden-links");
        let created = studio.doc_new("linked", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap().to_string();
        studio
            .journal_append(
                &id,
                ToolName::DocNew,
                &json!({"name":"linked","width":4,"height":4,"doc_id":id}),
            )
            .unwrap();

        let outside = studio.docs_dir.join("outside.txt");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, studio.doc_dir(&id).join("cels/unmanaged.png")).unwrap();
        let checkpoints = studio.doc_dir(&id).join(".checkpoints");
        fs::create_dir(&checkpoints).unwrap();
        symlink(&outside, checkpoints.join("linked")).unwrap();

        let report = studio.verify_store().unwrap();
        assert!(!report.ok);
        assert!(report.errors >= 2, "findings: {:?}", report.issues);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.component == "cels/unmanaged.png")
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.component == ".checkpoints/linked")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_linked_transaction_directory() {
        use std::os::unix::fs::symlink;

        let studio = studio("linked-transactions");
        let outside = studio.docs_dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, studio.docs_dir.join(".transactions")).unwrap();

        let report = studio.verify_store().unwrap();
        assert!(!report.ok);
        assert!(report.issues.iter().any(|issue| {
            issue.component == ".transactions" && issue.severity == IntegritySeverity::Error
        }));
    }
}
