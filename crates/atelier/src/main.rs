//! atelier: the pixel-art studio agents can see — a headless editor.
//!
//! Agents create layered/animated documents, paint them with drawing primitives,
//! render PNG previews to inspect, and iterate. Documents live in a flat,
//! slug-addressed store (no projects, no baked-in style). Engine-agnostic
//! PNG/sheet/GIF output.
//!
//! The CLI is the front door — every tool is one in-process call:
//!
//! ```text
//! atelier call doc_create '{"name":"cat","width":32,"height":32}'
//! atelier call doc_look '{"doc_id":"cat","out_path":"/tmp/cat.png"}'
//! ```
//!
//! MCP is an optional add-on transport for clients that only speak MCP:
//!
//! ```text
//! atelier                        # stdio (a client spawns it)
//! atelier --http [ADDR]          # Streamable HTTP, default 127.0.0.1:8765
//! ATELIER_HTTP=0.0.0.0:8765 atelier
//!
//! # Extra allowed Host headers for LAN/remote use (DNS-rebind guard):
//! ATELIER_ALLOWED_HOSTS="my-workstation.local,192.168.1.20:8765"
//! ```
//!
//! Daemon (background HTTP server, survives logout/reboot), and the store:
//!
//! ```text
//! atelier install                # launchd (macOS) / systemd --user (Linux)
//! atelier status
//! atelier uninstall
//! atelier doctor                 # check the whole setup, print what to fix
//! atelier tools [--html]         # the tool surface / the reference page
//! atelier library [rm ...]       # inspect or prune the document store
//! atelier replay <recipe|id>     # rebuild a document from its journal
//! atelier call <tool> '<json>'   # one tool call, in-process (the CLI front door)
//! atelier init                   # stamp ./.atelier — a project store here
//! atelier skills [install|show]  # the shipped skills, for your agent
//! atelier agent --task <t>       # the one online mode (feature-gated, off by default)
//! ```

use atelier_mcp::server;

#[cfg(feature = "agent")]
mod agent;
mod call;
mod doctor;
mod init;
mod library;
mod replay;
mod service;
mod skills;

const HELP: &str = "atelier — the pixel-art studio agents can see (headless; CLI-first, MCP optional).

USAGE:
    atelier                       run the MCP server over stdio (for clients that spawn it)
    atelier --http [ADDR]         run the streamable-HTTP MCP server (default 127.0.0.1:8765, endpoint /mcp)
    atelier --record <recipe.jsonl> record a whole session (across documents) as a recipe
            (works with stdio and --http; also ATELIER_RECORD=<path>)
    atelier install               install + start the background daemon (launchd / systemd --user)
            [--bind ADDR] [--home DIR]
    atelier status                show daemon state and log locations
    atelier uninstall             stop + remove the daemon
    atelier library               list the documents in the store (ATELIER_HOME)
            rm <id>... | rm --prefix <p> | rm --all [--yes]
                                  delete documents — permanent, confirms first
    atelier replay <recipe|id>    replay a recipe file, or rebuild a document from its
                                  own journal (every document records one)
            [--home DIR]          run against an isolated ATELIER_HOME
    atelier call <tool> ['<json>' | --file PATH | --stdin] [--home DIR]
                                  run one tool call in-process — the whole op
                                  surface, scriptable from a shell. stdout gets the
                                  JSON report; exit 0 ok, 1 tool error, 2 bad call
    atelier init                  stamp ./.atelier so this directory keeps its own
                                  project store (art + recipes live next to it)
    atelier tools [--html|--schema <name>]
                                  list the tools (plain text; --html emits the
                                  reference page; --schema dumps one input schema)
    atelier doctor                check the whole setup — store, daemon (with a live MCP
                                  probe), client registrations, skills; prints each fix
    atelier skills                the shipped skills; `skills install [--for claude|kimi|cursor|all]`
                                  writes them for your agent (~/.claude/skills by default, --dir DIR
                                  for anywhere else), `skills show <name>` prints one
    atelier agent --task <t>      draw a task by driving an OpenAI-style API (ONLINE; needs
                                  OPENAI_API_KEY; build with --features agent)
    atelier --version             print the version

ENVIRONMENT:
    ATELIER_HOME             where documents/exports live (default ~/.atelier)
    ATELIER_HTTP             HTTP bind address (alternative to --http)
    ATELIER_ALLOWED_HOSTS    extra allowed Host headers for LAN/remote use
    ATELIER_RECORD           record tool calls into this recipe path (alternative to --record)
