//! Transactional publication for project builds.
//!
//! Exports render into a project-local workspace first. Only a complete staged
//! artifact set is promoted; previous outputs are backed up and restored if any
//! rename fails.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Export, Project, display_output};
use serde_json::Value;

static NEXT_BUILD_WORKSPACE: AtomicU64 = AtomicU64::new(0);

pub(super) struct BuildLock {
    file: std::fs::File,
}

impl BuildLock {
    pub(super) fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join(".atelier").join("build.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| format!("cannot open build lock {}: {error}", path.display()))?;
        fs4::FileExt::lock(&file).map_err(|error| format!("cannot lock project build: {error}"))?;
        Ok(Self { file })
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// Project-local staging and backup space. It shares a filesystem with normal
/// project outputs, so successful exports can be promoted with rename.
pub(super) struct BuildWorkspace {
    root: PathBuf,
    preserve: Cell<bool>,
}

impl BuildWorkspace {
    pub(super) fn new(project_root: &Path) -> Result<Self, String> {
        for _ in 0..100 {
            let serial = NEXT_BUILD_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let root = project_root
                .join(".atelier")
                .join(format!(".build-{}-{serial}", std::process::id()));
            match std::fs::create_dir(&root) {
                Ok(()) => {
                    if let Err(error) = std::fs::create_dir(root.join("outputs"))
                        .and_then(|()| std::fs::create_dir(root.join("backups")))
                    {
                        let _ = std::fs::remove_dir_all(&root);
                        return Err(format!(
                            "cannot prepare build workspace {}: {error}",
                            root.display()
                        ));
                    }
                    return Ok(Self {
                        root,
                        preserve: Cell::new(false),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot create build workspace {}: {error}",
                        root.display()
                    ));
                }
            }
        }
        Err("cannot allocate a unique project build workspace".into())
    }

    pub(super) fn staged_output(
        &self,
        project_root: &Path,
        destination: &Path,
    ) -> Result<PathBuf, String> {
        let relative = destination.strip_prefix(project_root).map_err(|_| {
            format!(
                "build output {} is outside project {}",
                destination.display(),
                project_root.display()
            )
        })?;
        Ok(self.root.join("outputs").join(relative))
    }

    fn backup(&self, index: usize) -> PathBuf {
        self.root.join("backups").join(index.to_string())
    }

    fn outputs(&self) -> PathBuf {
        self.root.join("outputs")
    }
}

impl Drop for BuildWorkspace {
    fn drop(&mut self) {
        if !self.preserve.get() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

struct Promotion {
    staged: PathBuf,
    destination: PathBuf,
    backup: PathBuf,
    backed_up: bool,
    promoted: bool,
}

fn rollback_promotions(promotions: &mut [Promotion]) -> Vec<String> {
    let mut failures = Vec::new();
    for promotion in promotions.iter_mut().rev().filter(|item| item.promoted) {
        if let Some(parent) = promotion.staged.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            failures.push(format!(
                "cannot recreate staging parent {}: {error}",
                parent.display()
            ));
            continue;
        }
        if let Err(error) = std::fs::rename(&promotion.destination, &promotion.staged) {
            failures.push(format!(
                "cannot roll back new output {}: {error}",
                promotion.destination.display()
            ));
            continue;
        }
        promotion.promoted = false;
    }
    for promotion in promotions.iter_mut().rev().filter(|item| item.backed_up) {
        if let Some(parent) = promotion.destination.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            failures.push(format!(
                "cannot recreate output parent {}: {error}",
                parent.display()
            ));
            continue;
        }
        if let Err(error) = std::fs::rename(&promotion.backup, &promotion.destination) {
            failures.push(format!(
                "cannot restore previous output {}: {error}",
                promotion.destination.display()
            ));
            continue;
        }
        promotion.backed_up = false;
    }
    failures
}

fn promotion_error(
    error: String,
    promotions: &mut [Promotion],
    workspace: &BuildWorkspace,
) -> String {
    let rollback = rollback_promotions(promotions);
    if rollback.is_empty() {
        error
    } else {
        workspace.preserve.set(true);
        format!(
            "{error}; rollback also failed: {}; recovery files were preserved in {}",
            rollback.join("; "),
            workspace.root.display()
        )
    }
}

fn rewrite_staged_path(value: &mut Value, staged_root: &Path, project_root: &Path) {
    match value {
        Value::String(text) => {
            if let Ok(relative) = Path::new(text.as_str()).strip_prefix(staged_root) {
                *text = project_root.join(relative).to_string_lossy().into_owned();
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_staged_path(item, staged_root, project_root);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                rewrite_staged_path(item, staged_root, project_root);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn rewrite_metadata_file(
    path: &Path,
    staged_root: &Path,
    project_root: &Path,
) -> Result<(), String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read staged metadata {}: {error}", path.display()))?;
    let mut metadata: Value = serde_json::from_str(&source)
        .map_err(|error| format!("cannot parse staged metadata {}: {error}", path.display()))?;
    rewrite_staged_path(&mut metadata, staged_root, project_root);
    std::fs::write(
        path,
        serde_json::to_string_pretty(&metadata).map_err(|error| {
            format!("cannot encode staged metadata {}: {error}", path.display())
        })?,
    )
    .map_err(|error| format!("cannot update staged metadata {}: {error}", path.display()))
}

fn rewrite_metadata_tree(
    directory: &Path,
    staged_root: &Path,
    project_root: &Path,
) -> Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        format!(
            "cannot inspect staged output {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "cannot inspect an entry in {}: {error}",
                directory.display()
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "cannot inspect staged output type {}: {error}",
                entry.path().display()
            )
        })?;
        if file_type.is_dir() {
            rewrite_metadata_tree(&entry.path(), staged_root, project_root)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            rewrite_metadata_file(&entry.path(), staged_root, project_root)?;
        }
    }
    Ok(())
}

fn rewrite_metadata_paths(
    project: &Project,
    exports: &[&Export],
    workspace: &BuildWorkspace,
) -> Result<(), String> {
    let staged_root = workspace.outputs();
    for export in exports {
        let staged_output = workspace.staged_output(&project.root, &export.out_path)?;
        match export.op.as_str() {
            "sheet" | "atlas" | "tileset" => rewrite_metadata_file(
                &staged_output.with_extension("json"),
                &staged_root,
                &project.root,
            )?,
            "all" => rewrite_metadata_tree(&staged_output, &staged_root, &project.root)?,
            "anim" => {}
            _ => unreachable!("export op was validated"),
        }
    }
    Ok(())
}

pub(super) fn promote_outputs(
    project: &Project,
    exports: &[&Export],
    workspace: &BuildWorkspace,
) -> Result<(), String> {
    rewrite_metadata_paths(project, exports, workspace)?;
    let destinations = exports
        .iter()
        .flat_map(|export| export.output_claims())
        .collect::<Vec<_>>();
    let mut promotions = Vec::with_capacity(destinations.len());
    for (index, destination) in destinations.into_iter().enumerate() {
        let staged = workspace.staged_output(&project.root, &destination)?;
        std::fs::symlink_metadata(&staged).map_err(|error| {
            format!(
                "export did not produce expected output {}: {error}",
                display_output(&project.root, &destination)
            )
        })?;
        promotions.push(Promotion {
            staged,
            destination,
            backup: workspace.backup(index),
            backed_up: false,
            promoted: false,
        });
    }

    for index in 0..promotions.len() {
        match std::fs::symlink_metadata(&promotions[index].destination) {
            Ok(_) => {
                if let Err(error) =
                    std::fs::rename(&promotions[index].destination, &promotions[index].backup)
                {
                    return Err(promotion_error(
                        format!(
                            "cannot back up existing output {}: {error}",
                            promotions[index].destination.display()
                        ),
                        &mut promotions,
                        workspace,
                    ));
                }
                promotions[index].backed_up = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(promotion_error(
                    format!(
                        "cannot inspect existing output {}: {error}",
                        promotions[index].destination.display()
                    ),
                    &mut promotions,
                    workspace,
                ));
            }
        }
    }

    for index in 0..promotions.len() {
        if let Some(parent) = promotions[index].destination.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return Err(promotion_error(
                format!(
                    "cannot create output directory {}: {error}",
                    parent.display()
                ),
                &mut promotions,
                workspace,
            ));
        }
        if let Err(error) =
            std::fs::rename(&promotions[index].staged, &promotions[index].destination)
        {
            return Err(promotion_error(
                format!(
                    "cannot publish output {}: {error}",
                    promotions[index].destination.display()
                ),
                &mut promotions,
                workspace,
            ));
        }
        promotions[index].promoted = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_restores_old_outputs_and_removes_new_ones() {
        let root =
            std::env::temp_dir().join(format!("atelier-build-rollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for directory in ["staged", "dest", "backup"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(root.join("dest/a"), b"new a").unwrap();
        std::fs::write(root.join("dest/b"), b"new b").unwrap();
        std::fs::write(root.join("backup/a"), b"old a").unwrap();
        let mut promotions = vec![
            Promotion {
                staged: root.join("staged/a"),
                destination: root.join("dest/a"),
                backup: root.join("backup/a"),
                backed_up: true,
                promoted: true,
            },
            Promotion {
                staged: root.join("staged/b"),
                destination: root.join("dest/b"),
                backup: root.join("backup/b"),
                backed_up: false,
                promoted: true,
            },
        ];

        assert!(rollback_promotions(&mut promotions).is_empty());
        assert_eq!(std::fs::read(root.join("dest/a")).unwrap(), b"old a");
        assert!(!root.join("dest/b").exists());
        assert_eq!(std::fs::read(root.join("staged/a")).unwrap(), b"new a");
        assert_eq!(std::fs::read(root.join("staged/b")).unwrap(), b"new b");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_rollback_preserves_recovery_workspace() {
        let root =
            std::env::temp_dir().join(format!("atelier-build-recovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        {
            let workspace = BuildWorkspace {
                root: root.clone(),
                preserve: Cell::new(false),
            };
            let mut promotions = vec![Promotion {
                staged: root.join("staged"),
                destination: root.join("destination"),
                backup: root.join("missing-backup"),
                backed_up: true,
                promoted: false,
            }];
            let error = promotion_error("publish failed".into(), &mut promotions, &workspace);
            assert!(error.contains("recovery files were preserved"));
            assert!(workspace.preserve.get());
        }
        assert!(root.exists(), "Drop must retain possible recovery files");
        let _ = std::fs::remove_dir_all(root);
    }
}
