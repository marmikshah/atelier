//! atelier: an MCP-native headless pixel-art editor (Aseprite-as-API).
//!
//! Agents create layered/animated documents, paint them with drawing primitives,
//! render PNG previews to inspect, and iterate. Documents live in a flat,
//! slug-addressed store (no projects, no baked-in style). Engine-agnostic
//! PNG/sheet/GIF output.
//!
//! Transports:
//!   atelier                      # stdio (default)
//!   atelier --http [ADDR]        # Streamable HTTP, default 127.0.0.1:8765
//!   ATELIER_HTTP=0.0.0.0:8765 atelier
//! Extra allowed Host headers for LAN/remote use (DNS-rebind guard):
//!   ATELIER_ALLOWED_HOSTS="my-workstation.local,192.168.1.20:8765"
//!
//! Daemon (background HTTP server, survives logout/reboot):
//!   atelier service install      # launchd (macOS) / systemd --user (Linux)
//!   atelier service status
//!   atelier service uninstall

use atelier::{replay, server, service};

const HELP: &str = "atelier — an MCP-native headless pixel-art editor (Aseprite-as-API).

USAGE:
    atelier                       run the MCP server over stdio (for clients that spawn it)
    atelier --http [ADDR]         run the streamable-HTTP MCP server (default 127.0.0.1:8765, endpoint /mcp)
    atelier service install       install + start the background daemon (launchd / systemd --user)
            [--bind ADDR] [--home DIR]
    atelier service status        show daemon state and log locations
    atelier service uninstall     stop + remove the daemon
    atelier replay <recipe.json>  replay a scripted sequence of tool calls (MCP client)
            [--home DIR]          run against an isolated ATELIER_HOME
    atelier --version             print the version

ENVIRONMENT:
    ATELIER_HOME             where documents/exports live (default ~/.atelier)
    ATELIER_HTTP             HTTP bind address (alternative to --http)
    ATELIER_ALLOWED_HOSTS    extra allowed Host headers for LAN/remote use";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Subcommands / flags that don't start the server.
    match args.get(1).map(|s| s.as_str()) {
        Some("service") => std::process::exit(service::run(&args[2..])),
        // Runs inside this runtime (it drives a child MCP server over stdio).
        Some("replay") => std::process::exit(replay::run(&args[2..]).await),
        Some("--version") | Some("-V") => {
            println!("atelier {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help") | Some("-h") => {
            println!("{HELP}");
            return Ok(());
        }
        _ => {}
    }

    // Resolve HTTP address from --http flag or env; otherwise stdio.
    let mut http_addr: Option<String> = std::env::var("ATELIER_HTTP").ok();
    if let Some(i) = args.iter().position(|a| a == "--http") {
        http_addr = Some(
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| "127.0.0.1:8765".into()),
        );
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