";

/// Agents that load the standard `SKILL.md`: `--for` selector → skills dir
/// under the user's home. Cursor additionally reads project-level
/// `.cursor/skills/`; that is what `--dir` is for.
const SKILL_TARGETS: &[(&str, &str)] = &[
    ("claude", ".claude/skills"),
    ("kimi", ".kimi-code/skills"),
    ("cursor", ".cursor/skills"),
];

/// `atelier skills` — inspect or install the shipped skills. Pure markdown
/// generation from the typed registry; no network, no key.
fn skills_cmd(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        // Write each SKILL.md for the selected agent(s) into <dir>/<name>/SKILL.md.
        Some("install") => {
            if flag_value(args, "--dir").is_some() && flag_value(args, "--for").is_some() {
                eprintln!("atelier: --dir and --for are mutually exclusive");
                return 2;
            }
            let dirs: Vec<std::path::PathBuf> = if let Some(dir) = flag_value(args, "--dir") {
                vec![std::path::PathBuf::from(dir)]
            } else {
                // Agent skills dirs, not near the document store.
                let home = || service::home().unwrap_or_else(|| std::path::PathBuf::from("."));
                match flag_value(args, "--for").unwrap_or("claude") {
                    "all" => SKILL_TARGETS.iter().map(|(_, d)| home().join(d)).collect(),
                    target => match SKILL_TARGETS.iter().find(|(name, _)| *name == target) {
                        Some((_, d)) => vec![home().join(d)],
                        None => {
                            eprintln!(
                                "atelier: unknown --for '{target}' (claude | kimi | cursor | all)"
                            );
                            return 2;
                        }
                    },
                }
            };
            for dir in &dirs {
                for sk in skills::ALL {
                    let out = dir.join(sk.name).join("SKILL.md");
                    if let Some(p) = out.parent() {
                        if let Err(e) = std::fs::create_dir_all(p) {
                            eprintln!("atelier: {}: {e}", p.display());
                            return 1;
                        }
                    }
                    if let Err(e) = std::fs::write(&out, sk.skill_md()) {
                        eprintln!("atelier: {}: {e}", out.display());
                        return 1;
                    }
                }
                eprintln!(
                    "installed {} skills into {}",
                    skills::ALL.len(),
                    dir.display()
                );
            }
            0
        }
        // Print one skill's SKILL.md.
        Some("show") => match args.get(1).and_then(|n| skills::Skill::by_short(n)) {
            Some(sk) => {
                print!("{}", sk.skill_md());
                0
            }
            None => {
                eprintln!("atelier: skills show <sprite|scene|review>");
                2
            }
        },
        None => {
            for sk in skills::ALL {
                println!("  {:<14} {}", sk.short, first_sentence(sk.description));
            }
            0
        }
        Some(other) => {
            eprintln!("atelier: unknown skills command '{other}' (install | show <name>)");
            2
        }
    }
}

/// Value following `flag` in `args`, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn first_sentence(s: &str) -> &str {
    s.split_once(". ").map(|(a, _)| a).unwrap_or(s)
}

