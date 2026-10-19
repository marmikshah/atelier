//! `atelier uninstall` — remove Atelier from this machine.
//!
//! Removes what Atelier itself put there: the MCP registrations that
//! `atelier connect` made, the background daemon, the skills that `atelier
//! skills install` wrote for each agent, the `PATH` line install.sh added to
//! a shell profile, the binaries the npm package downloaded, and the binary
//! that is running. Documents are the user's art, so the global store is kept
//! unless `--documents` is passed, and the plan is shown and confirmed before
//! anything is deleted.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::connect::{AGENTS, Agent};
use crate::{library, service, skills};

const USAGE: &str = "usage: atelier uninstall [--documents] [--yes]

Remove atelier's MCP server from Claude Code and Codex, the daemon, the
installed agent skills, the PATH line the installer added, downloaded
binaries, and this binary. Shows the plan and asks for confirmation first.

  --documents  also delete the global document store (ATELIER_HOME or
               ~/.atelier) — your artwork; never implied
  --yes, -y    skip the confirmation prompt";

/// Ends every line install.sh adds to a shell profile, so this can find them.
const PROFILE_MARKER: &str = "# added by the atelier installer";

/// The fish snippet install.sh writes is a file of its own.
const FISH_SNIPPET: &str = "atelier.fish";

/// Profiles deleted once only the installer's line was in them: the fish
/// snippet, and a `~/.bash_profile` the installer created, which would
/// otherwise go on hiding `~/.profile` from bash login shells.
const DELETE_WHEN_EMPTY: &[&str] = &[FISH_SNIPPET, ".bash_profile"];

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

/// Where this machine keeps what Atelier may have installed, before checking
/// what is actually there.
#[derive(Debug, Default)]
struct Places {
    /// Agents that have a server named `atelier` registered.
    agents: Vec<&'static Agent>,
    home: Option<PathBuf>,
    /// The systemd unit's path (Linux only).
    daemon_unit: Option<PathBuf>,
    /// The running binary.
    binary: Option<PathBuf>,
    /// Shell profiles install.sh may have added a `PATH` line to.
    profiles: Vec<PathBuf>,
    /// The npm package's download cache.
    download_cache: Option<PathBuf>,
}

impl Places {
    /// This machine, as the environment describes it.
    fn here() -> Self {
        let home = service::home();
        let profiles = home
            .as_deref()
            .map(|home| {
                let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
                profile_candidates(home, zdotdir.as_deref())
            })
            .unwrap_or_default();
        Places {
            agents: AGENTS.iter().filter(|agent| agent.registered()).collect(),
            daemon_unit: if cfg!(target_os = "linux") {
                service::daemon_manifest()
            } else {
                None
            },
            binary: std::env::current_exe().ok(),
            profiles,
            download_cache: home.as_deref().map(download_cache),
            home,
        }
    }
}

/// Everything one run will remove, resolved up front so it can be shown and
/// confirmed as a whole. Only paths that exist are listed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// Agents that have a server named `atelier` registered.
    agents: Vec<&'static Agent>,
    /// The systemd unit, when one is installed (Linux only).
    daemon_unit: Option<PathBuf>,
    /// `<agent>/skills/<atelier-skill>` directories that exist.
    skills: Vec<PathBuf>,
    /// Shell profiles that hold a line install.sh added.
    profiles: Vec<PathBuf>,
    /// The running binary.
    binary: Option<PathBuf>,
    /// `<cache>/<version>` directories the npm package downloaded into.
    downloads: Vec<PathBuf>,
    /// The global document store, when `--documents` asked for it and it exists.
    documents: Option<PathBuf>,
    /// The global store that is being kept, named so the user knows where it is.
    kept_documents: Option<PathBuf>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.agents.is_empty()
            && self.daemon_unit.is_none()
            && self.skills.is_empty()
            && self.profiles.is_empty()
            && self.binary.is_none()
            && self.downloads.is_empty()
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

/// Every profile install.sh chooses from, whether or not it exists.
fn profile_candidates(home: &Path, zdotdir: Option<&Path>) -> Vec<PathBuf> {
    vec![
        zdotdir.unwrap_or(home).join(".zshrc"),
        home.join(".bashrc"),
        home.join(".bash_profile"),
        home.join(".bash_login"),
        home.join(".profile"),
        home.join(".config/fish/conf.d").join(FISH_SNIPPET),
    ]
}

