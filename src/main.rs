const HELP: &str = "atelier — an MCP-native headless pixel-art editor (Aseprite-as-API).

USAGE:
    atelier                       run the MCP server (more transports land as the port completes)";

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--version") | Some("-V") => {
            println!("atelier {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            println!("{HELP}");
        }
    }
}
