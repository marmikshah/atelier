//! `atelier connect` — hand Atelier to the coding agents on this machine.
//!
//! Each agent whose command is on `PATH` gets Atelier's MCP server registered
//! through that command, so its config format stays its own business, and the
//! bundled skills written to its skills folder. The server is registered by
//! this binary's absolute path, which desktop apps and editor extensions can
//! start even when they do not see the shell's `PATH`. `atelier uninstall`
//! undoes both.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::service;

const USAGE: &str = "usage: atelier connect [claude] [codex]

Register atelier's MCP server with each agent and install its skills. With no
names, connects every supported agent whose command is on PATH. Run it again
after moving the binary; it replaces the earlier registration.";

/// An agent whose command-line program manages its MCP servers.
#[derive(Debug, PartialEq, Eq)]
pub struct Agent {
    /// The agent's command; also its name on the command line and its
    /// `skills install --for` target.
    pub command: &'static str,
    /// The product name people know it by.
    pub label: &'static str,
    /// Registers the server; the binary's path follows.
    add: &'static [&'static str],
    /// Removes the registration `add` made. Claude's `add` refuses a name it
    /// already has, so `connect` runs this first to replace one.
    remove: &'static [&'static str],
}

pub const AGENTS: &[Agent] = &[
    Agent {
        command: "claude",
        label: "Claude Code",
        add: &["mcp", "add", "--scope", "user", "atelier", "--"],
        remove: &["mcp", "remove", "--scope", "user", "atelier"],
    },
    Agent {
        command: "codex",
        label: "Codex",
        add: &["mcp", "add", "atelier", "--"],
        remove: &["mcp", "remove", "atelier"],
    },
];

impl Agent {
    fn by_command(command: &str) -> Option<&'static Agent> {
        AGENTS.iter().find(|agent| agent.command == command)
    }

    /// Whether the agent's command is an executable file on `PATH`. Checked
    /// without running it, so an absent agent is skipped rather than failed.
    pub fn installed(&self) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| {
                std::fs::metadata(dir.join(self.command))
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
        })
    }

    /// Whether the agent has a server named `atelier`, as its own
    /// `mcp get` reports it.
    pub fn registered(&self) -> bool {
        self.installed()
            && Command::new(self.command)
                .args(["mcp", "get", "atelier"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    }

    fn run(&self, args: &[&str], binary: Option<&Path>) -> Result<(), String> {
        let mut command = Command::new(self.command);
        command.args(args).stdin(Stdio::null());
        if let Some(binary) = binary {
            command.arg(binary);
        }
        let output = command
            .output()
            .map_err(|error| format!("{}: {error}", self.command))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        Err(format!(
            "`{} {}` failed{}{detail}",
            self.command,
            args.join(" "),
            if detail.is_empty() { "" } else { ": " }
        ))
    }

    /// Remove the registration `connect` made.
    pub fn unregister(&self) -> Result<(), String> {
        self.run(self.remove, None)
    }

    /// Register `binary` as the agent's `atelier` server, replacing any
    /// earlier registration, and install the skills under `home`.
    fn connect(&self, binary: &Path, home: &Path) -> Result<Vec<String>, String> {
        // Claude refuses to add over an existing name; Codex overwrites. A
        // failure here only means there was nothing to replace.
        let _ = self.unregister();
        self.run(self.add, Some(binary))?;
        let skills = crate::skill_target_root(self.command, home)
            .map(|root| root.join("skills"))
            .ok_or_else(|| format!("{}: no skills folder is known", self.command))?;
        let count = crate::write_skills(&skills)?;
        Ok(vec![
            format!("MCP server registered → {}", tilde(binary, home)),
            format!("{count} skills installed in {}", tilde(&skills, home)),
        ])
    }
}

/// `path` with the home directory written as `~`, for reading.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => Path::new("~").join(rest).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// The absolute path agents should start, as this process was started.
/// Symlinks are kept, so a link a package manager maintains survives updates.
fn binary() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(std::path::absolute)
        .map_err(|error| format!("cannot resolve this binary's path: {error}"))
}

/// Entry point for `atelier connect`. Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let mut chosen: Vec<&'static Agent> = Vec::new();
    for arg in args {
        match Agent::by_command(arg) {
            Some(agent) if !chosen.iter().any(|c| c.command == agent.command) => chosen.push(agent),
            Some(_) => {}
            None => {
                eprintln!("atelier: unknown agent '{arg}'\n\n{USAGE}");
                return 2;
            }
        }
    }
    if chosen.is_empty() {
        chosen = AGENTS.iter().filter(|agent| agent.installed()).collect();
        if chosen.is_empty() {
            eprintln!(
                "No supported agent found on PATH (looked for {}).\n\
                 For another MCP client, register this binary as a stdio server with no\n\
                 arguments, and see `atelier skills install --help` for its skills.",
                AGENTS
                    .iter()
                    .map(|agent| agent.command)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return 0;
        }
    }
    let (binary, home) = match (binary(), service::home()) {
        (Ok(binary), Some(home)) => (binary, home),
        (Err(error), _) => {
            eprintln!("atelier: {error}");
            return 1;
        }
        (_, None) => {
            eprintln!("atelier: HOME is not set, so there is no skills folder to write");
            return 1;
        }
    };
    let mut failed = false;
    for agent in chosen {
        if !agent.installed() {
            eprintln!("✗ {}: `{}` is not on PATH", agent.label, agent.command);
            failed = true;
            continue;
        }
        match agent.connect(&binary, &home) {
            Ok(done) => {
                for what in done {
                    println!("✓ {}: {what}", agent.label);
                }
            }
            Err(error) => {
                eprintln!("✗ {}: {error}", agent.label);
                failed = true;
            }
        }
    }
    if failed {
        1
    } else {
        eprintln!("Start a new agent session to use atelier.");
        0
    }
}
