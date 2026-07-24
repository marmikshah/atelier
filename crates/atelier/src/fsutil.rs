//! Conservative writes for configuration and skill files owned by other tools.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn sibling_with_suffix(path: &Path, suffix: &str) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!("{name}{suffix}")))
}

/// Write text through a same-directory temporary file.
///
/// Existing content is copied to `<name>.atelier-backup` first. Unix renames
/// replace atomically; Windows cannot replace an existing file with `std`, so
/// the backup remains the recovery point across its remove-then-rename step.
pub(crate) fn write_text(path: &Path, body: &str) -> Result<(), String> {
    if fs::read_to_string(path).ok().as_deref() == Some(body) {
        return Ok(());
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;

    let old_permissions = match fs::metadata(path) {
        Ok(metadata) => {
            let backup = sibling_with_suffix(path, ".atelier-backup")?;
            fs::copy(path, &backup).map_err(|error| {
                format!(
                    "cannot back up {} to {}: {error}",
                    path.display(),
                    backup.display()
                )
            })?;
            Some(metadata.permissions())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    };

    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temporary = sibling_with_suffix(
        path,
        &format!(".atelier-{}-{serial}.tmp", std::process::id()),
    )?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(body.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
        drop(file);

        if let Some(permissions) = old_permissions {
            fs::set_permissions(&temporary, permissions).map_err(|error| {
                format!(
                    "cannot preserve permissions on {}: {error}",
                    temporary.display()
                )
            })?;
        }

        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)
                .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
        }
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_replaces_with_a_backup() {
        let root = std::env::temp_dir().join(format!("atelier-fsutil-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("config.toml");

        write_text(&path, "first\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n");
        assert!(!root.join("config.toml.atelier-backup").exists());

        write_text(&path, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(
            fs::read_to_string(root.join("config.toml.atelier-backup")).unwrap(),
            "first\n"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
