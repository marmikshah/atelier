//! Atelier: an offline, headless pixel-art editor.
//!
//! Automation clients create layered and animated documents, apply drawing
//! operations, inspect PNG previews and measurements, and export assets.
//! Documents live in a flat, opaque-id-addressed store without process-global
//! selection state or a prescribed visual style.
//!
//! The CLI is the front door — every tool is one in-process call:
//!
//! ```text
//! atelier call doc_new '{"name":"cat","width":32,"height":32}'
//! atelier call doc_look '{"doc_id":"550e8400-e29b-41d4-a716-446655440000","out_path":"/tmp/cat.png"}'
//! ```
//!
//! MCP is an optional add-on transport for clients that only speak MCP:
//!
//! ```text
//! atelier                        # stdio (a client spawns it)
//! atelier --http [ADDR]          # Streamable HTTP, default 127.0.0.1:8765
//! ATELIER_HTTP=0.0.0.0:8765 ATELIER_HTTP_TOKEN=secret atelier
//!
//! # Extra allowed Host headers for LAN/remote use (DNS-rebind guard):
//! ATELIER_ALLOWED_HOSTS="my-workstation.local,192.168.1.20:8765"
//! ```
//!
//! Daemon (background HTTP server, survives logout/reboot), and the store:
//!
//! ```text
//! atelier install                # asks for port; systemd --user
//! atelier status
//! atelier uninstall
//! atelier tools [--html]         # the tool surface / the reference page
//! atelier library [COMMAND]      # inspect, archive, or prune the document store
//! atelier replay <journal|id>    # rebuild a document from its journal
//! atelier call <tool> '<json>'   # one tool call, in-process (the CLI front door)
//! atelier init                   # stamp a directory-local ./.atelier store
//! atelier skills [install|show]  # the shipped skills, for your agent
//! ```

// The store publishes each generation with an atomic directory exchange, which
// only some platforms provide (see `atelier-studio`'s `atomic_rename`). Linux
// and macOS both do; anything else is refused here rather than at the first
// mutation.
//
// Ubuntu x86_64 and the released `linux/amd64` container remain the supported
// targets. macOS builds and passes the test suite so the crate can be developed
// and tested there, but ships no binary and has no daemon: `atelier install`
// needs `systemd --user`.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("atelier needs an atomic directory exchange; only Linux and macOS provide one");

use atelier_mcp::server;

mod call;
mod fsutil;
mod init;
mod library;
mod replay;
mod service;
mod skills;

const HELP: &str = "atelier — offline, headless pixel-art editing through a CLI and MCP server.

USAGE:
    atelier                       run the MCP server over stdio (for clients that spawn it)
    atelier --http [ADDR]         run the streamable-HTTP MCP server (default 127.0.0.1:8765, endpoint /mcp)
    atelier install               install/reconfigure the background daemon; asks for port
            [--port PORT | --bind LOOPBACK_ADDR] [--home DIR]
    atelier status                show daemon state and log locations
    atelier uninstall             stop + remove the daemon
    atelier library               list the documents in the store (ATELIER_HOME)
            verify [--json]       validate stored metadata, cels, references, and journals
            pack <id> --out FILE [--home DIR]
                                  write a portable archive; never overwrites FILE
            unpack FILE [--home DIR] [--replace --yes]
                                  restore its UUID; replacement needs both flags
            rm <id>... | rm --prefix <p> | rm --all [--yes]
                                  delete documents — permanent, confirms first
    atelier replay <journal|id>   replay a JSONL journal, or rebuild a document from its
                                  own journal (every document records one)
            [--home DIR]          run against an isolated ATELIER_HOME
    atelier call <tool> ['<json>' | --file PATH | --stdin]
            [--home DIR] [--image-out PATH]
                                  run one tool call in-process — the whole op
                                  surface, scriptable from a shell. --image-out saves
                                  an inline image; stdout gets the JSON report.
                                  exit 0 ok, 1 tool/output error, 2 bad call
    atelier init                  stamp ./.atelier so this directory keeps its
                                  own art and recipes
    atelier tools [--html|--schema <name>]
                                  list the tools (plain text; --html emits the
                                  reference page; --schema dumps one input schema)
    atelier skills                the shipped skills; `skills install [--for claude|codex|kimi|cursor|all]`
                                  writes them for your agent (~/.claude/skills by default, --dir DIR
                                  for anywhere else), `skills show <name>` prints one
    atelier --version             print the version

ENVIRONMENT:
    ATELIER_HOME             where documents/exports live (default ~/.atelier)
    ATELIER_HTTP             HTTP bind address (alternative to --http)
    ATELIER_HTTP_TOKEN       bearer token; required for non-loopback HTTP
    ATELIER_ALLOWED_HOSTS    extra allowed Host headers (DNS-rebinding guard)
    ATELIER_IMPORT_ROOT      HTTP-only root for relative reference-image paths
    ATELIER_EXPORT_ROOT      HTTP-only root for relative output paths
    ATELIER_LOG              log filter (RUST_LOG syntax; default info, output on stderr)
";

/// Agents that load the standard `SKILL.md`: `--for` selector → skills dir
/// under the user's home. Codex and Cursor additionally read project-level
/// skill directories; that is what `--dir` is for.
const SKILL_TARGETS: &[&str] = &["claude", "codex", "kimi", "cursor"];

/// Root directory for one client's skills, under the user's home. `--dir`
/// writes them anywhere else.
fn skill_target_root(target: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    match target {
        "claude" => Some(home.join(".claude")),
        "codex" => Some(home.join(".agents")),
        "kimi" => Some(home.join(".kimi-code")),
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
        // Background daemon management (systemd --user). The verb is
        // args[1], which service::run dispatches on directly.
        Some("install") | Some("uninstall") | Some("status") => {
            std::process::exit(service::run(&args[1..]))
        }
        // Inspect / prune the document store.
        Some("library") => std::process::exit(library::run(&args[2..]).await),
        // The CLI front door: one tool call, in-process, through dispatch.
        Some("call") => std::process::exit(call::run(&args[2..]).await),
        // Stamp ./.atelier, opting into a directory-local store.
        Some("init") => std::process::exit(init::run(&args[2..])),
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
        Some(other) if other != "--http" => {
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
            server::run_http(&addr, allowed).await
        }
        None => server::run().await,
    }
}

#[cfg(test)]
mod tests {
    use super::{HELP, skill_target_root};
    use std::path::Path;

    #[test]
    fn core_commands_are_listed() {
        assert!(
            HELP.contains("atelier library"),
            "USAGE still lists its neighbours"
        );
        for cmd in ["atelier call", "atelier init", "atelier replay"] {
            assert!(HELP.contains(cmd), "USAGE must list the {cmd} subcommand");
        }
        assert!(HELP.contains("pack <id> --out FILE"));
        assert!(HELP.contains("unpack FILE [--home DIR] [--replace --yes]"));
    }

    #[test]
    fn each_skill_target_resolves_under_the_user_home() {
        let home = Path::new("user-home");
        assert_eq!(
            skill_target_root("claude", home),
            Some(home.join(".claude"))
        );
        assert_eq!(skill_target_root("codex", home), Some(home.join(".agents")));
        assert_eq!(
            skill_target_root("kimi", home),
            Some(home.join(".kimi-code"))
        );
        assert_eq!(
            skill_target_root("cursor", home),
            Some(home.join(".cursor"))
        );
        assert_eq!(skill_target_root("unknown", home), None);
    }
}
