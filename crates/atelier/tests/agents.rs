//! End-to-end contract for `atelier connect` and `atelier uninstall`, run
//! against stand-in `claude` and `codex` commands that record their calls and
//! keep a registration the way the real ones do.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// A throwaway home directory with its own `PATH` and a copy of the binary,
/// so uninstall removes the copy and not the one under test.
struct Machine {
    root: PathBuf,
}

impl Machine {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "atelier-agents-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("agents")).expect("create agents dir");
        std::fs::create_dir_all(root.join("local/bin")).expect("create bin dir");
        std::fs::copy(
            env!("CARGO_BIN_EXE_atelier"),
            root.join("local/bin/atelier"),
        )
        .expect("copy the binary");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn binary(&self) -> PathBuf {
        self.path("local/bin/atelier")
    }

    /// A stand-in agent command. Claude's `add` refuses an existing name and
    /// its `remove` fails when there is nothing to remove; Codex does neither.
    fn agent(&self, name: &str, strict: bool) {
        let strict = if strict { "yes" } else { "no" };
        let script = self.path("agents").join(name);
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$0.log"
registered="$0.registered"
case "$1 $2" in
  "mcp get") test -e "$registered" ;;
  "mcp add")
    if [ -e "$registered" ] && [ {strict} = yes ]; then echo "already exists" >&2; exit 1; fi
    printf '%s\n' "$*" > "$registered" ;;
  "mcp remove")
    if [ -e "$registered" ]; then rm "$registered"
    elif [ {strict} = yes ]; then echo "No MCP server named atelier" >&2; exit 1; fi ;;
  *) exit 2 ;;
esac
"#
            ),
        )
        .expect("write stand-in agent");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make stand-in agent executable");
    }

    fn calls(&self, agent: &str) -> Vec<String> {
        std::fs::read_to_string(self.path("agents").join(format!("{agent}.log")))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn registered(&self, agent: &str) -> bool {
        self.path("agents")
            .join(format!("{agent}.registered"))
            .exists()
    }

    fn atelier(&self, args: &[&str]) -> Output {
        let path = std::env::join_paths([self.path("agents"), "/usr/bin".into(), "/bin".into()])
            .expect("join PATH");
        let mut command = Command::new(self.binary());
        command
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("CARGO_HOME", self.path(".cargo"))
            .env("ATELIER_LOG", "off");
        // On Linux, a process another test thread forks while a copy is being
        // written briefly holds it open, and exec refuses a file open for writing.
        for _ in 0..50 {
            match command.output() {
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                result => return result.expect("run atelier"),
            }
        }
        panic!("the copied binary stayed busy");
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The binary path an agent was told to start, from its recorded `add` call.
fn registered_binary(calls: &[String]) -> PathBuf {
    let add = calls
        .iter()
        .rfind(|call| call.starts_with("mcp add"))
        .expect("an add call");
    let (_, binary) = add.split_once(" -- ").expect("binary after --");
    PathBuf::from(binary)
}

fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

#[test]
fn connect_registers_every_agent_and_uninstall_takes_it_all_back() {
    let machine = Machine::new("round-trip");
    machine.agent("claude", true);
    machine.agent("codex", false);

    let connected = machine.atelier(&["connect"]);
    assert_ok(&connected);
    let report = String::from_utf8_lossy(&connected.stdout);
    assert!(
        report.contains("Claude Code") && report.contains("Codex"),
        "{report}"
    );

    let claude = machine.calls("claude");
    assert_eq!(claude[0], "mcp remove --scope user atelier");
    assert!(
        claude[1].starts_with("mcp add --scope user atelier -- /"),
        "{claude:?}"
    );
    assert!(same_file(&registered_binary(&claude), &machine.binary()));
    let codex = machine.calls("codex");
    assert_eq!(codex[0], "mcp remove atelier");
    assert!(codex[1].starts_with("mcp add atelier -- /"), "{codex:?}");
    assert!(same_file(&registered_binary(&codex), &machine.binary()));
    for skills in [".claude/skills", ".agents/skills"] {
        let sprite = machine.path(skills).join("atelier-sprite/SKILL.md");
        assert!(sprite.is_file(), "{} missing", sprite.display());
    }

    // Again, over the existing registrations: Claude would refuse a bare add.
    assert_ok(&machine.atelier(&["connect"]));
    assert!(machine.registered("claude") && machine.registered("codex"));

    std::fs::create_dir_all(machine.path(".atelier/documents")).unwrap();
    let uninstalled = machine.atelier(&["uninstall", "--yes"]);
    assert_ok(&uninstalled);
    let plan = String::from_utf8_lossy(&uninstalled.stderr);
    assert!(plan.contains("Claude Code registration"), "{plan}");
    assert!(plan.contains("Codex registration"), "{plan}");
    assert!(!machine.registered("claude") && !machine.registered("codex"));
    assert_eq!(
        machine.calls("claude").last().map(String::as_str),
        Some("mcp remove --scope user atelier")
    );
    assert!(!machine.path(".claude/skills/atelier-sprite").exists());
    assert!(!machine.path(".agents/skills/atelier-sprite").exists());
    assert!(!machine.binary().exists());
    assert!(machine.path(".atelier/documents").is_dir(), "art is kept");
}

#[test]
fn connect_with_no_agent_on_path_writes_nothing() {
    let machine = Machine::new("none");
    let output = machine.atelier(&["connect"]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("No supported agent"));
    assert!(!machine.path(".claude").exists());
    assert!(!machine.path(".agents").exists());
}

#[test]
fn connect_refuses_unknown_and_missing_agents() {
    let machine = Machine::new("refuse");
    machine.agent("codex", false);

    assert_eq!(
        machine.atelier(&["connect", "cursor"]).status.code(),
        Some(2)
    );
    let missing = machine.atelier(&["connect", "claude"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("not on PATH"));
    assert!(
        machine.calls("codex").is_empty(),
        "only the named agent is touched"
    );
}