/// Where npm/bin/atelier.js caches binaries; it applies the same rule.
fn download_cache(home: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("ATELIER_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        return home.join("Library/Caches/atelier");
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("atelier")
}

/// The cache's `X.Y.Z` directories. Nothing else in it is touched, so a cache
/// directory that turns out to be shared loses only what looks like ours.
fn downloads_in(cache: &Path) -> Vec<PathBuf> {
    let is_version = |name: &str| {
        let parts: Vec<&str> = name.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(cache)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| entry.file_name().to_str().is_some_and(is_version))
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

fn has_installer_line(profile: &Path) -> bool {
    std::fs::read_to_string(profile).is_ok_and(|text| {
        text.lines()
            .any(|line| line.trim_end().ends_with(PROFILE_MARKER))
    })
}

fn plan(places: Places, store: &Path, remove_documents: bool) -> Plan {
    let skills = places
        .home
        .as_deref()
        .map(skill_dirs)
        .unwrap_or_default()
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect();
    let store_exists = store.is_dir();
    Plan {
        agents: places.agents,
        daemon_unit: places.daemon_unit.filter(|unit| unit.is_file()),
        skills,
        profiles: places
            .profiles
            .into_iter()
            .filter(|profile| has_installer_line(profile))
            .collect(),
        binary: places.binary.filter(|path| path.is_file()),
        downloads: places
            .download_cache
            .as_deref()
            .map(downloads_in)
            .unwrap_or_default(),
        documents: (remove_documents && store_exists).then(|| store.to_path_buf()),
        kept_documents: (!remove_documents && store_exists).then(|| store.to_path_buf()),
    }
}

/// `text` without the lines install.sh added, nor the blank line it put
/// before each.
fn without_installer_lines(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim_end().ends_with(PROFILE_MARKER) {
            if kept.last().is_some_and(|last| last.trim().is_empty()) {
                kept.pop();
            }
        } else {
            kept.push(line);
        }
    }
    let mut rest = kept.join("\n");
    if !rest.is_empty() && text.ends_with('\n') {
        rest.push('\n');
    }
    rest
}

/// Take the installer's lines out of one profile, deleting the ones in
/// [`DELETE_WHEN_EMPTY`] when nothing else is left.
fn remove_profile_lines(profile: &Path) -> Result<String, String> {
    let fail = |error: std::io::Error| format!("{}: {error}", profile.display());
    // A dotfile manager's symlink keeps pointing at the file it names.
    let target = std::fs::canonicalize(profile).map_err(fail)?;
    let text = std::fs::read_to_string(&target).map_err(fail)?;
    let rest = without_installer_lines(&text);
    let deletable = profile
        .file_name()
        .is_some_and(|name| DELETE_WHEN_EMPTY.iter().any(|ours| name == *ours));
    if deletable && rest.trim().is_empty() {
        std::fs::remove_file(profile).map_err(fail)?;
        return Ok(format!("PATH setup {}", profile.display()));
    }
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let staged = target.with_file_name(format!(".{name}.atelier-uninstall"));
    std::fs::write(&staged, rest)
        .and_then(|_| std::fs::metadata(&target))
        .and_then(|meta| std::fs::set_permissions(&staged, meta.permissions()))
        .and_then(|_| std::fs::rename(&staged, &target))
        .map_err(|error| {
            let _ = std::fs::remove_file(&staged);
            fail(error)
        })?;
    Ok(format!("PATH line from {}", profile.display()))
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

fn remove_dir(what: &str, dir: &Path) -> Result<String, String> {
    std::fs::remove_dir_all(dir)
        .map(|_| format!("{what} {}", dir.display()))
        .map_err(|error| format!("{}: {error}", dir.display()))
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
    for agent in &plan.agents {
        record(
            agent
                .unregister()
                .map(|_| format!("{} MCP server registration", agent.label)),
        );
    }
    if plan.daemon_unit.is_some() {
        record(service::remove_daemon().map(|unit| format!("daemon {}", unit.display())));
    }
    for dir in &plan.skills {
        record(remove_dir("skill", dir));
    }
    for profile in &plan.profiles {
        record(remove_profile_lines(profile));
    }
    if let Some(store) = &plan.documents {
        record(remove_dir("documents", store));
    }
    // Last, so a failure above still leaves a binary to retry with. The
    // running binary may itself be a download, so the downloads go after it.
    if let Some(binary) = &plan.binary {
        record(remove_binary(binary, cargo_bin).map(|what| format!("binary {what}")));
    }
    for dir in &plan.downloads {
        record(remove_dir("downloads", dir));
    }
    (removed, failed)
}

fn describe(plan: &Plan) -> String {
    let mut lines = vec!["atelier uninstall will remove:".to_string()];
    for agent in &plan.agents {
        lines.push(format!("  mcp:       {} registration", agent.label));
    }
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
    for profile in &plan.profiles {
        lines.push(format!(
            "  path:      the PATH line the installer added to {}",
            profile.display()
        ));
    }
    match &plan.binary {
        Some(binary) => lines.push(format!("  binary:    {}", binary.display())),
        None => lines.push("  binary:    could not be resolved; remove it by hand".into()),
    }
    for dir in &plan.downloads {
        lines.push(format!(
            "  downloads: {} (fetched by the npm package)",
            dir.display()
        ));
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
    let places = Places::here();
    let cargo_bin = cargo_bin_dir(places.home.as_deref());
    let plan = plan(places, &service::global_home(), options.documents);
    eprintln!("{}", describe(&plan));
    if plan.is_empty() {
        eprintln!("nothing to remove");
        return 0;
    }
    if !options.yes && !library::confirm() {
        eprintln!("aborted; nothing was removed");
        return 1;
    }
    let (removed, failed) = apply(&plan, cargo_bin.as_deref());
    for what in &removed {
        println!("✓ removed {what}");
    }
    for error in &failed {
        eprintln!("✗ {error}");
    }
    eprintln!(
        "\nNot touched: directory-local ./.atelier stores created with `atelier init`,\n\
         and atelier entries in MCP clients other than Claude Code and Codex."
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
        let places = || Places {
            home: Some(home.clone()),
            binary: Some(binary.clone()),
            ..Places::default()
        };

        let first = plan(places(), &store, false);
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

        assert!(plan(places(), &store, false).is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn documents_are_removed_only_when_asked() {
        let home = fake_home();
        let store = home.join(".atelier");
        std::fs::create_dir_all(store.join("documents")).unwrap();
        let places = Places {
            home: Some(home.clone()),
            ..Places::default()
        };

        let plan = plan(places, &store, true);
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
    fn the_installers_path_lines_come_out_and_nothing_else() {
        let home = fake_home();
        let candidates = profile_candidates(&home, None);
        let zshrc = home.join(".zshrc");
        let line = format!(
            "case \":$PATH:\" in *\":$HOME/.local/bin:\"*) ;; *) export PATH=\"$HOME/.local/bin:$PATH\" ;; esac {PROFILE_MARKER}"
        );
        std::fs::write(
            &zshrc,
            format!("alias ll='ls -l'\n\n{line}\nexport EDITOR=vi\n"),
        )
        .unwrap();
        let bashrc = home.join(".bashrc");
        std::fs::write(&bashrc, "alias ll='ls -l'\n").unwrap();
        let fish = home.join(".config/fish/conf.d/atelier.fish");
        touch(&fish);
        std::fs::write(
            &fish,
            format!("fish_add_path --path \"$HOME/.local/bin\" {PROFILE_MARKER}\n"),
        )
        .unwrap();
        let places = Places {
            home: Some(home.clone()),
            profiles: candidates,
            ..Places::default()
        };

        let plan = plan(places, &home.join(".atelier"), false);
        assert_eq!(plan.profiles, vec![zshrc.clone(), fish.clone()]);
        let (_, failed) = apply(&plan, None);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            std::fs::read_to_string(&zshrc).unwrap(),
            "alias ll='ls -l'\nexport EDITOR=vi\n"
        );
        assert_eq!(
            std::fs::read_to_string(&bashrc).unwrap(),
            "alias ll='ls -l'\n"
        );
        assert!(!fish.exists(), "the fish snippet was ours alone");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn the_installer_writes_the_marker_this_looks_for() {
        let installer = include_str!("../../../install.sh");
        assert!(installer.contains(PROFILE_MARKER));
        assert!(installer.contains(FISH_SNIPPET));
    }

    #[test]
    fn only_version_directories_of_the_download_cache_go() {
        let home = fake_home();
        let cache = home.join("cache");
        touch(&cache.join("0.2.0/aarch64-apple-darwin/atelier"));
        touch(&cache.join("0.10.1/x86_64-unknown-linux-musl/atelier"));
        touch(&cache.join("someone-else/data"));
        touch(&cache.join("1.2/data"));
        assert_eq!(
            downloads_in(&cache),
            vec![cache.join("0.10.1"), cache.join("0.2.0")]
        );
        assert!(downloads_in(&home.join("missing")).is_empty());
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
