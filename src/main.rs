//! atelier: an MCP-native headless pixel-art editor (Aseprite-as-API).
//!
//! Agents create layered/animated documents, paint them with drawing primitives,
//! render PNG previews to inspect, and iterate. Documents are grouped into thin
//! projects (a named folder; no baked-in style). Engine-agnostic PNG/sheet/GIF
//! output.
//!
//! Transports:
//!   atelier                      # stdio (default)

use atelier::server;

const HELP: &str = "atelier — an MCP-native headless pixel-art editor (Aseprite-as-API).

USAGE:
    atelier                       run the MCP server over stdio (for clients that spawn it)
    atelier --version             print the version

ENVIRONMENT:
    ATELIER_HOME             where documents/exports live (default ~/.atelier)";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Subcommands / flags that don't start the server.
    match args.get(1).map(|s| s.as_str()) {
        Some("service") => {
            eprintln!("atelier service: not yet available");
            std::process::exit(1);
        }
        Some("replay") => {
            eprintln!("atelier replay: not yet available");
            std::process::exit(1);
        }
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

    server::run().await
}
