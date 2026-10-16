//! `atelier uninstall` — remove Atelier from this machine.
//!
//! Removes what Atelier itself put there: the background daemon, the skills
//! that `atelier skills install` wrote for each agent, and the binary that is
//! running. Documents are the user's art, so the global store is kept unless
//! `--documents` is passed, and the plan is shown and confirmed before
//! anything is deleted.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{library, service, skills};

const USAGE: &str = "usage: atelier uninstall [--documents] [--yes]

Remove the daemon, the installed agent skills, and this binary. Shows the
plan and asks for confirmation first.

  --documents  also delete the global document store (ATELIER_HOME or
               ~/.atelier) — your artwork; never implied
  --yes, -y    skip the confirmation prompt";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    documents: bool,
    yes: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        documents: false,
        yes: false,
    };
    for arg in args {
        match arg.as_str() {
            "--documents" if !options.documents => options.documents = true,
            "--yes" | "-y" if !options.yes => options.yes = true,
            "--documents" | "--yes" | "-y" => return Err(format!("{arg} may only be passed once")),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(options)
}

/// Everything one run will remove, resolved up front so it can be shown and
/// confirmed as a whole. Only paths that exist are listed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// The systemd unit, when one is installed (Linux only).
    daemon_unit: Option<PathBuf>,
    /// `<agent>/skills/<atelier-skill>` directories that exist.
    skills: Vec<PathBuf>,
    /// The running binary.
    binary: Option<PathBuf>,
    /// The global document store, when `--documents` asked for it and it exists.
    documents: Option<PathBuf>,
    /// The global store that is being kept, named so the user knows where it is.
    kept_documents: Option<PathBuf>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.daemon_unit.is_none()
            && self.skills.is_empty()
            && self.binary.is_none()
            && self.documents.is_none()
    }
}

/// The skill directories `atelier skills install --for all` would have
/// written under `home`, whether or not they exist.
fn skill_dirs(home: &Path) -> Vec<PathBuf> {
    crate::SKILL_TARGETS
        .iter()
        .filter_map(|target| crate::skill_target_root(target, home))
        .flat_map(|root| {
            skills::ALL
                .iter()
                .map(move |skill| root.join("skills").join(skill.name))
        })
        .collect()
}

fn plan(
    home: Option<&Path>,
    daemon_unit: Option<PathBuf>,
    binary: Option<PathBuf>,
    store: &Path,
    remove_documents: bool,
) -> Plan {
    let skills = home
        .map(skill_dirs)
        .unwrap_or_default()
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect();
    let store_exists = store.is_dir();
    Plan {
        daemon_unit: daemon_unit.filter(|unit| unit.is_file()),
        skills,
        binary: binary.filter(|path| path.is_file()),
        documents: (remove_documents && store_exists).then(|| store.to_path_buf()),
        kept_documents: (!remove_documents && store_exists).then(|| store.to_path_buf()),
    }
}

/// `$CARGO_HOME/bin`, where `cargo install` puts binaries.
fn cargo_bin_dir(home: Option<&Path>) -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin"))
}

/// Remove the binary. One that `cargo install` placed is removed through
/// `cargo uninstall`, so cargo's record of it goes too; otherwise, or when
/// cargo is unavailable, the file is unlinked directly.
fn remove_binary(binary: &Path, cargo_bin: Option<&Path>) -> Result<String, String> {
    if cargo_bin.is_some_and(|dir| binary.starts_with(dir)) {
        let uninstalled = Command::new("cargo")
            .args(["uninstall", "atelier"])
            .output()
            .is_ok_and(|output| output.status.success() && !binary.exists());
        if uninstalled {
            return Ok(format!("{} (cargo uninstall)", binary.display()));
        }
    }
    std::fs::remove_file(binary)
        .map(|_| binary.display().to_string())
        .map_err(|error| format!("{}: {error}", binary.display()))
}

/// Delete everything in the plan. Returns what was removed and what failed;
/// one failure does not stop the rest, so a second run can finish the job.
fn apply(plan: &Plan, cargo_bin: Option<&Path>) -> (Vec<String>, Vec<String>) {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    let mut record = |result: Result<String, String>| match result {
        Ok(what) => removed.push(what),
        Err(error) => failed.push(error),
    };
    if plan.daemon_unit.is_some() {
        record(service::remove_daemon().map(|unit| format!("daemon {}", unit.display())));
    }
    for dir in &plan.skills {
        record(
            std::fs::remove_dir_all(dir)
                .map(|_| format!("skill {}", dir.display()))
                .map_err(|error| format!("{}: {error}", dir.display())),
        );
    }
    if let Some(store) = &plan.documents {
        record(
            std::fs::remove_dir_all(store)
                .map(|_| format!("documents {}", store.display()))
                .map_err(|error| format!("{}: {error}", store.display())),
        );
    }
    // Last, so a failure above still leaves a binary to retry with.
    if let Some(binary) = &plan.binary {
        record(remove_binary(binary, cargo_bin).map(|what| format!("binary {what}")));
    }
    (removed, failed)
}

