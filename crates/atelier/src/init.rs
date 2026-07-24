//! `atelier init` — stamp a project store: `./.atelier/documents` in the
//! current directory. One command, once per project. From then on atelier
//! calls made here keep this project's art next to the project instead of the
//! global store: ids mint clean per project (`hero`, never `hero-2` from some
//! other project's hero), and the recipes can be committed with the game.
//! Idempotent; opting out is deleting the directory.

use std::path::{Path, PathBuf};

struct InitResult {
    root: PathBuf,
    store_existed: bool,
    manifest_created: bool,
}

/// Create `<dir>/.atelier/documents` and its starter project manifest without
/// replacing either one when this is an existing project store.
fn init_in(dir: &Path) -> std::io::Result<InitResult> {
    let root = dir.join(".atelier");
    let store_existed = root.is_dir();
    std::fs::create_dir_all(root.join("documents"))?;
    let manifest_created = crate::project::ensure_manifest(&root)?;
    Ok(InitResult {
        root,
        store_existed,
        manifest_created,
    })
}

/// Entry point for `atelier init`. Returns a process exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("atelier init: takes no arguments — it stamps ./.atelier here");
        return 2;
    }
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("atelier init: cannot read the current directory: {e}");
            return 1;
        }
    };
    match init_in(&cwd) {
        Ok(result) => {
            if result.store_existed {
                println!("already a project store: {}", result.root.display());
            } else {
                println!("project store created: {}", result.root.display());
            }
            if result.manifest_created {
                println!(
                    "project manifest created: {}",
                    result.root.join("project.toml").display()
                );
            } else {
                println!(
                    "project manifest preserved: {}",
                    result.root.join("project.toml").display()
                );
            }
            println!("atelier calls from here now keep their art and recipes in ./.atelier");
            if std::env::var_os("ATELIER_HOME").is_some() {
                eprintln!("note: ATELIER_HOME is set and overrides this store");
            }
            0
        }
        Err(e) => {
            eprintln!(
                "atelier init: cannot create {}: {e}",
                cwd.join(".atelier").display()
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_creates_documents_idempotently() {
        let dir = std::env::temp_dir().join(format!("atelier-init-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let result = super::init_in(&dir).unwrap();
        assert!(
            !result.store_existed,
            "a fresh directory is not a store yet"
        );
        assert!(result.root.join("documents").is_dir());
        assert!(result.root.join("project.toml").is_file());
        assert!(result.manifest_created);
        let manifest = std::fs::read_to_string(result.root.join("project.toml")).unwrap();
        assert!(manifest.contains("version = 1"));

        let result = super::init_in(&dir).unwrap();
        assert!(result.store_existed, "a second init sees the store");
        assert!(!result.manifest_created);
        assert_eq!(
            std::fs::read_to_string(result.root.join("project.toml")).unwrap(),
            manifest,
            "reinitializing must not replace a project manifest"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_rejects_arguments() {
        assert_eq!(super::run(&["x".to_string()]), 2);
    }
}
