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
//! atelier install                # asks for port; launchd / systemd --user
//! atelier status
//! atelier uninstall
//! atelier doctor                 # check the whole setup, print what to fix
//! atelier tools [--html]         # the tool surface / the reference page
//! atelier library [rm ...]       # inspect or prune the document store
//! atelier replay <recipe|id>     # rebuild a document from its journal
//! atelier call <tool> '<json>'   # one tool call, in-process (the CLI front door)
//! atelier init                   # stamp ./.atelier and its project manifest
//! atelier build                  # build the manifest's named exports
//! atelier recipe compact|expand  # convert recipes without replaying them
//! atelier skills [install|show]  # the shipped skills, for your agent
//! ```

use atelier_mcp::server;

mod call;
mod clients;
mod doctor;
mod fsutil;
mod init;
mod library;
mod project;
mod recipe_cmd;
mod replay;
mod service;
mod skills;

const HELP: &str = "atelier — the pixel-art studio agents can see (headless; CLI-first, MCP optional).

USAGE:
    atelier                       run the MCP server over stdio (for clients that spawn it)
    atelier --http [ADDR]         run the streamable-HTTP MCP server (default 127.0.0.1:8765, endpoint /mcp)
    atelier --record <recipe.jsonl> record a whole session (across documents) as a recipe
            (works with stdio and --http; also ATELIER_RECORD=<path>)
    atelier install               install/reconfigure the background daemon; asks for port
            [--port PORT | --bind ADDR] [--home DIR]
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
    atelier init                  stamp ./.atelier and its project.toml so this
                                  directory keeps its own art, recipes, and builds
    atelier build                 build every named export in .atelier/project.toml
            [--only NAME] [--dry-run]
                                  select one export, or print calls without writing
    atelier recipe <compact|expand|stats> <INPUT|->
                                  convert or measure recipes without replaying them
    atelier tools [--html|--schema <name>]
                                  list the tools (plain text; --html emits the
                                  reference page; --schema dumps one input schema)
    atelier doctor                check the whole setup — store, daemon (with a live MCP
                                  probe), client registrations, skills; prints each fix
    atelier clients install       register the MCP add-on for an agent:
            --for <claude|codex|kimi> --mode <http|stdio> [--allow-tools]
                                  preserves existing registrations; --allow-tools
                                  pre-approves every atelier MCP tool
    atelier skills                the shipped skills; `skills install [--for claude|codex|kimi|cursor|all]`
                                  writes them for your agent (~/.claude/skills by default, --dir DIR
                                  for anywhere else), `skills show <name>` prints one
    atelier --version             print the version

ENVIRONMENT:
    ATELIER_HOME             where documents/exports live (default ~/.atelier)
    ATELIER_HTTP             HTTP bind address (alternative to --http)
    ATELIER_ALLOWED_HOSTS    extra allowed Host headers for LAN/remote use
    ATELIER_RECORD           record tool calls into this recipe path (alternative to --record)
    ATELIER_LOG              log filter (RUST_LOG syntax; default info, output on stderr)
";

/// Agents that load the standard `SKILL.md`: `--for` selector → skills dir
/// under the user's home. Codex and Cursor additionally read project-level
/// skill directories; that is what `--dir` is for.
const SKILL_TARGETS: &[&str] = &["claude", "codex", "kimi", "cursor"];

/// Root directory for one client's skills. Kimi's config and skills share the
/// same overridable home; keeping that rule here lets install and doctor agree.
fn skill_target_root(target: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    let kimi_home = std::env::var_os("KIMI_CODE_HOME");
    skill_target_root_with_kimi_home(target, home, kimi_home.as_deref())
}

fn skill_target_root_with_kimi_home(
    target: &str,
    home: &std::path::Path,
    kimi_home: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    match target {
        "claude" => Some(home.join(".claude")),
        "codex" => Some(home.join(".agents")),
        "kimi" => Some(
            kimi_home
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.join(".kimi-code")),
        ),
        "cursor" => Some(home.join(".cursor")),
        _ => None,
    }
}

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
                let home = service::home().unwrap_or_else(|| std::path::PathBuf::from("."));
                match flag_value(args, "--for").unwrap_or("claude") {
                    "all" => SKILL_TARGETS
                        .iter()
                        .filter_map(|target| skill_target_root(target, &home))
                        .map(|root| root.join("skills"))
                        .collect(),
                    target => match skill_target_root(target, &home) {
                        Some(root) => vec![root.join("skills")],
                        None => {
                            eprintln!(
                                "atelier: unknown --for '{target}' (claude | codex | kimi | cursor | all)"
                            );
                            return 2;
                        }
                    },
                }
            };
            for dir in &dirs {
                for sk in skills::ALL {
                    let out = dir.join(sk.name).join("SKILL.md");
                    if let Some(p) = out.parent()
                        && let Err(e) = std::fs::create_dir_all(p)
                    {
                        eprintln!("atelier: {}: {e}", p.display());
                        return 1;
                    }
                    if let Err(e) = fsutil::write_text(&out, &sk.skill_md()) {
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
        // Register the optional MCP add-on and, only when explicitly asked,
        // pre-approve atelier's tools in one supported agent client.
        Some("clients") => std::process::exit(clients::run(&args[2..])),
        // The CLI front door: one tool call, in-process, through dispatch.
        Some("call") => std::process::exit(call::run(&args[2..]).await),
        // Stamp ./.atelier, opting this directory into a project store.
        Some("init") => std::process::exit(init::run(&args[2..])),
        // Build the portable, named exports declared by the project.
        Some("build") => std::process::exit(project::run(&args[2..]).await),
        // Inspect or convert recipes without executing their tool calls.
        Some("recipe") => std::process::exit(recipe_cmd::run(&args[2..])),
        // Runs inside this runtime: an in-process dispatch loop, no transport.
        Some("replay") => std::process::exit(replay::run(&args[2..]).await),
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
        // for supported agents; bare / `show <name>` prints. No network, no key.
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
    use super::{HELP, skill_target_root_with_kimi_home};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

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
        for cmd in [
            "atelier call",
            "atelier init",
            "atelier build",
            "atelier recipe",
            "atelier clients install",
        ] {
            assert!(HELP.contains(cmd), "USAGE must list the {cmd} subcommand");
        }
    }

    #[test]
    fn skill_targets_share_kimis_overridable_home() {
        let home = Path::new("user-home");
        assert_eq!(
            skill_target_root_with_kimi_home("kimi", home, None),
            Some(home.join(".kimi-code"))
        );
        assert_eq!(
            skill_target_root_with_kimi_home("kimi", home, Some(OsStr::new("custom-kimi"))),
            Some(PathBuf::from("custom-kimi"))
        );
        assert_eq!(
            skill_target_root_with_kimi_home("codex", home, Some(OsStr::new("ignored"))),
            Some(home.join(".agents"))
        );
        assert_eq!(
            skill_target_root_with_kimi_home("unknown", home, None),
            None
        );
    }
}