/// Install the global `tracing` subscriber. Logs go to **stderr only** —
/// stdout is the stdio MCP transport, so a single log line there corrupts the
/// protocol stream. `ATELIER_LOG` uses `RUST_LOG` syntax (e.g. `debug`,
/// `atelier_mcp=trace`); default `info`.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    // rmcp's per-request transport lifecycle chatters at info — three lines
    // per call under the stateless HTTP transport. Our own per-call line is
    // the signal; rmcp speaks only when something is wrong.
    let filter =
        EnvFilter::try_from_env("ATELIER_LOG").unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        // The daemon's stderr is a log file; escape codes would gunk it up.
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();
    let args: Vec<String> = std::env::args().collect();

    // Subcommands / flags that don't start the server.
    match args.get(1).map(|s| s.as_str()) {
        // Background daemon management (launchd / systemd --user). The verb is
        // args[1], which service::run dispatches on directly.
        Some("install") | Some("uninstall") | Some("status") => {
            std::process::exit(service::run(&args[1..]))
        }
        // Inspect / prune the document store.
        Some("library") => std::process::exit(library::run(&args[2..])),
        // Self-diagnostics: store, daemon (live MCP probe), clients, skills.
        Some("doctor") => std::process::exit(doctor::run(&args[2..])),
        // The CLI front door: one tool call, in-process, through dispatch.
        Some("call") => std::process::exit(call::run(&args[2..]).await),
        // Stamp ./.atelier, opting this directory into a project store.
        Some("init") => std::process::exit(init::run(&args[2..])),
        // Runs inside this runtime: an in-process dispatch loop, no transport.
        Some("replay") => std::process::exit(replay::run(&args[2..]).await),
        // The one online mode: draw a task via an OpenAI-style API. Gated so a
        // default build carries no HTTP stack.
        #[cfg(feature = "agent")]
        Some("agent") => std::process::exit(agent::run(&args[2..]).await),
        #[cfg(not(feature = "agent"))]
        Some("agent") => {
            eprintln!(
                "atelier: `agent` is the one online mode and is OFF in this build.\n                 Rebuild with it enabled: cargo install --path crates/atelier --features agent"
            );
            std::process::exit(2);
        }
        Some("--version") | Some("-V") => {
            println!("atelier {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        // List the tools (generated from the live registry). Plain text by
        // default; `--html` emits the reference page `make docs` publishes;
        // `--schema <name>` dumps one tool's input JSON schema.
        Some("tools") => {
            let rest = &args[2..];
            if let Some(name) = flag_value(rest, "--schema") {
                match server::Atelier::registry_tools()
                    .into_iter()
                    .find(|t| t.name.as_ref() == name)
                {
                    Some(t) => println!(
                        "{}",
                        serde_json::to_string_pretty(&t.input_schema).unwrap_or_default()
                    ),
                    None => {
                        eprintln!("atelier: unknown tool '{name}' — see `atelier tools`");
                        std::process::exit(2);
                    }
                }
            } else if rest.iter().any(|a| a == "--html") {
                print!("{}", server::tools_html());
            } else {
                print!("{}", server::tools_text());
            }
            return Ok(());
        }
        // Emit the shipped skills. `install [--dir DIR]` writes SKILL.md files
        // for Claude Code; bare / `show <name>` prints. No network, no key.
        Some("skills") => std::process::exit(skills_cmd(&args[2..])),
        Some("--help") | Some("-h") => {
            println!("{HELP}");
            return Ok(());
        }
        // Any other first token that isn't a known server flag is a mistake —
        // erroring beats silently starting the stdio server (which then blocks
        // on stdin) under a typo like `atelier serve`.
        Some(other) if !matches!(other, "--http" | "--record") => {
            eprintln!("atelier: unknown argument '{other}'\n\n{HELP}");
            std::process::exit(2);
        }
        _ => {}
    }

    // Resolve HTTP address from --http flag or env; otherwise stdio. The next
    // token is the address only when it isn't another flag; otherwise default.
    let mut http_addr: Option<String> = std::env::var("ATELIER_HTTP").ok();
    if let Some(i) = args.iter().position(|a| a == "--http") {
        http_addr = Some(match args.get(i + 1) {
            Some(a) if !a.starts_with("--") => a.clone(),
            _ => "127.0.0.1:8765".into(),
        });
    }

    // Resolve the optional session-recording path: --record <path> or ATELIER_RECORD.
    // The path is required, so a missing or flag-like next token is an error.
    let mut record: Option<std::path::PathBuf> = std::env::var_os("ATELIER_RECORD").map(Into::into);
    if let Some(i) = args.iter().position(|a| a == "--record") {
        let Some(path) = args.get(i + 1).filter(|a| !a.starts_with("--")) else {
            eprintln!("atelier: --record needs a recipe path argument");
            std::process::exit(2);
        };
        record = Some(path.into());
    }

    match http_addr {
        Some(addr) => {
            let mut allowed: Vec<String> = std::env::var("ATELIER_ALLOWED_HOSTS")
                .ok()
                .map(|s| {
                    s.split(',')
                        .map(|h| h.trim().to_string())
                        .filter(|h| !h.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            // Allow the bind host itself (with and without port).
            allowed.push(addr.clone());
            if let Some((host, _)) = addr.rsplit_once(':') {
                allowed.push(host.to_string());
            }
            server::run_http(&addr, allowed, record).await
        }
        None => server::run(record).await,
    }
}

#[cfg(test)]
mod tests {
    use super::HELP;

    #[test]
    fn doctor_is_wired_and_in_usage() {
        assert!(
            HELP.contains("atelier doctor"),
            "USAGE must list the doctor subcommand"
        );
        assert!(
            HELP.contains("atelier library"),
            "USAGE still lists its neighbours"
        );
        for cmd in ["atelier call", "atelier init"] {
            assert!(HELP.contains(cmd), "USAGE must list the {cmd} subcommand");
        }
    }
}