fn describe(plan: &Plan) -> String {
    let mut lines = vec!["atelier uninstall will remove:".to_string()];
    match &plan.daemon_unit {
        Some(unit) => lines.push(format!("  daemon:    {}", unit.display())),
        None if cfg!(target_os = "linux") => lines.push("  daemon:    not installed".into()),
        None => {}
    }
    if plan.skills.is_empty() {
        lines.push("  skills:    none installed".into());
    } else {
        for dir in &plan.skills {
            lines.push(format!("  skill:     {}", dir.display()));
        }
    }
    match &plan.binary {
        Some(binary) => lines.push(format!("  binary:    {}", binary.display())),
        None => lines.push("  binary:    could not be resolved; remove it by hand".into()),
    }
    if let Some(store) = &plan.documents {
        lines.push(format!(
            "  documents: {} — your artwork, permanently",
            store.display()
        ));
    }
    if let Some(store) = &plan.kept_documents {
        lines.push(format!(
            "  documents: kept at {} (pass --documents to delete them)",
            store.display()
        ));
    }
    lines.join("\n")
}

/// Entry point for `atelier uninstall`. Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    let options = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("atelier: {error}\n\n{USAGE}");
            return 2;
        }
    };
    let home = service::home();
    let daemon_unit = if cfg!(target_os = "linux") {
        service::daemon_manifest()
    } else {
        None
    };
    let binary = std::env::current_exe().ok();
    let store = service::global_home();
    let plan = plan(
        home.as_deref(),
        daemon_unit,
        binary,
        &store,
        options.documents,
    );
    eprintln!("{}", describe(&plan));
    if plan.is_empty() {
        eprintln!("nothing to remove");
        return 0;
    }
    if !options.yes && !library::confirm() {
        eprintln!("aborted; nothing was removed");
        return 1;
    }
    let (removed, failed) = apply(&plan, cargo_bin_dir(home.as_deref()).as_deref());
    for what in &removed {
        println!("✓ removed {what}");
    }
    for error in &failed {
        eprintln!("✗ {error}");
    }
    eprintln!(
        "\nNot touched: MCP client registrations (for Claude Code, `claude mcp remove atelier`)\n\
         and directory-local ./.atelier stores created with `atelier init`."
    );
    if failed.is_empty() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arguments_are_explicit() {
        assert_eq!(
            parse(&v(&[])).unwrap(),
            Options {
                documents: false,
                yes: false
            }
        );
        assert_eq!(
            parse(&v(&["--documents", "-y"])).unwrap(),
            Options {
                documents: true,
                yes: true
            }
        );
        assert!(parse(&v(&["--yes", "--yes"])).is_err());
        assert!(parse(&v(&["--force"])).is_err());
    }

    fn fake_home() -> PathBuf {
        // Tests run in parallel; a clock alone can hand two of them one dir.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "atelier-uninstall-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    #[test]
    fn plan_lists_only_what_exists_and_keeps_documents_by_default() {
        let home = fake_home();
        let sprite = home.join(".claude/skills/atelier-sprite/SKILL.md");
        let foreign = home.join(".claude/skills/someone-else/SKILL.md");
        let cursor_review = home.join(".cursor/skills/atelier-review/SKILL.md");
        let binary = home.join("bin/atelier");
        let store = home.join(".atelier");
        for path in [&sprite, &foreign, &cursor_review, &binary] {
            touch(path);
        }
        std::fs::create_dir_all(store.join("documents")).unwrap();

        let first = plan(Some(&home), None, Some(binary.clone()), &store, false);
        assert_eq!(
            first.skills,
            vec![
                home.join(".claude/skills/atelier-sprite"),
                home.join(".cursor/skills/atelier-review"),
            ]
        );
        assert_eq!(first.binary, Some(binary.clone()));
        assert_eq!(first.documents, None);
        assert_eq!(first.kept_documents, Some(store.clone()));
        assert!(describe(&first).contains("pass --documents"));

        let (removed, failed) = apply(&first, None);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(removed.len(), 3);
        assert!(!sprite.exists());
        assert!(!cursor_review.exists());
        assert!(!binary.exists());
        assert!(foreign.exists(), "other skills are not ours to remove");
        assert!(store.join("documents").is_dir(), "art is kept by default");

        let again = plan(Some(&home), None, Some(binary), &store, false);
        assert!(again.is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn documents_are_removed_only_when_asked() {
        let home = fake_home();
        let store = home.join(".atelier");
        std::fs::create_dir_all(store.join("documents")).unwrap();

        let plan = plan(Some(&home), None, None, &store, true);
        assert_eq!(plan.documents, Some(store.clone()));
        assert_eq!(plan.kept_documents, None);
        assert!(describe(&plan).contains("permanently"));
        let (removed, failed) = apply(&plan, None);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(removed.len(), 1);
        assert!(!store.exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_binary_outside_cargo_bin_is_unlinked_directly() {
        let home = fake_home();
        let binary = home.join("local/bin/atelier");
        touch(&binary);
        let cargo_bin = home.join(".cargo/bin");
        let what = remove_binary(&binary, Some(&cargo_bin)).unwrap();
        assert!(!what.contains("cargo uninstall"));
        assert!(!binary.exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn skill_dirs_cover_every_agent_and_skill() {
        let dirs = skill_dirs(Path::new("h"));
        assert_eq!(dirs.len(), crate::SKILL_TARGETS.len() * skills::ALL.len());
        assert!(dirs.contains(&PathBuf::from("h/.agents/skills/atelier-scene")));
    }
}
